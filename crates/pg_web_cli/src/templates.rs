//! Template files rendered into a new app directory on `pg-web init`.
//!
//! All small; keep them inline rather than `include_str!`ing from a
//! `templates/` dir. Each constant is the full file content. The `{APP}`
//! placeholder is the literal string to be replaced with the app name
//! at render time — no real templating engine needed for the CLI side.

pub const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{{ title }}</title>
</head>
<body>
  <h1>Welcome to {{ app_name }}</h1>
  <p>Edit <code>pages/index.html</code> and <code>pages/index.sql</code> to change this page.</p>
  <p>Run <code>pg-web dev</code> to auto-push on save.</p>
</body>
</html>
"#;

pub const INDEX_SQL: &str = r#"-- Handler for GET /
--
-- Contract: every handler takes a single `req json` argument and returns
-- either `json` (rendered through the sibling `.html` via Tera) or `text`
-- (sent as-is; no sibling `.html`).
--
-- `req` shape:
--   { "body":   { ...parsed form fields... },
--     "query":  { ...parsed query string... },
--     "method": "GET", "path": "/" }
--
-- Access fields: req->'body'->>'key', (req->'body'->>'n')::int, etc.
-- See docs/APP-LAYOUT.md for the full contract.

CREATE OR REPLACE FUNCTION pgweb.pages__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'title',    '{APP}',
    'app_name', '{APP}'
  )
$$ LANGUAGE sql STABLE;
"#;

pub const PGWEB_TOML: &str = r#"# pg-web app config

[server]
# Port the extension's HTTP server binds inside the Postgres container.
port = 8080
# "development" gives rich error pages; "production" returns generic 500s.
env  = "development"
# Per-request statement timeout (prompt 014). Bounds every handler and
# internal lookup so one slow query cannot wedge the whole site.
# The worker does SET LOCAL statement_timeout = '...' inside the request tx.
# request_timeout = "15s"  # default in extension if omitted; override here and re-push

# Health & readiness (prompt 018.1). The framework seeds working defaults
# for the conventional public endpoints GET /health and GET /readiness so
# a fresh `pg-web init` (or bare CREATE EXTENSION) is immediately useful for
# simple probes. These defaults are *overridable*:
#   - Create pages/health/index.sql (+ optional .html) per APP-LAYOUT.md
#     and push — your route row replaces the seeded one completely.
#   - Set the flag false to suppress *only the framework default* (public
#     /health then falls through to normal 404 / your _404). User routes for
#     the path are never suppressed by the flag.
# The protected platform probes (`/_pgweb/health`, `/_pgweb/readiness`) are
# always available, never overridable, and are the ones the Dockerfile
# HEALTHCHECK and load balancers should target.
# health_enabled = true
# readiness_enabled = true

[database]
# Name of the environment variable holding your Postgres connection string.
url_env = "DATABASE_URL"

[dev]
# Directories `pg-web dev` watches for changes (M1.2+).
watch_paths = ["pages", "public"]
"#;

pub const DOCKER_COMPOSE: &str = r#"# One-container pg-web stack.
#
# The `postgres` service uses the official published runtime image
# `rtaylor96/pg-web:latest` (Postgres 17 + the pg_web_ext extension + the
# pg-web CLI baked in). After `cargo install pg-web` on a brand new machine
# you can run `pg-web up` — Docker will automatically pull the image.
# 
# NOTE: This is using rtaylor96's personal namespace temporarily.
# Once the official `pgweb` Docker Hub organization is available,
# this will change to `pgweb/pg-web` or `pgweb/postgres`.
#
# Dev: direct access on :8080.
# Prod: uncomment the `caddy` service below and set your domain in Caddyfile.

services:
  postgres:
    image: rtaylor96/pg-web:latest  # temporary - will become pgweb/pg-web or pgweb/postgres later
    restart: unless-stopped
    environment:
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-devpassword}
      POSTGRES_DB: app
    ports:
      - "5432:5432"   # for `pg-web push` over the wire
      - "8080:8080"   # the app itself (dev only — remove in prod, route via Caddy)
    volumes:
      - pgdata:/var/lib/postgresql/data

  # Uncomment for prod TLS termination. Edit Caddyfile to set your domain.
  # caddy:
  #   image: caddy:2
  #   restart: unless-stopped
  #   ports:
  #     - "80:80"
  #     - "443:443"
  #   volumes:
  #     - ./Caddyfile:/etc/caddy/Caddyfile
  #     - caddy_data:/data
  #   depends_on:
  #     - postgres

