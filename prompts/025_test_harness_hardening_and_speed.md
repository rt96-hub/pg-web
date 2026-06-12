# 025 — Test-harness hardening: minimize time, false signals, redundant builds, and triage friction

**Status:** Open handoff prompt — high priority for dev-loop + CI trustworthiness
**Date opened:** 2026-06-12
**Author:** Handoff from the 2026-06-12 harness bring-up session (see `docs/internal/TESTING-SETUP.md`, written the same day)
**Prerequisites:** None hard. Reads better after the prompt-014 worker fix lands (it removes the 13×60s timeout noise from baseline runs). Related: 024 (the one known flaky test).

---

## Summary

The five-tier `scripts/test-all.sh` design is right: one command, hard gates, no silent skips, Docker tiers mandatory. But a full day of running it on a fresh-ish machine (three complete runs, timestamped on the third) exposed a set of time sinks, false signals, and triage dead-ends that are all fixable without weakening any gate. This prompt is the work order to harden it.

Measured baseline (2026-06-12, M-series MacBook, warm caches, WIP tree whose worker crash-looped — see TESTING-SETUP.md for the run table):

| Phase | Cost | Of which avoidable |
|---|---|---|
| Tier 1 (89 pg_tests) | 13 s | — |
| Tier 2a (HTTP smoke) | ~16 s | — |
| Tier 2b (150 CLI tests) | <1 s | — |
| Tier 3 image rebuild | 2 s cached / ~9 min after any layer miss / ~56 min cold | mostly (see #2, #3) |
| Tier 3 (13 E2E tests) | 794 s vs broken worker; ~3–4 min when passing | ~12 min of it (see #4) |
| Tier 4 (19 smoke sections) | 7 s | integrity hole (see #1) |

A warm all-green run should be ~5 minutes. The failure modes below cost us hours.

## The findings (each observed, not hypothesized)

### 1. Tier 4 validates the wrong artifact (CRITICAL — integrity)

`pg-web up` does an unconditional `docker compose pull` (`crates/pg_web_cli/src/stack.rs:73-87`). In every run today, tier 4 pulled the **published Docker Hub image** over the image the suite had built minutes earlier (run 3's timestamped log: built `13:37:00`, re-tagged from Hub `13:50:15`), then passed all 19 sections against that stale artifact while tier 3 was failing 13/13 against the real one. Same run, two different images, one green checkmark.

Side effect: the local tag is now older than the sources again, so **every subsequent run re-triggers an image rebuild** (the staleness check compares mtimes vs image Created time) — a permanent rebuild loop.

Fix directions (pick + combine):
- `stack.rs`: pull only when the image is missing locally (preserves fresh-machine UX; one-line guard). This is a product-behavior change — confirm with maintainer.
- `smoke-cli.sh`: postcondition assert — the container's running image ID equals the ID `ensure_image_fresh` decided on; hard-fail with a clear message if not.
- Longer term: tier 4 should drive the scaffold with an explicit image pin (e.g. `PGWEB_IMAGE` env respected by the scaffolded compose template) instead of whatever `latest` resolves to.

### 2. mtime-based image staleness check causes false rebuilds

`ensure_image_fresh` compares source file mtimes against the image's `Created` timestamp. Anything that touches mtimes (branch switch, `git stash` pop, checkout) or re-tags the image (finding #1) forces a full rebuild with zero content change. Replace with content addressing: hash the build inputs (`git hash-object` over the watched set, or `tar | sha256`) and bake it into the image as a `LABEL pgweb.src_hash=…`; rebuild only when the hash differs. Mtime can stay as a fast-path pre-check.

### 3. True rebuilds are needlessly slow (no cargo cache inside Docker)

The Dockerfile's builder stage recompiles the entire dependency graph on every source change (no BuildKit cache mounts). Today's observed spread: 2 s (all layers cached) → ~9 min (source layer changed) → ~56 min (cold, during a Docker Desktop cache eviction). Add `--mount=type=cache` for `/root/.cargo/registry` + `target/` in the builder stage (BuildKit syntax is already enabled via `# syntax=docker/dockerfile:1.7`), so a source-only change rebuilds in ~1–2 min. Also consider `cargo chef` or a deps-only pre-layer, and registry/`--cache-from` caching for CI.

### 4. A broken worker costs 13 × 60 s of identical timeouts (fail-fast missing)

Every docker_e2e test independently boots a container and burns the full `wait_for_http` 60 s deadline when the BGW can't serve (today: 794 s to learn the same fact 13 times). Two-part fix:
- **Canary preflight:** before the 13 tests (or as test #0 / inside `preflight_or_panic`), boot one container, give it ~30 s; if `/` never answers, fail the whole suite immediately **and print `docker logs` tail** — today the crash-loop reason (`FATAL: role "pgweb_app" is not permitted to log in`) was only discoverable by manually running a probe container.
- **`wait_for_http` panics must carry the container log tail** (last ~20 lines) in the panic message. One-line quality-of-life change in `docker_e2e.rs:65-79` that would have saved an hour of triage today.

### 5. Tier-2a bootstrap should self-heal

Tier 2a hard-depends on a `pg_web_ext` database existing on the pgrx dev instance — a one-time manual `createdb` that cost a full failed tier today (now documented in TESTING-SETUP.md, and hinted in test-all.sh's failure text). Better: `scripts/test-http.sh` should `createdb` it when missing (idempotent, 1 line with `|| true` + existence check). Same spirit: it could also append `shared_preload_libraries` to the data dir if absent and bounce PG, making tier 2a fully self-bootstrapping after `cargo pgrx init`.

### 6. Script exit code can be green while tiers failed (CI false-green hazard)

Tiers 1/2a/3 are soft-failed (deliberately, for dev UX) — today's runs exited `rc=0` with tier 2a and all of tier 3 red. Fine locally; fatal for CI, which must currently grep the log. Add a strict mode: `STRICT=1 scripts/test-all.sh` (default ON when `CI` env var is set) that converts any tier failure into a non-zero final exit, while keeping the keep-going behavior so later tiers still produce signal. Print a one-line per-tier status table at the end either way (`tier1 PASS / tier2a FAIL(app) / …`) — today the statuses are scattered across ~3000 log lines.

### 7. Observability: timestamps + per-tier durations

The 78-minute "mystery run" today was macOS sleep — invisible because nothing in the harness records wall-clock time (and sleep pauses the monotonic clocks tests use, so their self-reported durations look normal). The caffeinate guard now prevents the cause, but the lesson stands: add opt-in per-line timestamps (`TEST_TS=1` → pipe through a tiny awk/perl stamper) and always print per-tier start/end/duration lines + the end-of-run table from #6. Cheap, and makes any future stall self-locating.

### 8. Redundant work worth shaving (lower priority)

- Tier 2b runs the docker_e2e binary just to report "13 ignored"; tier 1 and tier 2a each install a different flavor of the `.so` (test vs runtime — inherent, but the second install is a full `cargo pgrx install` even when nothing changed since the last one).
- Tier ordering: the image build (tier 3 prep) is independent of tiers 1/2a/2b — `ensure_image_fresh` could kick off in the background at script start and be awaited at tier 3 entry, hiding most of a warm rebuild behind tier 1/2.
- `smoke-cli.sh` has two sections numbered "16" (cosmetic).

## What a fixing agent should do

1. Read `docs/internal/TESTING-SETUP.md` (the bring-up record this prompt distills) + `scripts/test-all.sh`, `scripts/test-http.sh`, `scripts/smoke-cli.sh`, `scripts/build-image.sh`, `crates/pg_web_cli/tests/docker_e2e.rs`, `crates/pg_web_cli/src/stack.rs`, `Dockerfile`.
2. Land the integrity fixes first (#1, #6) — they change what a green run *means*. #1's `stack.rs` half needs a maintainer decision (product behavior); the smoke postcondition half does not.
3. Then the time fixes (#2, #3, #4) — target: warm all-green ≤ 5 min; broken-worker run fails in < 90 s with the container log in the output.
4. Then self-healing + observability (#5, #7), then #8 as time allows.
5. Keep every existing hard gate: tier 3/4 mandatory, no silent skips, image is the artifact under test. Nothing here may weaken that.
6. Update `docs/TESTING.md`, `docs/internal/TESTING-SETUP.md`, and CLAUDE.md (the tier-4 caveat line becomes obsolete once #1 lands — remove it then, per the update-the-bibles rule).
7. Acceptance: a full `scripts/test-all.sh` (and a `STRICT=1` run) green on the post-014-fix tree; a deliberately-broken-worker build demonstrating the fast-fail path with logs; before/after wall-clock numbers recorded in TESTING-SETUP.md's verification table.

## References

- `docs/internal/TESTING-SETUP.md` — § Known harness-integrity gotcha (pull clobber, with timestamps), § macOS sleep, § Diagnosing, verification-run table (the measured baseline above).
- `scripts/test-all.sh` — `ensure_image_fresh` (mtime check), soft-fail tier wrappers, caffeinate guard, tier-2a hint text.
- `crates/pg_web_cli/src/stack.rs:73-87` — the unconditional `docker compose pull`.
- `crates/pg_web_cli/tests/docker_e2e.rs:65-79` — `wait_for_http` (no log capture on panic).
- `Dockerfile` — builder stage, no cargo cache mounts.
- Prompt 024 — the one known flaky tier-3 test (don't conflate its fix with this prompt).
- Run logs from 2026-06-12 (`/tmp/pgweb-test-all-run{1,2,3}.log` while they survive): run 3 is fully timestamped and shows the build-then-clobber sequence verbatim.
