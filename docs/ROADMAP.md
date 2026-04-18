# pg-web — Roadmap

Phased delivery. Each phase must be stable, shippable, and usable on its own — no half-shipped phases. The companion app at `examples/demo/` exercises every feature of every phase.

## Phase 1 — The Synchronous Core (current focus)

**Goal:** a working framework that can serve a real HTMX app end-to-end. Broken into four milestones so we can validate each piece before layering the next.

### Milestone 1.1 — Walking Skeleton (the "hello world" moment)

Goal: `pg-web init` a project, `docker compose up -d`, `pg-web push`, `curl localhost:8080/` returns HTML rendered by Tera from a template stored in Postgres.

- [x] pgrx extension scaffolded (`crates/pg_web_ext/`); workspace compiles on PG 15/16/17.
- [x] Local pgrx dev environment (`cargo pgrx init` → PG 15.17/16.13/17.9 in `~/.pgrx/`).
- [x] Background worker registered via `BackgroundWorkerBuilder`; boots with extension.
- [x] HTTP server (Axum) binds `:8080` inside the worker.
- [x] Framework schema (`pgweb`) + minimal tables: `routes`, `templates`.
- [x] Request lifecycle (happy path only): SPI route lookup → SPI handler call → Tera render → HTTP response.
- [x] Tera template engine integrated; auto-escape on by default.
- [x] CLI `pg-web init` — scaffolds `pages/index.html`, `pages/index.sql`, `docker-compose.yml`, `pgweb.toml`, `Caddyfile`, `.gitignore`.
- [x] CLI `pg-web push` — one-shot sync of routes + templates to a local/remote DB.
- [x] Docker image `pgweb/postgres:latest` (PG 17 + extension preinstalled). Local build via `scripts/build-image.sh`; registry publishing deferred to v0.1 tag.
- [ ] Companion app `examples/demo/` at this stage = minimal hello-world page that proves the loop works. **This is NOT the first "real" demo — see Milestone 1.3.**

### Milestone 1.2 — Interactive Dev Loop

Goal: a developer can run `pg-web dev`, save a `.sql` file, and see the change reflected at `localhost:8080` without restarting anything. The CLI also owns stack lifecycle — users shouldn't need to think about `docker compose` directly for day-to-day work.

- [ ] CLI `pg-web up` — starts the Docker Compose stack, waits for PG + `:8080` readiness, resolves `DATABASE_URL` from `pgweb.toml`, prints the URL. Shortcut that replaces manual `docker compose up -d` + remembering the connection string.
- [ ] CLI `pg-web down` — stops the stack. `--volumes` flag drops the data volume.
- [ ] CLI `pg-web dev` — file watcher on `pages/` and `public/`. Auto-invokes `up` if stack isn't running. Auto-re-pushes on `.sql`/`.html` save. Streams container logs.
- [ ] Shift-left SQL pre-flight: parse and run in `BEGIN; ... ROLLBACK;` before applying live.
- [ ] Dynamic route patterns — `pages/posts/[id].html` matches `/posts/:id` with `id` threaded into `req.path_params`.
- [ ] Dev-mode error page (SQLSTATE, MESSAGE, DETAIL, HINT, file, line, transaction state).
- [ ] Production-mode generic 500 page.
- [ ] Structured JSON logging: NOTICE/LOG capture → stdout.
- [ ] Static asset serving (BYTEA for < 1 MiB, `pg_largeobject` with streaming for ≥ 1 MiB).

### Milestone 1.3 — First Real Demo App (todo list)

Goal: `examples/demo/` is a fully functional todo list with real DB interactions, not a toy. Exercises the framework surface from the user's POV.

