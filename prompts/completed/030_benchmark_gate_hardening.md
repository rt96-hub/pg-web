# 030 — Benchmark gate hardening: a ≥99% success floor + p99/req-s ceilings + a loud, always-on regression banner

**Status:** Open handoff prompt — medium priority. **Part A is DONE** (root cause found and fixed); Part B (the gate + the loud banner) is the remaining work.
**Date opened:** 2026-06-15. **Reworked:** 2026-06-15, after Part A overturned the original premise (see below).
**Follows:** 029 (idempotent harness + shared content-hash image freshness — keep it idempotent), 028 (the `PGWEB-BENCH` reporting contract this extends), 015 (the original bench harness + the single-worker concurrency design it characterizes).
**Prerequisites:** the worker-self-termination fix (commit `729eb93`) must be in the tree — without it the bench reads 0% and none of this is measurable.

> **Note on the rename.** This prompt was opened as "kill the connection-churn artifact." **Part A proved there was no connection-churn / Docker-Desktop-proxy artifact** — the bench's `0%` was a product bug (the HTTP worker exited 8s after startup), now fixed (`729eb93`). It has therefore been renamed from `030_benchmark_methodology_connection_churn_real_throughput.md` to `030_benchmark_gate_hardening.md`; the actual work below is **benchmark gate hardening**. (Docs reference it via the rename-safe `prompts/030_*.md` glob.)

---

## Premise correction (read this first)

The original 030 hypothesized that `bench/run.sh`'s "first workload ~72% success, every later workload 0.00%" pattern was a **load-generator / Docker-Desktop measurement artifact** (ephemeral-port / userspace-proxy connection-table exhaustion under per-request reconnection). **Part A disproved that.** The real cause was a **product regression**: the HTTP worker self-terminated 8 seconds after startup, so the bench only ever caught the first ~8s of the first workload.

What this means for the work:
- **There is no proxy artifact to engineer around.** With the worker alive, the host→published-port path serves **100% at up to c128** (`oha` keeps connections alive by default). Moving the load generator in-network is therefore **optional** (portability/realism), **not** a correctness fix — demoted accordingly in Part B.
- **The high-value work is the regression gate.** The bug sailed through 016/028/029 because the bench gate only checks "did the worker bind at all" (`a-static-c1 ≥ 1%`). A **≥99% success floor** would have flipped `OVERALL=fail` the moment the worker started dying. That, plus a **loud, itemized, always-printed regression banner**, is what this prompt now delivers.

---

## Part A — root cause (DONE; written up here)

Investigation procedure and evidence (reproduced on a fresh stack with **zero external load**, probing in-container `127.0.0.1:8080` so the host proxy is not even in the path):

