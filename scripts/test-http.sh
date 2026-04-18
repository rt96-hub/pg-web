#!/usr/bin/env bash
# Run the HTTP smoke test against a running pg-web worker.
# Idempotent about PG state: starts it if not running, leaves it running on exit.
set -euo pipefail

PG_VERSION="${PG_VERSION:-17.9}"
PG_MAJOR="${PG_VERSION%%.*}"
PGRX_HOME="${PGRX_HOME:-$HOME/.pgrx}"
PG_CTL="$PGRX_HOME/$PG_VERSION/pgrx-install/bin/pg_ctl"
DATA_DIR="$PGRX_HOME/data-$PG_MAJOR"
LOG_FILE="$PGRX_HOME/$PG_MAJOR.log"

if [ ! -x "$PG_CTL" ]; then
    echo "FATAL: pg_ctl not found at $PG_CTL" >&2
    echo "Have you run 'cargo pgrx init'?" >&2
    exit 1
fi

# Start PG if not already running (fast path: check pg_ctl status)
if ! "$PG_CTL" -D "$DATA_DIR" status >/dev/null 2>&1; then
    echo "Starting Postgres $PG_VERSION..."
    "$PG_CTL" -D "$DATA_DIR" -l "$LOG_FILE" start
fi

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

# Run the test
cd "$(dirname "$0")/.."
cargo test --test http_smoke -p pg_web_ext --features "pg$PG_MAJOR" --no-default-features
