# 024 — Fix flaky `dev_watcher_repushes_on_save` Tier 3 E2E test

**Status:** Open handoff prompt — high priority for dev-loop reliability  
**Date opened:** 2026-06-12  
**Author:** Handoff from implementation of prompt 013 (Response Contract v2)  
**Prerequisites:** None (this is a test reliability / environment issue)

---

## Summary

The `dev_watcher_repushes_on_save` test in the Docker E2E suite consistently times out. It:

1. Boots a fresh `rtaylor96/pg-web:latest` container (via testcontainers).
2. Copies `examples/todo/` into a tempdir.
3. Runs `migrate apply` + initial `push`.
4. Spawns `pg_web_cli::dev::watch(...)` in a background thread (with `livereload=true`).
5. Sleeps 250 ms (to let the debouncer hooks install).
6. Edits `pages/index.html` in the tempdir, replacing the empty-state text with a unique marker.
7. Polls `GET /` every 200 ms (up to the deadline) waiting for the rendered body to contain the marker.
8. On timeout it panics with the last observed body (which is always the pre-edit version plus the livereload script).

The test has been the **only consistent failure** across many full Tier 3 runs (12/13 passing, the watcher test being the sole outlier) after the response contract v2 work landed. All other E2E tests that copy the todo tree and push (including `full_todo_crud_flow`, reconcile tests, asset tests, error-page tests, concurrent push tests, in-image push, etc.) pass reliably once the image is freshly built.

This test is **not** a regression test for the new response contract functionality. It only edits a `.html` template (a legacy full-mode route). The new raw-text routes we added for 013 companion coverage (`pages/seeother/index.sql` using `pgweb.redirect` and `pages/status/index.sql` using `pgweb.json`) are present in the copied tree, so the *initial* push in this test (and all other copy_tree tests) now exercises the new envelope path — and that initial push succeeds.

## Why this matters

- Tier 3 Docker E2E is **mandatory** (CLAUDE.md, `scripts/test-all.sh`, `docker_e2e.rs` preflight). Silent skips or ignored tests are explicitly forbidden.
- `pg-web dev` (the file-watcher hot-reload loop) is a core M1.2 deliverable and a primary user-facing dev experience.
- The test is the only automated coverage that the "edit template on disk → watcher detects → push re-runs → Tera re-renders → HTTP serves the new content" loop actually works end-to-end against a real containerized stack.
- The dev watcher is intentionally exercised with `livereload=true` in the test so the same code path used in production `pg-web dev` is covered.

## Reproduction (exact steps the test performs)

1. `preflight_or_panic()` — requires Docker + the image (`rtaylor96/pg-web:latest` as of the 013 session).
2. Start container with exposed 5432/8080, wait for "database system is ready to accept connections".
3. `copy_tree(&todo_app_dir(), tmp.path())` — `todo_app_dir()` resolves to the checked-in `examples/todo`.
4. `migrate::apply` + `push::push` into the test DB.
5. Assert baseline body contains "No todos yet".
6. `std::thread::spawn(|| pg_web_cli::dev::watch(&watch_dir, &db_url, stop, true))`.
7. `sleep(250ms)`.
8. Overwrite `pages/index.html` with the marker text.
9. `loop { GET / ; if body.contains(MARKER) { break } ; if deadline { panic!(last body) } ; sleep(200ms) }`.
10. Signal stop and join the watcher thread.

The panic always shows the original empty-state HTML (plus `<script src="/_pgweb/livereload.js" ...>` because the container is started in development mode).

## Files and functionality involved

**Primary test:**
- `crates/pg_web_cli/tests/docker_e2e.rs`
  - `dev_watcher_repushes_on_save` (the failing test)
  - `todo_app_dir()`, `copy_tree()` (used by almost every Tier 3 test)
  - `preflight_or_panic()`, `wait_for_http()`, `get()`
  - IMAGE/TAG constants (updated during 013 to `rtaylor96/pg-web:latest`)

**Core functionality under test:**
- `crates/pg_web_cli/src/dev.rs`
  - `dev()` entry point
  - `watch(app_dir, url, stop, livereload)` — public so tests can drive it directly
  - `event_loop` + `handle_batch`
  - `classify(path, app_dir)` — the heart of event filtering (dotfiles, editor turds, only `.sql`/`.html` under `pages/`, anything under `public/`)
  - Blake3 content-hash dedup
  - 200 ms `DEBOUNCE_WINDOW` via `notify_debouncer_full`
  - Pre-flight of changed SQL handlers (`preflight_sql`)
  - Call to `push::push`
  - Optional livereload NOTIFY after push
- `notify_debouncer_full` + `notify` crate (native OS watcher → debounced events)
- Interaction with `push`, `migrate`, and the running extension (Tera render of the edited template must produce a visible change over HTTP)

**Related test helpers / patterns:**
- Almost all other `#[ignore]` Tier 3 tests also do `copy_tree(&todo_app_dir(), tmp)` + initial push. Adding the two 013 companion routes (`seeother/`, `status/`) therefore affects the "shape" of every one of these tests.
- `pg_web_cli::dev::watch` is also used by the real `pg-web dev` command (with log tailing, Ctrl-C handling, etc.).

