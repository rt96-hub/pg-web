# pg-web — Roadmap

Phased delivery. Each phase must be stable, shippable, and usable on its own — no half-shipped phases. The companion app at `examples/todo/` exercises every feature of every phase.

## Feature matrix — where every planned capability lives

Source of truth for "what ships when" across the full plan. Status legend: **✅ shipped** / **🎯 next** / **⬜ planned** / **⏸ deferred**.

### Phase 1 — Synchronous Core (`v0.1.0` + `v0.2.0` polish — complete)

| Feature | Session | Status | Notes |
|---|---|---|---|
| Extension scaffolding + BGW on `:8080` | 1 (M1.1) | ✅ | `crates/pg_web_ext/`; single cdylib. |
| `CREATE EXTENSION` installs `pgweb` schema + seeds `/` | 1 (M1.1) | ✅ | `schema.rs`'s `extension_sql!` block. |
| CLI `pg-web init` | 1 (M1.1) | ✅ | Minimal scaffold, replaced by `--template` in Session 4. |
| CLI `pg-web push` (route/template/handler sync) | 1 (M1.1) | ✅ | Fully reconciling (upsert + delete); transactional. |
| Docker image `rtaylor96/pg-web:latest` | 1 (M1.1) | ✅ | Base `postgres:17`; `scripts/build-image.sh`. |
| Handler contract `(req json) RETURNS json\|text` | 2 (M1.3) | ✅ | Request-side JSON with `body`/`query`/`method`/`path`/`path_params`. |
| Directory-as-route + filename-as-method layout | 2 (M1.3) | ✅ | `paths::scan`; reserved-stem enforcement. |
| `_404.html` fallback (+ optional `_404.sql`) | 2 (M1.3) | ✅ | Default 404 body if user doesn't override. |
| Raw-SQL migrations + `pg-web migrate apply` | 2 (M1.3) | ✅ | `pgweb.migrations` ledger. |
| Demo app (`examples/todo/`) + docker E2E tier | 2 (M1.3) | ✅ | Renamed from `examples/demo/` in Session 4. |
| `docs/TUTORIAL.md` walkthrough | 2 (M1.3) | ✅ | End-state matches `examples/todo/`. |
| CLI `pg-web up` / `down` (docker compose wrappers) | 3 (M1.2) | ✅ | `DATABASE_URL` auto-resolution. |
| CLI `pg-web dev` (file watcher + hot push) | 3 (M1.2) | ✅ | notify-debouncer + Blake3 dedupe + SQL preflight. |
| Dynamic route patterns (`[id]` → `req.path_params`) | 3 (M1.2) | ✅ | Static beats capture on specificity. |
| Dev-mode typed error page (PGWEB_E001-E999) | 3 (M1.2) | ✅ | SQLSTATE + MESSAGE + DETAIL + handler name. |
| Production-mode generic 500 (no internals leaked) | 3 (M1.2) | ✅ | Env-gated via `pgweb.settings.env`. |
| Static assets from `public/*` (BYTEA + ETag) | 3 (M1.2) | ✅ | 20 MiB per-file cap (raised from 2 MiB in v0.2 / Component I); 304 on `If-None-Match`. |
| Push-time Tera template validation | 3 (M1.2) | ✅ | Parse errors caught pre-DB. |
| Port-shadowing preflight on `pg-web up` | 3 | ✅ | Catches stray pgrx dev PG on `:8080`. |
| `pgweb.html_escape(text)` SQL helper | 4 (M1.4 A) | ✅ | STRICT IMMUTABLE PARALLEL SAFE; raw-text handlers. |
| Form-validation UX (`check_violation` → inline error) | 4 (M1.4 B) | ✅ | PL/pgSQL EXCEPTION → HTMX OOB swap. |
| CLI `pg-web env set/unset/list` | 4 (M1.4 C) | ✅ | `pgweb.settings` CRUD; reserved-key guard. |
| `pgweb.setting(key)` SQL helper | 4 (M1.4 C) | ✅ | `STABLE STRICT PARALLEL SAFE`; NULL on miss. |
| CLI `pg-web init --template <name>` | 4 (M1.4 D) | ✅ | `include_dir!`-bundled; `--template todo` ships. |
| Scaffolded `README.md` on every `init` | 4 (M1.4 D) | ✅ | App-facing; distinct from repo's `examples/todo/README.md`. |
| CLI `pg-web check` (offline validator) | 4 (M1.4 E) | ✅ | Layout + Tera + SQL parse via `sqlparser`. `--url` ledger drift. |
| `pg-web push --dry-run` | 4 (M1.4 F.1) | ✅ | Rolls back instead of committing; output tagged. |
| `pg-web push --with-migrate` + pending-migration gate | 4 (M1.4 F.1) | ✅ | Apply-then-push; refuse without flag. |
| `pgweb.deployments` ops ledger | 4 (M1.4 F.1) | ✅ | One append-only row per committed push. |
| Browser live-reload via SSE + channel-aware LISTEN | 4 (M1.4 G) | ✅ | CSS cache-bust fast path; full reload fallback. |
| CI workflow (`.github/workflows/ci.yml`) | 4 (M1.4 J) | ✅ | `scripts/test-all.sh` on push + PR. |
| Release workflow (tag-driven image publish) | 4 (M1.4 J) | ✅ | Pending Docker Hub creds secret. |
| `CHANGELOG.md` + `v0.1.0` tag | 4 (M1.4 J) | ✅ | Grouped by milestone. |
| Push retry on serialization conflict | 5 | ✅ L | `retry::with_retry` wrapper + `pg_stat_activity`-based sibling-pusher diag. |
| `pg-web push --target <name>` (SSH tunnel) | 5 | ⏸ F.2 | Deferred to Session 6 — needs real remote infra to validate. Manual `ssh -L` + in-image CLI (F.3) are the supported paths today. |
| CLI bundled in `rtaylor96/pg-web:latest` | 5 | ✅ F.3 | `/usr/local/bin/pg-web` baked in builder stage; `docker exec postgres pg-web push` works. Standalone `pgweb/cli:<ver>` not shipped — bundled image proved sufficient. |
| Content-hash asset filenames + `immutable` cache | 5 | ✅ H | Push-time HTML rewrite when `[server].env = "production"`; pure-Rust string-replace; double-quoted attribute values only. |
| Larger asset cap (BYTEA 2 MiB → 20 MiB) | 5 | ✅ I (cap-raise variant) | Cap-raise without `pg_largeobject` streaming. Covers virtually every practical asset. True streaming for >20 MiB stays Phase 2+ work. |
| Single-dev guard (file lock) | 5 | ⏸ M | Skipped — L retry proved sufficient in validation. |

