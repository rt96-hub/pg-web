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
  <p>Run <code>pg-web dev</code> to watch for changes (coming in M1.2).</p>
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
# Dev: direct access on :8080.
# Prod: uncomment the `caddy` service below and set your domain in Caddyfile.

services:
  postgres:
    image: pgweb/postgres:latest
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

/// Substitute the `{APP}` placeholder with the actual app name. The
/// placeholder is deliberately chosen to never occur in valid template
/// content (no brace + "APP" + brace in our templates except as markers).
pub fn render(template: &str, app_name: &str) -> String {
    template.replace("{APP}", app_name)
}
