#!/usr/bin/env bash
# harness.sh — shared idempotency primitives for the pg-web test/bench harness.
#
# Sourced (never executed) by scripts/test-all.sh, bench/run.sh, and
# scripts/build-image.sh. This is the single source of truth for the things
# prompt 029 makes idempotent so that `./scripts/test-all.sh` and `./bench/run.sh`
# "just work" — first run, tenth run, after an edit, after a crash — with zero
# manual hygiene and zero manual flags:
#
#   - the image tag           (pgweb_image / PGWEB_DEFAULT_IMAGE)
#   - image freshness         (compute_src_hash / ensure_image_fresh / pgweb_build_image)
#   - environment reclaim     (reclaim_environment — stop pgrx PG, rm our containers, reap dirs)
#   - the cross-run lock       (acquire_lock / release_lock — PID + age self-healing)
#
# DESIGN RULE (029): when in doubt, do the safe expensive thing (reclaim,
# rebuild, re-seed). We have explicitly traded time for reliability. Never make
# correctness depend on prior machine state or a remembered flag. The flags
# REBUILD_IMAGE / SKIP_IMAGE_CHECK / FORCE are debugging-only escape hatches and
# must NOT be used to coax a run green — see CLAUDE.md.
#
# Marker dependency: the decision/observability markers (mk_note "REUSED",
# "RECLAIMED", "STALE → BUILD → BUILT", container-removal STEP lines) come from
# scripts/report-lib.sh, which the two test entrypoints source *before* us.
# build-image.sh sources us only for compute_src_hash + PGWEB_DEFAULT_IMAGE and
# does not source report-lib; the no-op marker fallback below keeps that safe.

# --- marker fallback --------------------------------------------------------
# If report-lib.sh hasn't been sourced (build-image.sh path), define no-op
# markers so the image helpers can be sourced without "command not found".
# When report-lib IS present its real markers were defined first and win here.
for _pgweb_mk in mk_start mk_step mk_pass mk_fail mk_skip mk_ok mk_note mk_build; do
    declare -F "$_pgweb_mk" >/dev/null 2>&1 || eval "${_pgweb_mk}() { :; }"
done
unset _pgweb_mk

# Repo root. Each entrypoint (test-all.sh / bench/run.sh / build-image.sh)
# computes this reliably from its own $0 and EXPORTS it before sourcing us — so
# we honour that. The fallback (git toplevel, else cwd) only matters if the lib
# is sourced directly. We deliberately do NOT derive it from BASH_SOURCE: when a
# bare `harness.sh` is sourced, dirname is `.` and `./../..` would climb two
# levels above the repo (to $HOME on a typical checkout) and the content hash
# would walk the entire home directory. Anchored prunes (`-path './target'`)
# also require the hash to run from the true repo root.
PGWEB_REPO_ROOT="${PGWEB_REPO_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"

# Shared cross-run lock (both entrypoints use the SAME dir so a standalone
# bench run and a test-all run serialize against each other — the :8080 hazard
# is real for either). PID + start-time files inside make it self-healing.
PGWEB_LOCKDIR="${PGWEB_LOCKDIR:-/tmp/pg-web-test-all.lockdir}"
# A run that legitimately exceeds this is implausible (full bookend ≈ 30–60 min);
# a lock older than this whose PID happens to be alive is treated as a PID-reuse
# false positive and reclaimed. Liveness is the primary signal; this is a backstop.
PGWEB_LOCK_MAX_AGE="${PGWEB_LOCK_MAX_AGE:-14400}"   # 4h
# Stale temp/log dirs older than this (minutes) are reaped. A live or just-finished
# run's dirs are newer, so they survive for post-hoc inspection.
PGWEB_REAP_MIN="${PGWEB_REAP_MIN:-180}"             # 3h

# --- image tag (single source of truth) -------------------------------------
# The shipped/default image name. The harness scripts all defer to this; an
# override via TEST_IMAGE (or PGWEB_IMAGE) propagates to every harness path.
PGWEB_DEFAULT_IMAGE="${PGWEB_DEFAULT_IMAGE:-rtaylor96/pg-web:latest}"

# Resolve the image tag: TEST_IMAGE (user-facing) → PGWEB_IMAGE (internal
# handoff to build-image / compose) → the shipped default.
pgweb_image() {
    echo "${TEST_IMAGE:-${PGWEB_IMAGE:-$PGWEB_DEFAULT_IMAGE}}"
}

