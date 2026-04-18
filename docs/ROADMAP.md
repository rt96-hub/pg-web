# pg-web — Roadmap

Phased delivery. Each phase must be stable, shippable, and usable on its own — no half-shipped phases. The companion app at `examples/demo/` exercises every feature of every phase.

## Phase 1 — The Synchronous Core (current focus)

**Goal:** a working framework that can serve a real HTMX app end-to-end. Broken into four milestones so we can validate each piece before layering the next.

### Milestone 1.1 — Walking Skeleton (the "hello world" moment)

Goal: `pg-web init` a project, `docker compose up -d`, `pg-web push`, `curl localhost:8080/` returns HTML rendered by Tera from a template stored in Postgres.

- [x] pgrx extension scaffolded (`crates/pg_web_ext/`); workspace compiles on PG 15/16/17.
- [x] Local pgrx dev environment (`cargo pgrx init` → PG 15.17/16.13/17.9 in `~/.pgrx/`).
- [ ] Background worker registered via `BackgroundWorkerBuilder`; boots with extension.
- [ ] HTTP server (**Axum leaning** — see ARCHITECTURE.md) binds `:8080` inside the worker.
- [ ] Framework schema (`pg_web`) + minimal tables: `_pg_web_routes`, `_pg_web_templates`.
- [ ] Request lifecycle (happy path only): SPI route lookup → SPI handler call → Tera render → HTTP response.
- [ ] Tera template engine integrated; auto-escape on by default.
- [ ] CLI `pg-web init` — scaffolds `pages/index.html`, `pages/index.sql`, `docker-compose.yml`, `pg_web.toml`, `Caddyfile`.
- [ ] CLI `pg-web push` — one-shot sync of routes + templates to a local/remote DB.
- [ ] Docker image `pgweb/postgres:latest` (PG 17 + extension preinstalled).
- [ ] Companion app `examples/demo/` at this stage = minimal hello-world page that proves the loop works. **This is NOT the first "real" demo — see Milestone 1.3.**

### Milestone 1.2 — Interactive Dev Loop

Goal: a developer can run `pg-web dev`, save a `.sql` file, and see the change reflected at `localhost:8080` without restarting anything.

- [ ] CLI `pg-web dev` — file watcher on `pages/` and `public/`.
- [ ] Shift-left SQL pre-flight: parse and run in `BEGIN; ... ROLLBACK;` before applying live.
- [ ] Dynamic route patterns — `pages/posts/[id].html` matches `/posts/:id` with `id` as a SQL parameter.
- [ ] Dev-mode error page (SQLSTATE, MESSAGE, DETAIL, HINT, file, line, transaction state).
- [ ] Production-mode generic 500 page.
- [ ] Structured JSON logging: NOTICE/LOG capture → stdout.
- [ ] Static asset serving (BYTEA for < 1 MiB, `pg_largeobject` with streaming for ≥ 1 MiB).

### Milestone 1.3 — First Real Demo App (todo list)

Goal: `examples/demo/` is a fully functional todo list with real DB interactions, not a toy. Exercises the framework surface from the user's POV.

- [ ] Todo list schema (raw `.sql` migration files — no declarative diffing yet).
- [ ] CLI `pg-web migrate apply` — runs raw SQL migrations in order, records in `_pg_web_migrations` ledger. (No `migrate create` / diffing in Phase 1 — punted to later phase. Users hand-write migration SQL.)
- [ ] CRUD routes (index, create, update, delete, toggle complete).
- [ ] HTMX patterns: form submit with `hx-swap-oob`, out-of-band swap on validation errors.
- [ ] Unique/check constraint validation exercised in the todo "add" flow.
- [ ] At least one `public/` static asset (CSS file) styling the app.
- [ ] `README.md` in `examples/demo/` documenting the app, its migrations, and every framework feature it touches.

### Milestone 1.4 — Remaining Phase 1 Feature Surface

Goal: close out Phase 1 for a releasable v0.1.

- [ ] CLI `pg-web env set KEY=VAL` / `env list` / `env unset KEY` — GUC injection for secrets.
- [ ] Asset serving in the demo app with a large asset (image via `pg_largeobject`).
- [ ] `pg-web push` polished for prod deploy (transaction-wrapped, migration-runner integrated).
- [ ] Release pipeline: CI builds Docker image, runs full test matrix (PG 15/16/17), publishes on tag.
- [ ] Docs pass: APP-DEVELOPER-GUIDE revised against the actual demo app.

### Known Phase 1 limitations (deliberately deferred)

- SQL handlers that call external APIs (Stripe, etc.) will block the HTTP worker thread until the API returns. **Fixed in Phase 3** via the async job queue.
- No declarative schema-diffing (`pg-web migrate create`). Users hand-write `migrations/NNNN_name.sql`. **Schema-diffing (from Prisma / DBML / DB introspection) is punted to a later phase.**
- No auth, no sessions, no RLS bridge. **Delivered in Phase 2**.
- No in-browser debugger dashboard. **Delivered in Phase 4.**

## Phase 2 — Security & Identity

**Goal:** pg-web can safely run a multi-user app.

### Deliverables

- [ ] Native cookie-based session management. Framework-provided SQL helpers:
  - `pg_web.session_create(user_id)` → returns signed cookie value
  - `pg_web.session_validate(cookie)` → returns user_id or null
