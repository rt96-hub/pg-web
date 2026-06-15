# 029 — Test & benchmark harness: full idempotency + proof that every script is currently green

**Status:** Open handoff prompt — high priority. Follows **028** (summary modes + clear step markers); 028's explicit `REUSED`/`BUILT`/`CANARY`/`PGWEB-RESULT` markers are what make this prompt's verification observable. Do 028 first, then this.
**Date opened:** 2026-06-15
**Author:** Handoff from the owner. Direct quote of the requirement: *"this needs to be an entirely idempotent process, I don't even care if it is slow, it just needs to work when I put in `./scripts/test-all.sh` or `./bench/run.sh` … without a bunch of 'because we changed this random file, the docker needs rebuilt manually or put in a flag'."*
**Prerequisites:** 028 (for the markers used in the verification protocol). Related: 024 (the historical watcher flake — must stay fixed), 025 (the content-hash freshness + canary work this builds on).

---

## Summary

The harness *mostly* self-heals, but there are concrete, identifiable gaps where a run's correctness depends on prior state — the machine being clean, the last run having exited cleanly, the right files having "newer" mtimes, or the developer remembering a flag. Those gaps are the source of the "flaky bullshit" the owner is describing: a run that passes once and fails the next, a `bench/run.sh` that silently serves a stale image after a code change, a crashed run that poisons every subsequent run until manual cleanup.

The contract this prompt establishes is blunt: **`./scripts/test-all.sh`, `RUN_BENCH=1 ./scripts/test-all.sh`, and `./bench/run.sh` must each produce a correct result every single time** — on a cold machine, on back-to-back runs, after editing any source file, after a branch switch or stash, and after a previous run was killed mid-flight leaving containers / locks / temp dirs behind — **with zero manual hygiene and zero manual flags.** Slowness is explicitly acceptable. Manual steps and "you have to pass `REBUILD_IMAGE=1` because you touched a file" are explicitly **not**.

Two halves:
- **Part A — fix the non-idempotent paths** (enumerated below, each grounded in the current scripts).
- **Part B — prove it.** Run an adversarial verification matrix (cold, warm, post-edit, post-crash, branch-noise) with no manual intervention between runs, and record every run's compact `PGWEB-RESULT` / `PGWEB-BENCH` line. Acceptance is "every cell green, with the expected image-marker behavior, no flags, no cleanup."

## Why this matters now

- **`bench/run.sh` silently runs against stale images.** This is the literal complaint. `ensure_image` (`bench/run.sh:51-64`) rebuilds **only** if the image is missing or `REBUILD_IMAGE=1` is set — it has *no content-hash check*. Change extension code, run `./bench/run.sh`, and you benchmark the old binary with no warning. The user found this by hand.
- **A crashed run poisons the next one.** The lock dir `/tmp/pg-web-test-all.lockdir` (`scripts/test-all.sh:56-75`) is created on entry and only removed by the EXIT trap. If a run is `kill -9`'d (or the machine sleeps and the shell dies), the dir survives, and **every** subsequent run aborts with "Another scripts/test-all.sh appears to be running" and demands `FORCE=1`. The comment claims "stale lock detection" but the code does none — it just refuses. That is the opposite of idempotent.
- **Manual hygiene is currently a prerequisite, documented as the user's job.** `CLAUDE.md:25-30` tells the operator to run `cargo pgrx stop`, `pkill`, and `docker rm -f` *before* the bookend. The owner wants the script to do that itself, every time, unconditionally — "I don't care if it's slow."
- **Agents weaponize the flags as excuses.** When a run isn't green, the path of least resistance today is `REBUILD_IMAGE=1`, `SKIP_IMAGE_CHECK=1`, or `FORCE=1` — and then a report that says "it's just flaky, passed with the flag." Once the default path is genuinely idempotent, those flags become unnecessary, and CLAUDE.md can forbid using them as a workaround. A non-green default run becomes a real bug to fix, not a thing to flag past.

## Current behavior — the non-idempotent paths (evidence)

