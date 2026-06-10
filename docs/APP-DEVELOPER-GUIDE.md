# pg-web — App Developer Guide

For developers *using* pg-web to build apps. You write SQL and HTML. You never write or compile Rust.

> **Status:** v0.2.0 (Phase 1 complete). All core features (dev loop, dynamic routes, assets, live-reload, validation UX, `pg-web check`, etc.) are shipped. See `docs/ROADMAP.md` for Phase 2+ plans. This guide targets app developers writing `.sql` + `.html`. Framework maintainers: start with `CONTRIBUTING.md` + `docs/internal/`.

## 60-second orientation

1. **A pg-web app is two things**: a `pages/` directory (routes + templates + handler SQL) and a `migrations/` directory (your database schema, forward-only).
2. **Each directory under `pages/` is a URL route.** Files inside are named after the HTTP method — `index.{html,sql}` for GET, `post.{html,sql}` for POST.
3. **Two deploy commands**: `pg-web migrate apply` advances the schema; `pg-web push` replaces the routes, templates, and handler functions from the current filesystem. Always migrate first, push second.
4. **Everything runs inside Postgres**: the web server is a background worker in the same process tree as the database. No Node/Python/Go backend to install.

The exhaustive layout spec is `docs/APP-LAYOUT.md`. This guide is the narrative "how do I build something" version.

## Install

```bash
cargo install pg-web
```

The published crate installs the `pg-web` CLI. The runtime (Postgres + extension) is supplied by the official Docker image `pgweb/postgres:latest` — the CLI itself does not embed Postgres.

Plus Docker (for running Postgres + the extension locally during development). For production deploys you also use the same image.

## Create a project

```bash
pg-web init my-blog
cd my-blog
pg-web up          # boots the Docker Compose stack, prints DATABASE_URL
pg-web push        # auto-resolves the URL from pgweb.toml + env
open http://localhost:8080     # or `curl localhost:8080/`
```

Four commands and you're serving. Edit `pages/index.html`, refresh — live (if `pg-web dev` is running; otherwise rerun `pg-web push` first).

Want more code to poke at? `pg-web init my-todos --template todo` scaffolds the full HTMX todo list instead of the minimal shell — dynamic routes, migrations, static assets, form validation. Available templates: `todo`. The scaffolded `README.md` in either path has the quickstart commands and pointers to the docs.

`pg-web up` / `pg-web down` are thin wrappers over `docker compose up -d` / `down`. `up` polls Postgres + the HTTP server until both accept connections, and resolves `DATABASE_URL` from `pgweb.toml`'s `[database].url_env` (default `DATABASE_URL`), falling back to the dev-scaffold default baked into `docker-compose.yml`. `pg-web down --volumes` also drops the `pgdata` volume (destructive).

`pg-web dev` watches `pages/` and `public/` for changes and auto-pushes on save. It also tails the Postgres container's logs inline (`--no-logs` turns that off). A save triggers a 200ms debounce → content-hash dedupe (so re-saving with identical bytes is a no-op) → a shift-left `BEGIN;...ROLLBACK;` preflight on any changed handler `.sql` (so parse errors surface without touching live routes) → a full `pg-web push`. Ctrl-C stops the watcher cleanly. Browser tabs auto-reload on successful push via injected SSE (dev only; production mode never injects).

## Project anatomy

