# pg-web — Maintainer's Development Guide

For framework maintainers hacking on `pg_web_ext` and `pg_web_cli`. App developers should read `APP-DEVELOPER-GUIDE.md` instead.

This guide focuses on the daily development loop, key architectural constraints, workspace conventions, testing expectations, packaging, and the gotchas that actually bite people working on the code.

Platform bring-up details (exact WSL2 steps, user creation, Git Bash quirks, etc.) are left to the reader. The original development happened on WSL2 Ubuntu with a dedicated non-root `pgweb` user; native Linux and macOS also work. See `HANDOFF.md` or the git history of this file if you want one person's cold-start notes.

## Reference Environment Requirements

- A working `cargo-pgrx` development setup for Postgres 15, 16, and 17.
- `cargo-pgrx` pinned to `~0.18` (matches the version in `Cargo.toml`).
- Rust stable (1.95+ recommended).
- Docker (required for tier 3 Docker E2E and tier 4 smoke tests; `scripts/test-all.sh` will fail loudly without the `pgweb/postgres:latest` image).
- A non-root user for pgrx work (Postgres `initdb` refuses to run as root).

The critical one-time pgrx configuration step (not optional):

```bash
echo "shared_preload_libraries = 'pg_web_ext'" >> ~/.pgrx/data-17/postgresql.conf
# Repeat for data-15 and data-16 if you test those versions
```

This is required so the background worker is registered at postmaster startup. `CREATE EXTENSION` alone is not enough.

After changing the conf, restart the instance: `cargo pgrx stop pg17 && cargo pgrx run pg17`.

## Dev Loop

### Extension (`crates/pg_web_ext`)
```bash
cargo pgrx run pg17          # Compile, install .so, start PG, drop into psql
cargo pgrx run pg16          # Same against PG 16
cargo pgrx test pg17         # Run the #[pg_test] suite (recommended for most work)
cargo pgrx install           # Install into a real system PG (rare; prefer the Docker image)
```

- `cargo pgrx run` gives you an interactive psql session with the extension loaded, but you must still type `CREATE EXTENSION pg_web_ext;` yourself the first time.
- `cargo pgrx test` handles extension creation automatically inside each test transaction.
- Plain cargo commands require the version feature:
  ```bash
  cargo check --features pg17 -p pg_web_ext
  cargo clippy --features pg17 -p pg_web_ext -- -D warnings
  ```

### CLI (`pg_web_cli`)
```bash
cargo build -p pg-web
cargo run -p pg-web -- init test-app --template todo
cargo test -p pg-web
```

Pure Rust. No pgrx features needed.

### Whole Workspace
```bash
cargo check --workspace --features pg17
cargo clippy --workspace --features pg17 -- -D warnings
```

The `--features pg17` flag only affects the extension crate; the CLI ignores it.

Before committing non-trivial changes, run the above plus the relevant `cargo pgrx test pgXX` and `scripts/test-all.sh` (when Docker is available).

## Key Architectural Constraints

### BGW Connection Accounting
The background worker uses Postgres backend slots as follows:

- **1 SPI session (always)**: Established at worker startup via `BackgroundWorker::connect_worker_to_spi`. Every HTTP request uses `BackgroundWorker::transaction` on this session for route lookup + handler execution.
- **1 libpq LISTEN session (development only)**: Opened when `pgweb.settings.env = 'development'`. Used by the livereload mechanism (Component G). It `LISTEN`s on `pgweb_livereload` and forwards notifications through the in-memory `ListenRouter`.

**Totals**: 2 backend slots in dev, 1 in production.

The extra dev slot is usually negligible against `max_connections = 100`, but it matters on very small instances. The fan-out from one LISTEN to N browser SSE tabs happens entirely in-memory via `tokio::sync::broadcast` — no additional backends.

Phase 2 app-level realtime subscriptions are designed to reuse the same LISTEN connection (the `ListenRouter` is channel-agnostic).

### Tokio Runtime Constraint
The background worker runs a single-threaded runtime:

```rust
tokio::runtime::Builder::new_current_thread()
```

**Why**: SPI context is pinned to the specific OS thread that performed `connect_worker_to_spi`. A multi-threaded runtime can migrate tasks, causing panics when they later touch SPI.

**Rules for new code**:
- Anything that calls SPI (directly or via `BackgroundWorker::transaction`) must stay on the main task of the current-thread runtime.
- `tokio::spawn` is fine inside our current-thread runtime (everything shares the thread).
- Pure network I/O (e.g., the livereload `tokio-postgres` LISTEN task) can be spawned more freely.
- Never introduce code that assumes a multi-threaded runtime when touching the extension's request path.

