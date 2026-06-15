#!/usr/bin/env bash
# Full local test run: five tiers.
#   1)  SQL / pgrx #[pg_test]
#   2a) HTTP smoke against a running extension
#   2b) CLI unit + hermetic integration tests
#   3)  Docker E2E — boots the test image (rtaylor96/pg-web:latest) in a container and drives
#       the full CRUD flow against examples/todo
#   4)  CLI black-box smoke — init → up → push → break 3 ways → down,
#       exercising the user-visible CLI stdout and HTTP bodies
#
# Tier 3 is mandatory. Tier 4 is also mandatory — it's what catches
# gotchas that fall between the rust tests (wrong image baked, stray
# pgrx dev PG shadowing :8080, docker-compose service rename, etc.).
# Both need Docker + the test image (currently rtaylor96/pg-web:latest
# while the canonical pgweb/ org namespace is still being claimed).
#
# This is what CI should invoke.
#
# ── Reporting (prompt 028) ───────────────────────────────────────────────
# Output is reported like a build system: a paired START/END marker per phase,
# real x/x counts per tier, and a single un-truncatable `PGWEB-RESULT … OVERALL`
# line as the LAST output. Three verbosity modes (env TEST_MODE or flags):
#   errors  (DEFAULT) — markers + compact results; on failure auto-surface the
#                       captured detail for the failing items only (NO re-run).
#   short             — markers + compact results only; never auto-expand.
#   verbose           — stream all raw cargo/docker output too (today's behavior).
# Each phase's full output is captured to $RUN_DIR/<phase>.log regardless of mode
# (kept after the run). The auto-surfaced detail in `errors` mode IS that capture
# — we never re-execute a failed tier (re-running flaky/expensive Docker tiers is
# exactly what 029 exists to eliminate; a fresh run can also mask the failure).
# A green claim requires `OVERALL=PASS` with real x/x (failed=0) for every
# mandatory tier; SKIP / missing counts / a missing verdict line all mean NOT green.
set -euo pipefail

# Defaults that must be available even for very early cleanup calls (e.g. the
# stop_pgrx_dev_pg we invoke right after the lock).
PG_MAJOR="${PG_MAJOR:-17}"

# macOS: hold a power assertion for the duration of the run. A full run is
# 30+ minutes; without this, an unattended Mac enters "Maintenance Sleep"
# and freezes every tier mid-flight — and because sleep also pauses the
# monotonic clocks that cargo/libtest use for their "finished in Xs" lines,
# the stall is invisible in any log (the run just takes an hour longer than
# the sum of its parts). See docs/internal/TESTING-SETUP.md § macOS sleep.
# No-op on Linux/CI (no caffeinate binary).
if [[ -z "${PGWEB_CAFFEINATED:-}" ]] && command -v caffeinate >/dev/null 2>&1; then
    export PGWEB_CAFFEINATED=1
    exec caffeinate -is bash "$0" "$@"
fi

# Opt-in per-line timestamps (TEST_TS=1). Uses a tiny awk stamper so any stall
# (macOS sleep, blocked I/O, etc.) becomes visible in wall time.
# Always-on: per-tier wall durations and the end-of-run status table.
if [[ "${TEST_TS:-}" == "1" ]]; then
    if command -v gawk >/dev/null 2>&1; then
        exec > >(gawk '{ print strftime("[%F %T]"), $0; fflush(); }' ) 2>&1
    else
        # Portable fallback (perl one-liner); not as pretty but sufficient.
        exec > >(perl -ne 'chomp; print "[" . localtime() . "] $_\n"; $|=1;' ) 2>&1
    fi
fi

# Shared reporting helpers (markers, libtest parsing, surfacing). Sourced after
# the caffeinate re-exec + TEST_TS pipe so its TTY/glyph detection sees the final
# stdout (a pipe under TEST_TS → ASCII keywords; a terminal → unicode glyphs).
PGWEB_SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=scripts/report-lib.sh
source "$PGWEB_SCRIPT_DIR/report-lib.sh"
# Shared idempotency primitives (prompt 029): image freshness, unconditional
# environment reclaim, the self-healing cross-run lock, pgrx stop, the unified
# image tag. Export PGWEB_REPO_ROOT from our own $0 (reliable regardless of the
# caller's cwd) so the lib + the content hash anchor to THIS repo.
export PGWEB_REPO_ROOT="$(cd "$PGWEB_SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/harness.sh
source "$PGWEB_SCRIPT_DIR/lib/harness.sh"

