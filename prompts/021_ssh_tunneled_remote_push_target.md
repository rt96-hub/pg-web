# 021 — SSH-tunneled remote deploy: `pg-web push --target <name>` and `migrate apply`

**Status:** Open work order — high priority; user-flagged important; primary remaining deliverable from Session 5
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from Session 5 design + external analysis, 2026-06-11)
**Prerequisites:** none (F.3 CLI-in-image is already shipped and pairs well; validation ultimately requires real remote infrastructure)
**Context:** Session 5 explicitly deferred F.2 because "implementation cost is large for a feature only validatable end-to-end against a real remote target." The detailed design was locked earlier (Session 4 planning, commit `f6a809d` referenced in session_5.md) and left intact. Today, remote production deploys still require either exposing Postgres publicly, holding a manual `ssh -L` tunnel, or SSHing in and using the in-image CLI (F.3). `docs/DEPLOYMENT.md`, `ROADMAP.md`, and `OVERVIEW.md` all still document the manual-tunnel story as the supported path. The full spec lives in `docs/internal/sessions/session_5.md` § F.2 and the handoff prompt at the end of that file.

This is the single biggest remaining friction point for "pg-web works in production without ceremony."

---

## Summary

`pg-web push` (and `migrate apply`) currently connect as a normal libpq client to whatever `DATABASE_URL` or `--url` the user supplies. For a real VPS the Postgres port is almost always on 127.0.0.1 inside the box.

The locked design adds first-class support for a named deploy target:

```toml
[deploy.prod]
ssh = "deploy@app.example.com"   # or any ssh(1) target, ssh_config alias, etc.
# ssh_port    = 22
# db_host     = "127.0.0.1"
# db_port     = 5432
# pgpass_from = "PGWEB_PROD_PASSWORD"   # env var on the *client* machine
```

`pg-web push --target prod` (and the same flag on `migrate apply`):
- Uses the `openssh` crate (thin wrapper over the system `ssh` binary) to open a session, inheriting `~/.ssh/config`, ssh-agent, known_hosts, ProxyJump, etc.
- Requests a local port-forward `127.0.0.1:<ephemeral> → remote:127.0.0.1:5432`.
- Runs the normal push/migrate transaction against the forwarded address.
- Tears the tunnel down on exit (success or error).
- Only port 22 needs to be reachable on the server; Postgres itself never listens publicly.

The same target can be used for `--dry-run` (tunnel opens, work runs inside a transaction, tunnel closes, DB untouched — excellent for CI).

## Why this matters now

- It was the "user-flagged important" item in Session 5 and the reason F.2 was called out in the v0.2.0 handoff.
- Without it, every real deployment story still contains "and then you hold this terminal window open with an ssh tunnel" or "ssh in and run the command by hand."
- F.3 (already shipped) makes the "ssh in and run it" path convenient; F.2 makes the "never ssh in for deploys" path possible.
- The design is small in the CLI (TOML parsing + openssh orchestration) but the end-to-end validation cost is high — exactly why it was punted with the explicit note to resume "when remote infra is available."

## Current behavior (evidence)

- `crates/pg_web_cli/src/main.rs` and `push.rs` / `migrate.rs` only ever use direct libpq connections via the `db` module.
- No `[deploy.*]` section is parsed anywhere (the only `[server]` and `[dev]` sections are handled).
- `docs/DEPLOYMENT.md` still walks through manual `ssh -L`, background tunnels, and `pg-web push --url postgres://...@127.0.0.1:5432/...`.
- `ROADMAP.md` and `OVERVIEW.md` list F.2 as ⏸ "Deferred to Session 6 — needs real remote infra to validate. Manual `ssh -L` + in-image CLI (F.3) are the supported paths today."
- The original detailed spec and error-path requirements remain only in `docs/internal/sessions/session_5.md:80-123` (and the handoff prompt at the bottom of that file).

## Proposed direction (options)

The design was deliberately locked before Session 5 started. The work order is now "implement the locked design + make it testable + update all docs + add the missing smoke path."

**Lean:** Implement exactly as specified in session_5.md § F.2. Use the `openssh` crate (already chosen for inheritance reasons). Support both `push` and `migrate apply`. Add `--target` to the existing flag parsing. Produce clear, non-swallowed SSH and "remote PG unreachable" errors.

Key implementation notes (from the locked spec):
- The tunnel is a local forward (`-L` style) — the CLI still speaks normal Postgres protocol to 127.0.0.1:ephemeral. This is simpler and inherits libpq retry/backoff.
- Credentials: user's normal ssh keys/agent + normal libpq `~/.pgpass` / `PGPASSWORD` / etc. No new pg-web credential types.
- `pgpass_from` (optional) lets you point at a client-side env var that supplies the Postgres password (useful for CI).
- Dry-run must open the tunnel, do its work inside a transaction, then close the tunnel with the DB untouched.

## Detailed design notes

1. **TOML surface.** Add parsing for `[deploy.<name>]` (probably in a small new `deploy.rs` or alongside `pgweb.toml` handling in `config.rs` / `push.rs`). Support the four documented keys with the stated defaults. Error nicely if `--target foo` is given but `[deploy.foo]` is missing (list the defined ones).

2. **SSH session lifecycle.** Use `openssh::Session::connect` (or equivalent) with the ssh target. Request a local forward. The forwarded address becomes the effective `db_host`/`db_port` for that run only. Make sure the session is closed even on error paths (use `scopeguard` or `Drop` + `?`).

3. **Migrate support.** The same `--target` flag must work for `migrate apply`. The migration runner already takes a `Db` connection; the target logic should produce an equivalent connection string or `tokio_postgres` config.

