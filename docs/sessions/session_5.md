# Session 5 — Deferred polish + remote deploy (0.2.0 target)

**Status:** in progress. Component L shipped.
**Theme:** close the `v0.2.0` scope. Everything the 0.1 release explicitly deferred, plus a small batch of post-0.1 fixes the user discovered during validation. Biggest user-facing win: **F.2** (`pg-web push --target <name>` — SSH-tunneled remote deploy), which removes the last "you need to hold an `ssh -L` tunnel open by hand" friction from the prod workflow.

## Shipping log (running)

| Component | Commit | Notes |
|---|---|---|
| L. Push retry on serialization conflict | `ed55de4` | `retry::with_retry` wrapper, sibling-pusher diag via `pg_stat_activity`, `db::connect` helper tags every CLI connection so the diag can extract OS PID + host |
| (docs) | `8ff0c5a` | ROADMAP backup story — operational, code-only, source-tree-in-DB tracks |
| F.3. CLI bundled in image | (this commit) | `cargo build --release -p pg_web_cli` in builder stage, binary at `/usr/local/bin/pg-web` in runtime; `.dockerignore` un-ignores `examples/todo/` so `include_dir!` works in the image bake |
| F.2 (deferred) | — | Skipped this session — implementation cost is large for a feature only validatable end-to-end against a real remote target. Picks back up when remote infra is available. |

Unlike Session 4 — which had to finish a feature surface on a deadline — Session 5 is polish + robustness. The `v0.1.0` app works; nothing here is blocking new use cases, every component is upgrading an existing capability or removing a known rough edge.

---

## State of the project at Session 5 start

### What's working today (end of Session 4 / M1.4 / `v0.1.0`)

- Full Phase-1 feature surface. See `CHANGELOG.md` for the grouped-by-milestone release notes and `docs/OVERVIEW.md` for the current-state summary.
- All five test tiers green: 70 pgrx `#[pg_test]`, 2 HTTP smoke, 124 CLI unit+integration, 9 docker E2E, 19 smoke sections.
- `pg-web dev` auto-reloads connected browser tabs via SSE. Channel-aware LISTEN router is in place (`crates/pg_web_ext/src/listen_router.rs`), so Phase-2 app-level subscriptions plug in without a rewrite.
- Release artifacts (CHANGELOG, CI workflow, release workflow) in place. `git tag v0.1.0` would fire the release pipeline.

### Invariants that stay put (Sessions 1-4 locked, Session 5 MUST NOT revisit)

1. Directory-as-route, filename-as-method layout (`docs/APP-LAYOUT.md`).
2. Handler contract `(req json) RETURNS <json|text>`; `req` keys `body`/`query`/`method`/`path`/`path_params`.
3. Dispatch via `template_path` nullability.
4. `pgweb.pages__*(json) RETURNS json|text` is the reserved push-managed handler namespace.
5. Extension ↔ CLI talk only via framework-table upserts. No shared crate, no RPC.
6. One HTTP request = one SPI transaction.
7. `pgweb.settings` is the runtime-config source of truth; `pg-web push` syncs from `pgweb.toml`.
8. Push is fully reconciling and validates handler signatures + template parse pre-DB.
9. Target PG 15/16/17; async only inside the BGW; HTTPS out-of-process (Caddy).
10. One LISTEN session per BGW (dev-mode only); N SSE subscribers share it in-memory via `broadcast::Sender`. Phase-2 app-subscription channels reuse the same session.

### Entry point for this session

Read `docs/sessions/session_4.md` § Retrospective + § Deferred to Session 5 for the punt rationale. Read this file. If any open questions below are unresolved at the top of the session, close them with the user first. Then start on Component L (smallest, unblocks nothing but removes a real annoyance the user hit during Session 4 validation).

---

## Prerequisites (shipped through Session 4)

- `pg-web init` / `up` / `down` / `dev` / `push` / `migrate apply` / `env` / `check` all stable.
- Rich dev error page (typed catalog) + production generic 500.
- Dynamic routes, `req.path_params`.
- Static assets from `public/*` (BYTEA, 2 MiB cap, ETag revalidation).
- `pgweb.settings.env` + `pgweb.toml [server] env` plumbing.
- Push reconciles + validates handlers + validates templates.
- `pgweb.deployments` append-only ops ledger.
- Browser live-reload via SSE + channel-aware router.

The usable surface is done. Session 5 upgrades it.

---

## Work breakdown

### L. Push retry on serialization conflict

**Priority:** highest, smallest. Real bug the user hit in Session 4 validation.

When two pushers run against the same DB simultaneously (e.g., forgotten `pg-web dev` + new `pg-web dev`, or two devs on a shared staging DB), the second one's `CREATE OR REPLACE FUNCTION` call against `pg_proc` can hit `ERROR: tuple concurrently updated` (PG's MVCC race report for DDL). Push's whole transaction aborts; the second watcher's save is lost.

**Work:**
- In `push::push_with_options`, wrap the transaction body in a retry loop: on `SerializationFailure` or the specific `tuple concurrently updated` message, sleep a short random jitter (10-100ms) and retry the whole transaction. Cap at 3 attempts, then bubble the error up with context ("push retried N times against concurrent DDL; try stopping any other `pg-web dev` processes").
- Matches PG's standard advice for serializable-level conflict handling. Low risk because push is already one-big-transaction — retrying is safe (no side effects in the host that survive the rollback).

**Tests:**
- Unit-ish: extract the retry helper as a pure function taking a closure; unit-test it with an injectable error sequence (first call returns TupleConcurrentlyUpdated, second succeeds → helper returns success).
- Tier 3: spawn two concurrent `push` calls against the same container, assert both eventually commit (second retries around the conflict), both deployments rows land.

### F.2. SSH-tunneled `pg-web push --target <name>` — **user-flagged important**

**Priority:** high. This is the single feature most likely to turn "pg-web works locally" into "pg-web works in production without ceremony."

**The problem today:** pushing to a remote production stack means either (a) exposing `:5432` to the internet (bad), (b) opening an SSH tunnel manually before each push (tedious + easy to forget in CI), or (c) SSHing into the VPS and running `pg-web push` there (which requires F.3, next).

**The design (locked during Session 4 planning, commit `f6a809d`):**

`pgweb.toml` gains a `[deploy.<name>]` section:

```toml
[deploy.prod]
ssh = "deploy@app.example.com"         # anything ssh accepts; ssh_config aliases work
# All below have sensible defaults. Declare only what differs.
# ssh_port    = 22
# db_host     = "127.0.0.1"              # on the remote
# db_port     = 5432                     # on the remote
# pgpass_from = "PGWEB_PROD_PASSWORD"    # env var on the dev machine
```

`pg-web push --target prod` reads `[deploy.prod]`, opens an SSH session via the [`openssh`](https://docs.rs/openssh) crate (thin wrapper over the system `ssh` binary — inherits `~/.ssh/config`, ssh-agent, known_hosts, ProxyJump for free), sets up a local port-forward `127.0.0.1:<ephemeral> → remote:127.0.0.1:5432`, runs the normal push transaction against that forwarded port, tears down the tunnel on exit.

`pg-web migrate apply --target <name>` gets the same treatment.

**What's exposed on the server:** port 22 (already open for admin). Postgres stays bound to `127.0.0.1`; **nothing** listens on the public internet for PG.

**Credentials story:**
- SSH auth: user's existing keys via ssh-agent / `~/.ssh/id_*` / CI deploy keys (via [`webfactory/ssh-agent`](https://github.com/webfactory/ssh-agent) or similar).
- PG auth: libpq's `~/.pgpass` / `$PGPASSWORD` / env-var mechanisms. No pg-web-specific credential store.
- **Zero new secret types.** Nothing pg-web invents around auth.

**Implementation choice: `openssh` over `russh`.** Reasons already locked: (1) user's existing ssh config / keys work unchanged, (2) ProxyJump / known_hosts / known_hosts_fingerprints all inherited, (3) less auth code for us to own. Tradeoff: requires system `ssh` binary — non-issue on Linux / macOS / WSL; ships with Windows 10+.

**Error paths to test:**
- SSH auth failure (wrong key, no agent, known_hosts mismatch) → surface the underlying ssh error verbatim, don't swallow.
- Remote PG not reachable on `127.0.0.1:5432` inside the server → clear message suggesting `ssh <target> 'docker ps'` or similar diagnostic.
- Deploy target name not in `pgweb.toml` → error lists the defined targets.
- `--dry-run --target <name>` → tunnel opens, dry-run runs (rolls back), tunnel closes, DB untouched. Useful for CI smoke.

**Tests:**
- Unit: TOML deserialization of `[deploy.<name>]` variants + defaults.
- Integration: spawn `sshd` in a container (via testcontainers or docker-in-docker), tunnel to a second container's PG, push, assert. Gated under `--ignored` like other tier-3 tests.
- Tier 4 smoke: opt-in additional script (`smoke-deploy.sh`) — the main smoke script shouldn't require sshd infrastructure.

### F.3. CLI bundled in `pgweb/postgres:latest` (+ standalone `pgweb/cli:<ver>`)

**Priority:** high, pairs naturally with F.2.

**The need:** once you've SSHed to a VPS (whether manually or via F.2's tunnel), you sometimes want to run the CLI **on** the server. `docker compose exec postgres pg-web push --dir /app` bypasses even `127.0.0.1:5432` publish — everything stays inside the compose network.

**The work:**
- Modify the `pgweb/postgres` image Dockerfile to `cargo install --path crates/pg_web_cli` during build. Binary lands at `/usr/local/bin/pg-web` inside the image.
- Optionally ship a standalone `pgweb/cli:<version>` image (tiny Alpine + the CLI binary). Users compose it in with `network_mode: "service:postgres"` so it talks to Postgres on the internal network without publishing anything.

**Which to use when:**
- `pgweb/postgres:latest` with CLI baked in — single-container convenience for small deployments.
- `pgweb/cli:<ver>` separate — cleaner for larger ops, lets the CLI version-bump independently of Postgres.

**Tests:** tier 3 — `docker compose exec postgres pg-web --version` succeeds; add a full push flow that runs from inside the container (bind-mount the demo into `/app`).

**Interplay with F.2:** orthogonal. F.2 makes remote push work from your laptop without SSHing in. F.3 means that if you DO SSH in (or CI drops you in), the CLI is already there. Many deployments end up using both over their lifecycle.

### H. Content-hash asset filenames + HTML rewrite

**Priority:** medium. Quality-of-life in prod; v0.1's ETag revalidation works but costs one 304 round-trip per asset per page load.

**Work:**
- Push-time: compute the content hash of every asset under `public/` (Blake3; first 8-10 chars is plenty of collision space for realistic app sizes).
- In prod mode only, rewrite `<link href="/styles.css">` and `<img src="/logo.png">` in templates to `<link href="/styles.<hash>.css">` / etc. Store the asset in `pgweb.assets` under the hashed path.
- Router recognizes the hashed-filename pattern (regex on `\.<hex>\.ext` suffix) → emits `Cache-Control: public, max-age=31536000, immutable` instead of revalidate-every-time headers.
- Dev mode: skip the rewrite. Unhashed URLs + `no-cache` keep the iteration loop fast.

**Open questions (resolve at session start):**
- HTML-rewrite implementation: regex-based attribute scan, simple HTML tokenizer, or full html5ever parser? **Leaning:** regex `(href|src)="([^"]+)"` for v0.2; document caveats (won't handle single-quoted attrs, won't rewrite dynamic `{{ var }}` URLs). Same tradeoff the Session 4 plan called out.
- Where to store the original → hashed mapping? The asset table's primary key is `path`. Two candidates: store only the hashed path (simpler; original doesn't exist in DB), or keep a `canonical_path` column for future diagnostics (more complex).  **Leaning:** simpler — only hashed path lives in the DB; template rewrite is the single source of truth.
- Dynamic `<img src="{{ user.avatar }}">` — can't rewrite at push time. Skip; document that fingerprinting works only for literal URLs. (Phase-2 could add a Tera filter `| fingerprint` for explicit opt-in.)

**Tests:** CLI unit for the template-rewrite pure function; tier 3 — push demo in prod mode, assert `/styles.css` in template became `/styles.<hash>.css`, GET that URL returns `Cache-Control: public, max-age=31536000, immutable`.

### I. `pg_largeobject` streaming for assets ≥ 1 MiB — **risk-flagged**

**Priority:** medium-low. Real capability gap (BYTEA 2 MiB cap forces CDN for images/PDFs), but implementation is the heaviest item here.

**Work:**
- New table `pgweb.assets_large(path PK, oid OID, content_type, etag)`.
- Push: if file size ≥ `[assets] large_cutoff_bytes` in `pgweb.toml` (default 1 MiB), `lo_create` a new OID, stream bytes in, store the OID. Reconcile drops the row AND runs `lo_unlink`.
- Extension router's asset lookup tries `pgweb.assets` first (fast BYTEA path), then `pgweb.assets_large`. For large: open the large object in the SPI transaction, stream bytes out via `lo_read` with a bounded buffer (64 KiB chunks).
- New `ServeOutcome::StreamingAsset` variant so http.rs can hand a streaming body to Axum instead of buffering the whole thing first.

**Open question (resolve at session start):** Axum + pgrx streaming compatibility. The existing `BackgroundWorker::transaction` pattern commits at the end of the closure. Holding the SPI tx open across an async Axum stream is technically possible but non-obvious. **Fallback:** buffer in memory up to a higher cap (say 20 MiB) and document the remaining ceiling. Worst case, we ship I as a cap-raise without true streaming.

**Risk flag:** this is the scope-risk item. If we hit the Axum-plus-SPI-stream friction and the buffer-in-memory fallback isn't satisfying, punt to Session 6 / Phase 2.

### M. Single-dev guard (stretch)

**Priority:** low, nice-to-have. From the Session 4 push-concurrency race.

`pg-web dev` could acquire a file lock at `$APP_DIR/.pgweb/dev.lock` on start and refuse to run if another dev already holds it. Cleanup on SIGTERM/SIGINT. Simple `std::fs::File` + `fcntl(F_SETLK)` on Unix. **Skip if L (retry) proves sufficient in practice** — L is the robustness backstop either way.

### N. Docs / release closeout for v0.2

**Priority:** done at session close.

- `CHANGELOG.md` entry for `[0.2.0]` with the component list.
- Cargo.toml bump `0.1.0` → `0.2.0`.
- ROADMAP.md Phase-1 section marked fully done; Phase-2 moves into focus.
- `docs/sessions/session_5.md` recap table (mirror session_4.md's format) at session close.
- Update `docs/APP-DEVELOPER-GUIDE.md` deploy section with the `pg-web push --target` flow + the "CLI is in the image" instructions.

---

## Testing plan (consolidated)

| Tier | What gains coverage |
|------|----------------------|
| 1 — `#[pg_test]` | `pgweb.assets_large` large-object round-trip (if I ships). Hashed-path matcher in router (if H ships). |
| 2a — HTTP smoke | Streaming-asset response (if I ships). |
| 2b — CLI unit / integration | `[deploy.<name>]` TOML deserialization + defaults. Template-rewrite pure function for H. Push retry helper's error-injection unit test. |
| 3 — Docker E2E | `pg-web push --target <name>` against an sshd-in-container + PG-in-container setup (F.2). CLI bundled in image: `docker compose exec postgres pg-web --version` + full push-from-inside flow (F.3). Concurrent-push retry: two pushes race, both commit (L). Fingerprinted asset URL served with `Cache-Control: immutable` (H). >2 MiB asset uploaded + retrieved (I). |
| 4 — CLI smoke | Extend `smoke-cli.sh` with push-retry scenario (run two pushes simultaneously via `&`, assert both succeed). SSH-deploy scenario gated into a separate `smoke-deploy.sh` (needs sshd infra, don't force it on every smoke run). |

**Target:** 220+ Rust tests + smoke sections tracking every new component. (Session 4 closed at 203 Rust tests.)

---

## Open design questions to resolve at session start

1. **Push retry: retry whole transaction, or specific statements?** Leaning whole-transaction retry with jitter; statement-level retry breaks atomicity.
2. **SSH deploy: `openssh` crate (system ssh wrapper) or `russh` (pure Rust)?** Locked during Session 4 to `openssh` — reconfirm.
3. **CLI-in-image: bake into `pgweb/postgres:latest` + ship `pgweb/cli:<ver>` separately, or only one?** Leaning both; costs are low.
4. **Content-hash rewrite implementation (regex vs tokenizer vs full parser)?** Leaning regex for v0.2.
5. **Streaming assets vs buffered BYTEA up to N MiB?** Leaning buffered 20 MiB cap as the practical v0.2 floor, with true streaming flagged for Phase 2+.
6. **Single-dev guard as its own component, or roll into L's context?** Leaning optional stretch; L's retry is the real robustness fix.
7. **v0.2.0 tag timing — before or after Phase 2 begins?** Discuss. Phase 2 (auth + realtime) is a bigger theme shift; cutting `0.2.0` before that lets users depend on the `0.2.x` line without worrying about Phase-2 churn.

---

## Suggested order

1. **L** — Push retry on serialization conflict. Small, closes a real user-observed bug, unblocks nothing so doing it first de-risks later work if we accidentally race again during testing.
2. **F.2** — SSH-tunneled push. User-flagged important; the "production feels friction-free" win.
3. **F.3** — CLI bundled in image. Pairs naturally with F.2.
4. **H** — Content-hash assets. Polish; clean scope.
5. **I** — pg_largeobject streaming. Scope-risk; punt-candidate if Axum-SPI streaming friction is real.
6. **M** — Single-dev guard. Stretch; skip if L turned out to be sufficient.
7. **N** — Docs + v0.2.0 release artifacts.

Each followed by a stop-and-check at phase boundaries — same workflow as Session 3 and Session 4.

**If scope runs long,** punt I to Session 6 (Phase 2 territory) and cut `0.2.0` with L + F.2 + F.3 + H. That's still a substantial release: "remote deploys work without ceremony, and prod asset caching is Vite-parity."

---

## Recap — what shipped

(To be filled in at session close, mirroring `session_4.md`'s recap table + retrospective.)

---

## Handoff prompt for Session 5

Paste the block below into a fresh Claude Code session to pick up work cleanly.

---

> I'm resuming pg-web work on Session 5. The project's at `v0.1.0` — full Phase-1 feature surface, all five test tiers green, released via `CHANGELOG.md`. This session's scope is the deferred polish: **L (push retry on serialization conflict — post-Session-4 discovery), F.2 (SSH-tunneled `pg-web push --target`), F.3 (CLI baked into the `pgweb/postgres:latest` image), H (content-hash asset filenames), I (`pg_largeobject` streaming for assets > 2 MiB — risk-flagged).** Target is a `v0.2.0` release at session close.
>
> **Workspace lives in WSL2 Ubuntu-22.04 at `/home/pgweb/pg-web`, owned by the `pgweb` user.** Code does NOT live under my primary working directory on Windows. From Git Bash / Claude Code on Windows reach it via:
>
>     wsl -d Ubuntu-22.04 -u pgweb -- bash -c 'cd /home/pgweb/pg-web && <command>'
>
> For Read / Write / Edit tools, the UNC prefix is:
>
>     \\wsl.localhost\Ubuntu-22.04\home\pgweb\pg-web\...
>
> Always `MSYS_NO_PATHCONV=1` before `wsl` commands that carry Linux absolute paths. Always escape `\$?` when reading exit codes across the Git Bash → WSL boundary (pitfall #14). `cargo` is not in the pgweb user's non-interactive-shell PATH — use `/home/pgweb/.cargo/bin/cargo` (pitfall #12).
>
> **Read these first, in this order:**
>
> 1. `docs/OVERVIEW.md` — 30-second picture of where `v0.1.0` sits.
> 2. `docs/sessions/session_4.md` — full Session 4 recap including the "Retrospective" + "Deferred to Session 5" sections. Read the Retrospective before touching any file — it captures what I learned about testing gaps, Git Bash quirks, and dev-process cleanup.
> 3. `docs/sessions/session_5.md` — this file. Full work breakdown for Components L, F.2, F.3, H, I + the open design questions.
> 4. `docs/ROADMAP.md` § Feature matrix at the top — comprehensive view of where every feature sits.
> 5. `CHANGELOG.md` — `v0.1.0` release notes. Understand what DID ship.
>
> **Workflow conventions** (user preferences, from memory):
>
> - No Claude trailer on commits. Conventional-style subjects (`feat(cli):`, `fix:`, `docs(dev-guide):`).
> - Stop-and-check at phase boundaries. Land one component cleanly, verify all tiers green, summarize for the user, wait for sign-off before the next.
> - Companion-app coverage per feature. `examples/todo/` is the acceptance gate.
> - Bias toward *why* in inline comments, not *what*. Well-named symbols document themselves.
> - `pgweb.pages__*(json) RETURNS json|text` is the reserved push-managed namespace — user helpers must use a different pattern.
> - Test-all.sh runs five tiers; all mandatory. Auto-stops the pgrx dev PG between tiers 3 and 4.
> - Docker image has the install SQL baked in — run `bash scripts/build-image.sh` after any `schema.rs` change before tier 3/4.
>
> **First task for this session:** read `docs/sessions/session_5.md` + § Open design questions top to bottom. Surface any unresolved with the user. Then start on Component L (push retry on serialization conflict — smallest, closes a real bug, doesn't block anything).