# Output mode: env TEST_MODE, overridable by --short/--errors/--verbose flags.
for _arg in "$@"; do
    case "$_arg" in
        --short)   TEST_MODE=short ;;
        --errors)  TEST_MODE=errors ;;
        --verbose) TEST_MODE=verbose ;;
    esac
done
TEST_MODE="${TEST_MODE:-errors}"
export TEST_MODE

# Per-run capture dir. Unique per PID so sequential/sibling runs never clobber
# each other. Kept after the run (NOT reaped here — 029's startup hygiene owns
# retention) so a failure can be inspected post-hoc.
RUN_DIR="${RUN_DIR:-/tmp/pg-web-test-all-$$}"
mkdir -p "$RUN_DIR"

# Self-healing cross-run lock + unconditional environment reclaim (prompt 029).
# Both serialize the harness (concurrent test-all.sh / bench runs are the #1
# source of :8080 fights — pgrx dev PG BGW + smoke compose + bench compose all
# want the port — and /tmp/pg-web-smoke races) AND guarantee a clean slate every
# run with zero manual hygiene: a portable mkdir-based lock (atomic on Linux +
# macOS) that auto-reclaims a dead owner's stale dir (no FORCE=1 after a crash),
# and reclaim_environment frees :8080 + removes our own leftover
# containers/stacks/dirs from any prior (crashed, killed, or finished) run.
#
# Skipped only when we are a nested child of another pg-web run that already
# holds the lock + reclaimed (RUN_BENCH=1 runs bench/run.sh in-process below),
# so we never deadlock on our own lock or fight our own containers mid-run.
if [[ "${PGWEB_NESTED:-}" != "1" ]]; then
    trap 'release_lock "$PGWEB_LOCKDIR"' EXIT INT TERM
    acquire_lock "$PGWEB_LOCKDIR"   # PID + age self-healing; blocks ONLY on a genuinely-live concurrent run
    reclaim_environment             # safe now — holding the exclusive lock means no concurrent pg-web run
fi
# Children inherit this and skip their own lock + reclaim.
export PGWEB_NESTED=1

# ── Reporting state + helpers (prompt 028) ───────────────────────────────
# Per-tier results carry real counts + failing names + the captured log path so
# the end-of-run table and the PGWEB-RESULT verdict are computed, never assumed.
declare -a TIER_NAME TIER_STATUS TIER_DUR TIER_PASS TIER_TOTAL TIER_FAIL TIER_LOG
record_tier() {
    # <phase> <PASS|FAIL|SKIP> <dur-secs> <passed> <total> <failnames> <logpath>
    TIER_NAME+=("$1");  TIER_STATUS+=("$2"); TIER_DUR+=("$3")
    TIER_PASS+=("$4");  TIER_TOTAL+=("$5");  TIER_FAIL+=("$6"); TIER_LOG+=("$7")
}
status_of() {
    local want="$1" i
    for i in "${!TIER_NAME[@]}"; do
        [[ "${TIER_NAME[$i]}" == "$want" ]] && { echo "${TIER_STATUS[$i]}"; return; }
    done
    echo "MISSING"
}

# Run a phase, capturing combined output to a log. verbose also tees to the
# terminal. Returns the command's real exit code (PIPESTATUS under tee).
# Callers wrap in `set +e … rc=$? … set -e` so a failure never aborts the suite
# before the summary prints.
run_phase() {
    local logfile="$1"; shift
    if [[ "$TEST_MODE" == "verbose" ]]; then
        "$@" 2>&1 | tee "$logfile"
        return "${PIPESTATUS[0]}"
    fi
    "$@" >"$logfile" 2>&1
}