- [ ] Rust worker: on each request, read `Cookie` header, call `session_validate` via SPI, set `pg_web.user_id` for the transaction via `SET LOCAL`.
- [ ] **RLS Bridge:** documented pattern for user tables:
  ```sql
  CREATE POLICY tenant_isolation ON posts
    USING (author_id = current_setting('pg_web.user_id')::bigint);
  ```
- [ ] CSRF protection (double-submit cookie pattern, automatic on HTMX non-GET requests).
- [ ] Password hashing helpers using `pgcrypto` (`crypt` + `gen_salt('bf', 12)`).
- [ ] `pg-web init --with-auth` template variant.
- [ ] Companion app: extend the todo list with signup, login, per-user rows backed by RLS.

### Open questions (resolve before starting Phase 2)

- Session cookie format: opaque (DB-stored) vs signed-JWT-like (stateless)?
- Secret key rotation strategy.
- OAuth provider integration — in-scope for Phase 2 or defer?

## Phase 2.5 — Schema Tooling (floating between Phase 2 and 3)

**Goal:** `pg-web migrate create` generates SQL migrations from a declarative schema source. Deferred from Phase 1 to avoid front-loading complexity.

### Options to evaluate

- Parse a `schema.prisma` file (adds a Prisma-parser dep).
- Parse DBML (simpler grammar, less familiar to most devs).
- Stay on raw SQL + `pg_dump --schema-only` diffing against the live DB (no new parser needed).

Decision can wait until we have the Phase 1 demo running and can see which pain is real.

## Phase 3 — Async & Scale

**Goal:** pg-web survives real-world traffic and external API blocking.

### Deliverables

- [ ] Second pgrx background worker dedicated to job queues.
- [ ] `pg_web._pg_web_jobs` table + state machine (pending / running / succeeded / failed / retrying).
- [ ] SQL API: `pg_web.enqueue(job_type, payload, run_at?)`.
- [ ] Async job runner: polls queue, dispatches to registered handlers (HTTP, email, generic).
- [ ] Built-in handlers: HTTP request (via `reqwest`), email (SMTP via `lettre`).
- [ ] **Internal concurrency management:** HTTP-level queue inside the web worker's Tokio runtime. Traffic spikes absorbed at the web tier before opening SPI transactions — prevents Postgres connection exhaustion.
- [ ] Health endpoints (`/_pg_web/health`, `/_pg_web/metrics`) for load balancer probes.
- [ ] Companion app: webhook handler, email confirmation on signup (via job queue).

## Phase 4 — Observability & Tooling

**Goal:** developers can debug and profile pg-web apps without leaving the browser.

### Deliverables

- [ ] `/_pg_web/dashboard` — token-protected in-browser admin UI served by the extension.
- [ ] Live request trace viewer: per-request SPI query list, timing, memory allocation.
- [ ] Slow-query ring buffer with EXPLAIN ANALYZE output.
- [ ] PL/pgSQL breakpoint support via `pldbgapi` integration.
- [ ] Dev mode: rich error overlay injected into the browser on fatal SQL exception.
- [ ] Metrics export in Prometheus format.
- [ ] Companion app: dashboard walkthrough in its README.

## Out of scope (for v1.x; revisit post-1.0)

- Managed-DB compatibility (RDS, Cloud SQL, Supabase). Fundamentally requires upstream vendor cooperation to allow custom extensions.
- Non-HTMX frontends (React/Vue/Svelte). Deliberately an HTMX-first framework.
- TLS termination inside the extension. Always via Caddy/Nginx/Traefik.
- Multi-database support (MySQL, SQLite). Postgres-only.
- GraphQL. Over HTTP JSON is fine if someone wants to build it on top; the framework won't ship with it.
- Server-sent events / WebSockets. May revisit if HTMX 2.x SSE support stabilizes.

## Decision log

Track architectural decisions here as they solidify. Each entry: date, decision, rationale, alternatives considered.

- *2026-04-17* — **pgrx 0.18.0 pinned**. Latest stable at project start; supports PG 15/16/17.
- *2026-04-17* — **Dual MIT/Apache-2.0 license**. Rust-ecosystem default; permissive enough for enterprise adoption.
- *2026-04-17* — **WSL2 Ubuntu 22.04 for maintainer dev**. Native Windows pgrx support exists but is painful; WSL is the paved path.
- *2026-04-17* — **Schema diffing (Prisma/DBML-based `migrate create`) deferred out of Phase 1.** Phase 1 ships raw-SQL migrations only. Rationale: complexity doesn't buy us anything for the walking skeleton; we can see what pain is real after Phase 1 lands.
- *2026-04-17* — **Milestone 1.1 walking skeleton includes CLI + Docker Compose,** not just the extension. Rationale: user experience loop must work end-to-end from day one; an extension without the CLI scaffolding is not a validated framework.
- *2026-04-17* — **First "real" companion app = todo list** (Milestone 1.3). The walking-skeleton hello-world at 1.1 is not the demo app — it's just proof-of-life. The todo list exercises migrations, HTMX forms, validation, and static assets honestly.
- *2026-04-17* — **Axum chosen as HTTP library**, used as a thin shell. Rationale: pg-web's "routing lives in the DB" model maps cleanly to Axum's fallback-handler pattern; Tower middleware simplifies per-request SPI transaction wrapping + request-ID tracing; Axum doesn't hide Hyper, so dropping to raw Hyper later stays cheap. Hyper-raw was considered (for tighter control / fewer abstractions) but rejected for the cost of rebuilding URL/header/query parsing for a small HTTP surface. Actix-web was considered and rejected over governance concerns and weaker Tower composability.