```
my-blog/
├── pages/                   # URL routes (directory = route)
│   ├── index.html           # GET / template
│   └── index.sql            # GET / handler (returns JSON for the template)
├── public/                  # Static assets (served at /<filename>)
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
    title   -- use pgweb.html_escape() for raw-text handlers that interpolate user content.
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

### Static assets in `public/`

Files under `public/` are served from the database at their URL-equivalent path:

- `public/styles.css` → `GET /styles.css`
- `public/img/logo.png` → `GET /img/logo.png`

`pg-web push` walks the tree, reads each file, computes a Blake3 content hash (used as the HTTP `ETag`), and upserts the row into `pgweb.assets`. Deletions are reconciled: remove a file from `public/`, re-run push, the row is dropped.

Responses include:
- `ETag`: the Blake3 hash, double-quoted as the HTTP spec requires.
- `Cache-Control`:
  - `no-cache` in development.
  - `public, max-age=0, must-revalidate` in production for canonical URLs.
  - `public, max-age=31536000, immutable` in production for fingerprinted URLs (see "Content-hash filenames" below).

A follow-up request that sends back the advertised `ETag` in `If-None-Match` gets `304 Not Modified` with no body — so repeat hits save bytes but always revalidate.

### Content-hash filenames in production (v0.2 Component H)

When `pgweb.toml [server].env = "production"`, push fingerprints each asset's URL: `/styles.css` becomes `/styles.<8hex>.css` (Blake3-derived). Templates get rewritten in the same step — literal `href="/styles.css"` swaps to `href="/styles.<hex>.css"` before the row is upserted. Combined with `Cache-Control: immutable`, this gives Vite-class long-cache behavior: zero round-trip on cache hit, automatic invalidation on content change.

Limitations:
- Only **double-quoted** attribute values are rewritten. Single-quoted (`href='/styles.css'`) and unquoted (`href=/styles.css`) attrs stay literal.
- **Dynamic refs** like `<img src="{{ user.avatar }}">` can't be rewritten at push time. Their URLs stay canonical and pick up `must-revalidate` instead of `immutable`.
- Dev mode (`[server].env = "development"`, the default) skips the rewrite. Iteration loop stays predictable — saves don't change asset URLs.

**Limits:**
- Per-file cap is **20 MiB** (CHECK constraint in `pgweb.assets`, raised from 2 MiB in v0.2 Component I). Push refuses oversized files with the path in the error.
- `.gitkeep` is skipped so an empty `public/` dir can live in git.
- If a page route exists at the same path as an asset, the page wins — user-defined routes are always more specific than the asset fallback.

Larger assets (>20 MiB) via `pg_largeobject` with `lo_read`-backed streaming is Phase 2+ work; until then, host those on a CDN or object store and link to them from your templates.

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
watch_paths = ["pages", "public"]   # For `pg-web dev` watcher
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
  RETURN json_build_object('success', true);
EXCEPTION
  WHEN unique_violation THEN
    RETURN json_build_object('success', false, 'error', 'Email already taken');
  WHEN check_violation THEN
    RETURN json_build_object('success', false, 'error', 'Invalid input');
END;
$$ LANGUAGE plpgsql;
```

Your `pages/signup/post.html` template branches on `{% if success %}` vs `{% else %}`. HTMX's `hx-swap-oob="true"` lets you update a separate region (e.g. a `<div id="form-error">` next to the form) from the same response, so the main target still receives the success fragment on happy path and the error lands beside the form on failure.

**Live reference:** `examples/todo/pages/todos/post.sql` + `pages/todos/post.html` + `pages/index.html` exercise this pattern end-to-end. The table's `CHECK (length(trim(title)) > 0)` on `public.todos` is the validation rule; the handler catches `check_violation` and the template dispatches between an appended `<li>` (success) and an OOB `#form-error` fragment (failure).

### Escaping user input in raw-text handlers

When you return text directly from a handler (no `.html` template sibling), Tera is not in the render path — nothing auto-escapes the bytes you emit. Use `pgweb.html_escape(text)` to make user input safe to interpolate inline:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__search__post(req json) RETURNS text AS $$
  SELECT '<p>No results for "' || pgweb.html_escape(req->'body'->>'q') || '".</p>'
$$ LANGUAGE sql;
```

Escapes the five HTML-unsafe characters (`&`, `<`, `>`, `"`, `'`). `STRICT`, so NULL input → NULL output without extra ceremony. Handlers returning via a Tera template don't need this — Tera's default `{{ var }}` already escapes — but it's the right tool whenever you're concatenating strings into raw HTML.

## Runtime settings & secrets

