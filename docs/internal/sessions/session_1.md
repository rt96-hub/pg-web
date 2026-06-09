# Session 1 — M1.1 Walking Skeleton

**Dates:** 2026-04-17 → 2026-04-18
**Status:** ✅ complete. Every M1.1 deliverable shipped and verified end-to-end via Docker.
**Final commit:** `5a64daa` (feat: pgweb/postgres Docker image + end-to-end deploy)

---

## What we set out to do

Turn pg-web from zero code into a running "hello-world" stack a developer can touch: scaffold an app with a CLI, push it into a Postgres running inside a Docker container, see the rendered HTML in a browser.

Milestone decomposition (from `ROADMAP.md`):

1. Framework schema + install SQL
2. Background worker + Axum HTTP on `:8080`
3. SPI → Tera render lifecycle
4. CLI `pg-web init`
5. CLI `pg-web push`
6. Dockerfile + `pgweb/postgres:latest`

---

## What we actually built

### The extension (`crates/pg_web_ext`)

- pgrx 0.18 extension, target Postgres 15/16/17 (default feature `pg17`).
- Install SQL (`schema.rs`) creates the `pgweb` schema with two tables — `routes` and `templates` — plus a seeded `GET /` route and template so `CREATE EXTENSION pg_web_ext;` produces an immediately-curlable page.
- `_PG_init` registers a Postgres background worker. Branches on `process_shared_preload_libraries_in_progress`: the production path uses `.load()` for a static worker; the dev path (CREATE EXTENSION from a backend) uses `.load_dynamic()`.
- Worker (`worker.rs`) attaches to its target DB via SPI, boots a **single-threaded** Tokio runtime, and binds Axum on `:8080`. Target DB comes from `PGWEB_DATABASE` env var, falling back to `POSTGRES_DB`, falling back to `pg_web_ext` (pgrx dev default).
- Per-request flow (`router.rs`): open a `BackgroundWorker::transaction(|| ...)` → SPI lookup of route + template + handler function → call handler → render via Tera → return HTML. Unknown paths return 404; SPI errors return 500.
- Logging (`logging.rs`) uses `tracing-subscriber` with quiet-dep defaults (`axum=warn,tower=warn,hyper=warn,tokio=warn`; our own crate at `info`).

### The CLI (`crates/pg_web_cli`)

- `pg-web init <name>` scaffolds a new app directory with `pages/`, `public/`, `migrations/`, `pgweb.toml`, `docker-compose.yml`, `Caddyfile`, `.gitignore`. Templates live inline in `src/templates.rs` with a single `{APP}` substitution marker.
- `pg-web push --url <DATABASE_URL>` walks the local `pages/` directory and, in one transaction, executes every `.sql` handler file (`CREATE OR REPLACE FUNCTION`), UPSERTs every `.html` into `pgweb.templates`, and UPSERTs a matching row in `pgweb.routes` per HTML file. Path-to-route-and-handler conversion lives in `src/paths.rs` (pure functions, unit tested).

### The Docker image

- Multi-stage `Dockerfile` on `postgres:17-bookworm`. Builder stage installs Rust + cargo-pgrx and compiles the extension in release mode. Runtime stage keeps only the `.so`, `.control`, install `.sql`, plus `curl` (healthcheck + debugging).
- `docker/init-pgweb.sh` runs once on initial container bootstrap: appends `shared_preload_libraries='pg_web_ext'` to `postgresql.conf`, then `CREATE EXTENSION pg_web_ext` in `POSTGRES_DB`.
- `scripts/build-image.sh` builds `pgweb/postgres:latest` locally. No registry push.

### Documentation & ops

- `docs/OVERVIEW.md` — single-page "where are we, how to try it, known gaps, 8-item gotcha table."
- `docs/DEVELOPER-GUIDE.md` grew a "Common pitfalls" section (annotated history of everything we hit).
- `docs/TESTING.md` rewritten to document the actual three-tier reality (SQL / HTTP smoke / CLI unit).
- `scripts/test-all.sh` — single-command CI entry. Runs all three tiers in order. Exits non-zero on any failure.
- `scripts/test-http.sh` — handles PG start/stop, extension reset, and version-flavor `.so` reinstall.

---

## Key architectural decisions locked in this session

- **`pgweb` schema (no underscore).** Postgres reserves schema names starting with `pg_`. We hit `SQLSTATE 42939 reserved_name` on our first `CREATE SCHEMA pg_web`. Decision log entry in `ROADMAP.md`.
- **Axum as a thin shell, not a router.** Our routing is data-driven from `pgweb.routes`. Axum's fallback handler + Tower middleware fits perfectly; we never used its compile-time router. Keeps migration to raw Hyper a one-day job.
- **Single-threaded Tokio runtime.** SPI is bound to the worker's main thread. A multi-threaded runtime would let async tasks migrate to worker threads that lack SPI attachment, panicking on any SQL call.
- **`BackgroundWorker::transaction` wraps every request.** Calling `Spi::*` from a BGW without transaction/snapshot setup aborts the process with SIGABRT. Root cause of an early "empty reply from server" that cost a debugging hour.
- **Env-var-driven target DB (`PGWEB_DATABASE` / fallback `POSTGRES_DB`).** Hardcoding `pg_web_ext` would have broken Docker deployments where `POSTGRES_DB` is something else.
- **Raw-SQL migrations in Phase 1, declarative diffing deferred.** We don't parse Prisma/DBML yet. `migrations/*.sql` files run in order via `pg-web migrate apply` (which itself is deferred to session 2).
- **String-escaping parameterized queries instead of `DatumWithOid` arrays.** rustc 1.95 hits an internal compiler error on `[DatumWithOid; N]` with `N >= 2` under the `#[pg_guard]` macro. Documented workaround; revisit when M1.2 needs real parameterization.
- **Dual MIT/Apache-2.0 license.** Rust-ecosystem default.

