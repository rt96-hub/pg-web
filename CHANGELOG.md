# Changelog

All notable changes to pg-web. Phase 1 is tagged as `0.1.x`. Phase-2
features will move forward as `0.2.x` when they ship.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Dates are ISO-8601. Version numbers follow semver — breaking pre-1.0
changes may land in minor releases.

## [0.1.0] — 2026-04-24

First release candidate for Phase 1 — a usable, production-deployable
Postgres-native web framework with dev-loop, error handling, schema
migrations, runtime settings, static assets, and browser live-reload.

Full feature surface below, grouped by milestone and component.

### M1.1 — Walking skeleton (Session 1)

- pgrx-backed background worker binds `:8080` and serves HTTP from
  inside Postgres. Single extension; no separate app server.
- Seeded `pgweb.routes` + `pgweb.templates` make `CREATE EXTENSION
  pg_web_ext;` immediately curl-able.
- `pg-web init <name>` scaffolds a minimal app (`pages/index.{html,sql}`,
  `pgweb.toml`, `docker-compose.yml`, `Caddyfile`, `.gitignore`).
- `pg-web push` walks `pages/` and upserts routes + templates +
  handler SQL into Postgres in one transaction.
- `pgweb/postgres:latest` Docker image bundles Postgres 17 + the
  extension; scripts/build-image.sh is the one-command build.

### M1.3 — Interactive contracts + real demo (Session 2)

- `(req json) RETURNS json|text` handler contract with `req` keys
  `body`, `query`, `method`, `path`, `path_params`.
- Directory-as-route, filename-as-method layout. Reserved stems
  (`index`, `post`, `_404`) enforced at push time by `paths::scan`.
- Custom `_404` fallback: `pages/_404.html` (+ optional `_404.sql`)
  serves on route miss; baked-in default kicks in when the user
  hasn't provided one.
- `pg-web migrate apply` runs `migrations/*.sql` in filename order,
  recording each in `pgweb.migrations`.
- Full HTMX todo list at `examples/todo/` exercises every feature.
- Tier-3 docker E2E tests via testcontainers.
- `docs/TUTORIAL.md` walks through building the todo list from a
  fresh `pg-web init`.

### M1.2 — Interactive dev loop (Session 3)

- `pg-web up` / `pg-web down` own the Docker Compose stack; waits
  for PG + :8080 readiness; resolves `DATABASE_URL` from
  `pgweb.toml` / env.
- `pg-web dev` watches `pages/` + `public/` and auto-pushes on save.
  200 ms debounce → Blake3 content-hash dedupe → shift-left SQL
  preflight → full push. Container logs tail inline.
- Dynamic route patterns: `pages/posts/[id]/` becomes `/posts/:id`
  with `id` in `req.path_params`.
- Dev-mode error page — typed `ServeError` catalog (PGWEB_E001–E999)
  surfaces SQLSTATE + MESSAGE + DETAIL + HINT + handler name + req
  dump + remedy. Production mode serves a generic 500 with no
  internals leaked.
- Push validates Tera templates pre-DB; a broken `{% if %}` fails
  push instead of 500'ing later.
- Static assets: `public/*` served as BYTEA-backed HTTP responses
  with Blake3-ETag + `If-None-Match` revalidation. 2 MiB per-file
  cap enforced at push time.
- Port-shadowing preflight on `pg-web up`: bails with a fix-it
  message if something non-Docker (typically a `cargo pgrx run`
  leftover PG) holds `:8080`.

### M1.4 — Phase 1 closeout (Session 4)

- `pgweb.html_escape(text) → text` SQL helper for raw-text handlers
  that interpolate user input without Tera's auto-escape.
  STRICT IMMUTABLE PARALLEL SAFE.
- User-facing form-validation UX: PL/pgSQL handlers catch
  `check_violation` in their own `EXCEPTION` block and return an
  OOB-swapped error fragment to an `#form-error` div next to the
  form. Demo's POST `/todos` exercises the pattern — empty title →
  200 + inline error, no 500.
- `pg-web env set/unset/list` + `pgweb.setting(key)` SQL helper.
  Runtime settings + secrets live in `pgweb.settings`, readable from
  any handler. Reserved keys (`env`) rejected at the CLI.
- `pg-web init --template <name>` extracts a bundled example into
  the new app. Ships `--template todo`. Plain `init` now also
  writes a scaffolded `README.md`.
- `pg-web check` — offline project validator: layout, Tera parse,
  SQL parse (via pure-Rust `sqlparser`, no system build deps),
  migration filename-prefix uniqueness. `--url` enables ledger-drift
  comparison. Pre-commit / CI gate.
- `pg-web push --dry-run` rolls back instead of committing and
  tags every output line `[dry-run]`.
- `pg-web push --with-migrate` applies pending migrations before
  push; without the flag, push refuses to run with pending
  migrations (points at the fix flag).
- `pgweb.deployments` append-only ledger: one row per committed push
  with `pushed_at`, `from_host`, `file_count`, `migrations_applied`.
- Browser live-reload via SSE: `pg-web dev` NOTIFYs on successful
  push; extension's LISTEN task forwards to `/_pgweb/livereload`;
  injected client stub cache-busts stylesheets for CSS changes or
  `location.reload()`s for everything else. Channel-aware internal
  router — Phase-2 app-level subscriptions will reuse the same
  primitive. `--no-livereload` opt-out. Dev-only: zero extra
  Postgres backend slots in production.

### Known deferred to 0.2

- **Content-hash asset filenames + HTML rewrite.** Stable URLs
  (`/styles.css`) with ETag revalidation ship in 0.1; fingerprinted
  URLs + `Cache-Control: immutable` defer to 0.2.
- **`pg-web push --target <name>` SSH-tunneled remote deploy.**
  Local-loopback pushing works; remote push requires manual SSH
  tunnel today.
- **CLI bundled in `pgweb/postgres:latest`.** Build the image, push
  from the same box.
- **`pg_largeobject`-backed streaming assets above 2 MiB.**
  BYTEA cap of 2 MiB holds in 0.1; larger assets → CDN.
- **Phase-2 app-level real-time subscriptions** via `LISTEN/NOTIFY` +
  SSE. The channel-aware primitive shipped in 0.1 Component G is
  reusable — Phase 2 is additive, not a rewrite.

### Testing

Five tiers, all green at release:

- **Tier 1** — 70 `#[pg_test]` cases against live PG via pgrx.
- **Tier 2a** — HTTP smoke test against a running extension.
- **Tier 2b** — 124 CLI unit + integration tests.
- **Tier 3** — 9 docker E2E tests (testcontainers + examples/todo).
- **Tier 4** — 19-section black-box CLI smoke script.

### Supported Postgres versions

15, 16, 17. The reference image is PG 17.

### Contributors

- rt96 — project author, all commits.

[0.1.0]: https://github.com/rt96-hub/pg-web/releases/tag/v0.1.0