1. **Bench image freshness is absent.** `bench/run.sh:51-64` (`ensure_image`) — missing-or-`REBUILD_IMAGE` only. Compare `scripts/test-all.sh:175-260` (`ensure_image_fresh`), which has the mtime fast-path **and** the `pgweb.src_hash` content-hash compare. The two entrypoints disagree on what "fresh" means; the bench one is wrong.
2. **Image tag is hardcoded in bench, parameterized elsewhere.** `bench/run.sh` hardcodes `rtaylor96/pg-web:latest` (`:55,110`) and `CONTAINER_NAME="bench-postgres-1"` (`:37`), while `scripts/test-all.sh` threads `TEST_IMAGE`/`PGWEB_IMAGE` (`:173`) and `build-image.sh` honours `PGWEB_IMAGE` (`build-image.sh:22`). Drift risk; a tag override applied to one path silently doesn't apply to the other.
3. **The lock is not self-healing.** `scripts/test-all.sh:62-75` — `mkdir` lock, no PID recorded, no liveness check, no staleness reclaim. A dead owner blocks forever absent `FORCE=1`.
4. **No unconditional startup reclaim.** `stop_pgrx_dev_pg` is called early (`scripts/test-all.sh:97`) and before tier 4 (`:387`) — good for the pgrx PG — but nothing `docker rm -f`s leftover containers from a crashed prior run: the canary (`pgweb-canary-$$`, `:325`), a half-up tier-4 smoke compose project (`SMOKE_DIR=/tmp/pg-web-smoke-$$`, `:396`), or a leftover bench compose stack (`bench/run.sh` only cleans *its own* project via `down` at start, `:112`, and won't touch a stranded canary or smoke stack). A stranded container holding `:8080` (e.g. a leftover `pg-web up`) is caught late by `test-http.sh`'s port-shadow preflight (`test-http.sh:97-158`) but not proactively cleared.
5. **The image-watch set may be incomplete.** Both the mtime path (`scripts/test-all.sh:219-233`) and the content-hash (`:246-254`, and `build-image.sh:34-45`) watch a *fixed enumerated list*: `crates/pg_web_ext/src`, `crates/pg_web_cli/src`, `Dockerfile`, `.dockerignore`, `docker/init-pgweb.sh`, `Cargo.toml`, `Cargo.lock`, plus `examples` (hash only). Any image-affecting input **not** in that list (another file under `docker/`, a new build asset, a workspace member added later) changes nothing and yields a false "fresh" → stale image → flaky tier 3/bench. The two lists also differ (mtime omits `examples/`), which is a latent inconsistency.
6. **Temp-dir / results accumulation.** `/tmp/pg-web-smoke-$$` (`:396`) and (post-028) per-run log dirs accumulate across runs; a crashed run's compose project under an old smoke dir can linger. Nothing reaps them.
7. **Stale `FORCE=1` friction documented as normal.** `CLAUDE.md:25-30` pre-bookend hygiene + the `FORCE=1` advice (`scripts/test-all.sh:67`) institutionalize manual cleanup. Once A is done, this should be deleted/relaxed.

## Part A — make every path idempotent

Guiding principle: **when in doubt, do the safe expensive thing** (reclaim, rebuild, re-seed). The user has explicitly traded time for reliability. Never make correctness depend on prior state or a remembered flag.

1. **Unify image freshness; make bench use it.** Extract the content-hash freshness logic into a single shared helper (e.g. `scripts/lib/image.sh` sourced by both, or a `pg-web`-internal step) and call it from **both** `scripts/test-all.sh` and `bench/run.sh`. After this, `./bench/run.sh` on a changed tree rebuilds automatically and the 028 marker shows `STALE→BUILD→BUILT`; on an unchanged tree it shows `REUSED`. No flag, ever, on the default path.
2. **Make the image-watch set provably complete.** Stop enumerating a hand-maintained file list. Derive the hash from the **actual Docker build context** — i.e. the set of files `docker build` would send given `.dockerignore` — or hash the whole repo tree minus an explicit volatile denylist (`target/`, `bench/results/`, `.git/`, `*.log`, the per-run log dirs, `node_modules`-style dirs). Either way, no image-affecting file can be silently missed. Keep a fast mtime/`git`-status pre-check if you want speed, but the content hash is the source of truth, and the cost (a `find | sha256sum`, ~1s) is acceptable. Both entrypoints and `build-image.sh` must compute the hash identically (one shared function) so the baked `pgweb.src_hash` LABEL always matches what the checker computes.
3. **Unify the image tag.** `bench/run.sh` (and `bench/docker-compose.yml`) must honour `TEST_IMAGE`/`PGWEB_IMAGE` exactly as `scripts/test-all.sh` does; remove the hardcoded `rtaylor96/pg-web:latest` literals (`bench/run.sh:55,110`). One source of truth for the tag across test-all, bench, build-image, smoke-cli, docker_e2e.
4. **Self-healing PID-aware lock.** Write the owning PID (and a timestamp) into the lock dir. On `mkdir` failure: if the recorded PID is not alive (`kill -0`), reclaim the lock automatically and proceed (log a `LOCK reclaimed (stale owner pid=… dead)` marker); only a genuinely-alive concurrent run blocks (and that block is correct). `FORCE=1` becomes a rarely-needed escape hatch, not a routine requirement.
5. **Unconditional startup reclaim in both entrypoints.** A single idempotent `reclaim_environment` step at the very top of both `scripts/test-all.sh` and `bench/run.sh` that:
   - stops the pgrx dev PG if running (already have `stop_pgrx_dev_pg`),
   - `docker rm -f` any container whose name matches the pg-web families: `pgweb-canary-*`, the smoke compose project(s), the bench compose project(s), and any stray `pg-web`/`bench`/`smoke`-named container (the families listed in `CLAUDE.md:29`),
   - `docker compose down --remove-orphans --volumes` for the bench and smoke projects (idempotent; no-op when absent),
   - reaps stale `/tmp/pg-web-smoke-*` dirs and old per-run log dirs,
   - reclaims a stale lock (per #4).
   This runs **every time**, regardless of mode or prior state. It is the "I don't care if it's slow" guarantee. Make it safe to run when nothing is present (all `|| true`, existence-checked).
6. **Guarantee fresh DB/seed state per run.** Confirm (and fix if needed) that each Docker-backed phase starts from clean volumes: bench already does `down --volumes` (`bench/run.sh:112`); ensure tier-4 smoke (fresh `SMOKE_DIR`) and the bench/seed truncation (`seed_todos` `TRUNCATE`, `bench/run.sh:171-179`) cannot carry state across runs. The canary uses `--rm` (`scripts/test-all.sh:325`); the reclaim step covers the kill-before-`rm` case.
7. **Keep the Docker build itself idempotent + tag-atomic.** `build-image.sh` already tags only on success and prunes danglers (`build-image.sh:49-71`). Verify a half-failed build cannot leave the tag pointing at a partial image, and that re-running after a failed build cleanly rebuilds. If BuildKit cache mounts landed (025 #3), confirm a cache-corruption case still converges on a re-run.

## Part B — prove every script is currently green (the verification matrix)

This is the "verify all the test and benchmark scripts are CURRENTLY passing" deliverable. Run the matrix below **in order, with no manual cleanup or flags between cells**, capturing each run's compact 028 output. Every cell must be green with the expected image-marker behavior. Record the `PGWEB-RESULT` / `PGWEB-BENCH` line + wall time for each in `docs/internal/TESTING-SETUP.md`'s verification table.

| # | Action (no manual steps between) | Must observe |
|---|---|---|
| A | `./scripts/test-all.sh` from a clean tree | `OVERALL=PASS`, all 5 tiers `x/x` `failed=0`; image `BUILT` or `REUSED` |
| B | `./scripts/test-all.sh` **again immediately** (warm, no cleanup) | `OVERALL=PASS`; image marker = **REUSED** (proves no false rebuild) |
| C | `touch`/no-op edit a file under `crates/pg_web_ext/src/`, then `./scripts/test-all.sh` | image marker = **STALE→BUILD→BUILT** with **no flag**, then `OVERALL=PASS` |
| D | `./bench/run.sh` immediately after C, **no flags** | rebuilds-if-needed automatically (REUSED here, since C built it), clean numbers, `PGWEB-BENCH … OVERALL=ok` |
| E | `RUN_BENCH=1 ./scripts/test-all.sh` | all 5 tiers + bench green in one invocation |
| F | Start `./scripts/test-all.sh`, `kill -9` it mid-tier-3 (leaving a lock dir + a canary/smoke container), then **immediately** `./scripts/test-all.sh` again | second run auto-reclaims the lock + containers (markers say so), `OVERALL=PASS`, **no `FORCE=1`, no manual `docker rm`** |
| G | `git stash` then `git stash pop` (mtime noise), then `./scripts/test-all.sh` | content-hash decides correctly: **REUSED** if content unchanged (no false rebuild from mtime noise), green either way, no flag |
| H | `BENCH_CPUS=1 BENCH_MEM=2g ./bench/run.sh` (the primary VISION tier) | clean numbers, `OVERALL=ok` |

If any cell is red or needs a manual step / flag, that is a Part-A bug — fix it and re-run the affected cells. The matrix is the acceptance gate. (Run cells sequentially, never in parallel background jobs — the `:8080` contention rule still holds; the lock + reclaim make sequential runs safe.)

If, while proving green, a **real product/test bug** surfaces (e.g. a genuine tier-3 failure unrelated to harness idempotency), stop and report it per the CLAUDE.md startup-gate rule (exact command, failing `PGWEB-RESULT` + surfaced detail, root-cause analysis) — do **not** paper over it with a flag, an `#[ignore]`, or a changed expectation. Harness idempotency and product correctness are separate; this prompt owns the former and must not mask the latter.

## Documentation updates (required)

- **`CLAUDE.md`** — rewrite the "Critical startup gate" / pre-bookend hygiene block (`:25-30`) and the `FORCE=1`/rebuild guidance:
  - State plainly: the entrypoints **self-clean and self-detect-staleness**; just run `./scripts/test-all.sh` (or `RUN_BENCH=1 …`) / `./bench/run.sh` — no manual `pkill`/`docker rm`/`cargo pgrx stop` first, no `REBUILD_IMAGE`/`SKIP_IMAGE_CHECK`/`FORCE` on the normal path.
  - **New rule:** `REBUILD_IMAGE` / `SKIP_IMAGE_CHECK` / `FORCE` are debugging-only and **must not** be used to get a run green or to explain away a failure. A non-green default run is a real failure (product bug) or a harness-idempotency bug to fix — never a thing to flag past or attribute to "flakiness." If a run isn't green, report it; don't route around it.
  - Keep the "run sequentially, never parallel `&`" rule (still true).
- **`docs/TESTING.md`** — document the unconditional startup reclaim, the self-healing lock, the unified content-hash freshness (now used by bench too), and that the env knobs in `:27-31` are debugging aids, not routine inputs. Note bench now rebuilds-on-stale automatically.
- **`docs/BENCHMARKS.md`** — update "How to reproduce / regression guard" (`:106-116`): `./bench/run.sh` now auto-rebuilds on source change (no `REBUILD_IMAGE` needed); document the freshness behavior and the idempotency guarantee.
- **`docs/internal/TESTING-SETUP.md`** — record the Part-B verification matrix results (the table above, filled in with real `PGWEB-RESULT`/`PGWEB-BENCH` lines + wall times), and update "Harness-integrity fixes" (`:152-171`) + "Known flakes" (`:184-187`) to reflect that crashed-run recovery, stale-lock reclaim, and bench staleness are now handled automatically.

## Constraints & invariants to respect

- **No gate weakened.** Same five tiers, Docker tiers mandatory, no silent skips, no altered test expectations (per CLAUDE.md coding practices + the prompt-025/016 precedent). Idempotency is achieved by *doing more cleanup/rebuild work*, never by skipping or relaxing checks.
- **028's reporting contract is preserved.** The reclaim/rebuild decisions must surface as the explicit 028 markers (`LOCK reclaimed`, `REUSED`/`BUILT`, container-removal lines) so the verification matrix is observable.
- **The image is the shipped artifact.** Freshness must err toward rebuilding; a stale image passing tests is a false green and is exactly what this prompt eliminates.
- **Sequential-only.** The parallel-run `:8080` hazard is real (`CLAUDE.md:13`, `:96-98`); the lock + reclaim make *sequential* re-runs safe but do not license parallel runs.

## Acceptance criteria

1. `bench/run.sh` uses the **same content-hash image-freshness check** as `scripts/test-all.sh` (shared helper); a source change before `./bench/run.sh` triggers an automatic rebuild with **no flag**, surfaced via the 028 `STALE→BUILD→BUILT` markers.
2. The image-watch set is **provably complete** (derived from the build context or whole-tree-minus-denylist), and `build-image.sh` + both entrypoints compute the `src_hash` identically.
3. Image tag is unified on `TEST_IMAGE`/`PGWEB_IMAGE` across `test-all.sh`, `bench/run.sh`, `bench/docker-compose.yml`, `build-image.sh`, `smoke-cli.sh`, `docker_e2e.rs`; no hardcoded literals remain in bench.
4. The lock is **self-healing**: a dead owner is reclaimed automatically (with a marker); `FORCE=1` is no longer needed after a crash.
5. An **unconditional `reclaim_environment` step** runs at the top of both entrypoints (stop pgrx PG; `docker rm -f` canary/smoke/bench/pg-web containers; `compose down --remove-orphans --volumes` for bench+smoke; reap stale smoke/log dirs; reclaim stale lock) — idempotent and safe on a clean machine.
6. **The Part-B verification matrix (A–H) is fully green**, with no manual hygiene and no flags between cells, and the expected image-marker behavior at each cell (REUSED where unchanged, BUILT where changed, reclaim on the post-crash cell). Results recorded in `docs/internal/TESTING-SETUP.md`.
7. `CLAUDE.md`, `docs/TESTING.md`, `docs/BENCHMARKS.md`, `docs/internal/TESTING-SETUP.md` updated, including the **"flags are debugging-only; a non-green default run is a real bug, not flakiness to flag past"** rule.
8. `cargo check --workspace` + `cargo clippy --workspace -- -D warnings` clean (per the commit gate), and a final `RUN_BENCH=1 ./scripts/test-all.sh` green with its `PGWEB-RESULT`/`PGWEB-BENCH` lines pasted into the completion report.

## Open questions

1. **Build-context hashing vs whole-tree-minus-denylist.** Which is the more robust "provably complete" hash source? Deriving the exact `docker build` context (respecting `.dockerignore`) is most precise but more code; whole-tree-minus-denylist is simpler and strictly safe (over-rebuilds at worst). Lean: whole-tree-minus-denylist (over-rebuilding is acceptable per the time/reliability trade), revisit if it rebuilds too eagerly.
2. **How aggressive should reclaim be?** `docker rm -f` by name-family is targeted; should it also prune by a `pgweb`/`bench` **label** to catch renamed projects, and could that ever clobber a developer's unrelated container? Lean: match the documented name families + a dedicated label we set on our own containers; never a blanket prune.
3. **Lock timeout vs PID-liveness only.** Is PID `kill -0` liveness enough, or also a max-age (e.g. reclaim any lock older than the longest plausible run)? PID liveness handles `kill -9`; an age cap handles a PID-reuse edge. Lean: both (liveness primary, age cap as backstop).
4. **Should `./bench/run.sh` also run the unconditional reclaim, given it can run standalone and concurrently-ish with a dev PG?** Lean: yes — same reclaim, same guarantee; bench is an entrypoint too.
5. **CI vs local divergence.** Should the unconditional reclaim be softened in CI (fresh runners don't need it and `docker rm` of nothing is just noise)? Lean: keep it on everywhere (idempotent + cheap on a clean box); the consistency is worth more than the saved second.

---

*The standard is simple and the owner stated it: type `./scripts/test-all.sh` or `./bench/run.sh`, and it works — first run, tenth run, after an edit, after a crash, with no remembered flag and no manual cleanup. Buy that with cleanup and rebuilds, not with skips. Then prove it with the adversarial matrix and paste the green `PGWEB-RESULT` lines. After this, "it's just flaky" stops being an acceptable sentence.*
