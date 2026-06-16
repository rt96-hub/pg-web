#!/usr/bin/env bash
# 015 benchmark harness.
# Boots a pg-web stack (the unified $IMAGE tag), pushes the dedicated bench app,
# seeds data for the workloads, runs oha (pinned) against each, and writes
# raw results under bench/results/.
#
# This Step 1 deliverable is shippable on its own. Step 2 (multi-worker) is
# optional and comes after the numbers exist.
#
# Entry points:
#   bash bench/run.sh
#   BENCH_CPUS=1 BENCH_MEM=2g bash bench/run.sh   # primary 1-vCPU/2-GiB tier
#   RUN_BENCH=1 scripts/test-all.sh               # opt-in full (heavy)
#
# Reporting (prompt 028): honours TEST_MODE (errors default | short | verbose).
# Per-workload one-line markers (req/s + p50/p99), a compact end-of-run table,
# an explicit HOLB before/after pair, a threshold check, and a single
# `PGWEB-BENCH … OVERALL=ok|fail` verdict line that ALWAYS prints (even on an
# infra failure, via the EXIT trap). Raw oha histograms are captured to
# bench/results/*.txt and streamed to the terminal only in verbose mode.
#
# Tool choice: oha (pinned). Justification in docs/BENCHMARKS.md.
# - Single static binary, no runtime deps (fits "one binary" ethos).
# - First-class p50/p90/p95/p99/p99.9 + histogram.
# - -q / --qps for constant-arrival-rate (open model) — required for honest
#   head-of-line-blocking measurement (closed-model tools self-throttle).
# - -z duration, -c concurrency, easy to script.
#
# Hardware note: primary tier uses Docker cgroup limits (--cpus/--memory).
# State the actual instance or "Docker --cpus=1 --memory=2g on $(uname -m) host"
# in the published BENCHMARKS.md. Comparison tier runs unconstrained on the
# same box to demonstrate the single-thread ceiling (more cores don't help).

set -euo pipefail

# BASH_SOURCE (not $0) so the path resolves to run.sh itself even when the file
# is sourced (e.g. unit-testing the gate functions) — identical to $0 on a normal
# `bash bench/run.sh` execution.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Shared reporting helpers (markers + parsers). Sourced after cd so the relative
# path resolves; TTY/glyph detection happens at source time.
# shellcheck source=scripts/report-lib.sh
source "$REPO_ROOT/scripts/report-lib.sh"
# Shared idempotency primitives (prompt 029): bench is a first-class entrypoint,
# so it gets the SAME unified content-hash image freshness, env reclaim, and
# self-healing lock as scripts/test-all.sh — `./bench/run.sh` auto-rebuilds on a
# source change with no flag, and a stale image never silently benchmarks old
# code. Export PGWEB_REPO_ROOT from our own $0 so the lib anchors here.
export PGWEB_REPO_ROOT="$REPO_ROOT"
# shellcheck source=scripts/lib/harness.sh
source "$REPO_ROOT/scripts/lib/harness.sh"
# Regression-gate knobs + per-tier baseline tables (prompt 030). Kept separate so
# the platform baselines are easy to re-capture/review (029 open-Q2).
# shellcheck source=bench/thresholds.sh
source "$REPO_ROOT/bench/thresholds.sh"

# Output mode: env TEST_MODE, overridable by --short/--errors/--verbose.
for _arg in "$@"; do
    case "$_arg" in
        --short)   TEST_MODE=short ;;
        --errors)  TEST_MODE=errors ;;
        --verbose) TEST_MODE=verbose ;;
    esac
done
TEST_MODE="${TEST_MODE:-errors}"

BENCH_CPUS="${BENCH_CPUS:-}"
BENCH_MEM="${BENCH_MEM:-}"          # e.g. 2g
OHA_VERSION="${OHA_VERSION:-1.14.0}" # pinned for reproducibility (see bench/run.sh ensure_oha for asset mapping)
RESULTS_DIR="bench/results"
COMPOSE="docker compose -f bench/docker-compose.yml"
CONTAINER_NAME="bench-postgres-1"   # compose-derived container name (project `bench`)