### Phase 2 — Auth + realtime + declarative schema

| Feature | Status | Notes |
|---|---|---|
| Cookie sessions + login/logout flow | ⬜ | Framework-provided or opt-in? Decision at Session 6 kickoff. |
| `req.session` handler field | ⬜ | Server-signed; HTTP-only cookie. |
| RLS bridge: `SET LOCAL pgweb.user_id` (GUC) + RLS policies under non-superuser serving role | ⬜ | The big Phase-2 security primitive. Execution role must be `pgweb_app` (NOSUPERUSER, NOBYPASSRLS) — see prompt 014. Superusers bypass RLS; without the floor the policies are dead code. |
| CSRF double-submit cookie on non-GET HTMX | ⬜ | Automatic; opt-out per route. |
| App-level realtime subscriptions via SSE | ⬜ | `<div hx-ext="sse" sse-connect="/_pgweb/subscribe/<ch>">` — reuses Session-4 G's channel-aware `ListenRouter`. |
| Handler-side `NOTIFY pgweb_app_<ch>` helper | ⬜ | Payload cap 8 kB; signal-then-refetch for larger. |
| Declarative schema diffing — `pg-web migrate create` | ⬜ | Phase 2.5; **approach locked 2026-04-25** — native-Rust SQL diff against `schema/*.sql` as desired state. No Prisma / DBML / migra shell-out. Implementation punted to a dedicated future session. |
| `pg-web init --template <name>` registry (beyond bundled) | ⬜ | Fetch templates from a Git URL / registry. |

### Phase 3 — Async job queue

| Feature | Status | Notes |
|---|---|---|
| `pgweb.jobs` table + `pg-web enqueue` | ⬜ | Row-level locking for worker contention. |
| Worker pool in BGW | ⬜ | Separate tokio tasks, separate SPI sessions. |
| Scheduled jobs (cron-like) | ⬜ | Leverages `pgweb.settings` for timing config. |
| Retry + dead-letter | ⬜ | Exponential backoff; permanent-failure row. |

### Phase 4 — Observability / dashboard

| Feature | Status | Notes |
|---|---|---|
| In-browser dev dashboard at `/_pgweb/admin` | ⬜ | HTMX against the BGW. Visual dev-mode page with sitemaps/route overviews, checks (incl. health/readiness from 018.1), migration overview, and other runtime state. Dev-only (env-gated like livereload). Read primarily from existing `pgweb.*` tables; does not require full production request logging or metrics (see prompt 026 and the slimmed 018 for current thinking). |
| Request log + slow-request capture | ⬜ | `pgweb.request_log` with sampling. |
| Query plans for the last N handler invocations | ⬜ | `pg_stat_statements` integration. |
| Live tail of background worker logs | ⬜ | SSE stream to admin UI. |
| `pg-web backup` / `pg-web restore` | ⬜ | Operational `pg_dump`/`pg_restore` wrapper — captures schema + user data + framework metadata in one file. The "everything in PG" thesis means one dump = whole app. |
| `pg-web export --code-only` / `pg-web import` | ⬜ | Code-only dump: `pgweb.routes` + `pgweb.templates` + `pgweb.assets` + `pgweb.pages__*` handlers + `pgweb.settings` (minus secrets). Useful for cloning a prod app's code into a dev DB without copying user data, or for source-control snapshots that survive a DB rebuild. |
| Source tree in `pgweb.sources` (file blobs + `.git/` history) | ⬜ | Push-on-commit hook mirrors working tree (and optionally git objects) into framework tables so `pg_dump` produces a *runnable-from-dump* snapshot — schema + data + app code + version history in one file. The full version of the parking-lot "project-in-database backup" idea. |

### Parking lot / explicit non-goals

| Item | Status | Notes |
|---|---|---|
| Managed-DB support (RDS / Cloud SQL / Supabase) | ⏸ | None accept custom extensions; Phase 1+ is BYO-server. |
| JavaScript build integration (Vite / esbuild) | ⏸ | Framework stays HTML-first; users can run Vite alongside if needed. |
| Declarative routes (annotations over filesystem) | ⏸ | Directory-as-route is the invariant — no second mechanism. |
| GraphQL surface | ⏸ | Out of scope forever; REST/HTML is enough. |
| ORM | ⏸ | You write SQL. That's the deal. |

---

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
- [x] Docker image `rtaylor96/pg-web:latest` (PG 17 + extension preinstalled). Local build via `scripts/build-image.sh`; registry publishing deferred to M1.4.
- [x] Hello-world proof-of-life via the image's seeded route. The "real" companion app lands in M1.3.

### Milestone 1.3 — Interactive Contracts + First Real Demo (shipped Session 2)

