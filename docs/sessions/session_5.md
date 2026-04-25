# Session 5 — Deferred polish + remote deploy (0.2.0 target)

**Status:** in progress. Component L shipped.
**Theme:** close the `v0.2.0` scope. Everything the 0.1 release explicitly deferred, plus a small batch of post-0.1 fixes the user discovered during validation. Biggest user-facing win: **F.2** (`pg-web push --target <name>` — SSH-tunneled remote deploy), which removes the last "you need to hold an `ssh -L` tunnel open by hand" friction from the prod workflow.

## Shipping log (running)

| Component | Commit | Notes |
|---|---|---|
| L. Push retry on serialization conflict | `ed55de4` | `retry::with_retry` wrapper, sibling-pusher diag via `pg_stat_activity`, `db::connect` helper tags every CLI connection so the diag can extract OS PID + host |
| (docs) | `8ff0c5a` | ROADMAP backup story — operational, code-only, source-tree-in-DB tracks |
| F.3. CLI bundled in image | `7eaf724` | `cargo build --release -p pg_web_cli` in builder stage, binary at `/usr/local/bin/pg-web` in runtime; `.dockerignore` un-ignores `examples/todo/` so `include_dir!` works in the image bake |
| F.2 (deferred) | — | Skipped this session — implementation cost is large for a feature only validatable end-to-end against a real remote target. Picks back up when remote infra is available. |
| H. Content-hash asset filenames | `62c8cd7` | Push-time fingerprinting + template rewrite when `[server].env = "production"`; router emits `Cache-Control: immutable` for fingerprinted GETs. Dev mode unchanged. Pure-Rust string-replace rewrite (no regex dep); double-quoted attribute values only. |
| I. Asset BYTEA cap-raise (2 MiB → 20 MiB) | (this commit) | Cap-raise variant of the planned pg_largeobject feature — covers virtually every practical asset without shipping `lo_read`-backed streaming. CHECK constraint + CLI cap match. True streaming for >20 MiB assets remains Phase 2+ work. |

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

Full commit table (rows match `git log --oneline main` from the L commit through the v0.2.0 close-out):

| # | Commit | Component | Summary |
|---|---|---|---|
| 1 | `ed55de4` | L — Push retry + diag | `retry::with_retry` wrapper, sibling-pusher diag via `pg_stat_activity`, `db::connect` helper tags every CLI connection |
| 2 | `8ff0c5a` | (docs) | ROADMAP — backup/export/source-tree-in-DB tracks split across Phase 4 |
| 3 | `7eaf724` | F.3 — CLI in image | Builder stage builds `pg-web`; runtime copies to `/usr/local/bin/pg-web`; `.dockerignore` un-ignores `examples/todo/` |
| 4 | `62c8cd7` | H — Content-hash assets | Push-time fingerprint + template rewrite when prod; router emits immutable Cache-Control |
| 5 | `db6fb0d` | I — Asset cap-raise | BYTEA `CHECK` 2 MiB → 20 MiB; CLI cap matches; true streaming explicitly Phase 2+ |
| 6 | (this commit) | N — Docs + v0.2.0 release | CHANGELOG, version bump, OVERVIEW + ROADMAP + APP-DEVELOPER-GUIDE refresh, session_5 close-out |

## Retrospective

### What went well

- **Four components shipped** (L, F.3, H, I) plus the deferred deferral (F.2). Original plan flagged I as risk-prone with "may fall back to buffered cap"; the cap-raise variant landed cleanly in <30 lines of code + 2 tests, leaving true streaming explicitly framed as Phase 2+ work rather than a vague "TODO."
- **Zero invariant changes.** Handler contract, directory-as-route, dispatch via `template_path` nullability, push-managed handler namespace — every locked spec stayed put. v0.1 apps run on v0.2 unchanged; the only user-visible change is `pg-web push` against env=production now rewriting templates.
- **Test growth tracked features 1:1.** pgrx 70→72 (+2 H matchers), CLI 124→143 (+19 retry/db/H/I tests), tier-3 9→13 (+4: concurrent push, in-image push, fingerprinted Cache-Control, 5 MiB round-trip). No "we'll test it later."
- **Diagnostic UX upgrade with L.** Beyond the retry mechanic, the session shipped a pattern: `application_name` tagging on every CLI connection is a debugging multiplier well beyond push retry — `pg_stat_activity` now shows OS PID + host for every active pg-web client. The diag formatter the retry uses is reusable for any future "who else is touching this DB" question.
- **F.2 deferral was deliberate, not silent.** Discussed cost-to-validate with the user up front; documented the punt rationale in the shipping log + the OVERVIEW; left the original session_5 design intact so Session 6 picks up cleanly.

### What went wrong

