# 002 — Investigate and fix EventSource / SSE connection accumulation and leaks in `pg-web dev` live-reload under rapid navigation and bfcache

**Status:** Open diagnostic / improvement prompt for future session  
**Date opened:** 2026-05-27  
**Discovered in:** Real-world usage of pg-web by the trucking-carriers application (external project)

---

## Summary

The browser live-reload feature shipped in M1.4 (`pg-web dev`, on by default) works by injecting a tiny vanilla JS stub that opens a persistent native `EventSource` (SSE) connection to `/_pgweb/livereload`. The client never closes this connection on navigation. The server-side handler holds the response stream open indefinitely (with keep-alives).

In practice, rapid back/forward navigation, link-driven page changes, or any normal multi-page development workflow causes multiple EventSource connections to accumulate inside a single browser tab. After a handful of roundtrips the tab becomes sluggish or completely unresponsive. The entire symptom disappears when starting the dev loop with `pg-web dev --no-livereload`.

This is a classic long-lived connection + bfcache interaction bug. The feature that was meant to make inner-loop development delightful now forces developers to disable it for any realistic workflow involving page navigation.

## Detailed Description

### Symptoms (observed in trucking-carriers)

- A multi-page pg-web application (several `.sql` + `.html` handler pairs under `pages/`).
- Developer runs `pg-web dev` (default livereload enabled).
- Normal development involves clicking links, using browser back/forward buttons, or otherwise moving between full page renders.
- After 4–10 navigation roundtrips the tab's UI freezes, becomes slow to respond to clicks, or DevTools shows extremely high memory / large number of open network connections.
- Network tab in DevTools reveals many concurrent open requests to `/_pgweb/livereload` (text/event-stream) originating from the same tab, some of them from "previous" page instances.
- CPU in the tab process climbs; the tab may eventually be killed by the browser or require a full restart.
- **Critical confirmation:** Starting the exact same app directory with `pg-web dev --no-livereload` makes the problem vanish completely, even though the developer is still editing files and navigating the same pages. (The CLI simply stops emitting the `NOTIFY pgweb_livereload` after each push.)

### Reproduction steps

1. `cd` into a pg-web project that has at least 2–3 distinct full-page routes (e.g. the trucking-carriers app or a copy of `examples/todo` expanded with extra pages).
2. Run `pg-web dev` (or `cargo run --bin pg-web -- dev` from a checkout).
3. Open the app in a modern browser (Chrome/Edge/Firefox).
4. Rapidly navigate between pages using links in the app **and** the browser's back/forward buttons (this exercises bfcache).
5. Repeat the navigation cycle 5–8 times while watching DevTools → Network (filter for `livereload`) and Performance/Memory panels.
6. Observe the accumulation of open SSE connections and subsequent tab degradation.
7. Stop the dev process, restart with `--no-livereload`, repeat steps 3–5 → tab remains responsive indefinitely.

### Technical background: how the current dev live-reload works

The design (Session 4 / Component G) deliberately chose a very simple, zero-dependency implementation:

1. **CLI side (`pg-web dev`)**: A `notify`-based file watcher (debounced, Blake3 content-hash deduped) watches `pages/` and `public/`. On change it runs the normal `push`, then (if `livereload: true`) calls `notify_livereload()` which does a short-lived `NOTIFY pgweb_livereload, '{"kind":"full"}'` (or `"css"` for pure public CSS changes) over a fresh `libpq` connection. See `crates/pg_web_cli/src/dev.rs` (lines ~311–382, `LivereloadKind`, `livereload_kind`, `notify_livereload`, `DevOptions`, `handle_batch`).

2. **Extension LISTEN task**: Only started when `pgweb.settings.env = 'development'` at BGW startup (forced by the CLI dev command). A dedicated `tokio-postgres` loopback connection issues `LISTEN pgweb_livereload` and forwards every notification into an in-memory `broadcast::Sender` via `ListenRouter::publish`. One Postgres backend slot, shared by all browser tabs. Reconnection with backoff is built in. See:
   - `crates/pg_web_ext/src/worker.rs` (~92–105)
   - `crates/pg_web_ext/src/listen_router.rs` (the entire file: `ListenRouter`, `run_listen_loop`, `BROADCAST_BUFFER`, `preregister`, subscribe/publish)

3. **SSE endpoint + client stub**: 
   - `GET /_pgweb/livereload` returns an Axum `Sse` stream backed by a `BroadcastStream` of the channel. 30-second keep-alives. Env-gated (404s in production). See `crates/pg_web_ext/src/livereload.rs` (`serve_livereload_sse`, `build_reload_stream`).
   - `GET /_pgweb/livereload.js` serves a ~25-line IIFE that does `new EventSource('/_pgweb/livereload')`, listens for `reload` events, does CSS cache-busting or `location.reload()`, and has a minimal error handler that relies on browser auto-reconnect. **No `es.close()` anywhere.** See the `LIVERELOAD_JS` const (lines 64–88).
   - Injection: On every `ServeOutcome::Response` that looks like a full HTML document (contains `</body>`) and env=development, `inject_script_if_eligible` splices the `<script src="/_pgweb/livereload.js" async data-pgweb-livereload>` tag right before `</body>`. HTMX fragments are deliberately untouched. See `livereload.rs` (~164–186) and `http.rs` (~123–128).

