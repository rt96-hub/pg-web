# pg-web Threat Model (prompt 014)

**Status:** Initial version shipped with execution-role hardening (014). This is a living document; update on any material change to the request path, trust boundaries, or Phase 2+ features.

**Scope:** The model covers the Phase 1 synchronous core + the 014 privilege floor and per-request timeout. Phase 2 (auth/RLS/realtime) will extend it.

**Key invariant:** One HTTP request = one `BackgroundWorker::transaction` (SPI tx). The serving identity for that tx is the dedicated non-superuser role `pgweb_app` (NOSUPERUSER, NOBYPASSRLS). The role is `NOLOGIN`: no client (psql/libpq) session can ever authenticate as it, under any `pg_hba` method. The background worker still adopts it because it initializes its connection with `BGWORKER_BYPASS_ROLELOGINCHECK` — a PG 17 facility (pgrx 0.18's `connect_worker_to_spi` hardcodes `flags=0`, so `worker.rs` calls `pg_sys::BackgroundWorkerInitializeConnection` directly, replicating the wrapper). The flag is absent from the PG 15/16 headers: those cargo features keep compiling, but a worker built for them crash-loops on the NOLOGIN role and cannot serve — accepted under the 2026-06-12 decision that only the bundled image major (PG 17) is a runtime-correctness target. Admin operations (`pg-web push`, `migrate`) use whatever `DATABASE_URL` grants (typically a privileged role with DDL rights); they are a separate trust tier.

---

## Assets

| Asset | Why it matters |
|-------|----------------|
| Postgres cluster (DDL, all schemas, roles) | Total compromise if the serving role were superuser. The floor limits this to the DB(s) the worker is attached to. |
| Secrets (`pgweb.secrets`; also anything an operator puts in `pgweb.settings`) | Stripe keys, future session signing secret, etc. Plaintext at rest. |
| User application data (public.* and other schemas the app owns) | Multi-tenant isolation is the Phase 2 RLS promise. |
| Framework integrity (`pgweb.routes`, `templates`, `assets`, `migrations`, `deployments`, `settings`, `pgweb.pages__*` functions) | Tampering can reroute traffic, swap UIs, or replace business logic. |
| Web-tier availability (:8080) | Single-threaded current-thread Tokio runtime + blocking SPI tx per request. One unbounded handler = full outage (014 timeout intended; **currently not effective** — see the Unbounded execution row). |

## Actors

| Actor | Assumed capability / trust |
|-------|----------------------------|
| Anonymous web client | Arbitrary HTTP to :8080 (Caddy in front in prod). No credentials. |
| Authenticated user (Phase 2) | Valid session; must only see their own rows under RLS. |
| Malicious / careless handler author | Writes arbitrary PL/pgSQL into `pages/**/*.sql`. Today "trusted-ish" (the developer or their team controls the source tree that `pg-web push` installs). The privilege floor is intended to contain honest mistakes and limit the blast radius of a malicious handler. |
| Compromised dependency | A crate inside the worker or a malicious migration executed by the admin-tier connection. |
| DB-credential holder (ops, leaked `DATABASE_URL`) | Can `psql` as whatever that URL grants. This is the admin tier — expected to be able to do DDL and read secrets. |

## Attack Surfaces, Mitigations, and Gaps

| Surface | Attack | Mitigation (post-014) | Gap / Notes |
|---------|--------|-----------------------|-------------|
| :8080 HTTP listener | Oversize body | 2 MiB cap in http.rs (MAX_BODY_BYTES) | No rate limiting, no per-IP throttling, no slow-loris protection. |
| :8080 | CSRF on state-changing routes | None in Phase 1 | Planned Phase 2 (double-submit cookie on non-GET HTMX). Cookie helpers from prompt 013 already emit HttpOnly + SameSite=Lax + Secure-in-prod. |
| :8080 | Missing security headers (X-Frame-Options, CSP, etc.) | None in extension | Caddy (or equivalent) in the documented deployment story can add them; not yet codified. |
| SPI handler path | Handler reads/writes data it shouldn't (cross-tenant, framework tables, secrets) | **Serving role is `pgweb_app` (NOSUPERUSER, NOBYPASSRLS).** GRANTs are minimal (SELECT on catalog tables + EXECUTE on framework + user handlers; no writes on pgweb.* from the request path). Secrets behind `pgweb.secret()` SECURITY DEFINER (no table SELECT for the role). | App tables in `public` rely on `ALTER DEFAULT PRIVILEGES` at install time + developer-managed GRANTs for stricter setups. Hand-rolled `quote_literal` + `is_safe_ident` on handler names is tracked (low risk under standard_conforming_strings). |
| SPI handler path | Unbounded execution (`pg_sleep`, heavy lock wait, runaway query) → outage | **Intended:** per-request `SET LOCAL statement_timeout` (default 15s, from `pgweb.settings.request_timeout`, configurable via pgweb.toml); 57014 surfaces as a dedicated `ServeError::RequestTimeout` (clear 500 + log). **KNOWN GAP (2026-06-12, empirically verified):** the GUC is set but never *armed* — Postgres arms `statement_timeout` only in the regular-backend command loop (`start_xact_command` → `enable_statement_timeout` in `tcop/postgres.c`), which background-worker SPI execution never enters. In the shipped artifact a `pg_sleep(30)` handler ran to completion (HTTP 200 after 30.0 s) with `request_timeout='15s'` in effect. The 57014→RequestTimeout mapping is real and tier-1-tested; producing the cancel requires in-worker timer arming (custom `RegisterTimeout` handler raising the cancel interrupt) or a watchdog — required 014 follow-up before this row counts as a mitigation. | Long-poll/SSE endpoints (`/_pgweb/livereload` and future Phase-2 subscribe) are Axum-native and exempt (intentional). No per-route override yet (documented answer: "if you need >15s it's a job, see Phase 3"). |
| SPI handler path | RLS bypass | None pre-014 (superuser always bypassed). | **Fixed by the floor:** with `pgweb_app` + `NOBYPASSRLS`, a `SET LOCAL pgweb.user_id = '...' ` + `USING (author_id = current_setting('pgweb.user_id', true)::bigint)` policy now actually filters. Superuser path (pre-fix tests) leaked; the new role enforces. |
| SPI handler path | SQL injection via interpolated identifiers | `quote_literal` + `is_safe_ident` allowlist on the handler name before the dynamic `EXECUTE` in `_framework_call_handler`. Correct under PG defaults since 9.1. | Hand-rolled escaper on the trust boundary. Tracked, not removed. Handler name is itself allowlisted before interpolation. |
| CLI push/migrate path | Untrusted SQL installs malicious handlers or drops framework objects | Connection is operator-driven (`application_name` tagged "pg-web push/migrate ..."). Push is transactional with validation + reconcile. | This *is* the admin tier. It legitimately needs DDL on `pgweb.pages__*` and writes to framework tables. The worker never uses this identity. |
| LISTEN loopback (dev only) | Auth on the `tokio-postgres` connection used for livereload fan-out | Relies on `pg_hba` (trust/peer on 127.0.0.1) + POSTGRES_* env. Warns if password empty. | Separate auth path from the SPI role. Changing the SPI role does not affect it (the LISTEN task authenticates independently). Channel prefix `pgweb_app_<ch>` is unrelated to the role name `pgweb_app` — explicit call-out to avoid confusion. |
| Secrets at rest | Exfiltration via handler, `pg_dump`, future export | Role floor + `pgweb.secrets` + `SECURITY DEFINER pgweb.secret(key)` (serving role cannot `SELECT *` the table). `pg-web env set` echoes only the key. `env list` masks by default (`--show-values` to opt in). | Still plaintext. No `pg_dump` exclusion yet (operator must remember `--exclude-table-data=pgweb.secrets`). True at-rest encryption / KMS is Phase-later (acknowledged trade-off, consistent with prior docs). `pgweb.settings` remains readable by the serving role (by design — it holds non-secret config like `env`). |
| Direct client login as `pgweb_app` | `psql -U pgweb_app` (or any libpq client) to reuse the serving role's grants interactively | **`NOLOGIN`** — `InitializeSessionUserId` rejects the role for regular backends unconditionally (`FATAL: role "pgweb_app" is not permitted to log in`), regardless of `pg_hba` method — even `trust`/`peer` lines cannot admit it. Only the bgworker path can adopt the role, and only because `worker.rs` passes `BGWORKER_BYPASS_ROLELOGINCHECK`; client backends have no such bypass. The role also has **no password**, belt-and-braces. | A superuser can always `ALTER ROLE ... LOGIN`; that is the admin tier. `SET ROLE pgweb_app` from a session whose user is a member of the role (or superuser) also works by design — membership is an admin-tier grant. |
| Extension install / upgrade | Role lifecycle, GRANT drift | Guarded `CREATE ROLE` (DO + IF NOT EXISTS on pg_roles) with an ELSE `ALTER ROLE` that re-converges attributes (NOLOGIN, NOSUPERUSER, NOBYPASSRLS, NOCREATEDB, NOCREATEROLE, CONNECTION LIMIT -1) on every install — heals roles left by older definitions (e.g. the short-lived interim `LOGIN + CONNECTION LIMIT 0` form). GRANTs are in the bootstrap block. `ALTER DEFAULT PRIVILEGES` for public schema at install time. | Role is cluster-global; multi-DB clusters share one `pgweb_app`. Re-install / `DROP EXTENSION` leaves the role (documented). |

## Trust Assumptions (explicit)

- Handler authors are "trusted-ish." The floor primarily contains honest bugs (wrong table, missing WHERE, slow query) and limits the damage a malicious or compromised handler file can do. If pg-web is ever used to host *untrusted* third-party handler code, the `quote_literal` boundary, the dynamic EXECUTE in `_framework_call_handler`, and every `SECURITY DEFINER` helper become higher-stakes review items.
- The admin role (whatever `DATABASE_URL` grants for `pg-web push`/`migrate`) is fully trusted. It can read secrets, run arbitrary DDL, and overwrite handlers.
- The DB host is operator-controlled. Managed services that forbid custom extensions are out of scope.
- No HTTPS termination inside the extension (Caddy or equivalent terminates TLS; invariant).

## Cross-Cutting Items Deferred / Not Yet Mitigated

- Rate limiting, per-IP throttling, slow-loris guards on :8080.
- CSRF, security headers (Caddy layer in the deployment docs is the current recommendation).
- Full encryption-at-rest for secrets (documented as later work).
- Formal audit of the hand-rolled identifier escaper or the dynamic EXECUTE path.

## How 014 Changed the Model

Before 014 the serving identity was the bootstrap superuser (NULL username to `connect_worker_to_spi`). Every handler, every framework lookup, and any SQL injection was cluster-wide superuser. RLS policies would have been dead code. One `pg_sleep(999999)` wedged :8080 forever.

After 014:
- The connection identity is `pgweb_app` (`NOLOGIN` — every client login path is closed unconditionally; the worker alone adopts the role via `BGWORKER_BYPASS_ROLELOGINCHECK`, PG 17. On PG 15/16, whose headers lack the flag, the worker cannot serve — those majors are compile-only per the 2026-06-12 version-gate decision).
- `SET LOCAL statement_timeout` is issued in every request tx and 57014 maps to a dedicated error variant — but the bound is **not yet effective** (background-worker SPI never arms the statement timer; see the Unbounded execution row for the verified gap and the required follow-up).
- Secrets have a documented stricter path.
- The written threat model exists.
- ROADMAP/ARCHITECTURE contradictions on the RLS bridge mechanism are resolved (GUC + non-superuser serving role is the chosen path; `SET ROLE` per user is dropped as unscalable and still unsafe without the floor).

See also: `docs/ARCHITECTURE.md` (secrets section updated), `docs/ROADMAP.md` (Phase 2 RLS wording + decision log), `CLAUDE.md` (invariants), and the acceptance criteria in `prompts/014_execution_role_hardening_and_threat_model.md` (prompt still active — `statement_timeout` arming is a known follow-up).

---

**Maintenance:** Any change to SPI usage per request, the router dispatch, new framework tables, the LISTEN path, or the CLI push surface should trigger an update to this document and a re-evaluation of the GRANT list and timeout placement.