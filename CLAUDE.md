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
├── docs/                         # Authoritative spec — source of truth (public docs at top level)
│   ├── OVERVIEW.md               # Current-state snapshot (read first)
│   ├── VISION.md
│   ├── APP-DEVELOPER-GUIDE.md    # Narrative reference for app developers
│   ├── TUTORIAL.md               # Step-by-step walkthrough building a todo app
│   ├── APP-LAYOUT.md             # Canonical: directory/file/handler conventions
│   ├── DEPLOYMENT.md             # Caddy + Docker + VPS
│   ├── ROADMAP.md
│   ├── ARCHITECTURE.md
│   ├── TESTING.md
│   ├── BENCHMARKS.md             # 015 performance measurements + harness usage (re-run for hot-path changes)
│   └── internal/                 # Maintainer-only: DEVELOPER-GUIDE, HANDOFF, sessions/, prompts/
├── prompts/                      # Active work orders (015 benchmark harness + concurrency design is the canonical example)
├── CLAUDE.md                     # Agent north-star (this file) + internal/ copies of maintainer docs
└── examples/
    └── todo/                     # Companion todo app — tier 3 E2E target (and docs-site dogfood target)
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
- **Performance characterization is part of the process.** The `bench/` harness (`bench/run.sh`, `docs/BENCHMARKS.md`) is the reproducible way to measure throughput, tail latency, and head-of-line blocking on the real serving path (using `oha`, dedicated workloads, and Docker resource constraints for the 1-vCPU/2-GiB tier). It is opt-in (`RUN_BENCH=1`) because full runs are heavy, but it is the required check for request-path, SPI, or concurrency changes and before any latency/throughput claims. The HOLB experiment is the key validation artifact for the current single-worker model and for future multi-worker work. Re-run it (and update BENCHMARKS.md) as the primary "before/after" proof when the worker architecture changes.
- **Handoff prompts (prompts/)**: Active work orders (013–020) live here. 015 (this benchmark harness + the multi-worker design) is the model: the benchmark step is independently valuable and was done first; the design is written up even if implementation is deferred. When implementing something from a prompt, also update the "bibles" (this file + `docs/internal/DEVELOPER-GUIDE.md`) with any new rituals, constraints, or gotchas.
- **Response contract v2 (013)**: Handlers may now return a `$pgweb` envelope (via `pgweb.respond`/`redirect`/`json`/`set_cookie`) for status/headers/cookies/ct/redirects. No marker = legacy behavior (mandatory byte-identical compat for todo/site). Raw-text routes may declare `RETURNS json` (envelope or plain JSON body); template routes still require `json`. Router detects; http layer maps to Axum (denylist on hop-by-hop). Livereload is now ct-aware. This is Phase-1-completing infrastructure (unblocks Phase 2 auth + real JSON APIs). Every new envelope feature must add a flow to `examples/todo/`.
- **Phase discipline.** We are in **Phase 1** (Synchronous Core). Do not add Phase 2+ features (auth/RLS, job queues, dashboard) into Phase 1 code paths. Stage them properly. Prompt 015 benchmark work is phase-neutral and was deliberately done early.

## Commit style

- Short imperative title (≤72 chars). Body optional; if present, explain WHY, not WHAT.
- Conventional Commits prefixes (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`) — use them.
- **Do not** add a `Co-Authored-By: Claude ...` trailer on commits in this repo. The human owner has opted out.

## Session rituals

- **Before writing non-trivial code** — read the relevant `docs/*.md` section first. If your change touches an invariant above, raise a flag and wait for human confirmation.
- **Before finishing a feature** — confirm the demo app in `examples/todo/` exercises it. If not, add the flow.
- **Before committing** — run `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and the relevant `cargo pgrx test pgXX` for whichever Postgres version is current. For any change that touches the HTTP request path, SPI usage per request, response generation, Tera, routing, or concurrency behavior, also run the performance benchmark harness (`RUN_BENCH=1 scripts/test-all.sh` or directly `bash bench/run.sh` + the constrained 1-vCPU/2GiB variant). The HOLB experiment in the harness is the primary empirical check for the single-threaded worker model.

## Current phase & milestones

**Phase 1 — Synchronous Core.** All milestones + v0.2.0 polish shipped (2026-04-25). `v0.1.0` was the core; see `CHANGELOG.md` + `docs/OVERVIEW.md`. Phase 2 (auth/RLS/realtime) planning in `docs/sessions/session_6.md` (internal).

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

- Concurrency model (single background worker + single-threaded Tokio runtime): fully measured by the `bench/` harness (prompt 015 / `docs/BENCHMARKS.md`). The HOLB experiment quantifies the serialization / tail-latency problem. Multi-worker design (more processes via `SO_REUSEPORT`, each still single-threaded + own SPI backend) + per-worker bounded queue exists as the path forward (see prompt 015 and `BENCHMARKS.md`). Default worker count, livereload fan-out, and `max_connections` budgeting are the main open parameters.
- Asset size cutoff (BYTEA vs pg_largeobject): 1 MiB default — the general benchmark harness now exists to re-evaluate if needed.
- Dynamic-route pattern matching algorithm (naïve scan vs trie): still the naïve specificity-sorted scan (see `router.rs`).
- Request-path caching (templates, routes, env) and graceful shutdown: scoped in prompt 016; will reduce per-request SPI round-trips that the 015 benchmark characterized.

When any of these move, update this file and the corresponding doc in the same commit. Re-run the benchmark harness (`bench/run.sh`) as part of the validation when the change affects the hot path or the single-worker assumptions.