# --- content hash (provably complete; 029 #2) ------------------------------
# Whole-tree-minus-denylist content hash. The denylist is ONLY volatile/scratch
# paths that can never affect the produced image regardless of how .dockerignore
# evolves — so no image-affecting input can be silently missed (the failure mode
# of the old hand-maintained enumerated file list). Over-rebuilding (e.g. a docs
# edit triggers a cache-hit rebuild that just re-bakes the LABEL) is the accepted
# cost of that guarantee (029 open-Q1 lean: whole-tree-minus-denylist).
#
# MUST be byte-identical wherever it runs (build-image bakes it into the
# pgweb.src_hash LABEL; test-all + bench compare against that label) — which is
# guaranteed because all three call THIS one function, from the repo root.
# Cost ≈ 1–2 s (a find | sha256sum), which 029 explicitly accepts.
compute_src_hash() {
    ( cd "$PGWEB_REPO_ROOT" && \
      find . \
        \( -path './.git' -o -path './target' -o -path './bench/results' \
           -o -path './bench/bin' -o -name 'node_modules' \
           -o -name '.DS_Store' -o -name '*.log' -o -name '.env' \) -prune \
        -o -type f -print 2>/dev/null \
      | LC_ALL=C sort \
      | xargs sha256sum 2>/dev/null \
      | sha256sum | awk '{print $1}' )
}

# --- the build wrapper ------------------------------------------------------
# Run build-image.sh with explicit BUILD → BUILT/FAIL markers + elapsed time,
# output captured to <log> (streamed too in verbose). build-image.sh is NEVER
# run silently — silence reads identically to "skipped the build". Returns
# nonzero on a build failure (a hard prerequisite for tier 3 / bench).
#   $1 = image tag   $2 = build log path
pgweb_build_image() {
    local image="$1" blog="$2" bstart bdur brc built_hash
    mk_build image "$image (docker build) — log: $blog"
    bstart=$(date +%s)
    if [[ "${TEST_MODE:-errors}" == "verbose" ]]; then
        PGWEB_IMAGE="$image" bash "$PGWEB_REPO_ROOT/scripts/build-image.sh" 2>&1 | tee "$blog"
        brc=${PIPESTATUS[0]}
    else
        PGWEB_IMAGE="$image" bash "$PGWEB_REPO_ROOT/scripts/build-image.sh" >"$blog" 2>&1
        brc=$?
    fi
    bdur=$(( $(date +%s) - bstart ))
    if [[ "$brc" -ne 0 ]]; then
        mk_fail image "docker build FAILED (rc=$brc)" "${bdur}s"
        echo "    ---- last 40 lines of $blog ----"
        tail -40 "$blog" 2>/dev/null | sed 's/^/    /'
        return 1
    fi
    built_hash=$(docker image inspect "$image" --format '{{index .Config.Labels "pgweb.src_hash"}}' 2>/dev/null | cut -c1-12 || echo "?")
    mk_ok image BUILT "src_hash=${built_hash}" "${bdur}s"
    return 0
}

# Decide reuse-vs-rebuild and ALWAYS emit an explicit decision marker: exactly
# one of `image REUSED (fresh …)` or the `STALE → BUILD → BUILT` triple. The
# content hash is the SOLE source of truth — no mtime fast-path, because mtime
# noise (git stash/pop, checkout, re-tag) caused false rebuilds (029 cell G) and
# mtime can't see a content change that didn't advance it. Reuse is never
# silent — REUSED is the proof the freshness check ran and decided.
#   $1 = image tag   $2 = build log path
ensure_image_fresh() {
    local image="$1" blog="$2" want_hash have_hash
    if [[ "${SKIP_IMAGE_CHECK:-}" == "1" ]]; then
        mk_skip image "SKIP_IMAGE_CHECK=1 — using whatever $image is present (DEBUG ONLY; not for greening a run)"
        return 0
    fi
    want_hash=$(compute_src_hash)
    if [[ "${REBUILD_IMAGE:-}" == "1" ]]; then
        mk_note image STALE "REBUILD_IMAGE=1 forced (DEBUG ONLY)"
        pgweb_build_image "$image" "$blog"; return $?
    fi
    have_hash=$(docker image inspect "$image" --format '{{index .Config.Labels "pgweb.src_hash"}}' 2>/dev/null || echo "")
    if [[ -z "$have_hash" ]]; then
        mk_note image STALE "$image not present locally (or carries no pgweb.src_hash label)"
        pgweb_build_image "$image" "$blog"; return $?
    fi
    if [[ "$have_hash" != "$want_hash" ]]; then
        mk_note image STALE "src_hash mismatch (have=${have_hash:0:12} want=${want_hash:0:12})"
        pgweb_build_image "$image" "$blog"; return $?
    fi
    mk_ok image REUSED "fresh, src_hash=${have_hash:0:12}"
    return 0
}