- **Test-all.sh exit-code masking via `tee`.** Twice in this session I piped `bash scripts/test-all.sh 2>&1 | tee /tmp/log` and saw the wrapping shell report exit 0 from `tee`'s success — masking real script failures. First instance hid a `cargo: command not found` in the script (cargo not on the non-interactive PATH); second masked an extension-build failure at the `examples/todo/` `include_dir!` step. Lesson: when running scripts via background subagents, capture exit code into a separate variable (`cmd > log; echo EXIT=$?`) instead of relying on the pipeline's tail-out exit. Updated my Bash invocations partway through; documenting here so the pattern doesn't sneak back in.
- **Stale `pg-web up` container on `:8080` shadowed the dev pgrx PG.** Tier 2a HTTP smoke failed with the user's `my-todos-postgres-1` container serving stale demo state. The script does DROP/CREATE EXTENSION + restart pg_ctl, but the BGW silently failed to bind `:8080` because the leftover container already had it. The retry-and-diag pattern from L would have caught this if applied here too — flagging "the container holding `:8080` had `from_host = some-container-id` in `pg_stat_activity`" would have been the perfect tell.
- **rustc 1.95 ICE on a `let mut` miss.** While writing the F.3 docker test, an "internal compiler error" panic during `mir_borrowck` masked a real "needed `let mut`" diagnostic on `migrate_res` and `push_res`. Cost ~10 min of "is this another `[DatumWithOid; 2]`-shape ICE?" before reading the function carefully. Lesson: when the compiler reports an ICE in `mir_borrowck`, the next thing to suspect is "I'm calling a `&mut self` method on a non-mut binding" — the borrow-check diagnostic that should have been printed got swallowed by the panic instead.
- **Image rebuild required after schema.rs and after http.rs changes.** Both H (http.rs) and I (schema.rs) needed `bash scripts/build-image.sh` before tier 3 would see the new behavior. Forgot once and got a confused tier-3 result. Fixed in process, but a `make tier3` target that auto-rebuilds would close this loop. Punted to a future "test infra polish" task.

### Lessons compiled

1. **Don't pipe to tee in background subagent runs.** Capture exit code explicitly: `bash <script> > log 2>&1; echo EXIT=$?; tail log`. The Monitor tool's `grep --line-buffered` filter misses the `EXIT=` line if the pipe consumes its own tail.
2. **Treat `mir_borrowck` ICEs as a hint to look for `let mut` errors first.** rustc 1.95 has at least one path where the borrow-check diagnostic gets swallowed by panic instead of printed.
3. **`application_name` is a free debugging multiplier.** Once you tag every CLI-initiated connection with verb + OS PID + host, every "who is doing what" question reduces to a `pg_stat_activity` query. Should bake this into Phase 2's auth/session work too.
4. **Cap-raise > true streaming for v0.2 I.** When `lo_read` + Axum streaming compatibility had real friction (the planned BackgroundWorker SPI tx lifetime story is non-trivial), shipping a 20 MiB BYTEA cap covered the practical use case in <30 lines and let the design space for true streaming stay open. The session_5 plan called this out as the fallback; correctly reading "fallback acceptable" was the right call.

### Metrics

- **5 commits** on Session 5: L, ROADMAP-backups, F.3, H, I, and N. (Plus N is this commit.)
- **Test growth:** pgrx +2 (70→72), CLI +19 (124→143), tier-3 +4 (9→13), tier-4 unchanged (19 sections).
- **230 Rust tests + 19-section black-box smoke**, all five tiers green at session close.
- **Docker image:** rebuilt twice (schema.rs change for I, extension `.so` change for H).
- **Binary size impact:** F.3 added ~10 MiB to the image (the `pg-web` binary, debug-stripped). Acceptable for the convenience-of-`docker exec` payoff.

## Deferred to Session 6

- **F.2 — SSH-tunneled `pg-web push --target <name>`.** Implementation surface is well-scoped (per the original session_5 design); validation needs a real remote target. Picks up when remote infra is available.
- **True `pg_largeobject` streaming.** v0.2 ships only the BYTEA cap-raise. `lo_read`-backed streaming for assets >20 MiB needs the SPI-tx-during-Axum-stream design discussion the original plan flagged.
- **Docker-build-aware test infra.** A `make tier3` (or equivalent) that auto-rebuilds the image when `schema.rs` or `crates/pg_web_ext/src/*.rs` changed would catch the "forgot to rebuild" error class.
- **Tier-2a port-shadowing diagnostic.** When `:8080` is held by something other than the pgrx dev PG, the smoke test should bail with a "stop the shadowing process" message instead of returning a confusing 200 with someone else's body.

## Gotchas hit this session

1. **`tee` masks pipeline exit codes.** When piping a `set -e` script through `tee`, the wrapping shell reports `tee`'s exit (always 0) instead of the script's. Captured as a Session 5 lesson; document if it happens twice more, then DEVELOPER-GUIDE pitfall #16.
2. **Stale `pg-web up` container shadowed `:8080`.** A leftover container from a prior dev session held `:8080`, the dev PG's BGW silently failed to bind, and tier 2a's smoke saw the container's response instead. Easy to spot with `docker ps` + `ss -tlnp | grep 8080`; harder when the symptom is "the test asserts the wrong body."
3. **rustc 1.95 ICE wraps `let mut` errors.** At least once, a "needed `let mut`" borrow-check error came back as an internal compiler panic in `mir_borrowck` instead of a normal diagnostic. If you see ICEs in `mir_borrowck` while writing test code, suspect missing `let mut`.
4. **`.dockerignore` excludes `examples/`.** The CLI's `init.rs` `include_dir!` bundles `examples/todo/` at compile time, but `.dockerignore` excluded all of `examples/`. Build failed with a proc-macro panic until I un-ignored `examples/todo/` specifically.
5. **F.3 + L composability.** The `application_name` tag introduced for L's diag also surfaces in the F.3 in-image case: pushes from inside the container show `host=<container_id>` in `pg_stat_activity` (vs. dev box's hostname). Used this as the F.3 test's discriminator. Nice cross-feature win.

## Handoff prompt for Session 6

(Paste into a fresh Claude Code session to restart cleanly.)

---

> I'm resuming pg-web work on Session 6. v0.2.0 shipped at the end of Session 5; feature surface is L (push retry + sibling-pusher diag), F.3 (CLI in image), H (content-hash assets + immutable cache), I-cap-raise (BYTEA 2 MiB → 20 MiB). All five tiers green at 230 Rust + 19 smoke sections.
>
> **This session's scope:**
>
> - **F.2 — SSH-tunneled `pg-web push --target <name>`** — the user-flagged remote-deploy story, deferred from Session 5 because validation needs a real remote target. Original design in `docs/sessions/session_5.md` § F.2 is intact (locked to the `openssh` crate; `[deploy.<name>]` in `pgweb.toml`). I have remote infra available now.
> - **(stretch) True `pg_largeobject` streaming** — open question is the BackgroundWorker SPI-tx lifetime during Axum streaming. Buffered up to 20 MiB ships in v0.2 (Component I); >20 MiB still requires Phase 2 design work.
> - **Phase 2 kickoff candidates** — auth/sessions/RLS bridge, app-level realtime subscriptions reusing the channel-aware ListenRouter from Session 4 G. Pick what to start once F.2 lands.
>
> **Workspace lives in WSL2 Ubuntu-22.04 at `/home/pgweb/pg-web`, owned by user `pgweb`.** From Git Bash on Windows reach it via `wsl -d Ubuntu-22.04 -u pgweb -- bash -c '...'` with `MSYS_NO_PATHCONV=1` prefix and `\$?` escape for exit-code capture.
>
> **Read these first, in this order:**
>
> 1. `docs/OVERVIEW.md` — 30-second picture at v0.2.0.
> 2. `docs/sessions/session_5.md` — full Session 5 recap including the "Retrospective" + "Deferred to Session 6" sections.
> 3. `docs/sessions/session_5_validation.md` — the validation playbook for L / F.3 / H / I, useful for reproducing v0.2.0's expected behavior on a fresh box.
> 4. `CHANGELOG.md` — `v0.2.0` release notes.
> 5. `docs/ROADMAP.md` § Feature matrix at the top.
>
> **Workflow conventions** (user preferences, from memory):
>
> - No Claude trailer on commits. Conventional-style subjects (`feat(cli):`, `fix:`, `docs(dev-guide):`).
> - User-flagged in Session 5: keep moving without sign-off if tests are green and docs are updated; deliver expected-behaviors at session close.
> - Companion-app coverage per feature. `examples/todo/` is the acceptance gate.
> - Bias toward *why* in inline comments, not *what*. Well-named symbols document themselves.
> - `pgweb.pages__*(json) RETURNS json|text` is the reserved push-managed namespace.
> - Test-all.sh runs five tiers; all mandatory. Auto-stops the pgrx dev PG between tiers 3 and 4.
> - Docker image bakes install SQL — run `bash scripts/build-image.sh` after any `schema.rs` change before tier 3/4. **Also rebuild after extension `*.rs` changes** (the `.so` is baked in too).
>
> **First task for this session:** decide F.2's connection-routing model — does the SSH tunnel terminate before push opens its libpq connection (`-L` style, locally-bound ephemeral port that push connects to), or does the openssh crate session forward the entire libpq protocol stream? Both work; the `-L` model is simpler and inherits libpq's normal connection retry. Resolve with the user, then implement.

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
