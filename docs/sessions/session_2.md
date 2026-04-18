# Session 2 — First Interactive Demo (todo app)

**Status:** planned, not started.
**Theme:** turn pg-web from read-only into interactive. By the end of this session, `examples/demo/` is a functional todo list that a developer can add, toggle, and delete items in via HTMX forms, all served by `docker compose up`.

**Exit criteria:**
- `GET /` renders the current todo list.
- `POST /add` creates a new todo and returns an HTMX fragment.
- `POST /toggle` toggles `done` on an existing todo (id from form body).
- `POST /delete` removes a todo (id from form body).
- All state lives in a `todos` table in the user's app DB — not in the `pgweb` framework schema.
- Raw-SQL migration applied via `pg-web migrate apply` on scaffold.
- End-to-end Docker test verifies the full CRUD loop.

---

## Prerequisites (already shipped in session 1)

- Extension with BGW + Axum + SPI→Tera pipeline ✅
- CLI with `init` and `push` ✅
- Docker image `pgweb/postgres:latest` ✅

## New pieces needed

### 1. Form body parsing in the request handler

**Where:** `crates/pg_web_ext/src/http.rs` + `router.rs`.

**What:** Axum's `axum::Form<T>` or manual body extraction. For the walking skeleton we want a `HashMap<String, String>` (or `serde_json::Value`) representing the parsed `application/x-www-form-urlencoded` body. HTMX submits urlencoded by default.

**Decision to make:**
- *Parse inline inside the fallback handler?* — simpler.
- *Use a dedicated extractor and a second route?* — more idiomatic Axum, more code.

**Rec:** inline inside the fallback. Keeps the "one handler rules them all" pattern.

### 2. Parameterized SQL handlers

**Where:** `router.rs::call_handler`.

**Today:** handler is called as `SELECT pgweb.pages__foo()::text`. No arguments.

**Need:** handler accepts a JSON object with request inputs. Shape options:

```sql
CREATE FUNCTION pgweb.pages__todos__add(req json) RETURNS json AS $$ ... $$
-- called as: SELECT pgweb.pages__todos__add('{"title":"buy milk"}'::json)
```

Request inputs to merge into that JSON:
- Form body fields (url-decoded)
- Query string (M1.2)
- Path captures (M1.2)
- Request method, user-agent, etc. available as special keys (future)

**Work:**
- Decide the JSON shape. Draft: `{ "body": {...}, "query": {...}, "method": "POST", "path": "/add" }`.
- `call_handler(handler_name, request_json)` → build the SQL call.
- `#[pg_test]` covering the shape.
- rustc ICE workaround still applies — `format!` + Postgres's `quote_literal()` (SQL function) keeps the JSON string safely escaped.

### 3. `pg-web migrate apply`

**Where:** new `crates/pg_web_cli/src/migrate.rs`.

**What:** Walk `migrations/` (sorted by filename — enforce `NNNN_description.sql` convention). For each file not yet in `pgweb.migrations` ledger, execute inside a transaction + insert a ledger row. Print per-file status.

**Schema:** add to the extension's install SQL:
```sql
CREATE TABLE pgweb.migrations (
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    name       TEXT PRIMARY KEY
);
```
Add `#[pg_test]` for it.

**CLI:** `pg-web migrate apply [--dir migrations]`. Reads `DATABASE_URL` from env (same as push).

**Tests:** tier 2b hermetic (verifies file walking + ordering); full DB test when we're ready for testcontainers (deferred).

### 4. Todo app (`examples/demo/`)

**Directory layout:**
```
examples/demo/
├── README.md                      # how to run this demo
├── docker-compose.yml             # (or rely on `pg-web init`-generated one)
├── pgweb.toml
├── migrations/
│   └── 0001_create_todos.sql      # CREATE TABLE public.todos ...
└── pages/
    ├── index.html                 # list view — HTMX form at top, <ul> below
    ├── index.sql                  # handler: SELECT todos + render list
    ├── add.html                   # HTMX fragment for a single new row
    ├── add.sql                    # handler: INSERT, return the new <li> fragment
    ├── toggle.html                # updated <li> fragment
    ├── toggle.sql                 # handler: UPDATE done = NOT done, return <li>
    └── delete.html                # empty body (HTMX removes the row)
        delete.sql                 # handler: DELETE, return nothing
```

**HTMX patterns to exercise:**
- `hx-post="/add"` on the form; `hx-target="#todos"` + `hx-swap="beforeend"` to append the new row.
- Each row has a toggle button with `hx-post="/toggle" hx-vals='{"id":123}' hx-target="closest li"`.
- Delete button with `hx-post="/delete" hx-target="closest li" hx-swap="outerHTML"`.