volumes:
  pgdata:
  # caddy_data:
"#;

pub const CADDYFILE: &str = r#"# Update `example.com` to your domain, then uncomment the `caddy`
# service in docker-compose.yml. Caddy provisions a Let's Encrypt TLS
# certificate automatically and renews it. pg-web itself serves plain
# HTTP on :8080 — TLS termination is always out-of-process.

example.com {
    reverse_proxy postgres:8080
}
"#;

pub const GITIGNORE: &str = r#".env
.env.*
!.env.example
*.log
/target
.DS_Store
"#;

pub const README_MINIMAL: &str = r#"# {APP}

A pg-web app. Routes + templates + handlers live under `pages/`, schema under `migrations/`, static assets under `public/`.

## Quick start

```bash
pg-web up          # boots Postgres + the HTTP server, resolves DATABASE_URL
pg-web push        # syncs pages/ / public/ / pgweb.toml into the DB
# visit http://localhost:8080
```

Iterate without re-pushing by hand:

```bash
pg-web dev         # watches pages/ + public/, auto-pushes on save
```

## Next steps

- Edit `pages/index.html` + `pages/index.sql` to change the root page.
- Add a route by creating `pages/<name>/index.sql` (+ optional `index.html`). Every directory is a URL; the filename stem picks the HTTP method — `index` = GET, `post` = POST.
- Add a migration as a numbered file under `migrations/` (e.g. `0001_create_users.sql`), then `pg-web migrate apply`.
- Store runtime settings / secrets in `pgweb.settings` via `pg-web env set/unset/list`; read them from handlers with `SELECT pgweb.setting('KEY')`.

## A bigger starting point

```bash
pg-web init another-app --template todo
```

scaffolds a full HTMX todo list — dynamic routes, form validation, static assets, the works.

## Configuration files

- `pgweb.toml` — framework config (port, `env = "development"/"production"`, watch paths).
- `docker-compose.yml` — dev stack. Uncomment the `caddy` service for prod TLS.
- `Caddyfile` — TLS reverse-proxy config. Set your domain.

## Health & readiness endpoints

pg-web provides two surfaces out of the box (both are present after `CREATE EXTENSION` or on a fresh `pg-web init` + `up`, before any `push`):

- **Protected platform probes** (always available, never overridable, the ones you point load balancers / orchestrators / Docker HEALTHCHECK at):
  - `GET /_pgweb/health`
  - `GET /_pgweb/readiness`
- **App-level conventional endpoints** (sensible JSON defaults that you are expected to customize or disable):
  - `GET /health`
  - `GET /readiness`

To provide your own logic (e.g. "at least N open todos", "downstream X is up", or a richer JSON body), create `pages/health/index.sql` (and optionally a sibling `index.html` for a Tera-rendered response) exactly like any other route. `pg-web push` replaces the seeded row; your handler wins.

To stop the *framework default* from answering (so `/health` falls through to your `_404` or a later custom route): in `pgweb.toml` under `[server]` set `health_enabled = false` (or `readiness_enabled = false`) and push. User routes for those paths are unaffected by the flags.

See `docs/DEPLOYMENT.md` (Monitoring) and `docs/APP-DEVELOPER-GUIDE.md` for the recommended pattern and which surface to use for container health vs. application health.
"#;

pub const README_TODO: &str = r#"# {APP}

A pg-web todo-list app, scaffolded from `pg-web init --template todo`. HTMX + Postgres, server-rendered, no JavaScript build step.

## Run it

```bash
pg-web up                  # Postgres + HTTP stack
pg-web migrate apply       # creates public.todos
pg-web push                # syncs routes / templates / handlers / assets

# visit http://localhost:8080
```

Add a todo via the form; toggle or delete via the row buttons (the Delete button uses a real `DELETE /todos/:id` with `hx-delete`). Every click round-trips through Postgres and renders the reply fragment server-side.

Iterate:

```bash
pg-web dev                 # watches pages/ + public/, auto-pushes on save
```

## Layout