4. **Route wiring**: The two reserved `/_pgweb/*` endpoints are mounted above the user fallback in `http::app()`. See `crates/pg_web_ext/src/http.rs`.

The JS is intentionally framework-free and tiny so it can be embedded anywhere. Reconnection and "costs nothing when quiet" were accepted trade-offs at the time.

### Why this is problematic

- **Persistent connections by design + no cleanup on navigation.** The EventSource is created once per full page load and lives until the page is fully torn down by the browser. Nothing in the injected script (or the surrounding page) ever calls `close()`.

- **bfcache interaction (the real killer).** Modern browsers preserve the JS heap, DOM, and even some network state of pages when you navigate away (the "back/forward cache"). A page that opened an `EventSource` can remain "frozen" with that connection still logically active (or in a reconnect loop). When you press Back, the page is restored from bfcache (`pageshow` with `persisted === true`) — often without a fresh script execution that would have given the old `es` variable a chance to be cleaned. The result: the restored page may create a *second* EventSource while the bfcached one still holds resources.

- **Server side offers no help.** `serve_livereload_sse` returns an infinite stream. There is no per-connection idle timeout, max lifetime, or rate limiting. The `ListenRouter` only tracks broadcast senders/receivers; it has no knowledge of HTTP-level SSE clients or their origin. A single misbehaving tab can therefore hold dozens of live subscriptions.

- **Rapid navigation amplifies it.** Every full navigation (link click that produces a new HTML document, back/forward, even some form submissions that do full loads) executes the injected script again → another `new EventSource`.

- The "EventSource will just stay quiet" claim in the docs (for `--no-livereload`) is only partially true on the event-delivery side; the TCP connection and browser-side object are still created and held.

### Impact

`pg-web dev` is the primary developer experience surface of the entire framework ("Zero-Proxy", "edit .sql + .html and see it instantly"). When the live-reload feature forces developers to disable it for any app with more than one page, the feature has crossed from "nice-to-have" into "actively harmful to the intended workflow." It makes the tool feel unreliable and pushes users toward manual refresh or competing stacks during the exact phase (rapid iteration) where pg-web should shine brightest.

The bug was invisible in the existing test suite because the livereload E2E test (`docker_e2e.rs`) and smoke scripts only ever open a single connection from a headless client and never simulate browser navigation or bfcache.

## Proposed directions for investigation / fix (in rough priority order)

1. **Client-side defensive cleanup (highest-ROI, lowest-risk first step)**  
   Rewrite the IIFE in `LIVERELOAD_JS` to:
   - Keep a single canonical reference (`window.__pgwebLivereload` or similar sentinel) so the script is idempotent even if injected twice.
   - On `pagehide` (and `beforeunload` as a fallback): `es.close()` and delete the sentinel.
   - On `visibilitychange` (when `document.hidden`): optionally close or at least pause.
   - On `pageshow` with `event.persisted === true` (bfcache restore): decide whether to re-create the EventSource or leave it (the safest is usually to close any lingering one and let a fresh one be created if the script runs again).
   - Remove listeners on close. This alone will eliminate the majority of accumulation.

2. **Server-side lifetime / idle controls on the dev-only SSE endpoint**  
   Wrap the stream returned by `serve_livereload_sse` (or inside `build_reload_stream`) with a timeout. Reasonable dev-only policies: close after 10–15 minutes of wall time, or after N minutes with no events and no activity on the receiver. Axum `TimeoutLayer` or a custom `Stream` combinator both work. Because this endpoint 404s in production, the change is completely safe for deployed apps. Log (at debug level) when a connection is forcibly closed.

3. **Make the transport itself more ephemeral or cheaper**  
   - Short-poll a lightweight "last-reload-timestamp" endpoint every 800–1500 ms instead of a persistent SSE. The poll response can carry the same `{"kind":"full"}` payload. Polling requests naturally die; no bfcache-held sockets.
   - Or keep SSE but make the client close the connection immediately after receiving any `reload` event that will cause a `location.reload()` (the connection is about to be destroyed anyway).
   - Long-poll "wait for next change" style: the client opens a request that blocks until the next NOTIFY, then immediately re-issues. Still cheaper than "always open."
   - Any of these can reuse the existing `ListenRouter` + NOTIFY path.

4. **Connection accounting + diagnostics (helps the investigation and future users)**  
   - Add (dev-only) counters or a small map of "active SSE connections" keyed by something cheap (e.g. connection start time + rough origin). Expose via a new `GET /_pgweb/livereload/debug` JSON endpoint when env=development.
   - Enhance the JS error handler to `console.debug` when connections are created/closed (gated behind a `?debug` query or localStorage flag).
   - In `ListenRouter`, optionally track subscriber count per channel with a simple gauge that the LISTEN task can log periodically in dev.

