# 016 — Request-path performance: caching (templates + routes) and graceful shutdown

**Status:** Open work order — low-risk, high-leverage; no API/contract changes
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Prerequisites:** none; complements 015 (concurrency) — do the benchmark in 015 first so wins are measured
**Context:** Every dynamic request currently re-scans `pgweb.routes`, re-parses its Tera template from scratch, and reads the `env` setting 1–2 extra times over SPI — all decisions documented as deliberate Phase-1 simplicity with explicit re-evaluation triggers. This work order is the "now revisit them" pass: add a BGW-local snapshot cache of framework metadata, invalidate it through the `ListenRouter` that already fans out `LISTEN/NOTIFY`, and wire `SIGTERM` into Axum's graceful shutdown so `pg_ctl stop` / `docker stop` drain cleanly.

---

## Summary

The hot request path does measurably redundant work that Phase 1 deliberately left alone (cited below, each with its re-eval trigger). None of it is a correctness bug; all of it is now fair game because the framework is past Phase 1 polish (`v0.2.0`) and the cache-invalidation primitive — the channel-aware `ListenRouter` — already exists in-tree and was built to be reused (`crates/pg_web_ext/src/listen_router.rs:14-17`).

This prompt proposes three coordinated, low-risk changes:

1. A **BGW-local snapshot cache** of `pgweb.routes` (parsed + specificity-sorted, match-ready), `pgweb.templates` (compiled `Tera` instances), and the `env` setting — built lazily and read with **zero SPI on the hot path**.
2. **Invalidation via the existing `ListenRouter`**: a `pgweb_reload` NOTIFY (issued by `pg-web push`) that the worker LISTENs on to drop/rebuild the snapshot. This requires the LISTEN task to run in production, not just dev — which aligns with Phase 2 realtime making LISTEN universal anyway (`docs/internal/sessions/session_6.md:85-89`, Track C).
3. **Graceful shutdown**: route `BackgroundWorker::sigterm_received()` (and/or a tokio signal) into `axum::serve(...).with_graceful_shutdown(...)` so in-flight requests drain and SSE streams close promptly on `pg_ctl stop` / container stop.

All three preserve the user-visible routing/rendering contract exactly. Only **framework metadata** is cached; user data is still read inside the one-request-one-transaction handler call, so invariant #4 is untouched.

## Why this matters now

- **The deferral triggers are being approached or are already cheap to remove.** The router's naïve-scan re-eval trigger is "route count exceeds 1000 OR router match appears in a measured hot path" (`docs/ROADMAP.md:325`; `crates/pg_web_ext/src/router.rs:26-28`). The 015 benchmark is exactly the "measured hot path" gate. Template caching has the same character: `settings.rs:11-13` already names "a BGW-local cache with invalidation" as "the next step … but don't build it pre-emptively." This is that step.
- **The invalidation primitive already exists and was designed for reuse.** `ListenRouter` (`listen_router.rs`) already fans one `LISTEN` connection out to N subscribers in memory. We do not need new infrastructure — we need one more channel and an always-on LISTEN task.
- **It compounds with 015.** Concurrency work (multiple workers / a request-handling pool) multiplies the per-request SPI cost. Cutting routes/template/env lookups to zero per request is the single biggest constant-factor win available without touching the handler contract, and it makes the 015 numbers look much better.
- **Graceful shutdown is an operational papercut today.** `pg_ctl stop` (smart/fast) and `docker stop` send `SIGTERM`; the worker registers a handler for it but never checks it (see below), so the postmaster must escalate to `SIGKILL` after its timeout. That slows every DB restart/redeploy and can truncate in-flight responses.

## Hot-path inefficiencies (evidence)

All four are documented-as-deliberate Phase-1 simplicity. Verify each against the cited lines before acting.

