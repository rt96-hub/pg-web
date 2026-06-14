# pg-web — App Layout

**Audience:** app developers (people writing `.sql` + `.html`). Framework maintainers should read this first too — `paths.rs` and `push.rs` are the mechanical encoding of these rules.

## TL;DR

A pg-web app is a tree of files under `pages/`. Three rules:

1. **Directory = route.** `pages/todos/toggle/` → `/todos/toggle`.
2. **Filename = HTTP method.** `index` (GET alias), `post`, later `put` / `patch` / `delete`.
3. **Each method has two files: `<method>.html` (template) + `<method>.sql` (handler). Either is optional.**

Which half you ship determines the pipeline:

| Files present          | Pipeline                                                     | Use for                                  |
|------------------------|--------------------------------------------------------------|------------------------------------------|
| `.html` only           | Render template with empty context `{}`. No SPI call.        | Static marketing / about / contact pages |
| `.html` + `.sql`       | Handler returns `json` → Tera renders template with it.      | Most pages; auto-escape safe by default  |
| `.sql` only            | Handler returns `text` (verbatim) *or* a response envelope → router applies status/headers/cookies/ct + body. | HTMX fragments; JSON APIs; redirects; no-content |

## Method filenames (directory-as-route)

Every method is expressed as a filename stem under its route directory. `index` is the GET spelling (by convention); the rest are literal.

| Filename stem | HTTP method | Notes |
|---------------|-------------|-------|
| `index`       | GET         | The conventional GET handler for the directory. |
| `post`        | POST        | |
| `put`         | PUT         | |
| `patch`       | PATCH       | |
| `delete`      | DELETE      | |
| `get`         | —           | Reserved. Use `index` for GET; `pg-web push` rejects `get.*`. |
| `head`        | HEAD        | **Auto-derived** (never author a `head.sql`). The server resolves the matching GET route/asset and returns identical headers with an empty body. |
| `options`     | OPTIONS     | **Auto-derived** (never author an `options.sql`). The server returns 204 with `Allow:` listing the methods that have rows (or implicit GET/HEAD for assets) for a matching path pattern, plus OPTIONS itself. HEAD is included when GET exists. |

Filenames are case-sensitive lowercase. `POST.html` is not recognized. HEAD/OPTIONS have no authorable files to avoid divergence from their GET twins.

Filenames are case-sensitive lowercase. `POST.html` is not recognized.

## Worked examples

### Pure static site

```
pages/
├── index.html              GET /        (static)
├── about/
│   └── index.html          GET /about   (static)
└── contact/
    └── index.html          GET /contact (static)
```

Zero SQL, zero migrations. `pg-web push` uploads three rows into `pgweb.templates` and three into `pgweb.routes`.

### Blog with dynamic index

```
pages/
├── index.html              GET /        (static welcome)
└── posts/
    ├── index.html          GET /posts   list template
    └── index.sql           GET /posts   handler: SELECT posts → JSON
```

### HTMX todo app (with real DELETE via method stem)

```
pages/
├── index.html              GET /                 app shell + initial list
├── index.sql               GET /                 handler: SELECT todos
├── todos/
│   ├── post.html           POST /todos           fragment template (new <li>)
│   └── post.sql            POST /todos           handler: INSERT, returns JSON
├── todos/toggle/
│   ├── post.html           POST /todos/toggle    fragment template (updated <li>)
│   └── post.sql            POST /todos/toggle    handler: UPDATE, returns text <li>
└── todos/[id]/
    ├── index.html          GET /todos/:id        detail template
    ├── index.sql           GET /todos/:id        handler: SELECT one (capture in path_params)
    └── delete.sql          DELETE /todos/:id     handler: DELETE, returns '' (text mode)
```

The delete button in the list (and the fragments returned by post/toggle) now uses the idiomatic:

```html
<button hx-delete="/todos/{{ todo.id }}"
        hx-target="closest li"
        hx-swap="outerHTML">Delete</button>
```

(See examples/todo/ for the exact buttons + the explanatory comments in `[id]/delete.sql`.) The old `POST /todos/delete` + body-id workaround (and its `todos/delete/post.sql`) has been removed.

