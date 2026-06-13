# pg-web — Testing Strategy

Four (plus one) tiers of tests. Each tier has a distinct scope, a distinct failure mode, and distinct tooling. Tests at tier N don't substitute for tests at tier N+1. The companion app (`examples/todo/`) is the acceptance gate: if a feature isn't exercised there (or in the docs-site app), it isn't done.

## TL;DR — what actually runs today (v0.2.0)

One command runs everything:

```bash
scripts/test-all.sh
```

It exits non-zero on any failure and prints `All tests passed.` on success. Tiers:

| Tier | Command | Tests (v0.2.0) |
|---|---|---|
| 1. SQL / pgrx  | `cargo pgrx test pg17` (from `crates/pg_web_ext/`) | **72** `#[pg_test]` — schema / seed / migrations / deployments ledger / settings helper / html_escape; ListenRouter + livereload; router contract + dynamic captures + asset lookup; error catalog + dev page; Tera classification; fingerprinted assets (H) |
| 2a. HTTP smoke | `scripts/test-http.sh` (starts PG, polls `:8080`, runs `cargo test --test http_smoke`) | 2 `#[test]` — seeded `GET /` renders, unknown path returns default 404 body |
| 2b. CLI        | `cargo test -p pg-web` | **143** — path scanner, migrate, push + reconcile + flags + retry (L), init, dev, env, check, stack, asset fingerprinting (H) |
| 3. Docker E2E  | `cargo test -p pg-web --test docker_e2e -- --ignored` (requires Docker + `rtaylor96/pg-web:latest` — the current test image while `pgweb/postgres` namespace is pending) | **13** — todo CRUD + dynamic routes; watcher; reconcile; error pages; assets + ETag; F.1 ledger; livereload; concurrent push retry (L); CLI-in-image (F.3); fingerprinted cache (H); 5 MiB assets (I) |
| 4. CLI smoke   | `scripts/smoke-cli.sh` | **19 sections** — full black-box walk of scaffold → up → push → 404 → dev error → prod 500 → assets → helpers → env → deployments → check → livereload (see `docs/OVERVIEW.md`) |

**230+ Rust tests + 19-section smoke, all tiers green via `scripts/test-all.sh`.** Tier 3 hard-fails (no silent skip) if Docker or the test image (`rtaylor96/pg-web:latest` today) is missing — the image *is* the runtime artifact.

Env knobs: `PG_MAJOR=16 scripts/test-all.sh` targets a different Postgres major; the default is 17. Tier 3 panics with a remediation message if Docker or the image is missing — no silent-skip (the image is a shipped artifact; false green would undermine the tier). Note: the concrete tag is `rtaylor96/pg-web` (temporary) until the `pgweb` Docker Hub org + `pgweb/postgres` image name are finalized; the harness (build-image, test-all, smoke-cli, docker_e2e) now agree on the tag via `TEST_IMAGE` / `PGWEB_IMAGE`.

Additional harness controls (prompt 025):
- `STRICT=1 scripts/test-all.sh` (default when `CI` is set) — any tier failure produces a non-zero exit (while still running later tiers for signal).
- `TEST_TS=1` — prefix every line of test-all.sh output with a wall-clock timestamp (aids stall diagnosis).
- `REBUILD_IMAGE=1`, `SKIP_IMAGE_CHECK=1` — force or bypass the (now content-hash + mtime) image freshness gate.

## CI integration

The scripts are the CI entrypoint. Machine bring-up details (macOS ICU/pkg-config gotcha, dev-DB creation for tier 2a, Docker image + port hygiene, caching strategy) live in `docs/internal/TESTING-SETUP.md` — read that before wiring a new runner or dev machine. A GitHub Actions workflow (not yet added) would:

1. Install Rust + system deps (`libclang-dev`, `flex`, `bison`, `libreadline-dev`, `zlib1g-dev`, `libssl-dev`, `libicu-dev`, `pkg-config`) as root. (`libicu-dev` is required: PostgreSQL ≥ 16 configure hard-fails without ICU.)
2. Create a non-root `pgweb` user, switch to it (pgrx can't run as root).
3. Cache `~/.pgrx/` (~2 GiB compiled PG) — first run is 20-60 min, cached runs are ~2 min.
4. Run `cargo install --locked cargo-pgrx --version ~0.18`.
5. Run `cargo pgrx init --pg17 download` (no-op if cached).
6. Append `shared_preload_libraries='pg_web_ext'` to `~/.pgrx/data-17/postgresql.conf`.
7. Run `scripts/test-all.sh`.

Each step takes ~30 sec on a cached run; ~25 min on a cold cache. For iterative CI it's cheap; for fresh PR builds expect ~10 min once caching is set up properly.


## Tier 1 — Unit tests (inside Postgres)

**Tool:** `pgrx-tests` + the `#[pg_test]` macro.
**Runs:** `cargo pgrx test pg17` (also `pg15`, `pg16`).
**Scope:** individual Rust functions in `pg_web_ext` that interact with Postgres internals.

### Example

```rust
use pgrx::prelude::*;

#[pg_schema]
mod tests {
    use super::*;

    #[pg_test]
    fn test_route_lookup_returns_handler() {
        Spi::run("
            CREATE SCHEMA IF NOT EXISTS pgweb;
            CREATE TABLE IF NOT EXISTS pgweb.routes
              (path_pattern text PRIMARY KEY, handler text, template_path text);
            INSERT INTO pgweb.routes
              VALUES ('/', 'home_handler', 'pages/index.html');
        ").unwrap();

        let result = crate::router::lookup_route("/").expect("route should match");
        assert_eq!(result.handler, "home_handler");
        assert_eq!(result.template_path, "pages/index.html");
    }

    #[pg_test]
    fn test_tera_renders_json_context() {
        let html = crate::templating::render(
            "<h1>Hello {{ name }}</h1>",
            r#"{"name": "Alice"}"#,
        ).unwrap();
        assert_eq!(html, "<h1>Hello Alice</h1>");
    }
}
```

Each `#[pg_test]` runs inside a fresh Postgres transaction. Rollback on teardown. No test ever sees another test's side effects.

### What to test here

- SPI query correctness against our framework tables.
- Tera rendering with real JSON inputs.
- Error propagation from SQL to the HTTP layer.
- GUC reads/writes.
- Route pattern matching (`/posts/[id]` vs `/posts/42`).
- Session token validation (Phase 2).
- Job-queue state transitions (Phase 3).

### What NOT to test here

- HTTP behavior (use Tier 2 HTTP smoke).
- CLI behavior (use Tier 2 CLI).
- Behavior that doesn't touch Postgres internals (plain `#[test]` is fine).

## Tier 2a — HTTP smoke (against a running extension)

**Tool:** standard Rust `#[test]` + `reqwest`.
**Lives at:** `crates/pg_web_ext/tests/http_smoke.rs`.
**Runs:** `scripts/test-http.sh` (starts PG if needed, polls `:8080`, runs `cargo test --test http_smoke`).
**Scope:** the HTTP surface of the extension's background worker. Route resolution, template rendering, status codes, response bodies.

Why this tier exists: `#[pg_test]` tests run inside an SPI transaction and can't reach the HTTP server (which lives in a separate BGW process). They also can't issue arbitrary HTTP requests. This tier is the smallest thing that proves "the worker is actually serving traffic correctly."

### Example

```rust
#[test]
fn root_returns_hello_from_pg_web() {
    let resp = reqwest::blocking::get("http://localhost:8080/").unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap().trim(), "hello from pg-web");
}
```

The script handles orchestration. The test itself is pure assertion.

### What to test here

- HTTP status codes for known routes, unknown routes, method mismatches.
- Response bodies for rendered templates (Tier 2a once step 3 lands).
- Content-Type / cache headers for assets.
- Error page format in dev vs production modes.

## Tier 2b — CLI tests (outside Postgres)

**Tool:** standard Rust `#[test]` + `testcontainers` for integration flows.
**Runs:** `cargo test -p pg-web`.
**Scope:** file watcher, migration diffing, `pg-web push`, CLI arg parsing.

### Example

```rust
#[test]
fn hot_reload_uploads_template_on_save() {
    use testcontainers::core::ImageExt;
    use testcontainers::runners::SyncRunner;
    let pg = testcontainers_modules::postgres::Postgres::default().start().unwrap();
    let url = format!("postgres://postgres:postgres@localhost:{}/postgres",
                      pg.get_host_port_ipv4(5432).unwrap());

    let app_dir = tempdir().unwrap();
    std::fs::write(app_dir.path().join("pages/index.html"), "<h1>Hi</h1>").unwrap();

    pg_web_cli::dev::sync_templates(app_dir.path(), &url).unwrap();

    let conn = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let row = conn.query_one(
        "SELECT content FROM pgweb.templates WHERE path = 'pages/index.html'",
        &[],
    ).unwrap();
    assert_eq!(row.get::<_, String>(0), "<h1>Hi</h1>");
}
```

Start a Postgres container per test module (cached via `testcontainers`), seed it with the framework schema, run the CLI logic against it, assert on database state.

### What to test here

- File→DB sync on save.
- Schema diffing (`pg-web migrate create` output).
- Shift-left SQL rollback pre-check.
- CLI argument parsing.
- Error message clarity.
- `pg-web push` idempotency (pushing the same app twice doesn't duplicate routes).

## Tier 3 — End-to-end (the companion app)

**Tool:** `examples/todo/` — a real pg-web app that exercises every framework feature.
**Runs:** CI spins up `rtaylor96/pg-web:latest`, runs `pg-web dev` pointing at the demo app, hits HTTP endpoints with `reqwest`, asserts on response bodies and status codes.
**Scope:** product behavior from the app developer's POV.

### The companion app IS the acceptance test

If a feature isn't exercised in `examples/todo/`, it isn't done. New features land with three things:

1. Implementation (in `pg_web_ext` or `pg_web_cli`).
2. Tier 1 or Tier 2 tests.
3. A new page/flow/migration in `examples/todo/` that uses the feature.

### Demo app trajectory

The demo app (`examples/todo/`) grows in lockstep with the framework and is the primary E2E target (tier 3 + tier 4 smoke). It is also the end-state of `docs/TUTORIAL.md`.

- **v0.1.0 / Phase 1 core:** full todo CRUD + HTMX, migrations, dynamic routes, assets, validation UX, live-reload, `_404`, dev/prod error modes.
- **Phase 2+:** will extend with auth + RLS-filtered data.
- **Later phases:** job-queue examples, dashboard screenshots in its README, etc.

See the feature matrix in `docs/OVERVIEW.md` for the exact v0.2.0 coverage. The rule: every shipped framework feature must have a corresponding exercised path in the companion app (or the dogfooded `pg-web.dev` docs site).

### Demo app feature matrix (summary)

The exhaustive current-state matrix lives in `docs/OVERVIEW.md` (and the roadmap in `docs/ROADMAP.md`). Key rule: **if a feature isn't exercised in `examples/todo/` (or the docs-site app), it isn't done.**

High-level coverage at v0.2.0 includes: static + dynamic routes, JSON→Tera + raw-text handlers, custom 404, migrations + ledger, HTMX forms + validation UX, `html_escape`, `pgweb.setting()`, `pg-web check`, `--dry-run`/`--with-migrate`, deployments ledger, live-reload SSE, content-hashed assets + immutable caching, 20 MiB assets, push retry (L), CLI-in-image (F.3).

For the detailed per-component checklist see `docs/OVERVIEW.md` § Test story and `docs/ROADMAP.md`.

Abridged table (historical snapshot; see OVERVIEW for live numbers):

| Framework feature | Demo coverage | Status |
|---|---|---|---|
| Static route (`GET /`) | `pages/index.html` + `pages/index.sql` | M1.1 | ☑ |
| SQL handler returning JSON | `pgweb.hello_handler` returns `{"name":"pg-web"}` | M1.1 | ☑ |
| Tera `{{ }}` basic substitution | `<h1>hello from {{ name }}</h1>` | M1.1 | ☑ |
| `pg-web init` scaffold | Demo app produced by `pg-web init my-app` | M1.1 | ☑ |
| `pg-web push` | `scripts/test-http.sh` invokes it against the dev PG | M1.1 | ☑ |
| Docker image boots ext | `docker compose up` → `GET /` returns 200 | M1.1 | ☑ |
| Handler accepts `req json` arg | `pgweb.pages__*(req json)` uniform signature | M1.3 | ☑ |
| Raw-text handler mode | `POST /todos/delete` returns `text`, bypasses Tera | M1.3 | ☑ |
| Custom 404 template | `pages/_404.html` served on route miss | M1.3 | ☑ |
| Static handler mode (HTML only, no SQL) | `pages/_404.html` with synthesized `{}` handler | M1.3 | ☑ |
| `pg-web migrate apply` ledger | `migrations/0001_create_todos.sql` + `pgweb.migrations` | M1.3 | ☑ |
| Tera `{% for %}` + `{{ }}` | Todo list rendered | M1.3 | ☑ |
| HTMX POST form (create) | "Add todo" form appends fragment via `hx-swap="beforeend"` | M1.3 | ☑ |
| HTMX fragment swap (toggle) | `POST /todos/toggle` with `hx-swap="outerHTML"` replaces the `<li>` | M1.3 | ☑ |
| HTMX empty-body swap (delete) | `POST /todos/delete` text-mode returns `''`, HTMX removes the `<li>` | M1.3 | ☑ |
| `pg-web up`/`down` stack mgmt | Tutorial uses raw `docker compose` until `up` ships | M1.2 | ☐ |
| Hot reload: `.sql` save | Edit a todo handler, see change <500ms | M1.2 | ☐ |
| Hot reload: `.html` save | Same | M1.2 | ☐ |
| Dynamic route (`[id]` param) | Todo detail: `pages/todos/[id]/index.html` | M1.2 | ☐ |
| Dev error page | One route intentionally throws | M1.2 | ☐ |
| Static asset (small, BYTEA) | `public/styles.css` linked from the demo | M1.2 | ☐ |
| Validation via `check_violation` | Empty-title CHECK surfaced to the user | M1.4 | ☑ |
| Static asset (large, pg_largeobject) | `public/hero.jpg` banner image | M1.4 | ☐ |
| Secrets via `pgweb.settings` | `pg-web env set` + `pgweb.setting()` read in handler | M1.4 | ☑ |
| Production 500 page | Dev error path flipped to prod mode | M1.4 | ☐ |
| `pg-web check` lint | Offline project validator (sqlparser + Tera, pre-commit/CI gate) | M1.4 | ☑ |
| `pg-web init --template todo` | Scaffold the todo app straight from bundled `examples/todo/` | M1.4 | ☑ |
| `pgweb.html_escape()` SQL helper | Raw-text handler with user content | M1.4 | ☑ |
| **Phase 2** — auth | Login, logout, RLS-filtered todo list | P2 | ☐ |
| **Phase 3** — async job | Email confirmation on signup | P3 | ☐ |
| **Phase 4** — dashboard | Screenshot in README | P4 | ☐ |

### E2E test harness

```rust
// examples/todo/tests/e2e.rs
#[test]
fn home_renders_greeting() {
    let app = start_demo_app();
    let resp = reqwest::blocking::get(&format!("{}/", app.url)).unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().unwrap();
    assert!(body.contains("<h1>Welcome"));
}
```

`start_demo_app()` boots a container from the test image (rtaylor96/pg-web:latest today), runs `pg-web dev` (or direct calls) against the demo app, waits for the HTTP port to open, and returns a handle.

## CI matrix

Every PR runs:

- Tier 1 on the bundled Postgres major (17). (Multi-major CI was dropped 2026-06-12 — pg-web ships Postgres in its own image, so only the bundled major is a correctness target; `pg15`/`pg16` features need only compile. See ROADMAP § Decision log.)
- Tier 2 on current stable Rust.
- Tier 3 against the current test image `rtaylor96/pg-web:latest` (built from the PR; will become `pgweb/postgres` post-namespace cutover).
- `cargo clippy --workspace -- -D warnings`.
- `cargo fmt --check`.

Breaking any of these blocks merge.

## Test data conventions

- Tier 1: inline seed data inside each `#[pg_test]`. No shared fixtures (tests must be independent).
- Tier 2: per-test `testcontainers` Postgres. Seed in the test's setup.
- Tier 3: `examples/todo/migrations/` contains the demo app's canonical seed data. Checked into repo.

## Debugging failing tests

- **Tier 1 flaky:** add `println!()` in the test and re-run with `cargo pgrx test pg17 -- --nocapture`.
- **Tier 2 flaky from testcontainers:** bump Postgres start timeout (`POSTGRES_START_TIMEOUT=60`).
- **Tier 3 flaky:** run locally with `docker compose -f examples/todo/docker-compose.yml up` and hit endpoints by hand with `curl -v`.

## Performance benchmarks (Phase 1+)

See `docs/BENCHMARKS.md` (prompt 015) for the published, reproducible numbers against the real serving path.

The harness is `bench/run.sh` (uses `oha`, dedicated `bench/app/`, Docker resource constraints for the 1 vCPU/2 GiB tier, open-model HOLB experiment, etc.). It is opt-in via `RUN_BENCH=1` precisely because a full matrix is minutes long.

Micro-benchmarks (if added later) would live under `cargo bench -p pg_web_ext`. The product-level story is the `bench/` one, not ad-hoc `wrk` against the todo demo.
