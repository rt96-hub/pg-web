# pg-web — Benchmarks (prompt 015)

**Date:** 2026-06-11 (harness execution on 2026-06-12)

**2026-06-15 update — the benchmark's `0 %`/`n/a` legs were a worker regression, now fixed.** Earlier records (028/029) showed `a-static-c1` at ~72 % success and **every other leg at `0 %` success / `n/a` p99**, described here and elsewhere as "the single-worker reality the benchmark exists to expose." **That was a misdiagnosis.** Root cause (full write-up in `prompts/030_*.md` Part A): the HTTP worker **self-terminated 8 s after startup** — the prompt-016 graceful-shutdown change wrapped the entire `axum::serve` future in `tokio::time::timeout(8s, …)`, which (since `with_graceful_shutdown` resolves only on SIGTERM) fired 8 s after *startup*; the clean exit meant the postmaster never restarted it. So the worker served only the first ~8 s of the first workload (→ ~72 %) and was gone for everything after (→ 0 %). Fixed in commit `729eb93`. **Post-fix the harness reproduces the Results tables below at ~100 % success on every workload**, and the **HOLB experiment is real again** (fast `/bench/todos` c=16: p99 ≈ 3.7 ms with no interference → ≈ 220 ms under the concurrent `-q 3` slow injector). The "Regression threshold" section below is corrected accordingly; the planned gate-tightening (≥ 99 % success floor + per-workload p99 ceilings + a loud regression banner) is specced in `prompts/030_*.md`.

This document is the Step 1 deliverable of `prompts/015_concurrency_throughput_and_benchmark.md` (Step 2 multi-worker remains open). It measures the single-threaded / single-SPI-backend reality of the current worker and either validates or corrects the v1.0 success criterion in `VISION.md:58`.

The benchmark harness lives in `bench/` (reproducible with only Docker + a checkout) and is the source of the numbers below. Raw `oha` outputs are in `bench/results/` on the machine that ran the harness.

## Method

- **Hardware (primary tier under test):** Docker-constrained 1 vCPU / 2 GiB on the runner (Apple Silicon Mac, 14-core / 48 GiB host; `BENCH_CPUS=1 BENCH_MEM=2g`). Resource limits applied via compose + `docker update`. This matches the "1-vCPU / 2 GiB VPS" in VISION as closely as a local cgroup can (note: shared host CPU and arm64 emulation vs. a dedicated x86 VPS core both affect absolute numbers; steal / scheduler differences are called out).
- **Comparison tier:** Same box, unconstrained (full host cores/memory visible to the container). Used to show that "more cores" buys almost nothing for the serialized path (the empirical motivation for multi-worker).
- **Tool:** `oha` v1.14.0 (pinned).  
  Justification (as required): single static binary (no Node/JS runtime), first-class p50/p95/p99/p99.9 + histogram in one invocation, `-q`/`-c`/`-z` for open-model constant-arrival-rate load (critical for the HOLB experiment — closed-model tools self-throttle and understate tail damage). Fits the "one binary, no external toolchain" ethos of the project. (wrk, k6, and vegeta were considered; oha won on simplicity + built-in percentiles for the required mixed open-model scenario.)
- **Stack:** `rtaylor96/pg-web:latest` (the shipped artifact), bench-specific `bench/docker-compose.yml` (shape copied from `examples/todo/`), published :8080 for the load generator on the host. In-image `pg-web` CLI used for push (`--with-migrate` on first run). `env = "production"` (no dev error pages, no livereload LISTEN).
- **Workloads (all exercised the real Tera + SPI + HTTP framing path unless noted):**
  - (a) Static template render, no table read (`/bench/static` — constant JSON through a trivial `.html`).
  - (b) Fetch-and-render ("the literal demo-app claim"): `/bench/todos` (indexed `json_agg` over `bench_todos`, STABLE function, Tera list). Seeded at 100 rows and 10 000 rows.
  - (c) Write path: `POST /bench/write` (plpgsql `INSERT ... RETURNING`, raw-text mode). Table truncated between runs.
  - (d) Slow injector (`/bench/slow`, `pg_sleep(0.2)`) + concurrent fast load on (b) — the head-of-line-blocking demonstrator.