# Unified image tag (prompt 029 #3): single source of truth shared with
# test-all.sh + build-image.sh. Honors TEST_IMAGE / PGWEB_IMAGE, defaults to the
# shipped image. Exported so bench/docker-compose.yml's `image: ${PGWEB_IMAGE:-…}`
# resolves to exactly the tag we freshness-check — no hardcoded literal drift.
IMAGE="$(pgweb_image)"
export PGWEB_IMAGE="$IMAGE"

# Tier identity (for the markers / verdict line / table header).
if [[ -n "$BENCH_CPUS" || -n "$BENCH_MEM" ]]; then
    BENCH_TIER_TAG="${BENCH_CPUS:-?}c-${BENCH_MEM:-?}"          # e.g. 1c-2g
    BENCH_TIER_LABEL="constrained ${BENCH_CPUS:-?} vCPU / ${BENCH_MEM:-?}"
else
    BENCH_TIER_TAG="unconstrained"
    BENCH_TIER_LABEL="unconstrained (full host)"
fi

# Regression gate (prompt 030). The old "did it serve at all" placeholder
# (a-static-c1 success >= 1%) is gone: it was justified by "the loaded legs
# always report 0% success / n/a — that's the single-worker reality," which was
# WRONG (the worker self-terminated 8s after startup; fixed in 729eb93). On a
# healthy server EVERY leg is ~100% success with real p50/p99, so the gate is now
# meaningful: a per-workload >=99% success floor (always on) + per-tier p99
# ceilings + successful-req/s floors (opt-in via BENCH_STRICT). Knobs + baselines
# live in bench/thresholds.sh; evaluation is evaluate_gate; breaches print the
# loud, always-on, itemized banner (print_bench_banner). Tune via BENCH_MIN_SUCCESS
# / BENCH_P99_MARGIN / BENCH_RPS_FLOOR_FRAC / BENCH_STRICT (all in thresholds.sh).

mkdir -p "$RESULTS_DIR"

# Per-workload accumulators for the compact end-of-run table.
BENCH_LABELS=(); BENCH_REQS=(); BENCH_SUCC=(); BENCH_P50=(); BENCH_P99=()
BENCH_VERDICT_PRINTED=0
# Breached-check accumulators for the loud regression banner (parallel arrays,
# one entry per failed check). Populated by evaluate_gate / bench_on_exit.
FAIL_LABEL=(); FAIL_METRIC=(); FAIL_OBS=(); FAIL_REL=(); FAIL_THRESH=(); FAIL_DELTA=()
BENCH_BANNER_PRINTED=0

log() { echo "[bench] $*"; }

# Always emit a PGWEB-BENCH verdict, even on an early/infra exit. The trap is
# installed at the top of main() so a missing-docker / stack-timeout / push
# failure still produces the greppable verdict line (029 asserts on its presence).
bench_on_exit() {
    local rc=$?
    stop_stack 2>/dev/null || true
    # Release the cross-run lock if we acquired it (standalone bench). A nested
    # run under test-all (PGWEB_NESTED=1) never took the lock, so it must not
    # release the parent's; release_lock also pid-guards, so this is doubly safe.
    [[ "${PGWEB_NESTED:-}" != "1" ]] && release_lock "$PGWEB_LOCKDIR"
    if [[ "${BENCH_VERDICT_PRINTED:-0}" != "1" ]]; then
        # An infra/early exit (no Docker, stack timeout, push failure, oha
        # missing, missing result file, kill mid-run, …). The banner must be
        # loud here too — wire it through the trap so these never emit just a
        # terse one-liner. One synthetic "startup" breach item carries the rc.
        _gate_add_fail "(infra)" "startup" "rc=$rc" "!=" "clean run (rc=0)" "stack/oha/push/timeout"
        print_bench_banner "$BENCH_TIER_TAG" "infra/early-exit before the gate ran"
        echo
        printf 'PGWEB-BENCH tier=%s workloads=%d threshold="infra/early-exit (rc=%s)"  OVERALL=fail\n' \
            "$BENCH_TIER_TAG" "${#BENCH_LABELS[@]}" "$rc"
    fi
}

require_docker() {
  if ! command -v docker >/dev/null; then
    echo "docker is required for the benchmark harness (tier 3+ style)" >&2
    exit 1
  fi
}