1. **Full route scan + parse + sort, every request.**
   `crates/pg_web_ext/src/router.rs:382-423` — `fetch_method_routes` runs `SELECT path_pattern, handler_name, template_path FROM pgweb.routes WHERE method = <m>`, returning **every** route for that method. `lookup_route` (`router.rs:425-465`) then `ParsedPattern::parse`s each row and `sort_by`s the whole set by specificity **on every request** before the first-match scan. The module doc states the decision and its trigger explicitly (`router.rs:22-28`: "naïve specificity-sorted scan … we revisit if route count exceeds ~1000 or router match becomes a measured hot path"). Decision-log entries: `docs/ROADMAP.md:324` (captures derived from pattern, not stored) and `docs/ROADMAP.md:325` (naïve scan; **re-eval trigger** named verbatim).

2. **Fresh Tera parse, every render.**
   `crates/pg_web_ext/src/templating.rs:13-19` — `render()` calls `Tera::one_off(template_src, &context, true)`, which **tokenizes and parses the template string from scratch every call**. There is no compiled-template cache anywhere; `fetch_template` (`router.rs:488-499`) re-reads the source from `pgweb.templates` over SPI as well. For a hot page this re-parses identical bytes on every request.

3. **Multiple SPI transactions per request; `env` read redundantly.**
   The main dispatch opens a transaction at `router.rs:70` (`BackgroundWorker::transaction(... serve_in_tx ...)`). Then, back in the HTTP layer, `http.rs:127` opens a **second** `BackgroundWorker::transaction(settings::current_env)` purely to read `env` for livereload injection. The asset path opens its own at `http.rs:165`, and the error path another at `http.rs:203`. Each ultimately runs `settings::current_env` → `Spi::get_one("SELECT value FROM pgweb.settings WHERE key = 'env' …")` (`settings.rs:38-43`). The SSE endpoint reads env again per connection (`livereload.rs:161`). Net: a normal templated page = **2 SPI transactions** (router + env), and `env` is fetched on a separate round-trip from everything else.

4. *(Minor)* **Two SELECTs for one row in the 404 path.**
   `crates/pg_web_ext/src/router.rs:470-486` — `lookup_fallback` issues one `SELECT handler_name …` and a second `SELECT template_path …` against the same `method='404' AND path_pattern='/'` row. Trivially collapsible to a single row fetch (and moot once routes are cached, since the fallback row lives in the same snapshot).

## Proposed direction (options)

### A. Scope of the cache

- **A1 — Routes only.** Cache the parsed + sorted route table; keep `one_off` templating and per-request env.
- **A2 — Routes + compiled templates + env (full snapshot).** One `RouteCache` struct holding: the specificity-sorted parsed routes, a `Tera` instance (or `HashMap<String, Tera>`) of compiled templates, and the resolved `Env`. Hot path does zero framework-metadata SPI.
- **A3 — Templates only, content-hash keyed (interim).** Leave routes/env alone; cache compiled `Tera` keyed by `(template_path, content_hash)`, reading just the cheap hash per request and falling back to `one_off` on miss.

**Lean:** A2. The three reads share the same lifecycle (all change only on `pg-web push` / `pg-web env`), so one snapshot with one invalidation channel is simpler than three half-measures, and it's the only option that hits the "0 framework SPI on the hot path" acceptance bar. A3 is a reasonable **smaller first PR** if the reviewer wants to de-risk — it removes the largest single cost (template parsing) with the least surface area and no always-on LISTEN dependency — but it leaves the route scan and env reads in place, so land it only as a stepping stone to A2.

### B. Build timing

- **B1 — Eager at worker start.** Populate the snapshot once in `pg_web_worker_main` before serving.
- **B2 — Lazy on first request, then cached.** First request after start/invalidation pays the build; subsequent requests are free.

