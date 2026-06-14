#!/usr/bin/env bash
# Run the HTTP smoke test against a running pg-web worker.
#
# Idempotent: ensures the non-test .so is installed, PG is running with a
# fresh extension install (default seed data), and :8080 is responsive
# before handing control to `cargo test --test http_smoke`.
#
# Why the reinstall step exists: `cargo pgrx test` and `cargo pgrx install`
# both write to $PGRX_HOME/<ver>/pgrx-install/lib/postgresql/pg_web_ext.so.
# If `cargo pgrx test` ran last, the installed library contains test-only
# wrapper functions that CREATE EXTENSION will fail to resolve. Re-running
# `cargo pgrx install --profile dev` restores the runtime-flavor build.

set -euo pipefail

PGRX_HOME="${PGRX_HOME:-$HOME/.pgrx}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

PG_MAJOR="${PG_MAJOR:-17}"

# Auto-detect the actual installed Postgres minor version for the requested major.
# pgrx users often end up with 17.9, 17.10, 17.11 etc. Hard-coding 17.9 breaks
# when the machine has a different patch level (as seen with 17.10).
if [[ -z "${PG_VERSION:-}" ]]; then
  pg_config_glob=$(ls -1 "$PGRX_HOME/${PG_MAJOR}."*/pgrx-install/bin/pg_config 2>/dev/null | sort -V | tail -1 || true)
  if [[ -n "$pg_config_glob" && -x "$pg_config_glob" ]]; then
    # e.g. /Users/.../.pgrx/17.10/pgrx-install/bin/pg_config  ->  PG_VERSION=17.10
    PG_VERSION=$(basename "$(dirname "$(dirname "$(dirname "$pg_config_glob")")")")
  else
    PG_VERSION="${PG_MAJOR}.10"   # last-resort fallback; the ! -x check below will give a good message
  fi
fi
PG_MAJOR="${PG_VERSION%%.*}"

PG_CTL="$PGRX_HOME/$PG_VERSION/pgrx-install/bin/pg_ctl"
PSQL="$PGRX_HOME/$PG_VERSION/pgrx-install/bin/psql"
DATA_DIR="$PGRX_HOME/data-$PG_MAJOR"
LOG_FILE="$PGRX_HOME/$PG_MAJOR.log"
PG_CONFIG="$PGRX_HOME/$PG_VERSION/pgrx-install/bin/pg_config"

if [ ! -x "$PG_CTL" ]; then
    echo "FATAL: pg_ctl not found at $PG_CTL" >&2
    echo "Have you run 'cargo pgrx init'?" >&2
    exit 1
fi

# Restore the runtime-flavor .so (overwrites any test-featured build).
# `cargo pgrx test` writes test-wrapper SQL that the runtime .so can't satisfy,
# so we always reinstall here before starting PG.
(
    cd "$REPO_ROOT/crates/pg_web_ext"
    # Strip any prior test-flavor artifacts so pgrx definitely regenerates.
    rm -f "$PGRX_HOME/$PG_VERSION/pgrx-install/share/postgresql/extension/pg_web_ext--0.0.1.sql"
    cargo pgrx install --profile dev --features "pg$PG_MAJOR" --no-default-features \
        --pg-config "$PG_CONFIG"
)

# Restart PG to load the freshly-installed .so
if "$PG_CTL" -D "$DATA_DIR" status >/dev/null 2>&1; then
    "$PG_CTL" -D "$DATA_DIR" -m immediate stop >/dev/null
fi
"$PG_CTL" -D "$DATA_DIR" -l "$LOG_FILE" start >/dev/null

# Self-heal tier 2a bootstrap (prompt 025 #5):
# - create the dev DB if it doesn't exist (idempotent; the one-time `createdb`
#   after cargo pgrx init is now automatic).
# - ensure shared_preload_libraries contains pg_web_ext in the conf; if we
#   had to append it, bounce PG so the BGW actually registers.
if ! "$PSQL" -p 28817 -h localhost -d pg_web_ext -c "SELECT 1" >/dev/null 2>&1; then
    echo "  tier2a: database pg_web_ext missing — creating (self-heal)"
    "$PGRX_HOME/$PG_VERSION/pgrx-install/bin/createdb" -h localhost -p 28817 pg_web_ext || true
fi
if ! grep -q "shared_preload_libraries.*pg_web_ext" "$DATA_DIR/postgresql.conf" 2>/dev/null; then
    echo "  tier2a: appending shared_preload_libraries = 'pg_web_ext' to $DATA_DIR/postgresql.conf (self-heal)"
    echo "shared_preload_libraries = 'pg_web_ext'" >> "$DATA_DIR/postgresql.conf"
    "$PG_CTL" -D "$DATA_DIR" -m immediate stop >/dev/null 2>&1 || true
    "$PG_CTL" -D "$DATA_DIR" -l "$LOG_FILE" start >/dev/null