4. **Error UX (non-negotiable per spec).**
   - Bad SSH key / no agent / known_hosts mismatch → surface the underlying `openssh` / ssh error verbatim.
   - Tunnel succeeded but remote `127.0.0.1:5432` is not listening (Postgres not up, wrong db_host in the target section) → clear message suggesting `ssh <target> 'docker ps'` or equivalent.
   - Unknown target name → list the ones that *are* defined in the user's `pgweb.toml`.

5. **Dry-run interaction.** `--dry-run --target prod` must behave exactly like local dry-run except that the tunnel is alive for the duration. The rollback + "nothing written" guarantee must still hold.

6. **CI / smoke story.** The existing tier-3 Docker E2E and `scripts/smoke-cli.sh` cannot assume a real VPS. The original plan called for:
   - Integration test (gated `--ignored`): spawn an sshd container + a second PG container, exercise a full push over the tunnel.
   - Optional `smoke-deploy.sh` (opt-in, not run in normal `test-all.sh`) for real-remote validation.

7. **Documentation updates required.**
   - `DEPLOYMENT.md` — replace the manual-tunnel walkthrough with the `--target` flow as the primary story (keep a "manual tunnel still works" note for the transition period).
   - `APP-DEVELOPER-GUIDE.md` deploy section.
   - `README.md` quickstart / production notes.
   - `ROADMAP.md` and `OVERVIEW.md` — mark F.2 ✅ once shipped.
   - Example `pgweb.toml` in the todo template or docs.

## Research tasks for the implementing session

1. Re-read the exact locked design and error cases in `docs/internal/sessions/session_5.md:80-123` and the handoff prompt at the bottom of that file.
2. Confirm the current `openssh` crate API and how to request a local forward + get the local port (the design assumes this is straightforward).
3. Decide where the `[deploy.*]` parsing lives (new module vs. existing config handling) and how it interacts with the existing `pgweb.toml` parser.
4. Prototype the happy-path tunnel + push against a local sshd + local PG to validate the forward + connection string story before touching real remote infra.
5. Map the exact changes needed in `push.rs`, `migrate.rs`, and the CLI argument parsing.
6. Design the testcontainers-based integration test (sshd sidecar + separate PG container is the classic pattern).
7. Decide on the name and location of the opt-in remote smoke script (`smoke-deploy.sh` was the name floated).

## Constraints & invariants to respect

- Extension ↔ CLI remain strictly decoupled (this is 100% CLI work; the extension never knows about targets or SSH).
- No new secret types or credential stores — reuse ssh agent + libpq mechanisms exactly as specified.
- `pg-web push --dry-run --target` must be safe for CI (tunnel + rollback, no data left on the remote).
- Companion-app / testing discipline: the feature must be exercised in tier-3 (containerized) and have a documented path for real-remote validation. `examples/todo/` does not need new app-level behavior, but the deploy flow must be demonstrated.
- Respect the "one container" and "BYO Postgres is a non-goal" realities — this feature is for people who already have a Postgres host they can SSH to.

## Acceptance criteria

1. `pg-web push --target prod` (with a correctly configured `[deploy.prod]` section) successfully deploys an app to a real remote Postgres (or a faithful container simulation) and lands a row in `pgweb.deployments` with sensible `from_host`.
2. The same target works for `pg-web migrate apply --target prod`.
3. `--dry-run --target prod` opens the tunnel, performs all validation/reconciliation, reports `[dry-run]` output, closes the tunnel, and leaves the remote database unchanged.
4. Clear, actionable errors for the three main failure classes listed in the spec (SSH auth, remote PG unreachable, unknown target name).
5. Unit coverage for TOML deserialization of `[deploy.<name>]` (including defaults and partial sections).
6. Tier-3 / integration test (gated) that brings up sshd + a target Postgres container, exercises a full `--target` push over the tunnel, and asserts success + ledger row.
7. Optional but recommended: an opt-in `scripts/smoke-deploy.sh` (or equivalent) that the maintainer can point at a real Hetzner / VPS target.
8. All user-facing docs (`DEPLOYMENT.md`, `APP-DEVELOPER-GUIDE.md`, `README.md`, `ROADMAP.md`, `OVERVIEW.md`) are updated so the primary remote-deploy story is `--target` (manual tunnel is now the "you can still do it the old way" note).
9. `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and relevant test tiers pass.
10. The implementation matches the design locked in session_5.md (openssh crate, local-forward model, zero new secret types, support for both push and migrate).

## Open questions

1. **Connection model confirmation (from the S5 handoff prompt).** Does the SSH tunnel terminate before the CLI opens its libpq connection (classic `-L` local port forward that the existing `db` module then connects to), or does the openssh session forward the entire protocol stream? The `-L` model was preferred in the handoff because it is simpler and inherits libpq's normal retry behavior. Confirm with the maintainer and implement the chosen one.
2. **How much of the target section is required vs. defaulted** in practice (the spec says "all below have sensible defaults").
3. **Whether `pgpass_from` (the env-var indirection for the Postgres password) is still the right UX** or whether we should just document normal `~/.pgpass` + `PGPASSWORD` and drop the extra key.
4. **Error message wording for the "remote PG not listening" case** — the spec suggests suggesting `ssh <target> 'docker ps'`. Is that still the most helpful hint now that many people use the in-image CLI or compose?

---

*Write the code, make the containerized test pass, then validate against a real remote target (the original reason for the deferral). Once this lands, the "remote deploys just work" story in the docs can finally be true rather than aspirational.*
