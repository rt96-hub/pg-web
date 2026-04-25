# pg-web — Maintainer's Development Guide

For framework maintainers building pg-web itself. (App developers using pg-web see `APP-DEVELOPER-GUIDE.md`.)

## Environment

- WSL2 Ubuntu-22.04 (or native Linux / macOS). Native Windows is not supported for development.
- **A dedicated non-root user is required** — Postgres's `initdb` refuses to run as root, which breaks `cargo pgrx test` and `cargo pgrx run`. On this project's reference machine we use a `pgweb` user with home `/home/pgweb`.
- Rust stable **1.95+**. Update with `rustup update stable`.
- `cargo-pgrx` **0.18.x**. Install with `cargo install --locked cargo-pgrx`.
- Local Postgres 15/16/17 installations compiled by pgrx. Bootstrap with:
  ```
  cargo pgrx init --pg15 download --pg16 download --pg17 download
  ```
  This downloads Postgres source, compiles each version, and registers them at `~/.pgrx/config.toml`. Takes 20-60 minutes the first time.

### System packages (Debian/Ubuntu)

```
sudo apt install \
  build-essential libclang-dev libreadline-dev zlib1g-dev \
  flex bison libxml2-dev libxslt1-dev libssl-dev \
  pkg-config ccache patchelf
```

(`patchelf` is only needed if you move `~/.pgrx/` between users — see *Pitfalls* below.)

macOS: `brew install llvm openssl pkg-config ccache`.

### One-time setup on a fresh WSL2 machine

```bash
# As root
apt update && apt install -y build-essential libclang-dev libreadline-dev \
  zlib1g-dev flex bison libxml2-dev libxslt1-dev libssl-dev pkg-config ccache patchelf
useradd -m -s /bin/bash pgweb

# As pgweb
sudo -iu pgweb
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
echo '. "$HOME/.cargo/env"' >> ~/.bashrc
source ~/.cargo/env
cargo install --locked cargo-pgrx
cargo pgrx init --pg15 download --pg16 download --pg17 download
```

Then clone the repo under `/home/pgweb/pg-web`.

### One-time `postgresql.conf` tweak for dev