# --- pgrx dev PG stop -------------------------------------------------------
# Stop pgrx's dev Postgres if running. pgrx leaves it up after `cargo pgrx
# test`/`run` so iteration stays cheap — but its BGW holds :8080, which the
# docker tiers + bench publish on the host. Idempotent: a no-op if it isn't
# running or isn't installed (CI). The data dir is NOT touched (next
# `cargo pgrx run` boots it back up). -m immediate, not fast: pg_web_ext's BGW
# doesn't drain cleanly under fast-stop (DEVELOPER-GUIDE pitfall #8).
stop_pgrx_dev_pg() {
    local pg_major pg_ctl data_dir
    pg_major="${PG_MAJOR:-17}"
    pg_ctl=$(ls -1 "$HOME/.pgrx/${pg_major}."*/pgrx-install/bin/pg_ctl 2>/dev/null | head -1)
    data_dir="$HOME/.pgrx/data-${pg_major}"
    if [[ -z "$pg_ctl" || ! -d "$data_dir" ]]; then
        return 0
    fi
    if ! "$pg_ctl" -D "$data_dir" status >/dev/null 2>&1; then
        return 0
    fi
    mk_step reclaim "stopping pgrx dev PG (was holding :8080; data dir preserved)"
    "$pg_ctl" -D "$data_dir" -m immediate stop >/dev/null 2>&1 || true
}

# --- unconditional environment reclaim (029 #5) -----------------------------
# Free :8080 and remove our own leftover containers / stacks / temp dirs from a
# previous (crashed, killed, or just-finished) run. Runs at the top of BOTH
# entrypoints, every time, regardless of mode or prior state — the "I don't care
# if it's slow" guarantee. Safe + idempotent on a clean machine (all || true,
# existence-checked).
#
# MUST be called only while holding the lock (acquire_lock first): the lock
# guarantees no genuinely-concurrent pg-web run exists, so it is safe to be
# aggressive. SURGICAL by design — it only ever touches OUR families
# (pgweb-canary-*, pg-web-smoke*, the bench compose project, and testcontainers
# running our image). It never blanket-prunes, so unrelated containers (a
# developer's other postgres stacks) are never at risk.
reclaim_environment() {
    mk_step reclaim "freeing :8080 + clearing stale pg-web containers/stacks/dirs (idempotent, surgical)"
    local img c d
    img="$(pgweb_image)"

    # 1. pgrx dev PG (its BGW holds :8080).
    stop_pgrx_dev_pg

    if command -v docker >/dev/null 2>&1; then
        # 2. Canary probe containers — unique `pgweb-canary-<pid>` prefix, ours.
        for c in $(docker ps -aq --filter 'name=pgweb-canary' 2>/dev/null); do
            mk_step reclaim "rm canary container $c"
            docker rm -f "$c" >/dev/null 2>&1 || true
        done
        # 3. Tier-4 smoke stacks (compose project named after /tmp/pg-web-smoke*).
        for c in $(docker ps -aq --filter 'name=pg-web-smoke' 2>/dev/null); do
            mk_step reclaim "rm smoke container $c"
            docker rm -f "$c" >/dev/null 2>&1 || true
        done
        # 4. Bench compose project — scoped strictly to bench/docker-compose.yml,
        #    so only the `bench` project is ever touched.
        PGWEB_IMAGE="$img" docker compose -f "$PGWEB_REPO_ROOT/bench/docker-compose.yml" \
            down --remove-orphans --volumes >/dev/null 2>&1 || true
        # 5. Orphaned tier-3 testcontainers using OUR image. The
        #    org.testcontainers label marks ephemeral test artifacts (never a
        #    user's long-running service); AND-ing it with our image is surgical.
        for c in $(docker ps -aq --filter "ancestor=$img" --filter 'label=org.testcontainers=true' 2>/dev/null); do
            mk_step reclaim "rm orphaned tier-3 testcontainer $c"
            docker rm -f "$c" >/dev/null 2>&1 || true
        done
    fi

    # 6. Reap clearly-stale smoke dirs + per-run log dirs (older than the longest
    #    plausible run), never the current run's. compose-down inside each smoke
    #    dir first in case a container still references it.
    for d in /tmp/pg-web-smoke /tmp/pg-web-smoke-*; do
        [[ -d "$d" && "$d" != "${SMOKE_DIR:-}" ]] || continue
        if [[ -z "$(find "$d" -prune -mmin -"$PGWEB_REAP_MIN" 2>/dev/null)" ]]; then
            ( cd "$d" 2>/dev/null && docker compose down --remove-orphans --volumes >/dev/null 2>&1 ) || true
            rm -rf "$d" 2>/dev/null || true
        fi
    done
    for d in /tmp/pg-web-test-all-*; do
        [[ -d "$d" && "$d" != "${RUN_DIR:-}" ]] || continue
        if [[ -z "$(find "$d" -prune -mmin -"$PGWEB_REAP_MIN" 2>/dev/null)" ]]; then
            rm -rf "$d" 2>/dev/null || true
        fi
    done
}

