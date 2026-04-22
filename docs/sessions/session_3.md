# Session 3 — Interactive Dev Loop (M1.2)

**Status:** planned, not started.
**Theme:** turn pg-web from "type two commands after every edit" into a file-watcher-driven dev loop. The CLI also takes over stack lifecycle so developers stop thinking about `docker compose` directly. Dynamic routes + dev error page round out the DX.

By the end of this session, the daily loop is:

```bash
pg-web up           # boots the stack, prints URL, auto-resolves DATABASE_URL
pg-web dev          # watches pages/ and public/; auto-pushes on save; streams logs
# edit .sql/.html; refresh browser; see change
pg-web down         # stops the stack (or leave it; `up` is idempotent)
```

---

## State of the project at Session 3 start

### What's working today

- Extension installs via `CREATE EXTENSION pg_web_ext`. Framework schema has three tables: `pgweb.routes`, `pgweb.templates`, `pgweb.migrations`.
- HTTP server binds `:8080` from a background worker. Axum thin-shell over our own router.
- Every request = one SPI transaction. Handler contract: `pgweb.pages__<name>(req json) RETURNS <json|text>` with `req = { body, query, method, path }`.
- Dispatch: `template_path` non-NULL → Tera render, NULL → raw-text passthrough. Decided by whether the `.sql` has a sibling `.html`.
- Fallback `_404` route supported (root scope only in Phase 1). Served at HTTP 404 status with custom body, or a hardcoded minimal 404 when no user template.
- CLI: `init <name>` / `migrate apply --url` / `push --url`.
- Docker image `pgweb/postgres:latest` builds from `scripts/build-image.sh`.
- Companion app `examples/demo/` — full HTMX todo CRUD with create / toggle / delete + custom 404.
- Four test tiers, 58 tests, all green via `scripts/test-all.sh`.

### What a developer has to type right now

```bash
pg-web init my-app
cd my-app
docker compose up -d                                                       # manual
export DATABASE_URL="postgres://postgres:devpassword@localhost:5432/app"   # manual
pg-web migrate apply --url "$DATABASE_URL"                                 # after every migration edit
pg-web push --url "$DATABASE_URL"                                          # after every pages/ edit
# edit file
pg-web push --url "$DATABASE_URL"                                          # again
```

Every edit to `pages/` requires a push. Every new migration requires `migrate apply`. The developer holds the `docker compose` lifecycle and the connection string in their head. That's the friction this session removes.

### Invariants that stay put

Session 2 locked these; Session 3 MUST NOT revisit them:

1. Directory-as-route, filename-as-method layout (`docs/APP-LAYOUT.md`).
2. Handler contract `(req json) RETURNS <json|text>`.
3. Dispatch via `template_path` nullability.
4. Extension ↔ CLI talk only via framework-table upserts — no shared crate, no RPC.
5. One HTTP request = one SPI transaction.
6. Target PG 15/16/17; async only inside the BGW; HTTPS out-of-process.

If any of these look load-bearing for M1.2 work, raise a flag before proceeding.

### Known limitations entering this session

- No hot reload — every change is a manual push.
- No dynamic routes — `[id]` captures don't exist.
- Fatal SQL errors return a plain 500; no detail surfaced.
- `public/` exists but isn't served — static assets 404.
- Empty form submissions surface as HTTP 500 (CHECK constraint violation, no user-facing validation UX yet).
- Docker image not published to any registry; users build locally.
- Handler names don't support `req.path_params` (no dynamic routes to populate it from).

### Entry point for this session

Suggested first task: read this file, then read `docs/ROADMAP.md` § Milestone 1.2. If the design questions section below has open items after that, resolve them with the user. Then begin Component A (`pg-web up` / `pg-web down`) — smallest piece, no dependency on later ones.

---

## Prerequisites (shipped in Session 1 + 2)

- Extension serves HTTP with `(req json)` handler contract + template-path dispatch ✅
- CLI `init` / `push` / `migrate apply` ✅
- Docker image + examples/demo app + Docker E2E ✅
- Locked layout spec in `docs/APP-LAYOUT.md` ✅

The spec settled in Session 2 is the contract the watcher re-syncs against. Nothing in this session should need to revise it.

---

## Work breakdown

### A. CLI stack lifecycle — `pg-web up` / `pg-web down`

**New module:** `crates/pg_web_cli/src/stack.rs`.

