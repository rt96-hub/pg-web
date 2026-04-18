# pg-web — Current State Overview

Snapshot of what's implemented right now, how the pieces fit, and what's next. Re-generated at every milestone boundary. Read this first; chase into `ARCHITECTURE.md`, `ROADMAP.md`, etc. for depth.

> **Last updated:** 2026-04-18, after M1.1 step 3 completion (commit `c82d50e`).

---

## The 30-second picture

pg-web is a PostgreSQL extension that runs an HTTP server *inside* a Postgres background worker. As of today it can:

1. Install itself via `CREATE EXTENSION pg_web_ext;` — creates the `pgweb` schema, two tables (`routes`, `templates`), and seeds one default `GET /` route.
2. Start an HTTP server on `:8080` when Postgres boots (via `shared_preload_libraries`).
3. Serve a hello-world HTML page on `GET /`, rendered from a database-stored Tera template merged with a SQL-function-returned JSON payload.
4. Return `404` for any other path.

Everything a browser sees comes out of a single OS process tree rooted at the Postgres postmaster. No Node, no Python, no external app server. The web server and the database share memory, not sockets.

---

## Phase 1 — Synchronous Core (current focus)

| Step | Status | Deliverable |
|---|---|---|
| M1.1 step 1 | ✅ done | Framework schema + install SQL (`pgweb.routes`, `pgweb.templates`) |
| M1.1 step 2 | ✅ done | pgrx background worker + Axum HTTP on `:8080` |
| M1.1 step 3 | ✅ done | SPI lookup → Tera render lifecycle |
| **M1.1 step 4** | 🟡 **next** | **CLI `pg-web init` scaffolding** |
| M1.1 step 5 | ⏳ | CLI `pg-web push` (sync filesystem → DB) |
| M1.1 step 6 | ⏳ | Dockerfile + `pgweb/postgres:latest` image |

Later milestones (M1.2 hot-reload, M1.3 todo-list demo, M1.4 secrets/polish) and phases (2 auth/RLS, 3 async jobs, 4 observability) are tracked in `docs/ROADMAP.md`.

---

## Code map (what each file does)

```
pg-web/
├── CLAUDE.md                          # Agent north-star: invariants + coding rules
├── Cargo.toml                         # Workspace (resolver 2, panic = unwind)
├── scripts/
│   ├── test-all.sh                    # One-command CI entry: SQL + HTTP + CLI
│   └── test-http.sh                   # Starts PG if needed, runs http_smoke
├── docs/
│   ├── OVERVIEW.md                    # This file
│   ├── VISION.md                      # Mission statement
│   ├── ARCHITECTURE.md                # Full design (aspirational + current)
│   ├── ROADMAP.md                     # Phases + milestones + decision log
│   ├── DEVELOPER-GUIDE.md             # For maintainers — env + pitfalls
│   ├── APP-DEVELOPER-GUIDE.md         # For framework users — future state
│   ├── TESTING.md                     # Three-tier test strategy
│   └── DEPLOYMENT.md                  # Caddy + Docker + VPS
└── crates/
    ├── pg_web_cli/                    # ⏳ Empty scaffold, populates in M1.1 step 4-5
    │   └── src/main.rs                # hello-world main()
    └── pg_web_ext/                    # ✅ The extension, fully working through M1.1 step 3
        ├── Cargo.toml                 # Deps: pgrx, axum, tokio, tower, tracing,
        │                              #       tera, serde_json, reqwest (dev)
        ├── pg_web_ext.control         # pgrx extension manifest
        ├── sql/                       # (pgrx auto-generates install SQL — currently empty dir)
        ├── src/
        │   ├── lib.rs                 # Entry: pg_module_magic + _PG_init + module decls
        │   ├── schema.rs              # extension_sql! bootstrap — schema, tables, seed data
        │   ├── worker.rs              # BGW entry: SPI connect, Tokio runtime, Axum serve
        │   ├── http.rs                # Axum Router + fallback handler (status codes + headers)
        │   ├── router.rs              # SPI route lookup, handler call, inside a BGW transaction
        │   ├── templating.rs          # Tera render (JSON → HTML) — tiny wrapper
        │   └── logging.rs             # tracing-subscriber with quiet-dep defaults
        └── tests/
            └── http_smoke.rs          # Tier 2a: reqwest vs real :8080
```

