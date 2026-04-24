# CLAUDE.md — pg-web project guidance

This file is auto-loaded by Claude Code (and any other Anthropic agent) when working in this repo. It is the north-star prompt for all AI collaborators on pg-web. Always read `docs/` for full spec; this file is the invariants + coding practices digest.

## Mission (memorize this, do not drift from it)

Build a **"Zero-Proxy" PostgreSQL Full-Stack Framework** — turn PostgreSQL into a self-contained web server by embedding an async HTTP listener inside a Postgres background worker. No external Node/Python/Go backend. SQL is the business logic. HTML + HTMX is the UI. Tera is the template glue. Rust (via pgrx) is the engine. The developer writes `.sql` + `.html` files; they never compile Rust.

## Repo layout

```
pg-web/
├── CLAUDE.md                     # This file — agent guidance
├── Cargo.toml                    # Workspace root (resolver=2, panic=unwind)
├── crates/
│   ├── pg_web_ext/               # pgrx extension (cdylib) — HTTP, SPI, templating
│   └── pg_web_cli/               # `pg-web` binary — filesystem, migrations, deploy
├── docs/                         # Authoritative spec — source of truth
│   ├── VISION.md
│   ├── ARCHITECTURE.md
│   ├── ROADMAP.md
│   ├── OVERVIEW.md               # Current-state snapshot (read first)
│   ├── APP-LAYOUT.md             # Canonical: directory/file/handler conventions
│   ├── APP-DEVELOPER-GUIDE.md    # Narrative reference for app developers
│   ├── TUTORIAL.md               # Step-by-step walkthrough building a todo app
│   ├── DEVELOPER-GUIDE.md        # For framework maintainers (us)
│   ├── TESTING.md                # Four-tier test strategy + feature matrix
│   ├── DEPLOYMENT.md             # Caddy + Docker + VPS
│   └── sessions/                 # Per-session plans + recaps
└── examples/
    └── demo/                     # Companion todo app — tier 3 E2E target
```

## Architectural invariants — DO NOT VIOLATE

1. **No raw C bindings.** Everything that touches Postgres internals goes through `pgrx`. If pgrx doesn't expose a thing, file an upstream issue or propose a wrapper crate — don't reach for raw FFI.
2. **HTTPS is out-of-process.** The extension binds plain HTTP on :8080. Caddy terminates TLS in front. Never introduce `rustls` / `openssl` into the extension for termination.
3. **Extension ↔ CLI are strictly decoupled.** The extension has zero filesystem code. The CLI has zero HTTP-handler logic. They synchronize state *only* by upserting rows into framework-owned tables (`pgweb.routes`, `pgweb.templates`, `pgweb.assets_*`). No shared library crate between them beyond dumb types.
4. **One HTTP request = one SPI transaction.** Every request opens an SPI transaction on entry. It commits on a clean 2xx response or rolls back on any error. Never leak transactions; never handle a request across multiple transactions.
5. **Zero network hop inside the extension.** The worker never opens a TCP `postgres://` connection back to Postgres — always SPI. Using `libpq` from the extension is a correctness bug.
6. **Target Postgres versions are 15, 16, 17 only.** Features must work on all three. No pg18-only dependencies (the feature flag is intentionally absent from `Cargo.toml`).
7. **Async only in the background worker.** Don't introduce `tokio` code paths inside `#[pg_extern]` functions — those run on Postgres's synchronous backend threads and will deadlock.

## Coding practices

- **pgrx-first patterns.** Use `Spi::run`, `Spi::get_one`, `BackgroundWorkerBuilder`, `#[pg_extern]`, `#[pg_test]`. Read pgrx docs before inventing patterns.
- **Tests next to code.** Extension tests use `#[pg_test]` and run via `cargo pgrx test pg17`. Don't mock Postgres — run against the real compiled instances under `~/.pgrx/`.
- **No premature abstraction.** Three duplicated lines beats the wrong trait. The extension is small; keep modules flat until patterns genuinely emerge.
- **Error handling on the request path.** No `unwrap()` / `expect()` in the HTTP handler. Fatal SQL exceptions → generic 500 in prod, rich debug page in dev (mode from the `pgweb.env` GUC).
- **Every feature ships with a companion-app flow.** If a feature isn't exercised in `examples/todo/`, it isn't done. See `docs/TESTING.md`.
- **Phase discipline.** We are in **Phase 1** (Synchronous Core). Do not add Phase 2+ features (auth/RLS, job queues, dashboard) into Phase 1 code paths. Stage them properly.

