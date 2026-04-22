#!/usr/bin/env bash
# Full local test run: five tiers.
#   1)  SQL / pgrx #[pg_test]
#   2a) HTTP smoke against a running extension
#   2b) CLI unit + hermetic integration tests
#   3)  Docker E2E — boots pgweb/postgres:latest in a container and drives
#       the full CRUD flow against examples/demo
#   4)  CLI black-box smoke — init → up → push → break 3 ways → down,
#       exercising the user-visible CLI stdout and HTTP bodies
#
# Tier 3 is mandatory. Tier 4 is also mandatory — it's what catches
# gotchas that fall between the rust tests (wrong image baked, stray
# pgrx dev PG shadowing :8080, docker-compose service rename, etc.).
# Both need Docker + pgweb/postgres:latest.
#
# This is what CI should invoke.
set -euo pipefail

PG_MAJOR="${PG_MAJOR:-17}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "== Tier 1 — SQL tests (cargo pgrx test pg$PG_MAJOR) =="
( cd crates/pg_web_ext && cargo pgrx test "pg$PG_MAJOR" )

echo
echo "== Tier 2a — HTTP smoke (scripts/test-http.sh) =="
"$REPO_ROOT/scripts/test-http.sh"

echo
echo "== Tier 2b — CLI tests (cargo test -p pg_web_cli) =="
cargo test -p pg_web_cli

echo
echo "== Tier 3 — Docker E2E (pgweb/postgres:latest + examples/demo) =="
cargo test -p pg_web_cli --test docker_e2e -- --ignored

echo
echo "== Tier 4 — CLI black-box smoke (scripts/smoke-cli.sh) =="
bash "$REPO_ROOT/scripts/smoke-cli.sh"

echo
echo "All tests passed."
