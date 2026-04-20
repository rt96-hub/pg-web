# pg-web — Roadmap

Phased delivery. Each phase must be stable, shippable, and usable on its own — no half-shipped phases. The companion app at `examples/demo/` exercises every feature of every phase.

## Phase 1 — The Synchronous Core (current focus)

**Goal:** a working framework that can serve a real HTMX app end-to-end. Broken into four milestones so we can validate each piece before layering the next.

**Session mapping:** Session 1 → M1.1. Session 2 → M1.3 (contract locks + demo). Session 3 → M1.2 (interactive dev loop; the contracts settled in Session 2 give the watcher a stable spec to re-sync against). Session 4 → M1.4 (closeout + v0.1 release).

### Milestone 1.1 — Walking Skeleton (shipped Session 1)

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
- [x] Docker image `pgweb/postgres:latest` (PG 17 + extension preinstalled). Local build via `scripts/build-image.sh`; registry publishing deferred to M1.4.
- [x] Hello-world proof-of-life via the image's seeded route. The "real" companion app lands in M1.3.

### Milestone 1.3 — Interactive Contracts + First Real Demo (shipped Session 2)

Goal: `examples/demo/` is a fully functional todo list with real DB interactions, not a toy. Lock the interactive-handler contracts (request JSON, return-type dispatch, 404 fallback) that M1.2's watcher will re-sync against.

- [x] Directory-as-route, filename-as-method app layout. Full spec in `docs/APP-LAYOUT.md`.
- [x] Uniform handler contract: `(req json) RETURNS <json|text>` with `req = { body, query, method, path }`.
- [x] Router dispatches on `pgweb.routes.template_path` nullability — non-NULL → Tera render, NULL → raw text.
- [x] Custom 404 fallback via `pages/_404.html` (+ optional `_404.sql`). Default body served when no user template.
- [x] Extension installs `pgweb.migrations` ledger; CLI `pg-web migrate apply` runs raw-SQL migrations in filename order. (No `migrate create` / diffing — Phase 2.5.)
- [x] Companion app at `examples/demo/` — full todo CRUD with HTMX form (create / append), toggle (outerHTML swap), delete (empty-body swap), custom 404 page.
- [x] `README.md` in `examples/demo/` + `docs/TUTORIAL.md` walking through building the same app from `pg-web init`.
- [x] Tier 3 Docker E2E test — `testcontainers` boots `pgweb/postgres:latest`, runs `migrate apply` + `push` against the demo, exercises CRUD over HTTP.
- [ ] **Deferred to M1.4:** user-facing validation UX (`check_violation` rendered inline via `hx-swap-oob` and/or a dev error page), `pgweb.html_escape()` helper. Currently empty submissions surface as a 500. Acceptable gap for Session 2's contracts-first focus.
- [ ] **Deferred to M1.2:** `public/` static asset serving (the demo ships with inline CSS for now; `public/` exists empty).

### Milestone 1.2 — Interactive Dev Loop (Session 3 next)

Goal: a developer can run `pg-web dev`, save a `.sql` file, and see the change reflected at `localhost:8080` without restarting anything. The CLI also owns stack lifecycle — users shouldn't need to think about `docker compose` directly for day-to-day work.

- [ ] CLI `pg-web up` — starts the Docker Compose stack, waits for PG + `:8080` readiness, resolves `DATABASE_URL` from `pgweb.toml`, prints the URL. Shortcut that replaces manual `docker compose up -d` + remembering the connection string.
- [ ] CLI `pg-web down` — stops the stack. `--volumes` flag drops the data volume.
- [ ] CLI `pg-web dev` — file watcher on `pages/` and `public/`. Auto-invokes `up` if stack isn't running. Auto-re-pushes on `.sql`/`.html` save. Streams container logs.
- [ ] Shift-left SQL pre-flight: parse and run in `BEGIN; ... ROLLBACK;` before applying live.
- [ ] Dynamic route patterns — `pages/posts/[id]/index.html` matches `/posts/:id` with `id` threaded into `req.path_params`.
- [ ] Dev-mode error page (SQLSTATE, MESSAGE, DETAIL, HINT, file, line, transaction state).
- [ ] Production-mode generic 500 page.
- [ ] Structured JSON logging: NOTICE/LOG capture → stdout.
- [ ] Static asset serving (BYTEA for < 1 MiB, `pg_largeobject` with streaming for ≥ 1 MiB).
- [ ] Demo enhancement: swap the inline `<style>` in `examples/demo/pages/index.html` for `public/styles.css` once static asset serving ships.

### Milestone 1.4 — Remaining Phase 1 Feature Surface (closeout)

Goal: close out Phase 1 for a releasable v0.1.

