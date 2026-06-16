# Changelog

All notable changes to pg-web. Phase 1 is tagged as `0.1.x`. Phase-2
features will move forward as `0.2.x` when they ship.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Dates are ISO-8601. Version numbers follow semver — breaking pre-1.0
changes may land in minor releases.

## [Unreleased]

### Added
- Extension upgrade path (018.2): real `--from--to.sql` scripts, packaging so
  `pg_web_ext--A.B--C.D.sql` files land in the image next to the pgrx-generated
  install SQL, and `ALTER EXTENSION pg_web_ext UPDATE` now works for additive
  schema changes. Hand-authored scripts live in `crates/pg_web_ext/upgrades/`.
  The Dockerfile was updated to stage them; the existing wildcard COPY ships
  them. A skeleton `pg_web_ext--0.2.0--0.3.0.sql` (with policy header) proves
  the mechanism and is present in published images.
- `pgweb.ext_version()` SQL helper (returns the value from `pg_extension.extversion`
  for the running DB; distinct from `.control` default_version). Granted to the
  serving role. Useful for readiness probes (ties into 018.1) and observability.
  Added as an additive change in the bootstrap; the upgrade path + skeleton
  prepare the delta for the next version bump.
- New Tier 3 ignored test `extension_upgrade_preserves_data_and_serves` (in
  `docker_e2e.rs`) that does a full self-upgrade smoke: push real app data,
  write a synthetic additive script, run `ALTER EXTENSION ... UPDATE TO ...`,
  assert marker + all prior `pgweb.*` rows + user data + continued HTTP serving.
  Runs automatically as part of the Docker E2E matrix. DDL portability notes
  for 15/16/17.
- Policy documented in `CLAUDE.md` (new invariant + coding practice), 
  `docs/DEPLOYMENT.md` (completely rewritten upgrade section with restart-cost
  warning, backup rec, zero-downtime distinction, cross-refs), `docs/TESTING.md`,
  `crates/pg_web_ext/upgrades/README.md`, and this changelog. The previous
  situation (DEPLOYMENT.md promising a script that did not exist) is fixed.
- `cargo install pg-web` distribution for the CLI (prompt 010). The package on
  crates.io is now named `pg-web` (binary remains `pg-web`). Full recommended
  metadata (description, keywords, categories, homepage, docs link, readme).
  The `templates/todo/` tree is vendored inside the crate so `init --template
  todo` works out of the box for published installs.
- CI: `cargo publish -p pg-web --dry-run` on every PR/push (in the normal CI
  job) to catch packaging problems early.
- Release workflow: new `publish-cli` job that runs on `v*` tags (after the
  existing test-all + publish-image), gated by `CARGO_REGISTRY_TOKEN` secret.
  Matches the Docker Hub guard pattern; no accidental publishes from forks.
- Docs + scripts updated for the split install story: `cargo install pg-web`
  gets you the management CLI; the runtime (Postgres + `pg_web_ext`) always
  comes from the `rtaylor96/pg-web` Docker image. All `-p pg_web_cli` cargo
  selectors updated to the new package name `-p pg-web`. Internal lib name and
  directory unchanged.

### Changed
- **Benchmark regression gate hardened (prompt 030).** The placeholder bench
  gate (`a-static-c1` success ≥ 1 % — "did the worker bind at all") is replaced
  by a data-driven, two-layer gate: a per-workload **≥ 99 % success floor**
  (always on; platform-independent; this is the check that would have caught the
  016 worker-self-termination regression the moment it landed) plus opt-in
  (`BENCH_STRICT=1`) per-tier **p99 ceilings** (baseline × `BENCH_P99_MARGIN`)
  and **successful-req/s floors** (baseline × `BENCH_RPS_FLOOR_FRAC`, computed
  from successful — not error-inclusive — throughput). On any breach (or an
  infra/early exit) the bench prints a loud, full-width, ASCII-framed, itemized
  `BENCH REGRESSION DETECTED` banner at **every** `TEST_MODE` (incl. `short`),
  in addition to the unchanged machine-parseable `PGWEB-BENCH … OVERALL=fail`
  line. New `bench/thresholds.sh` holds the env-tunable knobs + per-tier
  baselines (re-baseline per deploy platform). `BENCH_SELFTEST=1` injects a
  guaranteed regression to prove the gate is live; the HOLB "under load" leg is
  now a gated workload (`workloads=13`). Knobs documented in
  `docs/BENCHMARKS.md` + `docs/internal/TESTING-SETUP.md`; `BENCH_MIN_STATIC_SUCCESS`
  is kept only as a back-compat alias. No product code changed (bench harness +
  docs only).

