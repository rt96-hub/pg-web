# 006 — Add Flask / FastAPI-style per-request access logging to `pg-web dev` terminal output

**Status:** Open DX improvement / future session prompt  
**Date opened:** 2026-05-29  
**Discovered in:** Direct user request during real development workflow ("I want to see when endpoints are hit and the request status... like how FastAPI or Flask tell you")

---

## Summary

`pg-web dev` is silent on every successful (and even 4xx) HTTP request. The only runtime visibility today is the raw `[pg]` tail of the Postgres container plus excellent *error* pages + `error!` traces for failures. Developers have no equivalent of:

```
INFO:     127.0.0.1:54321 - "GET /todos HTTP/1.1" 200 OK
```
or the classic Werkzeug line, or even a minimal `GET / 200 3ms`.

The file-watcher side already gives great feedback on *saves* (`⟳ pushed`). The missing half of the inner dev loop is *seeing requests arrive and succeed/fail at the HTTP layer* without having to open browser devtools or `docker compose logs`.

## Current State (research findings)

### Where terminal output comes from in dev mode
- `crates/pg_web_cli/src/dev.rs`:
  - Explicit `println!` / `eprintln!` for watcher lifecycle, pushes, preflight failures (lines ~196, 306, 298).
  - `spawn_logs_tail` (448–470) runs `docker compose logs -f --no-log-prefix postgres` in a thread and blindly prefixes every line with `[pg]`.
- No other mechanism surfaces per-request information to the human running `pg-web dev`.

### Where (and why) requests are invisible
- `crates/pg_web_ext/src/http.rs:120` — the single `handle()` entry point for all traffic.
  - Only one `error!` emission exists: `render_error` (line 201) for `ServeError` cases.
  - `ServeOutcome::Response`, `Asset` (including 304), and fallback 404 paths are completely silent.
- `crates/pg_web_ext/src/router.rs` — `serve_in_tx`, `render_route`, `lookup_*` all know the final status and handler but never log on the happy path.
- `crates/pg_web_ext/src/logging.rs` — default filter is intentionally quiet (`pg_web_ext=info,axum=warn,...`). Even if we added `info!` calls today they would only appear buried under the `[pg]` prefix mixed with checkpoint, autovacuum, and connection messages.
- `pgweb.settings.env = 'development'` is forced by `dev::force_env_development`, but the code never uses that flag to enable access logging.

### Architectural constraints that matter
- **Strict decoupling** (CLAUDE.md): the extension has zero knowledge of "terminal", "CLI", or "dev UX". It may only communicate outward via `pgweb.*` tables or `NOTIFY` channels. The CLI already owns the entire dev loop presentation.
- One request = one SPI transaction (committed only on clean 2xx/4xx).
- The existing `ListenRouter` + `pgweb_livereload` NOTIFY pattern (Session 4 / Component G) proves we can cheaply broadcast dev-only events from the BGW to a CLI-side listener.

### Reference formats (FastAPI/Flask)
- uvicorn (FastAPI `fastapi dev`): `INFO:     127.0.0.1:54321 - "GET / HTTP/1.1" 200 OK`
- Werkzeug (Flask): `127.0.0.1 - - [29/May/2026 12:34:56] "GET / HTTP/1.1" 200 -`

Both give the developer immediate, scannable confirmation that a route was exercised and what the server replied.

## Why This Matters

`pg-web dev` is the flagship developer experience of the entire framework. The promise is "edit SQL + HTML, see it instantly." Seeing only save/push events while requests are a black box breaks that mental model. Every other web framework (FastAPI, Flask, Rails, Express, etc.) gives you this feedback for free in dev mode. We are an outlier in the wrong direction.

Errors already feel first-class (rich page + structured log). Success and normal 404/redirect cases should feel equally visible.

## Proposed Solution Directions (in rough preference order)

### 1. Dedicated dev-only NOTIFY channel + CLI-side pretty printer (recommended)
- In dev mode only, after `ServeOutcome` (or inside `render_*` / asset path) is known, the extension does a lightweight `SELECT pg_notify('pgweb_dev_access', payload)` inside the existing request transaction (or via a tiny SPI call right before commit).
  - Payload example: `{"m":"GET","p":"/todos/42","s":200,"d":2,"h":"pgweb.pages__todos__42_index"}` (keep tiny).
- `pg-web dev` (when `tail_logs` or a new access-log flag is on) spawns a second tiny background thread that does a plain `LISTEN pgweb_dev_access` (using the existing `postgres` crate connection pattern) and prints a clean, non-`[pg]` line directly:
  ```
  ⟳ GET /todos/42          200   2ms
  ⟳ POST /todos            201   4ms
  ⟳ GET /missing           404   1ms
  ```
- Reuses the proven livereload NOTIFY pattern. Zero new tables, zero polling, instant, fully owned by the CLI for formatting and coloring later.
- Easy to add `--no-access-log` later (symmetric with `--no-livereload`).
- The existing Docker log tail remains unchanged; these lines appear at the same indentation level as the `⟳ pushed` messages.