For the HTTP background worker to actually start, the extension must be loaded at postmaster startup via `shared_preload_libraries` — not just via `CREATE EXTENSION` (which runs install SQL but doesn't force the `.so` to load). Add this line to your pgrx data dir's `postgresql.conf`:

```bash
echo "shared_preload_libraries = 'pg_web_ext'" >> ~/.pgrx/data-17/postgresql.conf
# repeat for data-15, data-16 if you're testing those PG versions
```

After editing, restart PG: `cargo pgrx stop pg17 && cargo pgrx run pg17`. The `_PG_init` callback now runs in shared-preload context, registers the BGW statically, and the postmaster forks the worker before accepting connections. You still run `CREATE EXTENSION pg_web_ext;` once to materialise the framework schema (`pgweb.routes`, `pgweb.templates`) — but the HTTP server comes up independently of that.

## Dev loop

### Extension

From `crates/pg_web_ext/`:

```
cargo pgrx run pg17       # Compile + load ext into fresh PG17 + drop into psql
cargo pgrx run pg16       # Same against PG16
cargo pgrx test pg17      # Run #[pg_test] suite inside live PG17
cargo pgrx install        # Install into a system-wide PG (rare — use Docker instead)
```

`cargo pgrx run pgXX` compiles the extension, installs the `.so` + `.control` into the local pgrx PG, starts that PG (reusing `~/.pgrx/data-XX`), and drops you into `psql`. **You still need to run `CREATE EXTENSION pg_web_ext;` manually** on first connect — pgrx doesn't auto-create it for you. The extension's install SQL (schema + tables) is applied at `CREATE EXTENSION` time. When you exit psql (`\q`), the Postgres instance stays running against the same data dir for subsequent `cargo pgrx run` invocations; only `cargo pgrx stop pgXX` shuts it down.

When to use each:
- `cargo pgrx run pgXX` — interactive psql, good for poking at the live extension and testing by hand.
- `cargo pgrx test pgXX` — non-interactive, runs every `#[pg_test]` inside its own transaction. No manual `CREATE EXTENSION` needed here; pgrx handles it.

Standard cargo commands also work but require `--features pgXX` because the ext crate uses pgrx's feature-flag-gated bindgen:

```
cargo check --features pg17 -p pg_web_ext
cargo clippy --features pg17 -p pg_web_ext -- -D warnings
```

### CLI

From the workspace root:

```
cargo build -p pg_web_cli
cargo run -p pg_web_cli -- init my-test-app
cargo test -p pg_web_cli
```

Standard Rust. No pgrx involvement.

### Whole workspace

```
cargo check --workspace --features pg17
cargo clippy --workspace --features pg17 -- -D warnings
```

Note: the `--features pg17` flag is consumed by `pg_web_ext`; `pg_web_cli` ignores it. This is intentional — the extension needs a PG version; the CLI is version-agnostic.

## Writing tests

See `TESTING.md` for full strategy. Maintainer tl;dr:

- **Rust code inside the extension** → `#[pg_test]` + `cargo pgrx test pgXX`.
- **CLI code** → `#[test]` + optional `testcontainers::postgres::Postgres` fixtures.
- **Product behavior** → add a flow in `examples/todo/` (the companion app is THE acceptance gate).

## BGW connection accounting

`pg_web_ext`'s background worker uses Postgres backend slots like this:

- **1 SPI session (always)** — `BackgroundWorker::connect_worker_to_spi` at startup. Every HTTP request runs its route-lookup + handler-call SQL on this session via `BackgroundWorker::transaction`.
- **1 libpq LISTEN session (dev only)** — started from `worker.rs::pg_web_worker_main` when `pgweb.settings.env = 'development'` at worker startup. Opens a tokio-postgres connection to `127.0.0.1:<PostPortNumber>`, issues `LISTEN pgweb_livereload`, forwards notifications to the in-memory `ListenRouter::publish`. The SPI session can't hold a LISTEN (it's request-scoped), so Component G needs its own connection.

Total: **2 backend slots in dev**, **1 in prod**. Against typical `max_connections = 100` defaults the +1 is noise; on a resource-starved instance (`max_connections = 10` or less) it's meaningful and the dev-only gating matters. The fan-out from LISTEN to N browser SSE tabs is all in-memory (`tokio::sync::broadcast`) — no per-tab backend.

SSE tasks are HTTP connections on the BGW's existing tokio runtime. They don't show up in `pg_stat_activity`; they're pure tokio tasks.

For Phase 2 app-level realtime subscriptions the same LISTEN connection is reused — `listen_router::ListenRouter` is deliberately channel-agnostic. Adding another channel (e.g. `pgweb_app_todos`) is one more entry in `worker.rs`'s `preregister` + one more `LISTEN` in the same session. No extra backend slot.

## Tokio runtime constraint

The BGW runs `tokio::runtime::Builder::new_current_thread()` — single-threaded. Reason: SPI is pinned to the OS thread `connect_worker_to_spi` attached to. A multi-threaded runtime would migrate tasks to worker threads that lack SPI context, causing panics.