This constraint is fundamental. Read `worker.rs` and `listen_router.rs` before adding async features.

## Workspace Conventions

- Workspace resolver = 2.
- `panic = "unwind"` in both dev and release profiles (required by pgrx to catch Postgres longjmps at the FFI boundary).
- Use `workspace.package` for shared metadata (version, license, repository).
- Feature flags on the extension crate are version-specific (`pg15`/`pg16`/`pg17`) because of pgrx bindgen.
- **Ambient environment injection for testability**: When code reads from `std::env`, the clock, or other globals, accept a closure or trait object instead of calling the global directly. Production passes the real reader; tests pass a mock. See `stack::resolve_database_url` for the pattern.
- Prefer focused, default-off features on dependencies (e.g., `toml = { version = "0.8", default-features = false, features = ["parse"] }`).
- Shell out thoughtfully: inherit stdout/stderr for user-visible commands (`stack::up`/`down`); pipe when you need to prefix or capture logs (`dev::spawn_logs_tail`).

## Testing & Acceptance

See `TESTING.md` for the full five-tier strategy.

Maintainer tl;dr:
- Extension internals that touch Postgres → `#[pg_test]` + `cargo pgrx test pgXX`.
- CLI logic → normal `#[test]`, often using `testcontainers` for Postgres fixtures.
- **Product behavior and user-visible contracts**: Add or extend a flow in `examples/todo/`. This is the acceptance gate. If a feature isn't exercised in the companion app (or the dogfooded docs site), it isn't done.
- `scripts/test-all.sh` is the one-command entry point. Tier 3 (Docker E2E) is mandatory — no silent skips.

Run `pg-web check` (using the built CLI) against `examples/todo/` as part of your workflow.

## Packaging & Distribution

The canonical shipped artifact is the Docker image `pgweb/postgres:latest` (based on `postgres:17`).

### Dockerfile Responsibilities
- Builder stage installs the system deps needed for pgrx + the extension, runs `cargo pgrx install --release`, and captures the generated `.so` + extension SQL files.
- Runtime stage is a minimal `postgres:17` image that only copies the built artifacts.
- The image also bakes in the `pg-web` CLI binary (see F.3).

After any change to `crates/pg_web_ext/src/schema.rs`, any SQL under the extension, or the Dockerfile itself, you must rebuild the image before running tier 3 or tier 4 tests:

```bash
bash scripts/build-image.sh
```

`scripts/test-all.sh` does **not** auto-rebuild the image on every run (it would be too slow).

### CLI Distribution
- `cargo install pg-web` (the published crate name).
- Prebuilt binaries on GitHub Releases.
- Homebrew (planned).

## Versioning & Releases

Follow SemVer. SQL-visible schema changes (new tables, changed function signatures, etc.) bump at least the minor version.

Extension upgrades are handled by Postgres itself:
- A migration script `pg_web_ext--A.B--C.D.sql` is generated as part of the pgrx build.
- Users run `ALTER EXTENSION pg_web_ext UPDATE;` after pulling the new image.

Before tagging a release:
1. All planned deliverables for the version are implemented.
2. `cargo pgrx test pg15`, `pg16`, and `pg17` are green.
3. `cargo test -p pg-web` is green.
4. `examples/todo/` exercises the new/changed behavior end-to-end via Docker.
5. `docs/ROADMAP.md` is updated (deliverables checked, open questions resolved if entering a new phase).
6. `docs/ARCHITECTURE.md` updated if public interfaces changed.
7. Migration SQL added if the extension schema changed.
8. `CHANGELOG.md` entry written.
9. `cargo check --workspace`, `cargo clippy --workspace -- -D warnings` clean.

## Debugging Tips

- `cargo pgrx run pg17` → psql with the extension. Use `rust-gdb` or `rust-lldb` attached to the backend PID for breakpoints inside `#[pg_extern]` functions.
- For the background worker: `SELECT pid FROM pg_stat_activity WHERE backend_type = 'pg_web_worker';` then attach.
- `RUST_LOG=pg_web_ext=trace` (when tracing is wired) for verbose worker output.
- Postgres `auto_explain` is useful for slow SPI queries: `SET auto_explain.log_min_duration = 100;`.
- If the worker crashes early, check the Postgres log (`cargo pgrx run pg17 --log-level debug5`).
- Port conflicts between the pgrx dev PG (`:8080`) and Docker stacks are common — `pg-web up` has a preflight, but manual `docker ps` + `cargo pgrx stop pg17` is often needed during mixed development.