## Naming derivation (exact)

Given a file `pages/<segments...>/<stem>.<ext>`:

- **URL path** = `/` + `<segments>` joined by `/`. Drop `index` when it's the leaf stem — that's the "this directory's GET" convention.
  - `pages/index.html`                           → `/`
  - `pages/todos/index.html`                     → `/todos`
  - `pages/todos/post.sql`                       → `/todos`
  - `pages/todos/toggle/post.sql`                → `/todos/toggle`
- **HTTP method** = `<stem>` mapped through the filename table above (`index` → `GET`, `post` → `POST`, etc.).
- **Template path** (row key in `pgweb.templates`) = the literal `pages/<segments...>/<stem>.html`. Preserved verbatim.
- **Handler function name** = `pgweb.pages__<segments_joined_with_double_underscore>__<stem>`.
  - `pages/index.sql`                            → `pgweb.pages__index`
  - `pages/todos/index.sql`                      → `pgweb.pages__todos__index`
  - `pages/todos/post.sql`                       → `pgweb.pages__todos__post`
  - `pages/todos/toggle/post.sql`                → `pgweb.pages__todos__toggle__post`

Filesystem slashes and backslashes both normalize to `/`; the handler name uses `__`.

## Dynamic segments

A directory name wrapped in brackets is a **capture**: it matches any single URL segment, and the captured string is threaded into the handler's `req.path_params`.

```
pages/posts/[id]/index.html              # GET /posts/:id  — capture "id"
pages/users/[user]/posts/[post]/index.sql# GET /users/:user/posts/:post — two captures
```

**Syntax rules (enforced at `pg-web push` time):**

- The bracket pair must enclose the whole directory name. `foo[id]`, `[id]bar`, or nested `[[id]]` are errors.
- The capture name inside the brackets must match `^[A-Za-z_][A-Za-z0-9_]*$` and be ≤ 63 characters.
- `[` and `]` are reserved anywhere in a path segment: a static directory name containing brackets is rejected.

**What the scanner emits:**

| Filesystem | Route pattern (DB) | Handler function name |
|---|---|---|
| `pages/posts/[id]/index.sql` | `/posts/:id` | `pgweb.pages__posts__$id__index` |
| `pages/users/[user]/posts/[post]/index.sql` | `/users/:user/posts/:post` | `pgweb.pages__users__$user__posts__$post__index` |

The `$name` in the handler function name is the capture marker. `$` is a legal character inside Postgres identifiers (after the first position) so it keeps the name SQL-valid while staying visually distinct from any literal directory name a user might use.

**Captures are always strings.** `/posts/123` and `/posts/all` both match `[id]`. The handler reads `req->'path_params'->>'id'` and casts / validates as needed:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__posts__$id__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'id',   req->'path_params'->>'id',
    'post', (
      SELECT to_json(p) FROM (
        SELECT id, title FROM posts
        WHERE id::text = req->'path_params'->>'id'
      ) p
    )
  )
$$ LANGUAGE sql STABLE;
```

The `id::text = req->'path_params'->>'id'` pattern keeps it tolerant: a non-numeric URL segment (like `/posts/all`) simply matches no rows, so `post` comes back NULL and the template renders a not-found branch.

**Specificity: static beats dynamic.** If both `pages/posts/new/index.html` and `pages/posts/[id]/index.html` exist, `GET /posts/new` resolves to the static handler; `GET /posts/42` resolves to the dynamic one. The router sorts patterns by (static-segment count desc, capture-segment count asc, length desc) and takes the first match.

## Handler contract

Every `.sql` handler must be a function in the `pgweb` schema taking a single `json` argument:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__<name>(req json)
  RETURNS <return_type>
  LANGUAGE <lang>
AS $$ ... $$;
```

### The `req` argument

Every handler receives the same shape:

```json
{
  "body":        { "title": "buy milk" },   // parsed application/x-www-form-urlencoded; {} if empty
  "query":       { "page": "2" },           // parsed query string; {} if empty
  "method":      "POST",                    // HTTP method, uppercase
  "path":        "/todos/42",               // URL path (after capture, not pattern)
  "path_params": { "id": "42" }             // captures from dynamic segments; {} if static route
}
```