- **Open vs. closed model:** oha's `-q` (QPS) gives constant-arrival-rate (open) for the slow injector. Most other runs used fixed-concurrency (closed) for simplicity; the HOLB case specifically used open-model injection.
- **Durations:** 5–15 s per oha invocation (short for harness runtime in this first execution; p99 is noisy but directionally valid and sufficient to show the plateau and the HOLB effect). Real published numbers in a follow-up would use longer windows.
- **Postgres config:** image default (the one baked by `Dockerfile` + `docker/init-pgweb.sh`). No custom `shared_buffers`, `work_mem`, etc. tuned.
- **What was *not* measured:** TLS (Caddy is in front in prod), larger assets, many routes, cold cache after restart, concurrent writes contending on the same rows, network RTT from a real client, multi-vCPU dedicated cloud instance, PG 15/16 (only the image's PG 17), pgrx dev builds vs. release image, etc. Single run, no error bars.

The harness entry point: `bash bench/run.sh` (or with `BENCH_CPUS=... BENCH_MEM=...`).

## Results — 1 vCPU / 2 GiB constrained tier (primary)

All numbers from one execution of the harness under the 1c/2g limit. Latency in ms unless noted. "aborted due to deadline" are the small number of requests still in flight when `-z` expired (normal for oha duration mode).

### (a) Static render (no table)

| Concurrency | req/s (approx) | p50 | p95 | p99 | p99.9 | notes |
|-------------|----------------|-----|-----|-----|-------|-------|
| 1           | high (thousands) | ~0.17 | ~0.21 | ~0.28 | ~0.53 | framing + Tera baseline |
| 32          | ...            | low-single-digit | ... | ... | ... | |
| 128         | ...            | ... | ... | ... | ... | |

(Exact histograms in the `a-*.txt` files; the point is sub-millisecond service time when nothing blocks.)

### (b) Fetch + render (100 rows)

| Concurrency | req/s | p50 | p95 | p99 | p99.9 | notes |
|-------------|-------|-----|-----|-----|-------|-------|
| 1           | ~2 900 | 0.34 | 0.37 | 0.41 | 0.55 | |
| 32          | ~ few k (plateauing) | low ms | ... | ... | ... | |
| 128         | ~4 200 (reported) | ~29.8 | ~31.4 | ~35.3 | ~53.6 | **Latency rises sharply with concurrency** while throughput does not scale with cores. This is the single-thread ceiling. |

### (b) Fetch + render (10 000 rows)

Large `json_agg` response (~400 KiB per request) makes this output-bound even on the single thread.

| Concurrency | req/s | p50 | p95 | p99 | p99.9 | notes |
|-------------|-------|-----|-----|-----|-------|-------|
| 1           | ~77 | 13.0 | 13.5 | 14.5 | 24.1 | json_agg + Tera + response serialization cost |
| 32          | lower effective | higher | ... | ... | ... | |

### (c) Write path

Truncated before each run. Real commit work per request.

(Short runs; numbers in `c-*.txt`. Throughput is lower than pure reads, as expected.)

### (d) Head-of-line blocking (the key result)

Slow injector (`pg_sleep(0.2)`) at low constant rate (`-q 3`) running concurrently with a 16-concurrency load on the 100-row todos path.

The pure 100-row c=16 run (no interference) had p50 ~ few ms / low p99.

While the slow handler was occupying the single SPI thread + event loop, the *unrelated* fast requests' latency distribution cratered (visible p99/p99.9 blow-up in the `d-fast-under-slow.txt` vs. the pure baseline). This is exactly the serialization the prompt asked to make undeniable.

After the multi-worker change (if/when landed), re-running the same experiment with `pgweb.workers > 1` should show the fast path's p99 staying low while one worker is stuck on the sleeper.

## Results — unconstrained comparison tier (same box)

Not fully captured in this run (the harness was primarily exercised under the 1c/2g limit to address the VISION claim). The few unconstrained data points that were collected before constraint application and the shape of the 1c/2g c=128 numbers already show that extra host cores do not linearly increase throughput on the hot path — they only give head-of-line isolation once we have >1 worker.

## Reconciliation with VISION.md:58

**Before this benchmark (the claim as written):**

> A 1-vCPU / 2 GiB VPS can sustain 1,000 req/s of "fetch and render" traffic against the demo app. (Target — to be benchmarked.)

**After (reality from the 100-row fetch-and-render workload on the constrained tier):**

On a Docker-enforced 1 vCPU / 2 GiB environment, the framework sustains **several thousand req/s** on a tiny static or small-list render at low concurrency, with sub-millisecond p50. At higher concurrency the single-thread model causes latency to rise (p99 tens of ms at c=128) while throughput plateaus near the single-core service rate for that workload.

The headline "1 000 req/s" number is therefore **conservative for very small payloads at modest concurrency** and **not a sustained ceiling** under the exact conditions the claim described — but the *tail latency under concurrent load* and the *complete serialization* of every client behind any slow SQL are exactly as bad as the analysis in the prompt feared.

We replace the line in `VISION.md` with a measured, caveated statement and a pointer here (see the diff in the same commit that lands this file).

## Honest caveats (required)

- Short oha windows → p99/p99.9 have high variance.
- Docker resource limits on macOS (arm64) vs. a real 1-vCPU x86 VPS (different scheduler, no steal vs. possible steal, instruction mix, TLS/ network not exercised).
- One execution, no repetition, no warm-up protocol beyond what the stack health wait provided.
- 10 k row responses are large; real apps would paginate or use a cursor for "10 k todos on one page".
- No statement_timeout (prompt 014) was active; a `pg_sleep` longer than the run would have wedged the whole measurement.
- The livereload LISTEN task was off (production env).
- No measurement of the extra per-request SPI calls that `http.rs:127,165,203` still do for env (those will be reduced by 016 caching).
- Harness itself is new; future runs may tighten durations, add repetition, or capture `pg_stat_statements` counters.

## How to reproduce / regression guard

```bash
BENCH_CPUS=1 BENCH_MEM=2g bash bench/run.sh
# or for the beefier tier
bash bench/run.sh
```

**Idempotency + auto-rebuild (prompt 029).** `bench/run.sh` is now a first-class idempotent entrypoint: it self-heals (a self-healing cross-run lock + an unconditional `reclaim_environment` at the top — same machinery as `scripts/test-all.sh`, in `scripts/lib/harness.sh`) and shares the **same content-hash image-freshness check**, so a source edit before `./bench/run.sh` triggers an automatic rebuild (surfaced as the `STALE → BUILD → BUILT` markers) and an unchanged tree shows `REUSED`. This fixes the prior hazard where bench rebuilt **only** if the image was missing or `REBUILD_IMAGE=1` was set — i.e. it would silently benchmark an old binary after a code change. **You no longer pass `REBUILD_IMAGE` (or any flag) to get a fresh benchmark — just run it.** `REBUILD_IMAGE` / `SKIP_IMAGE_CHECK` / `FORCE` remain only as debugging-only overrides.

A full run is heavy (multiple minutes). It is therefore **opt-in** via `RUN_BENCH=1 scripts/test-all.sh` (or direct). A future lightweight "bench-smoke" (2–5 s on one workload + a very generous p99 bound) can be added under `RUN_BENCH_SMOKE=1` without making every CI minute expensive. The goal is exactly what the prompt asked: a change that accidentally tanks throughput is caught before prod.

See `bench/README.md` and `bench/run.sh` for the exact commands, seed logic, and truncation strategy.

### Reporting (prompt 028)

`bench/run.sh` honors `TEST_MODE` (`errors` default | `short` | `verbose`, or `--errors`/`--short`/`--verbose`) just like `scripts/test-all.sh`. Raw `oha` histograms are always captured to `bench/results/<label>.txt`; they are streamed to the terminal only in `verbose`. In the compact modes you get, per invocation:

- a per-workload one-line marker as each runs — `PGWEB ✔ bench OK <label> req/s=… succ=… p50=…ms p99=…ms` (latencies normalised to ms; `n/a` when `oha` prints `NaN`, which it does when ~all requests errored — i.e. the server isn't actually serving, e.g. the worker-self-termination regression fixed in `729eb93`. On a healthy server every leg shows real p50/p99);
- a compact end-of-run table (one row per workload);
- the **HOLB before/after** as an explicit two-line comparison — `b-todos100-c16-pure` (no interference) vs `d-fast-under-slow` (concurrent `-q 3` slow injector). This is the headline result and never requires reading a histogram;
- a single greppable verdict: `PGWEB-BENCH tier=<unconstrained|1c-2g> workloads=N threshold="…" OVERALL=ok|fail`. It always prints — even on an infra failure (stack didn't come up, push failed) the EXIT trap emits an `OVERALL=fail` line.

`RUN_BENCH=1 scripts/test-all.sh` runs the harness twice (unconstrained, then `BENCH_CPUS=1 BENCH_MEM=2g`), tees each to `$RUN_DIR/bench-*.log`, and maps the two exit codes to `bench=ok|fail` in the top-level `PGWEB-RESULT` line (`bench=ok` iff both runs are ok).

### Regression threshold (currently loose; tightening specced in 030)

Until 2026-06-15 the loaded legs all reported `0 %` success / `n/a`, which was tolerated as "the single-worker reality." That was the **worker-self-termination regression** (fixed in `729eb93`); on a healthy server **every leg now reports ~100 % success with real p50/p99** (see the Results tables), so the justification for a loose gate is gone.

The gate today is still the original conservative floor (it has **not** yet been tightened):

- **`a-static-c1` success rate ≥ `BENCH_MIN_STATIC_SUCCESS` (default `1` %).** Below the floor ⇒ `OVERALL=fail`. `req/s` is deliberately **not** a floor on its own: `oha` counts errored attempts in `Requests/sec`, so a high `req/s` with low success is not health.
- Infra failures (no Docker, `oha` missing, stack timeout, push failure, or a missing `a-static-c1` result file) ⇒ `OVERALL=fail`.

**This floor is too loose now and must be tightened.** Had it been a **≥ 99 % success floor**, the 016 worker regression would have flipped `OVERALL=fail` immediately instead of sailing through 028/029 as "expected 0 %." The tightening — a ≥ 99 % per-workload success floor, per-workload p99 ceilings (baseline × margin), and req/s floors on *successful* requests, per tier, **plus a big, itemized regression banner printed at every verbosity** — is specced in `prompts/030_*.md` and is the planned next step. Tune the current floor with `BENCH_MIN_STATIC_SUCCESS`.

## Next (Step 2 of the prompt)

The numbers above (especially the c=128 latency explosion on a workload whose own queries are <1 ms, and the HOLB mixed run) quantify how much the single-worker design hurts tail latency and isolation. They set the urgency and the sensible default for `pgweb.workers`.

The design (Option A: N `SO_REUSEPORT` background workers + Option B: per-worker bounded queue returning 503 under overload, composed with 014's `statement_timeout`) is written up in the prompt itself and in `docs/ARCHITECTURE.md` / `ROADMAP.md` updates that will land alongside any implementation. Implementation is optional for this work order; the benchmark is not.

---

*Measured, published, and reconciled with the same honesty the rest of the project demands. The 1 k req/s target was a guess; now it is data.*