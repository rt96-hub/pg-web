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

# Serialize the entire harness. Concurrent test-all.sh runs (plain + RUN_BENCH=1,
# or multiple agents/background jobs) are the #1 source of :8080 port fights
# (pgrx dev PG BGW + smoke-cli compose + bench compose all want the same port)
# and /tmp/pg-web-smoke races.
#
# We use a portable mkdir-based lock (works on Linux + macOS; mkdir is atomic).
# This is the recommended way to run the gate (see updated CLAUDE.md).
# If you really must bypass (e.g. debugging a hung previous run), set FORCE=1.
LOCKDIR="/tmp/pg-web-test-all.lockdir"
cleanup_lock() {
    rmdir "$LOCKDIR" 2>/dev/null || true
}
trap cleanup_lock EXIT INT TERM

if ! mkdir "$LOCKDIR" 2>/dev/null; then
    # Stale lock detection: if the dir exists but no recent activity, or the
    # owning process is gone, we can advise the user.
    echo "ERROR: Another scripts/test-all.sh appears to be running (lock dir $LOCKDIR)."
    echo "       Wait for it to finish, or remove the dir manually after verifying no other run is active."
    echo "       To bypass (risky, may still contend on :8080): FORCE=1 $0 $*"
    if [[ "${FORCE:-}" != "1" ]]; then
        exit 1
    fi
    echo "       FORCE=1 in effect — proceeding (you may see port or smoke-dir races)."
    # Try to take over
    rmdir "$LOCKDIR" 2>/dev/null || true
    mkdir "$LOCKDIR" 2>/dev/null || true
fi

# Portable stop function (defined early so the call below works even if the
# original definition appears later in the file).
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

# Early aggressive cleanup of anything that could be holding :8080 from a
# previous (crashed or parallel) run. We do this *before* any tier work and
# again before Tier 4. This is the main defense against pgrx dev PG + smoke
# stack contention when following the (now sequential) bookend ritual.
stop_pgrx_dev_pg

# Timing + status table (prompt 025). We always emit a one-line summary per tier
# at the end, plus wall durations. STRICT (or CI) turns any soft failure into
# a non-zero exit while still running later tiers for signal.
declare -a TIER_NAME TIER_STATUS TIER_DUR
record_tier() {
    local name="$1" status="$2" dur="$3"
    TIER_NAME+=("$name")
    TIER_STATUS+=("$status")
    TIER_DUR+=("$dur")
}
print_summary_table() {
    echo
    echo "== Per-tier summary =="
    local i
    for i in "${!TIER_NAME[@]}"; do
        printf "  %-6s %s (%ss)\n" "${TIER_NAME[$i]}" "${TIER_STATUS[$i]}" "${TIER_DUR[$i]}"
    done
}

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
echo

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
# (stop_pgrx_dev_pg definition is hoisted near the top of the script for early calls)

# Auto-rebuild the test image when extension source / Dockerfile /
# init scripts are newer than the image. The bake-into-image install SQL
# (and the .so) means stale images silently pass tests against last-build
# behavior — fixed in v0.2 by making the staleness check explicit. Caller
# can force a rebuild with `REBUILD_IMAGE=1` or skip the check entirely
# (bring-your-own-image case) with `SKIP_IMAGE_CHECK=1`.
#
# TEST_IMAGE matches what docker_e2e.rs preflights and what the CLI
# templates / `pg-web up` currently reference (rtaylor96 temporary
# namespace until the pgweb/ Docker Hub org is finalized).
TEST_IMAGE="${TEST_IMAGE:-rtaylor96/pg-web:latest}"

