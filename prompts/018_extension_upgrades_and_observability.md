# 018 — Deferred observability tasks (request logging + metrics)

**Note (2026-06-13):** The original monolithic 018 was split. The two urgent/cheap items have been extracted into active work orders:

- Health + readiness endpoints → see `018.1_health_and_readiness_endpoints.md`
- Extension upgrade path → see `018.2_extension_upgrade_path.md`

This file is now kept as a lightweight holder for the **remaining two gaps** from the original prompt (Part 3: request logging and Part 4: metrics), plus the cross-cutting research / constraints / open questions that still apply to them. These are explicitly the "layer on after" items.

Current maintainer thinking (as of the split): metrics and full production request logging feel like potential overkill right now. Visibility during development may be better served by continued work on prompt 006 (dev access logs), richer per-app logging inside handlers, CLI improvements, and the in-browser dev dashboard interest captured in `026_in_browser_dev_dashboard.md`. Production exposure on the hosted site (or customer apps) is a concern. These items are therefore deprioritized / parked here for future re-evaluation.

Do not implement the split-out work (health or upgrades) from this file.

---

**Status:** Future / deferred (originally part of operational maturity work order)  
**Date opened:** 2026-06-11 (original); slimmed 2026-06-13  
**Related:** prompt 006 (dev access logging — this was intended as the production + table umbrella over it); 026 (in-browser dev dashboard); CLAUDE.md Phase discipline.

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
- **Access logs and metrics are table-stakes the moment more than one person operates an app.** Prompt `006` (an open DX prompt) already asks for FastAPI/Flask-style per-request lines in `pg-web dev`. This file captures the original umbrella thinking for production + persisted tiers.

---

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

## Research tasks (focused on remaining deferred items)

Read before writing. Be exhaustive; cite `file:line` in the resulting PR.