---

## Request flow TODAY (what actually happens)

```
Browser: GET /                                                [port 80/443 — prod only]
    │
    ▼ (prod: Caddy terminates TLS, proxies to 8080 — not wired in dev)
    │
Postgres postmaster (already running)
    │
    ▼ (BGW process forked at startup via shared_preload_libraries = 'pg_web_ext')
    │
pg_web_worker process — Tokio single-thread runtime, Axum bound :8080
    │
    ▼ fallback handler
    │
BackgroundWorker::transaction(|| { ... })     ← opens Postgres tx + snapshot
    │
    ├── SPI: SELECT handler_name FROM pgweb.routes WHERE method='GET' AND path='/' LIMIT 1
    │        → "pgweb.hello_handler"
    │
    ├── SPI: SELECT template_path FROM pgweb.routes WHERE ... LIMIT 1
    │        → "pages/index.html"
    │
    ├── SPI: SELECT content FROM pgweb.templates WHERE template_path = 'pages/index.html'
    │        → "<!doctype html>\n<html>...<h1>hello from {{ name }}</h1>..."
    │
    ├── SPI: SELECT (pgweb.hello_handler())::text AS result
    │        → "{\"name\": \"pg-web\"}"
    │
    └── Tera::one_off(template, json, auto_escape=true)
             → "<!doctype html>\n<html>...<h1>hello from pg-web</h1>..."
    │
    ▼ commits tx, returns from closure
    │
Axum: HTTP 200 OK, Content-Type: text/html, body = rendered HTML
    │
    ▼
Browser renders.
```

The whole request is one Postgres transaction. If any step fails, the tx rolls back and Axum returns a 500.

---

## Test story

One command:

```bash
scripts/test-all.sh
```

- **5 SQL tests** (`cargo pgrx test pg17`) — `schema_exists`, `default_route_seeded`, `default_template_seeded`, `default_handler_returns_expected_json`, `routes_table_accepts_additional_inserts`. Each runs inside an isolated Postgres transaction that rolls back on teardown.
- **2 HTTP smoke tests** (`cargo test --test http_smoke`) — `root_renders_seeded_template` (asserts body contains `<h1>hello from pg-web</h1>`), `unknown_route_returns_404`. Requires a running PG + worker on `:8080`; script handles orchestration.
- **0 CLI tests** for now (CLI is empty; populates in step 4-5).

All green today. Cold-compile time ~90 s; incremental run ~2 s.

---

## Dev loop (copy-paste to get started)

One-time, on a fresh WSL2 Ubuntu-22.04:
```bash
# As root
apt update && apt install -y build-essential libclang-dev libreadline-dev \
  zlib1g-dev flex bison libxml2-dev libxslt1-dev libssl-dev pkg-config ccache patchelf
useradd -m -s /bin/bash pgweb

# As pgweb
sudo -iu pgweb
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
echo '. "$HOME/.cargo/env"' >> ~/.bashrc
source ~/.cargo/env
cargo install --locked cargo-pgrx
cargo pgrx init --pg17 download
echo "shared_preload_libraries = 'pg_web_ext'" >> ~/.pgrx/data-17/postgresql.conf

# Clone repo to /home/pgweb/pg-web then:
cd ~/pg-web
scripts/test-all.sh  # all green
```

