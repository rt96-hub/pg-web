# Session 6 — Phase 2 kickoff: auth + RLS bridge + realtime SSE

**Status:** **DRAFT — spec under iteration with the user.** Don't start implementation until the open questions below are closed.
**Theme:** turn pg-web from "anonymous routes serve HTML" into "multi-user app framework." Three concurrent tracks: cookie sessions + login (auth), per-request `pgweb.user_id` GUC + RLS pattern (RLS bridge), and realtime SSE subscriptions on by default (realtime). The three are designed together because they share request-lifecycle plumbing — the worker reads the cookie, validates the session, sets `pgweb.user_id`, runs the handler, and the same auth context gates the SSE subscribe endpoint.

`v0.2.0` is the starting point. Phase 1 invariants stay intact; everything below is additive.

---

## State of the project at Session 6 start

- `v0.2.0` shipped with: push retry on concurrent DDL (L), CLI in image (F.3), content-hash assets + immutable cache (H), 20 MiB asset cap (I cap-raise), `pg_stat_activity` `application_name` tagging on every CLI connection.
- 230 Rust tests + 19-section smoke. All five tiers green; tier-2a now has a port-shadow preflight (#18); tier-3 auto-rebuilds the image when extension source is newer.
- F.2 SSH-tunneled push deferred: takes its place as a Session 6+ candidate when remote infra is real.
- Channel-aware `ListenRouter` shipped in Session 4 G — built explicitly to be Phase-2-reusable. Realtime SSE plugs into the same primitive.

### Invariants (don't revisit)

1. Directory-as-route, filename-as-method (`docs/APP-LAYOUT.md`).
2. Handler contract `(req json) RETURNS <json|text>`.
3. Dispatch via `template_path` nullability.
4. `pgweb.pages__*(json) RETURNS json|text` is the reserved push-managed namespace.
5. Extension ↔ CLI talk only via framework-table upserts.
6. One HTTP request = one SPI transaction.
7. `pgweb.settings` is the runtime-config source of truth.
8. Async only inside the BGW; HTTPS out-of-process via Caddy.
9. **New for Phase 2:** `pgweb.user_id` is the contract for "who is this request" — set via `SET LOCAL pgweb.user_id = <id>` on every authenticated request; NULL/unset = anonymous. RLS policies read it via `current_setting('pgweb.user_id', true)::bigint`.

---

## The three tracks

### Track A — Cookie sessions + login (auth)

**Surface:**

- `pgweb.sessions(id TEXT PRIMARY KEY, user_id BIGINT NOT NULL, created_at TIMESTAMPTZ, expires_at TIMESTAMPTZ, last_seen_at TIMESTAMPTZ)` — installed by the extension.
- SQL helpers (in install SQL):
  - `pgweb.session_create(p_user_id BIGINT, p_ttl INTERVAL DEFAULT '30 days') RETURNS TEXT` — generates a cryptographically random 32-byte ID, hex-encodes it, signs it with the framework's secret, returns the signed cookie value.
  - `pgweb.session_validate(p_cookie TEXT) RETURNS BIGINT` — verifies signature, looks up by ID, checks `expires_at > now()`, updates `last_seen_at`, returns user_id or NULL.
  - `pgweb.session_revoke(p_cookie TEXT) RETURNS BOOLEAN` — deletes the row, idempotent.
  - `pgweb.password_hash(p_plaintext TEXT) RETURNS TEXT` — wraps `crypt(plaintext, gen_salt('bf', 12))`.
  - `pgweb.password_verify(p_plaintext TEXT, p_hash TEXT) RETURNS BOOLEAN` — wraps `crypt(plaintext, hash) = hash`.
- Worker middleware: read `Cookie: pgweb_session=...` → SPI `SELECT pgweb.session_validate($1)` → if non-NULL, `SET LOCAL pgweb.user_id = '<id>'`.
- New `req.session` field (when authenticated): `{ "user_id": 42, "expires_at": "..." }`. NULL when anonymous.
- Framework secret stored in `pgweb.settings.framework_secret` (random on first install via install SQL using `gen_random_uuid()::text`). Rotation is manual: `pg-web env set framework_secret=<new>`. Documented as "rotating revokes all active sessions."
- `pg-web init --template auth` scaffolds:
  - `migrations/0001_users.sql` — `users(id BIGSERIAL PRIMARY KEY, email TEXT UNIQUE NOT NULL, password_hash TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT now())`.
  - `pages/login/index.html` + `post.sql` — login form, POST handler that calls `pgweb.password_verify` + `pgweb.session_create` + sets cookie via `Set-Cookie` header in the response.
  - `pages/logout/post.sql` — calls `pgweb.session_revoke` + clears cookie.
  - `pages/signup/index.html` + `post.sql` — same pattern, creates user row.
  - `pgweb.toml` `[server].auth_template = "default"` flag (cosmetic, signals what scaffold this app started from).

**Open questions (A):**

- **A1 — Cookie attributes:** `HttpOnly`, `Secure`, `SameSite=Lax`. `Secure` would block local-dev HTTP. Default to `Secure` only when env=production?
- **A2 — Password hashing:** bcrypt cost factor 12 (~250ms per hash on a modern CPU). Bumpable via `[server].password_cost` setting?
- **A3 — Email verification flow:** scaffolded in the auth template, or punt? Email verification needs an email-sending mechanism, which needs a job queue (Phase 3). Lean: punt, document as Phase 3 work.
- **A4 — Password reset flow:** same dependency. Lean: punt.
- **A5 — Session expiration model:** hard 30-day TTL (cookie expires, must re-login) vs sliding window (each request resets `expires_at`)? Sliding is friendlier; hard is simpler/clearer auditing. Lean: sliding window with a hard maximum (e.g., absolute expiry 90 days from creation regardless of activity).
- **A6 — Rate-limiting for login:** in scope or punt? Rate-limiting needs request-counting state — could ride on `pgweb.sessions` or a new `pgweb.login_attempts` table. Lean: ship a documented PL/pgSQL recipe; framework-provided rate limit is Phase 4.
- **A7 — Multi-device sessions:** one session per (user_id, device) or many concurrent? Lean: many concurrent. Logout from one device doesn't kill the others.

### Track B — RLS bridge

**Surface:**

- Worker sets `SET LOCAL pgweb.user_id = '<n>'` after session validation, before the handler runs. Both happen inside the same SPI transaction. NULL/unset = anonymous.
- Documented RLS pattern in `docs/APP-DEVELOPER-GUIDE.md`:
  ```sql
  ALTER TABLE todos ENABLE ROW LEVEL SECURITY;
  CREATE POLICY tenant_isolation ON todos
      USING (author_id = current_setting('pgweb.user_id', true)::bigint);
  ```
- Helper: `pgweb.current_user_id() RETURNS BIGINT` — wraps `current_setting('pgweb.user_id', true)::bigint` + handles NULL gracefully (returns NULL not a parse error). For ergonomic use in policies and handler bodies.
- `pg-web check` gains a "users without RLS" warning when it sees a `users`-shaped table (or any table referenced by an authenticated handler) without `ROW LEVEL SECURITY` enabled. Best-effort linting; documented as advisory.

**Open questions (B):**

- **B1 — `SET LOCAL ROLE` vs `SET LOCAL <guc>`:** ROLE-per-user requires `CREATE ROLE` for every user (admin overhead, no scale). Custom GUC + RLS is the standard pattern (Supabase, postgrest). Lean: GUC.
- **B2 — Anonymous policy patterns:** how do users express "anonymous reads OK, authenticated writes only"? Lean: documented `USING (true)` for SELECT + `USING (current_setting('pgweb.user_id', true) IS NOT NULL)` for write policies. No new framework helpers; the SQL is short enough.
- **B3 — Admin / service role:** does the framework own a "bypass RLS" role for admin tasks? `BYPASSRLS` on the connecting role is the standard answer. Lean: document the pattern; user creates their own admin role and uses it via `pg-web env set DATABASE_URL_ADMIN=...` for admin scripts.
- **B4 — `pgweb.user_id` setting persistence:** GUC namespacing — does the GUC name need to be `pgweb.user_id` (custom prefix) or `app.user_id` (more typical for app code)? Lean: `pgweb.user_id` matches the framework's existing namespace (`pgweb.settings`, `pgweb.routes`). Users who want a different name can `SET LOCAL my_app.user_id = current_setting('pgweb.user_id')` in a handler if needed.

### Track C — Realtime SSE — **on by default per user spec**

**Surface:**

- BGW always opens its LISTEN connection on startup. Already happens in dev mode for livereload (Session 4 G); Phase 2 makes this universal. The +1 PG backend slot cost is paid in production too.
- Auto-mounted endpoint: `GET /_pgweb/subscribe/<channel>` returns `text/event-stream`. Validates session (anonymous channels OK; user-scoped channels require matching user_id). Subscribes the connection to the `pg_app_<channel>` LISTEN topic via the channel-aware `ListenRouter`. Fan-out is in-memory; the LISTEN connection is shared.
- SQL helper: `pgweb.notify_app(p_channel TEXT, p_payload JSONB) RETURNS VOID` — wraps `NOTIFY pg_app_<channel>, '<json>'` with a payload-size assertion (PG's NOTIFY caps at 8 kB; we error on overflow with a "use a row-id ping + client refetch" hint).
- Channel naming convention (enforced by `pgweb.notify_app` + the subscribe endpoint):
  - `<topic>.public` — anyone (including anonymous) can subscribe. NOTIFYs go to all subscribers.
  - `<topic>.user.<id>` — only sessions where `pgweb.user_id = <id>` can subscribe. NOTIFYs are scoped to that user's subscribers.
  - Other shapes — refused at subscribe time; documented as future extension point.
- Auto-injected client stub: `<script src="/_pgweb/realtime.js" async></script>` parallels `livereload.js`. Sets up htmx-sse extension config so HTMX `<div hx-ext="sse" sse-connect="/_pgweb/subscribe/<channel>">` Just Works.
- Disable mechanism: `pgweb.toml [server].realtime = false` — skips the LISTEN connection startup, returns 404 for `/_pgweb/subscribe/*` and `/_pgweb/realtime.js`. Default is `true`.

**Open questions (C):**

- **C1 — Channel name format:** the proposed convention (`<topic>.public` / `<topic>.user.<id>`) is one shape. Alternative: `<topic>?user=<id>` (URL-style) or no convention at all + delegate to a SQL `pgweb.can_subscribe(user_id, channel)` policy function. Lean: convention-based for v0.3 simplicity; SQL-policy-based as Phase 4 add-on once channel patterns get richer.
- **C2 — Channel discovery for `pgweb.notify_app`:** validate the channel format at NOTIFY time too, or just at subscribe time? Lean: both — NOTIFY into a wrong-shape channel is a programmer error, fail loud.
- **C3 — NOTIFY payload size:** PG caps at 8 kB. The helper rejects oversize. Document the "row-id ping + client refetch" pattern: NOTIFY only the row id; client subscribes, gets the id, fetches via a normal handler. Lean: hard-error at 8 kB; pattern documented.
- **C4 — Heartbeat / keepalive:** SSE connections need periodic comments (`: keepalive\n\n`) or proxies time them out. How often? Lean: 30s, hardcoded.
- **C5 — Disconnect handling:** when a subscriber's TCP connection drops, the worker needs to clean up the broadcast::Receiver. Tokio's `broadcast::Receiver::Drop` does this; verify the cleanup path doesn't leak channel registrations in the LISTEN router.
- **C6 — Realtime + auth interaction:** the subscribe endpoint validates session via the same middleware as regular routes. If a session expires *during* an open SSE stream, do we drop the stream? Lean: validate at subscribe time only; an active stream survives session expiry. Re-validation at heartbeat would be 1 SPI roundtrip per heartbeat per subscriber — cost adds up. Documented as "long streams may outlive sessions; user code that cares should re-check on each NOTIFY-handled message."

### Cross-cutting — CSRF

- Phase 1 left CSRF off entirely (anonymous, public site). Phase 2 makes it mandatory for non-GET requests when a session is active.
- Double-submit cookie pattern. Worker sets `pgweb_csrf=<32-byte random>` cookie on every response (HttpOnly: false so JS can read it; Secure tracks the session cookie's Secure setting).
- HTMX requests: a tiny auto-injected script reads the cookie and adds `X-CSRF-Token` header to non-GET requests via `htmx.config.includeIndicatorStyles`-like config (actually `hx-headers` on the body).
- Validation: on non-GET handler dispatch, worker checks `req.body.csrf_token == cookie('pgweb_csrf')` (form posts) OR `req.headers['x-csrf-token'] == cookie('pgweb_csrf')` (HTMX). Mismatch → 403.
- GET requests: skipped (CSRF doesn't apply).
- Anonymous (no session) requests: also skipped — there's nothing to forge.

**Open questions (CSRF):**

- **CSRF1 — Where does the auto-inject live:** the realtime.js stub also gets injected. One combined `pgweb.js` covering both, or two separate scripts? Lean: one combined, `<script src="/_pgweb/pgweb.js" async>` covers livereload (dev only), realtime, CSRF.
- **CSRF2 — Disable mechanism:** users running an API-only backend may not want auto-injected CSRF JS. `[server].csrf = false`? Or per-route opt-out? Lean: settings flag, default true.
- **CSRF3 — Form field naming:** for non-HTMX form posts (regular `<form>` submission), users need to render `<input type="hidden" name="csrf_token" value="...">`. Provide a `pgweb.csrf_token()` SQL function callable in handlers, OR a Tera filter? Lean: Tera filter `{{ csrf_token | safe }}` reads from a context value the handler injects.

---

## Component shipping order (to lock at session start)

Suggested order from cheapest-to-validate up:

1. **B (RLS bridge)** — smallest, decoupled. Worker middleware sets `SET LOCAL pgweb.user_id` from a hardcoded test fixture (no auth yet). RLS policies in `examples/todo/` start using it. Locks the GUC contract.
2. **A.session helpers** — install SQL gains the helpers + the `pgweb.sessions` table. Unit tests via `#[pg_test]` cover create/validate/revoke + signature tampering.
3. **A.middleware** — worker reads cookie → calls `pgweb.session_validate` → sets `pgweb.user_id`. Connects A and B.
4. **A.template** — `pg-web init --template auth` scaffolds login/logout/signup. End-to-end test exercises the full flow.
5. **CSRF** — middleware on non-GET. Tests cover happy path + mismatch + GET-skip + anonymous-skip.
6. **C.realtime** — BGW LISTEN-on-by-default + subscribe endpoint + notify_app helper + realtime.js stub. Disable flag works. Tier-3 test sends a NOTIFY, asserts the SSE client sees the event.
7. **F.2 (Session 5 carry-over) — SSH-tunneled push.** Drops in if remote infra is up; otherwise still defers.
8. **N — release artifacts** for `v0.3.0`.

Each followed by a stop-and-check at phase boundaries (lessons from Sessions 4 + 5: corruption-of-state bugs are catastrophic for trust; verify before moving on).

---

## Test plan

| Tier | What gains coverage |
|---|---|
| 1 — `#[pg_test]` | `pgweb.session_create` returns a valid signed cookie; `validate` accepts it and rejects tampered ones; `revoke` is idempotent; `password_hash`/`verify` round-trip; `notify_app` rejects oversize payloads. RLS policy fixtures: data isolation between two `pgweb.user_id` values. |
| 2a — HTTP smoke | New endpoint: `/_pgweb/subscribe/<channel>` returns `text/event-stream` (when realtime enabled). Disable flag returns 404. |
| 2b — CLI | `pg-web init --template auth` scaffolds the expected file tree. Push validates the signup/login handlers. `pg-web check` warns on no-RLS-on-users-table fixtures. |
| 3 — Docker E2E | Full auth flow: signup → login → cookie set → authenticated GET sees `req.session.user_id` → other-user GET sees zero rows from another user's data → logout → cookie cleared → 401. CSRF: HTMX POST without token = 403. Realtime: handler NOTIFYs, SSE client receives. |
| 4 — CLI smoke | Extends `smoke-cli.sh` with the auth-template happy path. |

**Target:** 270+ Rust tests + 21 smoke sections. (v0.2 closed at 230 + 19.)

---

## Risks / known unknowns

- **GUC + SET LOCAL inside `BackgroundWorker::transaction`** — needs to verify that the SET LOCAL persists for the handler's SPI calls (it should — `SET LOCAL` is tx-scoped, and the handler runs in the same tx). Validate early.
- **bcrypt SPI cost.** Each login does a 250ms `crypt()` — that ties up a worker thread. The HTTP request is parked but not blocking the BGW's tokio runtime (SPI is sync; the await of the SPI call already yields). Validate that login latency under concurrent load is acceptable; if not, defer hashing to a Phase 3 job queue.
- **NOTIFY payload size limit (8 kB).** Documented + checked in the helper, but users will hit it eventually. Make sure the error message clearly points at the "row-id + refetch" pattern.
- **Realtime SSE + Caddy** — Caddy needs `flush_interval -1` or similar for SSE to stream cleanly. Document in `docs/DEPLOYMENT.md`. (Current Phase-1 livereload likely already handles this — verify.)
- **Session secret rotation** — rotating `framework_secret` invalidates all active sessions. Document the user-visible impact ("all your users get logged out"); maybe ship a `pg-web rotate-secret` CLI verb that does the rotation + a "drain sessions before rotation" pre-step (revoke everything older than X). Phase 3 stretch.
- **Cookie HttpOnly + JS reading CSRF cookie** — these are different cookies. `pgweb_session` is HttpOnly (JS can't read it; only the browser sends it back). `pgweb_csrf` is NOT HttpOnly (JS reads it for HTMX header injection). The threat model here is well-understood: JS-readable CSRF tokens are fine because reading != forging.

---

## Out of scope for Phase 2

- **OAuth / SSO providers.** Needs HTTP calls to OAuth servers, which the BGW can't make without a job queue. Phase 3 deliverable; auth template stays email+password until then.
- **Email verification + password reset.** Same dependency on email sending → job queue → Phase 3.
- **Per-user rate limiting.** Patterns documented; framework helpers Phase 4.
- **Audit log of authentication events.** Users can write their own; framework adds tooling Phase 4.
- **Channel-policy SQL function (`pgweb.can_subscribe`).** The convention-based approach (`*.user.<id>` / `*.public`) covers v0.3. Phase 4 adds the more flexible policy hook if patterns get richer.
- **Multi-tenant org/workspace primitive.** Userland concern; the RLS-by-`pgweb.user_id` primitive is the building block — orgs / teams sit on top via `org_members` join tables.
- **Anonymous CSRF** — anonymous requests don't need CSRF (no session to forge); skipped by design.

---

## Handoff prompt (FILLED IN at session-end; placeholder until then)

(To be written once the open questions are closed and the implementation starts.)

---

## Decisions log (updated as the discussion progresses)

| Date | Topic | Decision | Rationale |
|---|---|---|---|
| 2026-04-25 | Realtime default | **On by default**, disable via `[server].realtime = false`. | User flag — "we enforce it... maybe with a setting that disables it." |
| 2026-04-25 | Schema-diffing approach | Native-Rust SQL diff against `schema/*.sql`. | Locked separately in ROADMAP commit `552aa04`. Out of Session 6 scope (deferred to a dedicated session post-Phase-2). |

(Add rows here as we close the open questions above.)
