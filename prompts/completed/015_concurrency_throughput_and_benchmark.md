# 015 — Concurrency & throughput: the benchmark (Step 1, complete)

**Status:** **Step 1 (benchmark) — COMPLETE.** The `bench/` harness + `docs/BENCHMARKS.md` measure the single-threaded serving path; the regression gate was hardened in 030. **Step 2 (multi-worker) was attempted and REVERTED as unsatisfactory, and has been re-scoped into `prompts/015.2_multiworker_serving_realtime_and_worker_config.md`** — do the concurrency design + implementation *there*, not here. This file is retained as the benchmark work-order record.
**Date opened:** 2026-06-11 · **Step 1 shipped:** 2026-06-12 (harness) / 2026-06-15 (030 gate) · **Step 2 re-scoped to 015.2:** 2026-06-16
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Siblings (all since landed):** 014 (`statement_timeout`) ✅, 016 (request-path caching + graceful shutdown + always-on per-worker LISTEN) ✅. **Note:** the "Current behavior" evidence below was written *before* 014/016 shipped (e.g. it says there is no `statement_timeout` and that env is re-read via SPI every request); both are now fixed. See `prompts/015.2_*.md` for the current, corrected state.
**Context:** `docs/VISION.md:58` asserted a 1-vCPU / 2 GiB VPS can sustain 1,000 req/s of fetch-and-render traffic — flagged "(Target — to be benchmarked.)". Step 1 executed and published that benchmark. The serving path still runs on a single-OS-thread BGW; quantifying its ceiling and head-of-line blocking is what Step 1 delivered and what motivates 015.2.

---

## Summary

pg-web serves every HTTP request on **one OS thread inside one background worker**, over a **single-threaded** Tokio runtime. This is forced by Postgres: only the thread attached via `connect_worker_to_spi` may issue SPI calls, so a multi-thread runtime would migrate a task onto a thread with no SPI backend and panic on the first `Spi::*` call. The per-request SQL is a **synchronous, blocking** call (`BackgroundWorker::transaction`) on the event-loop thread, so while **any** handler's SQL runs, **nothing else on the runtime makes progress** — other pages, static assets, SSE keepalives, 404s all wait. The whole site is serialized behind the slowest in-flight request.

At low concurrency with sub-millisecond handlers this is fine; the real problems are **tail latency under mixed traffic** and **zero isolation** between one slow request and every other client. Step 1's job was to make that undeniable with numbers and reconcile the unverified 1k-req/s claim.

This work order was sequenced deliberately: **Step 1 is the benchmark — done first, shippable on its own.** Step 2 (the multi-worker design + implementation) is now `prompts/015.2_*.md`, justified by Step 1's numbers.

## Why this mattered

- **A headline success criterion was unverified.** `docs/VISION.md` listed 1k req/s as a v1.0 target while it was a guess. Step 1 measured it.
- **The serialization was invisible to the test suite.** Every tier exercised one request at a time; nothing put concurrent load on the worker, so head-of-line blocking had never been observed or regression-guarded. Step 1 (+ the 030 gate) fixed that.
- **It composes with siblings.** 014 (`statement_timeout`, ✅) bounds one wedged query; 016 (request-path caching, ✅) cuts per-request SPI; 015 is the structural ceiling. The three together are the "survives real traffic" story — and with 014/016 landed, multi-worker (015.2) is the last leg.

## Current behavior — the serialization (evidence as of 2026-06-11; see header note re 014/016)

Read these before proposing anything; the constraint is physics. (Line numbers have since drifted — 015.2 carries re-verified refs.)

1. **Single-threaded runtime, by necessity.** `worker.rs` builds `tokio::runtime::Builder::new_current_thread()`; SPI is attached once to that one OS thread (`connect_worker_to_spi`). That is the hard constraint the whole design orbits.
2. **Per-request work is synchronous, on the event-loop thread.** `http::handle` → `router::serve` → `BackgroundWorker::transaction(serve_in_tx)`, not `await`ed, not `spawn_blocking`ed. While it runs, the single-threaded reactor polls nothing else.
3. **Therefore everything serializes** behind the slowest in-flight request: other pages, static assets, the livereload SSE keepalive, even a hardcoded 404.
4. **(Pre-014, now fixed)** There was no `statement_timeout` on the serving path. 014 has since landed (`SET LOCAL statement_timeout` per request) — it bounds one request's blast radius.
5. **Exactly one worker is registered** (`pg_web_worker`), binding `0.0.0.0:8080` once. That is the whole serving capacity — the thing 015.2 multiplies.
6. **Per-worker always-on LISTEN.** As of 016 the listen loop runs unconditionally for the BGW (one `tokio-postgres` LISTEN connection on `pgweb_livereload` + `pgweb_reload`), fanning NOTIFYs to local subscribers in-memory (+1 backend slot per worker). This is the foundation 015.2 relies on for cross-worker realtime fan-out.

