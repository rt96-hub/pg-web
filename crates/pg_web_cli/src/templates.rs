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

Add a todo via the form; toggle or delete via the row buttons. Every click round-trips through Postgres and renders the reply fragment server-side.

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
│       ├── delete/post.sql         # raw-text, empty body
│       └── [id]/index.{html,sql}   # GET /todos/:id detail view
└── public/
    └── styles.css
```

Three handler dispatch modes exercised:

- **Dynamic** (JSON → Tera): `GET /`, `POST /todos`, `POST /todos/toggle`, `GET /todos/:id`
- **Static** (template, no SQL handler): `GET /_404`
- **Raw text** (SQL only, no sibling template): `POST /todos/delete`

## What to look at

- `pages/todos/post.sql` + `post.html` — the `check_violation` → inline error pattern. Empty title triggers an HTMX OOB swap instead of a 500.
- `migrations/0001_create_todos.sql` — the table `CHECK` is the validation rule; the handler just surfaces it.
- `pages/todos/[id]/` — dynamic route capture, `req.path_params.id`.

`docs/TUTORIAL.md` in the pg-web repo walks through building this from scratch.
"#;

/// Substitute the `{APP}` placeholder with the actual app name. The
/// placeholder is deliberately chosen to never occur in valid template
/// content (no brace + "APP" + brace in our templates except as markers).
pub fn render(template: &str, app_name: &str) -> String {
    template.replace("{APP}", app_name)
}
