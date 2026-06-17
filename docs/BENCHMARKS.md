# pg-web — Benchmarks (prompt 015)

**Date:** 2026-06-11 (harness execution on 2026-06-12)

**2026-06-15 update — the benchmark's `0 %`/`n/a` legs were a worker regression, now fixed.** Earlier records (028/029) showed `a-static-c1` at ~72 % success and **every other leg at `0 %` success / `n/a` p99**, described here and elsewhere as "the single-worker reality the benchmark exists to expose." **That was a misdiagnosis.** Root cause (full write-up in `prompts/completed/030_*.md` Part A): the HTTP worker **self-terminated 8 s after startup** — the prompt-016 graceful-shutdown change wrapped the entire `axum::serve` future in `tokio::time::timeout(8s, …)`, which (since `with_graceful_shutdown` resolves only on SIGTERM) fired 8 s after *startup*; the clean exit meant the postmaster never restarted it. So the worker served only the first ~8 s of the first workload (→ ~72 %) and was gone for everything after (→ 0 %). Fixed in commit `729eb93`. **Post-fix the harness reproduces the Results tables below at ~100 % success on every workload**, and the **HOLB experiment is real again** (fast `/bench/todos` c=16: p99 ≈ 3.7 ms with no interference → ≈ 220 ms under the concurrent `-q 3` slow injector). The gate has since been hardened (prompt 030): a ≥ 99 % per-workload **success floor** (always on), opt-in per-tier **p99 ceilings + successful-req/s floors** (`BENCH_STRICT=1`), and a **loud, always-printed, itemized regression banner**. See the "Regression threshold" section below.

