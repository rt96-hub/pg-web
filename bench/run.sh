#!/usr/bin/env bash
# 015 benchmark harness.
# Boots a pg-web stack (rtaylor96/pg-web:latest), pushes the dedicated bench app,
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

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

BENCH_CPUS="${BENCH_CPUS:-}"
BENCH_MEM="${BENCH_MEM:-}"          # e.g. 2g
OHA_VERSION="${OHA_VERSION:-1.14.0}" # pinned for reproducibility (see bench/run.sh ensure_oha for asset mapping)
RESULTS_DIR="bench/results"
COMPOSE="docker compose -f bench/docker-compose.yml"
CONTAINER_NAME="bench-postgres-1"   # default compose name

mkdir -p "$RESULTS_DIR"

log() { echo "[bench] $*"; }

require_docker() {
  if ! command -v docker >/dev/null; then
    echo "docker is required for the benchmark harness (tier 3+ style)" >&2
    exit 1
  fi
}

# Ensure the image exists and is reasonably fresh (reuses test-all logic shape).
ensure_image() {
  if [[ "${SKIP_IMAGE_CHECK:-}" == "1" ]]; then
    return 0
  fi
  if ! docker image inspect rtaylor96/pg-web:latest >/dev/null 2>&1; then
    log "rtaylor96/pg-web:latest missing — building via scripts/build-image.sh"
    bash scripts/build-image.sh
    return 0
  fi
  if [[ "${REBUILD_IMAGE:-}" == "1" ]]; then
    log "REBUILD_IMAGE=1 — rebuilding"
    bash scripts/build-image.sh
  fi
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
  log "starting bench stack (image rtaylor96/pg-web:latest)"
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

# Convenience: run oha with common flags and tee output to a result file.
# $1 = label for filename, $2 = url path, rest = extra oha args (e.g. -c 32 -z 15s)
run_oha() {
  local label=$1; shift
  local path=$1; shift
  local url="http://localhost:8080${path}"
  local outfile="$RESULTS_DIR/${label}.txt"
  log "oha $label -> $url (args: $*)"
  "$OHA_CMD" --no-tui --no-color -z 10s "$@" "$url" | tee "$outfile"
  # Also write a tiny summary line for easy grepping later.
  echo "RUN: label=$label url=$url args=$*" >> "$RESULTS_DIR/summary.txt"
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
  "$OHA_CMD" --no-tui --no-color -c 16 -z 12s "http://localhost:8080/bench/todos" | tee "$fast_log"
  wait $slow_pid || true
  log "HOLB run complete — compare p99/p99.9 in $fast_log vs the pure b-todos100-c16 run"

  # One more pure fast run at same conc for easy before/after in the report.
  seed_todos 100
  run_oha "b-todos100-c16-pure" "/bench/todos" -c 16
}

main() {
  require_docker
  ensure_image
  ensure_oha

  trap stop_stack EXIT

  start_stack
  push_bench_app

  run_workloads

  log "raw results in $RESULTS_DIR/"
  log "Next: review outputs, fill docs/BENCHMARKS.md, reconcile VISION.md:58"
  # Do not auto-stop here; trap will on exit, or caller can keep stack for
  # manual inspection. For CI-style runs we want cleanup, which the trap does.
}

main "$@"
