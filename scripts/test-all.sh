#!/usr/bin/env bash
# Full local test run: SQL (#[pg_test]) + HTTP smoke + CLI.
# This is what CI should invoke.
set -euo pipefail

PG_MAJOR="${PG_MAJOR:-17}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "== SQL tests (cargo pgrx test pg$PG_MAJOR) =="
( cd crates/pg_web_ext && cargo pgrx test "pg$PG_MAJOR" )

echo
echo "== HTTP smoke (scripts/test-http.sh) =="
"$REPO_ROOT/scripts/test-http.sh"

echo
echo "== CLI tests (cargo test -p pg_web_cli) =="
cargo test -p pg_web_cli

echo
echo "All tests passed."
