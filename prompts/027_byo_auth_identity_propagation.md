# 021 — BYO auth: identity propagation, not an identity provider

**Status:** Open work order — supersedes/overlays parts of the session_6 Phase 2 auth draft; encodes a maintainer scope decision
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis + maintainer design discussion, 2026-06-11)
**Prerequisites:** 013 (response/request contract — `req.headers`/cookies in, `Set-Cookie` out) and 014 (non-superuser execution role — **hard prerequisite**: RLS does not enforce against superusers). Option A below is the one mode that can ship with only partial 013. Consumed by 020 (site-v2 blog is the dogfood). Relates to `prompts/005` (bearer tokens are the natural credential for JSON/MCP clients).
**Context:** The maintainer has decided pg-web will **not** ship a first-party identity provider — no password storage with reset flows, no email verification, no 2FA. Users bring their own auth provider (Auth0, Clerk, Keycloak, Firebase, Supabase Auth, corporate OIDC, oauth2-proxy, Tailscale…). The framework's job shrinks to **identity propagation**: accept a credential on each request, verify it, derive a stable user identifier, expose it to SQL inside the request transaction, and let Row-Level Security do the enforcement.

---

## Summary

Define and implement pg-web's auth architecture as a small, identity-source-agnostic core primitive — per-request `SET LOCAL pgweb.user_id` + a `pgweb.current_user_id()` helper + documented RLS recipes — fed by pluggable **credential sources**: (A) trusted reverse-proxy headers, (B) JWT verification in the worker, and (C) framework-managed session cookies whose *login* step is app-defined. Ship A and B first (they are BYO-provider by definition and never grow a password surface); generalize the session_6 draft into C as an optional convenience/revocation layer. Demote session_6's password helpers from framework feature to documented recipe. This prompt also corrects the session_6 draft in three places (user-id type, the ROADMAP/session_6 ROLE-vs-GUC contradiction, and the implicit assumption that pg-web is the IdP).

## The maintainer's design mandate (scope decision — treat as settled)

1. **pg-web is not an identity provider.** No framework-owned signup/password-reset/email/2FA flows, ever. This mirrors the existing philosophy in `docs/VISION.md:46-52` ("Not a TLS termination layer — Caddy handles TLS in front"; "Not an ORM"). Add the corresponding line to VISION's non-goals: **"Not an identity provider. Bring your own (OIDC, JWT, proxy auth); pg-web propagates identity into the request transaction and enforces with RLS."**
2. **The proven target shape is:** credential → verified `user_id` string → `SET LOCAL` inside the request's SPI transaction → RLS policies on app tables key off `current_setting('pgweb.user_id', true)`. This is the PostgREST/Supabase-converged pattern; do not invent a new one.
3. **No enforced users table.** The framework needs only the identity *string*. Whether an app keeps a `users` table (profile data, FKs) is the app's business — document the JIT-provisioning recipe (below), don't impose schema.

## Background: how RLS identity actually works (the mental model to encode in docs)

This section exists because the question "does RLS create a Postgres user per app user?" will be asked by every newcomer. Write the answer into `docs/APP-DEVELOPER-GUIDE.md` as part of this work.

- RLS is a per-table filter: `CREATE POLICY … USING (expr)` hides rows where `expr` is false (and `WITH CHECK` gates writes). The expression may reference `current_user` (the Postgres role) **or any function/GUC** — that choice defines the two patterns:
  - **Role-per-user** (one Postgres role per app user, policies check `current_user`): rejected. Roles are cluster-level objects; signup would mean `CREATE ROLE`; thousands of roles are unmanageable; pooling breaks. Nobody does this for web apps.
  - **One shared low-privilege role + a transaction-local variable** (the standard): exactly one Postgres role executes all handler SQL. Before the handler runs, middleware does `SET LOCAL pgweb.user_id = '<id>'`; policies read it back with `current_setting('pgweb.user_id', true)`. **RLS creates no users.** App users live in ordinary tables (if at all).