Goal: `examples/todo/` is a fully functional todo list with real DB interactions, not a toy. Lock the interactive-handler contracts (request JSON, return-type dispatch, 404 fallback) that M1.2's watcher will re-sync against.

- [x] Directory-as-route, filename-as-method app layout. Full spec in `docs/APP-LAYOUT.md`.
- [x] Uniform handler contract: `(req json) RETURNS <json|text>` with `req = { body, query, method, path }`.
- [x] Router dispatches on `pgweb.routes.template_path` nullability — non-NULL → Tera render, NULL → raw text.
- [x] Custom 404 fallback via `pages/_404.html` (+ optional `_404.sql`). Default body served when no user template.
- [x] Extension installs `pgweb.migrations` ledger; CLI `pg-web migrate apply` runs raw-SQL migrations in filename order. (No `migrate create` / diffing — Phase 2.5.)
- [x] Companion app at `examples/todo/` — full todo CRUD with HTMX form (create / append), toggle (outerHTML swap), delete (empty-body swap), custom 404 page.
- [x] `README.md` in `examples/todo/` + `docs/TUTORIAL.md` walking through building the same app from `pg-web init`.
- [x] Tier 3 Docker E2E test — `testcontainers` boots `rtaylor96/pg-web:latest`, runs `migrate apply` + `push` against the demo, exercises CRUD over HTTP.
- [x] **Deferred to M1.4, shipped Session 4:** user-facing validation UX (`check_violation` caught in a PL/pgSQL `EXCEPTION` block, rendered inline via `hx-swap-oob`), `pgweb.html_escape()` helper. Demo's POST `/todos` now returns an inline error fragment for empty/whitespace titles instead of a 500.
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
- [ ] Demo enhancement: swap the inline `<style>` in `examples/todo/pages/index.html` for `public/styles.css` once static asset serving ships.

### Milestone 1.4 — Remaining Phase 1 Feature Surface (closeout)

Goal: close out Phase 1 for a releasable v0.1.

- [x] CLI `pg-web env set KEY=VAL` / `env list` / `env unset KEY` + `pgweb.setting(key)` SQL helper. Values persist in `pgweb.settings` (same table as framework-synced `env`), readable from any handler via `SELECT pgweb.setting('KEY')`. Push-managed keys (`env`) rejected to prevent silent-overwrite loops. (Session 4 / Component C.)
- [x] SQL helper `pgweb.html_escape(text) → text` shipped in the extension's install SQL for raw-text-return handlers that interpolate user content. (Session 4 / Component A.)
- [x] User-facing validation UX: `check_violation` / `unique_violation` exceptions in handlers render inline via `hx-swap-oob`. Demo's POST `/todos` demonstrates the pattern; empty/whitespace-only title → 200 + inline error fragment, no 500. (Session 4 / Component B.)
- [ ] Asset serving in the demo app with a large asset (image via `pg_largeobject`).
- [x] `pg-web push` polished for prod deploy — `--dry-run` (rollback instead of commit, `[dry-run]` tagged output), `--with-migrate` (detect pending + apply-then-push in one call; refuse without flag), and the `pgweb.deployments` ledger (one append-only row per committed push: `from_host`, `file_count`, `migrations_applied`, `pushed_at`). Ops-visibility query: `SELECT * FROM pgweb.deployments ORDER BY pushed_at DESC LIMIT 5`. (Session 4 / Component F.1.) Remote-deploy pieces — F.2 SSH tunneling + F.3 CLI-in-image — deferred.
- [x] CLI `pg-web init --template <name>` — bundles `examples/<name>/` into the binary via `include_dir!` and extracts it into the user's directory on `init`. `--template demo` ships today (the HTMX todo list); adding new templates is one `include_dir!` call + one match arm. Plain `init` stays the minimal hello-world. (Session 4 / Component D.)
- [x] Init scaffold (both paths) now writes a `README.md` with quickstart commands, a pointer to `docs/APP-DEVELOPER-GUIDE.md` + `docs/TUTORIAL.md`, and the `--template todo` hint for users who want more starting material. (Session 4 / Component D.)
- [x] CLI `pg-web check` — offline project validator. Walks `pages/` + `migrations/`; flags layout violations, Tera parse errors, SQL parse errors (via pure-Rust `sqlparser` with Postgres dialect — no system build deps), and migration-prefix duplicates. Grouped diagnostics, non-zero exit on findings, zero otherwise. `--url` opt-in adds a ledger-drift pass vs `pgweb.migrations`. Return-type mismatch detection deferred to v0.2 (harder; SQL-AST walking past the CREATE FUNCTION wrapper). Ships as a pre-commit / CI gate. (Session 4 / Component E.)
- [ ] Release pipeline: CI builds Docker image, runs full test matrix (PG 15/16/17), publishes `rtaylor96/pg-web:latest` + `rtaylor96/pg-web:0.1` to Docker Hub / GHCR on tag.
- [x] Docs pass (public surface cleaned for v0.2 / open-source launch; internal material moved under `docs/internal/`).
- [x] **Browser live-reload push via SSE.** `pg-web dev` post-push hook issues `NOTIFY pgweb_livereload, '{"kind":...}'`; extension's LISTEN task forwards to connected `/_pgweb/livereload` SSE subscribers; injected `livereload.js` stub cache-busts stylesheets for `kind=css` or `location.reload()`s for anything else. Script auto-injected into HTML responses in dev mode, 404s in prod. `pg-web dev --no-livereload` opts out. **Channel-aware fan-out in `listen_router.rs` — the same infrastructure is reusable for Phase-2 app-level realtime subscriptions (one LISTEN connection, N browser SSE tabs, pure in-memory broadcast).** Costs +1 Postgres backend slot in dev, 0 in prod. (Session 4 / Component G.)
- [ ] **Content-hash asset filenames + HTML rewrite.** Upgrade from the ETag-only caching shipped in M1.2 (stable `/styles.css` URLs) to fingerprinted URLs (`styles.abc123.css`) with `Cache-Control: public, max-age=31536000, immutable`. Requires a push-time transform step that rewrites asset references in templates. Matches the Vite/webpack caching model — zero round-trip on cache hit, truly immutable.

