#!/usr/bin/env bash
# Build the rtaylor96/pg-web:latest Docker image from source.
#
# ONLY NEEDED BY FRAMEWORK DEVELOPERS who are modifying pg_web_ext.
# Normal app developers should just `cargo install pg-web` and let
# `pg-web up` pull the official published image from Docker Hub.
#
# Run from anywhere — resolves to the repo root. The first build takes a
# while (~5-10 minutes) because it compiles pgrx + our extension against the
# base Postgres image. Subsequent builds are fast-cached via Docker layers
# as long as Cargo.toml/Cargo.lock don't change.
#
# The script now cleans up the previous version of the tag (which would become
# <none>:<none>) plus other dangling images after a successful build. This
# prevents the test harness (test-all.sh + ensure_image_fresh) from cluttering
# Docker Desktop with old builds over repeated runs.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

IMAGE="${PGWEB_IMAGE:-rtaylor96/pg-web:latest}"

# Remember any previous image for this tag. The harness (via ensure_image_fresh
# in test-all.sh, or manual runs) triggers rebuilds on source changes. Without
# cleanup, every rebuild moves the tag and leaves the old image as <none>:<none>,
# which quickly clutters `docker images` / Docker Desktop.
prev_image_id=$(docker image inspect "$IMAGE" --format '{{.Id}}' 2>/dev/null || echo "")

# Compute a content hash over the exact inputs that affect the produced image
# (prompt 025). Sorted paths + per-file sha256sum, then aggregate sha.
# This is robust to mtime noise (branch switch, checkout, re-tag) and detects
# real content changes (including uncommitted edits during dev).
compute_src_hash() {
    # The set must match what test-all.sh ensure_image_fresh watches (plus examples/,
    # which Dockerfile COPYs for the baked `pg-web init --template todo` payload).
    find \
        crates/pg_web_ext/src \
        crates/pg_web_cli/src \
        Dockerfile .dockerignore \
        docker/init-pgweb.sh \
        Cargo.toml Cargo.lock \
        examples \
        -type f 2>/dev/null | sort | xargs sha256sum 2>/dev/null | sha256sum | cut -d' ' -f1
}

SRC_HASH=$(compute_src_hash)
echo "Building $IMAGE from $REPO_ROOT (src_hash=${SRC_HASH:0:12}...)"
docker build \
    --build-arg "PGWEB_SRC_HASH=${SRC_HASH}" \
    -t "$IMAGE" \
    -f Dockerfile \
    .
echo
echo "✓ built $IMAGE (pgweb.src_hash=${SRC_HASH})"

# Clean up the previous build of this tag (now untagged/dangling) and any other
# dangling layers left by the multi-stage build. This keeps Docker Desktop from
# filling up with <none>:<none> images during repeated test harness runs.
if [[ -n "$prev_image_id" ]]; then
  current_id=$(docker image inspect "$IMAGE" --format '{{.Id}}' 2>/dev/null || echo "")
  if [[ "$prev_image_id" != "$current_id" && -n "$current_id" ]]; then
    echo "  Removing previous image for $IMAGE ($prev_image_id) — now dangling"
    docker rmi "$prev_image_id" 2>/dev/null || echo "    (skipped; may still be referenced by a container)"
  fi
fi

# Prune truly dangling images (builder stage leftovers, old untagged builds, etc.).
# Safe and targeted at the common source of clutter from this harness.
echo "  Pruning dangling images..."
docker image prune -f 2>/dev/null | tail -3 || true

echo
echo "Next steps:"
echo "  cd /tmp && pg-web init my-app && cd my-app"
echo "  docker compose up -d"
echo "  pg-web push --url postgres://postgres:devpassword@localhost:5432/app"
echo "  curl http://localhost:8080/"
