# Session 2 — First Interactive Demo (todo app)

**Status:** spec locked 2026-04-18. Starting implementation.
**Theme:** turn pg-web from read-only into interactive. By the end of this session, `examples/demo/` is a functional todo list that a developer can add, toggle, and delete items in via HTMX forms, all served by `docker compose up`.

**Exit criteria:**
- `GET /` renders the current todo list.
- `POST /todos` creates a new todo and returns an HTMX fragment.
- `POST /todos/toggle` toggles `done` on an existing todo (id from form body).
- `POST /todos/delete` removes a todo (id from form body).
- All state lives in a `todos` table in the user's app DB — not in the `pgweb` framework schema.
- Raw-SQL migration applied via `pg-web migrate apply` on scaffold.
- End-to-end Docker test verifies the full CRUD loop.

---

## Prerequisites (already shipped in session 1)

- Extension with BGW + Axum + SPI→Tera pipeline ✅
- CLI with `init` and `push` ✅
- Docker image `pgweb/postgres:latest` ✅

---

## Decisions locked (no more open design questions)

Four design questions were on the table at the end of session 1. All four are now resolved. The canonical spec is [`docs/APP-LAYOUT.md`](../APP-LAYOUT.md); this section is a quick summary.

### 1. App directory layout — "directory = route, filename = method"

Each directory under `pages/` is a URL route. Files inside are named `<method>.html` (template) and `<method>.sql` (handler). `index` is the GET filename (preserves Apache/Nginx tradition). Either half is optional:

- `.html` only → static page, rendered with empty context `{}`. No SPI call.
- `.html` + `.sql` → M1.1 pipeline (handler returns JSON → Tera renders).
- `.sql` only → handler returns `text`, sent as-is (HTMX fragments).

M1.1's flat `pages/about.html` form goes away. Everything is a directory now.

### 2. Request JSON shape — single-arg uniform signature

Every handler takes one `json` argument:

```sql
CREATE FUNCTION pgweb.pages__<name>(req json) RETURNS <json|text> AS $$ ... $$
```

`req` always has this shape: `{ body: {...}, query: {...}, method: "POST", path: "/todos" }`. `body` and `query` are always objects (never null) — `req->'body'->>'key'` is always safe.

### 3. POST handler return contract — dispatch by template existence

`pgweb.routes.template_path` is nullable. The CLI populates it based on the filesystem:

- `.html` sibling exists → `template_path` set → router expects `json` return + Tera render.
- `.sql` alone → `template_path` NULL → router expects `text` return + sends bytes verbatim.

No new column in `pgweb.routes`. No per-row `skip_template: bool` flag. The filesystem's shape is the source of truth.

### 4. Non-GET handler naming — method-in-filename

Handler function name is `pgweb.pages__<path_segments>__<method_filename>`. Examples:

- `pages/index.sql`                → `pgweb.pages__index`
- `pages/todos/index.sql`          → `pgweb.pages__todos__index`
- `pages/todos/post.sql`           → `pgweb.pages__todos__post`
- `pages/todos/toggle/post.sql`    → `pgweb.pages__todos__toggle__post`

`paths.rs::handler_for` updates to walk the directory tree instead of flat files. `route_for` + `template_path_for` adapt similarly.

---

## Work breakdown (components with stop-and-check boundaries)

Each component below is one or more commits; pause at component boundaries to confirm direction.

### A. Migrations ledger + `pg-web migrate apply`

**Extension (schema.rs):**
- Add `pgweb.migrations` table: `(name TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())`.
- `#[pg_test]` covering table existence + insert sanity.

**CLI (new `migrate.rs`):**
- `pg-web migrate apply [--dir migrations] [--url <DATABASE_URL>]`.
- Walks `<dir>/*.sql` sorted by filename. Convention: `NNNN_description.sql`.
- For each file not in `pgweb.migrations`: wrap in `BEGIN ... COMMIT`, execute the SQL, insert the ledger row.
- Fail loudly on checksum/ordering violations (future); for Phase 1 just apply-in-order.
- Output: one line per file (`applied 0001_foo.sql` / `skipped 0002_bar.sql`).