fi

# Reset extension so we get fresh seed data (route /, template, handler)
"$PSQL" -p 28817 -h localhost -d pg_web_ext -v ON_ERROR_STOP=1 \
    -c "DROP EXTENSION IF EXISTS pg_web_ext CASCADE; CREATE EXTENSION pg_web_ext;" \
    >/dev/null

# Wait for :8080 to open (up to 15s)
deadline=$(( $(date +%s) + 15 ))
while ! curl -sf http://localhost:8080/ >/dev/null 2>&1; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
        echo "TIMEOUT: :8080 did not open within 15s" >&2
        echo "Last 20 lines of $LOG_FILE:" >&2
        tail -20 "$LOG_FILE" >&2
        exit 1
    fi
    sleep 0.2
done

# Port-shadow preflight: confirm whoever's on :8080 is actually our BGW.
# A leftover `pg-web up` Docker container would happily serve HTTP on
# :8080, the curl above would have gotten a 200, and the smoke would
# fail with "wrong template body" — pointing at a code bug when the
# real cause is environmental contamination.
#
# We must support macOS dev machines (no `ss`) + Linux CI.
# Strategy: use lsof (present on macOS; often on Linux) to get a LISTENing PID,
# fall back to ss (Linux), then netstat. Then inspect the process args for the
# pg_web_worker rewrite that the Postgres postmaster does for BGWs.
# See DEVELOPER-GUIDE.md pitfall #18 for the failure mode this catches.
get_listener_pid() {
    local port="$1"
    # lsof is the most portable for "listening PID on TCP port" across macOS + Linux.
    if command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | head -1
        return 0
    fi
    # Linux ss (iproute2). The original implementation.
    if command -v ss >/dev/null 2>&1; then
        local line
        line=$(ss -tlnp "sport = :$port" 2>/dev/null | tail -n +2 | head -1)
        echo "$line" | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2
        return 0
    fi
    # Older netstat fallback (some minimal containers/CI).
    if command -v netstat >/dev/null 2>&1; then
        local line pid
        line=$(netstat -tlnp 2>/dev/null | grep -E ":$port[[:space:]]" | head -1)
        # Linux netstat -tlnp often shows "pid/progname" or "pid/"; extract leading digits.
        pid=$(echo "$line" | grep -oE '[0-9]+/' | head -1 | tr -d '/')
        if [[ -n "$pid" ]]; then
            echo "$pid"
            return 0
        fi
    fi
    echo ""
}

listener_pid=$(get_listener_pid 8080)
if [ -z "$listener_pid" ]; then
    # Listener exists (curl above succeeded) but we couldn't read its
    # PID — cross-user case (docker-proxy is typically root-owned and
    # invisible to non-root tools).
    echo "ERROR: :8080 has a listener but its process is invisible to this user (likely root-owned, e.g. docker-proxy)." >&2
    echo "Diagnose: sudo lsof -nP -iTCP:8080 -sTCP:LISTEN  OR  docker ps --format 'table {{.Names}}\t{{.Ports}}' | grep 8080" >&2
    echo "Fix: docker stop <container-name>  (or \`pg-web down\` from the original app dir)" >&2
    exit 1
fi

# ps -o args= (or command=) + wide output works on both GNU ps and macOS/BSD ps.
ps_args=$(ps -p "$listener_pid" -o args= -ww 2>/dev/null || ps -p "$listener_pid" -o command= 2>/dev/null || echo "")
if ! echo "$ps_args" | grep -q 'pg_web_worker'; then
    holder=$(echo "$ps_args" | head -1 || echo "<gone>")
    echo "ERROR: :8080 is held by PID $listener_pid (\`$holder\`), not the dev PG's pg_web_worker BGW." >&2
    echo "This is usually a leftover \`pg-web up\` Docker container shadowing the port." >&2
    echo "Diagnose:" >&2
    echo "    lsof -nP -iTCP:8080 -sTCP:LISTEN   (or ss / netstat on Linux)" >&2
    echo "    docker ps --format 'table {{.Names}}\t{{.Ports}}' | grep 8080" >&2
    echo "Fix: docker stop <container-name>  (or \`pg-web down\` from the original app dir)" >&2
    exit 1
fi

cd "$REPO_ROOT"
cargo test --test http_smoke -p pg_web_ext --features "pg$PG_MAJOR" --no-default-features
