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

PG_VERSION="${PG_VERSION:-17.9}"
PG_MAJOR="${PG_VERSION%%.*}"
PGRX_HOME="${PGRX_HOME:-$HOME/.pgrx}"
PG_CTL="$PGRX_HOME/$PG_VERSION/pgrx-install/bin/pg_ctl"
PSQL="$PGRX_HOME/$PG_VERSION/pgrx-install/bin/psql"
DATA_DIR="$PGRX_HOME/data-$PG_MAJOR"
LOG_FILE="$PGRX_HOME/$PG_MAJOR.log"
PG_CONFIG="$PGRX_HOME/$PG_VERSION/pgrx-install/bin/pg_config"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

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

cd "$REPO_ROOT"
cargo test --test http_smoke -p pg_web_ext --features "pg$PG_MAJOR" --no-default-features