# Turn a captured libtest log + rc into a marker + recorded result (+ surfaced
# detail in errors mode). Used by tiers 1, 2a, 2b, 3.
finalize_libtest_tier() {
    local phase="$1" log="$2" rc="$3" dur="$4"
    local counts passed failed total names
    counts=$(parse_libtest_counts "$log")
    passed=${counts%% *}; failed=${counts##* }
    total=$(( passed + failed ))
    if [[ "$total" -eq 0 ]]; then
        # Nothing ran: pgrx not ready (soft tiers) or a compile error (hard tiers).
        # Either way this is NOT a green tier — recorded SKIP, which reads as
        # not-green in the verdict and (for hard tiers) still fails the exit code.
        mk_skip "$phase" "0 tests ran (rc=$rc) — see $log"
        record_tier "$phase" SKIP "$dur" "0" "0" "" "$log"
        [[ "$TEST_MODE" == "errors" ]] && surface_log_tail "$phase" "$log" 40
        return 0
    fi
    if [[ "$rc" -ne 0 || "$failed" -gt 0 ]]; then
        names=$(collect_failure_names "$log")
        mk_fail "$phase" "$passed/$total  failing: ${names:-<rc=$rc; see log>}" "${dur}s"
        record_tier "$phase" FAIL "$dur" "$passed" "$total" "$names" "$log"
        [[ "$TEST_MODE" == "errors" ]] && surface_libtest_failures "$log"
        return 0
    fi
    mk_pass "$phase" "$passed/$total" "${dur}s"
    record_tier "$phase" PASS "$dur" "$passed" "$total" "" "$log"
}

# Tier 4 is bash-driven; count the machine-parseable PGWEB-SMOKE section markers
# (smoke-cli.sh aborts on first failure, so at most one FAIL marker exists).
finalize_smoke_tier() {
    local log="$1" rc="$2" dur="$3"
    local passed failed total failline
    # -a + LC_ALL=C: the smoke log contains NUL bytes (captured docker/curl
    # output); without -a, grep treats it as binary and the count comes back
    # empty → a false FAIL. This makes the count deterministic across grep impls.
    passed=$(LC_ALL=C grep -acE 'PGWEB-SMOKE step=[0-9]+ OK' "$log" 2>/dev/null || true)
    failed=$(LC_ALL=C grep -acE 'PGWEB-SMOKE step=[0-9]+ FAIL' "$log" 2>/dev/null || true)
    passed=${passed:-0}; failed=${failed:-0}
    total=$(( passed + failed ))
    if [[ "$rc" -ne 0 || "$failed" -gt 0 || "$total" -eq 0 ]]; then
        failline=$(LC_ALL=C grep -aE 'PGWEB-SMOKE step=[0-9]+ FAIL' "$log" 2>/dev/null | head -1 || true)
        mk_fail tier4 "$passed/$total  ${failline:-<smoke aborted; rc=$rc>}" "${dur}s"
        record_tier tier4 FAIL "$dur" "$passed" "$total" "${failline:-rc=$rc}" "$log"
        [[ "$TEST_MODE" == "errors" ]] && surface_log_tail tier4 "$log" 50
    else
        mk_pass tier4 "$passed/$total" "${dur}s"
        record_tier tier4 PASS "$dur" "$passed" "$total" "" "$log"
    fi
}

print_summary_table() {
    echo
    echo "== Per-tier summary =="
    local i cnt
    for i in "${!TIER_NAME[@]}"; do
        if [[ "${TIER_STATUS[$i]}" == "SKIP" ]]; then cnt="(skipped)"; else cnt="${TIER_PASS[$i]}/${TIER_TOTAL[$i]}"; fi
        printf "  %-7s %-4s %-10s (%ss)\n" "${TIER_NAME[$i]}" "${TIER_STATUS[$i]}" "$cnt" "${TIER_DUR[$i]}"
    done
    printf "  %-7s %-4s\n" "bench" "$(printf '%s' "$BENCH_STATUS" | tr '[:lower:]' '[:upper:]')"
}

# The un-truncatable verdict (prompt 028 §3). ASCII-only, last substantive output.
# OVERALL=PASS iff every mandatory tier is x/x (failed=0) and none is SKIP/missing.
# bench=skip does NOT fail OVERALL; bench=fail does.
emit_result() {
    local parts="" overall="PASS" i st
    for i in "${!TIER_NAME[@]}"; do
        st="${TIER_STATUS[$i]}"
        if [[ "$st" == "PASS" ]]; then
            parts+=" ${TIER_NAME[$i]}=${TIER_PASS[$i]}/${TIER_TOTAL[$i]}"
        elif [[ "$st" == "SKIP" ]]; then
            parts+=" ${TIER_NAME[$i]}=skip"; overall="FAIL"
        else
            parts+=" ${TIER_NAME[$i]}=${TIER_PASS[$i]}/${TIER_TOTAL[$i]}"; overall="FAIL"
        fi
    done
    parts+=" bench=${BENCH_STATUS}"
    [[ "$BENCH_STATUS" == "fail" ]] && overall="FAIL"
    echo
    printf 'PGWEB-RESULT %s  OVERALL=%s\n' "${parts# }" "$overall"
    for i in "${!TIER_NAME[@]}"; do
        st="${TIER_STATUS[$i]}"
        if [[ "$st" == "FAIL" ]]; then
            printf 'PGWEB-FAIL   %-6s failing: %s  (log: %s)\n' \
                "${TIER_NAME[$i]}" "${TIER_FAIL[$i]:-<see log>}" "${TIER_LOG[$i]}"
        elif [[ "$st" == "SKIP" ]]; then
            printf 'PGWEB-FAIL   %-6s SKIPPED — 0 tests ran  (log: %s)\n' \
                "${TIER_NAME[$i]}" "${TIER_LOG[$i]}"
        fi
    done
    PGWEB_OVERALL="$overall"
}

BENCH_STATUS="skip"
IMAGE_BUILD_FAILED=0

PG_MAJOR="${PG_MAJOR:-17}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Early, non-fatal diagnostics so a full `scripts/test-all.sh` run on a
# fresh or macOS dev machine surfaces the "system" requirements up front
# instead of failing deep inside a tier with a confusing toolchain error.
# The actual gates remain hard (no silent skip for tier 3/4).
echo "== Environment sanity (informational) =="
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  echo "  docker: reachable"
else
  echo "  docker: NOT reachable (tiers 3+4 will hard-fail with actionable message; they are mandatory)"
fi

pgrx_pg_config=$(ls -1 "$HOME/.pgrx/${PG_MAJOR}."*/pgrx-install/bin/pg_config 2>/dev/null | head -1 || true)
if [[ -x "$pgrx_pg_config" ]]; then
  echo "  ~/.pgrx pg$PG_MAJOR: usable pg_config found ($pgrx_pg_config)"
else
  echo "  ~/.pgrx pg$PG_MAJOR: NO usable pg_config (Tier 1 + 2a will print guidance and be skipped)"
  echo "    To enable them:  cargo pgrx init --pg$PG_MAJOR download"
  echo "    Then append:     shared_preload_libraries = 'pg_web_ext'   to the data dir's postgresql.conf"
fi
echo "  report mode: $TEST_MODE  (errors=compact+auto-surface | short=compact | verbose=stream all output)"
echo "  per-run logs: $RUN_DIR/   (kept after the run for post-hoc inspection)"
echo

# stop_pgrx_dev_pg (frees :8080 from the pgrx dev PG's BGW) now lives in the
# shared lib — reclaim_environment already called it at the top of the run; we
# call it again before tier 4 because tiers 1/2a leave the dev PG up. Idempotent;
# the data dir is preserved. (Full rationale in scripts/lib/harness.sh.)

# The image tag + freshness check + build wrapper now live in the shared lib
# (scripts/lib/harness.sh) so test-all.sh, bench/run.sh, and build-image.sh
# agree on ONE tag and ONE content-hash definition (prompt 029 #1/#2/#3). The
# old mtime fast-path is gone: it caused false rebuilds on git-stash/checkout
# mtime noise and could miss content edits that didn't advance mtime. The
# content hash (whole-tree-minus-denylist) is now the sole, provably-complete
# source of truth — see ensure_image_fresh / compute_src_hash in the lib.
TEST_IMAGE="$(pgweb_image)"

echo "== Tier 1 — SQL tests (cargo pgrx test pg$PG_MAJOR) =="
mk_start tier1 "cargo pgrx test pg$PG_MAJOR"
t1_start=$(date +%s)
set +e
run_phase "$RUN_DIR/tier1.log" bash -c "cd crates/pg_web_ext && cargo pgrx test 'pg$PG_MAJOR'"
tier1_rc=$?
set -e
t1_dur=$(( $(date +%s) - t1_start ))
finalize_libtest_tier tier1 "$RUN_DIR/tier1.log" "$tier1_rc" "$t1_dur"
if [[ "$(status_of tier1)" != "PASS" ]]; then
  echo "    hint: Tier 1 needs the pgrx dev Postgres for pg$PG_MAJOR. Enable with:"
  echo "          cargo pgrx init --pg$PG_MAJOR download   (then add shared_preload_libraries='pg_web_ext' to ~/.pgrx/data-$PG_MAJOR/postgresql.conf)"
fi

echo
echo "== Tier 2a — HTTP smoke (scripts/test-http.sh) =="
# Invoked via `bash` (not direct exec) so the script doesn't need the
# +x bit. Edit-via-UNC-mount writes from Claude tools land as 0644
# root-owned, dropping +x; using `bash <script>` sidesteps that
# without needing manual chmod after every doc-touching commit.
mk_start tier2a "HTTP smoke (test-http.sh: reinstall .so → restart PG → CREATE EXTENSION → wait :8080 → http_smoke)"
t2a_start=$(date +%s)
set +e
run_phase "$RUN_DIR/tier2a.log" bash "$REPO_ROOT/scripts/test-http.sh"
tier2a_rc=$?
set -e
t2a_dur=$(( $(date +%s) - t2a_start ))
finalize_libtest_tier tier2a "$RUN_DIR/tier2a.log" "$tier2a_rc" "$t2a_dur"
if [[ "$(status_of tier2a)" != "PASS" ]]; then
  echo "    hint: usually the same pgrx readiness issue as Tier 1, or a tier-2a-specific cause:"
  echo "          missing dev DB → createdb -h localhost -p 288$PG_MAJOR pg_web_ext ; ':8080 TIMEOUT' with a FATAL loop → BGW crashing (see $RUN_DIR/tier2a.log)"
fi

echo
echo "== Tier 2b — CLI tests (cargo test -p pg-web) =="
mk_start tier2b "cargo test -p pg-web --no-fail-fast"
t2b_start=$(date +%s)
set +e
run_phase "$RUN_DIR/tier2b.log" cargo test -p pg-web --no-fail-fast
tier2b_rc=$?
set -e
t2b_dur=$(( $(date +%s) - t2b_start ))
finalize_libtest_tier tier2b "$RUN_DIR/tier2b.log" "$tier2b_rc" "$t2b_dur"

echo
echo "== Tier 3 — Docker E2E ($TEST_IMAGE + examples/todo) =="
mk_start image "freshness check (content-hash)"
set +e
ensure_image_fresh "$TEST_IMAGE" "$RUN_DIR/image-build.log"
img_rc=$?
set -e
t3_start=$(date +%s)

# Canary preflight (prompt 025 #4): boot *one* container and give it a short
# ~30s deadline. If / never answers, print the container logs tail and abort
# tier 3 immediately. This turns "broken worker → 13 × 60 s of identical
# timeouts" into a <90 s failure with the root cause (e.g. role nologin,
# missing preload, crash loop) visible in the harness output.
do_tier3_canary() {
    local cname="pgweb-canary-$$" clog="$RUN_DIR/tier3-canary.log"
    docker run --rm -d --name "$cname" \
        -e POSTGRES_PASSWORD=testpw -e POSTGRES_DB=app \
        -P "$TEST_IMAGE" >/dev/null 2>&1 || {
        echo "  canary: docker run failed to start probe container"
        return 1
    }
    # Resolve the mapped HTTP port (testcontainers-style -P random).
    local http_mapped
    http_mapped=$(docker port "$cname" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || echo "")
    if [[ -z "$http_mapped" ]]; then
        # Fallback: try the default in case of --expose without -P mapping quirks.
        http_mapped=8080
    fi
    local dl=$(( $(date +%s) + 30 ))
    local ready=0
    while [ "$(date +%s)" -lt "$dl" ]; do
        if curl -sf "http://127.0.0.1:${http_mapped}/" >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.5
    done
    if [[ "$ready" == "1" ]]; then
        mk_ok tier3 CANARY "serving (mapped :$http_mapped)"
        docker rm -f "$cname" >/dev/null 2>&1 || true
        return 0
    fi
    docker logs --tail 30 "$cname" >"$clog" 2>&1 || true
    echo "  === TIER 3 CANARY ABORT: / never answered within 30 s — last 30 log lines ($clog) ==="
    tail -30 "$clog" 2>/dev/null | sed 's/^/    /'
    echo "  === (broken BGW: role nologin, missing preload, crash loop, etc.) ==="
    docker rm -f "$cname" >/dev/null 2>&1 || true
    return 1
}

if [[ "$img_rc" -ne 0 ]]; then
    # A failed image build is a hard prerequisite failure — tier 3 cannot run.
    IMAGE_BUILD_FAILED=1
    t3_dur=$(( $(date +%s) - t3_start ))
    mk_fail tier3 "image build failed — cannot run E2E" "${t3_dur}s"
    record_tier tier3 FAIL "$t3_dur" "0" "0" "image-build-failed" "$RUN_DIR/image-build.log"
else
    mk_start tier3 "canary probe GET /"
    set +e
    do_tier3_canary
    canary_rc=$?
    set -e
    if [[ "$canary_rc" -ne 0 ]]; then
        # Do not run the 14 tests (they would each burn another 60 s).
        t3_dur=$(( $(date +%s) - t3_start ))
        mk_fail tier3 "canary ABORT — broken worker (logs above)" "${t3_dur}s"
        record_tier tier3 FAIL "$t3_dur" "0" "0" "canary-abort" "$RUN_DIR/tier3-canary.log"
    else
        mk_start tier3 "docker_e2e (--ignored --test-threads=1 --no-fail-fast)"
        set +e
        # Sequential (--test-threads=1) to avoid 14 containers starting at once
        # (Docker Desktop + macOS struggles with concurrent startup + 30s waits).
        # --no-fail-fast so every failing name appears in one pass.
        run_phase "$RUN_DIR/tier3.log" \
            cargo test -p pg-web --test docker_e2e --no-fail-fast -- --ignored --test-threads=1
        tier3_rc=$?
        set -e
        t3_dur=$(( $(date +%s) - t3_start ))
        finalize_libtest_tier tier3 "$RUN_DIR/tier3.log" "$tier3_rc" "$t3_dur"
    fi
fi

# Reclaim :8080 from the pgrx dev PG before tier 4's docker stack
# tries to bind it. (When Tier 1 or 2a actually ran, they leave the pgrx PG up;
# tiers 2b + 3 do not. Safe to stop here either way.)
echo
echo "== Reclaiming :8080 for tier 4 =="
stop_pgrx_dev_pg

echo
echo "== Tier 4 — CLI black-box smoke (scripts/smoke-cli.sh) =="
# Use a unique smoke directory by default (PID-based). This lets multiple
# sequential runs (or the integrated RUN_BENCH=1 path) coexist without
# clobbering /tmp/pg-web-smoke or its docker compose project.
# Users can still override with SMOKE_DIR=... if they want a stable name.
: "${SMOKE_DIR:=/tmp/pg-web-smoke-$$}"
export SMOKE_DIR
mk_start tier4 "smoke-cli ($SMOKE_DIR)"
t4_start=$(date +%s)
set +e
run_phase "$RUN_DIR/tier4.log" bash "$REPO_ROOT/scripts/smoke-cli.sh"
tier4_rc=$?
set -e
t4_dur=$(( $(date +%s) - t4_start ))
finalize_smoke_tier "$RUN_DIR/tier4.log" "$tier4_rc" "$t4_dur"

# 015 benchmark (opt-in, heavy). Full matrix with oha under constrained + unconstrained
# tiers + HOLB experiment. A future lightweight bench-smoke (short duration + generous
# p99 bound) could be added behind RUN_BENCH_SMOKE=1 without bloating every CI run.
# The goal is catching accidental throughput regressions before they reach prod.
#
# bench/run.sh self-reports (per-workload one-liners + HOLB before/after + a
# PGWEB-BENCH … OVERALL=ok|fail line); we tee it (capturing + showing the
# compact summary in errors/short, the full stream in verbose) and map its exit
# code to the top-level bench=ok|fail. bench is opt-in + heavy, so a bench
# failure is treated as soft for the EXIT code (fatal only under STRICT/CI) but
# still flips OVERALL=FAIL in the verdict line.
if [[ "${RUN_BENCH:-}" == "1" ]]; then
  echo
  echo "== Opt-in Tier (015) — Concurrency/throughput benchmark (bench/run.sh) =="
  # Run unconstrained first (comparison), then the 1c/2g primary tier that the VISION
  # claim was about. The harness itself documents hardware, tool, and caveats.
  set +e
  bash "$REPO_ROOT/bench/run.sh" 2>&1 | tee "$RUN_DIR/bench-unconstrained.log"
  bench_rc1=${PIPESTATUS[0]}
  BENCH_CPUS=1 BENCH_MEM=2g bash "$REPO_ROOT/bench/run.sh" 2>&1 | tee "$RUN_DIR/bench-1c2g.log"
  bench_rc2=${PIPESTATUS[0]}
  set -e
  if [[ "$bench_rc1" -eq 0 && "$bench_rc2" -eq 0 ]]; then
    BENCH_STATUS="ok"
    mk_ok bench DONE "unconstrained + 1c/2g both ok (see PGWEB-BENCH lines above)"
  else
    BENCH_STATUS="fail"
    mk_fail bench "unconstrained rc=$bench_rc1  constrained rc=$bench_rc2 (see PGWEB-BENCH lines above)"
  fi
else
  mk_skip bench "set RUN_BENCH=1 to include the 015 benchmark"
fi

echo
echo "== Test run complete =="
print_summary_table

# Human-readable line (kept verbatim for CI greps: "All tiers completed successfully.").
if [[ "$(status_of tier1)" == "PASS" && "$(status_of tier2a)" == "PASS" \
   && "$(status_of tier2b)" == "PASS" && "$(status_of tier3)" == "PASS" \
   && "$(status_of tier4)" == "PASS" && "$BENCH_STATUS" != "fail" ]]; then
  echo "All tiers completed successfully."
else
  echo "Note: one or more tiers failed or were skipped — see the PGWEB-RESULT line below."
  echo "      Tier 2b (CLI) + Tier 4 (smoke) are hard gates; Tier 1/2a/3 print guidance when skipped."
  echo "      Captured per-phase logs: $RUN_DIR/   (errors mode already surfaced the failing detail above)."
fi

# The un-truncatable verdict — LAST substantive output. A completion/bookend
# report must quote this line verbatim; a green claim requires OVERALL=PASS.
emit_result

# ── Exit code: preserve the historical hard/soft contract ────────────────
# Hard gates (Tier 2b, Tier 4, and a failed image build) → non-zero exit always.
# Soft tiers (Tier 1, 2a, 3) and bench → non-zero exit only under STRICT/CI
# (so a dev machine missing pgrx can still iterate). This matches the pre-028
# behavior; the only change is that ALL tiers now run to completion (more
# signal) and the summary + PGWEB-RESULT line always print before we exit.
hard_fail=0
[[ "$(status_of tier2b)" != "PASS" ]] && hard_fail=1
[[ "$(status_of tier4)"  != "PASS" ]] && hard_fail=1
[[ "$IMAGE_BUILD_FAILED" == "1" ]] && hard_fail=1

soft_fail=0
for _t in tier1 tier2a tier3; do
  [[ "$(status_of "$_t")" != "PASS" ]] && soft_fail=1
done
[[ "$BENCH_STATUS" == "fail" ]] && soft_fail=1

exit_rc=0
[[ "$hard_fail" == "1" ]] && exit_rc=1
if [[ "${STRICT:-}" == "1" || -n "${CI:-}" ]]; then
  [[ "$soft_fail" == "1" ]] && exit_rc=1
fi
exit "$exit_rc"