Values that change per deploy (API keys, feature flags, environment markers) live in the `pgweb.settings` table, persisted in the database itself rather than in the image or a sidecar config file. The CLI manages them; handlers read them with one SQL call.

```bash
pg-web env set STRIPE_KEY=sk_test_abc_xyz
pg-web env set FEATURE_NEW_ONBOARDING=1
pg-web env list
# STRIPE_KEY=sk_test_abc_xyz
# FEATURE_NEW_ONBOARDING=1
# env=development
pg-web env unset STRIPE_KEY
```

Handlers read with `pgweb.setting(key)`:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__checkout__post(req json) RETURNS json AS $$
  SELECT json_build_object(
    'session', stripe_create_session(
      COALESCE(pgweb.setting('STRIPE_KEY'),
               pgweb.setting('STRIPE_KEY_FALLBACK'))
    )
  )
$$ LANGUAGE sql;
```

`pgweb.setting(key)` is `STABLE STRICT PARALLEL SAFE` and returns NULL on miss — `COALESCE(pgweb.setting('FOO'), 'default')` is the idiomatic way to provide a fallback.

**Reserved keys.** The `env` key is synced from `pgweb.toml [server].env` on every `pg-web push`, so `pg-web env set env=…` is rejected at the CLI — edit the toml and re-push instead. Everything else is free-form.

**What this is not.** The CLI writes values in cleartext; encrypted secrets / KMS integration are Phase 2. Don't store anything the DB admin shouldn't be able to `SELECT`. For TLS-at-rest, use a PG instance with disk encryption; for TLS-in-transit, use `sslmode=require` on the connection URL.

## Browser live-reload under `pg-web dev`

`pg-web dev` auto-reloads connected browser tabs on every save. No HTMX config, no extra scripts to include — the dev server injects a tiny client stub into every rendered HTML response, and the running extension exposes an SSE stream the stub subscribes to.

The chain:

1. You save `pages/index.html` (or `post.sql`, or `public/styles.css`, etc.).
2. `pg-web dev` debounces, preflights, pushes the change into the DB.
3. On successful push, `pg-web dev` issues `NOTIFY pgweb_livereload, '{"kind":"full"}'` (or `'{"kind":"css"}'` for pure CSS changes).
4. The extension's LISTEN task forwards to every connected SSE client.
5. Each browser tab either cache-busts stylesheets (CSS-only change → no page reload, no flash) or calls `location.reload()` (anything else).

**What gets injected.** In dev mode, a `<script src="/_pgweb/livereload.js" async data-pgweb-livereload></script>` is spliced in right before `</body>` on every rendered HTML document. HTMX fragment responses (no `</body>` tag) are left alone — the OOB swap path is unaffected.

**Production mode is untouched.** `pgweb.settings.env = 'production'` disables the injection AND 404s the `/_pgweb/livereload` SSE endpoint. No connection leaks, no stray script downloads in prod.

**Opt out:** `pg-web dev --no-livereload` skips the NOTIFY broadcast (connected tabs just stay quiet). The script injection still happens in dev mode. This is useful when you're running a heavy-JS app whose in-page state would be lost on reload and you'd rather manually control when the refresh happens.

**Known limitations (bfcache / rapid navigation).** The live-reload client opens a persistent `EventSource` (SSE) connection. On apps with frequent full-page navigation or heavy use of the browser back/forward buttons, these connections can accumulate because of how browsers preserve pages in the back/forward cache (bfcache). The implementation includes defensive client-side cleanup (`pagehide`, `beforeunload`, `pageshow` + sentinel) plus a hard 2-hour server-side lifetime on the SSE stream, but very rapid navigation can still produce a handful of connections per tab.

If you see sluggish tabs or many open `/_pgweb/livereload` requests in DevTools during development, use `--no-livereload` and refresh manually. This is the recommended setting for complex client-side apps during active iteration. The feature is deliberately dev-only and has no effect in production.

**What about HTMX?** No dependency. If your app already uses HTMX, live-reload's `location.reload()` blows away whatever state HTMX was maintaining — same as any other full-page reload. Phase 2 will layer an HTMX-friendly morph path on top of the same SSE transport so partial state can survive refreshes; the current v0.1 client is deliberately minimal.

**Wiring diagram:**

```
  pg-web dev                pg_web_ext (BGW)           browser
  ──────────               ──────────────────         ─────────
  save ──▶ push ──▶ NOTIFY ──▶ LISTEN task
                                   │
                                   ▼
                              broadcast ch ──▶ SSE /_pgweb/livereload
                                                        │
                                                        ▼
                                              EventSource onmessage
                                                        │
                                                        ▼
                                        (css cache-bust | location.reload)