- `pg-web up [--dir .]` — discover `docker-compose.yml` in `--dir`, shell out to `docker compose up -d`, then poll `:8080` (and `:5432`) until both respond. Print the resolved `DATABASE_URL` so users can copy it. `--detach` default; `--foreground` tails container logs.
- `pg-web down [--dir .] [--volumes]` — `docker compose down`, optional `--volumes` to drop `pgdata`.
- Preflight: check `docker --version` succeeds; bail with install hint otherwise.
- Exit codes: 0 on success, non-zero with clear stderr otherwise.

**Tests:**
- Unit: port-poll helper (pure, deadline-based).
- Hermetic: `up` with missing `docker-compose.yml` → clear error.
- No integration tests that actually boot Docker here — tier 3 E2E in `crates/pg_web_cli/tests/docker_e2e.rs` already covers the full flow; this is a thin wrapper.

### B. CLI `pg-web dev` — file watcher + auto-push

**New module:** `crates/pg_web_cli/src/dev.rs`. Uses `notify` crate for cross-platform file watching.

- `pg-web dev` — ensure `up` (no-op if already running), connect to DB, watch `pages/` and `public/`.
- On any `.sql` or `.html` save: re-run a targeted push for the affected file.
- On `.sql` save: shift-left pre-flight — wrap the file in `BEGIN; <contents>; ROLLBACK;` via the live connection before the real `CREATE OR REPLACE FUNCTION` goes in. If the rollback'd version errored, print the Postgres error and don't commit the real version. The live route keeps working until the developer fixes it.
- On non-recognized files (readmes, dotfiles): no-op, no spam.
- Tail container logs in-band (optional flag `--no-logs`).
- Ctrl-C: clean shutdown.

**Tests:**
- Unit: file-event → action classifier (save `.sql` → push, save `.gitignore` → ignore, etc.).
- Hermetic: dispatch a synthetic event, assert on side effects.
- Full behavior deferred to tier 3 — `docker_e2e.rs` gains a "watcher sees a save and re-pushes" test (starts `dev` in a thread, writes a file, polls for the new content at the HTTP endpoint).

### C. Dynamic route patterns — `[id]` captures

**Spec update:** `docs/APP-LAYOUT.md` gains a "Dynamic segments" section.

- `pages/posts/[id]/index.html` (+ `.sql`) → matches `GET /posts/:id`. The `id` segment from the URL is threaded into `req.path_params` as `{ "id": "42" }`.
- Multiple captures allowed: `pages/users/[user]/posts/[post]/index.html` → `/users/:user/posts/:post`.
- Path-param values are always strings; handlers cast as needed.
- Reserved: `[` and `]` in directory names are the capture markers. Literal brackets in URLs aren't supported.

**Implementation:**
- `paths.rs::scan` recognizes `[name]` directory segments and emits a pattern-form `path_pattern` (e.g., `/posts/:id`) along with a list of capture names.
- `pgweb.routes.path_pattern` stores the pattern (`/posts/:id`, not `/posts/42`). Add a `path_captures` column (TEXT[] or jsonb) listing capture names in order.
- Router match changes: instead of exact-match `path_pattern = $1`, do longest-prefix-match among templated patterns. Simple implementation for Phase 1: store patterns, iterate in rank order (static > single-capture > multi-capture), match each against the request path via a regex or manual segment-by-segment compare.
- Extract capture values → `req.path_params`.

**Tests:**
- `paths.rs` unit: `[id]` segment recognition, reserved-character handling.
- `#[pg_test]`: insert a dynamic route, look it up with a concrete path, verify captures.
- HTTP smoke: dynamic route + capture surfaces through `req.path_params`.
- Demo extension: add `pages/todos/[id]/index.html` → todo detail view.

### D. Dev error page

When the extension is running in `env = "development"` mode, a fatal SQL exception returns a styled error page instead of generic 500:

- SQLSTATE + message + DETAIL + HINT + CONTEXT (from the PG error)
- File + line of the failing handler (resolved via `pgweb.routes` lookup on handler_name)
- The `req` JSON that triggered the failure
- A stacktrace-ish view of SPI calls in the request

Production mode keeps the current generic 500. Mode selection via `pgweb.env` GUC (defaults to "development" in dev, "production" via `pgweb.toml`).

**Tests:** `#[pg_test]` for the error-page renderer. HTTP smoke test: induce a SQL error (e.g., hit a route pointing at a nonexistent handler), assert on the dev page content.

### E. Static asset serving

Two tiers:

- **Small (< 1 MiB):** `BYTEA` column in `pgweb.assets_small(path PK, content, content_type, etag)`. Served with `Cache-Control: public, max-age=31536000, immutable` when content-hashed.
- **Large (≥ 1 MiB):** `pg_largeobject` OID in `pgweb.assets_large(path PK, oid, content_type)`. Streamed via SPI `lo_read` with a bounded read buffer so memory stays flat regardless of file size.

CLI `pg-web push` walks `public/`; `pg-web dev` syncs on save. Content-type from file extension (ship a small mapping, maybe `mime_guess` crate).

**Cutoff configurable:** `[assets] large_cutoff_bytes` in `pgweb.toml`. Default 1048576.

**Demo enhancement:** pull the inline `<style>` block out of `examples/demo/pages/index.html` into `public/styles.css`, verify the page still renders correctly.

**Tests:** `#[pg_test]` round-trip for both tiers. HTTP smoke for small. Tier 3 demo now hits `public/styles.css` and asserts on content-type + cache-control.

---

## Testing plan (consolidated)

| Tier | What gains coverage                                                                  |
|------|---------------------------------------------------------------------------------------|
| 1 — `#[pg_test]`    | Dynamic route pattern storage + capture extraction. Dev error page renderer. Asset round-trip (BYTEA + pg_largeobject). |
| 2a — HTTP smoke     | GET /posts/42 hits a dynamic handler with `req.path_params.id = "42"`. Dev error page served on handler crash. Small asset served with correct content-type. |
| 2b — CLI            | `paths.rs` recognizes `[id]` dirs. `dev.rs` file-event classifier. `stack.rs` port poller. |
| 3 — Docker E2E      | `up` → `dev` (writes a file in a thread) → HTTP reflects the change. Asset flow. Dynamic route flow. |

Target: 80+ tests green (from 58 today). Not a hard requirement; additive is fine.

## Things deliberately NOT in session 3

- **Published Docker image to registry** — M1.4 release task.
- **`pg-web env set`** (secrets via GUC) — M1.4.
- **`pg-web check`** (lint) — M1.4.
- **`pgweb.html_escape()` helper** — M1.4.
- **User-facing form-validation UX** — M1.4 (depends on dev error page from this session + html_escape from M1.4).
- **Browser live-reload push (WebSocket or SSE)** — **deferred to M1.4 as an explicit near-term priority.** M1.2 ships save → DB sync → manual F5. The browser-push follow-up is gated on dogfooding M1.2 first so the transport choice (WS vs SSE) and UX triggers are evidence-driven. See `ROADMAP.md` M1.4 and decision log (2026-04-20).
- **Content-hash asset filenames** — deferred to M1.4 with browser live-reload. M1.2 ships ETag-only caching; the Vite/webpack-style `styles.abc123.css` + HTML-rewrite model is the long-term upgrade. See `ROADMAP.md` M1.4.
- **Declarative schema diffing** — Phase 2.5.
- **Auth / sessions / RLS** — Phase 2.

## Design questions — resolved 2026-04-20

All four are locked before coding. Full rationale in `ROADMAP.md` decision log (2026-04-20 entries). Each decision MUST be documented inline in the code when implemented.

1. **Dynamic route storage — DERIVE from pattern.** `pgweb.routes.path_pattern` is the single source of truth; router parses `:name` tokens at match time to populate `req.path_params`. No `path_captures` column. Rationale: denormalization introduces drift risk for zero measurable gain at <100 routes. **Inline doc required in `router.rs`.**
2. **Router match — NAÏVE specificity-sorted scan.** Load all routes → sort by (static-segment count desc, capture-count asc, path-length desc) → first segment-by-segment match wins. No trie, no `RegexSet`. **Reevaluation trigger: >1000 routes/app OR measured hot path.** **Inline doc + reevaluation TODO required in `router.rs`.**
3. **File watcher — `notify-debouncer-full` + 200ms debounce + Blake3 content-hash dedupe + extension/dotfile filter.** Replicates the Vite/Next/chokidar architecture (native watcher → write-finish debounce → content-hash skip → include/exclude). 200ms chosen between Vite (100ms) and Next (300ms). **Module-level doc in `dev.rs` citing the Vite model.**
4. **Asset caching — ETag + `If-None-Match` (Phase 1).** Stable URLs, ETag = Blake3 of content, dev uses `Cache-Control: no-cache`, prod uses `max-age=0, must-revalidate`. **Inline doc flagging the upgrade path to content-hash filenames + HTML rewrite** (the Vite/webpack model; deferred to M1.4).