- `current_setting(key, true)` returns NULL when unset, and NULL comparisons are false — so anonymous requests (no `SET LOCAL`) see only rows whose policy passes without an identity (e.g. `USING (published OR author_sub = current_setting(…))`). Anonymous-by-default falls out of the mechanism.
- **`SET LOCAL`, never `SET`.** The worker is one long-lived SPI session serving requests sequentially (`worker.rs`); a session-scoped `SET` would leak user A's identity into user B's request. Transaction-locality is the isolation boundary, and it composes exactly with CLAUDE.md invariant #4 (one request = one SPI transaction): identity evaporates on commit/rollback with zero cleanup code.
- **Superusers bypass RLS entirely; table owners bypass unless `FORCE ROW LEVEL SECURITY`.** Today the worker connects with a NULL username — bootstrap superuser (`crates/pg_web_ext/src/worker.rs:56`) — so any policy written today is a silent no-op. This is why 014 (execution-role hardening) gates this entire prompt. All RLS recipes must include both `ENABLE` and `FORCE ROW LEVEL SECURITY`.
- **How Supabase actually does it** (for the docs comparison): Supabase = PostgREST + GoTrue (their auth server). PostgREST verifies the JWT, does `SET LOCAL ROLE authenticated` (or `anon`) for coarse grants, stuffs claims into a `request.jwt.claims` GUC, and their `auth.uid()` helper is essentially `current_setting('request.jwt.claims')::json->>'sub'`. Their managed `auth.users` table exists **because they are also the identity provider** — GoTrue needs somewhere to write. BYO-auth pg-web needs no equivalent table.

Reference RLS recipe (target shape for `docs/APP-DEVELOPER-GUIDE.md` and the 020 blog):

```sql
ALTER TABLE posts ENABLE ROW LEVEL SECURITY;
ALTER TABLE posts FORCE ROW LEVEL SECURITY;   -- owner doesn't bypass either

CREATE POLICY read_visible ON posts FOR SELECT
  USING (published OR author_sub = current_setting('pgweb.user_id', true));

CREATE POLICY write_own ON posts FOR ALL
  USING (author_sub = current_setting('pgweb.user_id', true))
  WITH CHECK (author_sub = current_setting('pgweb.user_id', true));
```

## Current behavior (evidence @ 918f40b)

- **Handlers cannot see headers or cookies at all.** `crates/pg_web_ext/src/http.rs:69-115` builds `req` from method/path/query/body only; the only headers the worker reads are Content-Type (`:74-78`, to decide urlencoded parsing) and If-None-Match (`:82-85`, for asset 304s). `Cookie:` and `Authorization:` never reach SQL. Even a hand-rolled userland auth scheme is impossible today.
- **No `Set-Cookie` (or any response header) channel** — `http.rs:131,209` hardcode the content-type and the `ServeOutcome` carries no headers. Covered by 013; Options B-cookie and C depend on it.
- **Superuser execution** — `worker.rs:56`, covered by 014; without it RLS is decorative.
- **Bodies are urlencoded-only** (`http.rs:77`) — fine for this prompt; noted because OIDC callbacks use query params (already available in `req.query`).
- **The session_6 draft assumes pg-web is the IdP.** `docs/internal/sessions/session_6.md:37-49` drafts `pgweb.sessions(user_id BIGINT …)`, `session_create(p_user_id BIGINT)`, `pgweb.password_hash`/`password_verify` (bcrypt via pgcrypto), and an `init --template auth` with a `users(email, password_hash)` table and login/signup forms. `ROADMAP.md:183-194` mirrors this.
- **Existing contradiction to resolve:** `ROADMAP.md:59` says the RLS bridge is "handler's `SET LOCAL ROLE` from session" while `session_6.md:80` (open question B1) leans "GUC, not ROLE." This prompt settles it: **GUC for identity** (`pgweb.user_id`), **one fixed `SET LOCAL ROLE pgweb_app`** (from 014) for privilege — two different jobs, both per-request. Update ROADMAP:59 accordingly.
- **Type bug in the draft:** `session_6.md:27,37-40,73-75` and `ROADMAP.md:189` type the user id as BIGINT / cast `::bigint`. BYO-provider subjects are strings (`auth0|6a1f…`, UUIDs from Supabase/Firebase, numerics from homegrown). **`pgweb.user_id` must be TEXT end-to-end**; apps that want integer FKs map sub→local id themselves (JIT recipe below).

