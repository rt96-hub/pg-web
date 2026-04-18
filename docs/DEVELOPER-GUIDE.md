# pg-web — Maintainer's Development Guide

For framework maintainers building pg-web itself. (App developers using pg-web see `APP-DEVELOPER-GUIDE.md`.)

## Environment

- WSL2 Ubuntu-22.04 (or native Linux / macOS). Native Windows is not supported for development.
- Rust stable **1.95+**. Update with `rustup update stable`.
- `cargo-pgrx` **0.18.x**. Install with `cargo install --locked cargo-pgrx`.
- Local Postgres 15/16/17 installations compiled by pgrx. Bootstrap with:
  ```
  cargo pgrx init --pg15 download --pg16 download --pg17 download
  ```
  This downloads Postgres source, compiles each version, and registers them at `~/.pgrx/config.toml`. Takes 20-60 minutes the first time.

### System packages (Debian/Ubuntu)

```
sudo apt install \
  build-essential libclang-dev libreadline-dev zlib1g-dev \
  flex bison libxml2-dev libxslt1-dev libssl-dev \
  pkg-config ccache
```

macOS: `brew install llvm openssl pkg-config ccache`.

## Dev loop

### Extension

From `crates/pg_web_ext/`:

```
cargo pgrx run pg17       # Compile + load ext into fresh PG17 + drop into psql
cargo pgrx run pg16       # Same against PG16
cargo pgrx test pg17      # Run #[pg_test] suite inside live PG17
cargo pgrx install        # Install into a system-wide PG (rare — use Docker instead)
```

`cargo pgrx run` opens a psql session with the extension pre-loaded via `CREATE EXTENSION pg_web;`. When you exit psql (`\q`), the temporary Postgres instance shuts down cleanly.

Standard cargo commands also work but require `--features pgXX` because the ext crate uses pgrx's feature-flag-gated bindgen:

```
cargo check --features pg17 -p pg_web_ext
cargo clippy --features pg17 -p pg_web_ext -- -D warnings
```

### CLI

From the workspace root:

```
cargo build -p pg_web_cli
cargo run -p pg_web_cli -- init my-test-app
cargo test -p pg_web_cli
```

Standard Rust. No pgrx involvement.

### Whole workspace

```
cargo check --workspace --features pg17
cargo clippy --workspace --features pg17 -- -D warnings
```

Note: the `--features pg17` flag is consumed by `pg_web_ext`; `pg_web_cli` ignores it. This is intentional — the extension needs a PG version; the CLI is version-agnostic.

## Writing tests

See `TESTING.md` for full strategy. Maintainer tl;dr:

- **Rust code inside the extension** → `#[pg_test]` + `cargo pgrx test pgXX`.
- **CLI code** → `#[test]` + optional `testcontainers::postgres::Postgres` fixtures.
- **Product behavior** → add a flow in `examples/demo/` (the companion app is THE acceptance gate).

## Workspace conventions

- Resolver 2 (`resolver = "2"` at workspace root).
- Editions pinned per crate (ext: 2021 for pgrx compat; cli: 2021 for now, may bump to 2024).
- Shared profile settings at workspace root. `panic = "unwind"` is **required** by pgrx for both dev and release profiles — pgrx catches Postgres longjmps at the FFI boundary.
- Use `workspace.package` inheritance for `version`, `edition`, `license`, `repository`.
- Target-specific rustflags go in per-crate `.cargo/config.toml` (e.g., the ext's `-Wl,-undefined,dynamic_lookup` on macOS).
- Avoid `rustflags` at the workspace level — it applies to all crates including proc-macros and breaks things.

## Packaging

The canonical distribution artifact is a Docker image based on `postgres:17`.

### Dockerfile sketch

```dockerfile
# Build stage
FROM postgres:17 AS builder
RUN apt update && apt install -y curl build-essential libclang-dev \
    postgresql-server-dev-17 libreadline-dev zlib1g-dev flex bison \
    libxml2-dev libxslt1-dev libssl-dev pkg-config
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH=/root/.cargo/bin:$PATH
RUN cargo install --locked cargo-pgrx --version ~0.18
RUN cargo pgrx init --pg17 /usr/bin/pg_config

COPY crates/pg_web_ext /src/pg_web_ext
COPY Cargo.toml /src/Cargo.toml
WORKDIR /src/pg_web_ext
RUN cargo pgrx install --release

# Runtime stage
FROM postgres:17
COPY --from=builder /usr/lib/postgresql/17/lib/pg_web_ext.so /usr/lib/postgresql/17/lib/
COPY --from=builder /usr/share/postgresql/17/extension/ /usr/share/postgresql/17/extension/
```

Publishing:
- `pgweb/postgres:latest` on Docker Hub
- `ghcr.io/<org>/pg-web-postgres:latest` on GitHub Container Registry
- Versioned tags: `pgweb/postgres:0.1.0`, `pgweb/postgres:pg17-0.1.0`

### CLI distribution

- `cargo install pg-web-cli` — crates.io
- `brew install <tap>/pg-web` — homebrew tap (TBD)
- Prebuilt binaries per release on GitHub Releases

## Versioning

SemVer. Breaking SQL schema changes bump minor or major.

Each bump ships a migration script `crates/pg_web_ext/sql/pg_web--A.B--C.D.sql`. Users upgrade:

```
docker compose pull   # pulls new pgweb/postgres:latest
docker compose up -d  # reloads with new .so
psql> ALTER EXTENSION pg_web UPDATE TO '1.1';
```

Postgres runs the migration script natively. User data untouched.

## Release checklist

Before tagging a release:

1. All phases' deliverables for this version are implemented.
2. `cargo pgrx test pg15`, `cargo pgrx test pg16`, `cargo pgrx test pg17` all green.
3. `cargo test -p pg_web_cli` green.
4. Companion app at `examples/demo/` runs end-to-end in CI against the Docker image.
5. `docs/ROADMAP.md` updated — deliverables checked off; new phase's "open questions" resolved if entering that phase.
6. `docs/ARCHITECTURE.md` updated if any public interface changed.
7. Migration SQL added if schema changed.
8. `CHANGELOG.md` entry.

## Debugging tips

- `cargo pgrx run pg17` drops you into psql with the extension loaded. Attach `rust-gdb` or `rust-lldb` to the Postgres backend PID for Rust-level breakpoints inside `#[pg_extern]` functions.
- For the background worker: find its PID via `SELECT pid FROM pg_stat_activity WHERE backend_type = 'pg_web_worker';` then attach.
- `RUST_LOG=pg_web_ext=trace` in the WSL shell before `cargo pgrx run` enables verbose tracing in the worker (once we wire up `tracing-subscriber`).
- Use Postgres's `auto_explain` extension alongside pg-web to log slow SPI queries: `SET auto_explain.log_min_duration = 100;`.
- If the worker crashes at boot, check Postgres's main log (`cargo pgrx run pg17 --log-level debug5`).
