# 023 — Phase 2 core: cookie sessions + RLS bridge + realtime SSE (implement the three tracks from session_6.md)

**Status:** Open work order — the entire next phase after the 013/014 foundations
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from `docs/internal/sessions/session_6.md` + external analysis, 2026-06-11)
**Prerequisites:** 013 (Response Contract v2 — required for `Set-Cookie`, redirects in login, custom content types), 014 (Execution-role hardening — required so RLS policies actually filter rows instead of being bypassed by superuser), 017 uploads (for rich blog/media authoring in the dogfood), and the Phase 2 auth/RLS/realtime spec itself in `docs/internal/sessions/session_6.md`.
**Context:** `docs/internal/sessions/session_6.md` is a detailed but still-DRAFT spec for turning pg-web from "anonymous HTML routes" into a multi-user app framework. It defines three tightly coupled tracks that share the request lifecycle (cookie → session validation → `SET LOCAL pgweb.user_id` → handler → optional realtime fan-out). The spec was written at the end of Session 5 as the handoff, but (as of the 2026-06-11 analysis) no implementation work has begun. Prompts 013 and 014 were created precisely because they are the unacknowledged prerequisites sitting under this document; 020 (Site v2 blog) is the intended flagship dogfood once the core lands.

This prompt turns the session_6 spec into an actionable implementation work order, preserving its open questions, suggested shipping order, invariants, and acceptance criteria.

---

## Summary

Session 6 defines three concurrent tracks to be designed and implemented together:

**Track A — Cookie sessions + login (auth)**
- `pgweb.sessions` table + install-SQL helpers: `session_create`, `session_validate`, `session_revoke`, `password_hash`, `password_verify`.
- Worker middleware: read `Cookie: pgweb_session=...`, call `session_validate`, `SET LOCAL pgweb.user_id = '<id>'` (or leave NULL).
- New `req.session` field when authenticated.
- `framework_secret` in `pgweb.settings` (random on first install; rotation invalidates sessions).
- `pg-web init --template auth` scaffolds users table, login/signup/logout pages + handlers, etc.
- Open questions A1–A7 (cookie attributes, expiry model, rate limiting, email verification, etc.).

**Track B — RLS bridge**
- The same `pgweb.user_id` GUC (set after auth, before the user handler) is the contract that RLS policies read via `current_setting('pgweb.user_id', true)::bigint`.
- `pgweb.current_user_id()` helper.
- `pg-web check` gains a "table without RLS enabled" advisory warning for authenticated handlers.
- Open questions B1–B4 (GUC vs SET ROLE, anonymous policy patterns, admin bypass role, etc.).

**Track C — Realtime SSE subscriptions**
- Reuses the existing channel-aware `ListenRouter` (Session 4 G).
- New framework route `GET /_pgweb/subscribe/<channel>` (text/event-stream).
- SQL helper `pgweb.notify_app(channel, payload)` (payload cap ~8 kB; larger payloads documented as "signal then refetch").
- App-level channels (e.g. `orders.user.42`, `blog.public`).

The three tracks are deliberately coupled: the auth context established for a normal request is the same context that gates subscription and that appears in `pgweb.user_id` for RLS on any tables the realtime payload might reference.

## Why this matters now

- This is the entire "Phase 2 — Auth + realtime + declarative schema" row in the current ROADMAP.
- Without it, pg-web remains an excellent anonymous/internal-tool / single-user framework. With it (plus 013/014), it becomes a credible multi-tenant app platform.
- 020 (the Site v2 authenticated blog) exists specifically to dogfood the full surface in production on pg-web.dev itself. The CLAUDE.md "every feature ships with a companion-app flow" rule cannot be satisfied for Phase 2 until this lands.
- The analysis (prompts 013/014/019) repeatedly calls out that starting the auth/RLS work *before* the response contract and privilege floor would produce a compromised result (no Set-Cookie possible; RLS silently bypassed). Now that those two prompts exist, the implementation of the session_6 tracks can be scheduled cleanly.

## Current behavior (evidence)

