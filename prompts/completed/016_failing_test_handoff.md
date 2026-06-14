# Handoff Prompt: Fix Tier 3 E2E Test Breakage from 016 Caching + Graceful Shutdown

**Context and History**
- Pre-016: Full `RUN_BENCH=1 scripts/test-all.sh` was reliably green (all 5 tiers, including tier 3 Docker E2E with 14 tests against freshly rebuilt `rtaylor96/pg-web:latest` + full `examples/todo/` flows; benchmark phase produced clean numbers).
- Post-016 implementation (caching via RouteSnapshot for routes/templates/env with NOTIFY-driven invalidation; always-on listen task for pgweb_reload + livereload; graceful via axum with_graceful_shutdown + pgrx sigterm_received() poll + request_shutdown/Notify for SSE close + 8s drain timeout): Tier 3 consistently red.
- Specific recurring failure: `crates/pg_web_cli/tests/docker_e2e.rs::dev_error_page_surfaces_sql_exception_detail` (sometimes other E2E tests + smoke phase show "Empty reply", "IncompleteMessage", connection errors, or canary timeouts on /).
- In image rebuilds (triggered because sources changed), canary often fails (" / never answered within 30s"); container logs frequently show BGW "terminated by signal 11: Segmentation fault" right after "LISTEN task started (cache invalidation + livereload; always-on)" + "SPI identity is not the expected serving role" warnings. High-concurrency bench legs also degrade to connection errors / 0% success.
- We have iterated: forced direct `settings::current_env()` in render_error (to fix stale env causing prod errors to leak dev details); removed dedicated reload subscriber task (invalidate now direct in listen pump to reduce early tasks); lazy cache build; pgrx-only graceful poll (no extra tokio signal to avoid handler conflicts). Tier 3 still fails. The script exits non-zero on tier 3 (correct behavior); non-strict mode allows continuation but the canonical command is not clean.
- Do **not** paper over by ignoring tests, marking --ignored, using FORCE=1, skipping tiers, or changing test expectations. The breakage must be fixed in the implementation so the pre-016 green state is restored.

**Root Cause Hypothesis (Investigate and Confirm)**
The 016 changes increased early BGW concurrency and signal-related work in `worker.rs` (unconditional `tokio::spawn` of `run_listen_loop` with reload channel + LISTEN; `shutdown_signal` poll task with interval + `sigterm_received()` from t=0; `ListenRouter` with Notify for shutdown; lazy `get_snapshot()` / cache rebuild doing `Spi::connect` + Tera compiles; graceful `with_graceful_shutdown`).
- This interacts badly with pgrx BGW setup (`attach_signal_handlers`, `connect_spi_as_serving_role` for pgweb_app role, current_thread tokio runtime) especially inside Docker containers after `CREATE EXTENSION` + role grants.
- Result: segfaults or unresponsiveness visible in canary/E2E (but not always in pgrx dev or standalone bench). The env cache window (even with bypass) + async invalidation via listen pump can cause the specific dev_error test to see stale "development" behavior or fail to connect right after a push that sets production + boom route.
- Tier 3 is mandatory (image is the shipped artifact; canary + full flows exercise real startup + push + error paths post-rebuild). Benchmark phase also suffers secondary effects.
- Pre-016 the listen was dev-only (skipped in prod images), no extra graceful poll/Notify at startup, no RouteSnapshot — tests were stable.

**Requirements for Success**
- Achieve a **clean green execution of the exact single command `RUN_BENCH=1 scripts/test-all.sh`** (after proper pre-bookend hygiene: pgrx stop, pkill, docker rm for pg-web/bench/smoke/canary; no parallel runs).
- All 5 tiers must pass: Tier 1 (95 pgrx tests), 2a (HTTP smoke), 2b (CLI), **Tier 3 (full Docker E2E, 14 tests, canary must pass, no connection leaks or content leaks in dev_error test)**, 4 (smoke-cli), + benchmark phase (clean oha numbers, no mass connection errors).
- The 016 features must remain functional: cache (0 framework SPI on hot path for routes/templates/env; push reflected via NOTIFY without restart), graceful shutdown (prompt drain, SSE close on sigterm, no SIGKILL escalation).
- Preserve all invariants (one SPI tx per request for user data; no libpq on data path except the required always-on listen side-channel; etc.).
- Do not modify test code, docker_e2e.rs, or disable functions of the tests. Fix the root cause in the extension (worker.rs, listen_router.rs, cache.rs, http.rs, etc.).
- Re-run full command at the end to prove it; capture per-tier summary + bench results. Update BENCHMARKS.md if needed with before/after.
- When fixed, report back clearly: the exact command output summary, what the root cause was, the minimal targeted fix, confirmation of green run, and any benchmark deltas.

**Execution Instructions**
- Start with reproduction: hygiene + `RUN_BENCH=1 scripts/test-all.sh` (or focused `cargo test -p pg-web --test docker_e2e -- --ignored` after image build). Examine canary logs and failing test output on red.
- Use `docker logs` on canary containers for BGW crashes.
- Debug startup: inspect worker.rs listen/graceful/cache spawns, listen_router pump + invalidate, cache build_snapshot, render_error path, signal handling.
- Possible fixes to evaluate (choose minimal that stabilizes without regressing features or adding per-request cost): defer listen + shutdown poll spawns until after listener bind + first successful request or "listening" log; ensure cache invalidation + env rebuild is visible immediately for error paths; reduce early tasks further if needed; add defensive yield or init sequencing; make graceful poll start lazily.
- Run `cargo check --workspace && cargo clippy --workspace -- -D warnings` after edits.
- Do not batch; use the full single command for verification.
- When the full command succeeds with all green (including the dev_error test showing correct generic prod body and no connection failures), stop and report the results + diagnosis.

This must restore the pre-016 test state. The tests breaking is not acceptable — fix the code we changed.
"""

**Now spawning the subagent with this exact handoff prompt.**