- **Keep-alive present?** Yes, **server-side** (`worker.rs` serves via `axum::serve`, hyper's default HTTP/1.1 persistent connections; nothing sends `Connection: close`). `oha` also keep-alives by default. So per-request reconnection was never happening — the original churn theory was wrong on two counts.
- **Which resource is exhausted?** **None.** The worker *process exits.* In-container loopback gave `connection refused`; `oha` in-network (`--network bench_default → http://postgres:8080`) was *also* 0% once the worker was dead — exactly the prompt's own experiment-2 fallback ("if in-network also craters, the diagnosis is the server, not the proxy").
- **The smoking gun:** liveness poll showed health `200` from t+1s through t+8.5s, then `connection refused` from t+9s onward, forever. Logs: `listening addr=0.0.0.0:8080` → exactly 8.00s later → `graceful drain timed out after 8s; exiting anyway`, with **no restart** (clean exit ⇒ the postmaster does not restart it, despite `bgw_restart_time=5s`).
- **The defect:** `worker.rs` wrapped the whole `axum::serve(...).with_graceful_shutdown(shutdown)` future in `tokio::time::timeout(8s, …)`. Since `with_graceful_shutdown` resolves only after SIGTERM, that timer fired 8s after *startup*. Introduced by commit `7a4e7de` (prompt 016); pre-016 the serve was an unbounded `axum::serve(...).await`, and `docs/BENCHMARKS.md`'s Results tables (real p50/p99) are from that era.

**Fix (commit `729eb93`):** arm the 8s drain budget only *after* SIGTERM (a `oneshot` gates a `tokio::select!` arm), so the server runs for the postmaster's full lifetime and still drains ≤8s on shutdown. Regression-guarded by tier-3 test `worker_serves_past_drain_cap` (idles past the cap, asserts the worker still serves — the gap that let the bug through: every other tier-3 test finishes its HTTP work inside the 8s window).

**Post-fix baseline** (full `RUN_BENCH=1 scripts/test-all.sh`, Apple-Silicon Docker Desktop; single run — set thresholds with margin and re-baseline on the deploy platform). Every workload **100% success**:

| workload | unconstrained req/s · p50 · p99 | 1c/2g req/s · p50 · p99 |
|---|---|---|
| a-static-c1   | 6404 · 0.151 · 0.242 | ~6300 · ~0.15 · ~0.24 |
| a-static-c32  | 22254 · 1.433 · 1.753 | 13525 · 2.297 · 3.219 |
| a-static-c128 | 22130 · 5.799 · 6.685 | 13177 · 9.447 · 16.978 |
| b-todos100-c1   | 3022 · 0.327 · 0.403 | 3056 · 0.324 · 0.393 |
| b-todos100-c32  | 4298 · 7.399 · 8.562 | 4322 · 7.243 · 11.274 |
| b-todos100-c128 | 4218 · 30.0 · 35.2 | 4206 · 29.7 · 40.4 |
| b-todos10k-c1   | 64 · 15.5 · 17.3 | 63 · 15.7 · 18.4 |
| b-todos10k-c128 | 78 · 1882 · 2105 | 85 · 1753 · 1789 |
| c-write-c1 | 6251 · 0.156 · 0.234 | 6344 · 0.154 · 0.231 |
| c-write-c8 | 19454 · 0.408 · 0.530 | 17946 · 0.421 · 0.729 |
| **HOLB** b-todos100-c16-pure (baseline) | 4718 · 3.385 · **3.749** | 4743 · 3.314 · **3.890** |
| **HOLB** d-fast-under-slow (under `-q 3`) | 1437 · 3.553 · **221.6** | 1471 · 3.481 · **219.8** |

The **HOLB experiment is real again**: fast-path p99 blows up **~3.7ms → ~220ms** under the concurrent slow injector — the single-worker head-of-line-blocking signal 015 built the harness for, previously masked entirely by the worker-death bug. (These tables match the pre-016 numbers in `docs/BENCHMARKS.md`.)

---

## Part B — the work (in priority order)

### B.1 — Tighten the regression gate (primary)

Replace the "did it serve at all" placeholder (`BENCH_MIN_STATIC_SUCCESS` + `evaluate_threshold` in `bench/run.sh:~93/~388`) with **data-driven, env-tunable, per-tier** gates. On a healthy server every leg is ~100%, so the gate can finally be meaningful:

1. **Per-workload success floor — default ≥ 99%.** Applies to *every* workload (static, todos, write, and both HOLB legs — the slow injector degrades *latency*, not success). This single check is what would have caught the 016 worker regression. Env: e.g. `BENCH_MIN_SUCCESS` (global) with optional per-workload overrides.
2. **Per-workload p99 ceilings** = baseline × margin, **separate per tier** (the 1c/2g cgroup yields materially different numbers than unconstrained — see the two columns above). Store the baseline table in the script (or a sourced `bench/thresholds.sh`); env-overridable. Start with a generous margin (e.g. ×2–3) and tighten once reproducible across several runs. The c=128 / 10k-row legs are the noisiest — give them the most headroom.
3. **req/s floors on _successful_ requests** — not `oha`'s error-inclusive `Requests/sec`. Compute successful throughput from the `[200]` count / duration (or `req/s × success%`). Floor at a fraction of baseline.
4. **Keep the infra-failure cases** (no Docker, `oha` missing, stack timeout, push failure, missing result file) ⇒ `fail`.
5. **A synthetic regression must flip `OVERALL=fail`** — e.g. an injected `pg_sleep` on the fast path, or pointing a "fast" workload at `/bench/slow`. Add this as a guard you can run on demand (a `BENCH_SELFTEST=1` mode, or document the manual injection) so the gate is provably live.
6. Consider a `BENCH_STRICT=1` that turns on the tight p99/req-s ceilings while a sane default always enforces the **success floor** (the success floor is cheap, stable, and the single most valuable check — it should be on by default).

### B.2 — Loud, itemized, always-on regression banner (primary)

When the gate fails for **any** reason, the bench must print a **big, impossible-to-miss, itemized warning** — **regardless of `TEST_MODE` (`errors` / `short` / `verbose`)**. `short` mode must NOT suppress it; a regression is always loud. Requirements:

- A **visually framed banner** (e.g. a full-width row of `!` / `=` plus a `BENCH REGRESSION DETECTED` headline) that stands out when scrolling a long log.
- An **itemized list — one line per breached check** — naming the tier, the workload, the metric, the **observed value**, the **threshold it breached**, and the **delta**. Example shape:
  ```
  !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
  !!  BENCH REGRESSION  —  tier=1c-2g  —  3 checks failed
  !!  a-static-c1      success   41.2%   < floor 99%        (FAIL)
  !!  b-todos100-c32   p99       182ms   > ceiling 25ms     (FAIL)
  !!  b-todos100-c1    req/s      120    < floor 1500       (FAIL)
  !!  hint: 0% / n/a on a leg ⇒ server not serving — check `docker logs`
  !!  raw: bench/results/<label>.txt
  !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
  ```
- **Always prints**, including on an early/infra exit — wire it through the existing `bench_on_exit` EXIT trap so a stack-didn't-come-up / push-failed run also emits the banner, not just a terse line.
- It is **in addition to** the machine-parseable `PGWEB-BENCH … OVERALL=fail` line (keep that verbatim as the grep anchor — `test-all.sh` and 029's assertions depend on it).
- When `RUN_BENCH=1 scripts/test-all.sh` runs the two tiers, a failure in *either* must surface its banner (don't let the second tier's output bury the first — tee already captures both; ensure the banner survives in compact mode).
- (Nice-to-have) a one-line green confirmation when all checks pass, so a clean run is also unambiguous — but the banner is specifically the failure path.

### B.3 — In-network load generator (optional; portability, not a fix)

Part A showed the host→published-port path is **not** a bottleneck (100% to c128). So this is now a *portability/realism* improvement, not a correction — do it only if cheap and clearly worth it:

- Run the pinned **static** Linux `oha` (already downloaded by `ensure_oha`; the linux build is statically linked) as a one-shot container on the bench compose network (`--network bench_default → http://postgres:8080`), bind-mounted into a pinned minimal base (e.g. `alpine:<pin>` — static binary, so musl/glibc is irrelevant). Mirrors the prod Caddy→pg-web-over-a-Docker-network topology and produces identical numbers on macOS dev and Linux CI.
- Gate behind `BENCH_LOADGEN=net|host` (keep `host` for comparison). Publishing `:8080` on the host stays (humans + `pg-web push`).
- **If you add a load-gen container you MUST keep 029's idempotency:** tear it down on exit (extend `stop_stack` / `bench_on_exit`) AND have `reclaim_environment` in `scripts/lib/harness.sh` remove it via a surgical name prefix (e.g. `pgweb-loadgen*`), **never** a blanket prune. Re-verify the matrix cells (warm re-run → REUSED; post-`kill -9` → auto-reclaim) still hold.

### HOLB baseline

Already restored by the worker fix (see the table). Just capture the before/after p99 as the documented single-worker proof and the baseline for any future multi-worker (`pgweb.workers`) comparison. With honest numbers now visible, re-read 015's multi-worker design against them and note in `BENCHMARKS.md` / `ROADMAP.md` if the urgency shifts.

---

## Documentation updates

- **Already done as part of the worker fix** (do not redo): the "single-worker reality drives 0%/n/a" misdiagnosis has been corrected in `docs/BENCHMARKS.md` (:5, :124, threshold section), `docs/internal/TESTING-SETUP.md` (acceptance record + `BENCH_MIN_STATIC_SUCCESS` knob row), and `bench/run.sh` comments; CHANGELOG has a `Fixed` entry for `729eb93`.
- **This work must additionally:** document the new thresholds (the per-tier baseline table + margins) and the loud-banner behavior in `docs/BENCHMARKS.md` "Regression threshold"; update the `BENCH_*` knob rows in `docs/internal/TESTING-SETUP.md` (add `BENCH_MIN_SUCCESS` / `BENCH_STRICT` / `BENCH_LOADGEN` / `BENCH_SELFTEST` as applicable); and if the gate materially tightens, note in `CLAUDE.md` "Performance characterization" that the bench is now a real throughput/tail gate (it now *means* something).

## Constraints & invariants to respect

- **CLAUDE.md startup gate is non-negotiable.** Full `RUN_BENCH=1 scripts/test-all.sh` bookend before *and* after; quote `PGWEB-RESULT` + both `PGWEB-BENCH` lines verbatim.
- **Do not regress 029 idempotency.** Self-healing lock, unconditional `reclaim_environment`, shared content-hash freshness, unified image tag — all stay. Any new container must be torn down on exit and surgically reclaimed (B.3).
- **Flags are debugging-only; a non-green default run is a real bug, not flakiness** (029 rule). The point of B.1/B.2 is to make a non-green bench *meaningful and loud* — never reintroduce a loose gate to dodge a real regression.
- **No test tier weakened, no skips.** Still 5 tiers + bench; Docker mandatory.
- **Phase-neutral measurement work.** No Phase-2 features.
- **Companion-app rule:** a bench-only change exercises the dedicated bench app — sufficient for bench. (No `examples/todo/` flow needed unless server code changes.)
- **Reproducibility:** keep `oha` pinned (`OHA_VERSION`); pin any new tool/base image and justify in `docs/BENCHMARKS.md` as the oha choice is justified. Must work on macOS (Apple Silicon) dev and Linux CI.
- **Sequential only:** the `:8080` lock holds; the 029 lock makes sequential re-runs safe but does not license parallel runs.

## Acceptance criteria

1. **Gate tightened** to per-workload **≥99% success floor + per-tier p99 ceilings + req/s floors on _successful_ requests**, env-tunable, replacing the `≥1%` placeholder. Thresholds documented in `BENCHMARKS.md`.
2. **A synthetic injected regression flips `PGWEB-BENCH … OVERALL=fail`** and triggers the banner (demonstrate it; keep a `BENCH_SELFTEST` or documented manual injection).
3. **Loud regression banner** prints an itemized breakdown on any failure **at every verbosity** (`errors`/`short`/`verbose`) and on infra/early-exit (via the EXIT trap), in addition to the unchanged `PGWEB-BENCH` verdict line.
4. **`bench/run.sh` remains fully idempotent** per 029 (lock self-heals; `reclaim_environment` cleans any new load-gen container; shared freshness/tag); a `kill -9` mid-bench + immediate re-run self-recovers with no flag.
5. **(If B.3 done)** in-network is selectable via `BENCH_LOADGEN` and produces ~equal numbers to the host path (since the proxy was never the bottleneck), portable across macOS/Linux.
6. **Bookend green:** `RUN_BENCH=1 scripts/test-all.sh` `OVERALL=PASS` with both `PGWEB-BENCH … OVERALL=ok` lines pasted in; `cargo check`/`clippy -D warnings` clean if any Rust changed.

## Open questions

1. **How tight to set p99 ceilings.** Lean: start ×2–3 over the per-tier baseline above; tighten once reproducible across several runs; most headroom on c=128 / 10k legs. Consider `BENCH_STRICT=1` for the tight ceilings while the success floor is always on.
2. **Where to store the baseline thresholds** — inline in `bench/run.sh` vs a sourced `bench/thresholds.sh` (cleaner to re-baseline per platform). Lean: a small sourced file with per-tier tables.
3. **Banner styling** — `!`-frame vs box-drawing. Lean: ASCII `!`/`=` frame (the marker contract is already ASCII-first for non-TTY/CI; the banner must be greppable and not rely on glyphs).
4. **Re-baseline on the deploy platform.** The table above is one Apple-Silicon/Docker-Desktop run. The Linux VPS / CI numbers will differ — capture a per-platform baseline before locking ceilings.
5. **Does the honest HOLB change the multi-worker urgency?** Now that the real p99 blow-up (~3.7ms → ~220ms) is visible, re-read 015's `pgweb.workers` design against it and record any shift in `BENCHMARKS.md` / `ROADMAP.md`.

---

*The benchmark's job is to tell the truth about the serving path and to be loud when it regresses. Part A found the truth (a worker that quietly died 8s after boot, now fixed). Part B makes the bench refuse to stay quiet next time: a ≥99% success floor + real p99/req-s ceilings, and a regression banner you cannot miss at any verbosity.*
