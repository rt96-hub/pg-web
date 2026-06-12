# 015 — Concurrency model & throughput: benchmark first, then a multi-worker design

**Status:** Open work order — the framework's load-bearing performance constraint
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Prerequisites:** pairs with 014 (statement_timeout) and 016 (request-path caching); benchmark step has none
**Context:** `docs/VISION.md:58` asserts a 1-vCPU / 2 GiB VPS can sustain 1,000 req/s of fetch-and-render traffic against the demo app — flagged "(Target — to be benchmarked.)". That benchmark does not exist, and the serving path processes exactly one request at a time on a single OS thread with no per-request timeout. This prompt is two jobs in priority order: (1) build the benchmark and either validate or correct the claim, then (2) design a way past the single-thread ceiling that respects the SPI thread-affinity constraint that makes the single thread mandatory.

---

## Summary

pg-web serves every HTTP request on **one OS thread inside one background worker**, over a **single-threaded** Tokio runtime. This is not an oversight — it is forced by Postgres: only the thread attached via `connect_worker_to_spi` may issue SPI calls, so a multi-thread runtime would migrate a task onto a thread with no SPI backend and panic on the first `Spi::*` call (`worker.rs:60-62`). Worse, the per-request SQL work is a **synchronous, blocking** call (`BackgroundWorker::transaction`) executed directly on the event-loop thread (`router.rs:67-71`, invoked from `http.rs:120` with nothing `await`ed around it). The net effect is stronger than the ROADMAP's framing of the Phase-1 limitation ("handlers that call external APIs will block the worker thread", `ROADMAP.md:170`): while **any** handler's SQL runs, **nothing else on the runtime makes progress** — not other page requests, not static-asset serving, not SSE keepalives, not 404s. The whole site is serialized behind the slowest in-flight request. And because no `statement_timeout` is set anywhere on the serving path, a locked row, a bad plan, or a `pg_sleep()` in a handler wedges the entire web tier indefinitely.

To be fair and precise: at low concurrency with sub-millisecond SPI handlers this is genuinely fine, and sequential throughput on fast handlers could still be high. The real problems are (a) **tail latency under mixed traffic**, (b) **zero isolation** between one slow request and every other client, and (c) the **1k-req/s claim is unverified**. This project's honest documentation (the "(Target — to be benchmarked.)" caveat itself, the known-limitations sections) is a brand asset. Extend that honesty to performance: measure it, publish it, and design the fix in the open.

This work order is sequenced deliberately. **Step 1 is the benchmark — do it first, and treat its output as shippable even if Step 2 never happens.** Step 2 is a concurrency design that you write up and (optionally) implement *after* you have numbers, because the numbers decide how much the design is worth and what default to pick.

## Why this matters now

- **A headline success criterion is unverified.** `docs/VISION.md:54-59` lists the 1k-req/s target as a v1.0 success criterion. Shipping toward v1.0 while the single load-bearing performance number is a guess is exactly the kind of thing the rest of this repo refuses to do elsewhere. Either we can hit it (great — publish proof) or we can't (then VISION must say so, honestly).
- **The serialization is invisible until it isn't.** Every existing test exercises one request at a time (tier 2a HTTP smoke, tier 3 Docker E2E driving sequential CRUD — `scripts/test-all.sh`). Nothing in the suite puts concurrent load on the worker, so the head-of-line-blocking behavior has never been observed, measured, or regression-guarded. The first time anyone notices is in production, under real concurrency, with a slow query.
- **It composes with two sibling work orders.** 014 (statement_timeout) is the per-request guard that stops one wedged query from taking the site down forever. 016 (request-path caching) cuts how often we pay the SPI cost at all. This prompt (015) is the structural ceiling. The three together are the Phase-3 "survives real-world traffic" story (`ROADMAP.md:227-240`); none of them is complete alone.
- **The design space is constrained and worth getting right.** The obvious "just use a multi-threaded runtime" is **off the table** (it panics — see below). The real lever is *more processes*, not more threads, and that touches worker registration, Postgres connection budgeting, and the livereload LISTEN design. Better to reason about it deliberately than to bolt it on later.

