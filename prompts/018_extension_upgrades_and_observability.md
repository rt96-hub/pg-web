# 018 — Lifecycle & observability: extension upgrade scripts, health endpoint, metrics, request log

**Status:** Open work order — operational maturity; needed before real users accumulate data
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Prerequisites:** none; the health-endpoint piece should not wait for the Phase 4 dashboard
**Context:** pg-web ships as an all-in-one `postgres:17` image with `pg_web_ext` preinstalled and the web server living *inside* a Postgres background worker. v0.2.0 / Phase 1 is complete. The deploy side has real ops visibility (the append-only `pgweb.deployments` ledger), but the runtime side has almost none — no versioned upgrade path for the framework schema, no framework-owned health probe, no access log, no metrics. This work order establishes those four lifecycle/observability primitives, front-loading the two that are both cheap and time-sensitive.

---

## Summary

Four gaps, prioritized. Two are urgent and cheap; two layer on after.

1. **No extension upgrade path.** The schema installs only as a `bootstrap` `extension_sql!` block (`crates/pg_web_ext/src/schema.rs:13` / `:228-229`) and the image bakes a single `pg_web_ext--<ver>.sql` install file (`Dockerfile:84`). There are **no** `pg_web_ext--A.B--C.D.sql` migration scripts, so `ALTER EXTENSION pg_web_ext UPDATE` has nothing to run — even though `docs/DEPLOYMENT.md:135-144` already *documents* that exact command and claims "Postgres natively executes the included migration script (`pg_web_ext--A.B--C.D.sql`)." That script does not exist. The moment a real user has data and a new version changes framework tables, the only upgrade path is dump / recreate / restore.
2. **No real health endpoint; the Docker healthcheck probes the user's homepage.** `Dockerfile:102-104` runs `curl -sf http://127.0.0.1:8080/` — the *user's* `GET /` index handler. A bug in user code marks the whole container unhealthy and can trigger orchestrator restart loops, conflating "user app broken" with "platform down."
3. **No persisted/structured request log.** Logging is `tracing` to stdout (`crates/pg_web_ext/src/logging.rs`); the only request-path log line is a structured *error* line on failure (`crates/pg_web_ext/src/http.rs:201`). Successful and 4xx requests are silent — no method/path/status/latency/request-id access log.
4. **No metrics.** No Prometheus/OpenMetrics surface anywhere.

This order delivers a prioritized plan: **Part 1 (upgrade convention)** and **Part 2 (health endpoint)** are the urgent, cheap wins. **Part 3 (request logging)** and **Part 4 (metrics)** layer on top using the same `_pgweb/*` framework-reserved route machinery.

## Why this matters now