5. **Smarter scope for injection and/or the feature**  
   - Only inject the livereload script for responses that are the result of a top-level navigation (harder to detect reliably from inside the extension, but possible via custom header the CLI dev server could set on its own requests, or by looking at `Sec-Fetch-Dest` etc.).
   - Document clearly (and surface in `pg-web dev` startup banner) that `--no-livereload` is the recommended setting for apps with heavy client-side state or frequent full-page navigation during development.
   - Consider an additional escape hatch: a `pgweb.toml` key under `[dev]` or a magic comment in the HTML that disables the auto-injection for that page only.

6. **Test coverage for the failure mode**  
   - Add a (probably ignored/tier-3) test that opens multiple simulated "pages" (or at least multiple SSE clients) and asserts that after simulated navigation the number of live broadcast receivers does not grow without bound.
   - At minimum, add a regression test in `livereload.rs` that the generated JS contains the strings for the new cleanup listeners (so future edits don't accidentally remove them).

## Current Workaround (as used in trucking-carriers)

The team keeps rich development velocity by running:

```bash
pg-web dev --no-livereload
```

They perform edits, then manually hard-refresh the current page (or the pages they care about). When they are doing long stretches of work on a *single* view (e.g. perfecting a complex form or dashboard), they occasionally restart without the flag to get auto-reload for that view.

They have also added the flag to their local dev scripts and CI-adjacent docs so new contributors do not hit the unresponsive-tab problem on day one.

This works but directly contradicts the "edit → instant feedback in the browser with zero manual steps" value proposition that justified adding browser live-reload in the first place.

## Actionable Next Steps for a Future Session

- [ ] Reproduce the exact symptoms locally using a multi-page example (expand `examples/todo` or use a minimal synthetic app) + Chrome DevTools + rapid back/forward.
- [ ] Audit every code path that can create an EventSource or hold an SSE response (the four files listed below plus any future realtime SSE endpoints).
- [ ] Implement solution #1 (client-side `pagehide` / bfcache cleanup + sentinel) and verify that rapid navigation no longer accumulates connections.
- [ ] Decide on and implement at least one server-side safeguard (#2) so even imperfect clients cannot DoS the dev server with stuck connections.
- [ ] Add the diagnostic endpoint (#4) and a console-debug path in the JS for future troubleshooting.
- [ ] Update `docs/APP-DEVELOPER-GUIDE.md` (live-reload section) with a "Known limitations & bfcache" subsection and revise the "costs nothing" language.
- [ ] Update the startup banner in `dev.rs` and the `--help` text for the flag to set correct expectations.
- [ ] Extend the livereload portion of `scripts/smoke-cli.sh` and/or add a new smoke step that at least greps the served JS for `pagehide` / `close(` to prevent regression.
- [ ] Consider whether Phase-2 realtime subscriptions (`/_pgweb/subscribe/...`) will need the same hygiene from day one (they probably will) and design the shared client stub (`pgweb.js`?) accordingly.
- [ ] Update `docs/ROADMAP.md` and/or `docs/DEVELOPER-GUIDE.md` if any new "dev server connection management" principles are adopted.

**Related files / history**

- `crates/pg_web_ext/src/livereload.rs` (the entire live-reload implementation, especially the JS constant and `serve_livereload_sse`)
- `crates/pg_web_ext/src/http.rs` (route mounting + call to `inject_script_if_eligible`)
- `crates/pg_web_ext/src/listen_router.rs` (the broadcast fan-out used by SSE)
- `crates/pg_web_ext/src/worker.rs` (conditional spawning of the LISTEN task)
- `crates/pg_web_cli/src/dev.rs` (watcher, post-push NOTIFY, `--no-livereload` flag handling)
- `crates/pg_web_cli/src/main.rs` (CLI flag definition)
- `crates/pg_web_cli/tests/docker_e2e.rs` (the existing `livereload_sse_chain_end_to_end` test — single connection only)
- `docs/APP-DEVELOPER-GUIDE.md` (the published explanation of the feature)
- `docs/sessions/session_4.md` (original design decisions and trade-offs for Component G)
- `docs/ROADMAP.md`, `docs/OVERVIEW.md`, `docs/DEVELOPER-GUIDE.md` (mentions of livereload)
- `scripts/smoke-cli.sh` (smoke assertions for the feature)

**Priority:** High (core developer-experience promise of the framework is compromised for everyday multi-page work).  
**Risk of change:** Low for pure client-side cleanup and documentation; medium if a transport change is chosen (still contained to dev mode).

---

*This document is intentionally written as both a design record and a ready-to-use prompt for a future agent session. Feed it (plus the current state of the livereload, listen_router, dev, and http modules) to the agent when the work is scheduled.*
