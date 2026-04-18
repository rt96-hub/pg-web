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
│   ├── DEVELOPER-GUIDE.md        # For framework maintainers (us)
│   ├── APP-DEVELOPER-GUIDE.md    # For framework users (future)
│   ├── TESTING.md                # Three-tier test strategy + companion app
│   └── DEPLOYMENT.md             # Caddy + Docker + VPS
└── examples/
    └── demo/                     # Companion app — end-to-end acceptance test (TBD)
```

## Architectural invariants — DO NOT VIOLATE

1. **No raw C bindings.** Everything that touches Postgres internals goes through `pgrx`. If pgrx doesn't expose a thing, file an upstream issue or propose a wrapper crate — don't reach for raw FFI.
2. **HTTPS is out-of-process.** The extension binds plain HTTP on :8080. Caddy terminates TLS in front. Never introduce `rustls` / `openssl` into the extension for termination.
3. **Extension ↔ CLI are strictly decoupled.** The extension has zero filesystem code. The CLI has zero HTTP-handler logic. They synchronize state *only* by upserting rows into framework-owned tables (`pg_web._pg_web_routes`, `pg_web._pg_web_templates`, `pg_web._pg_web_assets_*`). No shared library crate between them beyond dumb types.
4. **One HTTP request = one SPI transaction.** Every request opens an SPI transaction on entry. It commits on a clean 2xx response or rolls back on any error. Never leak transactions; never handle a request across multiple transactions.
5. **Zero network hop inside the extension.** The worker never opens a TCP `postgres://` connection back to Postgres — always SPI. Using `libpq` from the extension is a correctness bug.
6. **Target Postgres versions are 15, 16, 17 only.** Features must work on all three. No pg18-only dependencies (the feature flag is intentionally absent from `Cargo.toml`).
7. **Async only in the background worker.** Don't introduce `tokio` code paths inside `#[pg_extern]` functions — those run on Postgres's synchronous backend threads and will deadlock.

## Coding practices

- **pgrx-first patterns.** Use `Spi::run`, `Spi::get_one`, `BackgroundWorkerBuilder`, `#[pg_extern]`, `#[pg_test]`. Read pgrx docs before inventing patterns.
- **Tests next to code.** Extension tests use `#[pg_test]` and run via `cargo pgrx test pg17`. Don't mock Postgres — run against the real compiled instances under `~/.pgrx/`.
- **No premature abstraction.** Three duplicated lines beats the wrong trait. The extension is small; keep modules flat until patterns genuinely emerge.
- **Error handling on the request path.** No `unwrap()` / `expect()` in the HTTP handler. Fatal SQL exceptions → generic 500 in prod, rich debug page in dev (mode from the `pg_web.env` GUC).
- **Every feature ships with a companion-app flow.** If a feature isn't exercised in `examples/demo/`, it isn't done. See `docs/TESTING.md`.
- **Phase discipline.** We are in **Phase 1** (Synchronous Core). Do not add Phase 2+ features (auth/RLS, job queues, dashboard) into Phase 1 code paths. Stage them properly.

## Commit style

- Short imperative title (≤72 chars). Body optional; if present, explain WHY, not WHAT.
- Conventional Commits prefixes (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`) — use them.
- **Do not** add a `Co-Authored-By: Claude ...` trailer on commits in this repo. The human owner has opted out.

## Session rituals

- **Before writing non-trivial code** — read the relevant `docs/*.md` section first. If your change touches an invariant above, raise a flag and wait for human confirmation.
- **Before finishing a feature** — confirm the demo app in `examples/demo/` exercises it. If not, add the flow.
- **Before committing** — run `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and the relevant `cargo pgrx test pgXX` for whichever Postgres version is current.

## Current phase & milestones

**Phase 1 — Synchronous Core.** Broken into four milestones (see `docs/ROADMAP.md`):

1. **M1.1 Walking Skeleton** — extension + CLI + Docker Compose + `pg-web push` produces a working `GET /` → Tera-from-DB render.
2. **M1.2 Interactive Dev Loop** — `pg-web dev` with hot reload; dynamic routes; dev error page.
3. **M1.3 First Real Demo (todo list)** — `examples/demo/` as a CRUD todo app. Raw-SQL migrations via `pg-web migrate apply`. Exercises HTMX forms, validation, static assets.
4. **M1.4 Closeout** — secrets, prod polish, release pipeline.

**Confirmed decisions (2026-04-17):**
- Schema migrations: **raw SQL only in Phase 1**. Declarative diffing (`migrate create`) deferred to later phase.
- Walking-skeleton milestone **includes CLI + Docker Compose** — not extension-only.
- First real demo app = **todo list** (not hello-world). Hello-world is only the proof-of-life at M1.1.
- HTTP library: **Axum** (thin-shell pattern; see `docs/ARCHITECTURE.md` § "Inside the extension" for rationale). We use a fallback handler + Tower middleware; our own modules own the framework logic so dropping to raw Hyper later stays a one-day job.

## Open architectural decisions

See `docs/ARCHITECTURE.md` and `docs/ROADMAP.md` for current defaults.

- Asset size cutoff (BYTEA vs pg_largeobject): 1 MiB default — not benchmarked.
- Framework schema name: `pg_web` vs `_pg_web` — tentatively `pg_web`.

When one of these is resolved, update this file and the corresponding doc in the same commit.