## Common Gotchas

These are real issues that have bitten during development. The numbered list below focuses on the ones most likely to affect code changes. Many environment-specific bring-up problems (especially around WSL2, Git Bash, and user permissions) are documented in the git history of this file and in `docs/internal/sessions/`.

A running list of gotchas also appears in the "Gotchas" table in `docs/OVERVIEW.md`.

### Core Framework Gotchas

**3. `unacceptable schema name "pg_web"` (SQLSTATE 42939)**

Postgres reserves `pg_*` schema names. The framework deliberately uses `pgweb` (no underscore). See the 2026-04-17 decision in `docs/ROADMAP.md`.

**6. `cargo pgrx run` does not auto-run `CREATE EXTENSION`**

`cargo pgrx run pg17` installs the `.so` and drops you into psql, but the `pgweb` schema and tables are not created until you execute `CREATE EXTENSION pg_web_ext;`. `cargo pgrx test` does this automatically inside its transactions.

**11. `pgweb.pages__*(json) RETURNS json|text` is a reserved namespace**

`pg-web push` creates, owns, and reconciles (drops) every function matching this exact signature. Do not define your own helpers in this namespace with this signature, or the next push will remove them. Safe patterns: `pgweb.helper_*`, `pgweb.util_*`, or anything whose argument list or return type differs.

**13. The Docker image bakes the install SQL**

Changes to extension install SQL (in `schema.rs` or `sql/` files) only take effect in the image after `bash scripts/build-image.sh`. Tier 3 and tier 4 tests will happily run against a stale image and give false confidence. Rebuild the image after touching anything that affects the extension's `CREATE EXTENSION` output.

**15. Watcher `strip_prefix` requires a canonical `app_dir`**

`pg-web dev` (with default `--dir .`) receives absolute paths from the notify debouncer. `strip_prefix(".")` on an absolute event path fails, causing the classifier to `Ignore` every change. Always canonicalize `app_dir` early in the dev path. Unit tests must cover the actual runtime shape the CLI produces (not just convenient fixtures).

**16. `tee` masks pipeline exit codes**

`cmd | tee log` reports `tee`'s exit code (always 0) to the parent, even if `cmd` failed under `set -e`. For unattended runs where exit code matters, capture the code explicitly first:

```bash
cmd > log 2>&1; echo "EXIT=$?"; tail log
```

Or use `set -o pipefail` + `tee` only when you control the shell.

### Environment & pgrx Bring-up Gotchas (Common Across Setups)

**8. `:8080` port conflicts between pgrx dev PG and Docker**

Both the local `cargo pgrx run` Postgres and the scaffolded `docker-compose.yml` want to publish `:8080`. `pg-web up` has a preflight that catches non-Docker holders. When mixing dev styles, use `cargo pgrx stop pg17`, `docker ps`, and `docker stop` as needed. Immediate stop (`-m immediate`) is often required because the BGW's tokio runtime does not drain cleanly on SIGINT.

**18. Stale `pg-web up` containers can shadow the dev BGW**

A leftover container holding `:8080` can cause the pgrx dev PG's background worker to fail to bind (silently). Tier 2a smoke then talks to the container instead of the dev instance. The `application_name` tagging on CLI connections helps spot this in `pg_stat_activity`. `scripts/test-http.sh` now does a port-shadow preflight.

### Other Notable Gotchas

**9. `notify_debouncer_full` re-exports `notify`, but `Watcher` is not in the prelude**

You must explicitly `use notify_debouncer_full::notify::{..., Watcher};` to call `.watch(...)`. The compiler error is not helpful.

**10. `pg-web dev` log tailing hardcodes the compose service name**

It does `docker compose logs -f --no-log-prefix postgres`. Renaming the service in your compose file will make `--logs` go silent. The scaffold template is the contract.

**17. rustc 1.95 ICEs in `mir_borrowck` are often just a missed `let mut`**

A panic ending in borrowck during a test run can be the ordinary "cannot borrow immutable binding as mutable" error that got swallowed. Look for a `let foo = ...` followed by a `&mut self` call on it; adding `let mut` usually fixes it.

---

This document should stay relatively short and focused on the things that affect daily code changes and architecture. Detailed session-by-session war stories and one-off debugging notes belong in `docs/internal/sessions/`. 

When in doubt, re-read `CLAUDE.md` (the invariants) before making changes.