- [ ] Todo list schema (raw `.sql` migration files — no declarative diffing yet).
- [ ] CLI `pg-web migrate apply` — runs raw SQL migrations in order, records in `migrations` ledger. (No `migrate create` / diffing in Phase 1 — punted to later phase. Users hand-write migration SQL.)
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
- [ ] CLI `pg-web init --template <name>` — fetches a named example (initially `todo-demo`) from this repo's `examples/` tree and drops it into the user's directory. Mirrors Next.js's `create-next-app --example <name>` pattern. Opt-in; plain `init` stays the minimal hello-world scaffold.
- [ ] CLI `pg-web check` — offline project validator (no IDE/LSP). Walks `pages/`, `migrations/`, `pgweb.toml`; reports:
  - Layout violations (flat `.html` under `pages/`, reserved stems, missing sibling files when required).
  - Tera template parse errors (compile templates, don't render).
  - SQL parse errors (via `BEGIN; ...; ROLLBACK;` against a throwaway Postgres, or `pg_query` crate for parse-only checks — decide at implementation time).
  - Return-type mismatches (handler declared `text` but template exists, or vice versa).
  - Migration filename ordering + ledger drift against a configured DB.
  Output: grouped diagnostics with file + line, exit non-zero if any found. Intended as a pre-push safety net and CI gate.
- [ ] SQL helper `pgweb.html_escape(text) → text` shipped in the extension's install SQL for raw-text-return handlers that interpolate user content.
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
  - `pgweb.session_create(user_id)` → returns signed cookie value
  - `pgweb.session_validate(cookie)` → returns user_id or null
- [ ] Rust worker: on each request, read `Cookie` header, call `session_validate` via SPI, set `pgweb.user_id` for the transaction via `SET LOCAL`.
- [ ] **RLS Bridge:** documented pattern for user tables:
  ```sql
  CREATE POLICY tenant_isolation ON posts
    USING (author_id = current_setting('pgweb.user_id')::bigint);
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
- [ ] `pgweb.jobs` table + state machine (pending / running / succeeded / failed / retrying).
- [ ] SQL API: `pgweb.enqueue(job_type, payload, run_at?)`.
- [ ] Async job runner: polls queue, dispatches to registered handlers (HTTP, email, generic).
- [ ] Built-in handlers: HTTP request (via `reqwest`), email (SMTP via `lettre`).
- [ ] **Internal concurrency management:** HTTP-level queue inside the web worker's Tokio runtime. Traffic spikes absorbed at the web tier before opening SPI transactions — prevents Postgres connection exhaustion.
- [ ] Health endpoints (`/_pgweb/health`, `/_pgweb/metrics`) for load balancer probes.
- [ ] Companion app: webhook handler, email confirmation on signup (via job queue).

## Phase 4 — Observability & Tooling

**Goal:** developers can debug and profile pg-web apps without leaving the browser.

### Deliverables

- [ ] `/_pgweb/dashboard` — token-protected in-browser admin UI served by the extension.
- [ ] Live request trace viewer: per-request SPI query list, timing, memory allocation.
- [ ] Slow-query ring buffer with EXPLAIN ANALYZE output.
- [ ] PL/pgSQL breakpoint support via `pldbgapi` integration.
- [ ] Dev mode: rich error overlay injected into the browser on fatal SQL exception.
- [ ] Metrics export in Prometheus format.
- [ ] Companion app: dashboard walkthrough in its README.

## Parking lot — post-v1 ideas

Speculative. Not yet scoped into a phase; parked here so the thinking isn't lost.

- **Project-in-database backup.** Store the full app source tree (and optionally its `.git/` history) inside framework-owned tables so that `pg_dump` produces a self-contained snapshot of *schema + data + app code*, and `pg_restore` reconstitutes a runnable app from just the dump. Extends the "Postgres is the substrate" thesis end-to-end: you can hand someone a `.dump` file and they have the whole system. Open questions before scoping: where source rows live (framework schema vs a dedicated `pgweb.sources` schema), how big a real `.git/` objects directory gets (CRINGE if multi-GB per commit), whether to store the working tree only (smallest) or objects+refs (restorable repo), and how `pg-web push` + `migrate apply` interact with this (push-on-commit hook that mirrors the working tree into DB rows?). Likely Phase 5+ once the core framework has settled.

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
- *2026-04-17* — **Framework schema named `pgweb`** (no underscore), tables `pgweb.routes` / `pgweb.templates` / `pgweb.assets_*` etc. Rationale: Postgres reserves schema names starting with `pg_` for system schemas (`CREATE SCHEMA pg_web` returns `SQLSTATE 42939 reserved_name`). Underscore-prefixed alternatives like `_pg_web` still trip up convention. `pgweb` reads cleanly, matches the Docker Hub namespace (`pgweb/postgres`), and avoids all reserved-name collisions. Table names inside the schema don't need a double prefix — schema name already scopes them.
- *2026-04-17* — **Dedicated WSL user `pgweb` (uid 1001) for development,** not root. Rationale: Postgres's `initdb` refuses to run as root, breaking `cargo pgrx test` / `cargo pgrx run`. `/home/pgweb/pg-web` is the project path; `/home/pgweb/.pgrx/` holds local PG installs.
- *2026-04-18* — **App layout: directory = route, filename = method.** Each directory under `pages/` is a URL route; `index.html`/`index.sql` = GET, `post.html`/`post.sql` = POST. Either file is optional — `.html` alone = static, `.sql` alone = raw-text handler, both = JSON→Tera pipeline. Flat `pages/about.html` no longer valid. Full spec in `docs/APP-LAYOUT.md`. Rationale: one mental model for pages, API endpoints, and HTMX fragments — simpler than Next.js (which splits page.tsx vs route.ts) and SvelteKit (which uses `+page.server.ts` actions). Canonical DX for our HTMX-first target.
- *2026-04-18* — **Handler signature: single `json` arg, shape `{ body, query, method, path }`.** Every `.sql` handler is `pgweb.pages__<name>(req json) RETURNS <json|text>`. `body` and `query` always objects (never null) — `req->'body'->>'key'` always safe. Uniform signature keeps the router code path singular and leaves room to grow (`path_params`, `session`, `headers`) without re-signing every handler.
- *2026-04-18* — **POST return contract: dispatch via `template_path` nullability.** If `.html` sibling exists → CLI writes `template_path` → router expects JSON return + Tera render. If only `.sql` → `template_path` NULL → router expects text return + bytes-as-is. No new schema column, no per-route flag; filesystem is source of truth. Alternatives (per-route `skip_template` bool, `pg_proc.prorettype` lookup each request) rejected as either redundant with filesystem state or a per-request performance cost.
- *2026-04-18* — **CLI owns the full dev loop; Docker should be invisible day-to-day.** The target UX is `cargo install pg-web-cli` → `pg-web init` → `pg-web up` → `pg-web dev`, with the CLI managing compose, pulling the published image on first run, and auto-resolving `DATABASE_URL`. Scoped to M1.2 (`up`/`down`/`dev`) + M1.4 (published image + `init --template`). Rationale: lowering the install surface matters as much as the runtime model; if users have to think about `docker compose up -d` and connection strings every session, the "Postgres is your whole stack" pitch gets tax-heavy. Mirrors `next dev` / `rails server` — one command, stack handled.