`body`, `query`, and `path_params` are always objects — never `null`. `req->'body'->>'title'`, `req->'path_params'->>'id'`, etc. are always safe to write; they return NULL for missing keys.

### Return type → pipeline dispatch (v1 + response contract v2)

The router picks the pipeline from the route row's `template_path`:

| `template_path` column | Handler declared return          | Router behavior |
|------------------------|----------------------------------|-----------------|
| non-NULL (template)    | `json` (envelope or bare context)| Tera render (or literal body if envelope supplies one) + envelope attrs |
| NULL (raw)             | `text` (verbatim body)           | bytes as-is + envelope attrs if present |
| NULL (raw)             | `json` (envelope or plain JSON)  | envelope (status/ct/headers/cookies) or verbatim JSON text |

`pg-web push` accepts:
- template routes: only `RETURNS json`
- raw routes: `RETURNS text` **or** `RETURNS json` (the latter enables envelopes / `pgweb.json` etc.)

The router detects the v2 envelope at runtime by the presence of a top-level `"$pgweb"` key in the returned JSON. No marker = legacy byte-for-byte behavior.

See the four helpers below.

### Handler examples

```sql
-- GET /todos — JSON return, renders pages/todos/index.html
CREATE OR REPLACE FUNCTION pgweb.pages__todos__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'todos', COALESCE(
      (SELECT json_agg(row_to_json(t) ORDER BY t.id) FROM todos t),
      '[]'::json
    )
  )
$$ LANGUAGE sql STABLE;

-- POST /todos — JSON return, renders pages/todos/post.html (HTMX fragment)
CREATE OR REPLACE FUNCTION pgweb.pages__todos__post(req json) RETURNS json AS $$
  INSERT INTO todos (title)
  VALUES (NULLIF(req->'body'->>'title', ''))
  RETURNING json_build_object('id', id, 'title', title, 'done', done)
$$ LANGUAGE sql;

-- POST /todos/toggle — text return, router sends as-is
-- Handler is responsible for HTML escaping (use Tera via a template whenever possible).
CREATE OR REPLACE FUNCTION pgweb.pages__todos__toggle__post(req json) RETURNS text AS $$
  UPDATE todos SET done = NOT done
  WHERE id = (req->'body'->>'id')::bigint
  RETURNING format(
    '<li class="%s">%s</li>',
    CASE WHEN done THEN 'done' ELSE '' END,
    /* TODO html_escape helper lands in Phase 1.4 */ title
  )
$$ LANGUAGE sql;
```

### Response contract v2 (status, headers, cookies, redirects, content-type)

A handler may optionally return a JSON *envelope* instead of a bare body. The envelope is recognized by its reserved top-level key `"$pgweb"`. All legacy handlers (no such key) continue to work identically.

Ergonomic helpers live in the `pgweb` schema (installed by the extension):

```sql
-- 303 redirect (Post-Redirect-Get)
SELECT pgweb.redirect('/todos');

-- JSON API with correct content type (raw-text route)
SELECT pgweb.json(jsonb_build_object('todos', (SELECT json_agg(...) FROM todos)));

-- General case + cookie (e.g. future login)
SELECT pgweb.respond(
  '', 303,
  jsonb_build_object('Location', '/dashboard'),
  NULL,
  jsonb_build_array( pgweb.set_cookie('pgweb_session', 'abc123', '{"http_only":true,"same_site":"Lax"}') )
);

-- Set a custom header or cache hint on a rendered page (template route can still return an envelope)
SELECT pgweb.respond( (SELECT json_build_object('todos', ...)), 200,
  jsonb_build_object('Cache-Control', 'no-cache'),
  NULL, '[]'::jsonb );
```

Cookie defaults (per the Phase 2 auth spec): `HttpOnly` + `SameSite=Lax` on by default; `Secure` only when `pgweb.settings.env = 'production'`. The caller can override `http_only` (required for the JS-readable CSRF double-submit cookie).

