# 030 — Benchmark methodology: kill the connection-churn artifact so `bench/run.sh` measures the real serving path

**Status:** Open handoff prompt — medium priority (no correctness/product bug; the *benchmark* can't currently measure what it exists to measure). Follows **029** (idempotent harness + shared content-hash image freshness; `bench/run.sh` now self-heals + rebuilds-on-stale) and **015** (the original bench harness + the single-worker/multi-worker concurrency design this bench is supposed to characterize).
**Date opened:** 2026-06-15
**Author:** Handoff from the owner, immediately after 029. **Trigger (direct):** *"why does the first set of benchmarks consistently have ~72% success and the others are always 0.0%?"* The answer turned out to be a **load-generator / Docker-Desktop measurement artifact, not a pg-web serving defect** — which means the benchmark today cannot measure throughput, tail latency, or head-of-line blocking (the entire point of 015). This prompt fixes the methodology so the numbers mean something, then tightens the regression gate that 029 deliberately left loose.
**Prerequisites:** 029 (bench is now idempotent — keep it that way). Strongly related: 015 (`docs/BENCHMARKS.md`, the HOLB experiment, the `pgweb.workers` multi-worker design the bench must justify), 016 (request-path caching — a hot-path change whose throughput effect the bench *should* be able to show but currently can't).

---

## Summary

`bench/run.sh` produces numbers that are dominated by a **measurement artifact**, not the server under test. In every run, the **first** workload (`a-static-c1`) reports ~72% success and **every** subsequent workload reports **0.00%** — including concurrency-**1** workloads (`b-todos100-c1`). The 0% legs are **100% TCP `connection error`** in the raw `oha` output (empty HTTP status-code distribution), **not** HTTP 5xx — i.e. connections never complete; the worker is not erroring. When a connection *does* get through, the worker returns clean `200`s (45,003 of them in the first workload). The artifact is **per-run and recovers between runs** (the first workload is ~72% in *every* run — D/E/H/the closing bookend/the old 028 record), which proves it is **not** a worker crash or a single-worker concurrency ceiling — it is exhaustion of a host/Docker-Desktop resource (ephemeral ports + the macOS Docker Desktop userspace port-forwarder's connection table) under raw connection volume, amplified by the bench reconnecting per request (no HTTP keep-alive on this path).

Consequence: the bench's **only** trustworthy signal today is "did the worker bind at all," which is exactly what its regression gate checks (`BENCH_MIN_STATIC_SUCCESS=1`, i.e. `a-static-c1` ≥ 1%). Real throughput, tail latency, the c=128 latency explosion, and the HOLB before/after — the things 015 built this harness to measure — are unmeasurable. And `docs/BENCHMARKS.md` actively **misdiagnoses** the 0% as "the single-worker reality the benchmark exists to expose" (`BENCHMARKS.md:133`), which a concurrency-1 leg at 0% disproves.

Two halves, in order:
- **Part A — explore + confirm the root cause** (don't fix blind; produce a short written finding).
- **Part B — fix the methodology** so the default measurement exercises the real serving path, re-establish a meaningful HOLB result, then **tighten the regression gate** from "did it serve at all" to real per-workload p99 ceilings + req/s floors + a success-rate floor (the tightening 029 explicitly deferred until "a stable green baseline exists").

## Why this matters now

- **015's purpose is currently unmet.** The harness exists to quantify the single-worker concurrency ceiling + HOLB and to justify the multi-worker design (`pgweb.workers`, `SO_REUSEPORT`). The artifact swamps that signal — we cannot honestly say "the single worker tops out at X req/s with p99 Y" because the host/proxy fails the connections before the worker is the bottleneck.
- **Hot-path changes can't be validated.** 016 (request-path caching) and any future change to SPI-per-request / Tera / routing / the worker model are supposed to be guarded by this bench. Today a real catastrophic throughput regression on an otherwise-healthy server would still pass the gate, because the gate only checks that the worker *binds*.
- **The published numbers mislead.** Lines like "`req/s=28655` succ=0.00%" (oha counts errored connection attempts in `Requests/sec`) and the "single-worker reality" caveat lead a reader to the wrong conclusion about both the server and the design.
- **029 left a placeholder.** `BENCHMARKS.md:138` says "029 should establish a stable green baseline and then tighten." 029 correctly did **not** tighten — because the artifact makes a stable, *meaningful* baseline impossible. This prompt removes the blocker and does the tightening.

## Current behavior — the evidence

Raw `oha` result files from a real run (`bench/results/*.txt`), constrained 1c/2g tier:

| workload | concurrency | HTTP 200s | error distribution | `oha` success |
|---|---|---|---|---|
| `a-static-c1`   | 1   | **45,003** | `17,858 connection error` (+1 deadline, +1 closed) | **71.59%** |
| `a-static-c32`  | 32  | **0**      | `226,637 connection error`                          | **0.00%** |
| `a-static-c128` | 128 | **0**      | `connection error` (all)                            | **0.00%** |
| `b-todos100-c1` | **1** | **0**    | `60,386 connection error`                           | **0.00%** |

Key tells (all from the captured output, not theory):
1. **Connection errors, not 5xx.** The 0% legs have an **empty `Status code distribution`** — zero HTTP responses. `oha`'s `Error distribution` is `connection error`. The handler/worker is not returning errors; TCP connections fail to complete.
2. **The worker serves fine when reached.** `a-static-c1` returns 45,003 clean `[200]` responses before erroring.
3. **Concurrency is not the cause.** `b-todos100-c1` is concurrency **1** (one request at a time) and is still 0%. A single-threaded worker handles serial requests trivially; this cannot be a concurrency ceiling.
4. **It's ordering + a per-run resource, and it recovers between runs.** Only the *first* workload of a run partially succeeds; everything after is 0%. Yet `a-static-c1` is ~72% in **every** run (Cells D/E/H, the closing bookend, and the pre-existing 028 record). A crashed worker would not recover without a rebuild; a per-run host resource that drains during the (minutes-long, stack-rebuilding) gap between runs does.

Mechanism (strongly supported by 1–4, exact internal limit to be confirmed in Part A):
- The throughput legs run `oha … -z 10s -c N` with **no keep-alive flag** (`bench/run.sh:297,299` — `"$OHA_CMD" --no-tui --no-color -z 10s "$@" "$url"`). At ~6k req/s × 10s that is **~60k TCP connections per workload**. If the path reconnects per request (server likely closes connections — consistent with invariant #4 "one request = one SPI transaction" and the Axum-over-custom-listener design), each request is a fresh connection.
- On macOS, published ports (`bench/docker-compose.yml:27`, `8080:8080`, "load generator hits this from host") are forwarded through the **Docker Desktop userspace proxy (vpnkit / gvisor-tap-vsock)** plus host ephemeral-port + `TIME_WAIT` budget. ~60k connections/10s exhausts that table; the first workload drains it partway (→ ~72%), and because workloads run **back-to-back** (`run_workloads`, `bench/run.sh:307`) the table never recovers within a run → every subsequent leg is ~100% connection errors → 0%.

Relevant code: `bench/run.sh` — `run_oha` (`:290`, the `-z 10s -c N` invocation `:297/299`), `run_workloads` (`:307`, the 12 back-to-back legs + the HOLB block `:342/348/350`), `_parse_oha` (`:256`, normalizes `NaN`→`n/a`), `evaluate_threshold` (`:388`) + `BENCH_MIN_STATIC_SUCCESS` (`:93`). Compose topology: `bench/docker-compose.yml:25-27` (host-published `8080`/`5432`, no dedicated network). Misdiagnosis to correct: `docs/BENCHMARKS.md:133-138` (+ the bench knob row in `docs/internal/TESTING-SETUP.md` that 029 wrote, which repeats "single-worker reality drives 0%/n/a").

## Part A — explore + confirm the root cause (do not fix blind)

Guiding principle: **measure, then fix.** Produce a short written finding (a paragraph in `docs/BENCHMARKS.md` or a session note) *before* implementing Part B, so the fix targets the confirmed bottleneck rather than a guess. Suggested experiments (cheap, sequential, respect the `:8080` lock — bring a bench stack up once and probe it):

1. **Confirm connection reuse / keep-alive behavior.** Against a running stack: `curl -v http://localhost:8080/bench/static` and inspect the `Connection:` response header; check whether the listener serves one-request-per-connection or sends `Connection: close`. Run `oha -c 1 -z 5s` and watch `netstat -an | grep -c TIME_WAIT` climb. **Decide:** is keep-alive absent because the *server* doesn't offer it (a real prod-relevant gap) or only because the bench doesn't ask for it?
2. **Localize the exhausted resource — the decisive experiment.** Run the *same* `oha` load two ways and compare success rates:
   - **host → published port** (today's path: `oha` on the host hitting `localhost:8080` through the Docker Desktop proxy), vs
   - **container → container, inside the Docker network** (an `oha` one-shot container on the bench compose network hitting `http://postgres:8080` directly, bypassing the host proxy + host NAT).
   If in-network is ~100% and host→proxy craters, the **Docker Desktop port-forwarder is the bottleneck** (the expected macOS result). If in-network *also* craters, the limit is the server's accept path / listen backlog and the diagnosis changes — chase that instead.
3. **Quantify the cliff.** At what connection rate / count does host→proxy start erroring? Does `oha`'s keep-alive (reused connections, far fewer sockets) make the host path succeed? Capture `sysctl net.inet.ip.portrange.first net.inet.tcp.msl` and a `TIME_WAIT` count at the moment it starts failing.
4. **Cross-check the real target if feasible.** The deploy target is a Linux VPS (no userspace port proxy). If a Linux box / CI runner is available, run the host-path load there; expect it *not* to exhibit the artifact, which both confirms the macOS-proxy diagnosis and tells you what the gate thresholds should look like on the real platform.

Deliverable of Part A: a 1–2 paragraph written finding stating (a) keep-alive present? server-side or bench-side?, (b) which resource is exhausted (proxy vs host ports vs server accept), (c) the in-network vs host-path success delta. That finding drives the Part-B choices below.

## Part B — fix the methodology

Lean recommendations (revisit per Part A's finding):

1. **Primary fix — run the load generator inside the Docker network.** Add an `oha` (still pinned — see invariants) one-shot/sidecar container on the bench compose network that targets `http://postgres:8080` directly, and make **in-network the default measurement path**. This bypasses the macOS Docker Desktop port-forwarder and host NAT entirely — the confirmed (Part A) root cause — and has the bonus that the measured path is identical on macOS dev and Linux CI (portable numbers). Keep the host→published-port path available behind a flag (e.g. `BENCH_LOADGEN=host`) for comparison, but it is no longer the number of record. Publishing `8080` on the host stays (humans + `pg-web push` still want it); the *load* just stops going through it.
2. **Stop the connection churn.** Drive the throughput legs with a **bounded, reused connection pool** (HTTP keep-alive) and **constant-arrival-rate (`-q`, open model)** as 015 intended for honest tail-latency measurement (`docs/BENCHMARKS.md` already documents the open-model rationale). Measure the server's ceiling, not the host's socket budget. If Part A shows the **server** doesn't keep-alive and that's the churn source, evaluate fixing the listener to support keep-alive — but that is a request-path change (see invariants: it must keep **one request = one SPI transaction** — keep-alive reuses the *socket*, it must **not** batch SPI transactions; full test+bench bookend; a companion-app flow). Only do the server change if it's small, safe, and Phase-1-appropriate; otherwise measure with whatever the server supports and document it. Do not contort the SPI-per-request invariant for a benchmark.
3. **Re-establish a meaningful HOLB experiment.** The head-of-line-blocking before/after (`b-todos100-c16-pure` vs `d-fast-under-slow`, `bench/run.sh:338-359`) is the 015 headline and is currently meaningless (both 0%). Once the fast path actually serves under load, this becomes the real single-worker proof and the before/after baseline for the multi-worker design.
4. **Then tighten the regression gate (the 029-deferred work).** With the artifact gone and a stable green baseline established (run it several times; record the spread), **replace** the "did it serve at all" placeholder (`BENCH_MIN_STATIC_SUCCESS`, `evaluate_threshold`) with real, data-driven, env-tunable gates:
   - a **success-rate floor** (e.g. ≥ 99% on the static + todos c1/c32 legs on a healthy server),
   - **per-workload p99 ceilings** (baseline × margin),
   - **req/s floors on *successful* requests** (not oha's error-inclusive `Requests/sec`).
   Set thresholds from the new baseline with headroom; keep them per-tier (the 1c/2g cgroup yields different numbers than unconstrained — separate baselines). A synthetic regression (e.g. an injected `pg_sleep` on the fast path) must flip `PGWEB-BENCH … OVERALL=fail`. Start slightly loose and tighten once reproducible across a few runs; document every threshold in `BENCHMARKS.md`.

## Documentation updates (required)

- **`docs/BENCHMARKS.md`** — **correct the misdiagnosis** at `:133-138`: the 0%/`n/a` legs are a **connection-churn / Docker-Desktop-proxy measurement artifact**, *not* "the single-worker reality" (a concurrency-1 leg at 0% disproves that framing). Document the new in-network methodology, the real numbers it produces, the (now meaningful) HOLB before/after, and the tightened gate. Update "Honest caveats" (`:95`) and "Regression threshold" (`:131`).
- **`docs/internal/TESTING-SETUP.md`** — correct the `BENCH_MIN_STATIC_SUCCESS` knob row (029 wrote "deliberately loose — the single-worker reality drives 0%/n/a"; that attribution is wrong) and update the bench acceptance record with the post-fix numbers + the new gate.
- **`CLAUDE.md`** — if the gate tightens materially or the default load path changes, update the "Performance characterization" section (the bench is the required validation for hot-path changes — that statement must stay true and now actually *means* something).
- If a **server keep-alive** change lands: note it in `docs/ARCHITECTURE.md` and reaffirm it respects invariant #4 in the same commit.

## Constraints & invariants to respect

- **The CLAUDE.md startup gate is non-negotiable.** Run the full `RUN_BENCH=1 scripts/test-all.sh` bookend before *and* after; quote the `PGWEB-RESULT` + both `PGWEB-BENCH` lines verbatim. This change is squarely bench/hot-path territory — `RUN_BENCH=1` is mandatory, not optional.
- **Do not regress 029's idempotency.** `bench/run.sh` must stay fully idempotent: self-healing lock, unconditional `reclaim_environment`, shared content-hash image freshness, unified `pgweb_image` tag. Any **new load-gen container** must (a) be torn down on exit (extend `stop_stack` / the `bench_on_exit` trap), and (b) be removed by `reclaim_environment` in `scripts/lib/harness.sh` — add its name to the surgical families (e.g. a `bench-loadgen*` / `pgweb-loadgen*` prefix), **never** a blanket prune. Re-verify the matrix-style cells (warm re-run → REUSED, post-`kill -9` → auto-reclaim) still hold with the new container in play.
- **029's "flags are debugging-only; a non-green default run is a real bug, not flakiness" rule holds.** Do **not** keep (or reintroduce) a loose gate as a way to dodge a real regression. The whole point of Part B.4 is that a non-green bench becomes *meaningful*.
- **No gate weakened, no skips.** Still 5 tiers + bench; Docker mandatory. Tightening the bench gate *adds* rigor; do not relax any test tier to accommodate bench changes.
- **Listener/request-path invariants** (if Part B.2 touches the server): #2 (HTTPS is out-of-process — never add TLS termination to the extension), #4 (one HTTP request = one SPI transaction — keep-alive must not batch/share transactions across requests), #7 (async only in the BGW, never in `#[pg_extern]`). Read `docs/ARCHITECTURE.md` + the relevant `http.rs`/listener code first and flag any invariant tension before coding.
- **Phase discipline.** This is phase-neutral measurement work (like 015 itself); do not smuggle Phase 2 features into it.
- **Companion-app rule.** A *bench-only* change (in-network load gen, gate tightening) exercises the dedicated bench app, which satisfies the rule for bench. A *server* change (keep-alive) additionally needs an `examples/todo/` flow + substantial explanatory comments per "Demo app as living documentation."
- **Reproducibility / tooling.** Keep `oha` pinned (`OHA_VERSION`); if a tool is added or swapped (e.g. running `oha` as a container image, or `wrk`/`vegeta`), pin it and justify in `docs/BENCHMARKS.md` exactly as the existing oha justification does. Must work on macOS (Apple Silicon) dev **and** Linux CI — the in-network approach helps both.
- **Sequential only.** The `:8080` contention rule (CLAUDE.md) still holds; the 029 lock makes sequential re-runs safe but does not license parallel runs. Part-A probes share the lock.

## Acceptance criteria

1. **Root cause confirmed + written up** (keep-alive present? server- or bench-side?; which resource is exhausted; in-network vs host-path success delta) before the fix lands.
2. **The default bench no longer hits the artifact:** the static + todos legs report **near-100% success with real p50/p99** (no `0.00%`/`n/a` on a healthy server). `req/s` reflects successful requests.
3. **The HOLB experiment produces a real before/after** (fast-path p99 with vs without the concurrent slow injector) on a serving path — the 015 headline result is restored.
4. **The regression gate is tightened** to data-driven, env-tunable per-workload **success-rate floors + p99 ceilings + req/s floors** (per tier), replacing the "did it serve at all" placeholder; a synthetic injected regression flips `PGWEB-BENCH … OVERALL=fail`. Thresholds documented in `BENCHMARKS.md`.
5. **`bench/run.sh` remains fully idempotent per 029** (lock self-heals; `reclaim_environment` cleans any new load-gen container; shared freshness/tag); a `kill -9` mid-bench + immediate re-run self-recovers with no flag.
6. **Docs corrected** — the "single-worker reality" misdiagnosis is replaced with the real explanation + methodology + numbers (`BENCHMARKS.md`, `TESTING-SETUP.md`).
7. **Bookend green:** `RUN_BENCH=1 scripts/test-all.sh` `OVERALL=PASS` with both `PGWEB-BENCH … OVERALL=ok` lines pasted into the completion report; `cargo check --workspace` + `cargo clippy --workspace -- -D warnings` clean if any Rust changed.

## Open questions

1. **In-network load gen vs host with raised limits.** Lean: **in-network** — it bypasses the confirmed root cause (the Docker Desktop proxy), is portable to Linux CI, and needs no fragile host `sysctl` tuning. Revisit only if a containerized `oha` can't run as a clean, pinned one-shot.
2. **Fix server keep-alive, or just measure as-is?** Lean: **investigate** in Part A; fix the listener only if it's a small, safe, Phase-1 change that preserves invariant #4 (it's genuine prod value — fewer reconnects). Otherwise document the server's behavior and measure within it. Never bend the SPI-per-request invariant for a bench.
3. **`oha` (as a container) vs `wrk`/`vegeta`.** Lean: **keep `oha`** (already pinned + justified, first-class `-q` open-model + percentiles) unless it can't run cleanly in-network; pin whatever lands.
4. **How tight to set the new gate.** Lean: success ≥ 99% on static/todos c1+c32; p99 ceilings at baseline × margin; req/s floors at a fraction of baseline; all env-tunable; per tier; start slightly loose, tighten once stable across several runs. Consider a `BENCH_STRICT=1` for the tight gate while a sane default protects against the worst regressions.
5. **Separate thresholds for the 1c/2g vs unconstrained tier?** Lean: **yes** — the cgroup materially changes the numbers; record a baseline per tier.
6. **Does fixing the measurement change the multi-worker story?** Once the single worker's *real* ceiling and HOLB tail are visible (not masked by the artifact), re-read 015's multi-worker design against the honest numbers — the urgency/justification for `pgweb.workers` may shift. Capture that in `BENCHMARKS.md` / `ROADMAP.md` if so.

---

*The benchmark's job is to tell the truth about the serving path. Right now it mostly measures the macOS Docker Desktop port-forwarder's connection table. Confirm that with the in-network-vs-host experiment, move the load off the proxy, restore a real HOLB result, and only then turn the loose "did it serve at all" gate into a real "did throughput/tail regress" gate — without ever weakening a test tier or papering over a regression with a flag.*
