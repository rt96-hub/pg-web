# 014 — Execution-role hardening, per-request statement_timeout, and a threat model

**Status:** Open work order — security-critical; prerequisite for the Phase 2 RLS bridge
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Prerequisites:** none, but should land before Phase 2 auth (session_6) and pairs with 013 (response contract)
**Context:** The pg-web background worker connects to SPI with a NULL username, so every user-written SQL handler currently executes with bootstrap-superuser rights. There is no privilege floor under app code and no per-request `statement_timeout`. Both facts silently break the Phase 2 RLS bridge (superusers bypass RLS) and leave the web tier wedge-able by a single slow handler. This prompt specifies a privilege-floor design, a per-request timeout, pragmatic secret-handling hardening, and the project's first written threat model.

---

## Summary

Three coupled gaps, all on the SPI request path:

1. **Handlers run as superuser.** `worker.rs:56` calls `BackgroundWorker::connect_worker_to_spi(Some(&target_db), None)`. The `None` username makes the worker's SPI backend run as the cluster bootstrap superuser. Every `_framework_call_handler` EXECUTE (router.rs → schema.rs:131-163) therefore runs user PL/pgSQL with superuser rights. There is no privilege floor: a SQL-injection bug in a *user's* handler, or simply a malicious handler author, owns the cluster.

2. **No per-request `statement_timeout`.** Grep confirms zero `SET LOCAL statement_timeout` (or any `set_config`) anywhere in `crates/`. Combined with the single-threaded `current_thread` Tokio runtime (`worker.rs:63-76`) where `BackgroundWorker::transaction` blocks the event loop, one `pg_sleep`/locked/runaway handler wedges the entire :8080 tier indefinitely. (See prompt 015 for the runtime-blocking detail.)

3. **Secrets at rest are plaintext and superuser-readable.** `pgweb.settings` stores secrets as cleartext (`schema.rs:72-77`), documented and recommended for Stripe/API keys (`APP-DEVELOPER-GUIDE.md:383-414`, `ARCHITECTURE.md:151-171`). Under superuser execution, any handler / injection / `pg_dump` / future `pg-web export` reads everything.

The load-bearing point: **the Phase 2 auth/RLS bridge cannot be trusted until the execution role changes.** session_6 invariant 9 + Track B set `pgweb.user_id` and write RLS policies keyed on it — but **superusers bypass Row-Level Security entirely**, so those policies will silently fail to filter while handlers run as superuser. This work establishes the privilege floor that makes Phase 2 enforceable, adds the timeout as a cheap reliability+DoS control, and writes down the threat model the project currently lacks (no `docs/SECURITY.md` / `docs/THREAT-MODEL.md` exists).

## Why this matters now