**Lean:** B2 with an eager warm-up call right after bind (best of both): the warm-up keeps the first real request fast, while lazy rebuild is the natural shape for invalidation (drop → next request repopulates). Either way the build must run on the SPI-owning thread (see invariant #7 below).

### C. Invalidation transport

- **C1 — Reuse `ListenRouter` + a `pgweb_reload` NOTIFY** issued by `pg-web push`. Worker LISTENs, drops the snapshot on receipt.
- **C2 — TTL / generation counter.** Cache for N seconds, or compare a cheap `max(updated_at)` / table-version probe per request.
- **C3 — `pg_notify` from a trigger** on `pgweb.routes`/`pgweb.templates` instead of from the CLI.

**Lean:** C1. It reuses infrastructure that already exists and is already the project's chosen pattern for "the DB changed, tell the worker" (livereload). C3 (triggers) is appealing for correctness (fires no matter who writes the tables) and worth noting as a future hardening, but C1 is the smaller change and consistent with how push already drives livereload. C2 reintroduces a per-request probe — the exact cost we're removing — so reject it except as a coarse backstop (e.g., a long TTL that bounds staleness if a NOTIFY is ever missed).

## Detailed design notes

- **Cache structure.** Introduce a `cache` module (e.g. `crates/pg_web_ext/src/cache.rs`) owning a snapshot:
  ```text
  RouteSnapshot {
      // per method, already specificity-sorted (the sort currently in lookup_route)
      routes: HashMap<String /*method*/, Vec<(ParsedPattern, RouteMeta)>>,
      // compiled templates keyed by template_path
      templates: HashMap<String, Tera>,
      env: Env,
  }
  ```
  Store behind a cheap interior-mutability cell readable without SPI — a `OnceCell<ArcSwap<RouteSnapshot>>` or `RwLock<Option<Arc<RouteSnapshot>>>`. Because the worker runs a **single-threaded** current-thread tokio runtime (`worker.rs:62-72`), contention is near-zero; even a `RefCell`/`Cell` behind a thread-local would be sound, but `ArcSwap`/`RwLock` keeps the door open for the 015 multi-worker direction without a rewrite. Whatever is chosen, document the concurrency assumption next to it (mirror the `ListenRouter` thread-safety note at `listen_router.rs:26-31`).
- **Refactor `lookup_route` to read from the snapshot.** Today it does fetch → parse → sort → match (`router.rs:425-465`). Split it: snapshot build does fetch+parse+sort once; the per-request path keeps only the final `pat.matches(&req_segs)` first-match scan (`router.rs:453-463`). `ParsedPattern` and its `matches`/specificity sort are already isolated and pure-tested (`router.rs:235-315`, `pure_tests`), so this is a move, not a rewrite — the existing unit tests should pass unchanged.
- **Compiled templates.** Replace `Tera::one_off` (`templating.rs:19`) on the hot path with a lookup into the snapshot's compiled `Tera`. Build each as `Tera::default()` + `add_raw_template(template_path, src)` at snapshot time, then `tera.render(template_path, &ctx)` per request. Keep `one_off` as the **miss/fallback** path so a template pushed-but-not-yet-invalidated still renders (correctness over speed on the cold edge). Preserve the parse-vs-render error classification (`templating.rs:25-62`) — it must still distinguish `TemplateParseError` from `TemplateRenderError`; note that with compiled templates, parse errors now surface at **build time**, so decide whether a bad template fails the whole snapshot build (and falls back to per-request `one_off`, which re-surfaces the same typed error to the user) or is skipped with the slot left empty. **Lean:** build-time parse failure for one template should not poison the others — skip it, leave it to `one_off` on request, which yields the identical dev error page.
- **Collapse env reads.** Read `env` once into the snapshot. Thread it from there into `livereload::inject_script_if_eligible` (already takes `env` as a param — `livereload.rs:209`) and into the asset/error Cache-Control + dev-page branches (`http.rs:165`, `http.rs:203`), replacing the three `BackgroundWorker::transaction(settings::current_env)` calls. The SSE env check (`livereload.rs:161`) can also read the snapshot. **Watch the consistency nuance:** `pg-web env` changes `env`; that write must also fire the reload NOTIFY (or the snapshot must include env's source row) so a dev→prod flip isn't served stale. Keep `settings::current_env` as the snapshot's builder and as the fallback when no snapshot exists.
- **404 fallback.** Fold `lookup_fallback` (`router.rs:470-486`) into the snapshot (the `method='404'` row is just another route); the two-SELECT shape disappears for free. If kept out of the snapshot for any reason, at least collapse it to one row fetch.
- **Where the cache does NOT reach.** The user handler call (`call_handler`, `router.rs:505-578`) stays exactly as-is — it runs the user's SQL inside the request transaction. Static asset bytes (`lookup_asset`, `router.rs:119-164`) are out of scope here (they already have ETag/immutable caching via `pgweb.assets`); caching asset *bodies* in memory is a separate, larger decision — call it out as a non-goal.

## Cache invalidation via the existing ListenRouter

The `ListenRouter` (`crates/pg_web_ext/src/listen_router.rs`) already does exactly the fan-out we need and is explicitly documented as Phase-2-reusable (`listen_router.rs:14-17`, and `docs/internal/sessions/session_6.md:15`). Reuse it:

- **Add a `pgweb_reload` channel** (name TBD — see Open questions). The worker `preregister`s it (`listen_router.rs:122-127`) and the LISTEN task issues `LISTEN pgweb_reload` alongside `pgweb_livereload`. On any payload, the worker drops/rebuilds the snapshot (lazy rebuild = just clear it; next request repopulates).
- **`pg-web push` issues the NOTIFY.** Today **only `pg-web dev` NOTIFYs** — it fires `NOTIFY pgweb_livereload, '<json>'` after each watcher-driven push (`crates/pg_web_cli/src/dev.rs:365-383`). A plain `pg-web push` does **not** notify anything (verified: `push.rs` contains no NOTIFY). For a production cache, `push` itself must emit the reload signal after a successful commit. The push is a **single transaction** that reconciles routes + templates + functions and `COMMIT`s once (`push.rs:244-305`), so there is no torn intermediate state to observe — the NOTIFY fires after COMMIT (or as the last statement before it, inside the tx; decide which — in-tx NOTIFY is delivered on commit by Postgres and avoids a second connection, but couples delivery to the tx; post-commit NOTIFY on a short-lived connection mirrors `notify_livereload` at `dev.rs:365`). **Lean:** in-transaction `NOTIFY pgweb_reload` as the final statement before COMMIT — Postgres delivers queued notifications atomically at commit, so it can't fire for a rolled-back push, and it needs no extra connection.
- **The LISTEN task must run in production.** Today it is **dev-only**: `worker.rs:92-105` spawns `run_listen_loop` only when `env == Env::Development`, and the module docs make "prod = zero extra backends" a feature (`listen_router.rs:19-24`, `worker.rs:88-105`). A prod cache needs the LISTEN task always-on, which costs **one extra Postgres backend slot per worker** in production. This is the **same cost Phase 2 Track C already commits to** ("BGW always opens its LISTEN connection on startup … The +1 PG backend slot cost is paid in production too" — `docs/internal/sessions/session_6.md:88-89`). Cross-reference it: doing this here is a down-payment on Track C, not new debt. Note the cost in `docs/APP-DEVELOPER-GUIDE.md` § Pushing (where the dev-only cost is documented today, per `listen_router.rs:22-24`).
- **Consistency model.** Between a push's COMMIT and the worker draining the NOTIFY, a stale snapshot can serve old routes/templates for a few milliseconds (one NOTIFY round-trip). **Is that acceptable?** **Lean: yes** — eventually-consistent within one NOTIFY round-trip. The push is transactional (no half-applied route set is ever visible), live-reload already has exactly this property and nobody minds, and the failure mode is "the previous valid version serves for a few more ms," never a torn or invalid state. Bound the worst case (a dropped NOTIFY — the broadcast buffer is only 8 deep, `listen_router.rs:45`, and lagged receivers drop) with an optional coarse TTL backstop or a generation check on the next push. Document the chosen guarantee.

## Graceful shutdown

**Current behavior (the gap):** `pg_web_worker_main` calls `BackgroundWorker::attach_signal_handlers(SIGHUP | SIGTERM)` (`worker.rs:48-50`), then enters `rt.block_on(async { … axum::serve(listener, http::app(router)).await … })` and **blocks there forever** (`worker.rs:76-112`). Nothing ever calls `BackgroundWorker::sigterm_received()` — verified: that symbol appears nowhere in the tree — and nothing wires the signal into Axum. So on `pg_ctl stop` / `docker stop`, `SIGTERM` is effectively ignored until the postmaster escalates (SIGQUIT/SIGKILL after its shutdown timeout), slowing DB shutdown/restart and risking truncated in-flight responses.

**Proposed:** use Axum's built-in drain:
```text
axum::serve(listener, http::app(router))
    .with_graceful_shutdown(shutdown_signal())
    .await
```
where `shutdown_signal()` is an async fn that completes when the worker should stop. Two complementary triggers, whichever fires first:
- **Postgres SIGTERM** — poll `BackgroundWorker::sigterm_received()` (it returns whether the SIGTERM flag is set since `attach_signal_handlers` armed it) on a short interval (e.g. a `tokio::time::interval` of ~100–250ms) and complete the future when true. A poll loop is the pragmatic shape because the pgrx flag is checked, not awaited; keep the interval small enough to be responsive and large enough to be free.
- **(Optional) tokio signal** — also `select!` on `tokio::signal::ctrl_c()` / a `SignalKind::terminate()` stream so a direct container `SIGTERM` to the process is honored even outside the postmaster's path.

On signal, `with_graceful_shutdown` stops accepting new connections and lets in-flight handlers finish. **SSE streams must also close** — the livereload (and future realtime) streams otherwise hold the server open until their 2-hour hard cap (`livereload.rs:170-179`). Give the SSE stream a shutdown-aware `take_until`/broadcast-close so it ends on shutdown, not just at the 2h ceiling. Cap the total drain with a timeout (e.g. wrap the serve future in `tokio::time::timeout`, a few seconds) so a stuck handler can't outlast the postmaster's own shutdown window and force a SIGKILL anyway. After `block_on` returns, the worker function returns normally and the process exits cleanly.

**Interaction with the LISTEN task:** the always-on `run_listen_loop` (now also prod) loops forever reconnecting (`listen_router.rs:142-223`). On shutdown it's an orphaned tokio task; since the runtime is torn down when `block_on` returns and the worker exits, it dies with the process. If you want clean teardown logging, give it a shutdown signal too (e.g. a `tokio::sync::Notify` or a watch channel) — optional, low priority.

## Research tasks

1. **Confirm the pgrx API surface.** Verify `BackgroundWorker::sigterm_received()` (and `sighup_received()`) exist and behave as "has the flag been set since arm" in the pgrx version this repo pins (check `Cargo.toml` / `Cargo.lock` for the `pgrx` version, then that version's `bgworkers` module). Confirm `with_graceful_shutdown` exists in the pinned `axum` version (it's stable in axum 0.6/0.7+). Adjust the polling vs awaiting shape to whatever pgrx actually offers (older pgrx exposed `wait_latch`-style helpers).
2. **Benchmark first (gates everything).** Run the 015 benchmark against an app with a non-trivial route count and a templated hot page, both before and after. Capture: requests/sec, p50/p99, and SPI calls per request (e.g. via `pg_stat_statements` or tracing). This is the "measured hot path" evidence the router's re-eval trigger asks for (`router.rs:26-28`, `ROADMAP.md:325`).
3. **Decide cache concurrency primitive** against the 015 outcome. If 015 stays single-worker, a thread-local/`RefCell` is enough; if it goes multi-worker or pooled, pick `ArcSwap`/`RwLock` and confirm each worker maintains its **own** snapshot + its **own** LISTEN subscription (see Open questions).
4. **Map every `current_env()` / `settings::current_env` call site** and confirm all can be served from the snapshot or its fallback: `http.rs:127`, `http.rs:165`, `http.rs:203`, `livereload.rs:161`, plus the seed/test reads. Ensure `pg-web env` writes fire the reload NOTIFY.
5. **Trace the push→NOTIFY→drain path end-to-end** in a running stack: `pg-web push` a new route, confirm it serves without a worker restart; delete a route, confirm 404. Decide in-tx vs post-commit NOTIFY (see Detailed design notes).
6. **Decide the invalidation granularity.** Whole-snapshot drop (simple) vs per-table payloads (`{"kind":"routes"}` / `{"kind":"templates"}` / `{"kind":"env"}`, mirroring livereload's `kind` payloads at `dev.rs:336-363`) so a CSS-only push doesn't rebuild the route table. **Lean:** start with whole-snapshot drop (rebuild is cheap and rare); add `kind` granularity only if a profile shows rebuild cost matters.

## Constraints & invariants to respect

From `CLAUDE.md` § Architectural invariants — DO NOT VIOLATE:

- **#4 — One HTTP request = one SPI transaction.** The cache must not break transactional reads of **user** data. Only **framework metadata** (`pgweb.routes`, `pgweb.templates`, `pgweb.settings.env`) is cached; the user handler still runs its own SQL inside the request transaction (`call_handler`, `router.rs:505-578`). Caching framework metadata that changes only on `push`/`env` does not violate this — it removes *bookkeeping* SPI, not *data* SPI. Call this out explicitly in the design doc so a reviewer doesn't read "cache" as "cache user query results."
- **#5 — Zero network hop inside the extension ("Using `libpq` from the extension is a correctness bug").** There is an existing tension: the LISTEN task **already** opens a `tokio-postgres` (libpq-protocol) connection to `127.0.0.1` (`listen_router.rs:130-150`, `worker.rs:127-143`) — today only in dev. Making it always-on for the prod cache extends that tension into production. The connection is **not** SPI and the module docs already justify it ("regular libpq-protocol session to loopback — so there's no SPI conflict", `listen_router.rs:135-138`). **Recommendation:** amend invariant #5 to carve out the dedicated LISTEN connection explicitly (it's a notification side-channel, not a data path; SPI remains the only way request handlers read/write user data). Raise this with the maintainer before shipping — per CLAUDE.md, touching an invariant requires a flag-and-confirm. Phase 2 Track C already assumes this carve-out (`session_6.md:88-89`), so the amendment is coming regardless; this work order is a good moment to make it.
- **#7 — Async only in the background worker.** All of this lives in the BGW (`worker.rs`, the tokio runtime, the LISTEN task) — never inside a `#[pg_extern]`. The snapshot **build** runs SPI, so it must execute on the SPI-attached thread (`BackgroundWorker::connect_worker_to_spi`, `worker.rs:52-56`; the runtime is current-thread for exactly this reason, `worker.rs:62-72`). Do not build or refresh the snapshot from a spawned task that could migrate off the SPI thread.
- **No premature abstraction / keep modules flat** (CLAUDE.md § Coding practices). One `cache` module, plain types; don't introduce a trait-based cache layer.
- **Phase discipline.** This is Phase-1-era performance hardening of existing paths, not a Phase 2 feature — but it deliberately borrows the Phase 2 "LISTEN is universal" decision. Frame the always-on LISTEN as bringing forward a Track C decision, and keep the cache itself free of any auth/RLS/realtime coupling.

## Acceptance criteria

- A templated hot-path page render performs **0 SPI lookups for routes, template, or env** (only the user handler's own SPI runs inside the request transaction) — verified by tracing / `pg_stat_statements` before vs after.
- A `pg-web push` is reflected within **one NOTIFY round-trip without a worker restart**: pushing a new route serves it; deleting a route returns 404 — covered by a `#[pg_test]` and/or tier-3 E2E.
- `pg_ctl stop` (smart/fast) and `docker stop` shut the worker down **promptly and cleanly**, draining in-flight requests and closing SSE streams, with **no SIGKILL escalation** within the postmaster's normal shutdown window.
- The 015 benchmark shows a measurable throughput / p99 improvement on a route-heavy, template-heavy app, with the numbers recorded.
- **No behavior change** to user-visible routing, rendering, error pages (parse vs render classification preserved), or asset caching — existing `router.rs`/`templating.rs` unit tests pass unchanged.
- The compiled-template fallback path is correct: a template pushed but not yet invalidated still renders (via `one_off`), and a template with a syntax error surfaces the **same** typed `TemplateParseError` dev page it does today.
- Invariant #5 is either honored or explicitly amended (with maintainer sign-off) to carve out the LISTEN connection; the prod LISTEN backend-slot cost is documented in `docs/APP-DEVELOPER-GUIDE.md`.
- A `#[pg_test]` (or pure-Rust test) covers cache correctness and the env-collapse, and `examples/todo/` still passes its tier-3 flow (per CLAUDE.md "every feature ships with a companion-app flow").

## Open questions

1. **Cache memory bound.** Compiled `Tera` instances and the parsed route table grow with app size. Is any cap or eviction needed, or is "framework metadata is small; hold it all" fine at expected scale? (Lean: no cap — it's the same data already in `pgweb.*`, just parsed; a 1000-route app is the documented upper bound before other rework anyway.)
2. **Per-worker cache coherence across the N workers from 015.** If 015 introduces multiple worker processes / a pool, each has its own snapshot and must hold its **own** LISTEN subscription so a single `pgweb_reload` NOTIFY reaches all of them (Postgres delivers a NOTIFY to every listening backend). Confirm the fan-out is per-process, not shared, and that one push invalidates **all** workers — or does the pool share one BGW? This depends on 015's chosen topology.
3. **Invalidation channel naming.** `pgweb_reload`? `pgweb_cache_invalidate`? Reuse/extend `pgweb_livereload` with a richer `kind` payload (one channel, `{"kind":"routes"|"templates"|"env"|"css"|"full"}`) vs a dedicated channel? A separate channel keeps prod (cache) and dev (livereload) concerns cleanly split; one channel is fewer `LISTEN`s. (Lean: dedicated `pgweb_reload` for cache, leave `pgweb_livereload` for the browser.)
4. **In-transaction vs post-commit NOTIFY from `push`.** In-tx is atomic-on-commit and connection-free; post-commit mirrors the existing `notify_livereload` helper. Which fits the CLI's transaction handling (`push.rs:244-305`) best?
5. **Dropped-NOTIFY backstop.** The broadcast buffer is 8 deep and lagged receivers drop (`listen_router.rs:45`, `livereload.rs:186-198`). For a cache (vs livereload) a missed invalidation is more serious — serving a stale route until the *next* push. Is a coarse TTL or a per-request generation check (cheap `txid`/version probe) worth adding as a safety net, accepting it reintroduces a tiny periodic SPI cost? (Lean: optional long TTL backstop only; full correctness via the always-fired push NOTIFY.)
6. **Should plain `pg-web push --no-livereload` still fire `pgweb_reload`?** The browser-reload suppression flag exists (`main.rs:91`), but cache invalidation is orthogonal to browser reload — a `--no-livereload` push must still invalidate the server cache or it silently serves stale routes. Confirm the flag gates only the *browser* NOTIFY, not the *cache* NOTIFY.