### Known Phase 1 limitations (deliberately deferred)

- SQL handlers that call external APIs (Stripe, etc.) will block the HTTP worker thread until the API returns. **Fixed in Phase 3** via the async job queue.
- No declarative schema-diffing (`pg-web migrate create`). Users hand-write `migrations/NNNN_name.sql`. **Schema-diffing (from Prisma / DBML / DB introspection) is punted to a later phase.**
- No auth, no sessions, no RLS bridge. **Delivered in Phase 2**.
- No in-browser debugger dashboard. **Delivered in Phase 4**.
- Browser live-reload (SSE + EventSource) has client-side bfcache/navigation cleanup and a server-side connection lifetime cap, but lacks automated tests that simulate rapid page navigation + bfcache restores and verify that SSE connections / broadcast subscribers do not accumulate. Full simulation test coverage is deferred.

## Phase 2 — Security & Identity

**Goal:** pg-web can safely run a multi-user app.

### Deliverables

- [ ] Native cookie-based session management. Framework-provided SQL helpers:
  - `pgweb.session_create(user_id)` → returns signed cookie value
  - `pgweb.session_validate(cookie)` → returns user_id or null
- [ ] Rust worker: on each request, read `Cookie` header, call `session_validate` via SPI, set `pgweb.user_id` for the transaction via `SET LOCAL`.
- [ ] **RLS Bridge:** documented pattern for user tables (requires the 014 privilege floor — serving role `pgweb_app` must be NOSUPERUSER NOBYPASSRLS; superusers ignore policies):
  ```sql
  CREATE POLICY tenant_isolation ON posts
    USING (author_id = current_setting('pgweb.user_id', true)::bigint);
  ```
  The GUC + non-superuser serving role approach is the settled design (prompt 014 resolved earlier contradictory "SET LOCAL ROLE from session" wording elsewhere in this document). Per-user DB roles do not scale and are still unsafe without the floor.
- [ ] CSRF protection (double-submit cookie pattern, automatic on HTMX non-GET requests).
- [ ] Password hashing helpers using `pgcrypto` (`crypt` + `gen_salt('bf', 12)`).
- [ ] `pg-web init --with-auth` template variant.
- [ ] Companion app: extend the todo list with signup, login, per-user rows backed by RLS.

### Open questions (resolve before starting Phase 2)

- Session cookie format: opaque (DB-stored) vs signed-JWT-like (stateless)?
- Secret key rotation strategy.
- OAuth provider integration — in-scope for Phase 2 or defer?

## Phase 2.5 — Schema Tooling (floating between Phase 2 and 3)

**Goal:** `pg-web migrate create` generates SQL migrations from a declarative schema source. Deferred from Phase 1 to avoid front-loading complexity. **Approach decided 2026-04-25 (post-v0.2); implementation punted.**

### Decision: native-Rust SQL diff against `schema/*.sql`

- **Source of truth is plain Postgres SQL DDL** in a `schema/` directory next to `migrations/`. Users write `schema/01_users.sql`, `schema/02_todos.sql`, etc. as the *desired* end-state schema (`CREATE TABLE`, indexes, constraints, etc.).
- `pg-web migrate create [name]` runs the desired schema and the current schema (post-applied-migrations) against two ephemeral PG instances (testcontainers or schemas-in-the-dev-PG), reads `pg_catalog`, and emits the delta as `migrations/NNNN_<name>.sql`.
- Diff engine is **native Rust** — walks `pg_catalog.pg_class`, `pg_attribute`, `pg_index`, `pg_constraint` between the two states. No `migra` shell-out, no Prisma parser, no DBML.

### Why not Prisma / DBML

- **Tonal mismatch.** pg-web's pitch is "you write SQL." Bolting Prisma on top is a different framework's vibe — same reason there's no ORM.
- **Mapping layer is its own spec.** Prisma's `@id` / `@@index` / `@@unique` syntax maps imperfectly to Postgres-specific features (partial indexes, CHECK constraints, generated columns, exclusion constraints, foreign-data wrappers). Every Postgres feature would need a Prisma-side annotation, and we'd be inventing a parallel schema language to cover the gaps.
- **Existing `prisma-schema` Rust crate is unmaintained.** Rolling our own parser to stay current with Prisma's spec is ongoing cost for a feature we'd rather not have shipped.
- **DBML has the same drawbacks** in a smaller community — same mapping-layer problem, less ecosystem support, no win.

### Why native Rust over `migra`

`migra` (the Python tool) does well-known schema-diff work, but shelling to it adds a Python runtime dep to a "one Rust binary" CLI. Native Rust diff is one-time work and keeps `cargo install pg-web` friction-free. `migra`'s algorithm is well-documented; reimplementing in Rust is straightforward — walk `pg_catalog`, compare, emit ALTER. ~1 week of focused work.

### Implementation status: punted

Locked the approach in Session 5; **no implementation this cycle**. The diff engine is the heaviest single feature on the roadmap below Phase 2 and deserves its own dedicated session. Picks back up after Phase 2 (auth/RLS/realtime) ships, OR earlier if schema-write fatigue starts hurting before then.

