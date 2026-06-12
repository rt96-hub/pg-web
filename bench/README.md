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

## Regression guard
A full run is opt-in (`RUN_BENCH=1`). A future bench-smoke (very short duration +
generous p99 bound) can be wired into `scripts/test-all.sh` without making every
CI run expensive. See acceptance criteria in the prompt.

## Phase note
The benchmark (Step 1) is phase-neutral and valuable even if the multi-worker
design (Step 2 / Option A + B) is only designed and not yet implemented.