## The core primitive (build once, identity-source-agnostic)

Per request, inside the existing transaction, after 014's role floor:

```
1. extract credential        (mode-dependent: header / cookie / bearer)
2. resolve to user_id TEXT   (mode-dependent: trust / verify-JWT / session lookup)
3. SET LOCAL ROLE pgweb_app                       -- 014, privilege floor
4. SET LOCAL pgweb.user_id = '<sub>'              -- identity (skip if anonymous)
   [optional] SET LOCAL pgweb.jwt_claims = '<json>'
5. run handler → RLS enforces on app tables
6. commit/rollback — SET LOCALs evaporate
```

Framework surface to ship with the primitive:

- `pgweb.current_user_id() RETURNS TEXT` — wraps `current_setting('pgweb.user_id', true)`, NULL for anonymous (replaces session_6:75's BIGINT version).
- `req.user_id` (and optionally `req.claims`) added to the handler `req` JSON so templates/handlers get identity without an extra lookup — keep it consistent with whatever 013 does to `req`.
- Docs: the RLS recipes (anonymous-read/auth-write per session_6 B2; admin `BYPASSRLS` ops role per B3; ENABLE+FORCE everywhere).
- **JIT user-provisioning recipe** (documented pattern, not framework schema):

```sql
-- migrations/000X_users.sql (app-owned, optional)
CREATE TABLE users (sub TEXT PRIMARY KEY, email TEXT, created_at TIMESTAMPTZ DEFAULT now());
-- first authenticated touch, e.g. in a handler or a login-landing route:
INSERT INTO users (sub, email)
VALUES (pgweb.current_user_id(), …)
ON CONFLICT (sub) DO NOTHING;
```

## Proposed direction — three credential sources (pluggable, not exclusive)

### Option A — trusted reverse-proxy headers (cheapest; most pg-web-flavored)

Caddy already fronts every deployment for TLS (invariant #2). Auth proxies — oauth2-proxy, Authelia, caddy-security, Cloudflare Access, Tailscale — authenticate against any OIDC/SAML provider and forward a verified identity header (`X-Auth-Request-User`, `Cf-Access-Authenticated-User-Email`, `Tailscale-User-Login`, …). pg-web config names the header; the worker reads it and runs the primitive.

- Zero auth logic in pg-web; "not an identity provider" the way "not a TLS layer" already works.
- **The only mode not gated on 013's `Set-Cookie`** (the proxy owns the session); it needs only header passthrough into the worker (a slice of 013's request-side work) + the primitive. Fastest path to a working RLS demo.
- Trust boundary is the whole design: honor the header **only** when `[auth] source = "proxy"` is explicitly configured, and document that `:8080` must be unreachable except via the proxy (compose-internal network — the scaffolded compose already supports this shape). Consider an optional shared-secret header (`[auth] proxy_secret_header`) for defense in depth.
- **Lean:** ship this first as the reference mode; it is the smallest diff that makes 020's auth-gated blog real.

### Option B — JWT verification in the worker (the PostgREST model, minus the auth server)

Config: `jwks_url` *or* static HS256 secret / RS256 public key, plus expected `iss`/`aud`, clock skew, and the claim to use as the subject (default `sub`). Credential arrives as an HttpOnly cookie (HTMX-friendly — browsers attach it automatically; `hx-headers` JS would be needed for bearer) **or** `Authorization: Bearer` (the right shape for machine clients — JSON APIs and MCP tools per `prompts/005`). Worker verifies signature/exp/iss/aud **in Rust** (e.g. the `jsonwebtoken` crate), then runs the primitive with `sub` (+ full claims into `pgweb.jwt_claims` if configured).

- Verify in Rust, not SQL: SQL-side JWT (pgjwt-style) is realistically HMAC-only; RS256/ES256 + JWKS rotation is trivial in Rust and constant-time. No new Postgres extension deps.
- JWKS fetching is async outbound HTTPS with caching — this does **not** serialize requests the way SPI does (it's genuine async I/O on the Tokio runtime; precedent: the LISTEN task's outbound `tokio-postgres` connection, `crates/pg_web_ext/Cargo.toml:45`). Cache keys by `kid`; refresh on unknown-kid + periodic TTL; keep an offline grace window.
- Works unmodified with Auth0, Clerk, Keycloak, Zitadel, Firebase, Supabase Auth, Azure AD.
- Open edge to document honestly: **how the JWT gets into the cookie for a server-rendered HTMX app.** The OIDC code flow is a redirect dance plus a server-side token exchange. Viable answers: the provider's hosted login + its JS SDK writes the cookie; pair with Option A and let the proxy do the dance; or the stretch item below.
- **Lean:** ship as the second mode, same release if feasible — it's what makes bearer-token API/MCP clients (005) and SPA-ish setups work with zero proxy.

### Option C — framework sessions, BYO verification (session_6 Track A, generalized)

The framework owns **session mechanics only**: `pgweb.sessions` table (with `user_id TEXT`), `pgweb.session_create(p_user_id TEXT, p_ttl)`, `session_validate`, `session_revoke`, the signed HttpOnly cookie, and the worker middleware (`session_6.md:44`) feeding the primitive. **Credential verification is the app's job**: its login handler verifies an OIDC callback / a provider JWT (via a `pgweb.jwt_verify(...)` helper exposed from Option B's machinery) / a magic link / passwords-if-they-insist — then calls `session_create`. Gives server-side revocation (pure JWTs can't be revoked before exp), first-party cookies, and sliding expiry (session_6 A5).

- Requires 013 fully (Set-Cookie out, Cookie in).
- session_6's `password_hash`/`password_verify` (`:42-43`) survive only as a **documented recipe** using pgcrypto directly — not framework install SQL, not the auth template default. The drafted `init --template auth` (`:47-53`) gets rebuilt around BYO (e.g. a proxy-mode or JWT-mode scaffold), and its email-verification/reset open questions (A3, A4) close as **permanent non-goals** rather than Phase 3 deferrals.
- **Stretch (separate prompt if pursued):** framework-reserved `/_pgweb/auth/login|callback|logout` implementing a generic OIDC Authorization-Code client in the worker (async token exchange is runtime-safe, same argument as JWKS) — effectively oauth2-proxy built in. Heavy; only worth it if Option A's sidecar proves to be real adoption friction.
- **Lean:** third, after A/B, once 013 lands — it is convenience + revocation, not the foundation.

### Trade-off matrix (for the docs)

| | A: proxy | B: JWT | C: sessions |
|---|---|---|---|
| Auth code in pg-web | none | verify-only | session mechanics |
| Needs 013 | headers-in only | headers-in (+cookie) | full (Set-Cookie) |
| Revocation | provider's problem | ✗ until `exp` | ✓ server-side |
| HTMX browser fit | ✓ transparent | ✓ cookie / ✗ bearer-needs-JS | ✓ first-party |
| Machine clients (005) | awkward | ✓ bearer | ✗ cookie-bound |
| Extra infra | sidecar/proxy | none | none |
| Per-request cost | header read | sig verify (no SPI) | 1 SPI lookup |
| Main risk | header spoofing if misconfigured | key/claims misconfig | framework owns more |

CSRF interacts per mode (session_6 cross-cutting draft): cookie-carried identity (A-with-proxy-cookie, B-cookie, C) needs the double-submit protection; pure `Authorization: Bearer` does not. The CSRF middleware should key off "was identity cookie-derived," not off "is there a session."

## What this changes in the existing session_6 / ROADMAP drafts

1. `pgweb.user_id` and everything that touches it becomes **TEXT** (`session_6.md:27,37,39,40,73,75`; `ROADMAP.md:189`).
2. Resolve the ROLE-vs-GUC contradiction (`ROADMAP.md:59` vs `session_6.md:80`): GUC carries identity; 014's fixed non-superuser role carries privilege. Both `SET LOCAL`, per request.
3. Password helpers (`session_6.md:42-43`) demoted to a pgcrypto recipe; A3/A4 (email verification, reset) closed as non-goals; A2 (bcrypt cost) moves into the recipe text.
4. Track A is re-scoped from "the auth system" to "Option C: optional session layer"; Options A/B are new and land first.
5. VISION.md non-goals gains the "Not an identity provider" line; ROADMAP Phase 2 table is restructured around the three modes + the core primitive.

## Detailed design notes

- **Config surface:** a `[auth]` section in `pgweb.toml`, synced into `pgweb.settings` on push exactly like `[server].env` is today (`crates/pg_web_cli/src/push.rs:459` `sync_env` precedent — generalize it). Keys: `source = "none" | "proxy" | "jwt" | "session"` (consider allowing `["jwt","session"]` multi-source with documented precedence: bearer > cookie), `proxy_header`, `proxy_secret_header`, `jwks_url`, `jwt_secret`, `jwt_issuer`, `jwt_audience`, `jwt_user_claim` (default `sub`), `cookie_name`, `clock_skew_secs`. Secrets among these follow 014's secret-handling rules.
- **Failure semantics:** invalid/expired credential → treat as anonymous by default (RLS hides what it hides), with an optional strict mode (`[auth] reject_invalid = true` → 401 via 013's status channel). Never 500 on a bad token. Missing credential is always anonymous — route-level "login required" is an app concern (a handler checking `pgweb.current_user_id() IS NULL` and redirecting via 013) until/unless a later prompt adds declarative route protection.
- **Role-claim mapping (deliberately deferred):** PostgREST also switches `SET LOCAL ROLE anon|authenticated` from a JWT claim. pg-web should **not** do per-request role switching in v1 of this work — one execution role + GUC identity keeps the model simple; revisit alongside 014 if coarse grant-tiers prove necessary. Note it in open questions.
- **`pgweb.jwt_claims` exposure:** ship it gated behind config (`jwt_expose_claims = false` default) — full claims in a GUC are handy (`…->>'email'`) but widen what any SQL can read; with 014's role floor this is less scary, still default-closed.
- **Performance:** JWT verify is sub-millisecond CPU on the single thread (acceptable; cross-ref 015 — include an auth-on benchmark variant); session mode adds exactly one indexed SPI lookup; proxy mode adds nothing. None of the modes touch SPI before the transaction opens except session validation, which belongs **inside** the request transaction (invariant #4) so `last_seen_at` updates ride the same commit.
- **Asset requests** (`router.rs` GET-asset path) skip identity resolution entirely — public by design; note it.
- **`SET LOCAL` leak test is mandatory:** two back-to-back requests on the same worker, first authenticated, second anonymous — the second must see NULL `pgweb.user_id`. This is the single most important regression test in the whole feature (one shared SPI session = one mistake away from cross-user identity bleed).

## Research tasks for the implementing session

1. Read: `crates/pg_web_ext/src/http.rs` (request construction), `worker.rs` (SPI attach, runtime), `router.rs` (`serve`/`serve_in_tx` — where steps 3–4 of the primitive slot in), `schema.rs` (install SQL — where `current_user_id()` lands), `crates/pg_web_cli/src/push.rs` (`sync_env` → generalize for `[auth]`), `docs/internal/sessions/session_6.md` (full), `ROADMAP.md:176-201`, prompts 013/014/020 in this folder, `prompts/005` in-repo.
2. Survey header conventions of oauth2-proxy, Authelia, caddy-security, Cloudflare Access, Tailscale serve — pick defaults for proxy mode and write the Caddy + oauth2-proxy compose recipe for `docs/DEPLOYMENT.md`.
3. Evaluate Rust JWT crates (`jsonwebtoken` vs `jwt-simple`) for RS256/ES256/HS256 + JWKS handling; design the JWKS cache (kid-keyed, TTL + unknown-kid refresh, offline grace).
4. Confirm the pgrx-side mechanics of `SET LOCAL` via `Spi::run` inside `BackgroundWorker::transaction` (vs any GUC C-API shortcut) and measure cost.
5. Decide the `req` shape additions with 013 (`req.user_id`, `req.claims`?) so the two prompts don't fork the contract.
6. Write the leak test (above), an RLS end-to-end test (two identities, FORCE RLS, data isolation — session_6:146 already sketches this), and a proxy-spoof test (header sent but mode off → anonymous).
7. Prototype the 020 blog's login against mode A to validate the dogfood path end-to-end.

## Constraints & invariants to respect

- CLAUDE.md #4 — identity lives and dies with the one request transaction (`SET LOCAL` only).
- CLAUDE.md #2 / VISION non-goals — TLS and (now) identity-provision are out-of-process; pg-web verifies/propagates only.
- CLAUDE.md #7 — JWKS fetch / token exchange are async in the BGW; never block, never `.await` in `#[pg_extern]`.
- CLAUDE.md #3 — auth config flows CLI→DB via `pgweb.settings` upserts only.
- CLAUDE.md #6 — all SQL (helpers, recipes) works on PG 15/16/17.
- 014 gates everything: do not ship any RLS documentation implying enforcement while the worker is superuser.
- Companion-app rule — the 020 blog (or an `examples/` variant) must exercise whichever modes ship; no feature is "done" without it.

## Acceptance criteria

- [ ] With `[auth] source = "jwt"` configured, a request bearing a valid provider JWT (cookie or bearer) reaches its handler with `pgweb.current_user_id()` = the token's subject; expired/garbage tokens behave per the configured failure semantics (anonymous default / 401 strict), never 500.
- [ ] With `[auth] source = "proxy"`, the configured header propagates to `pgweb.user_id`; the same header is **ignored** when the mode is off (spoof test).
- [ ] A FORCE-RLS table with the reference policies provably isolates two identities end-to-end through real HTTP requests — demonstrating the 014 role floor works.
- [ ] The sequential-request leak test passes: identity never survives its transaction.
- [ ] `pgweb.current_user_id()` is TEXT; no BIGINT user-id remains in session_6/ROADMAP surfaces that ship.
- [ ] Anonymous-read/auth-write recipe + JIT provisioning recipe + Supabase-comparison mental-model section land in `docs/APP-DEVELOPER-GUIDE.md`; VISION gains the "Not an identity provider" non-goal; ROADMAP:59 contradiction resolved in the same commit (docs-match-code rule, ARCHITECTURE.md:3).
- [ ] CSRF stance per mode is documented (cookie-derived identity ⇒ double-submit; bearer ⇒ exempt).
- [ ] The 020 blog logs in via at least one shipped mode, with author-scoped RLS on `posts`.
- [ ] Five test tiers green, including new pg_tests for `current_user_id` and the RLS fixtures.

## Open questions

1. Multi-source precedence when both are configured: bearer-over-cookie is the lean — confirm, and decide whether mismatched simultaneous credentials are an error or precedence-silent.
2. Strict mode granularity: global `reject_invalid` only, or per-path-prefix protection config (`[auth] required = ["/admin"]`)? Lean: global-only v1; route protection stays in handlers via 013 redirects.
3. Should `pgweb.jwt_claims` ship at all in v1, or is `user_id` + the JIT table enough until someone needs claims?
4. Claim-to-role mapping (PostgREST-style `anon`/`authenticated` SET ROLE tiers): permanently out, or revisit-with-014 once grants exist? Lean: revisit later; GUC-only now.
5. Does Option C's signed-cookie format reuse session_6's draft (random 32-byte id + HMAC with `pgweb.settings.framework_secret`) unchanged, given user_id is now TEXT? Rotation story per session_6 ("rotating revokes all sessions") still acceptable?
6. The stretch `/_pgweb/auth/*` generic OIDC client: park permanently (proxies exist) or keep as a named future prompt? Lean: park; reopen only on demonstrated sidecar friction.
7. Cookie naming/attributes defaults (`pgweb_session`, `HttpOnly`, `SameSite=Lax`, `Secure` when `env=production` — session_6 A1): confirm the production-only `Secure` toggle matches the dev-on-plain-HTTP loop.
8. Where does the JWT-mode quickstart point first-time users — Clerk? Keycloak-in-compose? Pick one provider for the tutorial so the docs have a concrete end-to-end path.