# Ensure the image is fresh using the SAME content-hash freshness check as
# test-all.sh (prompt 029 #1). This is the fix for the headline complaint: a
# source edit before `./bench/run.sh` now rebuilds automatically (surfaced as
# the STALE → BUILD → BUILT markers) instead of silently benchmarking the old
# binary. On an unchanged tree it prints REUSED. No flag, ever, on this path.
ensure_image() {
  mk_start image "freshness check (content-hash) — $IMAGE"
  ensure_image_fresh "$IMAGE" "$RESULTS_DIR/image-build.log"
}

# Download a pinned oha if not in PATH. Prefers a static release asset.
# Targets common CI / dev platforms. Falls back to a helpful message.
ensure_oha() {
  if command -v oha >/dev/null; then
    OHA_CMD=oha
    log "using oha from PATH: $(oha --version 2>/dev/null || echo 'unknown version')"
    return 0
  fi

  local os arch asset url out
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)

  case "$os-$arch" in
    darwin-arm64)   asset="oha-macos-arm64" ;;
    darwin-x86_64)  asset="oha-macos-amd64" ;;
    linux-x86_64)   asset="oha-linux-amd64" ;;
    linux-aarch64)  asset="oha-linux-arm64" ;;
    *)
      echo "No oha in PATH and no prebuilt for $os-$arch." >&2
      echo "Install oha (https://github.com/hatoo/oha) or run on a supported platform." >&2
      exit 1
      ;;
  esac

  # Assets are direct executables named oha-*-* (no .tar.gz wrapper for these).
  url="https://github.com/hatoo/oha/releases/download/v${OHA_VERSION}/${asset}"
  out="bench/bin/oha-${OHA_VERSION}-${asset}"
  mkdir -p bench/bin

  if [[ ! -x "$out" ]]; then
    log "downloading pinned oha v${OHA_VERSION} (${asset})"
    curl -fsSL -o "$out" "$url" || {
      echo "download failed from $url" >&2
      echo "You can set OHA_CMD=/path/to/oha or install oha and ensure it is in PATH." >&2
      exit 1
    }
    chmod +x "$out"
  fi
  OHA_CMD="$out"
  log "using downloaded oha: $("$OHA_CMD" --version 2>/dev/null || echo 'oha')"
}

start_stack() {
  log "starting bench stack (image $IMAGE, tier: $BENCH_TIER_LABEL)"
  # Stop anything left from previous run (idempotent).
  $COMPOSE down --volumes --remove-orphans >/dev/null 2>&1 || true

  # Bring up. Limits are applied via the compose file's deploy.resources when
  # BENCH_CPUS/BENCH_MEM are exported, plus an explicit docker update for
  # environments that don't fully apply compose limits to plain `docker compose`.
  BENCH_CPUS="$BENCH_CPUS" BENCH_MEM="$BENCH_MEM" $COMPOSE up -d --quiet-pull

  if [[ -n "$BENCH_CPUS" || -n "$BENCH_MEM" ]]; then
    log "applying resource constraints (CPUS=${BENCH_CPUS:-<none>} MEM=${BENCH_MEM:-<none>})"
    # docker update works on a running container and is reliable cross-platform.
    # Memory value must include unit for the flag.
    # Also set --memory-swap at the same time as --memory (to the same value)
    # to avoid "Memory limit should be smaller than already set memoryswap limit"
    # errors on Docker Desktop / macOS cgroups (seen in harness runs).
    local mem_flag=""
    local swap_flag=""
    if [[ -n "$BENCH_MEM" ]]; then
      mem_flag="--memory $BENCH_MEM"
      swap_flag="--memory-swap $BENCH_MEM"
    fi
    local cpu_flag=""
    [[ -n "$BENCH_CPUS" ]] && cpu_flag="--cpus $BENCH_CPUS"
    docker update $cpu_flag $mem_flag $swap_flag "$CONTAINER_NAME" >/dev/null || true
  fi

  # Wait for health (the image HEALTHCHECK does pg_isready + curl /).
  # Give the worker time to start and the seeded / to respond.
  log "waiting for stack health (:8080 + DB)"
  local deadline=$((SECONDS + 120))
  while [[ $SECONDS -lt $deadline ]]; do
    if curl -sf --max-time 2 http://localhost:8080/ >/dev/null 2>&1; then
      log "stack is up (HTTP responding)"
      return 0
    fi
    sleep 1
  done
  echo "timed out waiting for bench stack" >&2
  $COMPOSE logs --tail=50 postgres || true
  exit 1
}