## Current behavior — the serialization (evidence)

Read these before proposing anything. The constraint is physics, not preference.

1. **Single-threaded runtime, by necessity.** `crates/pg_web_ext/src/worker.rs:63-72` builds the runtime with `tokio::runtime::Builder::new_current_thread()`. The comment immediately above (`worker.rs:60-62`) states the reason verbatim:

   > Single-threaded current-thread runtime: all async tasks run on this thread, the one with SPI attached. A multi-threaded runtime would let tasks migrate to worker threads that lack SPI access, causing panics on any SQL call.

   The SPI attachment happens once, at `worker.rs:55-56` (`BackgroundWorker::connect_worker_to_spi(...)`), binding **this one OS thread** to a Postgres backend. That is the hard constraint the whole design orbits.

2. **Per-request work is synchronous and runs on the event-loop thread.** The Axum fallback handler `http::handle` (`crates/pg_web_ext/src/http.rs:69-143`) builds the `req` JSON and then calls `router::serve(&method, &path, req_value)` at `http.rs:120`. That call is **not** `await`ed against anything and is **not** wrapped in `spawn_blocking` — it runs inline. Inside, `router::serve` (`crates/pg_web_ext/src/router.rs:67-71`) is:

   ```rust
   pub fn serve(method: &str, path: &str, req: Value) -> ServeOutcome {
       // ...
       BackgroundWorker::transaction(move || serve_in_tx(&method, &path, req))
   }
   ```

   `BackgroundWorker::transaction` is a **synchronous, blocking** call that opens an SPI transaction, runs every route lookup and the handler call over SPI (`fetch_method_routes`, `lookup_asset`, `call_handler` → `pgweb._framework_call_handler`), and commits/rolls back — all before returning. While it runs, the single-threaded Tokio reactor cannot poll any other future. (Note: `http.rs` opens *additional* synchronous `BackgroundWorker::transaction(settings::current_env)` calls per request at `http.rs:127`, `http.rs:165`, `http.rs:203` — more serialized SPI round-trips on the same thread, relevant to 016.)

3. **Therefore everything is serialized behind the slowest in-flight request.** Concretely, while one handler's SQL is executing:
   - other page requests wait,
   - static-asset responses wait (`ServeOutcome::Asset`, served from `pgweb.assets` — also over SPI),
   - the livereload SSE stream's 30-second keepalive (`crates/pg_web_ext/src/livereload.rs:181-183`) cannot fire,
   - a hardcoded 404 cannot be written.

   This is materially worse than `ROADMAP.md:170`, which frames the limitation only as external-API calls blocking *the worker thread*. The accurate statement is: **any** slow SQL — not just external I/O — serializes **the entire site**.

4. **No `statement_timeout` anywhere on the serving path.** Grep the extension: there is no `SET statement_timeout`, no `SET LOCAL statement_timeout`, no timeout GUC applied around `BackgroundWorker::transaction` or inside `_framework_call_handler`. A handler that hits a locked row, plans pathologically, or literally calls `pg_sleep(60)` holds the one thread for the full duration, and every concurrent client sees their request hang. **The fix for this is specified in prompt 014 — do not re-spec it here; reference it.** It is the per-request guard that bounds the blast radius of (3).

5. **Exactly one worker is registered.** `crates/pg_web_ext/src/lib.rs:39-45` registers a single static/dynamic BGW named `pg_web_worker` (one `BackgroundWorkerBuilder::new("pg_web_worker")`, `.enable_spi_access()`, `set_restart_time(5s)`). The port is hardcoded (`worker.rs:21`, `const HTTP_PORT: u16 = 8080;`) and the listener binds `0.0.0.0:8080` once (`worker.rs:74-83`). There is exactly one HTTP acceptor, on one thread, in one process. That is the whole serving capacity of the system today.

