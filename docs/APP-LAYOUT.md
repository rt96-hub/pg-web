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
| `.sql` only            | Handler returns `text` → router sends bytes as-is.           | HTMX fragments; JSON APIs; no-content    |

## Phase 1 method filenames

Phase 1 supports **GET** and **POST** only. Other verbs land in Phase 2+.

| Filename stem | HTTP method | Notes                                                     |
|---------------|-------------|-----------------------------------------------------------|
| `index`       | GET         | The one GET spelling. Matches Apache/Nginx web tradition. |
| `post`        | POST        |                                                           |
| `put`         | PUT         | Reserved — rejected until Phase 2+.                       |
| `patch`       | PATCH       | Reserved — rejected until Phase 2+.                       |
| `delete`      | DELETE      | Reserved — rejected until Phase 2+.                       |
| `get`         | —           | Reserved. Use `index` for GET; `push` rejects `get.*`.    |
| `head`        | —           | Reserved. Auto-derived from GET later.                    |
| `options`     | —           | Reserved.                                                 |

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

### HTMX todo app

```
pages/
├── index.html              GET /                 app shell + initial list
├── index.sql               GET /                 handler: SELECT todos
├── todos/
│   ├── post.html           POST /todos           fragment template (new <li>)
│   └── post.sql            POST /todos           handler: INSERT, returns JSON
├── todos/toggle/
│   └── post.sql            POST /todos/toggle    handler: UPDATE, returns text <li>
└── todos/delete/
    └── post.sql            POST /todos/delete    handler: DELETE, returns text ''
```

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
  "body":   { "title": "buy milk" },   // parsed application/x-www-form-urlencoded; {} if empty
  "query":  { "page": "2" },           // parsed query string; {} if empty
  "method": "POST",                    // HTTP method, uppercase
  "path":   "/todos"                   // URL path
}
```

`body` and `query` are always objects — never `null`. `req->'body'->>'title'` is always safe to write; returns NULL for missing keys.

### Return type → pipeline dispatch

The router picks the pipeline from the route row's `template_path` (populated by `push` based on filesystem):

| `template_path` column | Handler must return | Router sends            |
|------------------------|---------------------|-------------------------|
| non-NULL               | `json`              | Tera(template, context) |
| NULL                   | `text`              | handler's text verbatim |

`pg-web push` verifies that handler return types match the filesystem mode before committing the transaction. Mismatches are loud errors, not silent coercion.

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

## When to pick which mode

- **Static content** (about, contact, marketing) → `.html` only. Zero SQL overhead, nothing to break.
- **Any page showing DB data** → `.html` + `.sql`. Always safe by default (Tera auto-escape).
- **HTMX fragment with dynamic values** → `.html` + `.sql`. Tera escaping protects you.
- **HTMX fragment that's a literal string, or a delete returning nothing** → `.sql` only.
- **JSON API endpoint** → `.sql` only, `RETURNS text` producing JSON. (First-class JSON content-type support may land in Phase 1.4.)

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

## Migration from M1.1 flat layout

M1.1 allowed flat files: `pages/about.html` → `/about`. Phase 1 (this spec) requires a directory: `pages/about/index.html`. The scaffold produced by `pg-web init` and the demo app migrate accordingly in the same PR that lands this spec.

No silent compatibility: `pg-web push` rejects flat `.html` files under `pages/` with a clear error pointing to this doc.