## Step 1: the benchmark (DELIVERED)

Deliverable: a reproducible, publishable benchmark (`bench/`) + `docs/BENCHMARKS.md` that measured the single-worker reality and reconciled `VISION.md:58`. Shipped 2026-06-12; the regression gate was hardened in prompt 030 (2026-06-15).

- **Hardware tiers:** primary 1 vCPU / 2 GiB (Docker `--cpus=1 --memory=2g`), plus an unconstrained comparison tier (shows throughput *fails to scale* with cores — the motivation for 015.2).
- **Workloads:** (a) static render, (b) todo-list fetch+render at 100 and 10k rows, (c) write path, (d) the **head-of-line-blocking demonstrator** — a `pg_sleep(0.2)` injector run concurrently with the fast path. Live in `bench/app/pages/bench/*`; never contort the todo example.
- **Tool:** `oha` (pinned `OHA_VERSION`) — single static binary, first-class p50/p95/p99/p99.9 + histogram, open-model `-q` for the HOLB test. Justification in `docs/BENCHMARKS.md`.
- **Harness + CI:** `bench/run.sh` (boots the stack, pushes, seeds, runs the matrix, writes `bench/results/`), opt-in via `RUN_BENCH=1 scripts/test-all.sh`. Prompt 030 turned the threshold into a real gate (≥99% per-workload success floor always on; per-tier p99/req-s ceilings under `BENCH_STRICT`; a loud regression banner). Knobs/baselines in `bench/thresholds.sh`.
- **Output:** `docs/BENCHMARKS.md` (method + tables + caveats + the HOLB before/after), and `VISION.md:58` reconciled to measured reality.

**Headline result (post-`729eb93` fix; see `docs/BENCHMARKS.md`):** the fast `/bench/todos` c=16 path runs at p99 ≈ 3.7–4.2 ms alone but **p99 ≈ 218–222 ms** under the concurrent `-q 3` slow injector — a ~60× tail blow-up on requests whose own queries are microseconds, *even on the unconstrained tier*. This is the empirical case for 015.2. (A worker-self-termination bug had masked this until 030 Part A — the bench used to read "72%-then-0%"; fixed in `729eb93`.)

## Step 2 — multi-worker: RE-SCOPED to 015.2

The original Step-2 implementation (multi-worker via `SO_REUSEPORT`, bounded queue, per-worker LISTEN fan-out, `pgweb.workers` config) was attempted and **reverted as unsatisfactory**. It has been rewritten — with the honest 030 HOLB data, accurate codebase references, the per-worker-LISTEN realtime requirement, and a `pgweb.toml` worker-count config (+ autoscale as a follow-up) — in:

> **`prompts/015.2_multiworker_serving_realtime_and_worker_config.md`**

Do the concurrency design + implementation there. The design reasoning (Options A/B/C, SPI-thread-affinity constraint, connection budgeting, cross-worker fan-out) lives in 015.2.

## Acceptance criteria (Step 1 — MET)

1. ✅ Published `docs/BENCHMARKS.md` with p50/p95/p99/p99.9 **and** req/s for workloads (a)–(d) on the 1-vCPU/2-GiB tier (+ comparison tier), full method/tool/version/seed/config.
2. ✅ Reproducible harness under `bench/` (`bench/run.sh`) runnable by a third party from the repo.
3. ✅ The head-of-line-blocking experiment captured as a before/after (`d-fast-under-slow` vs `b-todos100-c16-pure`) — and, via 030, now a live regression-guarded gate.
4. ✅ A regression guard — `RUN_BENCH=1 scripts/test-all.sh` with a defined threshold (hardened to a real gate in 030).
5. ✅ `docs/VISION.md:58` reconciled with measured reality in the same change.

(The original Step-2 acceptance criteria, research tasks, and open questions now live in `prompts/015.2_*.md`.)

---

*Benchmark delivered and honest; the numbers set the urgency for 015.2. The single-worker tail is real (~60× p99 blow-up under one slow handler) — see `docs/BENCHMARKS.md`. The fix (more processes via `SO_REUSEPORT`, each keeping its own LISTEN so DB changes reach every user) is specced in `prompts/015.2_*.md`.*