See `pgweb.respond`, `pgweb.redirect`, `pgweb.json`, `pgweb.set_cookie` for signatures and more examples. The wire shape under `"$pgweb"` is an implementation detail; never construct it by hand.

A template-mode route can return an envelope that both sets headers/cookies *and* renders its Tera template (put the context under the envelope's `"context"` key, or omit it and supply a literal `"body"` to bypass rendering).

## When to pick which mode

- **Static content** (about, contact, marketing) → `.html` only. Zero SQL overhead, nothing to break.
- **Any page showing DB data** → `.html` + `.sql`. Always safe by default (Tera auto-escape).
- **HTMX fragment with dynamic values** → `.html` + `.sql`. Tera escaping protects you.
- **HTMX fragment that's a literal string, or a delete returning nothing** → `.sql` only.
- **JSON API endpoint** → `.sql` only. With response contract v2 you `RETURNS json` and call `pgweb.json(payload)` (or the general `pgweb.respond`) to get `Content-Type: application/json` + any status/headers. Bare `RETURNS text` still works (legacy verbatim, served as text/html unless you use the envelope). See handler contract below.

## What `pg-web push` writes

For the whole `pages/` tree, one transaction:

- One row into `pgweb.templates(template_path, content)` per `.html` file.
- One row into `pgweb.routes(method, path_pattern, handler_name, template_path)` per `.sql` handler.
  - `template_path` is NULL when no sibling `.html` exists → raw-text dispatch.
  - `template_path` is non-NULL when a sibling `.html` exists → Tera dispatch.

Routes for static-only pages (HTML with no SQL) are synthesized by `push`: it installs a trivial handler that returns `'{}'::json` and sets `template_path` to the `.html` file. One row in routes, one in templates, no user-authored SQL needed.

## Caveats

- **No method conflict per directory.** A directory may have at most one GET artifact — either `index.html` / `index.sql` *or* (future, rejected today) `get.*`. `push` errors on conflict.
- **Static + handler.** `.html` without a sibling `.sql` is allowed (static mode). `.sql` without a sibling `.html` is allowed (text mode). Both together is the default.
- **`public/` ≠ `pages/`.** Static assets (CSS/JS/images) live under `public/` and are served by a different mechanism. That tree has no routing conventions (every file under `public/` is a literal URL).
- **Reserved stems.** See the filename table. Don't name a file `get.sql`, `head.html`, etc.
- **Casing.** All filename stems must be lowercase. `Index.html` is not recognized.

## Migrations vs push — two different things

`pg-web migrate apply` and `pg-web push` are separate commands with separate state models. Don't confuse them.

| Command        | Manages                                      | Storage                                              | State model               |
|----------------|----------------------------------------------|------------------------------------------------------|---------------------------|
| `migrate apply` | App DDL/DML (your tables, columns, seed data) | `migrations/*.sql` + `pgweb.migrations` ledger        | Append-only history       |
| `push`          | Routes + templates + handler functions        | `pages/**/*.{html,sql}` + `pgweb.routes`/`.templates` | Fully replaced each time  |

Routes and HTML are **not migrations.** Push idempotently overwrites them to match the current filesystem. Migrations are forward-only — once a `.sql` file's name is in `pgweb.migrations`, that file is never re-run (even if its contents changed — Phase 1 has no checksum).

Framework schema (`pgweb.routes`, `pgweb.templates`, `pgweb.migrations` themselves) is installed by `CREATE EXTENSION pg_web_ext` — neither command creates it. That DDL lives frozen inside the extension's `.so`.

Normal deploy loop:

```bash
pg-web migrate apply --url "$DATABASE_URL"   # advance app schema (forward-only)
pg-web push          --url "$DATABASE_URL"   # replace routes/templates/handlers
```

Run in that order — push assumes the tables the handlers touch exist.

## Migration from M1.1 flat layout

M1.1 allowed flat files: `pages/about.html` → `/about`. Phase 1 (this spec) requires a directory: `pages/about/index.html`. The scaffold produced by `pg-web init` and the demo app migrate accordingly in the same PR that lands this spec.

No silent compatibility: `pg-web push` rejects flat `.html` files under `pages/` with a clear error pointing to this doc.