- No `pgweb.sessions` table or any of the `session_*` / password helpers exist in the install SQL (`schema.rs`).
- The worker request path (`router.rs` `serve_in_tx`, `call_handler`) never reads cookies or issues any `SET LOCAL pgweb.user_id`.
- `req` JSON passed to handlers has no `session` key.
- No `/_pgweb/subscribe/*` routes are mounted (only the livereload and static framework routes exist in `http.rs`).
- `pg-web init --template` only offers the `todo` template; there is no `auth` scaffold.
- `pg-web check` has no RLS-related warnings.
- All references to the above appear only in planning documents (`session_6.md`, ROADMAP Phase 2 section marked entirely ⬜, the new 013/014/020 prompts that treat it as future work, CLAUDE.md "planning in session_6.md").

## Proposed direction (options)

Follow the component shipping order suggested in `session_6.md:124-140` (with the prerequisite work from 013/014 assumed complete):

1. **B (RLS bridge) first** — smallest and most decoupled. Hard-code a test `pgweb.user_id` in the worker, write RLS policies on `examples/todo/`, prove isolation with a `#[pg_test]`. This locks the GUC contract early.
2. **A session helpers + table** — add the table and the five SQL helpers. Unit-test create/validate/revoke, signature tampering, password round-trips.
3. **A middleware + `req.session`** — read the cookie (now possible thanks to 013), call `session_validate`, set the GUC, populate `req.session` when present.
4. **A template + `init --template auth`** — the login/signup/logout flows (exercising 013 redirects + Set-Cookie).
5. **Cross-cutting CSRF** (as defined in session_6).
6. **Track C realtime** — mount the subscribe endpoint, implement `notify_app`, wire a simple public channel in the companion app.

**Lean:** Treat the three tracks as one coordinated body of work (this single prompt) but land them in the B-then-A-then-C order above so the RLS contract is proven before anyone starts writing policies that depend on it. Carry every open question (A1–A7, B1–B4, C-related) forward into the implementation session; do not silently decide them.

## Detailed design notes

- **Invariant #9 (new for Phase 2, from session_6):** `pgweb.user_id` is the official contract for "who is this request." It is set via `SET LOCAL` inside the one SPI transaction. Anonymous = NULL or unset. RLS policies and handler code read it with `current_setting('pgweb.user_id', true)::bigint` (or the `pgweb.current_user_id()` helper).
- The framework secret, session signing, cookie attribute defaults, etc. are all specified in session_6 Track A. 013's cookie helper must be aligned with the attribute decisions (A1).
- Realtime channels reuse the existing `ListenRouter` broadcast fan-out. The new surface is just the HTTP subscribe endpoint + the `notify_app` SQL helper (with the 8 kB payload cap documented).
- `pg-web check` RLS warning is best-effort static analysis (look for authenticated handlers that touch a table that does not have `ROW LEVEL SECURITY` enabled). It is advisory, not a hard error.
- The `pg-web init --template auth` scaffold should produce a minimal but complete working example (users table + login flow) that can later be extended by the 020 blog dogfood.

## Research tasks for the implementing session