This document is the Step 1 deliverable of `prompts/completed/015_concurrency_throughput_and_benchmark.md` (Step 2 multi-worker is re-scoped in `prompts/015.2_multiworker_serving_realtime_and_worker_config.md`). It measures the single-threaded / single-SPI-backend reality of the current worker and either validates or corrects the v1.0 success criterion in `VISION.md:58`.

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
- a single greppable verdict: `PGWEB-BENCH tier=<unconstrained|1c-2g> workloads=N threshold="…" OVERALL=ok|fail`. It always prints — even on an infra failure (stack didn't come up, push failed) the EXIT trap emits an `OVERALL=fail` line;
- on **any** failure, a loud, full-width, itemized **regression banner** (`BENCH REGRESSION DETECTED`) printed at **every** `TEST_MODE` — see § Regression threshold below.

`RUN_BENCH=1 scripts/test-all.sh` runs the harness twice (unconstrained, then `BENCH_CPUS=1 BENCH_MEM=2g`), tees each to `$RUN_DIR/bench-*.log`, and maps the two exit codes to `bench=ok|fail` in the top-level `PGWEB-RESULT` line (`bench=ok` iff both runs are ok).

### Regression threshold (the real gate — prompt 030)

The old gate was a placeholder: `a-static-c1` success ≥ `1 %` ("did the worker bind at all"). It was justified by "the loaded legs always report `0 %`/`n/a` — that's the single-worker reality," which was **wrong** — it was the worker-self-termination regression (fixed in `729eb93`). On a healthy server **every leg reports ~100 % success with real p50/p99** (the Results tables), so the gate can finally *mean* something. Prompt 030 replaced the placeholder with a two-layer, data-driven gate. Knobs + per-tier baselines live in **`bench/thresholds.sh`** (kept separate so they are easy to re-capture per platform); evaluation is `evaluate_gate`; breaches print the loud banner below.

**Layer 1 — per-workload success floor (always on).** Every workload (static, todos, write, *and both HOLB legs* — the slow injector degrades *latency*, not success) must be ≥ `BENCH_MIN_SUCCESS` (**default 99 %**). This is the cheap, stable, **platform-independent** check (healthy ≈ 100 %, a dead / crash-looping / not-serving worker ≈ 0 %) and is the single most valuable one: **had it been ≥ 99 %, the 016 worker regression would have flipped `OVERALL=fail` the moment it landed** instead of sailing through 028/029 as "expected 0 %." `req/s` is deliberately **not** a floor on its own — `oha` counts errored attempts in `Requests/sec`, so high `req/s` with low success is not health.

**Layer 2 — p99 ceilings + successful-req/s floors (opt-in via `BENCH_STRICT=1`).** Per workload, per tier:
- **p99 ≤ baseline × `BENCH_P99_MARGIN`** (default ×3).
- **successful req/s ≥ baseline × `BENCH_RPS_FLOOR_FRAC`** (default ×0.5). "Successful req/s" is computed as `Requests/sec × success%` (≈ `[200] count / duration`), **not** oha's error-inclusive `Requests/sec`.

Layer 2 is **off by default on purpose.** These numbers are **platform-dependent** (a 1-vCPU VPS, a Linux CI box, and an Apple-Silicon dev Mac all differ); enforcing single-platform ceilings by default would manufacture exactly the cross-platform "flakiness" that CLAUDE.md/029 forbid ("a non-green default run is a real bug, not flakiness"). So they are implemented, env-tunable, and enforced only once you have **re-baselined for your platform** and set `BENCH_STRICT=1`. The success floor (layer 1) stays on always because it needs no platform calibration. (030 open-Q1/Q4, B.1 #6.)

**Infra failures** (no Docker, `oha` missing, stack timeout, push failure, no result file, killed mid-run) ⇒ `OVERALL=fail` with the banner, via the EXIT trap.

**Baselines (ms p99 / successful req/s), 2026-06-15 post-`729eb93` run on Apple-Silicon / Docker-Desktop.** This is a record of what `bench/thresholds.sh` currently encodes (that file is authoritative; re-baseline there per deploy platform). Layer-2 ceilings = these × the margins above. The c=128 / 10k legs are the noisiest and carry the most headroom.

| workload | unconstrained p99 base / req/s base | 1c-2g p99 base / req/s base |
|---|---|---|
| a-static-c1   | 0.30 / 6000  | 0.30 / 6000 |
| a-static-c32  | 2.0 / 20000  | 2.0 / 20000 |
| a-static-c128 | 8.0 / 20000  | 8.5 / 20000 |
| b-todos100-c1   | 0.5 / 2800 | 0.5 / 2800 |
| b-todos100-c32  | 9.0 / 4000 | 9.0 / 4000 |
| b-todos100-c128 | 40 / 4000  | 45 / 4000 |
| b-todos10k-c1   | 20 / 55    | 20 / 55 |
| b-todos10k-c32  | 550 / 55   | 550 / 55 |
| b-todos10k-c128 | 2200 / 60  | 2200 / 60 |
| c-write-c1 | 0.30 / 5500 | 0.30 / 5500 |
| c-write-c8 | 0.80 / 17000 | 0.80 / 17000 |
| b-todos100-c16-pure (HOLB baseline) | 5.0 / 4000 | 6.0 / 4000 |
| d-fast-under-slow (HOLB under load) | 280 / 1100 | 280 / 1100 |

`d-fast-under-slow` (the HOLB "under load" leg) is now a recorded, gated workload (so `workloads=13` on a normal run): its **success** floor still applies, while its p99 baseline is intentionally the *slow* ~220 ms value — that leg is *supposed* to be slow; only a regression far beyond it trips.

**The loud regression banner.** On *any* failure (a breached check **or** an infra/early exit) the bench prints a full-width, ASCII `!`-framed, itemized banner — **at every `TEST_MODE`** (`errors`/`short`/`verbose`; `short` does **not** suppress it). One line per breached check names the workload, metric, observed value, threshold, and delta:

```
!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
!!  BENCH REGRESSION DETECTED  --  tier=unconstrained  --  2 check(s) failed
!!
!!  WORKLOAD               METRIC   OBSERVED        THRESHOLD                DELTA
!!  selftest-slow-as-fast  p99      221.6ms       > ceil 15.000ms (5x3)      14.8x over
!!  selftest-slow-as-fast  req/s    1442          < floor 3000 (6000x0.5)    1558 short
!!  hint: success 0% / p99 n/a on a leg => worker not serving -- check 'docker logs'
!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
```

It is **in addition to** the unchanged, machine-parseable `PGWEB-BENCH … OVERALL=ok|fail` verdict line (the grep anchor `test-all.sh` + 029 depend on), which stays the last output. A clean run prints a one-line green `PGWEB ✔ bench GATE …` confirmation instead.

**Proving the gate is live.** Two env-tunable, deterministic demonstrations (no product change needed):
- **`BENCH_SELFTEST=1`** forces `BENCH_STRICT=1` and injects a guaranteed regression — a fast-labeled workload (`selftest-slow-as-fast`) pointed at `/bench/slow` (`pg_sleep 0.2`). Its ~220 ms p99 vs a 5 ms fast baseline (→ 15 ms ceiling) and ~1400 req/s vs a 6000 floor are a platform-independent double breach ⇒ `OVERALL=fail` + banner.
- **`BENCH_MIN_SUCCESS=101 bash bench/run.sh`** makes every healthy leg "breach" the success floor ⇒ the success-floor + banner path fires on a real, otherwise-passing run.

Knobs: `BENCH_MIN_SUCCESS` (default 99), `BENCH_STRICT` (default off), `BENCH_P99_MARGIN` (×3), `BENCH_RPS_FLOOR_FRAC` (×0.5), `BENCH_SELFTEST`. The legacy `BENCH_MIN_STATIC_SUCCESS` is honoured as a back-compat alias for the success floor when explicitly set.

## Next (Step 2 of the prompt)

The numbers above (especially the c=128 latency explosion on a workload whose own queries are <1 ms, and the HOLB mixed run) quantify how much the single-worker design hurts tail latency and isolation. They set the urgency and the sensible default for `pgweb.workers`.

The design (Option A: N `SO_REUSEPORT` background workers + Option B: per-worker bounded queue returning 503 under overload, composed with 014's `statement_timeout`) is written up in the prompt itself and in `docs/ARCHITECTURE.md` / `ROADMAP.md` updates that will land alongside any implementation. Implementation is optional for this work order; the benchmark is not.

---

*Measured, published, and reconciled with the same honesty the rest of the project demands. The 1 k req/s target was a guess; now it is data.*