stop_stack() {
  log "stopping bench stack"
  $COMPOSE down --volumes --remove-orphans >/dev/null 2>&1 || true
}

# Use the *in-image* pg-web (F.3) so the harness only needs Docker + checkout.
# Pass an explicit DATABASE_URL because the binary inside the PG container
# talks to 127.0.0.1:5432 (the postmaster in the same container).
push_bench_app() {
  log "pushing bench app via in-image CLI"
  $COMPOSE exec -T \
    -e DATABASE_URL="postgres://postgres:devpassword@localhost:5432/app" \
    postgres \
    /usr/local/bin/pg-web push --dir /bench --with-migrate
}

# Seed N rows of realistic-ish titles. Truncates first so repeated runs are
# bounded and deterministic in table size.
seed_todos() {
  local n=$1
  log "seeding $n bench_todos rows (truncate + insert)"
  $COMPOSE exec -T postgres psql -U postgres -d app -v ON_ERROR_STOP=1 <<SQL
    TRUNCATE public.bench_todos;
    INSERT INTO public.bench_todos (title)
    SELECT 'todo-' || g FROM generate_series(1, $n) g;
SQL
}

# Parse an oha result file into "reqs succ p50 p99" (p50/p99 normalised to ms,
# or "n/a" when oha printed NaN — which happens when ~all requests errored, i.e.
# the server isn't actually serving (e.g. the worker-self-termination regression
# fixed in 729eb93); a healthy run yields real numbers). oha's percentile lines
# carry an adaptive unit (ns / µs / ms / s), so we convert by unit.
_parse_oha() {
  # LC_ALL=C: oha result files can carry NUL / non-ASCII bytes (and the µs unit);
  # C locale keeps awk byte-safe and forces '.' decimal parsing.
  LC_ALL=C awk '
    function to_ms(v, u,   val) {
      if (v ~ /[Nn]a[Nn]/ || v=="") return "n/a"
      val = v + 0
      if (u=="ns") return sprintf("%.3f", val/1e6)
      if (u=="ms") return sprintf("%.3f", val)
      if (u=="s" || u=="sec" || u=="secs") return sprintf("%.3f", val*1000)
      return sprintf("%.3f", val/1e3)   # µs / us (microseconds) — the remaining oha unit
    }
    /Requests\/sec:/ { reqs=$2 }
    /Success rate:/  { succ=$NF }
    $1 ~ /^50(\.00)?%$/ { p50=to_ms($3,$4) }
    $1 ~ /^99(\.00)?%$/ { p99=to_ms($3,$4) }
    END {
      printf "%s %s %s %s", (reqs==""?"n/a":reqs), (succ==""?"n/a":succ), (p50==""?"n/a":p50), (p99==""?"n/a":p99)
    }
  ' "$1" 2>/dev/null || echo "n/a n/a n/a n/a"
}

# Record a workload's parsed metrics + emit the per-workload OK marker.
_bench_record() {
  local label="$1" out="$2" reqs succ p50 p99
  read -r reqs succ p50 p99 <<<"$(_parse_oha "$out")"
  BENCH_LABELS+=("$label"); BENCH_REQS+=("$reqs"); BENCH_SUCC+=("$succ")
  BENCH_P50+=("$p50"); BENCH_P99+=("$p99")
  mk_ok bench OK "$label  req/s=$reqs succ=$succ p50=${p50}ms p99=${p99}ms"
}

# Convenience: run oha with common flags, capture to a result file, record it.
# $1 = label for filename, $2 = url path, rest = extra oha args (e.g. -c 32 -z 15s)
# Raw histogram → result file always; streamed to terminal only in verbose.
run_oha() {
  local label=$1; shift
  local path=$1; shift
  local url="http://localhost:8080${path}"
  local outfile="$RESULTS_DIR/${label}.txt"
  mk_start bench "$label  ($url  $*)"
  if [[ "$TEST_MODE" == "verbose" ]]; then
    "$OHA_CMD" --no-tui --no-color -z 10s "$@" "$url" 2>&1 | tee "$outfile" || true
  else
    "$OHA_CMD" --no-tui --no-color -z 10s "$@" "$url" >"$outfile" 2>&1 || true
  fi
  # Also write a tiny summary line for easy grepping later.
  echo "RUN: label=$label url=$url args=$*" >> "$RESULTS_DIR/summary.txt"
  _bench_record "$label" "$outfile"
}

