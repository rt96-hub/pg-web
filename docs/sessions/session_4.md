# Session 4 — Closeout toward v0.1 (M1.4)

**Status:** planned, not started.
**Theme:** ship the remaining Phase-1 feature surface + the two explicit near-term deferrals from Session 3 (browser live-reload, content-hash asset filenames), polish push for prod deploy, and cut the v0.1 release.

Unlike M1.2 (which was user-facing DX), M1.4 is mostly about **finishing the frame**: the feature table in `APP-DEVELOPER-GUIDE.md` still has rows — turn them into "shipped". Not all items are equal weight. A realistic cut: Session 4 ships the user-facing items (A–F) and the deferred Session-3 near-term work (G–H); **Session 5 picks up the ops/release track** (I–K) if scope runs long.

---

## State of the project at Session 4 start

### What's working today (end of Session 3 / M1.2)

- Extension serves HTTP on `:8080` from a background worker. Routes + templates + handlers + settings + assets live in `pgweb.*`. Framework schema owned by install SQL; `pg-web push` owns app data.
- Request flow: page route lookup (with `:id` captures) → handler via PL/pgSQL wrapper (structured SQLSTATE/MESSAGE/...) → Tera render OR raw text → HTTP response. Asset fallback (GET-only) for `public/*`. `_404` fallback for the rest.
- `pgweb.settings.env` toggles dev-mode rich error pages (PGWEB_E001–E999 typed catalog) vs prod-mode generic 500.
- CLI: `init`, `up`, `down`, `dev`, `push`, `migrate apply`. Push is fully reconciling (adds + updates + deletes + drops stale handlers) and validates handler signatures + template parse pre-DB. Dev watcher mirrors the Vite architecture (200ms debounce + Blake3 content-hash dedupe).
- Five test tiers green: 49 pgrx (18 pg_test + 28 pure + 3 asset lookups), 2 HTTP smoke, 95 CLI unit/integration, 7 tier-3 docker E2E, 1 tier-4 black-box CLI smoke.

### What a developer still has to work around

- **Browser doesn't auto-refresh on save.** `pg-web dev` pushes changes to the DB in ~300 ms; the user still has to hit F5. (Deferred explicitly; now in scope for M1.4.)
- **Assets revalidate on every page view.** ETag caching returns 304-no-body, so it's bytes-cheap, but still a round trip. True long-cache (`immutable`) needs content-hash filenames. (Deferred explicitly; now in scope.)
- **Assets over 2 MiB are refused at push time.** Hero images / videos / PDFs → CDN. (M1.4 adds `pg_largeobject` streaming to lift the cap.)
- **Secrets live in `pgweb.toml` or `$DATABASE_URL`**, not in the DB. `pg-web env set KEY=VAL` with a GUC bridge is the v0.1 answer.
- **`pg-web check` doesn't exist** — users learn about layout errors / template errors / handler-signature errors at `pg-web push` time, which needs a running DB. A pre-push offline validator is the DX win.
- **Form validation** bubbles up as a generic 500 on `check_violation`. Users deserve an inline error fragment.
- **Image isn't published.** `pgweb/postgres:latest` is local-build-only. Release pipeline + Docker Hub publish is the v0.1 gate.

### Invariants that stay put

Sessions 1, 2, 3 locked these; Session 4 MUST NOT revisit them:

1. Directory-as-route, filename-as-method layout (`docs/APP-LAYOUT.md`).
2. Handler contract `(req json) RETURNS <json|text>`; `req` keys `body`/`query`/`method`/`path`/`path_params`.
3. Dispatch via `template_path` nullability.
4. `pgweb.pages__*(json) RETURNS json|text` is the reserved push-managed handler namespace.
5. Extension ↔ CLI talk only via framework-table upserts. No shared crate, no RPC.
6. One HTTP request = one SPI transaction.
7. `pgweb.settings` is the runtime-config source of truth; `pg-web push` syncs from `pgweb.toml`.
8. Push is fully reconciling and validates handler signatures + template parse pre-DB.
9. Target PG 15/16/17; async only inside the BGW; HTTPS out-of-process (Caddy).

### Entry point for this session

Read this file, then skim `docs/ROADMAP.md` § Milestone 1.4 and the 2026-04-20 / 2026-04-22 decision log entries. If any of the open questions below are unresolved at the top of the session, close them with the user first. Then start on Component A — it's tiny and unblocks B.

---

## Prerequisites (shipped through Session 3)

- `pg-web up` / `down` / `dev` / `push` / `migrate apply` / `init` all stable.
- Rich dev error page (typed catalog) + production generic 500.
- Dynamic routes, `req.path_params`.
- Static assets from `public/*` (BYTEA, 2 MiB cap, ETag revalidation).
- `pgweb.settings.env` + `pgweb.toml [server] env` plumbing.
- Push reconciles + validates handlers + validates templates.

The boring parts of the framework are done. Session 4 is about polish + the two deferred Vite-parity items.

---

## Work breakdown

### A. `pgweb.html_escape(text) → text`

**Extension:** install SQL adds the function. Thin wrapper that escapes `& < > " '` — same five chars as `errors::escape_html`. Safe to call on NULL (returns NULL).

**Why here:** unblocks Component B (inline form validation). Users who write raw-text handlers (`.sql` with no `.html` sibling) currently have no safe way to interpolate user input. Giving them `pgweb.html_escape(req->'body'->>'x')` closes that gap.

**Tests:** `#[pg_test]` — NULL pass-through, escapes all five chars, idempotent on already-escaped input (should double-escape; that's the contract).

### B. User-facing form validation UX

**Pattern (documented in `docs/APP-DEVELOPER-GUIDE.md`):** a PL/pgSQL handler catches `check_violation` / `unique_violation` from its own `BEGIN ... EXCEPTION WHEN …` block and returns an HTMX-friendly error fragment. The handler dispatches based on whether the SQL raised, returning JSON for the happy template OR text for the error swap (`hx-swap-oob`).

**Work:** add the pattern to `TUTORIAL.md` and to the demo. Extend the demo's POST /todos handler to catch `check_violation` (empty title) and return an inline error message instead of the current 500.

**Tests:** tier 3 demo — empty title → 200 + inline error fragment (not a 500 or dev page).

### C. `pg-web env set/unset/list`

**CLI:** a new `env` subcommand with three actions.

- `pg-web env set KEY=VAL` — upserts into `pgweb.settings` (same table that already carries `env`). Keys are free-form strings; values are text.
- `pg-web env unset KEY` — deletes the row.
- `pg-web env list` — `SELECT key, value FROM pgweb.settings` to stdout.

**Extension side:** nothing new. Handlers read via SPI: `SELECT value FROM pgweb.settings WHERE key = 'STRIPE_KEY'`. A sugar helper `pgweb.setting(key)` is worth shipping in install SQL.

**Why not GUCs?** Session-3 decision log: portability > microsecond-per-request. Settings are already in a table; extending the same table avoids introducing a second config mechanism.

**Tests:** CLI unit (parse `KEY=VAL` syntax), `#[pg_test]` for `pgweb.setting('foo')` returning NULL on miss.

### D. `pg-web init --template <name>` + scaffolded `README.md`

**Work:**
- New flag on `init`: `--template todo-demo` copies `examples/todo-demo/` (renamed from `examples/demo/`) into the new app dir. Other bundled templates: maybe `--template blog` (not required; flag the extension point).
- Plain `init` keeps today's minimal scaffold but gains a `README.md` with the "next steps" commands + pointer to `docs/APP-DEVELOPER-GUIDE.md`.

**Tests:** CLI unit — `init --template todo-demo` produces the expected file tree; `init` (no flag) produces the minimal scaffold + README.

### E. `pg-web check` — offline validator

**CLI:** new subcommand that runs everything push validates, **without a DB connection**. Walks `pages/` and `migrations/`, runs:

- `paths::scan` (layout + reserved-stem rules).
- Tera parse on every `.html`.
- SQL parse on every `.sql` + every `migrations/*.sql` via the `pg_query` crate (parse-only) OR `BEGIN; ROLLBACK;` against a throwaway Postgres — decide at implementation.
- Return-type consistency across `.html`+`.sql` sibling modes (same check push does; reimplement offline).
- Migration filename ordering + ledger drift (vs. `--url` if provided).

Output: grouped diagnostics, file + line where applicable. Non-zero exit on any finding. Hook candidate for pre-commit + CI.

**Tests:** CLI integration — fixtures under `tests/check/` with deliberate errors; assert exit code + diagnostic strings.

### F. Push polished for prod deploy

Split into three sub-components because remote-deploy ergonomics (F.2) and bundling the CLI with the image (F.3) are substantial on their own. F.1 can ship independently; F.2 and F.3 pair well but aren't strictly sequential.

#### F.1 — Local-push polish

- `pg-web push --dry-run` — report the planned changes (routes / templates / handlers to upsert, delete, drop) without touching the DB. Implementation: start the transaction, do the usual walk, print the summary, ROLLBACK instead of COMMIT.
- `pg-web push` with `migrate apply` wired: detect pending migrations and refuse (or, behind `--with-migrate`, run them first). Prevents the footgun where a dev pushes handler code that references a column that doesn't exist yet.
- `pgweb.deployments` ledger (new table: `id BIGSERIAL`, `pushed_at TIMESTAMPTZ`, `from_host TEXT`, `file_count INT`, `migrations_applied INT`). Every successful push inserts a row. For ops visibility — answer "when did we last deploy, from where?" via one query.

**Tests:** CLI integration + tier 3 — `--dry-run` against demo reports nothing committed; `--with-migrate` on an unmigrated app applies + pushes in one call; `pgweb.deployments` gains a row per push.

#### F.2 — Automated remote deploy via SSH tunnel — **user-flagged important**

**The problem:** `pg-web push` today is local-only. Pushing to a remote production stack means either (a) exposing the VPS's `:5432` to the internet (bad), or (b) manually opening an SSH tunnel before each push (tedious and error-prone in CI). Users have been asking "why can't I just point at the server?" — this is the answer that doesn't compromise security.

**The design:**

- `pgweb.toml` gains a `[deploy.<name>]` table with SSH target info:
  ```toml
  [deploy.prod]
  ssh = "deploy@app.example.com"     # anything `ssh` accepts; ssh_config aliases work
  # Everything below has sensible defaults — declare only what differs.
  # ssh_port  = 22
  # db_host   = "127.0.0.1"          # on the remote
  # db_port   = 5432                 # on the remote
  # pgpass_from = "PGWEB_PROD_PASSWORD"   # env var on the dev machine
  ```
- `pg-web push --target prod` reads that table, opens an SSH session via the [`openssh`](https://docs.rs/openssh) crate (thin wrapper over the system `ssh` binary — uses user's existing `~/.ssh/config`, ssh-agent, known_hosts transparently), sets up a local port-forward `127.0.0.1:<ephemeral> → remote:127.0.0.1:5432`, runs the normal push transaction against that forwarded port, tears down the tunnel on exit.
- `pg-web migrate apply --target <name>` gets the same treatment automatically — same tunnel lifecycle, different CLI verb.

**What's exposed:** SSH (port 22) on the server, which is already open for admin. Postgres stays bound to remote `127.0.0.1`; **nothing** listens on the public internet for Postgres.

**Credentials story:**
- SSH auth: user's existing keys via ssh-agent / `~/.ssh/id_*` / deploy keys (in CI, via [webfactory/ssh-agent](https://github.com/webfactory/ssh-agent) or similar).
- PG auth: libpq's standard `~/.pgpass` / `$PGPASSWORD` / env-var mechanisms. No pg-web-specific credential store.
- **Zero new secret types.** Nothing pg-web invents around auth.

**Implementation choice:** `openssh` crate over `russh`. Reasons: (1) user's SSH config / keys Just Work without re-implementing any of it; (2) ProxyJump + jump-host patterns inherited for free; (3) known_hosts checking handled correctly; (4) one less thing for us to audit for security bugs. Tradeoff: requires the system `ssh` binary, which is a non-issue on Linux/macOS/WSL and now ships by default on Windows 10+.

**Error paths to test:**
- SSH auth failure (wrong key, no ssh-agent, known_hosts mismatch) — surface the underlying ssh error verbatim, don't swallow.
- Remote PG not reachable on `127.0.0.1:5432` inside the server (container down, firewall internal to the box, etc.) — clear message pointing at `ssh <target> 'docker ps'` or similar diagnostic.
- Deploy target name not in `pgweb.toml` — list the defined targets, error.
- Dry-run with `--target` — tunnel opens, dry-run runs, tunnel closes, nothing touched. Useful for CI smoke.

**Tests:**
- Unit: TOML deserialization of `[deploy.<name>]` variants + defaults.
- Integration: spawn a sshd in a container (via testcontainers), tunnel to a second container's PG, push, assert. Harder to set up but real; alternatively mock the openssh::Session.
- Tier 3: gated under `--ignored` (needs sshd infrastructure), similar to docker_e2e.

#### F.3 — CLI bundled in the postgres image

**The need:** once you've SSHed to a VPS (manually or via F.2), you sometimes want to run the CLI **on** the server — e.g., an in-compose-network `docker compose exec postgres pg-web push` to bypass even the `127.0.0.1:5432` publish. Requires the CLI binary to already be on the server.

**The work:**
- Modify the `pgweb/postgres` image's Dockerfile to also `cargo install --path crates/pg_web_cli` into the image. Shipped binary goes to `/usr/local/bin/pg-web` inside the image.
- Users can then `docker exec postgres-1 pg-web push --dir /app` (with `/app` bind-mounted or pre-copied).
- Alternative: publish a standalone `pgweb/cli:<version>` image (tiny Alpine + the CLI binary). Users compose it in with `network_mode: "service:postgres"` or `--network <project>_default` and it can talk to postgres on the internal network.

**When you use which:**
- `pgweb/postgres:latest` with CLI baked in — one-container convenience for small deployments.
- `pgweb/cli:<ver>` separate — cleaner for larger ops, lets the CLI upgrade independently of Postgres.

**Tests:** tier 3 addition — `docker compose exec postgres pg-web --version` succeeds.

**Interplay with F.2:** F.2 and F.3 are orthogonal. F.2 lets you deploy from a laptop without SSHing in; F.3 means that if you DO SSH in (or CI drops you in), the CLI is waiting. Many deployments end up using both over their lifecycle.

### G. Browser live-reload push (WS or SSE) — **deferred from Session 3**

**Design decision to resolve at session start:** WebSocket or Server-Sent Events?

- **SSE** — simpler protocol, one-way server→client, works over HTTP/1.1, no framing library. Good enough for "file changed, reload" pings.
- **WS** — bidirectional, but we only need server→client, so WS is overkill. Extra framing library (`tokio-tungstenite`?).

Leaning: **SSE**. Resolve with the user before coding.

**Work:**
- Extension: new route `/_pgweb/livereload` that returns `text/event-stream` with `keep-alive` pings and a `reload` event when a dev-push lands.
- Notification from push to extension: the simplest is `NOTIFY pgweb_livereload` from `pg-web dev`'s post-push hook. The extension's worker `LISTEN`s on that channel (via `LISTEN/NOTIFY` over SPI) and fans out to connected SSE clients.
- Template injection: `pg-web dev` (not `push`) opt-in injects `<script>` for the live-reload client into every rendered HTML page. Only in dev mode. Gated by `pgweb.settings.env = 'development'`. **Opt-out flag** (`pg-web dev --no-livereload`) so the feature never breaks a heavy-JS app.

**Tests:** tier 3 — connect to `/_pgweb/livereload`, trigger a push, assert the client receives a `reload` event.

### H. Content-hash asset filenames + HTML rewrite

**Work:**
- Push-time transform: when a template `<link href="/styles.css">` or `<img src="/logo.png">` references an asset that exists in `public/`, rewrite the href to `/styles.abc123.css` (fingerprinted).
- `pgweb.assets.path` stores the fingerprinted form; original path is derivable but not stored (or stored as a denormalized convenience column — decide at impl).
- Cache-Control upgrades to `public, max-age=31536000, immutable` for fingerprinted URLs in prod mode.
- Dev mode: skip the rewrite; keep `/styles.css` + `no-cache`. So the dev loop stays fast (no rewrite cost on every push) and the user sees the unhashed URL.

**Open questions (resolve before coding):**
- Rewrite in `pg-web push` (offline) or in the extension at render time (template post-processor)?
  - Offline is cheaper (once per push, not per render) but requires push to parse HTML.
  - Inline is simpler (just a Tera filter / post-processor) but runs every request in prod.
  - **Leaning:** offline at push time. Use a simple HTML tokenizer (no full DOM) — just rewrite `href`/`src` attribute values that match an asset path.
- What about dynamic references (`<img src="{{ user.avatar }}">`)? Can't rewrite at push time. Skip these — document that fingerprinting only works for literal URLs in templates.

**Tests:** CLI unit — template-rewrite helper with literal + dynamic references. Tier 3 — push demo + assert `/styles.css` in template becomes `/styles.<hash>.css`; HTTP GET that URL returns `Cache-Control: public, max-age=31536000, immutable`.

### I. `pg_largeobject` streaming for assets ≥ 1 MiB

**Work:**
- New table `pgweb.assets_large(path PK, oid OID, content_type, etag)`.
- Push: if file size ≥ `[assets] large_cutoff_bytes` in `pgweb.toml` (default 1 MiB), `lo_create` a new OID, stream bytes in, store the OID. Reconcile drops the row AND runs `lo_unlink`.
- Extension: router's asset lookup tries `pgweb.assets` first (fast), then `pgweb.assets_large`. For large: open the large object in the SPI transaction and stream bytes out via `lo_read` with a bounded buffer (64 KiB). `ServeOutcome::StreamingAsset` variant?

**Open question:** Axum + pgrx streaming — can the SPI transaction stay open while Axum streams the body? The existing `BackgroundWorker::transaction` pattern commits at the end of the closure. Might need to buffer in memory anyway (up to a higher cap, say 20 MiB?) as a middle ground.

**Risk flag:** this is the heaviest item. If scope is tight, cap assets at 5–10 MiB (still BYTEA) and defer true streaming. The user shipped one-tier BYTEA knowing the trade.

### J. Release pipeline + Docker Hub publish

**Work:**
- CI workflow: on tag `v0.1.0`, run full `scripts/test-all.sh` against PG 15/16/17, build the image, push to `pgweb/postgres:latest` + `pgweb/postgres:0.1`.
- Version bumps in `Cargo.toml` (`[workspace.package] version = "0.1.0"`).
- `CHANGELOG.md` with Session 1–4 highlights.

**Tests:** dry-run the CI workflow on a throwaway tag.

### K. Docs pass

**Work:** read every file in `docs/` with fresh eyes against what actually shipped. Target files:

- `APP-DEVELOPER-GUIDE.md` — full refresh using the demo as the source of truth.
- `TUTORIAL.md` — a new chapter covering `pg-web dev` + dynamic routes + static assets + live reload (if G ships).
- `OVERVIEW.md` — update the 30-second picture.
- `ROADMAP.md` — Phase 1 items move to "shipped" column.

---

## Testing plan (consolidated)

| Tier | What gains coverage                                                                     |
|------|------------------------------------------------------------------------------------------|
| 1 — `#[pg_test]`   | `pgweb.html_escape` edge cases. `pgweb.setting(key)` NULL-on-miss. `pgweb.deployments` ledger row inserted per push. Large-object round-trip if I ships. |
| 2a — HTTP smoke    | `/_pgweb/livereload` keep-alive frame if G ships. Fingerprinted asset URL if H ships.     |
| 2b — CLI           | `env set/unset/list`. `init --template`. `check` (unit + fixture). `push --dry-run`. F.2 target-TOML deserialization. |
| 3 — Docker E2E     | Live-reload end-to-end (push → SSE event). Fingerprinted asset with `Cache-Control: immutable`. Large asset (>2 MiB) if I ships. Validation demo (empty form → inline error). **F.2 SSH-tunneled push**: spawn sshd in a container, deploy key, tunnel, push against a second container's PG, assert. **F.3 bundled CLI**: `docker compose exec postgres pg-web --version`. |
| 4 — CLI smoke      | Extend `smoke-cli.sh`: `env set` → `env list` visible. `check` fails on a broken fixture. Live-reload section if G ships. SSH-tunnel push section if F.2 ships (harder — needs sshd scaffolding; could split into `smoke-deploy.sh`). |

Target: 180+ Rust tests + smoke sections tracking every new component.

---

## Things deliberately NOT in Session 4

- **Declarative schema diffing** (`pg-web migrate create`) — Phase 2.5.
- **Auth / sessions / RLS bridge** — Phase 2.
- **Async job queue** — Phase 3.
- **In-browser dev dashboard** — Phase 4.
- **App testing framework** (`pg-web test`) — parking lot / Phase 5+.
- **Full streaming assets with no in-memory buffer** — even I bounds the buffer; true zero-copy would require rethinking the BGW's SPI-transaction lifecycle.

---

## Open design questions to resolve at session start

1. **SSE vs WS for live-reload?** Leaning SSE (simpler, one-way is sufficient).
2. **Content-hash rewrite: offline (push time) or inline (render time)?** Leaning offline.
3. **Large asset streaming vs buffered BYTEA up to N MiB?** Leaning buffered (5-10 MiB cap) with true streaming flagged as follow-up. Full streaming is a rabbit hole vs. the v0.1 release gate.
4. **`pg-web check` — pg_query crate vs throwaway Postgres for SQL parse?** `pg_query` adds a C dep; throwaway PG needs a running PG. Leaning pg_query — `check` should be offline by its nature.
5. **`pg-web init --template` — bundled templates or fetched from a registry?** v0.1 ships bundled (`examples/` tree). Registry is post-v1.
6. **F.2 SSH layer: `openssh` crate (system ssh wrapper) vs `russh` (pure-Rust)?** Leaning `openssh` — free inheritance of user's ssh config, keys, agent, known_hosts, ProxyJump. Requires system ssh, which is a non-issue on Linux/macOS/WSL and ships with Windows 10+.
7. **F.3 distribution: bake CLI into `pgweb/postgres:latest` OR ship `pgweb/cli:<ver>` separately OR both?** Leaning both — baked for single-container-convenience, separate for independent upgrade cycles.
8. **Tagged release strategy — `0.1.0` now or after a couple of Phase-2 items land?** Discuss with user. Defensible v0.1 = "what's in Session 4 at session end."

---

## Suggested order

Components can interleave more than Session 3 did (fewer cross-dependencies). One reasonable sequence:

1. **A** — `pgweb.html_escape()`. Tiny, unblocks B.
2. **B** — Form-validation UX + demo extension. Immediately raises the demo's quality.
3. **C** — `env set/unset/list`. Small CLI addition.
4. **D** — `init --template` + scaffolded README. DX win.
5. **E** — `pg-web check`. Bigger scope; multiple validators.
6. **F.1** — Push polish: `--dry-run`, `--with-migrate`, `pgweb.deployments` ledger.
7. **F.2** — SSH-tunneled `pg-web push --target <name>`. **User-flagged important** — the thing that makes deploy actually production-friendly without exposing `:5432`.
8. **F.3** — CLI bundled into `pgweb/postgres:latest` (and/or standalone `pgweb/cli:<ver>` image). Pairs with F.2 for the "SSH-into-server and run it there" path.
9. **G** — Browser live-reload. The deferred Session-3 priority.
10. **H** — Content-hash asset filenames. The other deferred priority.
11. **I** — Large-asset tier (if scope allows).
12. **J** — Release pipeline. Last, so the release reflects everything prior.
13. **K** — Docs pass. Can overlap with J.

Each followed by a stop-and-check at phase boundaries — same workflow as Session 3.

**F.2 is a legitimate Session 4 highlight** — it's what users have been asking about, and until it ships, `pg-web push` is effectively local-only or requires the user to hold an SSH tunnel open manually. Consider reordering to F.1 → F.2 → F.3 → A → B → … if the remote-deploy story matters more than the user-facing polish for this session.

If scope runs long, **punt I (large assets) to Session 5 + cut the release tag until M1.4 is complete**. Don't ship an incomplete release pipeline.

---

## Recap — what shipped

(To be filled in at session close, mirroring `session_3.md`'s recap table format.)