1. Read `docs/internal/sessions/session_6.md` in full (all three tracks, all open questions, the component shipping order, the test matrix, the risks section, and the handoff at the end).
2. Read the prerequisite prompts 013 and 014 (and their acceptance criteria) so the cookie/redirect surface and the non-superuser role are designed against.
3. Re-read the `pgweb.user_id` invariant (#9) and the exact GUC vs. SET ROLE discussion (B1). Confirm the chosen approach with the maintainer.
4. Prototype the session validation + `SET LOCAL pgweb.user_id` flow inside a request transaction (can be done with a test fixture before any cookie parsing exists).
5. Prototype a minimal `/_pgweb/subscribe/foo` endpoint using the existing `ListenRouter` so the fan-out behavior can be observed early.
6. Decide how the `framework_secret` is initially generated (install SQL `gen_random_uuid()` is the suggestion) and how rotation is exposed (`pg-web env set` is already the mechanism; document the "all sessions are revoked" consequence).
7. Map every place that will need to change: `schema.rs` (new table + helpers), `worker.rs` / `router.rs` (middleware), `http.rs` (new route mount), `init.rs` (new template), `check.rs` (RLS warning), push reconcile (new reserved handler namespace if any), docs.

## Constraints & invariants to respect

- All Phase 1 invariants (directory-as-route, `(req json) RETURNS json|text`, one request = one SPI tx, extension ↔ CLI only via tables, async only in BGW, PG 15/16/17, HTTPS out-of-process, etc.).
- New Phase 2 invariant #9 (`pgweb.user_id` contract) must be honored by everything that lands.
- Phase discipline: do not pull Phase 3 (jobs) or Phase 4 (dashboard) work into this prompt. Email verification, password reset, and rate-limiting are explicitly punted in the spec (to Phase 3/4).
- Companion-app rule: basic coverage (a protected route, an RLS-isolated table, a simple public realtime channel) must be exercised in `examples/todo/` or a minimal new scaffold so the feature is not "done" until a real app uses it. Full production dogfooding is 020's job.
- The implementation must not assume 013/014 have already landed in a way that would prevent incremental progress (the prompts exist; the session can start once the maintainer confirms the foundations are far enough along).

## Acceptance criteria

1. The `pgweb.sessions` table and the five SQL helpers (`session_create`, `session_validate`, `session_revoke`, `password_hash`, `password_verify`) are installed by the extension and pass `#[pg_test]` coverage for happy path, tampering, expiry, and idempotency.
2. A request carrying a valid session cookie results in `SET LOCAL pgweb.user_id = '<id>'` before the user handler runs, and `req.session` is populated in the JSON passed to the handler.
3. RLS policies written against `current_setting('pgweb.user_id', true)::bigint` (or `pgweb.current_user_id()`) actually filter rows when the request is authenticated as different users (proven with a test that would have leaked everything under the old superuser connection).
4. `pg-web check` emits a useful advisory when it sees a table referenced by an authenticated handler that does not have `ROW LEVEL SECURITY` enabled.
5. A working `pg-web init --template auth` produces a runnable scaffold with login, signup, logout, and at least one protected route that demonstrates `req.session` + RLS.
6. The realtime subscribe endpoint (`/_pgweb/subscribe/<channel>`) and `pgweb.notify_app(...)` helper exist, a simple public channel works end-to-end (browser SSE receives a server-sent notify), and the 8 kB payload cap is enforced/documented.
7. CSRF protection (double-submit or equivalent, per the session_6 cross-cutting section) is active on non-GET authenticated actions in the auth template.
8. All open questions from session_6 (A1–A7, B1–B4, realtime channel naming/payload, etc.) are either resolved with a documented Lean + rationale or explicitly carried forward as "still open" in the final docs.
9. Basic companion coverage exists in `examples/todo/` (or the auth template itself) so that "every feature ships with a companion-app flow."
10. The full test matrix from session_6.md (pgrx tests for helpers + RLS fixtures, tier-2a smoke for the subscribe endpoint, tier-3 for an authenticated flow, CLI smoke for the new template) is green.
11. `docs/APP-DEVELOPER-GUIDE.md`, `ROADMAP.md`, `OVERVIEW.md`, and the auth section of the tutorial are updated.
12. `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and all five mandatory test tiers pass on PG 15/16/17.

## Open questions

All the ones listed in `session_6.md` under Tracks A, B, and C are in scope for the implementing session. The most load-bearing ones (to be closed with the maintainer before or during the work) include:

- A1 (cookie `Secure` default, `HttpOnly`, `SameSite`).
- A5 (sliding vs. hard session expiry).
- B1 (GUC vs. `SET LOCAL ROLE` — the analysis strongly leans GUC + non-superuser role from 014).
- B3 (admin / `BYPASSRLS` role story).
- Realtime channel naming convention and the "signal then refetch for >8 kB" pattern.
- Whether the auth template is "the" framework-provided login or just a starting scaffold that users are expected to replace.

The implementing session should produce a short "decisions" appendix (or update the ROADMAP decision log) for every open question it closes.

---

*Prerequisites 013 and 014 must be solid before this work begins in earnest. Once they are, this prompt is the complete body of work that turns the drafted session_6 spec into shipped, tested, dogfoodable functionality. The 020 blog prompt is the proof that it all works together on a real public site.*
