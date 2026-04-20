# pg-web — Current State Overview

Snapshot of what's implemented right now and what's next. Re-generated at milestone boundaries. Read this first; chase into `APP-DEVELOPER-GUIDE.md`, `APP-LAYOUT.md`, `ARCHITECTURE.md`, `ROADMAP.md` for depth.

> **Last updated:** 2026-04-18, end of Session 2. M1.1 walking-skeleton shipped in Session 1 (`5a64daa`). Session 2 completes M1.3 (interactive todo demo) — five components A–E shipped (`c3960a3`, `21cc831`, `af50911`, `7fed892`, `c2c4985`). Next session targets M1.2 (interactive dev loop). See `docs/sessions/session_3.md`.

---

## The 30-second picture

pg-web is a PostgreSQL extension that runs an HTTP server *inside* a Postgres background worker. Today it can:

1. Install itself via `CREATE EXTENSION pg_web_ext;` — creates the `pgweb` schema (`routes`, `templates`, `migrations`) and seeds one default `GET /` route.
2. Start an HTTP server on `:8080` when Postgres boots (via `shared_preload_libraries`).
3. Serve HTML rendered from a database-stored Tera template merged with a SQL-function-returned JSON payload.
4. Return `404` for any unmatched path.

A CLI (`pg-web`) scaffolds apps (`init`), applies forward-only SQL migrations (`migrate apply`), and syncs the `pages/` tree into the DB (`push`). A prebuilt Docker image (`pgweb/postgres:latest`) packages PG 17 + the extension for one-command bringup.

Everything a browser sees comes out of a single OS process tree rooted at the Postgres postmaster. No Node, no Python, no external app server.

---

## Phase 1 — Synchronous Core (current focus)

| Milestone | Status | Deliverable |
|---|---|---|
| M1.1 Walking Skeleton       | ✅ shipped Session 1 | Framework schema + BGW + Axum + SPI→Tera + CLI `init`/`push` + Docker image |
| M1.3 Interactive demo + spec | ✅ shipped Session 2 | Migrations, directory-as-route layout, `(req json)` contract, `_404` fallback, demo todo app, Docker E2E |
| M1.2 Interactive Dev Loop   | ⬜ Session 3 next    | `pg-web up`/`down`/`dev`, hot reload, dynamic routes, dev error page, static assets |
| M1.4 Closeout               | ⬜ planned           | Secrets (GUC), `pg-web check` lint, `pg-web init --template`, release pipeline |

(Session 2 did M1.3 before M1.2 because the interactive-contract decisions — request-JSON shape, POST return dispatch, 404 fallback — had to be locked before the file-watcher would know what to re-sync. M1.2 inherits the stable spec.)

Later phases (2 auth/RLS, 3 async jobs, 4 observability) are tracked in `docs/ROADMAP.md`.

**Session 2 — M1.3 components:**
- **A** ✅ `pgweb.migrations` ledger + `pg-web migrate apply` CLI (`c3960a3`)
- **B** ✅ Directory-as-route layout: `paths::scan()` + `push.rs` walker (`21cc831`)
- **C** ✅ Router `(req json)` + text-return dispatch + `_404` fallback (`af50911`)
- **D** ✅ `examples/demo/` todo app + `docs/TUTORIAL.md` (`7fed892`)
- **E** ✅ Docker E2E tier against `pgweb/postgres:latest` (`c2c4985`)

---

## Code map