```
{APP}/
├── migrations/
│   └── 0001_create_todos.sql
├── pages/
│   ├── index.html                  # GET /  — list + form
│   ├── index.sql                   # GET /  — SELECT todos → JSON
│   ├── _404.html                   # static 404 fallback
│   └── todos/
│       ├── post.html               # POST /todos — success <li> or OOB error
│       ├── post.sql                # catches check_violation inline
│       ├── toggle/post.{html,sql}  # outerHTML swap on toggle
│       └── [id]/
│           ├── index.{html,sql}    # GET /todos/:id detail view
│           └── delete.sql          # DELETE /todos/:id (real method, text mode '')
└── public/
    └── styles.css
```

Three handler dispatch modes exercised:

- **Dynamic** (JSON → Tera): `GET /`, `POST /todos`, `POST /todos/toggle`, `GET /todos/:id`
- **Static** (template, no SQL handler): `GET /_404`
- **Raw text** (SQL only, no sibling template): `DELETE /todos/:id` (via pages/todos/[id]/delete.sql)

## What to look at

- `pages/todos/post.sql` + `post.html` — the `check_violation` → inline error pattern. Empty title triggers an HTMX OOB swap instead of a 500.
- `migrations/0001_create_todos.sql` — the table `CHECK` is the validation rule; the handler just surfaces it.
- `pages/todos/[id]/` — dynamic route capture, `req.path_params.id`.

`docs/TUTORIAL.md` in the pg-web repo walks through building this from scratch.

## Health & readiness (018.1)

pg-web ships two surfaces:

- **Protected platform probes** (always present, never overridable by your routes, the ones load balancers / orchestrators / the Dockerfile HEALTHCHECK should target):
  `GET /_pgweb/health` and `GET /_pgweb/readiness`.
- **App-level conventional endpoints** (seeded with simple JSON defaults so a fresh `pg-web init` or bare `CREATE EXTENSION` "just works"; you are expected to override or disable them for real apps):
  `GET /health` and `GET /readiness`.

### Overriding the defaults (the recommended pattern)

Create a normal route under `pages/health/` (or `pages/readiness/`):

```
pages/
└── health/
    └── index.sql          # GET /health — your app-specific logic
    # (optional) index.html  # if you want a Tera-rendered health page
```

Example `pages/health/index.sql` (raw-text, returns proper JSON via the v2 helper):

```sql
-- Override of the framework default for GET /health.
--
-- On a fresh install the framework seeds a row in pgweb.routes for
-- ('GET', '/health') pointing at pgweb._default_health_handler.
-- When you `pg-web push` this file, the CLI:
--   1. Executes this CREATE OR REPLACE FUNCTION.
--   2. Does an ON CONFLICT (method, path_pattern) DO UPDATE on pgweb.routes.
-- The user row completely replaces the seeded default. Your handler is now
-- called for /health.
--
-- The disable flag (health_enabled = false in pgweb.toml + push) only
-- suppresses the *framework default*. A user route for /health is never
-- suppressed by the flag.
--
-- The protected `/_pgweb/health` (hard-mounted in the HTTP layer before any
-- user fallback) is unaffected by user routes and by the enabled flags.
-- It is the one the container healthcheck actually curls.

CREATE OR REPLACE FUNCTION pgweb.pages__health__index(req json) RETURNS json AS $$
  SELECT pgweb.json(
    jsonb_build_object(
      'status', 'ok',
      'app',    'todo',
      'note',   'custom health override active — see pages/health/index.sql for the pattern'
    )
  )
$$ LANGUAGE sql STABLE;
```

Push as usual. `curl /health` now returns your payload (and `Content-Type: application/json` because we used the envelope helper).

To go back to the framework default: delete the `pages/health/` directory (or just the sql) and `pg-web push` again. Reconcile removes the user route row; the seeded default is restored.

The same pattern applies to /readiness.

See `docs/APP-DEVELOPER-GUIDE.md` (Operational endpoints) and `docs/DEPLOYMENT.md` for which surface to use for different audiences and the recommended HEALTHCHECK stanza.

The protected probes + "broken user handler does not affect the platform probe" are exercised and asserted in the test suite (http_smoke + docker_e2e). The override story is documented here in the companion todo app's README with a complete worked example.
"#;

/// Substitute the `{APP}` placeholder with the actual app name. The
/// placeholder is deliberately chosen to never occur in valid template
/// content (no brace + "APP" + brace in our templates except as markers).
pub fn render(template: &str, app_name: &str) -> String {
    template.replace("{APP}", app_name)
}