## Suggested order

Components land A → E, each followed by a stop-and-check:

1. **A** — `pg-web up`/`down`. Smallest, independent, nice DX win from day one.
2. **B** — `pg-web dev`. Depends on A (watcher expects stack running).
3. **C** — Dynamic routes. Schema + router + walker all change. Largest of the five.
4. **D** — Dev error page. Independent of A-C; could slot earlier, but relies on error paths that C may expose.
5. **E** — Static assets. Independent but demo enhancement ties it to the final state.

Order can shuffle if a component turns out to be blocked on an earlier one in a way the plan didn't anticipate.

---

## Recap — what shipped

All five components landed in the planned order, plus two unplanned but in-scope additions (push reconciliation + port-shadowing preflight). Final test state: **153 Rust tests + 1 black-box smoke all green via `scripts/test-all.sh`** (up from 58 at session start).

| #  | Commit      | Component / work                                     | Headline                                                                                      |
|----|-------------|-------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| A  | `a28ec87`   | `pg-web up` / `pg-web down` stack lifecycle           | Wraps `docker compose`, polls `:5432` + `:8080`, resolves `DATABASE_URL` from `pgweb.toml`.   |
| B  | `d3c3aa5`   | `pg-web dev` file watcher                             | notify-debouncer-full + 200ms + Blake3 dedupe + include/exclude filter + shift-left SQL preflight + in-band log tailing. Vite-architecture citation inline. |
| —  | `90df716`   | docs: Session 3 lessons + parking-lot test framework  | Testing-framework spec parked post-v1; DEVELOPER-GUIDE pitfalls + closure-injection convention. |
| —  | `5126182`   | **Push reconciliation** (pulled forward from M1.4)    | push is now 3-phase: apply → validate every handler's signature → reconcile stale routes / templates / `pgweb.pages__*(json)` handlers. `pgweb.pages__*(json) RETURNS json|text` is the formally-reserved push-managed namespace. |
| C  | `29e5ebc`   | Dynamic route `[id]` captures                         | Scanner emits `/posts/:id` + `pgweb.pages__posts__$id__index`. Router: naïve specificity-sorted scan, captures derived at match time, `req.path_params` always present. Captures are strings; handler casts. |
| D  | `bf5bfde`   | Typed error catalog + dev error page                  | `ServeError` enum (9 variants, PGWEB_E001–E999), code + title + remedy per variant. `pgweb.settings` table + `pg-web push` syncs env from `pgweb.toml`. PL/pgSQL `_framework_call_handler` wrapper catches `WHEN OTHERS` and returns SQLSTATE/MESSAGE/DETAIL/HINT/CONTEXT. Push-time Tera parse validation catches broken templates before the DB transaction. |
| —  | `0c00f20`   | Port-shadowing preflight + CLI smoke tier             | `pg-web up` now refuses to proceed if `:8080` / `:5432` are held by a non-Docker process (classic pitfall-#8 cause). `scripts/smoke-cli.sh` is tier 4 of `test-all.sh`; full black-box user flow with visible CLI stdout + HTTP body assertions. |
| E  | `b1d8e8b`   | Static asset serving from `public/*`                  | BYTEA-backed `pgweb.assets` with 2 MiB cap + Blake3 ETag. Router: asset fallback after page-route miss (GET-only). HTTP emits `ETag` + `Cache-Control`; `If-None-Match` → 304. `pg_largeobject` streaming + content-hash filenames deferred to M1.4. |

## Key architectural decisions locked this session

Logged in `docs/ROADMAP.md` § Decision log (all dated 2026-04-20 or 2026-04-22):

- **Dynamic route captures derived from pattern, not stored** — `path_pattern` is the single source of truth; `req.path_params` built at match time.
- **Router match = naïve specificity-sorted scan** — reevaluation trigger: >1000 routes/app OR measured hot path.
- **File watcher stack: Vite-architecture replica** — `notify-debouncer-full` + 200ms + Blake3 + filters. Browser live-reload via WS/SSE deferred to M1.4 as explicit near-term priority.
- **Static asset caching (M1.2): ETag + If-None-Match.** Content-hash filenames + HTML rewrite (the Vite/webpack model) deferred to M1.4 under "Content-hash asset filenames".
- **`pgweb.pages__*(json) RETURNS json|text` is the reserved push-managed handler namespace.** Push reconciles this namespace — helpers must live elsewhere.
- **`pgweb.settings` table is the runtime-config source of truth.** No GUC bridge; `pg-web push` syncs from `pgweb.toml`; `pg-web dev` overrides to `env='development'`. Choice: portability > microsecond-per-request savings on `current_setting`.
- **PL/pgSQL `_framework_call_handler` wrapper** catches `WHEN OTHERS` and returns structured SQLSTATE / MESSAGE / DETAIL / HINT / CONTEXT columns. Trades one savepoint per request for rich typed-error classification without longjmping across the Rust FFI.
- **Browser live-reload push (WS/SSE) deferred to M1.4 as near-term priority.** Gated on dogfooding M1.2's save-manual-F5 flow so the transport choice is evidence-driven.
- **App testing framework (`pg-web test`) parked in ROADMAP's post-v1 section.** User-flagged as meaningfully-later; sketched out so the thinking isn't lost.
- **Port-shadowing preflight in `pg-web up`** — refuses to run if a non-Docker process holds `:8080`/`:5432`. Formerly DEVELOPER-GUIDE pitfall #8; now caught at the top of the command.

## Gotchas hit this session

- **pgrx `#[pg_schema] mod` name must not start with `pg_`** — Postgres reserves `pg_*` schemas. My first attempt (`mod pg_tests`) failed at extension install with `SQLSTATE 42939 unacceptable schema name`. Module had to become `mod tests` (matching schema.rs) — pgrx's test framework calls `SELECT tests.<fn>()` with a hardcoded `tests` schema, so other names produce "function does not exist" at test time.
- **`notify-debouncer-full` re-exports `notify` but doesn't re-export traits into the root** — needed `use notify_debouncer_full::notify::Watcher;` explicitly to call `.watch()` on the inner watcher. Promoted to DEVELOPER-GUIDE pitfall #9.
- **`docker compose logs -f postgres` hardcodes service name** — if a user renames the scaffolded service, `pg-web dev --logs` silently goes quiet. Promoted to DEVELOPER-GUIDE pitfall #10; the scaffold template is the contract.
- **`BackgroundWorker::transaction` panics outside a registered BGW** — can't unit-test `router::serve()` directly in `#[pg_test]`. Tested individual helpers (`lookup_route`, `lookup_asset`, `call_handler`) and relied on tier 3 for the full-path smoke.
- **Stray `cargo pgrx run pg17` BGW shadows Docker's `:8080` publish on Linux** — pitfall #8. Docker's iptables rule + userspace port-publish doesn't intercept a pre-existing host listener; curl hits the pgrx dev PG with stale DB state and you chase ghosts. Fix shipped in `0c00f20` (preflight in `pg-web up`).
- **TCP accept on `:5432` ≠ PG ready for queries.** Cold container boot runs POSTGRES_DB init scripts for several seconds after the socket opens. Added app-level `wait_for_db_ready` poll (`SELECT 1` via libpq) to `stack::up`.
- **Tera parse vs render errors don't surface via a structured error kind** — tera 1.x's `Error::kind` is sparse. Had to string-match on the error chain in `templating::classify_tera_error` to split `TemplateParseError` from `TemplateRenderError`. Fragile across tera versions; worth revisiting if the crate evolves.
- **`ServeError` enum is ~144 bytes** — trips `clippy::result_large_err`. Boxing every `Result<_, ServeError>` would push a heap alloc into every `?` on hot paths for cold-path errors. Suppressed with a crate-level `#[allow]` and a comment explaining the tradeoff.
- **`push::push` used to leave stale routes / templates / handlers** after filesystem deletes — surfaced by the Component B watcher. Pulled push-reconciliation forward from M1.4 in-session; `push` is now fully reconciling and also validates handler existence + signature + template parse pre-DB.

## Handoff to Session 4

See `docs/sessions/session_4.md` for M1.4 closeout (v0.1 release). Highlights:

- `pgweb.html_escape()` SQL helper → user-facing form-validation UX.
- `pg-web env set/unset/list` for secrets via GUC.
- `pg-web init --template <name>` + scaffolded `README.md`.
- `pg-web check` offline validator (layout + Tera + SQL parse).
- **Browser live-reload push (WS/SSE)** — the near-term item we deferred from Session 3.
- **Content-hash asset filenames + HTML rewrite** — the Vite/webpack caching upgrade.
- **`pg_largeobject` streaming** for assets >2 MiB.
- `push` polished for prod deploy.
- Release pipeline + Docker Hub publish + docs pass.
