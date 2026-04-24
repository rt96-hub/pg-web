# pg-web Tutorial — Build a Todo List

You'll build a working HTMX-driven todo app from scratch. Add, toggle, and delete todos with no full-page reloads. Every click round-trips through Postgres and renders server-side.

**What you're going to end up with:** the same app that lives at `examples/todo/` in this repo. If you get stuck, that's your reference.

**Time:** 20–40 minutes.

**Audience:** you know SQL, some HTML, and you've run Docker before. You don't need to know Rust, pgrx, or anything Postgres-extension-specific.

---

## Before you start

You need:

1. **Docker.** `docker --version` should work.
2. **The `pg-web` CLI.** For now, that means cloning this repo and building it:
   ```bash
   git clone https://github.com/<you>/pg-web.git
   cd pg-web
   cargo build -p pg_web_cli                 # produces target/debug/pg-web
   bash scripts/build-image.sh               # builds pgweb/postgres:latest, ~5–10 min cold
   ```
   From here on the tutorial assumes `pg-web` is on your `$PATH` (or substitute `./target/debug/pg-web` in commands below). Pre-built binaries + `cargo install pg-web-cli` come with v0.1.
3. **A terminal.** That's it. No Node, Python, Go, or anything else.

---

## Step 1 — Scaffold a new app

```bash
cd /tmp                # or wherever you keep projects
pg-web init my-todos
cd my-todos
```

You get a directory that looks like:

```
my-todos/
├── pages/
│   ├── index.html
│   └── index.sql
├── migrations/        (empty, except .gitkeep)
├── public/            (empty, except .gitkeep)
├── pgweb.toml
├── docker-compose.yml
├── Caddyfile
└── .gitignore
```

Nothing special yet — a hello-world page.

## Step 2 — Boot the stack

```bash
docker compose up -d
```

This starts Postgres with the `pg_web_ext` extension preloaded. The HTTP server inside the extension listens on `:8080`.

Push the scaffolded routes in:

```bash
export DATABASE_URL="postgres://postgres:devpassword@localhost:5432/app"
pg-web push --url "$DATABASE_URL"
```

You should see:

```
✓ pushed — 1 routes, 1 templates, 1 SQL files
```

Visit `http://localhost:8080/` (or `curl` it). You get "Welcome to my-todos" rendered by the scaffolded template. Loop confirmed.

## Step 3 — Add the schema

Create a migration file:

**`migrations/0001_create_todos.sql`**
```sql
CREATE TABLE public.todos (
    id         bigserial PRIMARY KEY,
    title      text NOT NULL CHECK (length(trim(title)) > 0),
    done       boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX todos_created_at_idx ON public.todos (created_at DESC);
```

Apply it:

```bash
pg-web migrate apply --url "$DATABASE_URL"
```

Output:

```
✓ applied 0001_create_todos.sql
1 applied, 0 skipped
```

Re-run the command — you'll see `0 applied, 1 skipped`. Migrations are idempotent: each file runs exactly once, tracked in the `pgweb.migrations` ledger.

## Step 4 — Render the (empty) list

Replace the scaffolded `pages/index.html` and `pages/index.sql` with a list view.

**`pages/index.html`**
```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Todos</title>
  <script src="https://unpkg.com/htmx.org@1.9.12"></script>
  <style>
    body { font-family: system-ui, sans-serif; max-width: 520px; margin: 2rem auto; padding: 0 1rem; }
    ul { list-style: none; padding: 0; margin: 0; }
    li { padding: 0.6rem 0; border-bottom: 1px solid #eee; }
    .empty { color: #888; font-style: italic; }
  </style>
</head>
<body>
  <h1>Todos</h1>

  <ul id="todos">
    {% for todo in todos %}
      <li>{{ todo.title }}</li>
    {% else %}
      <li class="empty">No todos yet.</li>
    {% endfor %}
  </ul>
</body>
</html>
```

**`pages/index.sql`**
```sql
CREATE OR REPLACE FUNCTION pgweb.pages__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'todos', COALESCE(
      (SELECT json_agg(
         json_build_object('id', id, 'title', title, 'done', done)
         ORDER BY id
       ) FROM public.todos),
      '[]'::json
    )
  )
$$ LANGUAGE sql STABLE;
```

A few notes:

- **Handler signature is uniform.** Every handler is `pgweb.pages__<name>(req json) RETURNS <json|text>`. Even when you don't read from `req`, the argument is required.
- **The handler name mirrors the filename.** `pages/index.sql` → `pgweb.pages__index`. Nested dirs use `__` as the separator: `pages/todos/index.sql` → `pgweb.pages__todos__index`.
- **Empty arrays matter.** `json_agg` over zero rows returns NULL, which would break the `{% for %}` in Tera. `COALESCE` to an empty array keeps the template path clean.

Push it:

```bash
pg-web push --url "$DATABASE_URL"
```

Refresh `http://localhost:8080/`. You see "No todos yet." — the empty branch fires.

## Step 5 — Add the "create todo" flow

We need a POST endpoint. Directory + method-named files:

**`pages/todos/post.html`** — the fragment that'll be appended to the list for each new row:

```html
<li>{{ todo.title }}</li>
```

**`pages/todos/post.sql`** — INSERT and return the new row:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__todos__post(req json) RETURNS json AS $$
  INSERT INTO public.todos (title)
  VALUES (NULLIF(trim(req->'body'->>'title'), ''))
  RETURNING json_build_object(
    'todo', json_build_object('id', id, 'title', title, 'done', done)
  )
$$ LANGUAGE sql;
```

Notice how we read the form body: `req->'body'->>'title'`. HTMX submits `application/x-www-form-urlencoded` by default, and the framework parses that into `req.body` before your handler runs.

Wire up the form in the index template — add this block just above the `<ul>`:

```html
<form hx-post="/todos"
      hx-target="#todos"
      hx-swap="beforeend"
      hx-on::after-request="if(event.detail.successful) this.reset()">
  <input type="text" name="title" placeholder="What needs done?" required autofocus>
  <button>Add</button>
</form>
```

The `hx-post="/todos"` posts to your new handler. `hx-target="#todos"` + `hx-swap="beforeend"` tells HTMX to append the response fragment into the `<ul id="todos">`. `hx-on::after-request` resets the form on success.

Push and try it:

```bash
pg-web push --url "$DATABASE_URL"
```

Type something in the form and hit Add. The new `<li>` appears at the bottom without a page reload.

### What just happened?

- Browser POSTs `/todos` with body `title=buy+milk`.
- Extension parses the body, constructs `req = { "body": {"title":"buy milk"}, "query": {}, "method":"POST", "path":"/todos" }`.
- Calls `pgweb.pages__todos__post(req)` — returns `{"todo":{"id":1,"title":"buy milk","done":false}}`.
- Router sees `template_path` is set (there's a `post.html` sibling), so it renders that template with the handler's JSON as the Tera context: `<li>buy milk</li>`.
- Response sent to HTMX, which appends it to the list.

Reload the page fresh — the list persists because the data is in Postgres, not anywhere transient.

### What if the title is empty?

Try submitting a single space. (Purely-empty submissions are blocked by the input's `required` attribute in the browser; whitespace gets through.) You'll get a 500 error page.

That's because the `CHECK (length(trim(title)) > 0)` constraint in the migration bounces the insert, and the handler has no catch. The framework surfaces it as `PGWEB_E003_HANDLER_SQL_EXCEPTION` in dev mode and a generic 500 in production — neither is what a user should see. Let Postgres validate; catch the exception in the handler and return an inline error.

Rewrite `pages/todos/post.sql` as a PL/pgSQL function with an `EXCEPTION` block:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__todos__post(req json) RETURNS json AS $fn$
DECLARE
  v_title text := trim(COALESCE(req->'body'->>'title', ''));
  v_id    bigint;
  v_done  boolean;
BEGIN
  INSERT INTO public.todos (title)
  VALUES (v_title)
  RETURNING id, done INTO v_id, v_done;

  RETURN json_build_object(
    'success', true,
    'todo',    json_build_object('id', v_id, 'title', v_title, 'done', v_done)
  );
EXCEPTION WHEN check_violation THEN
  RETURN json_build_object(
    'success', false,
    'error',   'Title cannot be empty.'
  );
END;
$fn$ LANGUAGE plpgsql;
```

Update `pages/todos/post.html` so the template dispatches on the `success` flag:

```html
{%- if success -%}
<li>{{ todo.title }}</li>
<div id="form-error" hx-swap-oob="true"></div>
{%- else -%}
<div id="form-error" hx-swap-oob="true">{{ error }}</div>
{%- endif -%}
```

Finally, give the error somewhere to land — add an empty `<div id="form-error">` below the form in `pages/index.html`:

```html
<form ...> ... </form>

<div id="form-error"></div>

<ul id="todos"> ... </ul>
```

Push and try the empty / whitespace submission again. The error renders inline next to the form — no page reload, no 500.

**How it works:**

- `BEGIN ... EXCEPTION WHEN check_violation` is PL/pgSQL's try/catch. Any SQLSTATE `23514` raised inside the block is caught.
- The handler always returns a JSON object. On success, `{success: true, todo: {...}}`; on failure, `{success: false, error: "..."}`. Template dispatches on `success`.
- On success, the template emits the new `<li>` (appended via `hx-swap="beforeend"` into `#todos`) AND an empty `<div id="form-error" hx-swap-oob="true"></div>` that clears any prior error.
- On failure, the template emits only the error div. HTMX sees `hx-swap-oob="true"` with matching `id="form-error"` in the DOM, does an outerHTML swap there, and the main target gets nothing to append.
- One response, two swap destinations — no extra round trip.

Other exception classes follow the same shape: `WHEN unique_violation` for duplicate-key, `WHEN foreign_key_violation` for missing parent rows, and so on. Each can return a tailored error message.

## Step 6 — Toggle

Each row gets Toggle/Delete buttons. First, update the index + fragment templates to render the buttons. Replace `<li>{{ todo.title }}</li>` in both `pages/index.html` and `pages/todos/post.html` with:

```html
<li class="{% if todo.done %}done{% endif %}">
  <span class="title">{{ todo.title }}</span>
  <span class="actions">
    <button hx-post="/todos/toggle"
            hx-vals='{"id": {{ todo.id }}}'
            hx-target="closest li"
            hx-swap="outerHTML">Toggle</button>
    <button hx-post="/todos/delete"
            hx-vals='{"id": {{ todo.id }}}'
            hx-target="closest li"
            hx-swap="outerHTML">Delete</button>
  </span>
</li>
```

And add a style block for the `done` state:

```html
<style>
  li.done .title { color: #888; text-decoration: line-through; }
  /* ...your other styles... */
</style>
```

Now the toggle route. **`pages/todos/toggle/post.html`**:

```html
<li class="{% if todo.done %}done{% endif %}">
  <span class="title">{{ todo.title }}</span>
  <span class="actions">
    <button hx-post="/todos/toggle"
            hx-vals='{"id": {{ todo.id }}}'
            hx-target="closest li"
            hx-swap="outerHTML">Toggle</button>
    <button hx-post="/todos/delete"
            hx-vals='{"id": {{ todo.id }}}'
            hx-target="closest li"
            hx-swap="outerHTML">Delete</button>
  </span>
</li>
```

(Same markup as `post.html` — the toggled row is the same shape as a new one. In a larger app you'd extract this to a Tera partial via `{% include %}`; for this tutorial we just duplicate.)

**`pages/todos/toggle/post.sql`**:

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__todos__toggle__post(req json) RETURNS json AS $$
  UPDATE public.todos
  SET done = NOT done
  WHERE id = (req->'body'->>'id')::bigint
  RETURNING json_build_object(
    'todo', json_build_object('id', id, 'title', title, 'done', done)
  )
$$ LANGUAGE sql;
```

Push and try — Toggle strikes the row through.

## Step 7 — Delete (raw text mode)

Delete returns no body — HTMX just needs to know the request succeeded, then `hx-swap="outerHTML"` on an empty response removes the `<li>` from the list. So we don't need a template at all.

**`pages/todos/delete/post.sql`** (note: no sibling `.html`):

```sql
CREATE OR REPLACE FUNCTION pgweb.pages__todos__delete__post(req json) RETURNS text AS $$
  DELETE FROM public.todos WHERE id = (req->'body'->>'id')::bigint;
  SELECT ''::text;
$$ LANGUAGE sql;
```

Two things to notice:

- **`RETURNS text`, not `json`.** Because there's no sibling `.html`, the framework sends whatever the handler returns as the HTTP response body. No Tera involvement.
- **Two statements, last wins.** `DELETE FROM ...;` then `SELECT ''::text;` — the function returns the empty string regardless of whether the DELETE matched a row. Double-clicking Delete is idempotent.

Push. Delete buttons now work.

## Step 8 — Custom 404

Try visiting `http://localhost:8080/something-random`. You get a default "404 — Not found" page shipped by the framework. Let's replace it with your own.

**`pages/_404.html`**:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Not found</title>
  <style>
    body { font-family: system-ui; max-width: 500px; margin: 5rem auto; text-align: center; }
    h1 { font-size: 5rem; margin: 0; color: #bbb; }
  </style>
</head>
<body>
  <h1>404</h1>
  <p>That page doesn't exist.</p>
  <p><a href="/">Back to todos</a></p>
</body>
</html>
```

`_404` is a reserved filename stem — the framework recognizes it and uses that template (status 404) for any URL that doesn't match a declared route. It's a static page in this case (no sibling `.sql`), so no handler runs.

Push. Visit a random URL. Custom 404.

## You're done

Recap:

- One migration file defines your schema; `pg-web migrate apply` tracks what's been run.
- `pg-web push` syncs routes, templates, and handler functions from `pages/` into Postgres. Idempotent — run it whenever you edit a file.
- Every route directory has method-named files: `index` = GET, `post` = POST.
- The handler signature is always `(req json) RETURNS <json|text>`. `req = { body, query, method, path }` with `body`/`query` as objects.
- Three modes: static (template only), dynamic (template + SQL → Tera), raw text (SQL only → bytes-as-is).

## Where to go next

- **[`examples/todo/`](../examples/todo/)** — the reference version of what you just built. Exact file-for-file match if you followed along. Clone + diff if you got stuck.
- **[`docs/APP-LAYOUT.md`](./APP-LAYOUT.md)** — the exhaustive spec. Edge cases, reserved stems, the naming derivation rules.
- **[`docs/APP-DEVELOPER-GUIDE.md`](./APP-DEVELOPER-GUIDE.md)** — the narrative app-dev reference you can skim for patterns (forms with validation, HTMX conventions, configuration).
- **[`docs/ROADMAP.md`](./ROADMAP.md)** — what's coming. Hot reload, dynamic routes (`[id]` captures), auth, async jobs.

### Clean up

```bash
docker compose down --volumes    # stops the stack and drops the data
```