**Supporting docs / invariants:**
- CLAUDE.md: "Tier 3 Docker E2E is mandatory (hard fail if Docker/image missing) — no silent skips."
- `scripts/test-all.sh`: runs the ignored docker_e2e tests and treats them as required.
- `docs/ROADMAP.md` and decision log entries around the dev loop and notify-debouncer-full architecture (2026-04-20).
- The watcher is explicitly modeled on Vite/Next/chokidar (native watcher → write-finish debounce → content-hash dedupe → push).

## Details gathered during prompt 013 implementation

- The test was already timing-sensitive before 013 (original deadline = 10 s).
- During the 013 session we added two new raw `.sql` routes under `examples/todo/pages/`. Every E2E test that copies the tree now pushes two additional handlers that use the new response contract helpers. Initial pushes in the watcher test (and others) now succeed (previously they failed with "function pgweb.redirect does not exist" until a clean image build).
- The failure is **not** caused by the new response contract code paths themselves — the test only mutates `index.html`.
- Increasing the deadline (first to 60 s, then 120 s) did not make the marker appear. The body observed on timeout was always the pre-edit page.
- Fresh `--no-cache` builds of `rtaylor96/pg-web:latest`, explicit removal of leftover containers (`jovial_diffie`, `charming_hypatia`, etc.), and alignment of IMAGE/TAG constants all helped other tests but did not fix this one.
- The watcher process runs on the **host** (the test binary), watching a **host tempdir**. The Postgres it pushes to lives in a testcontainers-managed Linux container. FS event delivery therefore crosses the Docker Desktop VM boundary on macOS — a well-known source of flakiness for `notify`-based watchers.
- The test does a 250 ms sleep after spawning the watcher (to exceed the 200 ms debounce) before performing the edit. This heuristic sometimes isn't enough when event delivery is delayed.
- No evidence of hash dedup, preflight, or push transaction failures in the captured output — the watcher thread simply never produces a successful "⟳ pushed" that results in a visible template change within the deadline.

## Hypotheses (in rough order of likelihood)

1. **Docker Desktop macOS FS notify unreliability** (strongest suspect). Events for the tempdir bind-mount are delayed, coalesced, or dropped, so the debouncer never fires (or fires too late) for the HTML edit.
2. The 250 ms "let the watcher install hooks" sleep is insufficient under load or when many files are present in the tree (the two new 013 routes add a tiny amount of work on every push).
3. The HTTP client poll (5 s timeout on the reqwest client, 200 ms sleep) combined with container networking latency means the updated template isn't visible even if the push succeeded inside the deadline.
4. Interaction between the new livereload NOTIFY (fired on every push) and the test's direct HTTP GETs (no browser involved).
5. (Less likely) A change in push behavior or template serving introduced by 013 that only manifests under the exact timing of this test. (Counter-evidence: all other copy+push tests pass, and the initial push inside this test succeeds.)

## What a fixing agent should do

- Make the test reliably pass (without ignoring it) while still meaningfully exercising the dev watcher + template re-render loop.
- Prefer making the test more robust over "just increase the timeout further."
- Possible approaches:
  - Poll for observable side-effects of a successful push (e.g., check the container logs for the "⟳ pushed" line, or query `pgweb.routes` / `pgweb.templates` directly) instead of (or in addition to) the rendered HTML body.
  - Use a more reliable change-detection mechanism inside the test (e.g., watch the push summary returned by the test's own call, or add a test-only hook).
  - Reduce the amount of work the initial push does, or prime the hashes in the watcher so the first real edit is the only change.
  - Make the test tolerant of delayed events (longer but bounded deadline + better diagnostics on timeout — print watcher thread logs, last push summary, container logs around the edit time, etc.).
  - Investigate / work around Docker Desktop notify issues (different mounting strategy for the tempdir, use a named volume, run the watcher inside the container for this test, etc.).
  - Add unit-level coverage for the hot paths in `classify` + `handle_batch` + Blake3 dedup so the E2E test can be lighter.
- Keep Tier 3 mandatory and the test meaningful for the interactive dev loop.
- Update CLAUDE.md / the test file comments if the robustness strategy changes any invariants.
- After a fix, re-run the full `scripts/test-all.sh` (or at least Tier 3) and confirm the test is no longer the sole outlier.

## References

- Test: `crates/pg_web_cli/tests/docker_e2e.rs:642` (`dev_watcher_repushes_on_save`)
- Watcher implementation: `crates/pg_web_cli/src/dev.rs` (especially `watch`, `event_loop`, `handle_batch`, `classify`, `DEBOUNCE_WINDOW`)
- `copy_tree` and `todo_app_dir` helpers used across Tier 3.
- `scripts/test-all.sh` (Tier 3 execution + `ensure_image_fresh`)
- CLAUDE.md (Tier 3 mandatory, companion-app coverage, dev loop history)
- `docs/ROADMAP.md` (dev loop decisions, notify-debouncer-full architecture)
- Prompt 013 session notes (image name change to `rtaylor96/pg-web:latest`, addition of `seeother`/`status` routes to `examples/todo`, attempts at timeout increases, container cleanup, no-cache builds)

This prompt should contain enough context that a fresh agent can reproduce the failure, understand why it is important, see what was already tried, and design a robust fix without having to reconstruct the history from git blame or scattered session logs.