6. **Livereload's LISTEN connection is singular and dev-only.** `worker.rs:92-105` starts the `run_listen_loop` task **only** when `env == Development`, and it opens **one** tokio-postgres connection (the only place the worker uses a network `postgres://` connection rather than SPI — and it is explicitly out-of-band, not on the request path). It fans out in memory to N SSE tabs via a `broadcast` channel (`livereload.rs:156-199`). In production this task does not run, so prod costs **zero** extra Postgres backend slots from this machinery. Any multi-worker design has to decide what happens to this single LISTEN connection when there are K workers (see Step 2, Option A).

## Step 1: build the benchmark (do this FIRST)

The deliverable of Step 1 is a **reproducible, publishable benchmark** and a `docs/BENCHMARKS.md` that either validates `VISION.md:58` or replaces the claim with a measured one. This is valuable on its own and must not be blocked on Step 2.

### Hardware tiers
- **Primary (the claim under test):** 1 vCPU / 2 GiB — match `VISION.md:58` exactly. A pinned cloud instance (e.g. a 1-vCPU shared/dedicated VPS) or a cgroup/`--cpus=1 --memory=2g` Docker constraint on a bigger box. State which, because shared-vCPU steal vs a dedicated core changes the numbers; prefer a dedicated 1 vCPU and document the instance type.
- **Comparison tier:** a beefier box (e.g. 4–8 vCPU / 16 GiB) to show how throughput scales — and, critically, to show how it **fails to scale** today (one thread can't use more cores), which is the empirical motivation for Step 2.

### Workloads (run each in isolation, then at least one mixed run)
- **(a) Static template render, no table read.** A handler that returns a constant JSON object rendered through a trivial Tera template. Isolates Tera + the HTTP/SPI framing overhead from query cost. (Add a dedicated `pages/bench/static` route in a bench app; do not contort the todo example.)
- **(b) The todo list GET.** `examples/todo/pages/index.sql` — a single indexed read → `json_agg` (the function is `STABLE`) → Tera (`pages/index.html`). This is the literal "fetch and render" the VISION claim is about. Seed a realistic row count (e.g. 100 and 10,000 todos — report both; `json_agg` cost grows with rows).
- **(c) A write path.** `POST /todos` (`examples/todo/pages/todos/post.sql`, a plpgsql `INSERT ... RETURNING`). Exercises a real write transaction and commit on the serving path. (Decide how to keep the table from growing unbounded across a long run — e.g. truncate between runs, or a bench-only insert-then-rollback handler; document the choice.)
- **(d) A deliberately slow handler.** A bench-only route whose SQL does `pg_sleep(0.2)` (or similar). This is the **head-of-line-blocking demonstrator**: run it at low rate **concurrently** with workload (b), and show that (b)'s latency distribution craters even though (b)'s own query is microseconds. This single experiment is the most important graph in the whole document — it makes the serialization visible and undeniable.

### Tool & metrics
- **Pick one of `wrk` / `oha` / `k6` / `vegeta` and justify it.** Recommended default: **`oha`** (Rust, single static binary, prints p50/p90/p95/p99/p99.9 and a latency histogram out of the box, trivial to script, no JS runtime) — fits the "one binary, no Node" ethos. `wrk` is fine but needs a Lua script for latency percentiles and good mixed workloads; `k6` is the most expressive (scripted scenarios, thresholds) at the cost of a heavier dependency; `vegeta` excels at constant-rate (open-model) load, which is the *right* model for the slow-handler test (see below). **Justify the pick in `docs/BENCHMARKS.md`; do not just assert it.**
- **Measure throughput AND latency percentiles** — p50/p95/p99/p99.9, not just mean req/s. The mean hides exactly the tail-latency problem this work order is about.
- **Open vs closed model matters for the slow-handler test.** A closed-model tool (fixed N connections looping) will *self-throttle* behind the slow handler and understate the damage; a constant-arrival-rate (open-model) generator (`vegeta`, or `k6` with `constant-arrival-rate`, or `oha -q`) reveals the queue blow-up and rising p99 honestly. If the chosen tool is closed-model, run the slow-handler scenario with an open-model tool specifically, and say so.
- **Show the serialization explicitly:** for workload (b), plot the latency distribution at **1 concurrent client vs N** (N = 2, 4, 8, 16, 64). On a single thread, p50 should rise roughly linearly with N (each request waits behind those ahead of it) while throughput plateaus at ~`1 / mean_service_time` regardless of N. That plateau *is* the ceiling Step 2 attacks.

### Where it lives & how it runs
- A top-level **`bench/`** directory: the bench app (`bench/app/` with `pages/bench/*`), the load scripts (`bench/run.sh` or per-workload scripts), a seed script, and a results template. (`bench/` does not exist today — you are creating it.)
- A documented entry point: a **`make bench`** target (or `bench/run.sh`) that boots the stack (reuse the `examples/todo/docker-compose.yml` shape — `pgweb/postgres:latest`, `POSTGRES_DB`, `:8080` published), seeds, runs each workload, and writes results.
- **Wire a smoke version into the test story.** `scripts/test-all.sh` runs five tiers; tiers 3 and 4 already require Docker + `pgweb/postgres:latest`. Add a **bench-smoke tier** (very short duration, asserts only that the harness runs end-to-end and that a regression threshold isn't blown — e.g. p99 under a generous bound), **or** keep a heavier benchmark as a separate **manual/opt-in tier** (gated behind an env flag like `RUN_BENCH=1`) so CI time doesn't balloon. Decide which and document it. The goal is that a future change which accidentally tanks throughput is *caught*, not discovered in prod.

### Output
- **`docs/BENCHMARKS.md`** — method (hardware, tool, tool version, exact commands, seed sizes, Postgres config), the numbers (a table per workload × concurrency with p50/p95/p99/p99.9 + req/s), the head-of-line-blocking graph, and **honest caveats** (shared vs dedicated vCPU, warm vs cold cache, single-run variance, what was *not* measured).
- **Reconcile `VISION.md:58`.** If 1k req/s holds on workload (b) at the 1-vCPU tier: cite the number and drop "(Target — to be benchmarked.)". If it does not: update the line to the measured reality (e.g. "sustains ~X req/s of fetch-and-render on a 1-vCPU VPS; the single-thread model is the ceiling — see 015") in the **same** change. Do not leave the claim floating.

## Step 2: concurrency design (options)

Do this **after** Step 1. The numbers decide whether this is urgent or merely planned, and what the default worker count should be. Every option must respect the SPI-thread-affinity constraint: **parallelism comes from more *processes*, each with its own SPI backend and its own single-thread runtime — not from more threads in one process.**

### Option A — N background workers behind `SO_REUSEPORT`
Register **K** background workers (configurable; sensible default ≈ vCPU count). Each is its own OS process owned by the postmaster, each calls `connect_worker_to_spi` to get **its own** SPI backend on its own thread, each builds **its own** `new_current_thread()` Tokio runtime, and each binds `0.0.0.0:8080` with the **`SO_REUSEPORT`** socket option. The kernel then load-balances incoming connections across the K listeners. No shared memory is needed for request serving because all application state lives in the database — each worker independently opens its own SPI transaction per request (invariant #4 still holds, per worker). A slow request now stalls only **one** of K workers; the other K-1 keep serving. This is the **real throughput answer**.

Design surface to work through:
- **Worker registration.** `lib.rs:39-45` registers exactly one worker today. Change `_PG_init` to loop K times, giving each a distinct name (`pg_web_worker_0..K-1`) and passing its index as the BGW `set_argument` Datum (the index lets a worker log/identify itself; the *port is shared*, so the index is for diagnostics, not binding). Each still `.enable_spi_access()` and `.set_restart_time(...)`.
- **`SO_REUSEPORT` binding.** Tokio's `TcpListener::bind` does not set `SO_REUSEPORT`. Build the socket via `socket2` (set `SO_REUSEPORT` — and likely `SO_REUSEADDR` — before `bind`), then convert to `tokio::net::TcpListener`. Confirm `socket2` is an acceptable dependency (it is small, widely used, and pure-ish) and that this works on Linux (the deployment target — Docker on a VPS). macOS `SO_REUSEPORT` semantics differ (last-bind-wins vs load-balance); dev on macOS may need a fallback (e.g. K=1 in dev, or accept the difference). Document this.
- **GUC for K.** The worker count must be configurable. Postgres custom GUCs (`pgweb.workers`) are typically defined in `_PG_init` and, because they change worker *registration*, will require a **restart** (not just SIGHUP) to take effect — registration happens once at postmaster start. Spell out the reload policy: changing `pgweb.workers` needs a full Postgres restart. Default: pick after Step 1 shows the per-core scaling.
- **Connection budgeting (the real cost).** Each worker consumes **one Postgres backend slot** for its SPI connection, counted against `max_connections`, and one slot against `max_worker_processes`. K workers = K slots permanently held, *plus* the dev-only livereload LISTEN slot. On a 2 GiB VPS where `max_connections` might be ~100, K=4 is cheap; K=64 is not, and also leaves fewer connections for anything else. Document the budgeting math and recommend bounds (e.g. default K = min(vCPU, some cap), warn if `pgweb.workers` approaches a fraction of `max_worker_processes`/`max_connections`). The Docker image's default `postgresql.conf` may need `max_worker_processes` bumped to comfortably fit K + Postgres's own background workers.
- **Restart policy.** Each worker independently has `set_restart_time(Some(5s))` today. With K workers, a crash takes out 1/K of capacity for ~5s. Confirm that's acceptable and that a crashing worker doesn't thrash (a poison-pill request that panics every worker in turn would be bad — though invariant #4's per-request rollback and 014's timeout mitigate this).
- **Livereload across K workers.** This is the trickiest interaction. Today exactly one worker runs the single LISTEN loop (`worker.rs:92-105`). With K workers and `SO_REUSEPORT`, a browser's `/_pgweb/livereload` SSE connection lands on **one arbitrary worker**, and a NOTIFY only reaches the worker(s) actually running a LISTEN loop. Options: (i) only worker index 0 runs the LISTEN loop *and* only worker 0 serves SSE — but `SO_REUSEPORT` can't pin the SSE route to one worker; (ii) **every** worker runs its own LISTEN loop (K LISTEN connections in dev — costs K dev slots, but each worker can then serve SSE for whatever tabs the kernel routed to it); (iii) keep livereload as a known dev-only single-worker affair and accept that with K>1 in dev some reload events may not reach all tabs. Since livereload is **dev-only and prod runs zero LISTEN connections**, the simplest defensible answer is: **in dev, force K=1** (no `SO_REUSEPORT` fan-out, single LISTEN, today's behavior exactly); **in prod, K>1** with no LISTEN loops at all. Evaluate this against (ii). Whatever you choose, prod correctness must not depend on livereload, and dev ergonomics (reliable hot reload) must not regress.

**Lean:** A is the throughput mechanism. Default K ≈ vCPU count in prod, K=1 in dev. The cost is K backend slots — acknowledge it explicitly and budget for it.

### Option B — bounded work queue + backpressure inside one worker
Keep the single SPI thread, but place a **semaphore/bounded queue** in front of the `BackgroundWorker::transaction` call so the runtime can keep serving the cheap, non-SPI fast paths (the livereload JS static stub, SSE keepalives, hardcoded 404s) while a **bounded** number of SPI calls are queued. **Be honest about what this does and does not buy:** because SPI is synchronous on the one thread, this adds **no parallelism** — only one SPI call can ever execute at a time regardless of queue depth. What it buys is **graceful degradation**: bound the queue, and when it's full, return **`503 Service Unavailable`** (with `Retry-After`) instead of letting unbounded requests pile into kernel/accept buffers and time out opaquely. It also lets truly non-SPI responses (e.g. the static livereload JS) jump the queue and stay fast.

This is precisely the **"Internal concurrency management"** deliverable already sketched in `ROADMAP.md:238`:

> HTTP-level queue inside the web worker's Tokio runtime. Traffic spikes absorbed at the web tier before opening SPI transactions — prevents Postgres connection exhaustion.

Connect to it; don't reinvent it. Note that with the multi-worker model (A), "connection exhaustion" is less about one worker (each worker = one SPI backend, so a single worker can't exhaust connections by itself) and more about bounding total in-flight work and shedding load cleanly. B is the **overload safety valve**, not a throughput feature.

**Lean:** B is the graceful-degradation tool (return 503 under overload, keep fast paths alive), not a way to go faster. Implement it per-worker; it composes with A.

### Option C — offload blocking handler SQL (NOT available)
The intuitive escape hatch — move a slow handler's SQL onto a background thread via `spawn_blocking` so the reactor stays free — **does not work here**, because SPI can only run on the one attached thread (`worker.rs:60-62`). You cannot offload SPI off its thread. Therefore **external-API blocking inside handlers remains a real limitation** that A and B do not solve; the answer for that is still the **Phase-3 async job queue** (`ROADMAP.md:170`, `ROADMAP.md:227-240`): handlers enqueue work into `pgweb.jobs` and return immediately; a separate worker (or worker pool) with its own SPI session drains the queue and performs the external call out of band. Reference it; do not design it here.

**Composition (the actual recommendation):**

**Lean:** measure first (Step 1); then **SO_REUSEPORT multi-worker (A)** as the throughput path **+ a bounded queue (B)** as the per-worker overload valve **+ statement_timeout (014)** as the per-request guard. A adds cores, B sheds load cleanly, 014 bounds any single request's blast radius. The async job queue (Phase 3) remains the separate answer for external-API blocking. None of these is a substitute for the others.

## Detailed design notes

- **Invariant #4 is preserved by A, per worker.** "One HTTP request = one SPI transaction" holds independently inside each of the K workers — each opens, commits, or rolls back its own transaction on its own backend. K workers do **not** share a transaction; they share nothing but the listening port and the database itself.
- **Invariant #7 is preserved.** Async stays inside the BGW(s). A multiplies BGWs; it does not introduce tokio into `#[pg_extern]` backend functions.
- **Invariant #5 is preserved.** Serving stays on SPI. The only `postgres://` connection in the codebase is the dev-only livereload LISTEN loop (`worker.rs:127-143`), which is out-of-band and not on the request path; A must not add a network connection to the serving path.
- **`SO_REUSEPORT` vs a userspace load balancer.** `SO_REUSEPORT` keeps everything in-process-group and needs no extra moving parts — good fit for the "one container" thesis. The alternative (one acceptor that hands connections to workers) would require shared-memory or socket-passing plumbing and reintroduces a single-threaded accept bottleneck; reject it unless `SO_REUSEPORT` proves unworkable on the target kernel.
- **Per-worker connection accounting.** Make the relationship explicit in docs: **prod backend slots used by pg-web = K** (one SPI backend per worker). Dev adds the livereload LISTEN slot(s). This must be reconciled with the image's `max_connections` / `max_worker_processes` defaults and called out in `docs/DEPLOYMENT.md`.
- **Health/probe interaction.** A load balancer in front (Caddy) wants a cheap health endpoint; `ROADMAP.md:239` lists `/_pgweb/health` and `/_pgweb/metrics` as Phase-3 deliverables. With K `SO_REUSEPORT` workers a health probe hits a *random* worker — fine for liveness, but consider whether per-worker health (is *this* worker's SPI backend alive?) matters and whether the probe should be a fast non-SPI path so a saturated worker still answers it (ties into B).
- **The slow-handler test becomes the acceptance test.** The Step-1 head-of-line-blocking experiment (workload d concurrent with b) is exactly the test that should **flip** once A lands: with K>1 workers, a single slow handler occupies one worker while the others keep serving b at low latency. Keep that experiment; it's the before/after proof.

## Research tasks for the implementing session

1. **Re-read the constraint and confirm nothing has changed:** `worker.rs:46-113` (runtime build + bind + LISTEN spawn), `router.rs:67-71` (`serve` → `BackgroundWorker::transaction`), `http.rs:69-143` (handler, and the *extra* per-request `transaction` calls at `:127`, `:165`, `:203`), `lib.rs:34-69` (single-worker registration). Verify `BackgroundWorker::transaction` is synchronous/blocking in the pgrx version in `Cargo.toml`.
2. **Validate `SO_REUSEPORT` on the deployment target.** Confirm Linux load-balancing semantics under the kernel the `pgweb/postgres:latest` base image ships; confirm `socket2` can set it pre-bind and convert cleanly to `tokio::net::TcpListener`; determine macOS dev behavior and the fallback.
3. **Map the GUC + restart story.** Determine how a `pgweb.workers` custom GUC interacts with `_PG_init` worker registration (registration is once-per-postmaster-start ⇒ restart required), and how the existing `pgweb.settings`/`pg-web env` mechanism differs (that's a table read at request time; this is a registration-time GUC — they are not the same lever).
4. **Budget `max_connections` / `max_worker_processes`.** Inspect the image's default `postgresql.conf` (in `Dockerfile` / `docker/init-pgweb.sh`); compute headroom for K workers + Postgres internals on a 2 GiB box; propose defaults and a warning when K is set too high.
5. **Decide the livereload-across-K design.** Prototype "dev forces K=1" vs "every worker runs its own LISTEN" and pick based on dev-ergonomics + slot cost. Confirm prod (zero LISTEN) is unaffected either way.
6. **Pick and pin the bench tool.** Choose `oha`/`wrk`/`k6`/`vegeta`, pin a version, confirm it produces p99.9 and supports an open-model/constant-rate mode for the slow-handler test (or pair a second tool for that one scenario).
7. **Decide the CI wiring.** Bench-smoke tier in `scripts/test-all.sh` vs an opt-in `RUN_BENCH=1` manual tier; define the regression threshold and where results are stored.
8. **Read 014 and 016** (statement_timeout and request-path caching) before implementing so the three compose rather than collide (014 sets a timeout *inside* each worker's transaction; 016 may cache route/template/env lookups that today are separate SPI round-trips at `http.rs:127/165/203`).

## Constraints & invariants to respect

From `CLAUDE.md` (Architectural invariants — DO NOT VIOLATE):
- **#7 — Async only in the background worker.** Parallelism is added by registering **more workers**, each with its own single-thread runtime — never by introducing a multi-thread runtime or tokio into backend `#[pg_extern]` paths.
- **#4 — One HTTP request = one SPI transaction.** Preserved per worker under Option A. Option B must not split a request across transactions.
- **#5 — Zero network hop inside the extension (SPI only on the serving path).** No `libpq`/`postgres://` on the request path. The dev livereload LISTEN connection is the sole, out-of-band exception and stays that way.
- **The SPI-thread-affinity constraint** documented at `worker.rs:60-62` is the hard physics: only the `connect_worker_to_spi` thread may call SPI. Any design that would run SPI on a migrated/spawned thread is wrong by construction. This is *why* Option C is impossible and *why* A (more processes) is the answer rather than threads.
- **#1 (no raw C bindings — go through pgrx), #2 (HTTPS out-of-process; the worker binds plain HTTP), #6 (PG 15/16/17 only).** `SO_REUSEPORT` via `socket2` must work across all three PG versions and the three target kernels.
- **Phase discipline.** Multi-worker serving and the bounded queue are the Phase-3 "Async & Scale" surface (`ROADMAP.md:227-240`). Land them as Phase 3, not smuggled into Phase 1 paths. The benchmark itself (Step 1) is phase-neutral and can ship now.

## Acceptance criteria

1. A published **`docs/BENCHMARKS.md`** reporting p50/p95/p99/p99.9 **and** req/s for workloads (a)–(d) on the **1-vCPU / 2 GiB** tier (and the comparison tier), with full method, tool+version, seed sizes, and Postgres config.
2. A **reproducible bench harness** under `bench/` with a documented entry point (`make bench` or `bench/run.sh`) that boots the stack, seeds, runs all workloads, and writes results — runnable by a third party from the repo.
3. The **head-of-line-blocking experiment** (workload d concurrent with workload b) is captured as a graph/table that visibly shows unrelated requests' latency degrading on today's single-worker build.
4. A **regression guard**: either a bench-smoke tier added to `scripts/test-all.sh` or a documented opt-in tier (`RUN_BENCH=1`), with a defined threshold, so a future throughput regression is caught automatically.
5. `docs/VISION.md:58` is **reconciled with measured reality** in the same change: the 1k-req/s claim is either validated (number cited, "(Target — to be benchmarked.)" removed) or revised to the measured figure with a pointer to 015.
6. A **written, reviewed concurrency design doc** (this prompt's Step 2 worked through) recording the chosen option(s), the connection-budgeting math, the GUC/restart policy, and the livereload-across-K decision.
7. *(If implemented)* The **multi-worker (Option A) build exists** behind a `pgweb.workers` GUC, defaulting to ≈vCPU count in prod and 1 in dev, with `SO_REUSEPORT` binding verified on Linux.
8. *(If implemented)* The slow-handler experiment from criterion 3, **re-run on the multi-worker build, no longer tanks unrelated requests** — with K>1, a single slow handler occupies one worker while the others keep serving at low latency (before/after numbers in `docs/BENCHMARKS.md`).
9. *(If implemented)* A **per-worker bounded queue (Option B)** returns `503` + `Retry-After` under overload instead of unbounded pile-up, with the behavior demonstrated in the bench.
10. `docs/DEPLOYMENT.md` and `CLAUDE.md` are updated to document the per-worker backend-slot cost (prod slots = K) and the `max_connections`/`max_worker_processes` budgeting guidance.

## Open questions

1. **Default worker count.** Hard-default to `num_vcpus`? A fixed small number (e.g. 4)? Capped at a fraction of `max_worker_processes`? What does Step 1's per-core scaling say is worth it before diminishing returns on a 1-vCPU box (where K>1 may help *isolation* via head-of-line relief even when it can't add throughput)?
2. **`max_connections` budgeting on a 2 GiB VPS.** With K workers each holding one SPI backend permanently, what default K keeps comfortable headroom against a typical `max_connections`, and should the image bump `max_worker_processes` / `max_connections` defaults to fit it?
3. **How does SSE fan-out scale across N workers?** With `SO_REUSEPORT`, app-level realtime subscriptions (Phase-2 `/_pgweb/subscribe/<ch>`, `ROADMAP.md:61`) and livereload land on arbitrary workers. Does every worker need its own LISTEN loop (K connections), or is there a shared-subscription design that doesn't reintroduce a single bottleneck? (Dev livereload is the immediate case; app realtime is the looming one.)
4. **Should a separate health port be bound?** A dedicated, always-fast `/_pgweb/health` (possibly on its own port or as a non-SPI path that bypasses the bounded queue) so a saturated worker still answers liveness/readiness probes — and how does that interact with `SO_REUSEPORT` randomly routing the probe?
5. **macOS dev parity.** `SO_REUSEPORT` semantics differ on macOS (last-bind-wins, not load-balanced). Force K=1 in dev on all platforms (simplest, and aligns with the livereload K=1 lean), or platform-branch?
6. **Open-model vs closed-model benchmarking.** Which load model is canonical for the published numbers — constant-arrival-rate (reveals queueing/tail behavior honestly) or fixed-connections (simpler, what most people run)? Report both, or pick one and justify it as the headline?
7. **Restart vs reload for `pgweb.workers`.** Confirm that changing the worker count requires a full Postgres restart (registration is once-per-postmaster-start) and document it; is there any acceptable way to scale workers without a restart, or is "restart to re-scale" the honest answer?
8. **Poison-pill resilience.** With per-worker `set_restart_time(5s)`, could a request that reliably panics march through and crash all K workers in turn? Do invariant-#4 rollback + 014's `statement_timeout` fully cover this, or is an additional guard (panic catch, per-worker circuit breaker) warranted?

---

*Write the benchmark first; let the numbers set the urgency and the default worker count. Keep the honesty this repo is known for: publish what you measured, including what you did not measure, and reconcile VISION.md with reality in the same change.*