# The four workloads + the critical HOLB mixed run.
run_workloads() {
  : > "$RESULTS_DIR/summary.txt"

  # (a) static — no table read. Run at a couple of concurrencies to show
  # framing/Tera cost plateaus.
  seed_todos 0   # irrelevant
  run_oha "a-static-c1"   "/bench/static" -c 1
  run_oha "a-static-c32"  "/bench/static" -c 32
  run_oha "a-static-c128" "/bench/static" -c 128

  # (b) todo-list fetch+render at two realistic sizes.
  seed_todos 100
  run_oha "b-todos100-c1"   "/bench/todos" -c 1
  run_oha "b-todos100-c32"  "/bench/todos" -c 32
  run_oha "b-todos100-c128" "/bench/todos" -c 128

  seed_todos 10000
  run_oha "b-todos10k-c1"   "/bench/todos" -c 1
  run_oha "b-todos10k-c32"  "/bench/todos" -c 32
  run_oha "b-todos10k-c128" "/bench/todos" -c 128

  # (c) write path. Truncate to bound growth; short z because each write is heavier.
  seed_todos 0
  run_oha "c-write-c1"  "/bench/write" -c 1
  run_oha "c-write-c8"  "/bench/write" -c 8   # lower conc; writes contend

  # (d) + HOLB demo: the most important result.
  # Run a low-rate constant-arrival slow handler concurrently with a
  # realistic load on the fast todos path. The fast path's latency
  # distribution should visibly degrade even though its own queries are fast.
  # Use open-model (-q) for the slow injector so it doesn't self-throttle.
  seed_todos 100
  log "HOLB: starting slow injector (-q 3) + fast observer (-c 16) in parallel"
  local slow_log="$RESULTS_DIR/d-slow-injector.txt"
  local fast_log="$RESULTS_DIR/d-fast-under-slow.txt"
  "$OHA_CMD" --no-tui --no-color -q 3 -z 15s "http://localhost:8080/bench/slow" >"$slow_log" 2>&1 &
  local slow_pid=$!
  # Give the slow a moment to land and occupy the single thread.
  sleep 1
  mk_start bench "d-fast-under-slow (c=16 while slow -q 3 injector runs)"
  if [[ "$TEST_MODE" == "verbose" ]]; then
    "$OHA_CMD" --no-tui --no-color -c 16 -z 12s "http://localhost:8080/bench/todos" 2>&1 | tee "$fast_log" || true
  else
    "$OHA_CMD" --no-tui --no-color -c 16 -z 12s "http://localhost:8080/bench/todos" >"$fast_log" 2>&1 || true
  fi
  wait $slow_pid || true
  # Record it like any other workload so the gate covers it too. The slow
  # injector degrades this leg's *latency* (that is the whole point of the HOLB
  # experiment), NOT its success — so the >=99% success floor still applies (a
  # crater to 0% here is a dead worker), while its p99 ceiling uses the
  # intentionally-slow ~220ms baseline (see thresholds.sh).
  _bench_record "d-fast-under-slow" "$fast_log"

  # One more pure fast run at same conc for easy before/after in the report.
  seed_todos 100
  run_oha "b-todos100-c16-pure" "/bench/todos" -c 16

  # Self-test probe (BENCH_SELFTEST=1): a fast-labeled workload pointed at the
  # SLOW path. Its ~220ms p99 (vs the 5ms fast baseline => 15ms ceiling) and
  # ~1400 req/s (vs the 6000 floor) are a guaranteed, platform-independent double
  # breach that proves the gate + banner are live (030 B.1 #5 / acceptance #2).
  # Runs last so the real workload numbers above are untouched; BENCH_SELFTEST
  # also forces BENCH_STRICT on (see main) so the p99/req-s layer is evaluated.
  if [[ "${BENCH_SELFTEST:-}" == "1" ]]; then
    log "BENCH_SELFTEST=1 — injecting a guaranteed regression (fast label -> /bench/slow)"
    run_oha "selftest-slow-as-fast" "/bench/slow" -c 16
  fi
}

