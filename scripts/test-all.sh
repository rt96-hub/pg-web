#!/usr/bin/env bash
# Full local test run: five tiers.
#   1)  SQL / pgrx #[pg_test]
#   2a) HTTP smoke against a running extension
#   2b) CLI unit + hermetic integration tests
#   3)  Docker E2E — boots pgweb/postgres:latest in a container and drives
#       the full CRUD flow against examples/todo
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

# Stop pgrx's dev Postgres if it's running. pgrx leaves it up after
# `cargo pgrx test` / `cargo pgrx run` so iteration stays cheap — but
# tier 4's docker stack publishes :8080 on the host, and the dev PG's
# BGW is already holding that port. smoke-cli's preflight catches the
# shadowing and bails before running anything, which used to force
# manual cleanup between runs.
#
# Idempotent: if the dev PG isn't running (or isn't installed — e.g.
# CI with just a workspace checkout), this is a no-op. The data
# directory `~/.pgrx/data-$PG_MAJOR` is NOT touched — next
# `cargo pgrx run` boots it right back up.
#
# -m immediate, not -m fast: pg_web_ext's BGW doesn't drain cleanly
# under fast-stop. See docs/DEVELOPER-GUIDE.md pitfall #8.
#
# pg_ctl isn't in PATH in a default pgweb user shell — pgrx installs
# it at ~/.pgrx/<PG_MAJOR>.<minor>/pgrx-install/bin/pg_ctl. We glob on
# the minor version because it changes (17.8 → 17.9 → 17.10 …).
stop_pgrx_dev_pg() {
    local pg_ctl data_dir
    pg_ctl=$(ls -1 "$HOME/.pgrx/${PG_MAJOR}."*/pgrx-install/bin/pg_ctl 2>/dev/null | head -1)
    data_dir="$HOME/.pgrx/data-${PG_MAJOR}"
    if [[ -z "$pg_ctl" || ! -d "$data_dir" ]]; then
        return 0
    fi
    if ! "$pg_ctl" -D "$data_dir" status >/dev/null 2>&1; then
        return 0
    fi
    echo "  stopping pgrx dev PG (holding :8080) — data dir preserved"
    "$pg_ctl" -D "$data_dir" -m immediate stop >/dev/null
}

# Auto-rebuild `pgweb/postgres:latest` when extension source / Dockerfile /
# init scripts are newer than the image. The bake-into-image install SQL
# (and the .so) means stale images silently pass tests against last-build
# behavior — fixed in v0.2 by making the staleness check explicit. Caller
# can force a rebuild with `REBUILD_IMAGE=1` or skip the check entirely
# (bring-your-own-image case) with `SKIP_IMAGE_CHECK=1`.
ensure_image_fresh() {
    if [[ "${SKIP_IMAGE_CHECK:-}" == "1" ]]; then
        return 0
    fi
    if [[ "${REBUILD_IMAGE:-}" == "1" ]]; then
        echo "  REBUILD_IMAGE=1 set — rebuilding pgweb/postgres:latest"
        bash "$REPO_ROOT/scripts/build-image.sh" >/dev/null
        return 0
    fi
    local image_iso image_epoch newest_src
    image_iso=$(docker image inspect pgweb/postgres:latest --format '{{.Created}}' 2>/dev/null) || {
        echo "  pgweb/postgres:latest not present — building"
        bash "$REPO_ROOT/scripts/build-image.sh" >/dev/null
        return 0
    }
    image_epoch=$(date -d "$image_iso" +%s 2>/dev/null || echo 0)
    # Anything that affects the image's product: extension Rust source,
    # the Dockerfile + .dockerignore, the entrypoint init script, and the
    # workspace Cargo.toml/Cargo.lock (CLI binary baked at /usr/local/bin/pg-web).
    newest_src=$(find \
        crates/pg_web_ext/src \
        crates/pg_web_cli/src \
        Dockerfile .dockerignore \
        docker/init-pgweb.sh \
        Cargo.toml Cargo.lock \
        -type f -printf '%T@\n' 2>/dev/null \
        | sort -nr | head -1 | cut -d. -f1)
    if [[ -n "$newest_src" && "$newest_src" -gt "$image_epoch" ]]; then
        echo "  source newer than image (image=$image_iso) — rebuilding pgweb/postgres:latest"
        bash "$REPO_ROOT/scripts/build-image.sh" >/dev/null
    fi
}

echo "== Tier 1 — SQL tests (cargo pgrx test pg$PG_MAJOR) =="
( cd crates/pg_web_ext && cargo pgrx test "pg$PG_MAJOR" )

echo
echo "== Tier 2a — HTTP smoke (scripts/test-http.sh) =="
# Invoked via `bash` (not direct exec) so the script doesn't need the
# +x bit. Edit-via-UNC-mount writes from Claude tools land as 0644
# root-owned, dropping +x; using `bash <script>` sidesteps that
# without needing manual chmod after every doc-touching commit.
bash "$REPO_ROOT/scripts/test-http.sh"

echo
echo "== Tier 2b — CLI tests (cargo test -p pg-web) =="
cargo test -p pg-web

echo
echo "== Tier 3 — Docker E2E (pgweb/postgres:latest + examples/todo) =="
ensure_image_fresh
cargo test -p pg-web --test docker_e2e -- --ignored

# Reclaim :8080 from the pgrx dev PG before tier 4's docker stack
# tries to bind it. (Tiers 1 + 2a leave the pgrx PG running; tiers 2b
# + 3 don't touch it. Safe to stop here.)
echo
echo "== Reclaiming :8080 for tier 4 =="
stop_pgrx_dev_pg

echo
echo "== Tier 4 — CLI black-box smoke (scripts/smoke-cli.sh) =="
bash "$REPO_ROOT/scripts/smoke-cli.sh"

echo
echo "All tests passed."