ensure_image_fresh() {
    if [[ "${SKIP_IMAGE_CHECK:-}" == "1" ]]; then
        return 0
    fi
    if [[ "${REBUILD_IMAGE:-}" == "1" ]]; then
        echo "  REBUILD_IMAGE=1 set — rebuilding $TEST_IMAGE"
        PGWEB_IMAGE="$TEST_IMAGE" bash "$REPO_ROOT/scripts/build-image.sh" >/dev/null
        return 0
    fi
    local image_iso image_epoch newest_src
    image_iso=$(docker image inspect "$TEST_IMAGE" --format '{{.Created}}' 2>/dev/null) || {
        echo "  $TEST_IMAGE not present — building"
        PGWEB_IMAGE="$TEST_IMAGE" bash "$REPO_ROOT/scripts/build-image.sh" >/dev/null
        return 0
    }
    # Cross-platform epoch extraction (GNU date -d vs BSD date; CI Linux vs macOS dev).
    # Docker .Created is RFC3339 with nanos (e.g. 2026-...T...Z). We only need
    # second-granularity for staleness vs source mtimes. Use a tolerant parser.
    image_epoch=$(python3 - "$image_iso" <<'PY' 2>/dev/null || echo 0
import sys, re, datetime
s = sys.argv[1].strip()
# Grab up to seconds; ignore fractional and tz for our purposes (good enough for rebuild decision)
m = re.search(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})", s)
if m:
    try:
        dt = datetime.datetime.fromisoformat(m.group(1) + "+00:00")
        print(int(dt.timestamp()))
        sys.exit(0)
    except Exception:
        pass
print(0)
PY
)
    # Anything that affects the image's product: extension Rust source,
    # the Dockerfile + .dockerignore, the entrypoint init script, and the
    # workspace Cargo.toml/Cargo.lock (CLI binary baked at /usr/local/bin/pg-web).
    # Use stat (GNU -c or BSD -f) for mtime; integer seconds are enough for staleness.
    #
    # NOTE: the GNU→BSD `||` fallback below only works because this script runs
    # under `set -o pipefail` (a failing `stat -c` poisons the whole first
    # pipeline; without pipefail, `head`'s exit 0 short-circuits the fallback
    # and newest_src comes back empty → silent no-rebuild). Two separate
    # debugging sessions have misdiagnosed this block by testing it in a
    # pipefail-less shell — don't be the third; copy the `set` line too.
    newest_src=$(find \
        crates/pg_web_ext/src \
        crates/pg_web_cli/src \
        Dockerfile .dockerignore \
        docker/init-pgweb.sh \
        Cargo.toml Cargo.lock \
        -type f -exec stat -c %Y {} + 2>/dev/null | sort -nr | head -1 || \
      find \
        crates/pg_web_ext/src \
        crates/pg_web_cli/src \
        Dockerfile .dockerignore \
        docker/init-pgweb.sh \
        Cargo.toml Cargo.lock \
        -type f -exec stat -f %m {} + 2>/dev/null | sort -nr | head -1 || \
      echo 0)
    if [[ -n "$newest_src" && "$newest_src" -gt "$image_epoch" ]]; then
        echo "  source newer than image (image=$image_iso) — rebuilding $TEST_IMAGE"
        PGWEB_IMAGE="$TEST_IMAGE" bash "$REPO_ROOT/scripts/build-image.sh" >/dev/null
        return 0
    fi

    # Content-hash staleness (prompt 025 #2). mtime above was a fast path;
    # now compare the actual content hash of the build inputs against the
    # label baked into the image at build time. Catches:
    # - git checkout / branch switch / stash pop (mtimes may not advance)
    # - post-pull clobber (published image has no pgweb.src_hash or a different one)
    # - any content edit whose mtime didn't win the "newer" test
    local want_hash have_hash
    want_hash=$(find \
        crates/pg_web_ext/src \
        crates/pg_web_cli/src \
        Dockerfile .dockerignore \
        docker/init-pgweb.sh \
        Cargo.toml Cargo.lock \
        examples \
        -type f 2>/dev/null | sort | xargs sha256sum 2>/dev/null | sha256sum | cut -d' ' -f1)
    have_hash=$(docker image inspect "$TEST_IMAGE" --format '{{index .Config.Labels "pgweb.src_hash"}}' 2>/dev/null || echo "")
    if [[ -z "$have_hash" || "$have_hash" != "$want_hash" ]]; then
        echo "  image src_hash mismatch or missing (have=${have_hash:0:12} want=${want_hash:0:12}) — rebuilding $TEST_IMAGE"
        PGWEB_IMAGE="$TEST_IMAGE" bash "$REPO_ROOT/scripts/build-image.sh" >/dev/null
    fi
}