```
pg-web/
├── CLAUDE.md                         # Agent north-star: invariants + coding rules
├── Cargo.toml                        # Workspace (resolver 2, panic = unwind)
├── Dockerfile                        # pgweb/postgres:latest image
├── scripts/
│   ├── test-all.sh                   # One-command CI entry: SQL + HTTP + CLI
│   ├── test-http.sh                  # Starts PG if needed, runs http_smoke
│   └── build-image.sh                # Local Docker image build
├── docker/
│   └── init-pgweb.sh                 # First-boot script: preload + CREATE EXTENSION
├── docs/
│   ├── OVERVIEW.md                   # This file
│   ├── VISION.md                     # Mission statement
│   ├── ARCHITECTURE.md               # Engine internals
│   ├── ROADMAP.md                    # Phases + decision log
│   ├── APP-LAYOUT.md                 # ⭐ Canonical spec: file/route conventions
│   ├── APP-DEVELOPER-GUIDE.md        # For framework users — narrative walkthrough
│   ├── DEVELOPER-GUIDE.md            # For framework maintainers — env + pitfalls
│   ├── TESTING.md                    # Three-tier test strategy
│   ├── DEPLOYMENT.md                 # Caddy + Docker + VPS
│   └── sessions/                     # Per-session recaps and plans
└── crates/
    ├── pg_web_cli/                   # `pg-web` binary — init/push/migrate
    │   └── src/{init,paths,push,migrate,templates}.rs
    └── pg_web_ext/                   # The extension (cdylib via pgrx)
        └── src/{lib,schema,worker,http,router,templating,logging}.rs
```

---

## Request flow (what happens on every GET)

```
Browser: GET /                              [prod: :443 via Caddy → :8080]
    │
Postgres postmaster (background worker forked at startup)
    │
pg_web_worker process — Tokio single-thread runtime, Axum :8080
    │
    ▼ fallback handler
    │
BackgroundWorker::transaction(|| { ... })     ← opens Postgres tx + snapshot
    │
    ├── SPI: SELECT handler_name, template_path
    │        FROM pgweb.routes
    │        WHERE method='GET' AND path_pattern='/' LIMIT 1
    │
    ├── SPI: SELECT content FROM pgweb.templates
    │        WHERE template_path='pages/index.html'         [skipped if template_path NULL]
    │
    ├── SPI: SELECT (pgweb.hello_handler())::text AS result  [Session 2 C: `(req json)`]
    │
    └── Tera::one_off(template, json, auto_escape=true)      [skipped if text mode]
    │
    ▼ commits tx, returns from closure
    │
Axum: HTTP 200, Content-Type: text/html, body = rendered HTML
```

One request = one Postgres transaction. Any exception → rollback + 500.

Session 2 C will add: request body parsing, `req` JSON construction, and a branch that sends raw text (no Tera) when `template_path` is NULL.

---

## Test story

One command:

```bash
scripts/test-all.sh
```

| Tier | Command | Tests (today) |
|---|---|---|
| 1. SQL / pgrx  | `cargo pgrx test pg17`                              | 8 `#[pg_test]` — schema + seed + migrations ledger + `(req json)` handler contract |
| 2a. HTTP smoke | `scripts/test-http.sh`                              | 2 `#[test]` — `GET /` renders seeded template, unknown path returns default 404 body |
| 2b. CLI        | `cargo test -p pg_web_cli`                          | 47 — path scanner (layout, reserved stems, `_404`), migrate apply, push, init, demo-layout regression |
| 3. Docker E2E  | `cargo test -p pg_web_cli --test docker_e2e -- --ignored` | 1 — boots `pgweb/postgres:latest`, drives full todo CRUD + 404 via HTTP |

**58 tests all green via `scripts/test-all.sh`.**

Feature matrix in `docs/TESTING.md` tracks which deliverables are demo-covered.

---

## Try it — the Docker path

```bash
# 1. Build the image (one-time, ~5-10 min cold; cache-hit after that)
bash scripts/build-image.sh

# 2. Scaffold an app and boot it
cargo build -p pg_web_cli
cd /tmp
~/pg-web/target/debug/pg-web init demo-app
cd demo-app
~/pg-web/target/debug/pg-web up          # Session 3 M1.2 A — wraps `docker compose up -d`

# 3. Advance schema (only needed once you've written migrations/*.sql) and push app code
~/pg-web/target/debug/pg-web migrate apply        # URL auto-resolved from pgweb.toml + env
~/pg-web/target/debug/pg-web push

# 4. Hit it
curl http://localhost:8080/
```

Edit `demo-app/pages/index.html` or `.sql`, re-run push, refresh.

## Dev loop without Docker (framework maintainers)