1. **Request-id propagation.** Decide where the request-id is generated (Axum middleware vs inline in `handle`) and how it threads into both the access line and the existing error line (`http.rs:201`). Check whether a `tower-http` request-id layer fits the thin-shell-Axum model (`ROADMAP.md:315` documents the Axum-as-thin-shell decision) without dragging in async-on-backend-thread risk (invariant #7 — but this code is already in the BGW's runtime, so it's fine).
2. **`pgweb.request_log` shape + retention (Part 3).** Survey how the `pgweb.deployments` append-only ledger is structured (`schema.rs:52-60`) as the precedent. Decide sampling default, retention (time-based prune? row cap? unlogged table?), and who prunes (a BGW tick, a trigger, the CLI?). Note that as of the 2026-06-13 discussion, full persistent request logging feels potentially overkill; this research should also consider lighter alternatives (CLI richness, per-app handler logging, or feeding a dev dashboard without a sampled table).
3. **Metrics format + library (Part 4).** Hand-rolled Prometheus text exposition vs a crate (`prometheus`, `metrics` + exporter). Given CLAUDE.md "no premature abstraction" and the tiny fixed metric set, evaluate whether a dependency earns its place or a `format!`-based emitter is simpler. Current maintainer sentiment: production metrics may not be needed yet (especially on the hosted site) and could be replaced by richer dev-focused visibility.
4. **Companion-app coverage.** CLAUDE.md: "Every feature ships with a companion-app flow." For any work that eventually lands here, determine how `examples/todo/` exercises the logging and/or metrics surface (an access-log assertion; a metrics scrape) — `docs/TESTING.md:172-188` makes the companion app the acceptance gate. Also consider whether a future dev dashboard (see prompt 026) could provide equivalent or better visibility without needing these primitives first.

## Constraints & invariants to respect

Cite `CLAUDE.md` (the repo's invariants list):

- **#3 — extension ↔ CLI strictly decoupled.** The extension has zero filesystem code; the CLI has zero HTTP-handler logic; they sync only via `pgweb.*` tables / NOTIFY. Access-log emission (and any future metrics) live entirely in the extension. The dev-terminal *presentation* of access logs (prompt `006`) lives entirely in the CLI and consumes a NOTIFY channel — keep that firewall intact.
- **#4 — one HTTP request = one SPI transaction.** Any `request_log` insert, and any SPI a metrics read needs, are **framework reads/writes inside that single transaction** — never open a second transaction or leak one. The framework `_pgweb/*` routes that don't touch SPI (e.g. a metrics read from in-memory counters) don't need a transaction at all.
- **#6 — PG 15 / 16 / 17 only.** Any new schema must work on all three.
- **#7 — async only in the BGW.** All of this runs inside the worker's Tokio runtime (`worker.rs`), so async is fine *here*; do not introduce `tokio` paths into any `#[pg_extern]` function.
- **`_pgweb/*` is the framework-reserved route namespace.** Any future routes for logging or metrics (e.g. `/_pgweb/metrics`) MUST be `.merge`d above the `.fallback(handle)` in `http::app` (`http.rs:57-67`), exactly as `livereload` is, so they resolve before — and never collide with — user routes. A user *may* legally define `GET /_pgweb/foo`; the framework routes take precedence by mount order, which is the intended behavior.
- **CLAUDE.md coding practices:** no `unwrap()`/`expect()` on the request path; keep modules flat (no premature `metrics.rs` abstraction unless it genuinely earns its keep); every feature exercised in `examples/todo/`.

As of the 2026-06-13 discussion, the broader request logging + metrics work is deprioritized. Any future implementation here should also consider lighter dev-focused alternatives (CLI improvements, richer handler logging, or the in-browser dev dashboard direction in prompt 026) before committing to persistent tables or always-on production surfaces.

## Acceptance criteria (for the remaining deferred items)

- [ ] Structured access logs are emitted for every request with **request-id + latency + status** (JSON to stdout in production), and the request-id correlates the access line with the existing error line (`http.rs:201`).
- [ ] `GET /_pgweb/metrics` is scrapeable in Prometheus text format with at least request counts, a latency histogram, error counts, and BGW uptime — with bounded label cardinality (no raw-path labels).
- [ ] No `_pgweb/*` route (if any are added for logging/metrics) collides with user routes (all mounted above `.fallback` in `http::app`), and each feature is exercised in `examples/todo/` per the companion-app gate.

**Note:** As of the 2026-06-13 discussion, the value of production request logging + metrics is unclear (potential overkill, exposure concerns on hosted/customer sites, better dev visibility may come from CLI, richer app logging, or the dev dashboard direction in prompt 026). These criteria are retained for when/if the work is re-prioritized. Revisit before implementation.

## Open questions (focused on remaining deferred items)

As of the 2026-06-13 discussion, request logging and metrics are deprioritized. These questions are retained for any future re-evaluation. Consider whether lighter approaches (CLI improvements, per-handler logging, or the dev dashboard in prompt 026) could deliver most of the value with less surface.

1. **`pgweb.request_log` sampling rate + retention.** Default sample rate (off? 1%? 100% only when a setting is on)? Retention: time-based prune, fixed row cap, or `UNLOGGED` table that's acceptable to lose on crash? Who prunes (a BGW tick, a trigger, the CLI)?
2. **Metrics cardinality.** Label request metrics by matched route *pattern* (`/posts/:id`), by status class only, or omit path labels entirely? Dynamic routes make raw-path labels unbounded — what's the bound?
3. **`request_id` source & format.** UUID v4, a monotonic counter, ULID, or honor an inbound `X-Request-Id` (from Caddy in front, `CLAUDE.md` invariant #2) when present? Where is it generated — Axum/`tower-http` middleware or inline in `handle`?
4. **Does the access log live behind the env flag like livereload, or always-on?** Prompt `006` gates its dev-terminal lines on `env == Development`. Should the *production* structured access log be always-on (it's just stdout, cheap) while only the table-write tier is opt-in?
