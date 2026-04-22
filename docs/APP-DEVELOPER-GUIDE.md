# pg-web — App Developer Guide

For developers *using* pg-web to build apps. You write SQL and HTML. You never write or compile Rust.

> **Status (2026-04-18):** Session 2 in progress. M1.1 walking-skeleton is complete; M1.3 (interactive todo app) lands piece-by-piece. Features marked **(M1.x)** aren't implemented yet — see `docs/ROADMAP.md` for the full timeline.

## 60-second orientation

1. **A pg-web app is two things**: a `pages/` directory (routes + templates + handler SQL) and a `migrations/` directory (your database schema, forward-only).
2. **Each directory under `pages/` is a URL route.** Files inside are named after the HTTP method — `index.{html,sql}` for GET, `post.{html,sql}` for POST.
3. **Two deploy commands**: `pg-web migrate apply` advances the schema; `pg-web push` replaces the routes, templates, and handler functions from the current filesystem. Always migrate first, push second.
4. **Everything runs inside Postgres**: the web server is a background worker in the same process tree as the database. No Node/Python/Go backend to install.

The exhaustive layout spec is `docs/APP-LAYOUT.md`. This guide is the narrative "how do I build something" version.

## Install

```bash
cargo install --path crates/pg_web_cli     # from this repo for now
# Pre-built binaries and `brew install pg-web` land with v0.1.
```

Plus Docker (for running Postgres + the extension locally).

## Create a project

```bash
pg-web init my-blog
cd my-blog
pg-web up          # boots the Docker Compose stack, prints DATABASE_URL
pg-web push        # auto-resolves the URL from pgweb.toml + env
open http://localhost:8080     # or `curl localhost:8080/`
```

Four commands and you're serving. Edit `pages/index.html`, refresh — live (if `pg-web dev` is running; otherwise rerun `pg-web push` first).

`pg-web up` / `pg-web down` are thin wrappers over `docker compose up -d` / `down`. `up` polls Postgres + the HTTP server until both accept connections, and resolves `DATABASE_URL` from `pgweb.toml`'s `[database].url_env` (default `DATABASE_URL`), falling back to the dev-scaffold default baked into `docker-compose.yml`. `pg-web down --volumes` also drops the `pgdata` volume (destructive).

`pg-web dev` watches `pages/` and `public/` for changes and auto-pushes on save. It also tails the Postgres container's logs inline (`--no-logs` turns that off). A save triggers a 200ms debounce → content-hash dedupe (so re-saving with identical bytes is a no-op) → a shift-left `BEGIN;...ROLLBACK;` preflight on any changed handler `.sql` (so parse errors surface without touching live routes) → a full `pg-web push`. Ctrl-C stops the watcher cleanly. Note: after save, the browser still needs a manual refresh — browser-push (WebSocket/SSE) is an M1.4 follow-up.

## Project anatomy

```
my-blog/
├── pages/                   # URL routes (directory = route)
│   ├── index.html           # GET / template
│   └── index.sql            # GET / handler (returns JSON for the template)
├── public/                  # Static assets (M1.3+ — served at /<filename>)
├── migrations/              # Forward-only SQL migrations (NNNN_name.sql)
├── pgweb.toml               # Framework config
├── docker-compose.yml       # Boots pgweb/postgres locally
├── Caddyfile                # Prod TLS (commented out in dev)
└── .gitignore
```

## The app layout rules (essential)

**Rule 1 — Directory = route.** `pages/todos/toggle/` → `/todos/toggle`. A flat `pages/about.html` is an error; use `pages/about/index.html`.

**Rule 2 — Filename = HTTP method.** `index` = GET, `post` = POST. Other method stems (`put`/`patch`/`delete`/`head`/`options`) are reserved for Phase 2+ and rejected today.

**Rule 3 — Each method has two files, either optional.**

| Files present          | Pipeline                                                     | Use for                                  |
|------------------------|--------------------------------------------------------------|------------------------------------------|
| `.html` only           | Template rendered with empty context — **no SQL runs**       | Static marketing / about / contact pages |
| `.html` + `.sql`       | Handler returns `json`, Tera renders the template with it    | Most pages; auto-escape safe by default  |
| `.sql` only            | Handler returns `text`, router sends bytes as-is             | HTMX fragments; no-content responses     |

All three cases live side-by-side in the same tree. You pick per-route.

The full spec with every edge case is `docs/APP-LAYOUT.md`.

## The handler contract