**Implication for new async code:** anything that calls SPI (via `BackgroundWorker::transaction` or pgrx's `Spi`) must run on the current-thread runtime's main task — never on a `tokio::spawn`'d task if the spawn happens inside a multi-threaded runtime. In our current-thread runtime, `tokio::spawn` is fine; all tasks share the same thread. **Anything that does only network I/O** (like the livereload LISTEN task using tokio-postgres) is fine either way.

## Workspace conventions

- Resolver 2 (`resolver = "2"` at workspace root).
- Editions pinned per crate (ext: 2021 for pgrx compat; cli: 2021 for now, may bump to 2024).
- Shared profile settings at workspace root. `panic = "unwind"` is **required** by pgrx for both dev and release profiles — pgrx catches Postgres longjmps at the FFI boundary.
- Use `workspace.package` inheritance for `version`, `edition`, `license`, `repository`.
- Target-specific rustflags go in per-crate `.cargo/config.toml` (e.g., the ext's `-Wl,-undefined,dynamic_lookup` on macOS).
- Avoid `rustflags` at the workspace level — it applies to all crates including proc-macros and breaks things.
- **Ambient-environment dependency injection for testability.** When a CLI function reads from `std::env`, system clock, or other global state, take a closure (or trait object) rather than calling the global directly. Tests pass a mock; production passes the real reader. Example: `stack::resolve_database_url(app_dir, env_lookup)` takes `impl Fn(&str) -> Option<String>`; the CLI's main.rs passes `|k| std::env::var(k).ok()`. Keeps tests hermetic without having to mutate process state.
- **Prefer focused crate features over kitchen-sink deps.** Pin default-off + opt-in features when adding deps (e.g., `toml = { version = "0.8", default-features = false, features = ["parse"] }` — parse-only, no `toml_edit`). Saves compile time and closes attack surface.
- **Shell out with `std::process::Command` inheriting stdout/stderr for user-facing work, piping for log-tailing.** Pattern: `stack::up` / `stack::down` inherit so the user sees compose output live; `dev::spawn_logs_tail` pipes stdout so the watcher thread can prefix `[pg]`.

## Packaging

The canonical distribution artifact is a Docker image based on `postgres:17`.

### Dockerfile sketch

```dockerfile
# Build stage
FROM postgres:17 AS builder
RUN apt update && apt install -y curl build-essential libclang-dev \
    postgresql-server-dev-17 libreadline-dev zlib1g-dev flex bison \
    libxml2-dev libxslt1-dev libssl-dev pkg-config
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH=/root/.cargo/bin:$PATH
RUN cargo install --locked cargo-pgrx --version ~0.18
RUN cargo pgrx init --pg17 /usr/bin/pg_config

COPY crates/pg_web_ext /src/pg_web_ext
COPY Cargo.toml /src/Cargo.toml
WORKDIR /src/pg_web_ext
RUN cargo pgrx install --release

# Runtime stage
FROM postgres:17
COPY --from=builder /usr/lib/postgresql/17/lib/pg_web_ext.so /usr/lib/postgresql/17/lib/
COPY --from=builder /usr/share/postgresql/17/extension/ /usr/share/postgresql/17/extension/
```

Publishing:
- `pgweb/postgres:latest` on Docker Hub
- `ghcr.io/<org>/pg-web-postgres:latest` on GitHub Container Registry
- Versioned tags: `pgweb/postgres:0.1.0`, `pgweb/postgres:pg17-0.1.0`

### CLI distribution

- `cargo install pg-web-cli` — crates.io
- `brew install <tap>/pg-web` — homebrew tap (TBD)
- Prebuilt binaries per release on GitHub Releases

## Versioning

SemVer. Breaking SQL schema changes bump minor or major.

Each bump ships a migration script `crates/pg_web_ext/sql/pg_web_ext--A.B--C.D.sql`. Users upgrade:

```
docker compose pull   # pulls new pgweb/postgres:latest
docker compose up -d  # reloads with new .so
psql> ALTER EXTENSION pg_web_ext UPDATE TO '1.1';
```

Postgres runs the migration script natively. User data untouched.

## Release checklist

Before tagging a release:

1. All phases' deliverables for this version are implemented.
2. `cargo pgrx test pg15`, `cargo pgrx test pg16`, `cargo pgrx test pg17` all green.
3. `cargo test -p pg_web_cli` green.
4. Companion app at `examples/todo/` runs end-to-end in CI against the Docker image.
5. `docs/ROADMAP.md` updated — deliverables checked off; new phase's "open questions" resolved if entering that phase.
6. `docs/ARCHITECTURE.md` updated if any public interface changed.
7. Migration SQL added if schema changed.
8. `CHANGELOG.md` entry.

## Debugging tips

- `cargo pgrx run pg17` drops you into psql with the extension loaded. Attach `rust-gdb` or `rust-lldb` to the Postgres backend PID for Rust-level breakpoints inside `#[pg_extern]` functions.
- For the background worker: find its PID via `SELECT pid FROM pg_stat_activity WHERE backend_type = 'pg_web_worker';` then attach.
- `RUST_LOG=pg_web_ext=trace` in the WSL shell before `cargo pgrx run` enables verbose tracing in the worker (once we wire up `tracing-subscriber`).
- Use Postgres's `auto_explain` extension alongside pg-web to log slow SPI queries: `SET auto_explain.log_min_duration = 100;`.
- If the worker crashes at boot, check Postgres's main log (`cargo pgrx run pg17 --log-level debug5`).

## Common pitfalls (annotated history)

These are real issues we hit during bringup. Re-read before debugging similar symptoms.

> The numbered write-ups below cover the earliest environment bring-up bugs. For the running list of every gotcha hit so far (BGW transaction wrappers, `#[pg_test]` error-propagation, synthesized-handler arity, rustc 1.95 `[DatumWithOid; N]` ICE, `cargo pgrx test` running integration tests it can't serve, etc.), see the **Gotchas** table in `docs/OVERVIEW.md` and per-session recaps under `docs/sessions/`.

### 1. `initdb: error: cannot be run as root`

Postgres refuses to let `initdb` run as the root user — a long-standing safety check. This breaks every `cargo pgrx test` and `cargo pgrx run` invocation because both call `initdb` to create a per-test / per-dev Postgres data directory.

**Fix:** create a dedicated non-root user (we use `pgweb`) and do all pgrx work as that user:
```bash
wsl -d Ubuntu-22.04 -u pgweb           # enter WSL as pgweb
```
If you're already in WSL as root, `su - pgweb` (the dash is important — login shell) gets you there.

### 2. `$PGRX_HOME does not exist`

pgrx stores local PG installs in `$HOME/.pgrx/`. If you run as root but have the installs in `/home/pgweb/.pgrx/`, pgrx looks in the wrong place.

**Fix:** same as above — run as the user that owns the `.pgrx` directory.

### 3. `unacceptable schema name "pg_web"` (SQLSTATE 42939)

Postgres reserves schema names starting with `pg_` for system catalogs (`pg_catalog`, `pg_toast`, etc.). `CREATE SCHEMA pg_web` — or any `pg_<anything>` — fails with `ERROR: unacceptable schema name` / `reserved_name`.

**Fix:** the framework schema is `pgweb` (no underscore). See `docs/ROADMAP.md` decision log (2026-04-17) for the full rationale.

### 4. `error while loading shared libraries: libpq.so.5: cannot open shared object file`

pgrx compiles Postgres with absolute `RUNPATH` paths pointing at the original `.pgrx` directory. If you ever **move** `~/.pgrx/` (e.g., to switch dev users), the compiled binaries can't find their own shared libs.

**Fix (fast):** re-stamp the RPATH on every binary and `.so` under the moved tree:
```bash
for v in 15.17 16.13 17.9; do
  LIB=/home/pgweb/.pgrx/$v/pgrx-install/lib
  find /home/pgweb/.pgrx/$v/pgrx-install/{bin,lib} -type f \
       \( -perm -u+x -o -name "*.so*" \) \
       -exec patchelf --set-rpath "$LIB" {} \; 2>/dev/null
done
```

**Fix (clean):** delete the moved `.pgrx` and re-run `cargo pgrx init --pg15 download --pg16 download --pg17 download` as the new user. Takes 20-60 min.

### 5. Git Bash on Windows mangles paths and eats `$variables` when calling `wsl`

When invoking `wsl` from Git-for-Windows bash, MSYS2's path-translation layer mangles Linux paths (`/home/x` becomes `C:/Program Files/Git/home/x`) and can swallow dollar-variable expansions inside `bash -c '...'`.

**Fix:** prefix with `MSYS_NO_PATHCONV=1`:
```bash
MSYS_NO_PATHCONV=1 wsl -d Ubuntu-22.04 -u pgweb -- bash -c 'cd $HOME/pg-web && cargo check'
```
For anything non-trivial, write a shell script file to `\\wsl$\Ubuntu-22.04\home\pgweb\...` and invoke it with `MSYS_NO_PATHCONV=1 wsl ... -- bash /home/pgweb/<script>.sh`.

**Related trap: do not interpolate `$PATH` into the inner command.** A tempting "fix up PATH inside WSL" approach — `bash -c "export PATH=$HOME/.cargo/bin:$PATH && cargo test"` — fails because the outer Git-Bash shell expands `$PATH` (and `$HOME`) **before** the inner WSL bash ever sees the string. The Windows PATH that ends up embedded contains entries like `/mnt/c/Program Files (x86)/Common Files/Oracle/...`; the parentheses then parse as subshell syntax inside the inner bash, which dies with `syntax error near unexpected token '('`. Either use absolute binary paths (see pitfall #12) or set a fresh, static PATH with no `$PATH` reference at all:
```bash
wsl -d Ubuntu-22.04 -u pgweb -- bash -c \
  'PATH=/home/pgweb/.cargo/bin:/usr/local/bin:/usr/bin:/bin cargo test'
```
Hit in Session 4 Component A.

### 6. `cargo pgrx run` doesn't auto-run `CREATE EXTENSION`

`cargo pgrx run pg17` installs the `.so` + `.control` and opens psql, but the extension is **not yet created** in that database. Schema/tables won't exist until you type:
```sql
CREATE EXTENSION pg_web_ext;
```
at the psql prompt. (`cargo pgrx test` does this for you automatically; `cargo pgrx run` does not.)

### 7. `.bashrc` changes don't apply to the current shell

Adding `. "$HOME/.cargo/env"` to `~/.bashrc` only affects **new** shells. In the current shell you need `source ~/.bashrc` (or `source ~/.cargo/env` to load just that one file). The next time you open a shell via `wsl -d Ubuntu-22.04 -u pgweb`, it'll auto-source.

### 8. Host `:8080` conflict between pgrx dev PG and the Docker container

Both the pgrx dev Postgres (via `cargo pgrx run` / `pg_ctl start`) and the scaffolded `docker-compose.yml` bind host port `8080`. If dev PG is already running, Docker's port mapping silently does **not** take effect — curl will hit the dev instance, not the container. Symptoms: `pg-web push` updates DB rows but `curl http://localhost:8080/` keeps serving old/unrelated content.

**`pg-web up` preflights this** since Session 3 Component D: it tries to bind `:8080` + `:5432` before calling `docker compose up -d`. If something non-Docker already holds the port, it bails with the exact fix command instead of letting compose silently get shadowed. If the existing holder is a Docker container (your own previous `pg-web up`), preflight recognizes that and proceeds idempotently.

**Manual fix** if you end up in the broken state anyway:

```bash
/home/pgweb/.pgrx/17.9/pgrx-install/bin/pg_ctl -D ~/.pgrx/data-17 -m immediate stop
# or:
cargo pgrx stop pg17

ss -tlnp sport = :8080

# Then: pg-web up (or docker compose up -d)
```

Use `-m immediate`, not `-m fast`. Session 4 Component A found that fast-stop hangs indefinitely against our BGW — `pg_ctl` prints dots for ~30 s and then gives up with `server does not shut down`. The tokio runtime inside `pg_web_ext`'s worker doesn't cleanly unwind when the postmaster sends SIGINT, so fast mode never drains. Immediate mode is the equivalent of SIGQUIT — crash-stop the cluster, skip the shutdown checkpoint. In dev that's fine: the pgrx data dir holds no data worth preserving, and the next `cargo pgrx run` will run recovery on startup.

Diagnose by running `ss -tlnp sport = :8080` — whichever `users:(...)` it prints tells you who owns the port. `docker-proxy` is fine (that's us); `postgres` or anything else is the culprit.

### 9. `notify-debouncer-full` re-exports `notify` but the `Watcher` trait is NOT in scope by default

`new_debouncer(...)` returns a `Debouncer<T, C>` whose `.watcher()` method returns `&mut T`. To actually call `.watch(path, recursive_mode)` on that watcher you need the `notify::Watcher` trait in scope — the method isn't inherent. The trait is re-exported via `notify_debouncer_full::notify::Watcher`, but rustc's error message (E0599 "no method named `watch`") doesn't mention it. Hit in Session 3 Component B.

**Fix:**
```rust
use notify_debouncer_full::notify::{EventKind, RecursiveMode, Watcher};
```

### 10. `pg-web dev` log tailing hardcodes the compose service name `postgres`

`dev.rs::spawn_logs_tail` shells out to `docker compose logs -f --no-log-prefix postgres`. The scaffolded `docker-compose.yml` names the service `postgres`; if a user renames it, `--logs` goes silently quiet (no lines) instead of erroring. The scaffold template is the contract — don't rename without updating `dev.rs`. A future enhancement could parse `docker-compose.yml` to discover the service at runtime.

### 11. `pgweb.pages__*(json) RETURNS json|text` is the reserved push-managed namespace

`push::push` owns every Postgres function matching `pgweb.pages__<name>(req json) RETURNS <json|text>` — both the ones it creates from user `.sql` files and the ones it synthesizes for static routes. Phase 3 of push (reconcile) **drops any such function not in the expected set** computed from the current filesystem walk. Framework maintainers and app authors alike must avoid that namespace for helpers. Safe helper patterns: `pgweb.helper_<name>(...)`, `pgweb.util_<name>(...)`, or any function whose argument list isn't exactly `(req json)`. The safety gate in `reconcile_handlers` also filters to functions returning `json` or `text`, so a function like `pgweb.pages_util(json) RETURNS int` would survive even inside the prefix — but that's not a guarantee to rely on.

### 12. `cargo` is not in PATH for non-interactive WSL shells under `pgweb`

`wsl -d Ubuntu-22.04 -u pgweb -- bash -c 'cargo test ...'` fails with `bash: line 1: cargo: command not found`. The `pgweb` user's `~/.bashrc` sources `~/.cargo/env` (which adds `~/.cargo/bin` to PATH), but `bash -c '...'` is a non-interactive non-login shell and does not source `.bashrc` at all. Swapping to `bash -lc '...'` isn't a reliable fix either: the login-shell startup path looks at `~/.profile`, and on a vanilla rustup install `.profile` may or may not end up sourcing `~/.cargo/env` depending on how `rustup-init` was answered.

**Fix:** use the absolute binary path. Every Rust toolchain binary lives at `/home/pgweb/.cargo/bin/<name>`:
```bash
wsl -d Ubuntu-22.04 -u pgweb -- bash -c \
  '/home/pgweb/.cargo/bin/cargo test -p pg_web_cli'
wsl -d Ubuntu-22.04 -u pgweb -- bash -c \
  'cd /home/pgweb/pg-web/crates/pg_web_ext && /home/pgweb/.cargo/bin/cargo-pgrx pgrx test pg17'
```
Same applies to `rustc`, `cargo-pgrx`, `rustup`, and anything else under `~/.cargo/bin`. Hit on every session's first tool invocation; Session 4 Component A finally wrote it down.

### 13. Docker image bakes in the install SQL — rebuild after every extension change

The repo-root `Dockerfile` (built by `scripts/build-image.sh`) runs `cargo pgrx install --release` inside the build stage, which compiles the extension AND copies the generated `pg_web_ext--<ver>.sql` into `/usr/share/postgresql/17/extension/` in the final image. Once baked, changes to `crates/pg_web_ext/src/schema.rs`, to any `sql/` file, or to anything else that alters install SQL do **not** invalidate the image — tier 3 (docker E2E via testcontainers) and tier 4 (smoke against the compose stack that `pg-web up` boots) will both happily run against the previous image and return green while exercising the *old* schema.

**Fix:** after any extension-crate change, rebuild the image before running tier 3 or tier 4:
```bash
bash scripts/build-image.sh
```
`scripts/test-all.sh` deliberately does **not** rebuild — a couple of minutes of image build on every test run would wreck the dev loop, so rebuild is on the author's conscience when the touched tier crosses the Docker boundary. Tier 1 (`cargo pgrx test`) and tier 2 (CLI unit/integration) test the current source tree directly and are immune. Hit in Session 4 Component A: the new `pgweb.html_escape` function passed tier 1 and tier 2b, then tier 3 ran against a pre-Component-A image where the function didn't exist, producing a baffling "the test calls it but the DB doesn't know it" until the build-image step was re-run.

### 14. Git Bash eats `$?` from `wsl -- bash -c '...'` — inner exit code is invisible

Running `wsl -d Ubuntu-22.04 -u pgweb -- bash -c 'cmd; echo $?'` from Git Bash does **not** report `cmd`'s exit code — you get `0` (or whatever the outer Git Bash's `$?` happens to be) regardless of whether the inner command succeeded or failed. MSYS2 bash's argument-passing layer performs early dollar expansion when invoking native (non-MSYS) executables like `wsl.exe`, so `$?` is resolved against the *outer* shell before the string ever reaches inner WSL bash. Standard Linux bash doesn't do this; it's MSYS2-specific. The real bite is silent false negatives: any shell-orchestrated test or script that gates on exit codes from WSL will look green when it should be red.

**Fix:**
- Escape the dollar so inner bash sees it: `wsl -u pgweb -- bash -c 'false; echo "exit=\$?"'` → `exit=1`.
- Persist the code to a file: `cmd > /tmp/out 2>&1; echo \$? > /tmp/rc; cat /tmp/rc`.
- Use explicit short-circuit logic instead of reading `$?`: `cmd && echo ok || echo fail`.
- Put the whole thing in a `.sh` script under `/home/pgweb/...` and invoke that — no outer-shell expansion to worry about.

Hit in Session 4 Component E: spent ~20 min chasing why `pg-web check`'s `return Ok(ExitCode::FAILURE)` seemingly didn't propagate, even auditing Rust's `ExitCode`/`Termination` wiring. The code was correct — Git Bash was eating the `$?`. Narrower than #5 (which covers the broad path-translation / variable-eating class); this one is specifically about exit-code visibility so a future maintainer grepping for "exit code" lands here fast.

### 15. Watcher code paths that `strip_prefix` `app_dir` must canonicalize first — or silently ignore every event

`pg-web dev` runs with a default `--dir .` from clap. Inside `dev::watch`, the debouncer delivers `notify` events whose paths are **absolute** (the kernel joins the watched directory with the inotify event name, and that join produces an absolute path once the current working directory is resolved). `classify(path, app_dir)` then does `path.strip_prefix(app_dir)` to get the relative-under-app portion. With `app_dir = "."` and an event path of `/tmp/my-todos/pages/index.html`, `strip_prefix(".")` returns `Err` — the watcher classifies the event as `Action::Ignore`, no push fires, no NOTIFY fires, and to the user the dev loop looks alive (it prints `⟳ watching pages/ + public/`) while silently doing nothing on every save.

**Fix:** canonicalize `app_dir` once at the `dev::dev` entry point, before anything else touches it:
```rust
let app_dir_buf = app_dir.canonicalize()
    .with_context(|| format!("resolving app directory {}", app_dir.display()))?;
let app_dir = app_dir_buf.as_path();
```
Absolute ↔ absolute `strip_prefix` just works after that.

**Test-coverage lesson** (this is the bigger takeaway): the `classify` unit tests built paths under a hardcoded `/app` via `fn cwd(rel: &str) -> PathBuf { PathBuf::from("/app").join(rel) }` — every one of them fed an *absolute* `app_dir`. The tier-3 `dev_watcher_repushes_on_save` used `tempfile::tempdir()`, which returns an absolute path. Nothing exercised the shape the CLI actually produces at runtime (`PathBuf::from(".")`). Unit tests should cover the default CLI invocation, not just whatever the fixture happens to hand in; regression tests now pin this (`classify_ignores_absolute_event_when_app_dir_is_relative` + `classify_matches_under_canonical_app_dir`).

Hit in Session 4 post-release: the user started `pg-web dev` from their project root, edited a file, got no push, no livereload, no visible dev-loop activity. The bug had been in every component's validation cycle but was invisible because I only ever tested the relative-path case via the test suite (which used absolute paths). Fixed in commit `09054fa`.

### 16. `tee` masks pipeline exit codes — background script runs look green when they failed

When running a `set -e` script in the background and piping its output through `tee` for log capture, the wrapping shell reports `tee`'s exit (always `0`) instead of the script's. The script can fail loud — `cargo: command not found`, build error, `set -e` triggered — and the supervising tool will be told the run "completed (exit code 0)". This bit twice in Session 5: once when `cargo` wasn't on the non-interactive WSL PATH at tier 1, once when the Docker builder failed at the `examples/todo/` `include_dir!` step.

**Fix — never put `tee` at the end of a pipeline whose exit code matters.** Either:

```bash
# A. Capture exit code explicitly, then read the log.
bash scripts/test-all.sh > /tmp/test-all.log 2>&1; echo "EXIT=$?"; tail /tmp/test-all.log

# B. Set pipefail and use `tee` (works in interactive bash, but the
#    Bash tool's outer shell doesn't always honor it — option A is
#    safer for unattended runs).
set -o pipefail
bash scripts/test-all.sh 2>&1 | tee /tmp/test-all.log
```

Hit in Session 5 while running `test-all.sh` and `build-image.sh` via background subagents. The Monitor tool's filter passed because no failure tokens appeared on stdout (the tee saw the partial output before the script bailed); only verification via tail-of-log + explicit exit-code check caught the real state.

### 17. rustc 1.95 ICE in `mir_borrowck` is usually a missed `let mut`

A panic stack trace ending in `mir_borrowck` and `query stack during panic` looks like an ICE on the surface, but in at least one case (Session 5 F.3 docker test) the underlying error was the ordinary "cannot borrow immutable binding as mutable" diagnostic — swallowed by the panic instead of printed. If you hit this on test code, look for `let foo = container.exec(...)` (or any value bound `let foo = ...`) where you then call a `&mut self` method on `foo` (`foo.stdout_to_vec()`, `foo.stderr_to_vec()`). Add `let mut` and the ICE goes away.

Different from pitfall #2's similar-looking macro-expansion ICE on `[DatumWithOid; 2]` shapes — that one needed the `format!`-then-Rust-side-escape workaround. This one is a binding-mutability miss that the compiler should have shown as a friendly error.

### 18. Stale `pg-web up` containers shadow `:8080` and silently break the pgrx dev PG's BGW

The pgrx dev PG runs on port `28817` for SQL but its BGW binds `:8080` for HTTP — same port the Docker stack publishes. If a previous `pg-web up` left a container running, it holds `:8080`, the dev PG starts cleanly but the BGW silently fails to bind (only logged, no abort), and `scripts/test-http.sh` sees the *container's* HTTP responses instead of the pgrx PG's seeded template.

Symptom: tier-2a HTTP smoke fails with a body that doesn't match the seeded template — the container is serving something else (a previously-pushed app) over `:8080`.

**Fix:** `docker ps` to spot leftover containers; `docker stop <name>` (or `pg-web down` from the original app dir) to free `:8080`; rerun the smoke. The pgrx-dev-PG-vs-Docker port conflict is also covered by pitfall #8 from the other angle (the Docker side losing); this entry covers the dev-PG-loses-silently variant.

The Session 4 G `application_name` tagging from Component L of Session 5 makes this easier to spot now: `SELECT pid, application_name FROM pg_stat_activity` shows the in-container BGW connection alongside any host pg-web clients. If you see backends with `application_name = ''` or `'pg-web *'` from a `client_addr` you didn't expect, you've found a shadow.

**Tier 2a now catches this automatically.** `scripts/test-http.sh` runs a port-shadow preflight after `pg_ctl start` — it extracts the listener PID via `ss -tlnp 'sport = :8080'`, verifies the cmdline (`ps -p $pid -o args=`) contains `pg_web_worker`, and bails loud with a `docker stop <name>` suggestion if not. Same idea would help if applied to `pg-web up`'s preflight too (see pitfall #8).