- **It is a hard prerequisite for Phase 2, not a parallel nicety.** session_6 is drafted and the suggested shipping order (session_6 "Component shipping order") starts with Track B (RLS bridge) precisely because it is "smallest, decoupled." But Track B is a *no-op against a superuser connection*. If Phase 2 lands first, the project ships an authentication system that demonstrably does not isolate tenant data — the worst possible security outcome (looks secure, isn't). This prompt must land **before** session_6 Track B, or Track B must absorb it.
- **There is an unresolved contradiction between the two planning docs that this work must settle.** `ROADMAP.md:59` and `ROADMAP.md:186` describe the bridge as "handler's `SET LOCAL ROLE` from session". session_6 open question B1 leans the opposite way ("GUC over `SET ROLE`"). These are not interchangeable under the current connection model (see Detailed design). Closing this is part of the work order.
- **The blast radius today is total.** With superuser execution there is no degraded-but-contained failure mode. Every handler bug is a full-cluster bug. That is an unusual risk posture to ship publicly (v0.2.0 is heading toward public readiness per prompts 009/010).
- **The timeout is nearly free and independently valuable.** Even before the role split, `SET LOCAL statement_timeout` inside the request transaction is a few lines and converts an indefinite hang into a bounded 500.

## Current behavior (evidence)

Read these before designing — do not trust this summary blind.

- **Superuser connection.** `crates/pg_web_ext/src/worker.rs:56` —
  `BackgroundWorker::connect_worker_to_spi(Some(&target_db), None)`. The second arg is the SPI username; `None` ⇒ bootstrap superuser. `docs/internal/DEVELOPER-GUIDE.md:70` documents this as the single startup-time SPI session reused by every request.
- **Handler dispatch under that identity.** `crates/pg_web_ext/src/router.rs:505-578` (`call_handler`) builds `SELECT ... FROM pgweb._framework_call_handler($name, $req::json)` and runs it via `Spi::connect`/`client.select`. The wrapper `pgweb._framework_call_handler` (`crates/pg_web_ext/src/schema.rs:131-163`) does `EXECUTE format('SELECT (%s($1))::text', p_handler_name)` inside a `BEGIN ... EXCEPTION WHEN OTHERS` block. All of this inherits the worker's superuser role.
- **One request = one transaction.** `crates/pg_web_ext/src/router.rs:67-71` — `serve()` wraps `serve_in_tx` in `BackgroundWorker::transaction`. This is invariant #4 and is the exact scope where a `SET LOCAL ROLE` / `SET LOCAL statement_timeout` would live.
- **No timeout anywhere.** `grep -rn "statement_timeout\|set_config\|SET LOCAL" crates/` returns only `worker.rs:56` (the connect) and session_6's *planned* `SET LOCAL pgweb.user_id`. Nothing sets a statement timeout on any path.
- **Secrets in plaintext.** `crates/pg_web_ext/src/schema.rs:72-77` (`pgweb.settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)`); `crates/pg_web_cli/src/env.rs` (`set`/`unset`/`list` plain upsert/select). `docs/APP-DEVELOPER-GUIDE.md:414`: "The CLI writes values in cleartext... Don't store anything the DB admin shouldn't be able to `SELECT`." `docs/ARCHITECTURE.md:171`: GUC-based secrets are "Not encrypted at rest (acknowledged trade-off)." Note a **doc/impl split**: ARCHITECTURE.md:151-171 describes secrets as `ALTER DATABASE ... SET pgweb.X` GUCs read via `current_setting`, while the implemented path (`env.rs` + `pgweb.setting()` in `schema.rs:218-224`) uses the `pgweb.settings` table. Both are superuser-readable; the implementing session should pick the canonical mechanism and align the docs.
- **CLI is already connection-tagged.** `crates/pg_web_cli/src/db.rs:34-48` tags every CLI backend with `application_name = "pg-web <verb> (pid=…, host=…)"`. `push.rs` performs DDL — `CREATE OR REPLACE FUNCTION pgweb.pages__*(req json)` (`push.rs:491-497`), `DROP FUNCTION pgweb.{proname}(json)` (`push.rs:669-675`), route/template upserts — and `migrate.rs:44-60` runs arbitrary `batch_execute` SQL. So the **CLI legitimately needs DDL + framework-table writes; the request-serving path needs neither.** That asymmetry is the seam for an admin/serving role split.
- **Existing weak abuse controls.** The only request-path abuse control is the 2 MiB body cap (`crates/pg_web_ext/src/http.rs:29-32`, `MAX_BODY_BYTES`). Request-path SQL escaping is hand-rolled `quote_literal` (`router.rs:322-334`) plus an allowlist `is_safe_ident` (`router.rs:340-357`) gating the handler name before it is interpolated into the EXECUTE. This is **correct under `standard_conforming_strings=on`** (PG default since 9.1) and the handler name is allowlisted, so it is low-risk — but it is a hand-rolled escaper on the trust boundary and belongs in the threat model as "track, don't panic."
- **Naming caution.** The LISTEN fan-out uses the channel-name *string prefix* `pgweb_app_<channel>` (`crates/pg_web_ext/src/listen_router.rs:16`). That is unrelated to any database role. If this work introduces a role literally named `pgweb_app`, call out the collision-of-vibes explicitly so nobody conflates the NOTIFY channel prefix with the app role.
- **No security docs exist.** `find -iname '*security*' -o -iname '*threat*'` returns nothing. CSRF, rate-limiting, and security headers are only *mentioned* (session_6 "Cross-cutting — CSRF"; `ROADMAP.md` Phase 2), none implemented.

## Threat model (asset / actor / attack table)

This is the seed for a new `docs/THREAT-MODEL.md`. Keep it compact and honest; "Gap" means no control exists today.

### Assets
| Asset | Why it matters |
|---|---|
| Postgres cluster (DDL, all schemas, roles) | Superuser execution = total compromise of everything in the instance, not just the app DB. |
| Secrets (`pgweb.settings`, GUC secrets) | Stripe/API keys, framework session secret (Phase 2). Plaintext, superuser-readable. |
| User application data (`public.*`) | Multi-tenant rows Phase 2 RLS is meant to isolate. |
| Framework integrity (`pgweb.routes/templates/assets/migrations/deployments/settings`, `pgweb.pages__*`) | Tampering reroutes the app, swaps templates, or replaces handlers. |
| Web-tier availability (:8080 listener) | Single-threaded runtime; one stuck handler = full outage. |

### Actors
| Actor | Capability assumed |
|---|---|
| Anonymous web client | Arbitrary HTTP to :8080 (through Caddy). No credentials. |
| Authenticated user (Phase 2) | Valid session cookie; expected to see only their own rows. |
| Malicious / careless handler author | Can write any PL/pgSQL into `pages/*.sql` that `pg-web push` installs. Trusted-ish today, but the privilege floor should still contain mistakes. |
| Compromised dependency | A crate in the worker or a malicious migration executes in-process. |
| DB-credential holder | Anyone who can `psql` the instance (ops, leaked creds). |

### Attack surfaces × current mitigations vs gaps
| Surface | Attack | Mitigation today | Gap |
|---|---|---|---|
| :8080 HTTP listener | Oversize body → memory exhaustion | 2 MiB cap (`http.rs:32`) | No rate limit, no per-IP throttle, no slow-loris guard. |
| :8080 HTTP listener | Cross-site request forgery on state-changing routes | none | No CSRF (planned session_6); no `SameSite`/`Secure` story until Phase 2. |
| :8080 HTTP listener | Missing security headers (clickjacking, MIME-sniff, no HSTS hint) | none | No `X-Frame-Options`/`X-Content-Type-Options`/CSP. Caddy could add some; undocumented. |
| SPI handler path | Handler reads/writes data it shouldn't (cross-tenant, framework tables, secrets) | none | **Superuser execution — no privilege floor. This is the central gap.** |
| SPI handler path | Handler runs unbounded (`pg_sleep`, lock wait) → outage | none | **No `statement_timeout`.** |
| SPI handler path | SQL injection via interpolated route/handler identifiers | `quote_literal` + `is_safe_ident` allowlist; correct under `standard_conforming_strings=on` | Hand-rolled escaper on the trust boundary; track. Low risk. |
| SPI handler path | RLS bypass | n/a (no RLS yet) | Superuser **bypasses RLS**, so Phase 2 policies won't enforce. |
| CLI push/migrate path | Untrusted SQL installs malicious handlers / drops framework tables | Connection is operator-driven; `application_name` tagged | Push/migrate legitimately have superuser DDL; this is the *admin* trust tier — keep it separate from serving. |
| LISTEN loopback connection (dev) | Loopback `tokio-postgres` auth (`worker.rs:127-143`, `build_listen_conn_str`) | Relies on `pg_hba` trust/peer on 127.0.0.1; warns if password unset | Separate auth path from SPI; must not break when the SPI role changes (it authenticates independently, so role split shouldn't touch it — verify). |
| Secrets at rest | Exfiltration via handler / `pg_dump` / export | `env set` echoes key-only on write (`main.rs:315`); value masked at set time | `env list` prints values cleartext (`env.rs:61-73`); no `pg_dump` exclusion; no encryption. Superuser execution makes any handler a reader. |

## Proposed direction (options)

The goal is a **privilege floor under user handlers** plus a **bounded per-request timeout**, without breaking invariants #1/#4/#6 or the dev livereload LISTEN loop.

### Privilege floor

**Option A — dedicated non-superuser role `pgweb_app`; the worker connects as it.**
Create `pgweb_app` (NOSUPERUSER, NOBYPASSRLS, NOCREATEDB, NOCREATEROLE) in install SQL, then either:
- (A1) connect the worker as it: `connect_worker_to_spi(Some(db), Some("pgweb_app"))` at `worker.rs:56`; or
- (A2) keep the superuser connection but `SET LOCAL ROLE pgweb_app` as the first statement of every request transaction.

A1 is the real boundary (see Option B's caveat). GRANTs the framework needs for serving:
- `USAGE` on schema `pgweb`;
- `SELECT` on `pgweb.routes`, `pgweb.templates`, `pgweb.assets`, `pgweb.settings` (and Phase 2 `pgweb.sessions`);
- `EXECUTE` on `pgweb._framework_call_handler`, `pgweb.html_escape`, `pgweb.setting`, and the `pgweb.pages__*` handlers;
- **no** INSERT/UPDATE/DELETE/TRUNCATE/DDL on framework tables.

User app-table access: the standard Postgres pattern is that the serving role must be `GRANT`ed rights on `public` objects the handlers touch (or the developer's migrations `GRANT ... TO pgweb_app`). Decide whether pg-web (a) documents this as a developer responsibility, (b) auto-grants in `push`/`migrate` (e.g., `GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO pgweb_app` + `ALTER DEFAULT PRIVILEGES`), or (c) provides a `pg-web grant` helper. **Lean:** A1 (connect as `pgweb_app`) is the correct floor; pair it with `ALTER DEFAULT PRIVILEGES` set up at install/push time so the todo example and new apps "just work" without per-table grant ceremony, and document the override for stricter setups.

**Option B — keep superuser worker connection; `SET LOCAL ROLE` to a low-priv role per request, `RESET` on exit.**
Simpler to bootstrap (no connection-auth change), and the request transaction boundary already exists (invariant #4). **But be honest: `SET ROLE` alone is not a security boundary.** A malicious handler can execute `RESET ROLE;` (or `SET ROLE postgres;`) and climb straight back to superuser, because the *connection's* identity is still superuser. So B is only acceptable for containing *honest mistakes* (a buggy query against the wrong table), not for defending against an adversarial handler author. **Lean:** do not rely on B as the boundary. If B is used as a transitional step, gate handlers so they cannot issue `SET ROLE`/`RESET ROLE` — which in practice means restricting via the connection identity (Option A) or `SECURITY DEFINER` wrappers, i.e. you end up at A anyway. Treat B as "better than nothing, not a floor."

### Admin / bypass path
Split trust tiers explicitly:
- **Serving role** (`pgweb_app`, least privilege, `NOBYPASSRLS`) — the worker's SPI identity. Read-only on framework tables; scoped on app tables.
- **Admin role** for `pg-web push`/`migrate` — needs DDL on `pgweb.pages__*`, write on framework tables, and (for admin scripts that must see all tenant rows) optionally `BYPASSRLS`. This is exactly session_6 open question B3 ("user creates their own admin role... `DATABASE_URL_ADMIN`"). The CLI already distinguishes connections by `application_name` (`db.rs:44-48`); push needs DDL, serving does not. **Lean:** keep them as two distinct database roles; the worker never uses the admin role. Document that `pg-web push`/`migrate` connect with a privileged role (today: whatever `DATABASE_URL` grants) while the worker connects as `pgweb_app`.

### Per-request statement_timeout
`SET LOCAL statement_timeout = '<configurable>'` as an early statement inside the request transaction (`router.rs` `serve_in_tx`, before `lookup_route`/`call_handler`). `SET LOCAL` is transaction-scoped, so it auto-resets at commit/rollback — no cleanup. Default value: make it `pgweb.toml [server].request_timeout` synced into `pgweb.settings` (consistent with how `env` is synced; invariant #7). Suggested default **15s** (long enough for a slow report, short enough to bound an outage). On timeout the handler raises SQLSTATE `57014` (`query_canceled`), which flows through the existing `EXCEPTION WHEN OTHERS` in `_framework_call_handler` and `classify_handler_error` → a 500. Add a dedicated `ServeError` variant for `57014` so the response/log says "handler exceeded request_timeout" rather than a generic SQL exception. **Long-poll/SSE endpoints are exempt by construction** — `/_pgweb/livereload` and Phase 2 `/_pgweb/subscribe/*` are served by their own Axum handlers and do **not** go through `router::serve`/the SPI request transaction, so the timeout never touches them. Verify this remains true and note it.

### Secrets
Reduce blast radius without committing to full encryption-at-rest:
- **Role-split first** (above) removes the "any handler reads every secret" property for handlers that don't need a given secret — but note `pgweb.setting()` is `SECURITY INVOKER` reading `pgweb.settings`, so the serving role still needs `SELECT` on the table. To actually restrict, either (a) move secrets into a separate `pgweb.secrets` table the serving role lacks `SELECT` on, exposing only a `SECURITY DEFINER` `pgweb.secret(key)` function with an allowlist, or (b) a reserved key-prefix (e.g. `secret.*`) with column/row grants the serving role lacks.
- **Exclude from `pg_dump`/export by default** (e.g. `pgweb.secrets` marked so a future `pg-web export` skips it; document `pg_dump --exclude-table-data=pgweb.secrets`).
- `env list` should **mask values** by default (it currently prints cleartext, `env.rs:61-73`); `set` already echoes key-only (`main.rs:315`). Add `--show-values` to opt in.

**Lean:** role-split first (root fix) → secrets-table separation behind a `SECURITY DEFINER` accessor → `env list` masking → leave true encryption-at-rest / KMS as a documented Phase-later item (consistent with `APP-DEVELOPER-GUIDE.md:414` already calling it Phase 2+).

## Detailed design notes

- **Where the SETs go.** Both `SET LOCAL ROLE pgweb_app` (if Option A2 / transitional) and `SET LOCAL statement_timeout` belong at the top of `serve_in_tx` (`router.rs:73`), inside the `BackgroundWorker::transaction` opened by `serve` (`router.rs:67-71`). They must run *before* `call_handler`. With Option A1 (connect-as-role) you don't need `SET ROLE` at all — the timeout SET still goes here.
- **Phase 2 `SET LOCAL pgweb.user_id` ordering.** session_6 sets `pgweb.user_id` after session validation, before the handler. That SET and the timeout SET coexist in the same transaction; ordering is: timeout → (auth middleware validates session) → `SET LOCAL pgweb.user_id` → handler. Confirm `SET LOCAL` of a custom GUC requires the GUC be known; under a `pgweb.*` custom prefix it is accepted without prior declaration (custom placeholders are allowed). Validate on PG 15/16/17.
- **Install SQL changes** (`schema.rs` `extension_sql!` bootstrap block): `CREATE ROLE pgweb_app NOLOGIN NOSUPERUSER NOBYPASSRLS;` then the GRANTs above. `NOLOGIN` is fine if the worker reaches the role via `SET ROLE` (A2) or via `connect_worker_to_spi` with a role the bgworker can assume; if A1 requires a LOGIN role, reconsider — `connect_worker_to_spi` connects *as* the named role, which typically needs the role to exist but bgworker connections don't authenticate via `pg_hba` the way client connections do (verify against pgrx's `connect_worker_to_spi` semantics on all three PG versions; this is invariant #1 territory — pgrx only).
- **Idempotency / upgrade.** `CREATE ROLE` is not `IF NOT EXISTS`-friendly across re-install; guard with a `DO $$ ... IF NOT EXISTS (SELECT FROM pg_roles ...) $$` block. Extension-owned roles complicate `DROP EXTENSION` (roles are cluster-global, not schema objects) — decide whether the role is created by the extension or by a documented one-time bootstrap step. This is a real design fork; surface it.
- **`SECURITY DEFINER` interplay.** If serving is least-privilege, some framework functions may need `SECURITY DEFINER` to read framework tables on the serving role's behalf (e.g. a secrets accessor). Each `SECURITY DEFINER` function is itself a trust boundary — set `search_path` explicitly and keep them minimal.
- **dev livereload LISTEN loop is independent.** `build_listen_conn_str` (`worker.rs:127-143`) builds a `tokio-postgres` loopback connection authenticated via `POSTGRES_USER`/`pg_hba`, *not* via the SPI role. Changing the SPI role should not affect it — but confirm the LISTEN connection's role still has `LISTEN` privilege on the livereload channel (LISTEN needs no special grant in PG, so this should be fine) and that dev mode still hot-reloads after the change.

## Interaction with the Phase 2 RLS bridge

This is the crux. session_6 Track B + invariant 9 implement tenant isolation by `SET LOCAL pgweb.user_id = <id>` and policies like `USING (author_id = current_setting('pgweb.user_id', true)::bigint)`.

- **Superusers bypass RLS unconditionally.** Per Postgres semantics, a superuser (and any role with `BYPASSRLS`) ignores all policies. So under today's connection model, every Track B policy is dead code: the handler reads/writes all rows regardless. An attacker (or an honest cross-tenant bug) is not contained. **The RLS bridge is untrustworthy until the serving role is `NOSUPERUSER NOBYPASSRLS`.** This work order is the enabling change.
- **Resolve the B1 contradiction.** `ROADMAP.md:59`/`:186` say "`SET LOCAL ROLE` from session"; session_6 B1 leans GUC. The two only become equivalent-in-safety once the *connection* is non-superuser:
  - With a non-superuser serving role + the **GUC** approach (session_6 B1 lean): policies enforce, no per-user roles needed. This is the Supabase/PostgREST pattern and scales. **Recommended.**
  - The **`SET ROLE` per user** approach (ROADMAP wording) needs a DB role per app user — admin overhead, no scale — and is *still unsafe if the base connection is superuser*. Drop it.
  - Net: choose **non-superuser serving role (this prompt) + `pgweb.user_id` GUC (session_6 B1)**, and update `ROADMAP.md:59`/`:186` to match so the docs stop contradicting each other.
- **Acceptance linkage.** A passing proof is: with the floor in place, a policy keyed on `pgweb.user_id` actually filters rows for two different user ids (see Acceptance). That single test simultaneously proves the floor works and unblocks Phase 2.

## Research tasks for the implementing session

1. **Confirm `connect_worker_to_spi` role semantics on PG 15/16/17.** Does passing `Some("pgweb_app")` require a `LOGIN` role? Does the bgworker authenticate via `pg_hba`, or does it assume the role directly (like `SET SESSION AUTHORIZATION`)? Read pgrx's `bgworkers` source / docs — invariant #1 means no raw FFI workaround. This decides Option A1 vs A2.
2. **Verify RLS-bypass behavior empirically.** Create a `public.todos` with a `pgweb.user_id` policy; call a handler as superuser vs as `pgweb_app` with two different `SET LOCAL pgweb.user_id` values; confirm rows leak under superuser and are filtered under `pgweb_app`.
3. **Map the exact GRANT set** by enumerating every framework object the serving path touches at runtime (routes, templates, assets, settings/secrets, `_framework_call_handler`, `html_escape`, `setting`, `pgweb.pages__*`, Phase 2 `sessions`). Produce the minimal grant list.
4. **Decide app-table grant strategy** (developer-documented vs `ALTER DEFAULT PRIVILEGES` in push/migrate vs `pg-web grant`). Prototype against `examples/todo/`.
5. **Prove the timeout** end-to-end: a `pg_sleep(30)` handler with `request_timeout=15s` returns 500 (`57014`) and the site stays responsive to a concurrent request. Confirm `/_pgweb/livereload` (and a stubbed `/_pgweb/subscribe`) are unaffected.
6. **Settle the role-lifecycle question:** extension-created role vs documented bootstrap; behavior on re-install and `DROP EXTENSION`.
7. **Secrets:** prototype a `pgweb.secrets` table + `SECURITY DEFINER` `pgweb.secret(key)` accessor the serving role can `EXECUTE` but not `SELECT` the table; confirm `pgweb.setting()` callers migrate cleanly. Add `env list` masking + `--show-values`.
8. **Reconcile docs:** ARCHITECTURE.md secrets section (GUC vs table), ROADMAP.md RLS wording, and write `docs/THREAT-MODEL.md` from the table above.
9. **Companion-app coverage:** extend `examples/todo/` so the floor + timeout + (Phase-2-ready) RLS are exercised, per the "every feature ships with a companion-app flow" rule in CLAUDE.md.

## Constraints & invariants to respect

- **#1 pgrx only, no raw FFI.** Role/connection changes go through pgrx's `bgworkers` API; do not hand-roll the backend connection.
- **#4 one request = one SPI transaction.** The `SET LOCAL ROLE` (if used) and `SET LOCAL statement_timeout` live inside that single transaction and rely on its commit/rollback to reset. Do not split a request across transactions to manage role/timeout.
- **#6 PG 15/16/17.** All role DDL, GRANTs, `SET LOCAL`, custom-GUC behavior, and `connect_worker_to_spi(role)` semantics must work identically on all three. No pg18 assumptions.
- **#7 `pgweb.settings` is the runtime-config source of truth.** `request_timeout` (and any new flags) sync from `pgweb.toml` into `pgweb.settings` like `env` does.
- **Do not break dev livereload.** The loopback LISTEN connection (`worker.rs:127-143`) authenticates separately from SPI; the role change must leave `pg-web dev` hot-reload working. Verify.
- **Do not regress the existing escaping/allowlist** on the request path (`quote_literal`/`is_safe_ident`). They are correct; leave them, just document them in the threat model.
- **Phase discipline:** this is a security-hardening of the Phase 1 core that *enables* Phase 2; it must not pull Phase 2 auth/realtime features into the core path. It only establishes the floor and timeout the Phase 2 work will sit on.

## Acceptance criteria

1. A user handler can **no longer** `DROP TABLE pgweb.routes`, `ALTER`/`INSERT` framework tables, or read a schema/table it was not granted — verified by a `#[pg_test]` that asserts a permission-denied SQLSTATE (`42501`) for those operations when run as the serving role.
2. A handler executing `pg_sleep(30)` with `request_timeout=15s` is canceled at ~15s, surfaces as a 500 with a clear "request_timeout exceeded" error/log (SQLSTATE `57014`), and a concurrent request to a fast route returns 200 while it runs.
3. `/_pgweb/livereload` (and the Phase-2 `/_pgweb/subscribe/*` once present) are demonstrably **not** subject to `statement_timeout` — a long-lived SSE stream survives past the timeout window.
4. An RLS policy keyed on `current_setting('pgweb.user_id', true)::bigint` **actually filters rows**: a `#[pg_test]` sets two different `pgweb.user_id` values inside the serving role and sees only the matching rows each time (and sees *all* rows fail to filter under the old superuser path, documented as the regression this fixes). This proves the floor enables Phase 2.
5. `pg-web push` and `pg-web migrate` still succeed — they connect with the privileged/admin role and retain DDL on `pgweb.pages__*` and writes on framework tables; a `#[pg_test]`/CLI test confirms a push that creates/drops a handler works while the *serving* role cannot.
6. The serving role is verifiably `NOSUPERUSER` and `NOBYPASSRLS` (assert via `pg_roles`), and the worker's SPI identity at request time is the serving role, not the bootstrap superuser (assert `current_user`/`session_user` from inside a handler).
7. `pgweb.settings`/secrets are no longer freely readable by the serving role beyond what's needed: secrets live behind a `SECURITY DEFINER` accessor (or equivalent grant split) such that a handler can fetch an allowed secret but cannot `SELECT *` the secrets table; `pg-web env list` masks values by default.
8. `docs/THREAT-MODEL.md` exists, derived from the asset/actor/attack table here, and `ARCHITECTURE.md` + `ROADMAP.md` no longer contradict each other on the RLS-bridge mechanism (GUC vs `SET ROLE`).
9. `examples/todo/` exercises the floor + timeout (companion-app rule), and the full test suite stays green on PG 15/16/17 with the new tests added.
10. `pg-web dev` hot-reload still works end-to-end after the role change (manual or tier-3 verification noted).

## Open questions

1. **A1 vs A2:** does pgrx's `connect_worker_to_spi(Some(role))` cleanly connect the bgworker *as* a non-superuser role on all of PG 15/16/17, or must we keep the superuser connection and `SET LOCAL ROLE` (accepting that it isn't a hard boundary against adversarial handlers)? This is the single most important unknown.
2. **Role lifecycle:** should `pgweb_app` be created/owned by the extension (complicates `DROP EXTENSION`, and roles are cluster-global not DB-local) or by a documented one-time `pg-web` bootstrap step? What happens on extension upgrade and on multi-database clusters sharing one role?
3. **App-table grants:** auto-`GRANT`/`ALTER DEFAULT PRIVILEGES` in push/migrate (magical, but pg-web starts writing grants on the user's `public` schema) vs document-and-defer to the developer vs a `pg-web grant` verb? Where's the line between convenience and surprising the user's DBA?
4. **Adversarial vs careless handler authors:** what threat tier do we actually defend? If handler authors are fully trusted (single-team apps), the floor mainly contains bugs; if pg-web is ever used for multi-tenant/untrusted handler code, `SECURITY DEFINER` framework functions and the `quote_literal` boundary need a harder look. State the assumption in `docs/THREAT-MODEL.md`.
5. **Default `request_timeout` value** and whether SSE/long-running *handlers* (not the exempt SSE endpoints) need a per-route override mechanism, or whether "if you need >15s, it's a job, see Phase 3" is the documented answer.
6. **Secrets accessor shape:** `pgweb.secrets` table + `SECURITY DEFINER` `pgweb.secret(key)` vs reserved key-prefix with row/column grants vs leaving `pgweb.settings` as-is and relying solely on the role floor. How much is worth doing before real encryption-at-rest (which we're explicitly deferring)?
7. **`pg_dump` / future `pg-web export` posture:** should secrets be excluded from dumps by default, and does that risk silent data loss on restore (operator forgets secrets aren't in the dump)? Document the trade-off.
8. **GUC namespace for `pgweb.user_id`:** confirm a custom `pgweb.*` placeholder GUC is settable via `SET LOCAL` without prior `ALTER SYSTEM`/registration on PG 15/16/17 (it should be, as an unreserved custom prefix), so Track B's contract holds under the new role.