Every `.sql` file defines exactly one Postgres function in the `pgweb` schema, matching the file path:

| File                                    | Function name                              |
|-----------------------------------------|--------------------------------------------|
| `pages/index.sql`                       | `pgweb.pages__index`                       |
| `pages/todos/index.sql`                 | `pgweb.pages__todos__index`                |
| `pages/todos/post.sql`                  | `pgweb.pages__todos__post`                 |
| `pages/todos/toggle/post.sql`           | `pgweb.pages__todos__toggle__post`         |

Signature is uniform — **one `json` argument called `req`**:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__<name>(req json) RETURNS <json|text> AS $$ ... $$
```

### The `req` argument — what's in it

```json
{
  "body":        { "title": "buy milk" },   // parsed form body; {} if empty — never null
  "query":       { "page": "2" },           // parsed query string; {} if empty — never null
  "method":      "POST",                    // HTTP method, uppercase
  "path":        "/todos/42",               // URL path after matching (not the pattern)
  "path_params": { "id": "42" }             // captures from dynamic segments; {} for static routes
}
```

You read it like any JSON column:

```sql
req->'body'->>'title'                          -- string or NULL if missing
(req->'body'->>'id')::bigint                   -- cast when you need an int
req->>'method'                                 -- "GET", "POST", etc.
req->'path_params'->>'id'                      -- a capture from pages/posts/[id]/
```

### Return type decides the pipeline

- Function `RETURNS json` → the router feeds that JSON to Tera as the template context.
- Function `RETURNS text` → the router sends the text as-is (`content-type: text/html`).

Which you pick is tied to which files exist. If your route has a `.html` sibling, return `json` (or `pg-web push` will refuse it). If your route is `.sql`-only, return `text`.

## Three full examples

### Example 1 — Dynamic GET page (list view)

`pages/todos/index.html`

```html
<!doctype html>
<html>
<body>
  <h1>Todos</h1>
  <form hx-post="/todos" hx-target="#todos" hx-swap="beforeend">
    <input name="title" required>
    <button>Add</button>
  </form>
  <ul id="todos">
    {% for todo in todos %}
      <li>{{ todo.title }}</li>
    {% endfor %}
  </ul>
</body>
</html>
```

`pages/todos/index.sql`

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__todos__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'todos', COALESCE(
      (SELECT json_agg(row_to_json(t) ORDER BY t.id) FROM todos t),
      '[]'::json
    )
  )
$$ LANGUAGE sql STABLE;
```

### Example 2 — POST returning an HTMX fragment (through Tera)

`pages/todos/post.html`

```html
<li>{{ todo.title }}</li>
```

`pages/todos/post.sql`

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__todos__post(req json) RETURNS json AS $$
  INSERT INTO todos (title) VALUES (NULLIF(req->'body'->>'title', ''))
  RETURNING json_build_object('todo', json_build_object(
    'id', id, 'title', title, 'done', done
  ))
$$ LANGUAGE sql;
```

Tera auto-escapes `{{ todo.title }}` — XSS-safe by default.

### Example 3 — POST returning raw text (no template)

`pages/todos/toggle/post.sql` — handler returns `text`, no sibling `.html`:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__todos__toggle__post(req json) RETURNS text AS $$
  UPDATE todos SET done = NOT done
  WHERE id = (req->'body'->>'id')::bigint
  RETURNING format(
    '<li class="%s">%s</li>',
    CASE WHEN done THEN 'done' ELSE '' END,
    title   -- TODO: escape via pgweb.html_escape() once it ships in M1.4.
            -- Until then prefer the `.html` + `.sql` form for dynamic fragments.
  )
$$ LANGUAGE sql;
```

Use this mode for minimal no-interpolation fragments or for endpoints that return no body (delete returning `''`).

## Migrations

Raw SQL only in Phase 1. You hand-write `.sql` files; the CLI applies them in filename order, tracks what's applied in `pgweb.migrations`.

```
migrations/
├── 0001_create_todos.sql
├── 0002_add_done_column.sql
└── 0003_seed_categories.sql
```

```sql
-- migrations/0001_create_todos.sql
CREATE TABLE public.todos (
  id        bigserial PRIMARY KEY,
  title     text NOT NULL CHECK (length(title) > 0),
  done      boolean NOT NULL DEFAULT false,
  created_at timestamptz NOT NULL DEFAULT now()
);
```

Run:

```bash
pg-web migrate apply --url "$DATABASE_URL"
```

Output:

```
✓ applied 0001_create_todos.sql
✓ applied 0002_add_done_column.sql
— skipped 0003_seed_categories.sql (already in ledger)
2 applied, 1 skipped
```

Idempotent — re-running after a clean apply is a no-op. Each file runs in its own transaction (so if it errors, that file rolls back cleanly; earlier successful files stay applied).

**Rules**:
1. Migrations are **forward-only**. Once a filename is in `pgweb.migrations`, it's never re-run. Edits to an already-applied file are silently ignored (Phase 1 has no checksum — **don't edit applied files**).
2. New migrations get a higher number than the most recent — filename lex order is the apply order.
3. **Declarative schema-diffing** (`pg-web migrate create` from a Prisma file or DBML) is deferred to **Phase 2.5**. Write DDL by hand for now.
4. Migrations change the **database shape** (tables, columns, indexes, constraints). They do **not** manage routes/templates/handlers — those are `pg-web push`'s job.

## The deploy loop

Routes + templates + handlers live in `pages/`. Migrations live in `migrations/`. Two commands, always in this order:

```bash
pg-web migrate apply    # advance the schema (forward-only). --url optional.
pg-web push             # reconcile routes/templates/handlers with pages/
```

`push` is idempotent and **reconciling**: it runs every handler's `CREATE OR REPLACE FUNCTION`, upserts templates, upserts route rows, then deletes any `pgweb.routes` / `pgweb.templates` rows and drops any `pgweb.pages__*(json) RETURNS json|text` functions that no longer have a matching file on disk. Delete a file from `pages/`, run `push`, the corresponding route is gone. Running `push` twice in a row with no filesystem changes is a no-op.

Push also **validates** every handler after executing your `.sql`. If your file runs cleanly but doesn't actually define the function the router expects (typo, wrong argument list, wrong return type), push rolls the whole transaction back with a clear error pointing at the file and the expected signature — the live extension keeps serving the previous good push.

Order matters: `push` assumes the tables your handlers touch already exist (run `migrate apply` first).

### Running `pg-web dev` doesn't apply migrations

`pg-web dev` watches `pages/` and `public/` — **not `migrations/`.** Adding a new `migrations/NNNN_x.sql` file doesn't trigger anything; run `pg-web migrate apply` explicitly whenever the schema needs to advance. This is deliberate: migrations are permanent history, not reloadable code.

### The `pgweb.pages__*(json)` namespace is reserved

Push owns every Postgres function matching `pgweb.pages__<name>(req json) RETURNS <json|text>` — it creates them from your `.sql` files and drops any that no longer have a matching file. **Don't put your own helper functions in that namespace with that signature** or the next push will drop them. Safe patterns for helpers: `pgweb.helper_<name>(args)`, `pgweb.util_<name>(args)`, or `public.<name>(args)` — any name or signature that isn't `pages__*(json) RETURNS json|text` is left untouched.

## Errors: dev vs. prod

The extension behaves differently depending on `pgweb.settings.env`:

- `development` (default for fresh installs and `pg-web dev`): fatal errors render a **typed error page** with the code (e.g. `PGWEB_E003_HANDLER_SQL_EXCEPTION`), the SQL diagnostics (`SQLSTATE`, `MESSAGE`, `DETAIL`, `HINT`), the handler function name, a one-paragraph remedy, and the pretty-printed `req` JSON that triggered the failure.
- `production`: the response body is a generic `internal server error`. No internals leak. The full error still goes to the Postgres log.

Flip modes by editing `pgweb.toml`:

```toml
[server]
env = "production"   # or "development"
```

…and running `pg-web push`. Push upserts the value into `pgweb.settings` as its last step; the change takes effect on the next request. `pg-web dev` always forces `development` for the duration of its watch session.

### Error codes you'll see

| Code                                     | When                                                              |
|------------------------------------------|-------------------------------------------------------------------|
| `PGWEB_E001_HANDLER_MISSING`             | Route's handler function doesn't exist in `pg_proc`               |
| `PGWEB_E002_HANDLER_SIGNATURE`           | Handler exists but the arg list or return type is wrong           |
| `PGWEB_E003_HANDLER_SQL_EXCEPTION`       | SQL raised inside the handler (constraint violation, divide by 0) |
| `PGWEB_E004_HANDLER_RETURN_NOT_JSON`     | Full-mode handler returned text that doesn't parse as JSON        |
| `PGWEB_E005_TEMPLATE_MISSING`            | Route references a `template_path` not in `pgweb.templates`       |
| `PGWEB_E006_TEMPLATE_PARSE`              | Tera can't parse the template                                     |
| `PGWEB_E007_TEMPLATE_RENDER`             | Tera parsed but missing a variable / filter                       |
| `PGWEB_E008_ROUTE_PATTERN_MALFORMED`     | Stored `path_pattern` doesn't match the `:name` syntax            |
| `PGWEB_E999_OTHER`                       | Anything not yet classified — file an issue with the context      |