## Commit style

- Short imperative title (≤72 chars). Body optional; if present, explain WHY, not WHAT.
- Conventional Commits prefixes (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`) — use them.
- **Do not** add a `Co-Authored-By: Claude ...` trailer on commits in this repo. The human owner has opted out.

## Session rituals

- **Before writing non-trivial code** — read the relevant `docs/*.md` section first. If your change touches an invariant above, raise a flag and wait for human confirmation.
- **Before finishing a feature** — confirm the demo app in `examples/todo/` exercises it. If not, add the flow.
- **Before committing** — run `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and the relevant `cargo pgrx test pgXX` for whichever Postgres version is current.

## Current phase & milestones

**Phase 1 — Synchronous Core.** All four milestones shipped; `v0.1.0` tagged 2026-04-24 (see `CHANGELOG.md`).

1. **M1.1 Walking Skeleton** ✅ shipped Session 1 — extension + CLI + Docker Compose + `pg-web push` produces a working `GET /` → Tera-from-DB render.
2. **M1.3 Interactive Contracts + Real Demo** ✅ shipped Session 2 — `(req json)` handler contract, directory-as-route layout, `_404` fallback, `examples/todo/` todo app, tier 3 Docker E2E.
3. **M1.2 Interactive Dev Loop** ✅ shipped Session 3 — `pg-web up`/`down`/`dev` (file watcher + hot reload), dynamic routes (`[id]` captures), dev error page, static asset serving.
4. **M1.4 Closeout** ✅ shipped Session 4 — `pgweb.html_escape()`, inline-error validation UX, `pg-web env` + `pgweb.setting()`, `pg-web init --template`, `pg-web check` offline validator, push `--dry-run` + `--with-migrate` + `pgweb.deployments` ledger, browser live-reload via SSE + channel-aware LISTEN router, CHANGELOG + CI workflow.

Session 5 picks up the deferred polish: H (content-hash assets), F.2 (SSH-tunneled remote push), F.3 (CLI in image), I (pg_largeobject streaming).

(Session 2 did M1.3 before M1.2 because the interactive contracts had to settle before the watcher would know what to re-sync.)

**Confirmed decisions (see `docs/ROADMAP.md` § Decision log for full rationale):**

*2026-04-17:*
- Raw-SQL migrations only in Phase 1; declarative diffing → Phase 2.5.
- M1.1 ships CLI + Docker Compose together.
- Axum as thin-shell HTTP layer over our own router.
- Framework schema is `pgweb` (no underscore — `pg_` prefix is reserved).
- Dedicated `pgweb` WSL user for dev (not root — `initdb` refuses root).

*2026-04-18:*
- Directory-as-route, filename-as-method app layout. Spec in `docs/APP-LAYOUT.md`.
- Uniform handler contract: `(req json) RETURNS <json|text>` with `req = { body, query, method, path }`.
- Dispatch via `pgweb.routes.template_path` nullability — non-NULL = Tera render, NULL = raw text.
- `_404` reserved filename stem for fallback routes; router does longest-prefix-match on lookup miss.
- CLI owns full dev loop; `pg-web up/down/dev` in M1.2, published image + `init --template` in M1.4.
- Tier 3 Docker E2E is mandatory (hard fail if Docker/image missing) — no silent skips.

## Open architectural decisions

See `docs/ARCHITECTURE.md` and `docs/ROADMAP.md` for current defaults.

- Asset size cutoff (BYTEA vs pg_largeobject): 1 MiB default — not benchmarked.
- Dynamic-route pattern matching algorithm (naïve scan vs trie): TBD in Session 3 when `[id]` captures land.
- Hash-based vs ETag-only asset caching: TBD in Session 3.

When one of these is resolved, update this file and the corresponding doc in the same commit.