One-time on fresh WSL2 Ubuntu-22.04:

```bash
# As root
apt update && apt install -y build-essential libclang-dev libreadline-dev \
  zlib1g-dev flex bison libxml2-dev libxslt1-dev libssl-dev pkg-config ccache patchelf
useradd -m -s /bin/bash pgweb

# As pgweb
sudo -iu pgweb
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
cargo install --locked cargo-pgrx
cargo pgrx init --pg17 download
echo "shared_preload_libraries = 'pg_web_ext'" >> ~/.pgrx/data-17/postgresql.conf

cd ~/pg-web
scripts/test-all.sh   # all green
```

Daily iteration is `scripts/test-all.sh` and editing code.

---

## What's NOT wired yet

- **Hot reload** — save `.sql`/`.html`, nothing happens. Re-run push. **M1.2 (Session 3).**
- ~~**CLI stack management**~~ — `pg-web up` / `pg-web down` land **Session 3 Component A**. `pg-web dev` (file watcher) is still pending in Component B. `migrate apply` / `push` now auto-resolve `DATABASE_URL` from `pgweb.toml` + env, so `--url` is optional.
- **Dynamic routes** — `[id]` patterns don't match yet. **M1.2.**
- **Dev error page** — fatal SQL exceptions return generic 500 today. **M1.2.**
- **Static assets** — `public/*` → 404 still. **M1.2.**
- **Secrets** — `pg-web env set KEY=VAL` doesn't exist. **M1.4.**
- **Project validator** — `pg-web check` for offline lint. **M1.4.**
- **`pg-web init --template <name>`** — scaffold a prebuilt example. **M1.4.**
- **HTML-escape SQL helper** (`pgweb.html_escape()`) — recommended for raw-text handlers that interpolate user content. Until then, prefer the `.html` + `.sql` mode whenever dynamic values are involved. **M1.4.**
- **Published Docker image** — build locally with `scripts/build-image.sh`. Publishing to Docker Hub / GHCR is a v0.1 release task (M1.4).
- **Declarative migrations** — `pg-web migrate create` doesn't exist; raw SQL migrations via `migrate apply` (which does exist, Session 2 A). **Phase 2.5.**

---

## Gotchas (full write-ups in `docs/DEVELOPER-GUIDE.md` § Common pitfalls)

| Symptom | Root cause | Fix |
|---|---|---|
| `initdb: cannot be run as root` | PG safety check | Dev as non-root user (`pgweb`) |
| `$PGRX_HOME does not exist` | Wrong user's home dir | Same — use `pgweb` |
| `unacceptable schema name "pg_web"` | `pg_` prefix reserved | Schema is `pgweb` (no underscore) |
| `libpq.so.5: cannot open shared object file` | RPATH baked at compile | `patchelf --set-rpath` |
| Git Bash eats `$vars` passed to `wsl` | MSYS path-conv layer | `MSYS_NO_PATHCONV=1 wsl ...` |
| BGW HTTP crashes on first request with `SIGABRT` | SPI needs `BackgroundWorker::transaction` | Wrap every SPI call block |
| rustc 1.95 ICE on `[DatumWithOid; 2]` | rustc + `pg_guard` macro | `format!` + Rust-side escape |
| `Spi::get_one` returns `Err(InvalidPosition)` on zero rows | pgrx quirk | Normalizer helper |
| Extension BGW must be in `shared_preload_libraries` OR loaded dynamically | Two registration paths | Branch on `process_shared_preload_libraries_in_progress` |
| Docker `:8080` conflict with dev PG | Host-port already bound | Stop one: `pg_ctl -D ~/.pgrx/data-17 stop` |
| `cargo pgrx test` runs `tests/http_smoke.rs` too and fails cold | Cargo test runs all integration tests | `#![cfg(not(feature = "pg_test"))]` gate on http_smoke |
| `pgrx` schema generator fails on `#[cfg(test)]` modules | Schema introspection turns on `cfg(test)` | Gate test modules with `#[cfg(feature = "pg_test")]` only |