Most of these are caught by `pg-web push` before they can reach runtime (handler existence + signature + template parse). The ones that surface in prod are usually `PGWEB_E003` (bad user input hitting a constraint) and `PGWEB_E007` (template expects a field your handler didn't emit on an edge-case code path).

## Configuration (`pgweb.toml`)

```toml
[server]
port = 8080                 # Port the extension binds HTTP on
env  = "development"        # "development" | "production" — affects 500 page detail

[database]
url_env = "DATABASE_URL"    # Which env var holds the connection string

[dev]
watch_paths = ["pages", "public"]   # For `pg-web dev` in M1.2+
```

## Forms & validation

Let Postgres constraints own validation. Catch exceptions in PL/pgSQL, return targeted HTML:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__signup__post(req json) RETURNS json AS $$
DECLARE
  v_email text := req->'body'->>'email';
  v_pw    text := req->'body'->>'password';
BEGIN
  INSERT INTO users(email, password_hash)
  VALUES (v_email, crypt(v_pw, gen_salt('bf', 12)));
  RETURN json_build_object('ok', true);
EXCEPTION
  WHEN unique_violation THEN
    RETURN json_build_object('ok', false, 'error', 'Email already taken');
  WHEN check_violation THEN
    RETURN json_build_object('ok', false, 'error', 'Invalid input');
END;
$$ LANGUAGE plpgsql;
```

Your `pages/signup/post.html` template branches on `{% if ok %}` vs `{% else %}`. HTMX's `hx-swap-oob="true"` lets you update multiple page regions from one response.

## What you DON'T have to do

- Write Rust.
- Compile anything.
- Run a Node/Python/Go app server.
- Manage a connection pool (SPI doesn't use one).
- Set up a reverse proxy for local dev (`:8080` is direct).
- Wire up an ORM (there isn't one; you're writing SQL).
- Worry about CSRF yet (**Phase 2** wires automatic double-submit-cookie on non-GET HTMX requests).

## What you DO handle

- Write SQL. The handlers, the migrations, the validation constraints.
- Write HTML (optionally with Tera templating).
- Think in transactions — each request is one, committed on 2xx, rolled back on error.
- Own a VPS for production. Managed-DB services (RDS, Cloud SQL, Supabase) don't accept custom extensions; Phase 1 is BYO-server only.
- Set up Postgres RLS policies in **Phase 2+** if you have multi-tenant data.

## Not implemented yet

| Feature                                                         | Lands in |
|-----------------------------------------------------------------|----------|
| ~~Hot reload (`pg-web dev`)~~                                   | M1.2 ✓   |
| ~~Dynamic route patterns (`[id]` capture → `req.path_params`)~~ | M1.2 ✓   |
| ~~Dev error page (typed catalog + rich SQL overlay)~~           | M1.2 ✓   |
| Static asset serving (`public/*` → HTTP)                        | M1.2–1.3 |
| Browser live-reload push (WS/SSE; auto-F5 on save)              | M1.4     |
| CLI `pg-web env set/unset/list` (secrets via GUC)               | M1.4     |
| CLI `pg-web check` (project validator)                          | M1.4     |
| `pgweb.html_escape()` SQL helper                                | M1.4     |
| Declarative schema-diffing (`migrate create`)                   | Phase 2.5 |
| Auth + sessions + RLS bridge                                    | Phase 2   |
| Async job queue                                                 | Phase 3   |
| In-browser dashboard / dev error overlay                        | Phase 4   |

Check `docs/ROADMAP.md` before building against a "not yet" feature.

## Where to go next

- **`docs/APP-LAYOUT.md`** — the exhaustive, spec-level reference for routing and file conventions. Use this when you want the rule on an edge case.
- **`docs/ARCHITECTURE.md`** — how the framework actually works under the hood.
- **`docs/ROADMAP.md`** — what's shipping when, and what's deliberately out of scope.
- **`examples/demo/`** — the companion todo app. Runs end-to-end; read it to see every Phase 1 feature exercised together. (Lands as M1.3.)