**Tests:**
- Unit: filename sort + "not in ledger" filtering (pure functions).
- Integration (tier 2b): in-process `testcontainers` Postgres; apply a fixture directory twice; assert idempotency.

### B. Layout convention refactor

**CLI (`paths.rs` rewrite):**
- Replace flat-file walker with directory walker that yields `RouteEntry { method, path, handler_name, template_path: Option<String>, sql_path: Option<PathBuf>, html_path: Option<PathBuf> }`.
- Enforce reserved stems (`index`, `post` allowed in Phase 1; `get`, `put`, `patch`, `delete`, `head`, `options` rejected).
- Reject flat `.html` under `pages/` with a clear error.

**CLI (`push.rs` rewrite):**
- Walk the tree via the new iterator.
- For each RouteEntry:
  - If `html_path` present: upsert into `pgweb.templates`.
  - If `sql_path` present: execute the CREATE OR REPLACE FUNCTION inline.
  - Upsert into `pgweb.routes` (method, path, handler_name, template_path).
  - For HTML-only routes (no sql_path): synthesize a trivial handler that returns `'{}'::json` and register it.
- Verify handler return type matches mode before committing (introspect `pg_proc.prorettype`).

**CLI (`init.rs` / `templates.rs`):**
- Scaffold `pages/index.html` + `pages/index.sql` remains at root (good — that's the new convention too).
- Update `INDEX_SQL` to new `(req json) RETURNS json` signature.
- Any other scaffolded pages use the new dir-based layout.

**Tests:**
- Unit: every `route_for` / `handler_for` / `template_path_for` case in the APP-LAYOUT examples section.
- Integration: `push` against a fixture app with each of the three modes; assert on rows.

### C. Router — request JSON + return-type dispatch

**Extension (`http.rs`):**
- Parse `application/x-www-form-urlencoded` bodies into `serde_json::Value` (object shape).
- Parse query strings into `serde_json::Value`.
- Build the full `req` JSON before handing off to `router.rs`.

**Extension (`router.rs`):**
- `call_handler(handler_name, req)` — embed `req` as a `::json` literal via the same quote-literal escape path used today. (Still blocked by the rustc 1.95 `[DatumWithOid; N]` ICE; workaround unchanged.)
- Dispatch:
  - `template_path` Some: `Spi::get_one::<String>(...handler(req)::text)` → parse JSON → Tera render.
  - `template_path` None: `Spi::get_one::<String>(...handler(req)::text)` → send bytes verbatim with `content-type: text/html; charset=utf-8`.
- The seeded `pgweb.hello_handler` gains the `(req json)` signature; schema.rs updated in the same commit.

**Tests:**
- `#[pg_test]`: hello_handler returns JSON with seeded request; text-mode handler returns and is pass-through.
- HTTP smoke: POST form with body → handler sees body keys; text-mode POST returns bytes without Tera touching them.

### D. Companion demo — `examples/demo/todo`

```
examples/demo/
├── README.md                          # how to run
├── pgweb.toml
├── docker-compose.yml
├── migrations/
│   └── 0001_create_todos.sql          # CREATE TABLE public.todos ...
└── pages/
    ├── index.html                     # GET / — list view (HTMX form + <ul>)
    ├── index.sql                      # GET / — SELECT todos, return JSON
    ├── todos/
    │   ├── post.html                  # POST /todos — new <li> fragment
    │   └── post.sql                   # POST /todos — INSERT, return JSON for fragment
    ├── todos/toggle/
    │   └── post.sql                   # POST /todos/toggle — UPDATE, return <li> as text
    └── todos/delete/
        └── post.sql                   # POST /todos/delete — DELETE, return ''
```

HTMX patterns exercised:

- `hx-post="/todos"` on the form, `hx-target="#todos"`, `hx-swap="beforeend"`.
- Each `<li>` has a toggle button with `hx-post="/todos/toggle" hx-vals='{"id":N}' hx-target="closest li" hx-swap="outerHTML"`.
- Delete button with `hx-post="/todos/delete" hx-vals='{"id":N}' hx-target="closest li" hx-swap="outerHTML"`.

### E. Docker E2E tier + `scripts/test-all.sh`

- New `tests/docker_e2e.rs` (at workspace root or in `pg_web_cli`).
- Uses `testcontainers` to boot `pgweb/postgres:latest`.
- Runs `pg-web migrate apply` then `pg-web push` against `examples/demo/`.
- Hits the CRUD flow via `reqwest`:
  - `GET /` — assert rendered page contains the form.
  - `POST /todos` with `title=hello` — assert response contains `<li>` + "hello".
  - `POST /todos/toggle` — assert `class="done"`.
  - `POST /todos/delete` — assert empty body.
- Add tier 3 to `scripts/test-all.sh`. Skip gracefully (exit 0) if Docker unavailable.

---

## Testing plan (consolidated)

| Tier | What runs                                                 | What each new piece gets              |
|------|-----------------------------------------------------------|---------------------------------------|
| 1 — `#[pg_test]`    | `cargo pgrx test pg17`                      | Migrations table; handler return-type dispatch; `req` JSON roundtrip |
| 2a — HTTP smoke     | `scripts/test-http.sh`                      | POST with form body hits handler; text-mode pass-through; template-mode render |
| 2b — CLI tests      | `cargo test -p pg_web_cli`                  | `paths.rs` new conventions; `migrate.rs` walk/order/ledger; `push.rs` tree walker + mode detection |
| 3 — E2E (new)       | `scripts/test-all.sh` + Docker             | Full CRUD against real Docker stack |

`scripts/test-all.sh` grows a fourth stage that runs tier 3 conditionally on Docker availability. All existing tiers continue to be mandatory.

Feature-matrix rows in `docs/TESTING.md` get checked off as each component lands: `pg-web migrate apply`, Tera `{% for %}`, HTMX POST form, HTMX PATCH-style fragment swap, HTMX delete, validation via CHECK, the demo's `public/styles.css`.

---

## Things deliberately NOT in session 2

- **Hot reload / `pg-web dev`** — session 3.
- **Dynamic route patterns (`[id]`)** — session 3 (the todo app lives without them; IDs come via form bodies in session 2).
- **Dev error page overlay** — session 3.
- **Secrets / GUC** (`pg-web env set`) — M1.4, later.
- **Declarative schema diffing** (`pg-web migrate create`) — Phase 2.5, later.
- **`pg-web check` / lint tool** — M1.4 (added to roadmap in this session).
- **Publishing `pgweb/postgres:latest` to Docker Hub / GHCR** — v0.1 release task.
- **HTML-escape SQL helper (`pgweb.html_escape`)** — M1.4 closeout; session 2 demo uses Tera for any dynamic fragment so this isn't blocking.
- **Scaffolded `README.md` in the app directory** — small DX follow-up. `pg-web init` currently produces no README; adding one with next-step commands + a pointer to `docs/APP-LAYOUT.md` would help new users (human or agent). Track as a tiny trailing commit after component D/E.

---

## Known gotchas / things to watch

- **HTMX escaping.** Tera auto-escapes by default. For the todo demo this is what we want — fragment templates render title values through `{{ todo.title }}` and get safe output for free.
- **POST body size.** Axum defaults to 2 MiB. Fine for form submission; revisit if users upload files (not session 2).
- **CSRF.** Deferred. Any browser form-submit is technically vulnerable until Phase 2 wires tokens. Document the gap.
- **Transaction boundaries on POST.** Same invariant as GET — one request = one transaction. No new code needed.
- **rustc 1.95 ICE on `[DatumWithOid; N]`.** Session 1 workaround (escape-via-format + `quote_literal`) still applies for the `req` JSON. No change.
- **Migration ordering semantics.** We sort by filename ascending. If a user renames `0002_foo.sql` to `0001b_foo.sql` after applying, we don't detect it — Phase 2+ migration hardening task.

---

## Suggested order

Components land in the order above (A → E), each followed by a stop-and-check:

1. **A** — Migrations: smallest diff, unblocks the demo app's schema. Commit.
2. **B** — Layout refactor: touches `paths.rs`, `push.rs`, `init`. All unit-testable without the extension. Commit.
3. **C** — Router + request JSON: extension-side work, unblocks interactive handlers. Commit.
4. **D** — Demo app: exercises everything above. Commit.
5. **E** — E2E tier: proves the full stack works under the Docker image. Commit.