Daily loop — edit code, then:
```bash
scripts/test-all.sh    # or, if only the ext changed:
cd crates/pg_web_ext && cargo pgrx install --profile dev --pg-config ~/.pgrx/17.9/pgrx-install/bin/pg_config
pg_ctl -D ~/.pgrx/data-17 -m immediate stop
pg_ctl -D ~/.pgrx/data-17 -l ~/.pgrx/17.log start
psql -p 28817 -h localhost -d pg_web_ext -c "DROP EXTENSION IF EXISTS pg_web_ext; CREATE EXTENSION pg_web_ext;"
curl http://localhost:8080/
```

---

## What's NOT wired yet (don't expect these to work)

- **CLI** — `pg_web_cli` crate exists but has no commands implemented. `cargo run -p pg_web_cli` prints "Hello, world!". Populates in M1.1 step 4-5.
- **Hot reload** — save an `.sql` or `.html` file, nothing happens. M1.2.
- **Dynamic routes** — `/posts/[id]` pattern doesn't match `/posts/42`. M1.2.
- **Dev error page** — fatal SQL errors return a generic 500. M1.2.
- **Static assets** — `/styles.css` returns 404. M1.3 (BYTEA) + M1.4 (large-object).
- **Secrets via GUC** — `pg-web env set KEY=VAL` doesn't exist. M1.4.
- **Docker image** — `pgweb/postgres:latest` doesn't exist; you build locally with `cargo pgrx`. M1.1 step 6.
- **Graceful shutdown** — `pg_ctl stop` hangs because Axum doesn't handle SIGTERM; we `-m immediate` instead. Fix eventually.
- **Multi-DB support** — hardcoded to connect to `pg_web_ext` database. Real production needs a `pgweb.database` GUC. M1.4.

---

## Gotchas we've hit (saved for future-you)

Quick reference. Full write-ups in `docs/DEVELOPER-GUIDE.md` § Common pitfalls.

| Symptom | Root cause | Fix |
|---|---|---|
| `initdb: cannot be run as root` | PG safety check | Dev as non-root user (`pgweb`) |
| `$PGRX_HOME does not exist` | Wrong user's home dir | Same — use `pgweb` |
| `unacceptable schema name "pg_web"` | `pg_` prefix reserved by PG | Schema is `pgweb` (no underscore) |
| `libpq.so.5: cannot open shared object file` | Absolute RPATH baked at compile | `patchelf --set-rpath` or re-init pgrx |
| Git Bash eats `$vars` passed to `wsl` | MSYS path-conv layer | `MSYS_NO_PATHCONV=1 wsl ...` |
| `cargo pgrx run` doesn't auto-`CREATE EXTENSION` | pgrx design | Type it yourself at psql prompt |
| BGW HTTP server crashes on first request with `SIGABRT` | SPI needs `BackgroundWorker::transaction` wrapper | Wrap every SPI call block |
| rustc 1.95 ICE on `[DatumWithOid; 2]` | rustc + pg_guard macro interaction | Use `format!` + Rust-side escape |
| `Spi::get_one` returns `Err(InvalidPosition)` not `Ok(None)` on no rows | pgrx quirk | Wrap in a normalizer helper |
| Extension BGW must be in `shared_preload_libraries` for worker to start | static vs dynamic BGW registration paths | Preload-line in `postgresql.conf` |

---

## Next up — M1.1 step 4

Implement the `pg-web init` CLI command:

```
pg-web init my-app
```

Creates:
```
my-app/
├── pages/
│   ├── index.html           # Tera template
│   └── index.sql            # CREATE FUNCTION ... RETURNS json
├── public/                  # (empty, with .gitkeep)
├── migrations/              # (empty, with .gitkeep)
├── pgweb.toml               # Framework config
├── docker-compose.yml       # pgweb/postgres + Caddy
├── Caddyfile
└── .gitignore
```

Pure filesystem work — no SPI, no async, no rustc-bug minefield. Should land fast.

Tests go under `crates/pg_web_cli/tests/` using `tempfile::tempdir` + `assert_fs`. These'll populate the "CLI" tier in the test story.