# Compact end-of-run table (always printed; the raw histograms stay in the files).
print_bench_table() {
  echo
  echo "== Bench summary — tier: $BENCH_TIER_LABEL (p50/p99 in ms; n/a = oha NaN ≈ server not serving) =="
  local i
  for i in "${!BENCH_LABELS[@]}"; do
    printf "  %-22s req/s=%-11s succ=%-8s p50=%-9s p99=%-9s\n" \
      "${BENCH_LABELS[$i]}" "${BENCH_REQS[$i]}" "${BENCH_SUCC[$i]}" "${BENCH_P50[$i]}" "${BENCH_P99[$i]}"
  done
}

# The headline result: fast-path latency with vs. without a concurrent slow
# handler. Two lines, no histograms required.
print_holb() {
  echo
  echo "== HOLB (head-of-line blocking): fast /bench/todos c=16 =="
  local pr ps pp50 pp99 ur us up50 up99
  read -r pr ps pp50 pp99 <<<"$(_parse_oha "$RESULTS_DIR/b-todos100-c16-pure.txt")"
  read -r ur us up50 up99 <<<"$(_parse_oha "$RESULTS_DIR/d-fast-under-slow.txt")"
  printf "  baseline (no interference): req/s=%-11s succ=%-8s p50=%-9s p99=%-9s\n" "$pr" "$ps" "$pp50" "$pp99"
  printf "  under slow injector (-q 3): req/s=%-11s succ=%-8s p50=%-9s p99=%-9s\n" "$ur" "$us" "$up50" "$up99"
  echo "  (single-worker: the fast path's p99 degrades sharply under the slow handler; multi-worker should keep it flat)"
}

# ── the loud, always-on, itemized regression banner (prompt 030 B.2) ─────────
# A visually-framed, greppable, one-line-per-breach block that is IMPOSSIBLE to
# miss when scrolling a long log. ALWAYS prints — every TEST_MODE incl. `short`
# (a regression is never suppressed) and on infra/early-exit (via bench_on_exit).
# Print-once (BENCH_BANNER_PRINTED) so main()'s gate-fail path and the EXIT trap
# can't double it. ASCII `!`-frame (no box-drawing) so it survives non-TTY/CI
# logs and `grep`. It is IN ADDITION to the machine-parseable PGWEB-BENCH line.
#   $1 = tier tag   $2 = short headline reason
print_bench_banner() {
  [[ "${BENCH_BANNER_PRINTED:-0}" == "1" ]] && return 0
  BENCH_BANNER_PRINTED=1
  local tier="$1" reason="$2" n="${#FAIL_LABEL[@]}" i bar
  # Pure-ASCII frame + headline (no glyphs) so it survives non-TTY/CI logs + grep.
  bar="!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
  echo
  echo "$bar"
  printf '!!  BENCH REGRESSION DETECTED  --  tier=%s  --  %d check(s) failed\n' "$tier" "$n"
  [[ -n "$reason" ]] && printf '!!  %s\n' "$reason"
  echo "!!"
  printf '!!  %-22s %-8s %-13s %s %-24s %s\n' "WORKLOAD" "METRIC" "OBSERVED" " " "THRESHOLD" "DELTA"
  for i in "${!FAIL_LABEL[@]}"; do
    printf '!!  %-22s %-8s %-13s %s %-24s %s\n' \
      "${FAIL_LABEL[$i]}" "${FAIL_METRIC[$i]}" "${FAIL_OBS[$i]}" "${FAIL_REL[$i]}" "${FAIL_THRESH[$i]}" "${FAIL_DELTA[$i]}"
  done
  echo "!!"
  echo "!!  hint: success 0% / p99 n/a on a leg => worker not serving -- check 'docker logs'"
  echo "!!  raw:  $RESULTS_DIR/<label>.txt   (per-workload oha histograms)"
  echo "$bar"
}