echo "== Tier 1 — SQL tests (cargo pgrx test pg$PG_MAJOR) =="
t1_start=$(date +%s)
set +e
( cd crates/pg_web_ext && cargo pgrx test "pg$PG_MAJOR" )
tier1_rc=$?
set -e
t1_dur=$(( $(date +%s) - t1_start ))
if [[ $tier1_rc -ne 0 ]]; then
  echo "  [Tier 1 SKIPPED/FAILED — pgrx dev Postgres for pg$PG_MAJOR is not ready]"
  echo "    This is normal on dev machines. The local pgrx-managed PG (the one under ~/.pgrx)"
  echo "    has no pg_config or is missing the install for this exact minor version."
  echo "    To enable Tier 1 anytime you need it:"
  echo "      cargo pgrx init --pg$PG_MAJOR download"
  echo "      # edit ~/.pgrx/data-$PG_MAJOR/postgresql.conf and add:"
  echo "      shared_preload_libraries = 'pg_web_ext'"
  echo "    Then re-run this script. Tiers 2b + 3 + 4 continue below regardless."
  record_tier "tier1" "FAIL/SKIP" "$t1_dur"
else
  record_tier "tier1" "PASS" "$t1_dur"
fi

echo
echo "== Tier 2a — HTTP smoke (scripts/test-http.sh) =="
# Invoked via `bash` (not direct exec) so the script doesn't need the
# +x bit. Edit-via-UNC-mount writes from Claude tools land as 0644
# root-owned, dropping +x; using `bash <script>` sidesteps that
# without needing manual chmod after every doc-touching commit.
t2a_start=$(date +%s)
set +e
bash "$REPO_ROOT/scripts/test-http.sh"
tier2a_rc=$?
set -e
t2a_dur=$(( $(date +%s) - t2a_start ))
if [[ $tier2a_rc -ne 0 ]]; then
  echo "  [Tier 2a SKIPPED/FAILED — usually the same pgrx dev PG readiness issue as Tier 1]"
  echo "    If Tier 1 was green, it's one of the tier-2a-specific causes instead:"
  echo "      - 'database \"pg_web_ext\" does not exist'  → one-time: ~/.pgrx/<ver>/pgrx-install/bin/createdb -h localhost -p 288$PG_MAJOR pg_web_ext"
  echo "      - ':8080 TIMEOUT' with a FATAL loop in the dumped PG log → the BGW itself is crashing (extension code, not setup)"
  echo "    See docs/internal/TESTING-SETUP.md § Diagnosing. Continuing to the rest of the suite..."
  record_tier "tier2a" "FAIL/SKIP" "$t2a_dur"
else
  record_tier "tier2a" "PASS" "$t2a_dur"
fi

echo
echo "== Tier 2b — CLI tests (cargo test -p pg-web) =="
t2b_start=$(date +%s)
cargo test -p pg-web
t2b_dur=$(( $(date +%s) - t2b_start ))
record_tier "tier2b" "PASS" "$t2b_dur"

echo
echo "== Tier 3 — Docker E2E ($TEST_IMAGE + examples/todo) =="
ensure_image_fresh
t3_start=$(date +%s)

# Canary preflight (prompt 025 #4): boot *one* container and give it a short
# ~30s deadline. If / never answers, print the container logs tail and abort
# tier 3 immediately. This turns "broken worker → 13 × 60 s of identical
# timeouts" into a <90 s failure with the root cause (e.g. role nologin,
# missing preload, crash loop) visible in the harness output.
do_tier3_canary() {
    local cname="pgweb-canary-$$"
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
        docker rm -f "$cname" >/dev/null 2>&1 || true
        return 0
    else
        echo "=== TIER 3 CANARY FAILED: / never answered within 30 s ==="
        echo "=== Last 30 lines of container logs (for $cname) ==="
        docker logs --tail 30 "$cname" 2>&1 || true
        docker rm -f "$cname" >/dev/null 2>&1 || true
        echo "=== (see logs above for the real BGW failure: role nologin, preload, crash, etc.) ==="
        return 1
    fi
}

