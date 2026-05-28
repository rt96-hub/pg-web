# pg-web — Current State Overview

Snapshot of what's implemented right now and what's next. Re-generated at milestone boundaries. Read this first; chase into `APP-DEVELOPER-GUIDE.md`, `APP-LAYOUT.md`, `ARCHITECTURE.md`, `ROADMAP.md` for depth.

> **Last updated:** 2026-04-25, end of Session 5 / `v0.2.0`. Phase-1 feature surface plus the deferred polish track: push retry on concurrent DDL (L), CLI bundled in `pgweb/postgres:latest` (F.3), content-hash asset filenames + immutable cache-control (H), and the BYTEA cap-raise from 2 MiB to 20 MiB (cap-raise variant of I). SSH-tunneled remote push (F.2) deferred to Session 6 — needs a real remote target to validate. True `pg_largeobject` streaming (full I) deferred to Phase 2+. See `docs/sessions/session_5.md` for the shipping log, `docs/sessions/session_5_validation.md` for the user-validation playbook, and `CHANGELOG.md` for the release notes.

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

## Phase 1 — Synchronous Core (`v0.1.0` core + `v0.2.0` polish)

| Milestone | Status | Deliverable |
|---|---|---|
| M1.1 Walking Skeleton       | ✅ shipped Session 1 | Framework schema + BGW + Axum + SPI→Tera + CLI `init`/`push` + Docker image |
| M1.3 Interactive demo + spec | ✅ shipped Session 2 | Migrations, directory-as-route layout, `(req json)` contract, `_404` fallback, demo todo app, Docker E2E |
| M1.2 Interactive Dev Loop   | ✅ shipped Session 3 | `pg-web up`/`down`/`dev`, hot reload, dynamic routes, dev error page, static assets |
| M1.4 Closeout               | ✅ shipped Session 4 | `html_escape`, validation UX, `env`/`check`, `init --template`, push `--dry-run`/`--with-migrate`, `pgweb.deployments` ledger, browser live-reload, release pipeline |
| v0.2 polish track            | ✅ shipped Session 5 | Push retry on concurrent DDL (L), CLI in image (F.3), content-hash assets (H), 20 MiB asset cap (I cap-raise) |

(Session 2 did M1.3 before M1.2 because the interactive-contract decisions — request-JSON shape, POST return dispatch, 404 fallback — had to be locked before the file-watcher would know what to re-sync.)

Later phases (2 auth/RLS, 3 async jobs, 4 observability) are tracked in `docs/ROADMAP.md`.

**Session 2 — M1.3 components:**
- **A** ✅ `pgweb.migrations` ledger + `pg-web migrate apply` CLI (`c3960a3`)
- **B** ✅ Directory-as-route layout: `paths::scan()` + `push.rs` walker (`21cc831`)
- **C** ✅ Router `(req json)` + text-return dispatch + `_404` fallback (`af50911`)
- **D** ✅ `examples/todo/` todo app + `docs/TUTORIAL.md` (`7fed892`)
- **E** ✅ Docker E2E tier against `pgweb/postgres:latest` (`c2c4985`)

**Session 3 — M1.2 components** (see `docs/sessions/session_3.md` for the recap table):
- `pg-web up` / `pg-web down` + port-shadowing preflight
- `pg-web dev` file watcher (200 ms debounce + Blake3 dedupe + shift-left SQL preflight)
- Dynamic route patterns (`[id]` → `:id` + `req.path_params`)
- Dev error page (PGWEB_E001–E999 typed catalog) + generic prod 500
- Static assets under `public/*` with Blake3 ETag + `If-None-Match`
- Push-time Tera template validation

**Session 4 — M1.4 components** (full shipping log in `docs/sessions/session_4.md`):
- **A** ✅ `pgweb.html_escape()` SQL helper (`e41b522`)
- **B** ✅ Form-validation UX — `check_violation` → inline OOB error (`2966864`)
- **C** ✅ `pg-web env set/unset/list` + `pgweb.setting()` helper (`97bfaa2`)
- **D** ✅ `pg-web init --template todo` + scaffolded README (`1eb0cd0`)
- **E** ✅ `pg-web check` — offline project validator (`1b2afef`)
- **F.1** ✅ Push polish: `--dry-run`, `--with-migrate`, `pgweb.deployments` ledger (`42b725d`)
- **G** ✅ Browser live-reload via SSE + channel-aware LISTEN router (`537d909`)
- **J** ✅ v0.1.0 release artifacts: CHANGELOG, version bump, CI/release workflows (`6ad214b`)
- **K** ✅ docs sweep + close-out (`5157c8e`)