# --- self-healing cross-run lock (029 #4) -----------------------------------
# Portable mkdir-based lock (atomic on Linux + macOS) with a PID + start-time
# recorded inside. On contention we decide stale-vs-live instead of refusing:
#   - no PID / dead PID            → reclaim automatically (emit RECLAIMED marker)
#   - PID alive but lock too old   → PID-reuse backstop → reclaim
#   - PID alive and fresh          → a genuinely-running concurrent run → block
#                                     (this is the ONE correct block; FORCE=1 to override)
# After this, FORCE=1 is a rarely-needed escape hatch, not a routine post-crash
# requirement. $1 = lockdir (defaults to PGWEB_LOCKDIR).
acquire_lock() {
    local lockdir="${1:-$PGWEB_LOCKDIR}" owner started now age stale reason
    if mkdir "$lockdir" 2>/dev/null; then
        echo "$$" >"$lockdir/pid" 2>/dev/null || true
        date +%s >"$lockdir/started_at" 2>/dev/null || true
        return 0
    fi
    owner=$(cat "$lockdir/pid" 2>/dev/null || echo "")
    started=$(cat "$lockdir/started_at" 2>/dev/null || echo 0)
    now=$(date +%s); age=$(( now - ${started:-0} ))
    stale=0; reason=""
    if [[ -z "$owner" ]]; then
        stale=1; reason="no pid recorded"
    elif ! kill -0 "$owner" 2>/dev/null; then
        stale=1; reason="owner pid=$owner is dead"
    elif [[ "$age" -gt "$PGWEB_LOCK_MAX_AGE" ]]; then
        stale=1; reason="age ${age}s exceeds cap ${PGWEB_LOCK_MAX_AGE}s (pid=$owner likely reused)"
    fi
    if [[ "$stale" == "1" ]]; then
        mk_note lock RECLAIMED "stale lock auto-reclaimed ($reason)"
        rm -rf "$lockdir" 2>/dev/null || true
        if mkdir "$lockdir" 2>/dev/null; then
            echo "$$" >"$lockdir/pid" 2>/dev/null || true
            date +%s >"$lockdir/started_at" 2>/dev/null || true
            return 0
        fi
    fi
    if [[ "${FORCE:-}" == "1" ]]; then
        mk_note lock FORCE "FORCE=1 — taking over lock (was pid=$owner, age=${age}s); you may see :8080 races"
        rm -rf "$lockdir" 2>/dev/null || true
        mkdir "$lockdir" 2>/dev/null || true
        echo "$$" >"$lockdir/pid" 2>/dev/null || true
        date +%s >"$lockdir/started_at" 2>/dev/null || true
        return 0
    fi
    mk_fail lock "a pg-web run is genuinely active (pid=$owner, age=${age}s, lock=$lockdir)"
    echo "  This is the one case the lock SHOULD block — wait for that run to finish (it frees the lock on exit)."
    echo "  Only if you are certain no other run is active: FORCE=1 $0"
    exit 1
}

# Release the lock on exit — but ONLY if we still own it (pid matches), so we
# never remove a lock another live run holds.
release_lock() {
    local lockdir="${1:-$PGWEB_LOCKDIR}"
    if [[ "$(cat "$lockdir/pid" 2>/dev/null || echo)" == "$$" ]]; then
        rm -rf "$lockdir" 2>/dev/null || true
    fi
}