### Fixed
- **HTTP worker self-terminated 8 seconds after startup** (regression from the
  prompt-016 graceful-shutdown change). The 8s drain cap wrapped the entire
  `axum::serve` future, but `with_graceful_shutdown` resolves only after SIGTERM
  — so the timer fired 8s after *startup* and the worker exited. Because the exit
  was clean (code 0), the postmaster never restarted it (despite
  `bgw_restart_time = 5s`), so every deployment's HTTP server stopped answering
  ~8 seconds after boot. The 8s budget now starts only once SIGTERM is observed,
  so the server runs for the postmaster's full lifetime and still drains
  in-flight work for ≤8s on shutdown. New tier-3 regression test
  `worker_serves_past_drain_cap` (idles past the cap, asserts the worker still
  serves). This was also the true cause of the benchmark's "72%-then-0%" results
  (previously misattributed to "the single-worker reality"); post-fix the bench
  reports ~100% success on every workload with a real HOLB before/after.

## [0.2.0] — 2026-04-25

Polish release. Closes the deferred-from-0.1 work items, sharpens the
deploy story, and bumps the asset cap. No invariant changes — apps
written against `0.1.x` work unchanged on `0.2.x`.

### Added

- **Push retry on concurrent DDL** (`L`). `pg-web push` now wraps its
  transaction in a 3-attempt jittered retry. Triggered by SQLSTATE
  40001 (serialization failure) or the literal `tuple concurrently
  updated` message that concurrent DDL raises (XX000 internal error).
  On retry exhaustion, push opens a fresh diagnostic connection,
  queries `pg_stat_activity` for sibling `pg-web *` clients, and
  attaches a per-row `kill <os_pid>` (same host) or
  `pg_terminate_backend(<backend_pid>)` (remote host) suggestion to
  the error.
- **Application-name tagging on every CLI connection.** Every CLI
  subcommand (`push`, `dev`, `migrate`, `env`, `check`, `stack`) now
  opens its connections with `application_name = 'pg-web {verb}
  (pid={pid}, host={host})'`. Visible in `pg_stat_activity`; powers
  the retry diagnostic.
- **CLI bundled in `rtaylor96/pg-web:latest`** (`F.3`). The image
  builds and ships `pg-web` at `/usr/local/bin/pg-web` so
  `docker compose exec postgres pg-web push --dir /app` works from
  inside the compose network without publishing :5432 to the host.
- **Content-hash asset filenames** (`H`). When `pgweb.toml
  [server].env = "production"`, push rewrites template references
  like `<link href="/styles.css">` to fingerprinted URLs
  (`/styles.<8hex>.css`) and stores the asset under that URL. The
  router emits `Cache-Control: public, max-age=31536000, immutable`
  for any asset GET whose path matches the fingerprint shape AND env
  is production. Canonical paths still get `must-revalidate`; dev
  mode is unchanged.
- **Larger asset cap** (`I`). The BYTEA per-asset CHECK and the CLI's
  cap both bump from 2 MiB to 20 MiB. Covers virtually every
  practical asset without committing to true `pg_largeobject`
  streaming yet.
- **Roadmap: backup story split.** ROADMAP gains three Phase-4
  entries — `pg-web backup` / `pg-web restore` (operational
  `pg_dump` wrapper), `pg-web export --code-only` /
  `pg-web import` (portable code-only dump), source-tree-in-DB via
  a `pgweb.sources` schema. The parking-lot "project-in-database
  backup" entry redirects to the trio.

### Deferred

- **`pg-web push --target <name>` SSH-tunneled remote deploy**
  (`F.2`). Validation requires a real remote target; deferring to
  Session 6 when remote infra is available. Local-loopback push and
  the F.3 in-image CLI remain the supported deploy paths until then.
- **True `pg_largeobject` streaming.** v0.2 ships only the BYTEA
  cap-raise. `lo_read`-backed streaming for assets >20 MiB stays
  Phase 2+ work.

### Testing

- Tier 1 — 72 `#[pg_test]` (was 70).
- Tier 2a — 2 HTTP smoke (unchanged).
- Tier 2b — 143 CLI unit + integration (was 124).
- Tier 3 — 13 docker E2E (was 9).
- Tier 4 — 19-section black-box smoke (unchanged).

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
- `rtaylor96/pg-web:latest` Docker image bundles Postgres 17 + the
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
- **CLI bundled in `rtaylor96/pg-web:latest`.** Build the image, push
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