```

One LISTEN PG backend slot for the whole BGW — not per-tab. Browser tabs are HTTP/SSE only; they hold zero DB connections. Total cost in dev: **+1 Postgres backend**. In prod: **+0** (LISTEN task doesn't start when env is production).

## Pushing: `--dry-run`, `--with-migrate`, deployments ledger

`pg-web push` is transactional: it either commits every change together or leaves the live extension's state exactly as it was. Two flags shape when and how that commit happens.

**`pg-web push --dry-run`** runs every step — validation, upserts, reconciliation — inside the transaction, then rolls back instead of committing. The summary is tagged `[dry-run]` on every line so it's impossible to misread as a real push. Useful for CI pre-flight checks (does my branch push cleanly against staging?) and for "show me the plan" before a big deploy.

**`pg-web push --with-migrate`** detects any pending migrations in `migrations/` (compared against `pgweb.migrations`) and applies them before pushing. Without this flag, push refuses to run when migrations are pending — the "handler references a column that doesn't exist yet" class of bug is almost always "push preceded its migration." The error message names the pending files and points at `--with-migrate`.

```bash
# Normal local workflow — migrations and pushes are separate:
pg-web migrate apply
pg-web push

# Combined, for scripts / CI:
pg-web push --with-migrate

# Preview only — see what WOULD happen without touching anything:
pg-web push --with-migrate --dry-run
```

Every **committed** push appends a row to the `pgweb.deployments` ledger:

| column              | meaning                                                     |
|---------------------|-------------------------------------------------------------|
| `id`                | BIGSERIAL                                                   |
| `pushed_at`         | timestamptz; commit time (default `now()`)                  |
| `from_host`         | hostname of the machine running the CLI                     |
| `file_count`        | files from disk this push handled (routes + assets)         |
| `migrations_applied`| count applied in this push (0 if nothing pending)           |

Dry-run pushes do NOT append — the insert happens inside the rolled-back transaction. Ops queries:

```sql
-- When + from where did we last deploy?
SELECT pushed_at, from_host, file_count, migrations_applied
FROM pgweb.deployments
ORDER BY pushed_at DESC
LIMIT 5;
```

## Pre-commit / CI: `pg-web check`

`pg-web check` is an offline project validator — runs the same up-front checks that `pg-web push` does (layout, Tera parse, SQL syntax, migration filename rules) but without needing a DB. Drop it in a pre-commit hook or a CI gate; exit code is 0 clean, non-zero on any finding.

```bash
pg-web check
```

Passes when:
- `pages/` conforms to the layout spec (directory-as-route, reserved stems, no flat HTML at root).
- Every `.html` parses under Tera.
- Every `.sql` (handlers + migrations) parses under a Postgres dialect SQL parser **except** for two trusted categories in migrations (and harmlessly in handlers):
  - `COMMENT ON ...` statements using rich dollar-quoted (`$$...$$`) or adjacent string literals (high-quality self-documentation).
  - `CREATE EXTENSION ...` and `CREATE [UNIQUE] INDEX ...` statements, including extension opclass syntax such as `USING gin (col gin_trgm_ops)` (or GiST, SP-GiST, pgvector, PostGIS, etc.). These are valid PostgreSQL but outside the offline parser's grammar.
- Migration filenames have unique numeric prefixes (`0001_x.sql` / `0002_y.sql`…). Duplicate prefixes are flagged because filesystem order isn't guaranteed at migrate time.

Doesn't check:
- Semantic SQL (column existence, type agreement, RLS). Runtime PG does that at push / request time; this is a pre-push syntax gate.
- PL/pgSQL function bodies past the outer `CREATE FUNCTION` wrapper. Dollar-quoted bodies are opaque to the parser; `pg-web push` validates them when PG compiles the function.
- Extension DDL patterns listed above (by design — `pg-web migrate apply` is the source of truth).

**Policy for extension DDL:** If `pg-web check` emits a parser finding on a migration containing `CREATE EXTENSION` or opclass-bearing indexes but `pg-web migrate apply` (or `push --with-migrate`) succeeds cleanly, you are fine. The offline check is intentionally approximate for these cases so that real applications following the documented "migrations own schema and indexes" rule do not hit friction. Add a regression test in your own suite (or just rely on the framework's) and move on.

Opt-in DB check:

```bash
pg-web check --url "$DATABASE_URL"
```

adds a ledger-drift pass that compares local `migrations/*.sql` to `pgweb.migrations` in the DB. Flags files applied in the DB but deleted locally (historical record lost), and files present locally but not yet applied (push/migrate reminder). The default offline surface stays intact — everything above runs without `--url`.

Sample output on a broken app (typo case; extension-DDL findings are suppressed with guidance instead):

```
Migrations:
  ./migrations/0002_typo.sql: sql parser error: Expected: an SQL statement, found: CRATE at Line: 1, Column: 1 — if this is extension DDL (CREATE EXTENSION, GIN/GiST indexes with opclasses such as gin_trgm_ops, etc.), the SQL is likely valid; `pg-web migrate apply` against real Postgres is the source of truth.

Templates:
  ./pages/broken/index.html: Failed to parse 'index.html' — unclosed tag {% if %}

✗ 2 finding(s) — fix and re-run
```

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

## Implementation timeline (Phase 1 features — all shipped in v0.1.0 / v0.2.0)

| Feature                                                         | Lands in |
|-----------------------------------------------------------------|----------|
| ~~Hot reload (`pg-web dev`)~~                                   | M1.2 ✓   |
| ~~Dynamic route patterns (`[id]` capture → `req.path_params`)~~ | M1.2 ✓   |
| ~~Dev error page (typed catalog + rich SQL overlay)~~           | M1.2 ✓   |
| ~~Static asset serving (`public/*` → HTTP)~~                    | M1.2 ✓   |
| ~~`pgweb.html_escape()` SQL helper~~                            | M1.4 ✓   |
| ~~User-facing form validation UX (inline error via `check_violation`)~~ | M1.4 ✓   |
| ~~CLI `pg-web env set/unset/list` + `pgweb.setting()` helper~~  | M1.4 ✓   |
| ~~CLI `pg-web check` (offline project validator)~~              | M1.4 ✓   |
| ~~Browser live-reload push (SSE; auto-reload on save)~~         | M1.4 ✓   |
| Declarative schema-diffing (`migrate create`)                   | Phase 2.5 |
| Auth + sessions + RLS bridge                                    | Phase 2   |
| Async job queue                                                 | Phase 3   |
| In-browser dashboard / dev error overlay                        | Phase 4   |

Check `docs/ROADMAP.md` before building against a "not yet" feature.

## Where to go next

- **`docs/APP-LAYOUT.md`** — the exhaustive, spec-level reference for routing and file conventions. Use this when you want the rule on an edge case.
- **`docs/ARCHITECTURE.md`** — how the framework actually works under the hood.
- **`docs/ROADMAP.md`** — what's shipping when, and what's deliberately out of scope.
- **`examples/todo/`** — the companion todo app. Runs end-to-end; read it (and its README) to see every Phase 1 feature exercised together. It is the end state of the tutorial and the primary E2E target for the test suite.