### 2. Structured tracing + smart log-tail rewriting (simpler, slightly leakier)
- Add a single `info!(target: "pg_web_ext::access", ...)` or use `tracing::info_span!` + `tower-http` `TraceLayer` guarded by `env == Env::Development`.
- Teach `spawn_logs_tail` (or a new parallel reader) to recognize lines containing a magic marker or the access target and reprint them cleanly without the `[pg]` prefix (and optionally strip repetitive Postgres noise).
- Lower risk of new moving parts, but couples the presentation quality to Docker log format and makes the CLI do string parsing of its own child's output.

### 3. Ring-buffer table polled by CLI (avoid)
- Write a capped `pgweb.dev_request_log` row per request (unlogged table + trigger or direct insert).
- CLI polls every 150–300 ms.
- Works but feels heavier than NOTIFY for a pure-dev feature and adds write amplification on every request even when no one is watching the dev terminal.

**Strong recommendation:** pursue direction #1. It is the most consistent with how we already solved the "dev needs to hear about changes" problem (livereload) and preserves the extension/CLI firewall.

## Concrete Insertion Points for the Agent

**Emission (extension):**
- `crates/pg_web_ext/src/http.rs:69` (`async fn handle`) — wrap the call to `router::serve` + asset/error rendering with `Instant` timing. After the response is built, decide whether to emit the access event (only when `settings::current_env == Development`).
- `crates/pg_web_ext/src/router.rs:77` and `102` (the 404 fallback path) already surface the final status code to the caller.
- `render_asset` (http.rs:158) returns 200 or 304 — both are interesting for a dev log.
- Consider a tiny helper `log_access(method, path, status, duration, ...)` that does the `pg_notify` (or the tracing call).

**Presentation (CLI):**
- `crates/pg_web_cli/src/dev.rs:137` — the `logs_child` block and `DevOptions`. Add an `access_log: bool` field (default true).
- New function parallel to `spawn_logs_tail` and `notify_livereload`: `spawn_access_log_listener(url, stop)` that opens a `postgres::Client`, issues `LISTEN`, loops on notifications, parses, and `println!`s the formatted line.
- Startup banner (around line 144) should mention the new stream: "✓ access log — requests will appear as ⟳ GET /... 200"

**Config / docs surface:**
- `docs/APP-DEVELOPER-GUIDE.md` — the `pg-web dev` section (currently says it "tails the Postgres container's logs").
- Possibly a one-line mention in `docs/OVERVIEW.md` and the README produced by `pg-web init`.

**Tests:**
- The existing tier-3 Docker E2E harness in `crates/pg_web_cli/tests/docker_e2e.rs` already boots `pg-web dev` against `examples/todo`. Add a lightweight assertion or smoke step that greps the captured stdout for a recognizable access line after a synthetic `reqwest` hit (or just document the manual verification).

## Actionable Next Steps for a Future Session

- [ ] Reproduce the current silence: `cd examples/todo; cargo run --bin pg-web -- dev` (or the installed binary), hit a few routes with curl/browser, observe that only push messages and noisy `[pg]` lines appear.
- [ ] Decide on exact output format (propose 2–3 variants in the PR and pick one with Robert). Keep it short and aligned with the existing `⟳` vocabulary.
- [ ] Implement the NOTIFY + listener approach (or the tracing rewrite approach if we collectively prefer simplicity).
- [ ] Gate everything on `env == Development` so production images and `pg-web up` (without dev) stay completely quiet.
- [ ] Add the flag plumbing (`--no-access-log`) and update help text.
- [ ] Update the dev startup banner and `APP-DEVELOPER-GUIDE.md`.
- [ ] Verify that the new listener shuts down cleanly on Ctrl-C (the existing `stop` AtomicBool pattern).
- [ ] Run the full `scripts/test-all.sh` (especially the Docker E2E tier that exercises `pg-web dev`).
- [ ] Optional polish: truncate very long paths, right-align status, color 2xx/4xx/5xx differently (behind a `supports-color` or simple `TERM` check — keep deps minimal).

**Related files / history**
- `crates/pg_web_cli/src/dev.rs` (entire dev UX surface + log tailer)
- `crates/pg_web_ext/src/http.rs` + `router.rs` (the request path that currently only logs errors)
- `crates/pg_web_ext/src/listen_router.rs` + `livereload.rs` (the NOTIFY/broadcast precedent we should reuse or imitate)
- `crates/pg_web_ext/src/worker.rs:92` (where dev vs prod branching already happens)
- `crates/pg_web_cli/src/stack.rs` + `templates.rs` (the compose file and image that produce the `[pg]` stream)
- `docs/APP-DEVELOPER-GUIDE.md` (the public contract for `pg-web dev`)
- `docs/sessions/session_4.md` (Component G — the livereload NOTIFY design that this should feel like a natural sibling of)
- `CLAUDE.md` (decoupling invariant #3)

**Priority:** High (core dev-loop DX parity).  
**Risk of change:** Low-to-medium. The mechanism is isolated to dev mode; the worst case is "extra NOTIFY spam that we can turn off."

---

*This document is intentionally written as both a design record and a ready-to-use prompt for a future agent session. Feed it (plus the current state of `dev.rs`, `http.rs`, and `listen_router.rs`) to the agent when the work is scheduled.*
