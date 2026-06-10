# syntax=docker/dockerfile:1.7
#
# pg-web — all-in-one Postgres image with the pg_web_ext extension preinstalled.
#
# Multi-stage:
#   builder: postgres:17-bookworm + Rust + cargo-pgrx, compiles the extension
#            against the base image's pg_config.
#   runtime: postgres:17-bookworm + the compiled .so/.control/.sql, with
#            shared_preload_libraries wired in via docker-entrypoint-initdb.d.
#
# Built locally for now:
#   docker build -t pgweb/postgres:latest .
# (We'll publish to GHCR / Docker Hub at v0.1 tag.)

ARG PG_MAJOR=17
ARG PGRX_VERSION=0.18.0

# ---------- Stage 1: builder ----------
FROM postgres:17-bookworm AS builder
ARG PG_MAJOR
ARG PGRX_VERSION

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates \
      build-essential \
      libclang-dev \
      libreadline-dev \
      zlib1g-dev \
      flex bison \
      libxml2-dev \
      libxslt1-dev \
      libssl-dev \
      pkg-config \
      ccache \
      postgresql-server-dev-${PG_MAJOR} \
    && rm -rf /var/lib/apt/lists/*

# Rust (minimal profile — no docs, no rustfmt, no clippy)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

# cargo-pgrx pinned to the version in our Cargo.toml
RUN cargo install --locked cargo-pgrx --version =${PGRX_VERSION}

# pgrx against the image's pre-installed Postgres
RUN cargo pgrx init --pg${PG_MAJOR} /usr/bin/pg_config

# Copy the workspace. A .dockerignore keeps target/, .git/, etc. out of the
# context. examples/ comes along too because the CLI's `init.rs` baked the
# `examples/todo/` tree in via `include_dir!` for `pg-web init --template todo`
# — without examples/, the CLI build's proc-macro panics.
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY examples ./examples

WORKDIR /src/crates/pg_web_ext
RUN cargo pgrx install --release \
      --features pg${PG_MAJOR} --no-default-features \
      --pg-config /usr/bin/pg_config

# CLI binary (Session 5 F.3) — `pg-web init/push/migrate/dev/env/check/up/down`.
# Built into the same image so `docker compose exec postgres pg-web push --dir /app`
# works from inside the compose network without publishing :5432 to the host.
WORKDIR /src
RUN cargo build --release -p pg-web

# ---------- Stage 2: runtime ----------
FROM postgres:17-bookworm
ARG PG_MAJOR

# curl is used by the HEALTHCHECK below and is a handy debugging tool.
# Keep the runtime slim otherwise — no build deps carry over.
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

# Extension artifacts
COPY --from=builder /usr/lib/postgresql/${PG_MAJOR}/lib/pg_web_ext.so /usr/lib/postgresql/${PG_MAJOR}/lib/
COPY --from=builder /usr/share/postgresql/${PG_MAJOR}/extension/pg_web_ext.control /usr/share/postgresql/${PG_MAJOR}/extension/
COPY --from=builder /usr/share/postgresql/${PG_MAJOR}/extension/pg_web_ext--*.sql /usr/share/postgresql/${PG_MAJOR}/extension/

# CLI binary (Session 5 F.3). Lives at /usr/local/bin/pg-web so any
# user inside the container (including `docker compose exec`) can run
# `pg-web push --dir /app` directly without a path qualifier.
COPY --from=builder /src/target/release/pg-web /usr/local/bin/pg-web

# Initdb-time hook: append shared_preload_libraries, then CREATE EXTENSION.
# Runs under a short-lived temporary postmaster; the real postmaster that
# picks up after initdb.d scripts finish will then read the updated
# postgresql.conf and register the background worker statically.
COPY docker/init-pgweb.sh /docker-entrypoint-initdb.d/10-pgweb.sh
RUN chmod +x /docker-entrypoint-initdb.d/10-pgweb.sh

# The extension's HTTP listener. Port 5432 is already EXPOSEd by the base.
EXPOSE 8080

# Healthcheck: both the DB (pg_isready) AND the HTTP worker must be up.
HEALTHCHECK --interval=5s --timeout=3s --start-period=30s --retries=12 \
  CMD pg_isready -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-postgres}" \
      && curl -sf http://127.0.0.1:8080/ > /dev/null

LABEL org.opencontainers.image.title="pg-web/postgres"
LABEL org.opencontainers.image.description="PostgreSQL with the pg_web_ext extension preinstalled — HTTP server runs inside PG."
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"
LABEL org.opencontainers.image.source="https://github.com/rt96-hub/pg-web"
