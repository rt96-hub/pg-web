# pg-web benchmark harness (prompt 015)

This directory contains the reproducible harness for the concurrency, throughput,
and head-of-line-blocking measurements required by `prompts/015_concurrency_throughput_and_benchmark.md`.

## Layout
- `docker-compose.yml` — stack shape matching `examples/todo/` (rtaylor96/pg-web:latest, published :8080/:5432).
- `app/` — a minimal dedicated bench app (never contorts `examples/todo/`).
  - `pages/bench/static/*` — workload (a)
  - `pages/bench/todos/*`   — workload (b) (re-seeded to 100 and 10 k rows)
  - `pages/bench/write/post.sql` — workload (c)
  - `pages/bench/slow/*`     — workload (d) (the HOLB injector)
- `run.sh` — the documented entry point. Boots, pushes via the in-image CLI (F.3),
  seeds, runs the matrix, writes raw oha output under `results/`.
- `results/` — git-ignored; populated by runs.

## Running
```bash
# Full power (comparison tier on this machine)
bash bench/run.sh

# Primary tier under the VISION claim (1 vCPU / 2 GiB)
BENCH_CPUS=1 BENCH_MEM=2g bash bench/run.sh

# Opt-in from the full test suite (heavy; CI would gate on RUN_BENCH=1)
RUN_BENCH=1 scripts/test-all.sh
```

The script downloads a pinned `oha` (or uses one from PATH) so only Docker + a checkout are required for a third party to reproduce.

## Tool choice (oha)
See the justification and open/closed model discussion in `docs/BENCHMARKS.md`.
`oha` was selected because:
- Static single binary (no Node, no heavy runtime) — matches the project ethos.
- Excellent built-in percentile + histogram output (p50 … p99.9).
- `-q` / `--qps` gives constant-arrival-rate (open model) load — essential for the
  slow-handler HOLB experiment (a closed-model tool would mask the damage by
  self-throttling behind the slow request).

## Results & reporting
Raw `.txt` files + a `summary.txt` are written on every run. `docs/BENCHMARKS.md`
is the human-curated, published report (method + tables + caveats + HOLB graph
description + reconciliation of `VISION.md:58`).

## Regression guard (prompt 030)
A full run is opt-in (`RUN_BENCH=1`). The gate is now real, not a smoke check:

- **Always on:** a per-workload **≥ 99 % success floor** (`BENCH_MIN_SUCCESS`).
  Platform-independent — catches a dead / crash-looping / not-serving worker
  (e.g. the 016 self-termination regression). Below the floor ⇒ `OVERALL=fail`.
- **Opt-in (`BENCH_STRICT=1`):** per-tier **p99 ceilings** (baseline ×
  `BENCH_P99_MARGIN`) + **successful-req/s floors** (baseline ×
  `BENCH_RPS_FLOOR_FRAC`). Off by default because the baselines are
  platform-specific — re-baseline in `thresholds.sh` for your platform first.
- **Any breach (or infra/early exit)** prints a loud, itemized `BENCH REGRESSION
  DETECTED` banner at *every* `TEST_MODE`, plus the greppable `PGWEB-BENCH …
  OVERALL=fail` line.

Knobs + per-tier baselines live in `thresholds.sh`. `BENCH_SELFTEST=1` injects a
guaranteed regression to prove the gate is live. Full reference:
`docs/BENCHMARKS.md` § Regression threshold.

## Phase note
The benchmark (Step 1) is phase-neutral and valuable even if the multi-worker
design (Step 2 / Option A + B) is only designed and not yet implemented.