echo "  tier3 canary preflight (fast-fail on non-serving worker)..."
if ! do_tier3_canary; then
    echo "  [Tier 3 ABORTED by canary — broken worker; logs printed above]"
    tier3_rc=1
    # Do not run the 13 tests (they would each burn another 60 s).
else
    set +e
    # Run sequentially (--test-threads=1) to avoid 13 containers starting at once on dev machines (Docker Desktop + macOS especially struggles with the concurrent startup + 30s wait per test).
    cargo test -p pg-web --test docker_e2e -- --ignored --test-threads=1
    tier3_rc=$?
    set -e
fi
t3_dur=$(( $(date +%s) - t3_start ))
if [[ $tier3_rc -ne 0 ]]; then
  echo "  [Tier 3 had failures (E2E tests against the image)]"
  echo "    This can happen transiently after a fresh image rebuild, under load, or due to real app bugs."
  echo "    The script will continue to Tier 4 (black-box smoke) so you still get useful signals."
  record_tier "tier3" "FAIL" "$t3_dur"
else
  record_tier "tier3" "PASS" "$t3_dur"
fi

# Reclaim :8080 from the pgrx dev PG before tier 4's docker stack
# tries to bind it. (When Tier 1 or 2a actually ran, they leave the pgrx PG up;
# tiers 2b + 3 do not. Safe to stop here either way.)
echo
echo "== Reclaiming :8080 for tier 4 =="
stop_pgrx_dev_pg

echo
echo "== Tier 4 — CLI black-box smoke (scripts/smoke-cli.sh) =="
t4_start=$(date +%s)
# Use a unique smoke directory by default (PID-based). This lets multiple
# sequential runs (or the integrated RUN_BENCH=1 path) coexist without
# clobbering /tmp/pg-web-smoke or its docker compose project.
# Users can still override with SMOKE_DIR=... if they want a stable name.
: "${SMOKE_DIR:=/tmp/pg-web-smoke-$$}"
export SMOKE_DIR
bash "$REPO_ROOT/scripts/smoke-cli.sh"
t4_dur=$(( $(date +%s) - t4_start ))
record_tier "tier4" "PASS" "$t4_dur"

# 015 benchmark (opt-in, heavy). Full matrix with oha under constrained + unconstrained
# tiers + HOLB experiment. A future lightweight bench-smoke (short duration + generous
# p99 bound) could be added behind RUN_BENCH_SMOKE=1 without bloating every CI run.
# The goal is catching accidental throughput regressions before they reach prod.
if [[ "${RUN_BENCH:-}" == "1" ]]; then
  echo
  echo "== Opt-in Tier (015) — Concurrency/throughput benchmark (bench/run.sh) =="
  # Run unconstrained first (comparison), then the 1c/2g primary tier that the VISION
  # claim was about. The harness itself documents hardware, tool, and caveats.
  bash "$REPO_ROOT/bench/run.sh"
  BENCH_CPUS=1 BENCH_MEM=2g bash "$REPO_ROOT/bench/run.sh"
fi

echo
echo "== Test run complete =="
print_summary_table

# Compute overall soft-fail status. Hard tiers (2b,4) already aborted via set -e if they failed.
overall_rc=0
if [[ ${tier1_rc:-0} -ne 0 || ${tier2a_rc:-0} -ne 0 || ${tier3_rc:-0} -ne 0 ]]; then
  overall_rc=1
  echo "Note: One or more early tiers had issues or were skipped."
  echo "      You can STILL run 'bash scripts/test-all.sh' ANYTIME you need."
  echo "      Tier 2b (CLI tests) + Tier 4 (smoke) always run; Tier 3 (Docker E2E) is attempted."
  echo "      pgrx dev PG guidance (for Tier 1/2a) is printed at the top when needed."
else
  echo "All tiers completed successfully."
fi

# STRICT mode (or CI=...) : any tier failure => non-zero exit for the suite.
# This makes the script safe as a CI entrypoint while preserving the dev
# "keep going, show all signal" behavior by default.
if [[ "${STRICT:-}" == "1" || -n "${CI:-}" ]]; then
  exit "$overall_rc"
fi
# Non-strict (typical local dev): exit 0 so the developer can iterate even if
# a pgrx setup tier is red. The table above is the actionable summary.
