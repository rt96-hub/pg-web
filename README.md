# pg-web

**PostgreSQL is your web server.**

pg-web embeds an async HTTP listener inside a Postgres background worker. No Node, no Python, no Go, no separate app server. You write `.sql` handlers and `.html` (Tera) templates; the CLI syncs them into the database. SQL *is* the business logic. HTMX is the UI layer. One Docker image, one mental model, zero network hops between your web tier and your data.

- **Zero-proxy architecture**: the web server lives in the same process tree as Postgres. Requests use the Server Programming Interface (SPI) — no TCP, no connection pools, no ORM tax.
- **Directory-as-route, filename-as-method**: `pages/todos/[id]/index.html` + `index.sql` → `GET /todos/:id`.
- **Uniform handler contract**: every `.sql` is `pgweb.pages__<name>(req json) RETURNS json|text`. `req` carries `body`, `query`, `method`, `path`, `path_params`.
- **Dev UX that doesn't lie**: `pg-web dev` watches, preflights SQL, pushes, and live-reloads the browser via SSE. Production mode serves generic 500s and immutable fingerprinted assets.
- **Real companion app**: `examples/todo/` exercises the entire Phase 1 surface (CRUD, validation, dynamic routes, assets, live-reload, `_404`, deployments ledger, etc.).

Fully open source (MIT OR Apache-2.0). The runtime ships as `pgweb/postgres:latest`.

## Get started in < 5 minutes

```bash
# 1. Install the CLI (published crate)
cargo install pg-web

# 2. Get the runtime image (Postgres 17 + pg_web_ext + the pg-web CLI inside)
# One-time cold build from a checkout, or `docker pull pgweb/postgres:latest` once published
bash scripts/build-image.sh

# 3. Scaffold a real app (the HTMX todo list that exercises everything)
pg-web init my-todos --template todo
cd my-todos

# 4. Boot + schema + code
pg-web up          # docker compose under the hood + readiness poll + DATABASE_URL
pg-web migrate apply
pg-web push

# 5. Open it
open http://localhost:8080
# or: curl http://localhost:8080/
```

Edit `pages/index.html`, `pages/todos/post.sql`, etc., run `pg-web push` (or `pg-web dev` for the watcher + auto browser reload), and refresh. No Rust compilation on your app iteration path.

See `docs/TUTORIAL.md` for the step-by-step walkthrough that produces exactly `examples/todo/`.

## Production deploy (single VPS, Docker Compose)

```bash
# On the VPS
docker compose up -d
# From your machine (SSH tunnel or Tailscale; see docs/DEPLOYMENT.md)
pg-web migrate apply --url "..."
pg-web push --url "..."
```

Caddy terminates TLS in front. The extension only ever speaks plain HTTP on :8080. Full recipe, security checklist, CI/CD example, and backup story are in `docs/DEPLOYMENT.md`.

## I want to build an app with pg-web

- Start here: `docs/APP-DEVELOPER-GUIDE.md` (60-second orientation + handler contract + forms/validation + settings)
- Exact layout rules: `docs/APP-LAYOUT.md`
- Walkthrough that builds the todo list: `docs/TUTORIAL.md`
- The reference app: `examples/todo/` (and its `README.md`)
- Live friendly docs (when the dogfood site is up): https://pg-web.dev
- Config, env, check, up/dev, push flags: the CLI `--help` and `pg-web check`

Everything you write stays in SQL + HTML + a little TOML. The framework owns the HTTP worker, the router, Tera, asset serving, live-reload, and the dev/deploy loop.

## I want to understand the internals or contribute

- Current state snapshot: `docs/OVERVIEW.md`
- Mission + why: `docs/VISION.md`
- Phases, decision log, parking lot: `docs/ROADMAP.md`
- Architecture (two crates, SPI, BGW, Axum thin shell): `docs/ARCHITECTURE.md`
- Testing strategy (five tiers, companion-app rule): `docs/TESTING.md`
- Maintainer environment, pgrx, packaging, pitfalls: `docs/internal/DEVELOPER-GUIDE.md`
- Agent north-star + invariants (required reading before touching code): `CLAUDE.md`
- Historical working notes: `docs/internal/sessions/`

See `CONTRIBUTING.md` for the contribution process. Every framework feature must be exercised in `examples/todo/` (or the docs-site app) before it is considered shipped. We follow conventional commits and keep the five test tiers green.

## Status

- **v0.2.0** — Phase 1 (Synchronous Core) complete + polish (push retry, CLI-in-image, content-hashed immutable assets, 20 MiB asset cap).
- Phase 2 (auth + sessions + RLS bridge + app-level realtime) is in planning (`docs/sessions/session_6.md`).
- Supported Postgres: 15, 16, 17 (reference image is 17).
- Five-tier test suite (pgrx + HTTP smoke + CLI + Docker E2E + black-box smoke) is the gate.

See `CHANGELOG.md` for the detailed release notes.

## License

Dual-licensed under MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.

## Links

- GitHub: https://github.com/rt96-hub/pg-web
- crates.io (CLI): (pending publication — see prompt 010)
- Docker Hub: `pgweb/postgres:latest`
- Tutorial + guides: `docs/` in this repo
- Dogfooded docs site: https://pg-web.dev (work in progress)

---

**The database is the application.** Write SQL. Ship HTML. Done.