# ── the gate (prompt 030 B.1) ────────────────────────────────────────────────
# Replaces the old "did it serve at all" placeholder. Walks EVERY recorded
# workload and applies, per the design in bench/thresholds.sh:
#   1. success floor (ALWAYS): succ >= BENCH_MIN_SUCCESS — platform-independent,
#      catches a dead/crash-looping/not-serving worker (the 016 regression).
#   2. p99 ceiling   (BENCH_STRICT): p99 <= baseline * BENCH_P99_MARGIN.
#   3. req/s floor   (BENCH_STRICT): successful req/s (= reqs * succ/100, NOT
#      oha's error-inclusive Requests/sec) >= baseline * BENCH_RPS_FLOOR_FRAC.
# $BENCH_TIER_TAG selects the baseline column. Each breach appends to FAIL_*
# (for the banner). Sets THRESHOLD_NOTE (summary for the PGWEB-BENCH line) and
# returns 0 (ok) / 1 (fail).
THRESHOLD_NOTE=""
_gate_add_fail() {   # label metric observed rel threshold delta
  FAIL_LABEL+=("$1"); FAIL_METRIC+=("$2"); FAIL_OBS+=("$3"); FAIL_REL+=("$4")
  FAIL_THRESH+=("$5"); FAIL_DELTA+=("${6:-}")
}
evaluate_gate() {
  local tier="$BENCH_TIER_TAG" n="${#BENCH_LABELS[@]}" checks=0 fails_before
  fails_before=${#FAIL_LABEL[@]}

  if [[ "$n" -eq 0 ]]; then
    THRESHOLD_NOTE="no workloads recorded (oha produced nothing?)"
    _gate_add_fail "(all)" "workloads" "0" ">=" "1" "nothing ran"
    return 1
  fi

  local i label reqs succ p99 succ_num base ceil floor rps delta
  for i in "${!BENCH_LABELS[@]}"; do
    label="${BENCH_LABELS[$i]}"; reqs="${BENCH_REQS[$i]}"
    succ="${BENCH_SUCC[$i]}"; p99="${BENCH_P99[$i]}"
    # success% as a number; n/a / non-numeric -> 0 so a not-serving leg always breaches.
    succ_num=$(LC_ALL=C awk -v s="$succ" 'BEGIN{gsub(/%/,"",s); if(s ~ /^[0-9.]+$/) printf "%s", s; else print "0"}')

    # 1) success floor — ALWAYS.
    checks=$((checks+1))
    if ! LC_ALL=C awk -v s="$succ_num" -v m="$BENCH_MIN_SUCCESS" 'BEGIN{exit !(s+0 >= m+0)}'; then
      delta=$(LC_ALL=C awk -v s="$succ_num" -v m="$BENCH_MIN_SUCCESS" 'BEGIN{printf "%.1fpp under", m-s}')
      _gate_add_fail "$label" "success" "${succ_num}%" "<" "floor ${BENCH_MIN_SUCCESS}%" "$delta"
    fi

    # 2) p99 ceiling + 3) req/s floor — only under STRICT, only where a baseline exists.
    if [[ "${BENCH_STRICT:-}" == "1" ]]; then
      base=$(bench_baseline_p99 "$tier" "$label")
      if [[ -n "$base" ]]; then
        checks=$((checks+1))
        ceil=$(LC_ALL=C awk -v b="$base" -v mg="$BENCH_P99_MARGIN" 'BEGIN{printf "%.3f", b*mg}')
        if [[ "$p99" == "n/a" ]]; then
          _gate_add_fail "$label" "p99" "n/a" ">" "ceil ${ceil}ms (${base}x${BENCH_P99_MARGIN})" "not serving"
        elif ! LC_ALL=C awk -v v="$p99" -v c="$ceil" 'BEGIN{exit !(v+0 <= c+0)}'; then
          delta=$(LC_ALL=C awk -v v="$p99" -v c="$ceil" 'BEGIN{printf "%.1fx over", v/c}')
          _gate_add_fail "$label" "p99" "${p99}ms" ">" "ceil ${ceil}ms (${base}x${BENCH_P99_MARGIN})" "$delta"
        fi
      fi
      base=$(bench_baseline_rps "$tier" "$label")
      if [[ -n "$base" ]]; then
        checks=$((checks+1))
        floor=$(LC_ALL=C awk -v b="$base" -v fr="$BENCH_RPS_FLOOR_FRAC" 'BEGIN{printf "%.0f", b*fr}')
        rps=$(LC_ALL=C awk -v r="$reqs" -v s="$succ_num" 'BEGIN{ if(r ~ /^[0-9.]+$/) printf "%.0f", r*s/100; else print "0"}')
        if ! LC_ALL=C awk -v v="$rps" -v f="$floor" 'BEGIN{exit !(v+0 >= f+0)}'; then
          delta=$(LC_ALL=C awk -v v="$rps" -v f="$floor" 'BEGIN{printf "%.0f short", f-v}')
          _gate_add_fail "$label" "req/s" "$rps" "<" "floor ${floor} (${base}x${BENCH_RPS_FLOOR_FRAC})" "$delta"
        fi
      fi
    fi
  done

  local nfail=$(( ${#FAIL_LABEL[@]} - fails_before )) strict_note
  if [[ "${BENCH_STRICT:-}" == "1" ]]; then strict_note="strict (success+p99+req/s)"; else strict_note="success-floor only; BENCH_STRICT=1 for p99/req-s"; fi
  if [[ "$nfail" -eq 0 ]]; then
    THRESHOLD_NOTE="all ${n} workloads pass ${checks} checks [${strict_note}], success>=${BENCH_MIN_SUCCESS}%"
    return 0
  fi
  THRESHOLD_NOTE="${nfail} of ${checks} checks FAILED across ${n} workloads [${strict_note}] -- see banner"
  return 1
}

main() {
  trap bench_on_exit EXIT

  # Self-test (prompt 030 B.1 #5): force the strict layer on so the injected
  # /bench/slow probe (added last in run_workloads) is evaluated against the fast
  # ceilings and deterministically flips OVERALL=fail + fires the banner.
  if [[ "${BENCH_SELFTEST:-}" == "1" ]]; then
    BENCH_STRICT=1
    log "BENCH_SELFTEST=1 — forcing BENCH_STRICT=1; a guaranteed regression will be injected"
  fi

  # Self-healing lock + unconditional reclaim (prompt 029) — same guarantee as
  # test-all.sh. Skipped when nested under test-all (PGWEB_NESTED=1), which
  # already holds the lock + reclaimed, so RUN_BENCH=1 never deadlocks here.
  if [[ "${PGWEB_NESTED:-}" != "1" ]]; then
    acquire_lock "$PGWEB_LOCKDIR"   # blocks ONLY on a genuinely-live concurrent run
    reclaim_environment             # safe — holding the exclusive lock
  fi

  require_docker
  ensure_image
  ensure_oha

  start_stack
  push_bench_app

  run_workloads

  log "raw results in $RESULTS_DIR/"

  # --- compact reporting (prompt 028) ---
  print_bench_table
  print_holb

  local overall
  if evaluate_gate; then overall="ok"; else overall="fail"; fi
  if [[ "$overall" == "fail" ]]; then
    # Loud, itemized, always-printed (every TEST_MODE) — before the verdict line
    # so the verdict stays the last greppable output.
    print_bench_banner "$BENCH_TIER_TAG" "gate failed: $THRESHOLD_NOTE"
  else
    # Nice-to-have green confirmation so a clean run is also unambiguous (B.2).
    mk_ok bench GATE "$THRESHOLD_NOTE"
  fi
  echo
  printf 'PGWEB-BENCH tier=%s workloads=%d threshold="%s"  OVERALL=%s\n' \
    "$BENCH_TIER_TAG" "${#BENCH_LABELS[@]}" "$THRESHOLD_NOTE" "$overall"
  BENCH_VERDICT_PRINTED=1

  log "Next: review outputs, fill docs/BENCHMARKS.md, reconcile VISION.md:58"
  # Do not auto-stop here; the EXIT trap will. For CI-style runs we want cleanup,
  # which the trap does.
  [[ "$overall" == "ok" ]]
}

# Run main only when executed (not when sourced) so the gate/banner functions can
# be unit-tested in isolation. Every real invocation (`bash bench/run.sh`,
# test-all's `bash "$REPO_ROOT/bench/run.sh"`) executes the file, so this is true.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi
