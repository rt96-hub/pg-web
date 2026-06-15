# pg-web — Testing Strategy

Four (plus one) tiers of tests. Each tier has a distinct scope, a distinct failure mode, and distinct tooling. Tests at tier N don't substitute for tests at tier N+1. The companion app (`examples/todo/`) is the acceptance gate: if a feature isn't exercised there (or in the docs-site app), it isn't done.

## TL;DR — what actually runs today (v0.2.0)

One command runs everything:

```bash
scripts/test-all.sh
```

It reports like a build system (prompt 028): a paired `START`/`PASS|FAIL|SKIP` marker per phase, real `x/x` counts, and a single ASCII verdict line as the **last** output. The run is green **iff** that line says `OVERALL=PASS`:

```
PGWEB-RESULT  tier1=95/95 tier2a=6/6 tier2b=151/151 tier3=14/14 tier4=22/22 bench=skip  OVERALL=PASS
```

Counts are parsed from the real tool output (never hardcoded), so the numbers below are illustrative of the current tree, not a contract:

| Tier | Command | Tests (current) |
|---|---|---|
| 1. SQL / pgrx  | `cargo pgrx test pg17` (from `crates/pg_web_ext/`) | **95** `#[pg_test]` — schema / seed / migrations / deployments ledger / settings helper / html_escape; ListenRouter + livereload; router contract + dynamic captures + asset lookup; error catalog + dev page; Tera classification; fingerprinted assets (H); RLS / serving-role contract |
| 2a. HTTP smoke | `scripts/test-http.sh` (starts PG, polls `:8080`, runs `cargo test --test http_smoke`) | **6** `#[test]` — seeded `GET /` renders, unknown path → default 404, protected + default health/readiness probes |
| 2b. CLI        | `cargo test -p pg-web --no-fail-fast` | **151** — path scanner, migrate, push + reconcile + flags + retry (L), init, dev, env, check, stack, asset fingerprinting (H) |
| 3. Docker E2E  | `cargo test -p pg-web --test docker_e2e --no-fail-fast -- --ignored` (requires Docker + `rtaylor96/pg-web:latest` — the current test image while `pgweb/postgres` namespace is pending) | **14** — todo CRUD + dynamic routes; watcher; reconcile; error pages; assets + ETag; F.1 ledger; livereload; concurrent push retry (L); CLI-in-image (F.3); fingerprinted cache (H); 20 MiB assets (I); **extension upgrade path self-upgrade smoke (018.2)** |
| 4. CLI smoke   | `scripts/smoke-cli.sh` | **22 sections** (auto-numbered) — full black-box walk of preflight → scaffold → up → push → 404 → dev error → prod 500 → assets → helpers → env → deployments → check → livereload (see `docs/OVERVIEW.md`) |

**~270 Rust tests + 22-section smoke, all tiers green via `scripts/test-all.sh`.** Tier 3 hard-fails (no silent skip) if Docker or the test image (`rtaylor96/pg-web:latest` today) is missing — the image *is* the runtime artifact.

### Output modes + the result contract (prompt 028)

`TEST_MODE` (env) or `--errors`/`--short`/`--verbose` flags, honored by both `scripts/test-all.sh` and `bench/run.sh`:

- **`errors` (default)** — compact markers + per-tier `x/x`; on any failure it auto-surfaces the *captured* detail for the failing items only (cargo `failures:` block / smoke section body / canary `docker logs` / breached bench threshold). It does **not** re-run anything — the detail is the capture, available instantly. Green phases stay one line.
- **`short`** — compact only, never auto-expands. Still prints failing names, `n/m`, and the per-phase log path.
- **`verbose`** — additionally streams all raw `cargo`/`docker`/`oha` output (today's behavior + the markers).

Marker vocabulary (the stable ASCII keyword is the contract; the unicode glyph is decoration, ASCII under CI/non-TTY): `START`, `STEP`, `PASS`, `FAIL`, `SKIP`, and for the image phase `STALE` / `BUILD` / `BUILT` / `REUSED`, and `CANARY` for the tier-3 probe. The image decision is **always** explicit — exactly one of `REUSED (fresh …)` or the `STALE → BUILD → BUILT` triple; `build-image.sh` is never run with `>/dev/null` anymore. The benchmark prints per-workload one-liners + an HOLB before/after pair + a `PGWEB-BENCH … OVERALL=ok|fail` line.

The verdict: `OVERALL=PASS` **iff** every mandatory tier is `x/x` with `failed=0` and none is `SKIP`/missing (`bench=skip` does not fail it; `bench=fail` does). A green claim must quote the `PGWEB-RESULT` line verbatim — `SKIP`, a missing count, `n<m`, or a missing line all read as NOT green. Per-phase combined output is captured to `$RUN_DIR` (default `/tmp/pg-web-test-all-<pid>/<phase>.log`, printed in the banner and kept after the run for post-hoc inspection).

Env knobs: `PG_MAJOR=16 scripts/test-all.sh` targets a different Postgres major; the default is 17. Tier 3 panics with a remediation message if Docker or the image is missing — no silent-skip (the image is a shipped artifact; false green would undermine the tier). Note: the concrete tag is `rtaylor96/pg-web` (temporary) until the `pgweb` Docker Hub org + `pgweb/postgres` image name are finalized; the harness (build-image, test-all, smoke-cli, docker_e2e) now agree on the tag via `TEST_IMAGE` / `PGWEB_IMAGE`.

Additional harness controls (prompt 025/028):
- `TEST_MODE=errors|short|verbose` (or `--errors`/`--short`/`--verbose`) — output verbosity (above). Default `errors`.
- `STRICT=1 scripts/test-all.sh` (default when `CI` is set) — soft-tier (1/2a/3) or bench failure also produces a non-zero exit (while still running later tiers for signal). Hard tiers (2b, 4) and a failed image build are always fatal to the exit code.
- `TEST_TS=1` — prefix every line of test-all.sh output with a wall-clock timestamp (aids stall diagnosis).
- `REBUILD_IMAGE=1`, `SKIP_IMAGE_CHECK=1`, `FORCE=1` — **debugging-only** overrides (force a rebuild / bypass the content-hash freshness gate / take over a held lock). The default path needs none of them; never use one to coax a run green (prompt 029).

### Idempotency: the harness self-cleans, every run (prompt 029)

`scripts/test-all.sh` and `bench/run.sh` are fully idempotent — they produce a correct result on a cold machine, on back-to-back runs, after editing any file, after a branch switch / `git stash`, and after a previous run was `kill -9`'d mid-flight — with **zero manual hygiene and zero flags**. The machinery is shared in `scripts/lib/harness.sh`:

- **Self-healing cross-run lock.** A portable `mkdir` lock (`/tmp/pg-web-test-all.lockdir`) records the owner PID + start time. On contention it decides stale-vs-live: a dead owner (or a lock older than the max plausible run — a PID-reuse backstop) is auto-reclaimed with a `lock RECLAIMED` marker; only a genuinely-running concurrent run blocks. `FORCE=1` is no longer needed after a crash. test-all and bench share the *same* lock so they serialize against each other (the `:8080` hazard); a nested bench under `RUN_BENCH=1` skips it (no self-deadlock).
- **Unconditional `reclaim_environment`** at the top of every run (while holding the lock, so it's safe to be aggressive): stops the pgrx dev PG; `docker rm -f`s **our own families only** — `pgweb-canary-*`, the `pg-web-smoke*` stacks, the `bench` compose project (scoped to its compose file), and orphaned tier-3 testcontainers matched by the `org.testcontainers` label *AND* our image — never a blanket prune; reaps stale `/tmp/pg-web-smoke*` + old per-run log dirs. It is **surgical** (unrelated containers are never touched) and a no-op on a clean machine.
- **Unified content-hash image freshness.** One `compute_src_hash` — a whole-tree-minus-volatile-denylist `sha256sum` (~1–2 s) — is shared by `build-image.sh` (which bakes it into the `pgweb.src_hash` LABEL), `test-all.sh`, **and** `bench/run.sh`, so the label can never diverge from what the checkers compute. A changed tree ⇒ `STALE → BUILD → BUILT` (automatic, no flag); an unchanged tree ⇒ `REUSED`. There is no mtime fast-path anymore: it caused false rebuilds on `git stash`/checkout mtime noise and could miss content edits that didn't advance mtime. The denylist contains only volatile/scratch paths (`.git`, `target`, `bench/results`, `bench/bin`, `node_modules`, `.DS_Store`, `*.log`, `.env`) that can never affect the image, so no image-affecting input can be silently missed (the failure mode of the old enumerated file list). **`bench/run.sh` now rebuilds-on-stale automatically** — it previously had no freshness check and could silently benchmark an old binary.
- **One image tag.** `TEST_IMAGE` / `PGWEB_IMAGE` resolve through the shared `pgweb_image` (default `rtaylor96/pg-web:latest`) across test-all, bench, `bench/docker-compose.yml`, build-image, smoke-cli, and `docker_e2e.rs` — no hardcoded-literal drift.

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

### Upgrade path tier (018.2)

A dedicated ignored test (`extension_upgrade_preserves_data_and_serves` in `crates/pg_web_cli/tests/docker_e2e.rs`) exercises real in-place upgrade using `ALTER EXTENSION pg_web_ext UPDATE`.

- Boots the test image (which now contains both the pgrx-generated install SQL and hand-authored `pg_web_ext--*--*.sql` upgrade scripts).
- Performs a `pg-web push` (or direct inserts) to create realistic user data + `pgweb.deployments` / routes / etc.
- Writes a synthetic additive upgrade script (e.g. a marker table or safe `ALTER TABLE ... ADD COLUMN`) into the container's extension directory, using a throwaway target version.
- Executes `ALTER EXTENSION pg_web_ext UPDATE TO 'the-test-version';`.
- Asserts:
  - The synthetic change from the upgrade script is visible.
  - All prior user data, framework ledger rows, routes/templates/assets are intact.
  - The app still serves (basic HTTP smoke + the pushed todo flows).
- The test is part of the normal `cargo test ... -- --ignored` run inside Tier 3, so it is exercised whenever the full `scripts/test-all.sh` (or direct Docker E2E) runs against a fresh image.

For PG 15/16/17 coverage of the *DDL* (per invariant #6 and the prompt): the same majors that already run the full bootstrap install SQL via `cargo pgrx test pg$MAJOR` (and thus validate the current schema) also validate that the upgrade script files contain only portable constructs (simple `psql -f` or equivalent against a scratch database on the pgrx-managed PG or a stock `postgres:$MAJOR` container; no `.so` load is required for the pure-DDL synthetic changes used in the smoke).

This tier is the permanent guard against "only ever tested fresh `CREATE EXTENSION`" regressions. See `docs/DEPLOYMENT.md` § "Upgrading the framework itself", `CLAUDE.md` (policy + the new restart-cost invariant), and `crates/pg_web_ext/upgrades/README.md`.

### The companion app IS the acceptance test

If a feature isn't exercised in `examples/todo/`, it isn't done. New features land with four things:

1. Implementation (in `pg_web_ext` or `pg_web_cli`).
2. Tier 1 or Tier 2 tests.
3. A new page/flow/migration in `examples/todo/` that uses the feature.
4. Substantial explanatory comments in the demo files (especially the handler `.sql` files) that teach readers the pattern, the design rationale, and how to reuse it in their own apps. The companion app is living documentation, not just test coverage.

### Demo app trajectory

The demo app (`examples/todo/`) grows in lockstep with the framework and is the primary E2E target (tier 3 + tier 4 smoke). It is also the end-state of `docs/TUTORIAL.md`.

- **v0.1.0 / Phase 1 core:** full todo CRUD + HTMX, migrations, dynamic routes, assets, validation UX, live-reload, `_404`, dev/prod error modes.
- **Phase 2+:** will extend with auth + RLS-filtered data.
- **Later phases:** job-queue examples, dashboard screenshots in its README, etc.

See the feature matrix in `docs/OVERVIEW.md` for the exact v0.2.0 coverage. The rule: every shipped framework feature must have a corresponding exercised path **plus good explanatory comments** in the companion app (or the dogfooded `pg-web.dev` docs site). The demo app serves as both E2E coverage and primary teaching material.

### Demo app feature matrix (summary)

The exhaustive current-state matrix lives in `docs/OVERVIEW.md` (and the roadmap in `docs/ROADMAP.md`). Key rule: **if a feature isn't exercised in `examples/todo/` with good explanatory comments (or the docs-site app), it isn't done.**

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