**Session 5 — v0.2 polish** (full shipping log in `docs/sessions/session_5.md`):
- **L** ✅ Push retry on concurrent DDL + sibling-pusher diagnostic (`ed55de4`)
- **F.3** ✅ CLI bundled in `pgweb/postgres:latest` (`7eaf724`)
- **H** ✅ Content-hash asset filenames + `immutable` Cache-Control (`62c8cd7`)
- **I** ✅ Larger asset cap (BYTEA 2 MiB → 20 MiB) — cap-raise variant of the planned `pg_largeobject` work (`db6fb0d`)
- **F.2** ⬜ deferred to Session 6 — needs real remote infra to validate
- **(true streaming)** ⬜ Phase 2+ — `lo_read`-backed assets >20 MiB

**Longer-term direction** (parking lot, see ROADMAP):
- Documentation-focused MCP + marketplace skills so agents writing pg-web apps have excellent access to the real docs, CLAUDE.md, invariants, etc.
- Related but further out: simple CLI data access (`pg-web query` / `pg-web psql`) and eventual runtime data MCP for agents to reach the actual tables in a running app.

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

| Tier | Command | Tests at `v0.2.0` |
|---|---|---|
| 1. SQL / pgrx  | `cargo pgrx test pg17`                              | **72** — schema / seed / migrations / deployments ledger / settings helper / html_escape; ListenRouter fan-out + livereload injection; router `(req json)` contract + dynamic captures + asset lookup; error-catalog + dev-page formatting; Tera parse-vs-render classification; fingerprinted-URL detection (Session 5 H) |
| 2a. HTTP smoke | `scripts/test-http.sh`                              | 2 `#[test]` — `GET /` renders seeded template, unknown path returns default 404 body |
| 2b. CLI        | `cargo test -p pg_web_cli`                          | **143** — path scanner, migrate, push + reconcile + F.1 flags + L retry helper, application_name parser, init (including `--template todo`), dev classifier, env parser, check validator, stack, asset fingerprinting + rewrite (Session 5 H) |
| 3. Docker E2E  | `cargo test -p pg_web_cli --test docker_e2e -- --ignored` | **13** — todo CRUD + dynamic routes; watcher re-push; reconcile; push rejects missing handler / broken template; dev error page + prod 500; static asset ETag / 304 / reconcile; F.1 migration-gate + ledger; livereload SSE end-to-end; concurrent push retry (L); CLI-in-image push (F.3); fingerprinted asset cache (H); 5 MiB asset round-trip (I) |
| 4. CLI smoke   | `scripts/smoke-cli.sh`                              | **19 sections** — scaffold → up → push → 404 fallback → SQL exception dev page → broken template rejected → prod-mode hides internals → static CSS + ETag + 304 + reconcile → `pgweb.html_escape` end-to-end → `check_violation` inline error → `env set/list/unset` + `pgweb.setting()` round-trip → deployments ledger + `--dry-run` rollback + `--with-migrate` gate → `pg-web check` clean + bad migration + bad Tera → livereload injection + prod-404 |

**230 Rust tests + 19-section black-box smoke, all five tiers green via `scripts/test-all.sh`.**

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

## Not in `v0.2.0`

Deferred (still inside Phase 1's polish tail):

- **`pg-web push --target <name>`** — SSH-tunneled remote push. Local-loopback push works and the F.3 in-image CLI handles the "SSH in and push from inside" case; what's missing is the laptop-to-VPS automated tunnel. Validation requires a real remote target, so this slid to Session 6 when remote infra is available.
- **True `pg_largeobject` streaming.** v0.2 ships a 20 MiB BYTEA cap (Component I cap-raise variant) — covers virtually every practical asset. `lo_read`-backed streaming for assets >20 MiB is Phase 2+ work.

Longer-term / speculative (parking lot, see ROADMAP):

- Documentation MCP server + packaged agent skills so AIs writing pg-web code have first-class access to the authoritative docs, invariants, error catalog, etc.
- Related but further-out idea: lightweight `pg-web query` / `pg-web psql` CLI helpers (and eventually a data MCP) for agents to inspect the actual tables inside a running pg-web app.

Deferred to **Phase 2+** (explicit non-goals for Phase 1):

- **Declarative migrations** — `pg-web migrate create` doesn't exist; raw SQL migrations via `migrate apply` is the 0.1 story. **Phase 2.5.**
- **Auth / sessions / RLS bridge.** Write your own RLS policies today. **Phase 2.**
- **App-level realtime subscriptions** — `<div hx-ext="sse" sse-connect="/_pgweb/subscribe/...">` for live data push. The channel-aware `ListenRouter` primitive shipped in 0.1 Component G; Phase 2 adds the app-facing SSE endpoint + NOTIFY helper on top.
- **Async job queue.** **Phase 3.**
- **In-browser dev dashboard.** **Phase 4.**

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