## Phase 3 — Async & Scale

**Goal:** pg-web survives real-world traffic and external API blocking.

### Deliverables

- [ ] Second pgrx background worker dedicated to job queues.
- [ ] `pgweb.jobs` table + state machine (pending / running / succeeded / failed / retrying).
- [ ] SQL API: `pgweb.enqueue(job_type, payload, run_at?)`.
- [ ] Async job runner: polls queue, dispatches to registered handlers (HTTP, email, generic).
- [ ] Built-in handlers: HTTP request (via `reqwest`), email (SMTP via `lettre`).
- [ ] **Internal concurrency management:** HTTP-level queue inside the web worker's Tokio runtime. Traffic spikes absorbed at the web tier before opening SPI transactions — prevents Postgres connection exhaustion.
- [x] Health & readiness endpoints (protected `/_pgweb/*` probes + overridable public `/health` + `/readiness` defaults + disable flags). Shipped in 018.1 (see that prompt + schema seeds + http mounts + router suppression + Dockerfile HEALTHCHECK update). The protected probes are the ones operators and the image should use; public ones are the conventional overridable surface for app-specific checks. (Metrics remain future.)
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
- [ ] **Operational backup CLI: `pg-web backup [--out FILE]` / `pg-web restore FILE`.** Thin wrappers over `pg_dump --format=custom` and `pg_restore`. Reads connection target from `pgweb.toml` (same target resolution as `push --target`). The "everything in PG" thesis means one dump captures schema + user data + framework metadata + assets — and `restore` reboots into a working app. Document the `pg_dump`+cron recipe in `docs/DEPLOYMENT.md` for users who want vanilla PG ops; the CLI is the convenience layer on top.
- [ ] **Code-only export/import: `pg-web export --code-only [--out FILE]` / `pg-web import FILE`.** Dumps just `pgweb.routes`, `pgweb.templates`, `pgweb.assets` (sans `assets_large`?), `pgweb.pages__*` handler functions, and `pgweb.settings` (minus reserved-key `env`, optionally minus user-defined secrets via `--include-secrets`). Output is portable SQL or a `.pgweb` archive. Used for: cloning prod app code into a dev DB, snapshotting a `git tag`-stable view of the live app, restoring code after a DB-data wipe. **Distinct from operational backup** — code-only is for transferring shape between DBs that have their own user data; operational backup is for restoring the whole DB.
- [ ] **Source-tree-in-DB (`pgweb.sources` schema).** The full version of the parking-lot "project-in-database backup" idea. New schema with `files (path PK, content bytea, mode, mtime)` and optionally `git_objects (sha PK, type, content)` + `git_refs (name PK, target_sha)` so the user's `.git/` is dumpable too. Mirrored automatically by a `pg-web push` extension that ALSO pushes file blobs (or by a separate `pg-web sync-source` to keep push fast). `pg_dump` then produces a single file containing: schema + user data + framework metadata + app source + git history → `pg_restore` into a fresh PG and the project is fully runnable + auditable. Open questions before scoping: storage cost (a busy `.git/` is many GBs — store working tree only, or pack objects efficiently?), how to keep `pgweb.sources` in sync without slowing the dev loop (lazy push? git-hook-driven? manual `sync-source`?), and whether this is a Phase-4 feature or earns its own phase.

## Parking lot — post-v1 ideas

Speculative. Not yet scoped into a phase; parked here so the thinking isn't lost.

- **Project-in-database backup.** *Promoted to Phase 4 deliverable as `pg-web backup` (operational), `pg-web export --code-only` (code-only), and `pgweb.sources` schema (source-tree-in-DB).* See Phase 4 for the split rationale. The original framing — "you can hand someone a `.dump` file and they have the whole system" — survives in the source-tree-in-DB variant; the operational + code-only commands are the smaller, ship-sooner pieces.
- **Drop-in SSH deploy sidecar for the compose stack.** M1.4's F.2 assumes the VPS already has an SSH account that can reach Postgres on `127.0.0.1:5432` — which means SSH config lives on the host, not the project. The stretch-goal variant: ship an `sshd` sidecar container in the scaffolded `docker-compose.yml`, configured from an `authorized_keys` file that the user drops next to their compose file. The sidecar grants only a forced-command shell that invokes `pg-web push` (reading app files from a bind mount) and Postgres on the internal compose network — so the "deploy credential" lives inside the project's compose config instead of on the host. Lets you `scp` a compose file onto a VPS + `docker compose up` and the deploy story is self-contained. Open questions before scoping: how to keep the sshd sidecar small (Alpine + OpenSSH? `linuxserver/openssh-server`?), how to layer authorized_keys without baking them into the image, how to expose the sshd port without conflicting with the host's `:22` (use 2022 or similar), whether this opens a meaningful attack surface vs. using the host's sshd. **User-flagged as stretch goal after M1.4.** Likely Phase 5+ or whenever a deployment story gets painful enough that the value outweighs the operational novelty.
- **MCP + skills for framework documentation (agents writing pg-web code).** Long-term bet: the highest-leverage thing we can do for AI agents is give them *god-tier, always-current access to the actual documentation and invariants* while they are writing pg-web apps.

  Core idea: an MCP server (and/or packaged skills for the major agent marketplaces) that exposes the full body of pg-web knowledge — `CLAUDE.md`, every file under `docs/`, the error catalog, `APP-LAYOUT.md` rules, testing strategy, architecture decisions, VISION, etc. — as first-class resources and tools. An agent building a pg-web app should be able to ask "what's the exact handler contract for dynamic routes?" or "show me the current RLS bridge pattern" and get the authoritative text plus examples, without hallucinating or relying on stale training data.

  This is the evolution of the older "LLM-native knowledge base + agent skill" parking-lot item. Instead of (or in addition to) a `pg-web help <CODE>` CLI command and static markdown, we ship a proper MCP surface + skills that agent platforms can consume directly (Claude Desktop, Cursor, marketplace skills, etc.).

  This pairs naturally with the agent reporting idea below: excellent docs access for agents while they work → they discover a real bug or a legitimate improvement opportunity → they can file a proper report (with rich context) to the shared board. Nice closed loop between documentation access and feedback.

  Related but distinct longer-term direction (from earlier discussion): runtime data access for agents. Things like `pg-web query` / `pg-web psql` (thin wrappers that reuse the existing DATABASE_URL resolution) and eventually a data-oriented MCP server so agents can inspect the actual tables inside a running pg-web app. This is valuable but further out than the documentation surface.

  Open questions (very speculative, no hurry):
  - Packaging: dedicated `pg-web-mcp` binary? Built into the main CLI? Pure docs-as-resources vs. also having active tools (search across docs, "explain this invariant", "generate a compliant route from a description")?
  - How much of the knowledge lives in the MCP resources vs. being embedded in a living `.claude/skills/pg-web.md` (or equivalent for other platforms) that we keep in sync with the real docs.
  - Marketplace / distribution story — how do we make it trivial for someone using Claude or Cursor to just "add the pg-web docs MCP" for any project they're working on?

  This stays in the parking lot until the core framework is more mature and we've seen real agents struggle (or succeed) with the current docs. No near-term scheduling.