**Validation:**
- Empty title → `CHECK (length(title) > 0)` → unique_violation style error path. Need to verify our handler-returns-error-fragment flow works in M1.1 (it does — the SQL function just returns a different HTML string).

### 5. Docker E2E integration test

**Where:** new `tests/docker_e2e.rs` or `examples/demo/tests/`.

**What:** Tier 3 test that boots the full demo stack via testcontainers + `pgweb/postgres:latest`, runs `pg-web init` + `pg-web push` against it, exercises the CRUD flow via `reqwest`, asserts on response bodies.

**Why this needs the Docker image:** we don't want to require pgrx in CI; `pgweb/postgres:latest` + the built CLI is all that's needed.

**Deps:** `testcontainers`, `reqwest`.

**Decision needed:** where does the demo app source live for the test? Candidates:
- Inline in the test (fabricated directory via `tempfile`).
- Point the test at `examples/demo/` (couples test to demo layout — preferred).

### 6. Updated `scripts/test-all.sh`

Add the Docker E2E as a fourth tier. Should skip gracefully if Docker isn't available (for pure unit-test iteration).

---

## Things deliberately NOT in session 2

- **Hot reload / `pg-web dev`** — session 3.
- **Dynamic route patterns (`[id]`)** — session 3 (the todo app lives without them; IDs come via form bodies in session 2).
- **Dev error page overlay** — session 3.
- **Secrets / GUC** (`pg-web env set`) — M1.4, later.
- **Declarative schema diffing** (`pg-web migrate create`) — Phase 2.5, later.
- **Publishing `pgweb/postgres:latest` to Docker Hub / GHCR** — v0.1 release task.

---

## Known gotchas / things to watch

- **HTMX escaping.** Our Tera render has `auto_escape=true`. We saw this bite already: `{"greeting": "hello from /posts"}` rendered as `hello from &#x2F;posts`. For user-content-safe output this is correct; for rendering raw HTML fragments we'll need `{{ value | safe }}` in templates — document this clearly in the demo.
- **POST body size.** Axum has a default 2 MiB body limit. Fine for form submission; revisit if users upload files (not session 2).
- **CSRF.** Deferred. Any form-submit-from-browser is technically vulnerable until session 2-or-later wires CSRF tokens. Document the gap; fix in Phase 2 (auth).
- **Transaction boundaries on POST.** Same invariant as GET — one request one transaction. No new code needed; confirms the pattern holds.

---

## Potential design questions to resolve at the start of session 2

1. **Request JSON shape.** Lock in the keys: `body`, `query`, `method`, `path`, future `path_params`, future `session`. What's the default if the body is empty? `null` or `{}`?
2. **Handler return type on POST.** Same as GET — returns HTML string (NOT a JSON context this time; it's already-rendered HTML for HTMX swap). Does this change the request pipeline?
   - Option A: handler returns JSON, template renders it. For POST this means having a fragment template per route (lots of tiny files).
   - Option B: handler returns HTML string directly, skipping Tera. Simpler for fragments.
   - Option C: handler returns `{ "html": "...", "headers": { ... } }` — a richer contract. More work but more flexible.
   - **Lean:** B for POST handlers with explicit opt-out from Tera. Need to decide how the worker distinguishes A vs B routes — maybe a `skip_template: bool` column in `pgweb.routes`, or a naming convention, or set it in the handler's `returns` signature.
3. **Handler function naming for non-GET.** `pages/add.sql` → should the function be `pgweb.pages__add`? Collides if you also have a `GET /add`. Options:
   - Include method: `pgweb.pages__post__add`.
   - Separate tables per method (ugly).
   - **Lean:** embed method in handler name. `pgweb.pages__<method>__<path>` is unambiguous. Breaks M1.1's naive convention — update `paths.rs::handler_for`.
4. **Migrations ledger UX.** What does `pg-web migrate apply` print when there's nothing new? What about when an old migration is renamed/removed? (Probably bail loudly.)

---

## Rough time estimate

~3-5 hours of focused work, assuming no major rustc/pgrx surprises. The hardest piece is probably the request-JSON shape + handler-return contract (design work up front). Actual code is straightforward.

---

## Suggested order

1. Session start: lock answers to the four design questions above (~20 min, could be pre-session async).
2. Form-body + parameterized-handler plumbing in the extension — one commit.
3. Route-handler naming update in `paths.rs` + `push.rs` — one commit.
4. `pg-web migrate apply` — one commit.
5. Draft `examples/demo/` layout — one commit (mostly SQL + HTML).
6. Wire into test-all.sh with the tier-3 E2E — one commit.
7. Smoke the full demo manually from Windows (like we did in session 1), record anything new to `DEVELOPER-GUIDE.md § Common pitfalls`.