- **The upgrade gap is invisible exactly until it's catastrophic.** v0.2 changed the schema (e.g. the `pgweb.assets` `CHECK` cap went 2 → 20 MiB, `schema.rs:95`) but only ever via *fresh* installs — no running container with user data was upgraded in place. Phase 2 is about to add `pgweb.sessions`, `pgweb.user_id` plumbing, CSRF state, etc. (`docs/ROADMAP.md:53-64`). The first time a user with real data pulls a Phase-2 image, `ALTER EXTENSION ... UPDATE` will either no-op (schema silently stale, subtle breakage) or there will be nothing to run at all. Establishing the `--from--to.sql` convention *now*, while 0.2→0.3 is still hypothetical, is far cheaper than retrofitting it after users are on the upgrade treadmill. It also closes a live docs-vs-reality gap (DEPLOYMENT.md already promises the mechanism).
- **The healthcheck footgun is one line of Dockerfile away from a production incident.** Today, shipping a user index handler that 500s (or even just renders slowly) makes Docker/orchestrators consider the container unhealthy and restart it — restarting *Postgres*, killing every connection, for a bug that has nothing to do with platform liveness. A framework-owned `/_pgweb/health` fixes this completely and is tiny to build (the `_pgweb/livereload*` routes already prove the pattern, `http.rs:57-67`).
- **Access logs and metrics are table-stakes the moment more than one person operates an app.** Prompt `006` (an open DX prompt) already asks for FastAPI/Flask-style per-request lines in `pg-web dev`. This order is the *umbrella*: the same emission point feeds dev-terminal lines (006's scope), a structured production access log (stdout JSON for the Docker log driver — `docs/ARCHITECTURE.md:204-205`, `docs/DEPLOYMENT.md:173-174` already describe Datadog/Loki/CloudWatch collecting from stdout), and metrics counters.

---

## Part 1: extension upgrade path

**This is the time-sensitive one. Do it first.**

### Current state (verified)

- `crates/pg_web_ext/src/schema.rs:13-230` is a single `extension_sql!(... name = "framework_tables", bootstrap)` block. `bootstrap` means "this runs first on `CREATE EXTENSION`, before everything else" — it is an *install* artifact, not an upgrade artifact.
- pgrx compiles that into `pg_web_ext--<CARGO_VERSION>.sql`. `pg_web_ext.control:2` has `default_version = '@CARGO_VERSION@'`, substituted at build.
- `Dockerfile:84` copies `pg_web_ext--*.sql` (the install file) into the image. No `--A.B--C.D.sql` files are produced or copied.
- `docs/DEPLOYMENT.md:135-144` documents the *intended* upgrade flow: `docker compose pull` → `docker compose up -d` → `ALTER EXTENSION pg_web_ext UPDATE;`, and asserts Postgres runs `pg_web_ext--A.B--C.D.sql`. **That file does not exist** — the docs describe a mechanism that isn't wired up.

### The architectural coupling to document explicitly

Because the web server lives *inside* Postgres (the BGW, `crates/pg_web_ext/src/worker.rs`), upgrading the framework means replacing the `.so` — which means **replacing the Postgres image and RESTARTING THE DATABASE**. There is no rolling / zero-downtime *web* deploy independent of the DB. Note the distinction the current docs blur:

- **App pushes** (routes/templates/handlers/assets via `pg-web push`) *are* zero-downtime — new traffic picks up the new routing table on the next SPI lookup (`docs/DEPLOYMENT.md:126-133`). This is correctly advertised.
- **Framework upgrades** (new `.so`) are *not* — they require an image swap + `docker compose up -d` + a brief Postgres restart (`docs/DEPLOYMENT.md:139-141` mentions the restart but never frames the no-rolling-deploy *cost*).

This is an inherent cost of the "database is the web server" model and deserves an explicit, honest docs section. Users coming from stateless-app deploys (where you roll the web tier without touching the DB) will otherwise be surprised.

### Proposed work

1. **Establish the `--from--to.sql` upgrade-script convention now.** Even if 0.2→0.3 is the first real one, lay down:
   - A directory/build convention for `pg_web_ext--A.B--C.D.sql` files (pgrx supports shipping these alongside the install SQL; investigate `sql = "..."` / file emission options in pgrx 0.18 — see Research tasks). They must end up in `/usr/share/postgresql/${PG_MAJOR}/extension/` in the image (`Dockerfile:82-84` is where install artifacts are copied; the upgrade scripts need the same treatment).
   - The `.control` `default_version` already tracks `@CARGO_VERSION@`; confirm an upgrade from an older installed version to the new `default_version` resolves a chain of `--X--Y.sql` files.
2. **A policy for additive vs destructive schema changes.** Document (in `CLAUDE.md` and/or a new `docs/` section) the rule: additive changes (new tables, new nullable columns, widened CHECKs like the 2→20 MiB bump) are always safe and go in an upgrade script verbatim; destructive changes (drops, narrowed constraints, type changes that can fail on existing rows) require an explicit migration with data-preservation steps and a documented breaking-change note. The `pgweb.assets` CHECK cap change is the canonical worked example.
   - **Lean:** treat upgrade scripts as append-only and forward-only for Phase 2; no downgrade scripts (`ALTER EXTENSION ... UPDATE TO <older>`), and say so explicitly. Downgrades are a rabbit hole; punt with a documented "restore from backup to roll back."
3. **A new test tier: install vN, then `ALTER EXTENSION pg_web_ext UPDATE` to vN+1, assert data survives.** Today's tiers (`docs/TESTING.md`) never exercise an in-place upgrade. Add a tier-3-style test that: boots the *previous* published image (or installs the previous `--<ver>.sql`), pushes a small app + inserts user rows, swaps to the new `.so` + upgrade script, runs `ALTER EXTENSION ... UPDATE`, and asserts (a) the framework tables migrated, (b) user data + `pgweb.deployments` rows + user tables are intact, (c) the app still serves. This must pass on **PG 15, 16, and 17** (invariant #6) — the upgrade SQL has to be valid across all three.
   - **Lean:** for the *first* iteration, a self-upgrade smoke (install version N's SQL, hand-apply a synthetic `--N--N+0.0.1.sql` that adds a column, `UPDATE`, assert the column exists and seeded rows survive) is enough to lock the convention. Full previous-image-pull testing can follow once there are two real published versions.
4. **Docs: the image-swap / restart upgrade procedure.** Rewrite `docs/DEPLOYMENT.md:135-144` so it (a) is backed by real scripts, (b) states the DB-restart cost plainly, (c) recommends `pg_dump` before upgrading (the backup recipe already lives at `docs/DEPLOYMENT.md:146-156`), and (d) distinguishes app-push (zero-downtime) from framework-upgrade (restart required).

**Touched files:** `crates/pg_web_ext/src/schema.rs` (may need restructuring so the bootstrap block and future upgrade SQL coexist cleanly), the extension build/`Dockerfile:82-84` (emit + copy `--A.B--C.D.sql`), `pg_web_ext.control`, `docs/DEPLOYMENT.md:135-144`, `docs/TESTING.md` (new upgrade tier), `CLAUDE.md` (additive/destructive policy + the no-rolling-web-deploy invariant), and the tier-3 harness under `crates/pg_web_cli/tests/`.

## Part 2: health endpoint

**Ship this NOW. It is tiny and prevents a real production footgun.**

### Current state (verified)

- `Dockerfile:102-104` HEALTHCHECK: `pg_isready ... && curl -sf http://127.0.0.1:8080/ > /dev/null`. The `curl /` hits the **user's** `GET /` handler (seeded default is `pgweb.hello_handler`, `schema.rs:102-107`, but real apps replace it). A broken or slow user index → unhealthy container → orchestrator restart loop → Postgres restart.
- `docs/ROADMAP.md:239` lists `/_pgweb/health` + `/_pgweb/metrics` as **Phase 3**. There is no reason health needs to wait — it is decoupled from the async job queue that defines Phase 3.
- The `_pgweb/*` framework-reserved namespace already exists and is mounted *above* the fallback (`http.rs:51-67`): `livereload_routes` and `static_routes` are `.merge`d before `.fallback(handle)`, so a framework route resolves before any user route. `livereload.rs` is the working template for a framework-owned route that reads settings via `BackgroundWorker::transaction(...)` (`livereload.rs:161`).

### Proposed work

1. **A framework-owned `GET /_pgweb/health` (and `GET /_pgweb/ready`)** served directly by the extension, independent of any user route — mounted alongside the livereload routes in `http::app` (`http.rs:57-67`). Semantics:
   - `/_pgweb/health` (**liveness**): is the BGW alive and is SPI reachable? Do the cheapest possible SPI round-trip (`SELECT 1` via `BackgroundWorker::transaction`, mirroring `settings::current_env`'s pattern at `settings.rs:38-43`). 200 + tiny body (`{"status":"ok"}` or plain `ok`) when reachable; 503 when the SPI probe errors. This answers "is the platform up?" — *not* "is the user's app correct?"
   - `/_pgweb/ready` (**readiness**): liveness *plus* "framework schema present and at the expected version" — e.g. `pgweb.routes` exists and (once Part 1 lands) the installed extension version matches `default_version`. Useful for orchestrators that distinguish liveness from readiness, and for catching the "upgraded `.so` but forgot `ALTER EXTENSION ... UPDATE`" state.
2. **Point the Dockerfile HEALTHCHECK at `/_pgweb/health`** instead of `/`. Replace `curl -sf http://127.0.0.1:8080/` with `curl -sf http://127.0.0.1:8080/_pgweb/health` (`Dockerfile:104`). A broken user index no longer marks the container unhealthy.
3. **Consider a separate health port.** The worker runs a **single-threaded** current-thread Tokio runtime (`worker.rs:60-72`, and the SPI-attachment reason at `worker.rs:52-56`) — only the SPI-attached thread can serve. If the request path is saturated (head-of-line blocking on a slow handler), `/_pgweb/health` on the same listener could itself time out and *cause* the restart loop it's meant to prevent. Evaluate binding health on a second port (e.g. `:8081`) or a second listener so liveness is answerable even under load. Cross-reference the single-thread throughput limitation (the same constraint behind `docs/ROADMAP.md:238`'s "internal concurrency management" Phase-3 item, and the `worker.rs` single-thread rationale).
   - **Lean:** ship `/_pgweb/health` on the existing `:8080` listener first (it's correct for the common "user code crashed, server fine" case and immediately defuses the footgun). Treat the separate-port hardening as a fast-follow once the single-thread saturation behavior is actually measured — don't block the cheap win on it.

**Touched files:** `crates/pg_web_ext/src/http.rs:57-67` (mount the routes), a new small `health.rs` module (or a section of `http.rs` — keep it flat per CLAUDE.md "no premature abstraction"), `Dockerfile:104`, `docs/DEPLOYMENT.md` Monitoring section (`:171-176`), `docs/ROADMAP.md:239` (move health out of Phase 3 / mark shipped), and a tier-2a HTTP smoke + tier-3 assertion that `/_pgweb/health` returns 200 while a deliberately-broken user `/` returns 500.

## Part 3: request logging

**This is the umbrella for the existing open prompt `006`. Reference it; do not duplicate it.**

### Current state (verified)

- `crates/pg_web_ext/src/logging.rs` sets up `tracing` → stdout, compact format, quiet deps (`DEFAULT_FILTER = "pg_web_ext=info,axum=warn,..."`).
- The *only* request-path log line is the error line: `http.rs:201` `error!(method, path, pgweb_error, "serve error")`. Every `ServeOutcome::Response` (2xx/4xx), every `Asset` (200/304), and the 404 fallback are **silent** — no access log.
- Prompt `006` (open) already specifies a dev-terminal access log via a `pgweb_dev_access` NOTIFY channel + CLI-side pretty printer, reusing the livereload NOTIFY pattern, gated on `env == Development`. It deliberately scopes itself to the *dev loop*.
- `docs/ARCHITECTURE.md:200-205` and `docs/DEPLOYMENT.md:171-176` already describe the intended production logging model: structured JSON to stdout (timestamp, level, file, line, message, **request_id**), collected by the Docker log driver into Datadog/CloudWatch/Loki. The `request_id` field is described but not implemented.
- `docs/ROADMAP.md:80` (Phase 4) lists "Request log + slow-request capture — `pgweb.request_log` with sampling."

### Proposed work

Position this as the superset that subsumes `006`'s dev-terminal lines and adds the production + persisted tiers:

1. **A per-request access log emitted at one point on the request path.** Wrap `router::serve` + response shaping in `http::handle` (`http.rs:69-143`) with `Instant` timing, and after the `ServeOutcome` is known, emit a single structured line: method, path, status, latency-ms, request-id, and (optionally) matched handler name. In production this is structured **JSON to stdout** for the log driver (matching the model already documented at `ARCHITECTURE.md:204`); in dev it can additionally drive `006`'s `pgweb_dev_access` NOTIFY for the pretty terminal line. **Introduce the `request_id`** (generate per request in `handle`; thread it into both the access line and the existing error line at `http.rs:201` so a failure correlates to its access entry).
2. **Optional sampling into a `pgweb.request_log` table** (the Phase-4 ROADMAP item, `:80`). Append-only, sampled (not every request — write amplification on the hot path matters, especially given the single-thread worker), with method/path/status/latency/request-id/timestamp and a retention story (see Open questions). This is the durable, queryable tier behind a future `/_pgweb/dashboard` (`ROADMAP.md:79`). Keep it **off or low-rate by default**; opt-in via `pgweb.settings`.
3. **Mind the single-thread cost of synchronous logging.** Every synchronous `pg_notify` or table insert on the request path runs on the one SPI-attached thread (`worker.rs:52-72`) and serializes with real traffic. Stdout `tracing` is cheap; a per-request SPI write is not. Sample aggressively, make the table tier opt-in, and prefer the NOTIFY/stdout paths for the always-on case.

**Touched files:** `crates/pg_web_ext/src/http.rs:69-143` (timing + emission + request_id), `crates/pg_web_ext/src/logging.rs` (a structured access target/format), optionally `schema.rs` (the `pgweb.request_log` table — and if so, it becomes the first real customer of the Part 1 upgrade-script convention), `docs/ARCHITECTURE.md:200-205`, and explicit cross-references to prompt `006`. **Do not re-spec 006's CLI-side printer** — note that this order provides the emission point 006 consumes.

## Part 4: metrics

### Current state (verified)

No metrics surface exists. `docs/DEPLOYMENT.md:175` advertises "Prometheus (Phase 4): the extension exposes `/_pgweb/metrics` in Prometheus format" and `ROADMAP.md:239` lists it (Phase 3) + `:253` (Phase 4 "Metrics export in Prometheus format"). Nothing implements it.

### Proposed work

1. **A framework-owned `GET /_pgweb/metrics`** in Prometheus/OpenMetrics text exposition format, mounted next to `/_pgweb/health` in `http::app` (`http.rs:57-67`). Minimal initial metric set: request count (by status class), request latency histogram, error count, SPI/handler timing, BGW uptime. Pull-based (the scraper hits the endpoint) — no push, no aggregation service.
2. **Keep it minimal and in-process.** Counters/histograms live in the BGW's memory (incremented at the same `http::handle` emission point as Part 3's access log — they share the timing instrumentation). Reading `/_pgweb/metrics` is a framework read; if it touches SPI at all (e.g. for `pgweb.deployments` count or uptime derived from a stored start time) it is one transaction (invariant #4).
   - **Lean:** ship the scrape endpoint early with a small, fixed metric set. Defer rich dashboards and per-route cardinality to Phase 4. Watch metric **cardinality** — do *not* label by raw path (unbounded with dynamic routes like `/posts/:id`); label by the matched *pattern* or omit path labels entirely (see Open questions).

**Touched files:** `crates/pg_web_ext/src/http.rs:57-67` (mount) + the shared instrumentation from Part 3, a small `metrics.rs` module (flat; only if it earns its keep), `docs/DEPLOYMENT.md:171-176`, `docs/ROADMAP.md:239`/`:253`.

## Research tasks

Read before writing. Be exhaustive; cite `file:line` in the resulting PR.

1. **pgrx upgrade-script support (Part 1, start here).** How does pgrx 0.18.0 (`docs/ROADMAP.md:309` pins it) emit extension *upgrade* SQL alongside the `bootstrap` install SQL? Does `extension_sql!` / the `cargo pgrx` schema generator support `--from--to` artifacts, or must they be hand-authored files placed next to the generated install SQL? Determine exactly what lands in `/usr/share/postgresql/${PG_MAJOR}/extension/` and how `Dockerfile:82-84` must change to copy upgrade scripts. Verify `ALTER EXTENSION pg_web_ext UPDATE` chains multi-step (`--0.1--0.2.sql` then `--0.2--0.3.sql`) when jumping versions.
2. **Cross-version SQL validity (invariant #6).** Confirm any upgrade SQL pattern you choose is valid on PG 15, 16, and 17. The existing `#[pg_test]` suite runs on all three (`docs/TESTING.md:45`); the upgrade tier must too.
3. **Single-thread saturation behavior (Parts 2-4).** Empirically characterize what happens to `/_pgweb/health` on `:8080` when a slow handler occupies the single SPI-attached thread (`worker.rs:60-72`). This decides whether the separate-port hardening in Part 2 is a fast-follow or can be deferred. Relates to the Phase-3 "internal concurrency management" item (`ROADMAP.md:238`).
4. **Request-id propagation.** Decide where the request-id is generated (Axum middleware vs inline in `handle`) and how it threads into both the access line and the existing error line (`http.rs:201`). Check whether a `tower-http` request-id layer fits the thin-shell-Axum model (`ROADMAP.md:315` documents the Axum-as-thin-shell decision) without dragging in async-on-backend-thread risk (invariant #7 — but this code is already in the BGW's runtime, so it's fine).
5. **`pgweb.request_log` shape + retention (Part 3).** Survey how the `pgweb.deployments` append-only ledger is structured (`schema.rs:52-60`) as the precedent. Decide sampling default, retention (time-based prune? row cap? unlogged table?), and whether it should be the first consumer of the Part 1 upgrade-script convention (it should, as a dogfood).
6. **Metrics format + library (Part 4).** Hand-rolled Prometheus text exposition vs a crate (`prometheus`, `metrics` + exporter). Given CLAUDE.md "no premature abstraction" and the tiny fixed metric set, evaluate whether a dependency earns its place or a `format!`-based emitter is simpler.
7. **Companion-app coverage.** CLAUDE.md: "Every feature ships with a companion-app flow." Determine how `examples/todo/` exercises each part (an upgrade test against it; a health-probe assertion; an access-log assertion; a metrics scrape) — `docs/TESTING.md:172-188` makes the companion app the acceptance gate.

## Constraints & invariants to respect

Cite `CLAUDE.md` (the repo's invariants list):

- **#3 — extension ↔ CLI strictly decoupled.** The extension has zero filesystem code; the CLI has zero HTTP-handler logic; they sync only via `pgweb.*` tables / NOTIFY. Health/metrics/access-log emission live entirely in the extension. The dev-terminal *presentation* of access logs (prompt `006`) lives entirely in the CLI and consumes a NOTIFY channel — keep that firewall intact. Upgrade scripts are an extension/image concern, not a CLI one.
- **#4 — one HTTP request = one SPI transaction.** The health SPI probe, any `request_log` insert, and any SPI a metrics read needs are **framework reads/writes inside that single transaction** — never open a second transaction or leak one. The framework `_pgweb/*` routes that don't touch SPI (e.g. a metrics read from in-memory counters) don't need a transaction at all.
- **#6 — PG 15 / 16 / 17 only.** Upgrade scripts and any new schema must work on all three. No pg18-only features.
- **#7 — async only in the BGW.** All of this runs inside the worker's Tokio runtime (`worker.rs`), so async is fine *here*; do not introduce `tokio` paths into any `#[pg_extern]` function.
- **`_pgweb/*` is the framework-reserved route namespace.** New routes (`/_pgweb/health`, `/_pgweb/ready`, `/_pgweb/metrics`) MUST be `.merge`d above the `.fallback(handle)` in `http::app` (`http.rs:57-67`), exactly as `livereload` is, so they resolve before — and never collide with — user routes. A user *may* legally define `GET /_pgweb/foo`; the framework routes take precedence by mount order, which is the intended behavior.
- **CLAUDE.md coding practices:** no `unwrap()`/`expect()` on the request path; keep modules flat (no premature `health.rs`/`metrics.rs` abstraction unless it genuinely earns its keep); every feature exercised in `examples/todo/`.

## Acceptance criteria

- [ ] `ALTER EXTENSION pg_web_ext UPDATE` upgrades 0.2 → 0.3 (or the first real version pair) with **user data, framework metadata, and `pgweb.deployments` rows preserved**, covered by a new test tier that runs on PG 15, 16, and 17.
- [ ] The `pg_web_ext--A.B--C.D.sql` upgrade-script convention is established, emitted into the image, and `docs/DEPLOYMENT.md:135-144` is rewritten so its documented upgrade command is actually backed by shipped scripts.
- [ ] A documented additive-vs-destructive schema-change policy exists (in `CLAUDE.md` and/or `docs/`), with the `pgweb.assets` 2→20 MiB CHECK change as a worked example, and the no-downgrade-scripts decision stated.
- [ ] `docs/` explicitly documents the image-swap / DB-restart framework-upgrade procedure and the inherent **no-rolling-web-deploy** cost of the in-Postgres model — distinguished from zero-downtime `pg-web push`.
- [ ] `GET /_pgweb/health` returns 200 **independent of user routes** and the Dockerfile HEALTHCHECK uses it (`Dockerfile:104`).
- [ ] A deliberately-broken user `GET /` (handler 500) **no longer marks the container unhealthy** — proven by a test where `/` returns 500 while `/_pgweb/health` returns 200.
- [ ] `GET /_pgweb/ready` reports framework-schema/version readiness (and, post-Part-1, catches "upgraded `.so` without `ALTER EXTENSION ... UPDATE`").
- [ ] Structured access logs are emitted for every request with **request-id + latency + status** (JSON to stdout in production), and the request-id correlates the access line with the existing error line (`http.rs:201`).
- [ ] `GET /_pgweb/metrics` is scrapeable in Prometheus text format with at least request counts, a latency histogram, error counts, and BGW uptime — with bounded label cardinality (no raw-path labels).
- [ ] No `_pgweb/*` route collides with user routes (all mounted above `.fallback` in `http::app`), and each new feature is exercised in `examples/todo/` per the companion-app gate.

## Open questions

1. **Separate health port?** Bind `/_pgweb/health` on a second listener/port (e.g. `:8081`) so liveness is answerable even when the single SPI-attached thread (`worker.rs:60-72`) is saturated by a slow handler — or accept the same-port simplicity for now and revisit after measuring saturation? (Ship same-port first per the Lean; this is the hardening decision.)
2. **Upgrade-script generation & testing strategy.** Hand-author `--A.B--C.D.sql` files, or generate them from a schema diff (note Phase 2.5's native-Rust diff engine, `ROADMAP.md:206-225`, is explicitly punted — do not block on it)? And for the test tier: pull the previous *published image*, or hand-apply the previous install SQL + a synthetic upgrade script? (Lean starts with the synthetic-script smoke.)
3. **`pgweb.request_log` sampling rate + retention.** Default sample rate (off? 1%? 100% only when a setting is on)? Retention: time-based prune, fixed row cap, or `UNLOGGED` table that's acceptable to lose on crash? Who prunes (a BGW tick, a trigger, the CLI)?
4. **Metrics cardinality.** Label request metrics by matched route *pattern* (`/posts/:id`), by status class only, or omit path labels entirely? Dynamic routes make raw-path labels unbounded — what's the bound?
5. **`request_id` source & format.** UUID v4, a monotonic counter, ULID, or honor an inbound `X-Request-Id` (from Caddy in front, `CLAUDE.md` invariant #2) when present? Where is it generated — Axum/`tower-http` middleware or inline in `handle`?
6. **Does the access log live behind the env flag like livereload, or always-on?** Prompt `006` gates its dev-terminal lines on `env == Development`. Should the *production* structured access log be always-on (it's just stdout, cheap) while only the table-write tier is opt-in?
7. **Should `/_pgweb/ready` gate the Docker healthcheck instead of (or in addition to) `/_pgweb/health`?** Readiness catches the stale-schema-after-upgrade state, but a too-strict readiness probe during a legitimate mid-upgrade window could itself cause a restart loop — which probe does the orchestrator key on?