---

## Gotchas we hit (each saved to memory + `docs/DEVELOPER-GUIDE.md`)

1. `initdb: error: cannot be run as root` — dev user `pgweb` (uid 1001).
2. `$PGRX_HOME does not exist` — same root cause, wrong `$HOME`.
3. `unacceptable schema name "pg_web"` — pgweb (no underscore).
4. `libpq.so.5: cannot open shared object file` after moving `.pgrx` — `patchelf --set-rpath` on every binary and `.so` (dozens of files).
5. Git Bash on Windows mangles `$vars` and `/paths` when passing to `wsl` — `MSYS_NO_PATHCONV=1` prefix.
6. `cargo pgrx run` does not auto-run `CREATE EXTENSION` — you type it yourself.
7. `.bashrc` changes don't apply in the current shell — `source ~/.bashrc` (or reopen).
8. `:8080` host-port conflict between pgrx dev PG and the Docker container — stop one or the other; diagnose with `ss -tlnp | grep 8080`.
9. pgrx 0.18 requires `extern "C-unwind"` (not `extern "C"`) on BGW entry functions and `#[unsafe(no_mangle)]` (Rust 1.82+ form). Plain `extern "C"` produces a rustc ICE inside the `#[pg_guard]` macro.
10. rustc 1.95 ICEs on `[DatumWithOid; N]` arrays with N ≥ 2 under `#[pg_guard]` expansion. Workaround: `format!` + Rust-side `quote_literal`.
11. `Spi::get_one` returns `Err(pgrx::spi::Error::InvalidPosition)` — not `Ok(None)` — when the query matches zero rows. Wrap in a normalizer helper (`get_one_optional` in `router.rs`).
12. `BackgroundWorkerBuilder::load()` is a silent no-op when called from a regular backend. For CREATE-EXTENSION-path registration, use `.load_dynamic()`. We branch on `process_shared_preload_libraries_in_progress`.
13. pgrx schema generator turns on `cfg(test)` during introspection. A test module gated `#[cfg(any(test, feature = "pg_test"))]` therefore emits test-wrapper SQL into the runtime install, which the non-test `.so` cannot satisfy. Gate with `#[cfg(feature = "pg_test")]` only.
14. `cargo pgrx test` and `cargo pgrx install` write to the same `.so` / install-SQL paths. `scripts/test-http.sh` now rm's and reinstalls the runtime-flavor artifact before smoke tests.

---

## Tests

- **5 SQL `#[pg_test]`** — schema existence, seed data presence, default handler JSON shape, additional-insert sanity.
- **2 HTTP smoke `#[test]`** — `/` renders the seeded template, unknown path 404s.
- **18 CLI tests** — 10 path-conversion unit tests (route + handler + template-path derivation, incl. nested and Windows-backslash forms), 6 `init` integration tests, 2 `push` hermetic tests.

**25 total, all green** via `scripts/test-all.sh`. Cold compile ~90 s, incremental ~2 s.

---

## Commits this session (chronological)

```
86e352a  chore: initial workspace scaffold
942dc94  feat(ext): install pgweb schema and framework tables on CREATE EXTENSION
2edeae5  docs: capture bringup pitfalls in DEVELOPER-GUIDE
90b48ae  feat(ext): register background worker serving Axum HTTP on :8080
e3a63ec  fix(ext): support both shared_preload and CREATE EXTENSION load paths
bc6b3d0  test: add HTTP smoke tier and one-command CI entrypoint
898ce8b  chore: update Cargo.lock for reqwest dev-dependency
c82d50e  feat(ext): SPI -> Tera render lifecycle (M1.1 step 3)
2b5ee16  docs: add OVERVIEW.md — snapshot of current state after M1.1 step 3
0cd5a2b  feat(cli): scaffold pg-web init command (M1.1 step 4)
ae750b6  docs: mark M1.1 step 4 done in OVERVIEW + ROADMAP
b3f774d  docs+licenses: flag unimplemented CLI sections, add LICENSE files
f806269  feat: pg-web push + env-var DB selection (M1.1 step 5)
5a64daa  feat: pgweb/postgres Docker image + end-to-end deploy (M1.1 step 6)
```

---

## What a developer can do today

```bash
# once, to build the image:
bash scripts/build-image.sh

# normal flow:
pg-web init my-app
cd my-app
docker compose up -d
pg-web push --url postgres://postgres:devpassword@localhost:5432/app
curl http://localhost:8080/
```

Edit `pages/index.html` or `pages/index.sql`, re-push, refresh browser. Live.

---

## What the developer *cannot* do yet (by design, falls to later sessions)

- **Submit forms.** Request bodies aren't wired to SQL handlers — tracked as session 2's first task.
- **Dynamic routes** (`/posts/:id` patterns).
- **Hot reload.** Every edit needs an explicit `pg-web push`.
- **Static assets** (`/styles.css` → 404).
- **Migrations via `pg-web migrate apply`**. Write raw SQL and push manually for now.
- **Secrets** (`pg-web env set`).
- **Published Docker image.** `pgweb/postgres:latest` builds locally; no registry upload.

---

## Handoff to session 2

See `docs/sessions/session_2.md` for the next session's target + work items.