- **Agent bug & improvement reporting → shared board.** When an AI agent is working inside a pg-web project (or on the framework itself), it should be able to report real bugs and improvement ideas back to the maintainers, similar to how a human would file a GitHub issue. The agent would call a reporting tool (e.g. a generalized `pg-web report`, or an MCP tool like `report_bug` / `suggest_improvement`) and provide rich automatic context: what it was trying to accomplish, which documentation it consulted, relevant code/files, error details, stack traces, etc.

  These reports surface on a visible board — conceptually like GitHub Issues, but with a clear `agent-reported` (or `from-agent`) label / view so the team can easily see everything that came from agents. The destination could be actual GitHub Issues, Linear, a dedicated board, or something simpler. The key is that agent-reported bugs and ideas are captured in the same general place the maintainers already look, rather than disappearing into individual chat histories.

  This is the "feedback" half of the agent ergonomics story and pairs naturally with the documentation MCP above: agent has excellent access to the real invariants and docs while coding → it hits a real bug or sees a genuine improvement opportunity → it can file a proper report with context.

  (Note from the user: this entire concept is still a very early, barely-formed idea — just captured here on the roadmap so it doesn't get lost.)
- **App testing framework (`pg-web test`).** A command for users of pg-web to author and run tests against their own app. Three candidate layers to evaluate:
  - **Handler unit tests.** `tests/pages/**/*.test.sql` files call `pgweb.pages__<name>('{"body":{...},"query":{...},"method":"POST","path":"/x"}'::json)` and use framework-provided `pgweb.assert_*` helpers to check the return value. Per-test isolation via savepoint-and-rollback so fixtures aren't rebuilt every time.
  - **HTTP integration tests.** YAML / TOML fixtures describe a request plus expected response (status + body regex / template). `pg-web test` spins up a throwaway stack (or reuses `dev`'s), fires requests, diffs.
  - **Snapshot tests.** Capture rendered HTML on first run, compare on subsequent runs; update with `--update-snapshots`. Works for both handler-level and HTTP-level rendering.
  - **Fixture loading.** `tests/fixtures/*.sql` applied before a test (or per-describe-block) and rolled back after.
  Open questions before scoping: run-in-container vs run-in-host (latter needs a pg-web CLI that owns a test DB), per-test isolation model (SAVEPOINT vs fresh DB), snapshot policy (where to store, how to diff HTML robustly). **Likely Phase 5+ once the core framework has stabilized and real apps have shaped the mental model of what's worth asserting.** Parked here so the thinking isn't lost — user-flagged as a meaningfully-later feature.

## Out of scope (for v1.x; revisit post-1.0)

- Managed-DB compatibility (RDS, Cloud SQL, Supabase). Fundamentally requires upstream vendor cooperation to allow custom extensions.
- Non-HTMX frontends (React/Vue/Svelte). Deliberately an HTMX-first framework.
- TLS termination inside the extension. Always via Caddy/Nginx/Traefik.
- Multi-database support (MySQL, SQLite). Postgres-only.
- GraphQL. Over HTTP JSON is fine if someone wants to build it on top; the framework won't ship with it.
- Server-sent events / WebSockets. May revisit if HTMX 2.x SSE support stabilizes.

## Decision log

Track architectural decisions here as they solidify. Each entry: date, decision, rationale, alternatives considered.

- *2026-06-11* — **Execution-role hardening + per-request timeout + threat model (prompt 014)**: Dedicated non-superuser `pgweb_app` role (NOSUPERUSER, NOBYPASSRLS) connected at worker startup via `connect_worker_to_spi(..., Some("pgweb_app"))` (A1). `SET LOCAL statement_timeout` (configurable, default 15s) early in every request tx. `pgweb.secrets` + `SECURITY DEFINER pgweb.secret(key)` for credential isolation. `env list` now masks by default (`--show-values`). `docs/THREAT-MODEL.md` written from the seed asset/actor/attack table. Reconciled ARCHITECTURE (secrets) and ROADMAP (RLS bridge now explicitly "GUC under non-superuser serving role"; dropped the "SET LOCAL ROLE from session" wording as both unscalable and unsafe without the floor). This is the hard prerequisite that makes Phase 2 RLS enforceable. Touched schema (role+grants+secrets+fn+seed), worker (connect), router (timeout SET + 57014 error variant), errors, settings, push (toml sync + summary), CLI env list, docs, and `examples/todo/` + test coverage.
- *2026-06-11* — **Response contract v2 (prompt 013)**: handlers may return a `"$pgweb"` envelope (detected in router) carrying status/headers/cookies/content_type plus either a literal body or a Tera context. Four SQL helpers (`pgweb.respond`/`redirect`/`json`/`set_cookie`) are the author surface. Backward compatible: no marker = identical behavior for all existing bare `json`/`text` returns. CLI relaxes raw-text routes to accept `RETURNS json` (runtime disambiguation). Header policy is a denylist (hop-by-hop + CL/CT/TE). Cookie defaults: HttpOnly+SameSite=Lax, Secure=env=production only. Explicit content types only (no Accept negotiation in v1). This is the keystone enabling Phase 2 auth (Set-Cookie + redirects), the documented JSON API use case, and prompt 005. Derived purely from return value (no new routes column), consistent with 2026-04-18/20 decisions. Implementation touched http/router/schema (ext) + push validate (CLI) + docs + `examples/todo/` companion routes + tests.
- *2026-04-26* — **Direction: MCP + skills focused on framework documentation for agents writing pg-web code** (plus related longer-term runtime data access tools). Rationale: the biggest multiplier for AI agents building on pg-web is giving them first-class, always-fresh access to the real docs, invariants, error catalog, and architecture decisions while they edit code. A documentation-oriented MCP (and/or marketplace skills) is the natural evolution of the existing "LLM-native knowledge base + agent skill" idea. Runtime data access (`query`/`psql` wrappers + eventual data MCP) is a related but separate desire, also parked for much later. Both stay speculative until the core framework and docs have stabilized.
- *2026-04-17* — **pgrx 0.18.0 pinned**. Latest stable at project start; supports PG 15/16/17.
- *2026-04-17* — **Dual MIT/Apache-2.0 license**. Rust-ecosystem default; permissive enough for enterprise adoption.
- *2026-04-17* — **WSL2 Ubuntu 22.04 for maintainer dev**. Native Windows pgrx support exists but is painful; WSL is the paved path.
- *2026-04-17* — **Schema diffing (Prisma/DBML-based `migrate create`) deferred out of Phase 1.** Phase 1 ships raw-SQL migrations only. Rationale: complexity doesn't buy us anything for the walking skeleton; we can see what pain is real after Phase 1 lands.
- *2026-04-17* — **Milestone 1.1 walking skeleton includes CLI + Docker Compose,** not just the extension. Rationale: user experience loop must work end-to-end from day one; an extension without the CLI scaffolding is not a validated framework.
- *2026-04-17* — **First "real" companion app = todo list** (Milestone 1.3). The walking-skeleton hello-world at 1.1 is not the demo app — it's just proof-of-life. The todo list exercises migrations, HTMX forms, validation, and static assets honestly.
- *2026-04-17* — **Axum chosen as HTTP library**, used as a thin shell. Rationale: pg-web's "routing lives in the DB" model maps cleanly to Axum's fallback-handler pattern; Tower middleware simplifies per-request SPI transaction wrapping + request-ID tracing; Axum doesn't hide Hyper, so dropping to raw Hyper later stays cheap. Hyper-raw was considered (for tighter control / fewer abstractions) but rejected for the cost of rebuilding URL/header/query parsing for a small HTTP surface. Actix-web was considered and rejected over governance concerns and weaker Tower composability.
- *2026-04-17* — **Framework schema named `pgweb`** (no underscore), tables `pgweb.routes` / `pgweb.templates` / `pgweb.assets_*` etc. Rationale: Postgres reserves schema names starting with `pg_` for system schemas (`CREATE SCHEMA pg_web` returns `SQLSTATE 42939 reserved_name`). Underscore-prefixed alternatives like `_pg_web` still trip up convention. `pgweb` reads cleanly, matches the Docker Hub namespace (`rtaylor96/pg-web`), and avoids all reserved-name collisions. Table names inside the schema don't need a double prefix — schema name already scopes them.
- *2026-04-17* — **Dedicated WSL user `pgweb` (uid 1001) for development,** not root. Rationale: Postgres's `initdb` refuses to run as root, breaking `cargo pgrx test` / `cargo pgrx run`. `/home/pgweb/pg-web` is the project path; `/home/pgweb/.pgrx/` holds local PG installs.
- *2026-04-18* — **App layout: directory = route, filename = method.** Each directory under `pages/` is a URL route; `index.html`/`index.sql` = GET, `post.html`/`post.sql` = POST. Either file is optional — `.html` alone = static, `.sql` alone = raw-text handler, both = JSON→Tera pipeline. Flat `pages/about.html` no longer valid. Full spec in `docs/APP-LAYOUT.md`. Rationale: one mental model for pages, API endpoints, and HTMX fragments — simpler than Next.js (which splits page.tsx vs route.ts) and SvelteKit (which uses `+page.server.ts` actions). Canonical DX for our HTMX-first target.
- *2026-04-18* — **Handler signature: single `json` arg, shape `{ body, query, method, path }`.** Every `.sql` handler is `pgweb.pages__<name>(req json) RETURNS <json|text>`. `body` and `query` always objects (never null) — `req->'body'->>'key'` always safe. Uniform signature keeps the router code path singular and leaves room to grow (`path_params`, `session`, `headers`) without re-signing every handler.
- *2026-04-18* — **POST return contract: dispatch via `template_path` nullability.** If `.html` sibling exists → CLI writes `template_path` → router expects JSON return + Tera render. If only `.sql` → `template_path` NULL → router expects text return + bytes-as-is. No new schema column, no per-route flag; filesystem is source of truth. Alternatives (per-route `skip_template` bool, `pg_proc.prorettype` lookup each request) rejected as either redundant with filesystem state or a per-request performance cost.
- *2026-04-18* — **CLI owns the full dev loop; Docker should be invisible day-to-day.** The target UX is `cargo install pg-web` → `pg-web init` → `pg-web up` → `pg-web dev`, with the CLI managing compose, pulling the published image on first run, and auto-resolving `DATABASE_URL`. Scoped to M1.2 (`up`/`down`/`dev`) + M1.4 (published image + `init --template`). Rationale: lowering the install surface matters as much as the runtime model; if users have to think about `docker compose up -d` and connection strings every session, the "Postgres is your whole stack" pitch gets tax-heavy. Mirrors `next dev` / `rails server` — one command, stack handled.
- *2026-04-18* — **`_404` fallback via reserved stem at route-directory root.** `pages/_404.html` (+ optional `_404.sql`) registers a fallback route with `method='404'`; router looks it up on any unmatched `(method, path)` pair. Phase 1 supports root-only (`path_pattern='/'`). Phase 2+ will extend to longest-prefix-match so per-subtree 404 pages work (`pages/admin/_404.html` → only matches `/admin/*`). Handler name: `pgweb.pages___404` — triple underscore is cosmetic but identifier-valid. Alternative designs considered: dedicated `pgweb.fallbacks` table (rejected — redundant with `routes`), `pgweb.html_escape` baked into `_404` synthesis (rejected — irrelevant for static mode). The reserved-stem approach makes fallbacks first-class routes that reuse every other piece of the dispatch pipeline.
- *2026-04-18* — **Tier 3 Docker E2E is mandatory, not opt-skip.** `scripts/test-all.sh` fails loudly when Docker or `rtaylor96/pg-web:latest` is missing. Rationale: the image is the shipped artifact, so "silently skip" would give false-green confidence in whatever CI or contributor environment was running tests. Same philosophy as tier 2a, which requires `pg_ctl` + pgrx dev install.
- *2026-04-20* — **Dynamic route captures derived from pattern, not stored.** `pgweb.routes.path_pattern` remains the single source of truth; router parses `:name` tokens at match time to build `req.path_params`. No new schema column. Rationale: a denormalized `path_captures` column introduces drift risk (pattern and captures silently disagreeing) for zero measurable gain at Phase 1 route counts. Parse cost is a handful of segment splits per request — invisible. Decision documented inline in `router.rs`.
- *2026-04-20* — **Router match is a naïve specificity-sorted scan.** Load all routes, sort by (static-segment count desc, capture-count asc, path-length desc), pick first segment-by-segment match. Rationale: Phase 1 apps have <100 routes; ~100 × ~4 compares per request is invisible. Trie / `RegexSet` explicitly rejected as premature. **Reevaluation trigger: route count exceeds 1000 per app OR router match appears in a measured hot path.** Trigger criteria documented inline in `router.rs` next to the sort logic.
- *2026-04-20* — **File watcher stack: `notify-debouncer-full` + 200ms debounce + Blake3 content-hash dedupe + extension/dotfile filter.** Mirrors the Vite/Next/chokidar architecture: native OS watcher → write-finish debounce → content-hash skip → include/exclude filters. Debounce window (200ms) split between Vite's 100ms and Next.js's 300ms. Blake3 over SHA-256 for ~3× speed on small source files. Alternatives rejected: `notify-debouncer-mini` (lacks `await_write_finish` equivalent for rename-over-write editors); stable-state polling (more code, no win over debounce). Module-level doc in `dev.rs` cites the Vite model.
- *2026-04-20* — **Static asset caching (M1.2): ETag + `If-None-Match`.** Stable URLs (`/styles.css`), ETag = Blake3 of asset bytes, `Cache-Control: public, max-age=0, must-revalidate` in prod, `no-cache` in dev. One round-trip per asset revalidation, no body on 304. Rejected for M1.2: content-hash filenames + HTML rewrite (the Vite/webpack model — correct long-term answer; deferred to M1.4 under "Content-hash asset filenames"). Comment in the asset-serving code flags the upgrade path explicitly.
- *2026-04-20* — **Browser live-reload push deferred to M1.4 as an explicit near-term priority.** M1.2 ships hot-reload only to the backend: save → DB sync → manual F5. WebSocket or SSE push to the browser (so the page refreshes without F5) is the follow-up. Deliberately gated on M1.2 dogfooding — we want the manual-refresh flow in real use first to pick transport (WS vs SSE) from evidence rather than guess. Not a long-term deferral; user-flagged as "soon, but after testing." Documented in M1.4 bullets.
- *2026-06-12* — **Postgres support gate = the bundled image major (currently PG 17).** pg-web distributes Postgres itself via the runtime image (`postgres:17-bookworm`), so end users never choose a Postgres version — we do. Multi-major support (15/16) is no longer a feature gate: it forced worst-common-denominator designs (concretely: prompt 014's serving role couldn't rely on PG17's `BGWORKER_BYPASS_ROLELOGINCHECK` — the flag exists only in the pg17+ bindings — and was drifting toward a weaker LOGIN-role compromise just to keep older majors viable). The `pg15`/`pg16` cargo features stay around and should keep compiling while that's cheap, but correctness, tests, and validation target the bundled major only; tier 1 CI needs a single major. Removing the legacy feature flags entirely is a future cleanup decision. CLAUDE.md invariant #6 updated in the same change.
