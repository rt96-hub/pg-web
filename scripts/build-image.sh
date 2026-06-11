#!/usr/bin/env bash
# Build the pgweb/postgres:latest Docker image from source.
#
# ONLY NEEDED BY FRAMEWORK DEVELOPERS who are modifying pg_web_ext.
# Normal app developers should just `cargo install pg-web` and let
# `pg-web up` pull the official published image from Docker Hub.
#
# Run from anywhere — resolves to the repo root. The first build takes a
# while (~5-10 minutes) because it compiles pgrx + our extension against the
# base Postgres image. Subsequent builds are fast-cached via Docker layers
# as long as Cargo.toml/Cargo.lock don't change.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

IMAGE="${PGWEB_IMAGE:-pgweb/postgres:latest}"

echo "Building $IMAGE from $REPO_ROOT ..."
docker build -t "$IMAGE" -f Dockerfile .
echo
echo "✓ built $IMAGE"
echo
echo "Next steps:"
echo "  cd /tmp && pg-web init my-app && cd my-app"
echo "  docker compose up -d"
echo "  pg-web push --url postgres://postgres:devpassword@localhost:5432/app"
echo "  curl http://localhost:8080/"
