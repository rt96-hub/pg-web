# site/ — pg-web.dev (dogfooded docs site)

This directory **is** the pg-web application that serves https://pg-web.dev.

It is the primary dogfooding vehicle for the framework (alongside `examples/todo/`). The public documentation for pg-web is itself a pg-web app: Postgres + `pg_web_ext` (via the official `pgweb/postgres:latest` image), Caddy for TLS, standard `pages/`, `public/`, `pgweb.toml` layout, and the normal `pg-web` CLI workflow.

## Why this exists

- Credibility: the best proof that the framework is real is that the project's own docs run on it.
- Companion-app rule (see root `CLAUDE.md`): every significant capability should be exercised by a real app that ships with the project. This site covers documentation-oriented static + dynamic patterns.
- Reference for "how we host pg-web.dev".

## Layout (follows docs/APP-LAYOUT.md exactly)

```
site/
├── pages/
│   ├── index.html + index.sql   # GET / — dynamic (JSON → Tera), pitch + quickstart + live sections
│   ├── overview/index.html      # GET /overview — static
│   ├── app-layout/index.html    # GET /app-layout — static (the rules)
│   ├── tutorial/index.html      # GET /tutorial — static (pointers + highlights)
│   ├── deployment/index.html    # GET /deployment — static
│   ├── roadmap/index.html       # GET /roadmap — static
│   └── _404.html                # static custom 404
├── public/
│   └── styles.css
├── pgweb.toml
├── docker-compose.yml
├── Caddyfile
├── migrations/   (optional for first cut; none required yet)
└── README.md     # you are here
```

Most pages are pure static templates (`.html` only → zero SQL, synthesized trivial handler). The home exercises a real `(req json) RETURNS json` handler.

## Local development

From inside `site/` (adjust the path to the `pg-web` binary as needed):

```bash
# From repo root, one-time (or `docker pull pgweb/postgres:latest` when published)
bash ../scripts/build-image.sh

# Preferred dev loop (watcher + auto-push + browser live-reload via SSE)
../target/debug/pg-web dev

# Or the explicit steps
../target/debug/pg-web up
../target/debug/pg-web migrate apply
../target/debug/pg-web push

open http://localhost:8080
```

`pg-web dev` watches `pages/` and `public/`, debounces, content-hash dedupes, preflights SQL, pushes, and injects the livereload script (dev mode only).

Tear down:
```bash
../target/debug/pg-web down
../target/debug/pg-web down --volumes   # also drops pgdata
```

Run `pg-web check` (from this dir) before committing changes — it must pass cleanly.

## Deploying updates

The production deploy uses the **exact same pattern** end users are expected to follow:

1. On the VPS (or via CI):
   ```bash
   docker compose pull   # when the pgweb/postgres image is updated
   docker compose up -d
   ```

2. From your machine (SSH tunnel or Tailscale for DB access):
   ```bash
   # Background tunnel (one terminal)
   ssh -L 5432:localhost:5432 deploy@your-vps

   # In another terminal / CI step
   pg-web migrate apply --url "postgres://postgres:$POSTGRES_PASSWORD@localhost:5432/app"
   pg-web push --with-migrate --url "..."
   ```

Or, once the repo is on the VPS, use the in-image CLI:
```bash
docker compose exec postgres pg-web push --with-migrate
```

Point DNS for `pg-web.dev` (and `www.` if desired) at the VPS. Caddy handles Let's Encrypt + reverse proxy to the internal `:8080`.

The `docker-compose.yml` and `Caddyfile` in this directory are the source of truth for the live site (with the caddy service uncommented in prod).

## Content synchronization

- Authoritative specs and long-form maintainer material stay in the repo root `docs/` (OVERVIEW, VISION, APP-LAYOUT, APP-DEVELOPER-GUIDE, TUTORIAL, DEPLOYMENT, ROADMAP, etc.).
- This `site/` tree contains the public-facing, web-shaped version of the core content.
- For the initial implementation: content was ported by hand from `docs/*.md` into `pages/**/*.html` (light condensation + HTML structure for nav, headings, code blocks). No automatic sync step yet.
- When editing docs in `docs/`, also update the corresponding page(s) under `site/pages/` (or note that a follow-up pass will reconcile). Keep the prose close to the source for now; deep rewriting can happen incrementally.
- The long-term ideal is that the live site is the primary user-facing docs, while `docs/` in the monorepo remains the detailed spec + historical record.

See the original handoff prompt at `site/008_docs_site_pgweb_dev_dogfooding.md` for the full charter, success criteria, and constraints.

## References

- Root `CLAUDE.md` — invariants (one SPI tx per request, no raw C, extension/CLI decoupling, companion-app rule, Phase discipline).
- `docs/APP-LAYOUT.md` — the exact file → route rules this app follows.
- `examples/todo/` + its README — the other canonical Phase 1 reference (CRUD, validation, dynamic routes, HTMX, `_404`, etc.).
- `docs/DEPLOYMENT.md` — the full production story.
- `docs/APP-DEVELOPER-GUIDE.md`, `docs/TUTORIAL.md`, `docs/OVERVIEW.md`.

## Status

This site is a first-class, committed pg-web application. It must survive `pg-web check` and the spirit of the five-tier test strategy.

When you update it, run the local flow, verify the routes, then deploy with `pg-web push --with-migrate`.

**The database is the application.** This site proves it for documentation.
