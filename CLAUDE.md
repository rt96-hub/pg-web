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
6. **Target Postgres version = the bundled image major (currently 17).** pg-web ships Postgres itself (the runtime image bundles `postgres:17`), so users run the major we choose — older majors are not a support gate (decision 2026-06-12; see ROADMAP § Decision log). The `pg15`/`pg16` cargo features remain for now and should keep *compiling* (cfg-gate any version-specific `pg_sys` usage), but features only need to be **correct** on the bundled major — do not contort designs for older-version compatibility (PG17+-only facilities like `BGWORKER_BYPASS_ROLELOGINCHECK` — present only in the pg17/pg18 bindings — are fine to rely on). No pg18 features until the image moves.
7. **Async only in the background worker.** Don't introduce `tokio` code paths inside `#[pg_extern]` functions — those run on Postgres's synchronous backend threads and will deadlock.

## Coding practices

- **pgrx-first patterns.** Use `Spi::run`, `Spi::get_one`, `BackgroundWorkerBuilder`, `#[pg_extern]`, `#[pg_test]`. Read pgrx docs before inventing patterns.
- **Tests next to code.** Extension tests use `#[pg_test]` and run via `cargo pgrx test pg17`. Don't mock Postgres — run against the real compiled instances under `~/.pgrx/`.
- **No premature abstraction.** Three duplicated lines beats the wrong trait. The extension is small; keep modules flat until patterns genuinely emerge.
- **Error handling on the request path.** No `unwrap()` / `expect()` in the HTTP handler. Fatal SQL exceptions → generic 500 in prod, rich debug page in dev (mode from the `pgweb.env` GUC).
- **Every feature ships with a companion-app flow.** If a feature isn't exercised in `examples/todo/`, it isn't done. See `docs/TESTING.md`. Completion also requires a full green run of the single command that exercises the 5 tiers (see Session rituals).
- **Performance characterization is part of the process.** The `bench/` harness (`bench/run.sh`, `docs/BENCHMARKS.md`) is the reproducible way to measure throughput, tail latency, and head-of-line blocking on the real serving path (using `oha`, dedicated workloads, and Docker resource constraints for the 1-vCPU/2-GiB tier). For any change touching the HTTP request path, SPI per request, response generation, Tera, routing, or concurrency, the **required** validation is a full run via the single command with the benchmark enabled: `RUN_BENCH=1 scripts/test-all.sh` (this also runs the constrained 1-vCPU/2GiB variant). The HOLB experiment is the primary empirical check for the single-threaded worker model. Re-run it (and update BENCHMARKS.md) as the "before/after" proof when the worker architecture or hot path changes. Partial benchmark runs are not sufficient.
- **Handoff prompts (prompts/)**: Active work orders (013–020) live here. 015 (this benchmark harness + the multi-worker design) is the model: the benchmark step is independently valuable and was done first; the design is written up even if implementation is deferred. When implementing something from a prompt, also update the "bibles" (this file + `docs/internal/DEVELOPER-GUIDE.md`) with any new rituals, constraints, or gotchas. Completion of prompt work is gated on a full clean `scripts/test-all.sh` run (all 5 tiers) in addition to the companion-app rule.
- **Response contract v2 (013)**: Handlers may now return a `$pgweb` envelope (via `pgweb.respond`/`redirect`/`json`/`set_cookie`) for status/headers/cookies/ct/redirects. No marker = legacy behavior (mandatory byte-identical compat for todo/site). Raw-text routes may declare `RETURNS json` (envelope or plain JSON body); template routes still require `json`. Router detects; http layer maps to Axum (denylist on hop-by-hop). Livereload is now ct-aware. This is Phase-1-completing infrastructure (unblocks Phase 2 auth + real JSON APIs). Every new envelope feature must add a flow to `examples/todo/`.
- **Phase discipline.** We are in **Phase 1** (Synchronous Core). Do not add Phase 2+ features (auth/RLS, job queues, dashboard) into Phase 1 code paths. Stage them properly. Prompt 015 benchmark work is phase-neutral and was deliberately done early.

## Commit style

- Short imperative title (≤72 chars). Body optional; if present, explain WHY, not WHAT.
- Conventional Commits prefixes (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`) — use them.
- **Do not** add a `Co-Authored-By: Claude ...` trailer on commits in this repo. The human owner has opted out.

## Session rituals

- **Before writing non-trivial code** — read the relevant `docs/*.md` section first. If your change touches an invariant above, raise a flag and wait for human confirmation.
- **Before finishing / concluding a feature** — two things must both be true:
  1. The demo app in `examples/todo/` exercises the new behavior (per the companion-app rule). If not, add the flow.
  2. A **complete, clean run of the single command `scripts/test-all.sh`** succeeds, covering **all 5 tiers**:
     - Tier 1: `cargo pgrx test` (SQL / `#[pg_test]` inside real Postgres instances)
     - Tier 2a: HTTP smoke
     - Tier 2b: CLI unit tests
     - Tier 3: Docker E2E (mandatory — runs against `rtaylor96/pg-web:latest` + full `examples/todo/` flows; the script auto-detects when extension sources, Dockerfile, or init scripts are newer than the image and triggers `scripts/build-image.sh`)
     - Tier 4: CLI black-box smoke (`scripts/smoke-cli.sh`)
     The script (`scripts/test-all.sh`) is the canonical one-command entry point for the full matrix (it also stops stray pgrx dev PGs to avoid port shadowing before tier 4). Use `PG_MAJOR=16 scripts/test-all.sh` etc. when you need multi-version coverage. If the change touches request-path / SPI / routing / concurrency, also run with `RUN_BENCH=1 scripts/test-all.sh` (or the explicit bench harness) so the HOLB experiment and constrained 1-vCPU/2GiB numbers are refreshed.
- **Before committing** — the same gates as "before finishing a feature" above, plus `cargo check --workspace` and `cargo clippy --workspace -- -D warnings`. There is exactly one known non-blocking flaky test (a timeout in the dev-mode watcher repush flow inside tier 4 smoke). All other tests must be green. Known harness caveat: tier 4's `pg-web up` does an unconditional `docker compose pull`, which can silently replace the locally built test image with the published Docker Hub one (tier 4 then validates the wrong artifact) — see `docs/internal/TESTING-SETUP.md` § Known harness-integrity gotcha. "Partial tier runs" (only pgrx, only CLI, etc.) are not sufficient to declare a feature complete.

We have the full single-command test suite for a reason. Rebuilding the image when schema or extension code changes is expected and automatic inside `scripts/test-all.sh` (or forced with `REBUILD_IMAGE=1`). Do not declare work "done" until the entire matrix has passed under the single command.

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