- [ ] CLI `pg-web env set KEY=VAL` / `env list` / `env unset KEY` — GUC injection for secrets.
- [ ] SQL helper `pgweb.html_escape(text) → text` shipped in the extension's install SQL for raw-text-return handlers that interpolate user content.
- [ ] User-facing validation UX: `check_violation` / `unique_violation` exceptions in handlers render inline via `hx-swap-oob` (or a framework-provided error fragment). Currently surfaces as plain 500.
- [ ] Asset serving in the demo app with a large asset (image via `pg_largeobject`).
- [ ] `pg-web push` polished for prod deploy (transaction-wrapped, migration-runner integrated).
- [ ] CLI `pg-web init --template <name>` — fetches a named example (initially `todo-demo`) from this repo's `examples/` tree and drops it into the user's directory. Mirrors Next.js's `create-next-app --example <name>` pattern. Opt-in; plain `init` stays the minimal hello-world scaffold.
- [ ] Init scaffold gets a `README.md` — small DX follow-up noted in Session 2.
- [ ] CLI `pg-web check` — offline project validator (no IDE/LSP). Walks `pages/`, `migrations/`, `pgweb.toml`; reports:
  - Layout violations (flat `.html` under `pages/`, reserved stems, missing sibling files when required).
  - Tera template parse errors (compile templates, don't render).
  - SQL parse errors (via `BEGIN; ...; ROLLBACK;` against a throwaway Postgres, or `pg_query` crate for parse-only checks — decide at implementation time).
  - Return-type mismatches (handler declared `text` but template exists, or vice versa).
  - Migration filename ordering + ledger drift against a configured DB.
  Output: grouped diagnostics with file + line, exit non-zero if any found. Intended as a pre-push safety net and CI gate.
- [ ] Release pipeline: CI builds Docker image, runs full test matrix (PG 15/16/17), publishes `pgweb/postgres:latest` + `pgweb/postgres:0.1` to Docker Hub / GHCR on tag.
- [ ] Docs pass: APP-DEVELOPER-GUIDE revised against the actual demo app; TUTORIAL.md gains a chapter covering `pg-web up` / `dev` / hot reload once M1.2 ships.
- [ ] **Browser live-reload push (WebSocket or SSE).** M1.2 ships hot-reload as file-save → DB-updated only; the browser still requires manual F5. Target UX: editor save → backend re-sync → browser auto-refresh (Vite/Next `next dev` parity). Transport choice (WS vs SSE) pending M1.2 dogfooding. **User-flagged near-term priority, explicitly deferred to M1.4 only because we want M1.2 hot-reload in production use first to see which UX friction justifies the added transport.**
- [ ] **Content-hash asset filenames + HTML rewrite.** Upgrade from the ETag-only caching shipped in M1.2 (stable `/styles.css` URLs) to fingerprinted URLs (`styles.abc123.css`) with `Cache-Control: public, max-age=31536000, immutable`. Requires a push-time transform step that rewrites asset references in templates. Matches the Vite/webpack caching model — zero round-trip on cache hit, truly immutable.

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
- *2026-04-18* — **`_404` fallback via reserved stem at route-directory root.** `pages/_404.html` (+ optional `_404.sql`) registers a fallback route with `method='404'`; router looks it up on any unmatched `(method, path)` pair. Phase 1 supports root-only (`path_pattern='/'`). Phase 2+ will extend to longest-prefix-match so per-subtree 404 pages work (`pages/admin/_404.html` → only matches `/admin/*`). Handler name: `pgweb.pages___404` — triple underscore is cosmetic but identifier-valid. Alternative designs considered: dedicated `pgweb.fallbacks` table (rejected — redundant with `routes`), `pgweb.html_escape` baked into `_404` synthesis (rejected — irrelevant for static mode). The reserved-stem approach makes fallbacks first-class routes that reuse every other piece of the dispatch pipeline.
- *2026-04-18* — **Tier 3 Docker E2E is mandatory, not opt-skip.** `scripts/test-all.sh` fails loudly when Docker or `pgweb/postgres:latest` is missing. Rationale: the image is the shipped artifact, so "silently skip" would give false-green confidence in whatever CI or contributor environment was running tests. Same philosophy as tier 2a, which requires `pg_ctl` + pgrx dev install.
- *2026-04-20* — **Dynamic route captures derived from pattern, not stored.** `pgweb.routes.path_pattern` remains the single source of truth; router parses `:name` tokens at match time to build `req.path_params`. No new schema column. Rationale: a denormalized `path_captures` column introduces drift risk (pattern and captures silently disagreeing) for zero measurable gain at Phase 1 route counts. Parse cost is a handful of segment splits per request — invisible. Decision documented inline in `router.rs`.
- *2026-04-20* — **Router match is a naïve specificity-sorted scan.** Load all routes, sort by (static-segment count desc, capture-count asc, path-length desc), pick first segment-by-segment match. Rationale: Phase 1 apps have <100 routes; ~100 × ~4 compares per request is invisible. Trie / `RegexSet` explicitly rejected as premature. **Reevaluation trigger: route count exceeds 1000 per app OR router match appears in a measured hot path.** Trigger criteria documented inline in `router.rs` next to the sort logic.
- *2026-04-20* — **File watcher stack: `notify-debouncer-full` + 200ms debounce + Blake3 content-hash dedupe + extension/dotfile filter.** Mirrors the Vite/Next/chokidar architecture: native OS watcher → write-finish debounce → content-hash skip → include/exclude filters. Debounce window (200ms) split between Vite's 100ms and Next.js's 300ms. Blake3 over SHA-256 for ~3× speed on small source files. Alternatives rejected: `notify-debouncer-mini` (lacks `await_write_finish` equivalent for rename-over-write editors); stable-state polling (more code, no win over debounce). Module-level doc in `dev.rs` cites the Vite model.
- *2026-04-20* — **Static asset caching (M1.2): ETag + `If-None-Match`.** Stable URLs (`/styles.css`), ETag = Blake3 of asset bytes, `Cache-Control: public, max-age=0, must-revalidate` in prod, `no-cache` in dev. One round-trip per asset revalidation, no body on 304. Rejected for M1.2: content-hash filenames + HTML rewrite (the Vite/webpack model — correct long-term answer; deferred to M1.4 under "Content-hash asset filenames"). Comment in the asset-serving code flags the upgrade path explicitly.
- *2026-04-20* — **Browser live-reload push deferred to M1.4 as an explicit near-term priority.** M1.2 ships hot-reload only to the backend: save → DB sync → manual F5. WebSocket or SSE push to the browser (so the page refreshes without F5) is the follow-up. Deliberately gated on M1.2 dogfooding — we want the manual-refresh flow in real use first to pick transport (WS vs SSE) from evidence rather than guess. Not a long-term deferral; user-flagged as "soon, but after testing." Documented in M1.4 bullets.
