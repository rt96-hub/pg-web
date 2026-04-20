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
- **Product behavior** → add a flow in `examples/demo/` (the companion app is THE acceptance gate).

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
4. Companion app at `examples/demo/` runs end-to-end in CI against the Docker image.
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

**Fix:** stop one of them before starting the other.

```bash
# Stop dev PG
/home/pgweb/.pgrx/17.9/pgrx-install/bin/pg_ctl -D ~/.pgrx/data-17 -m fast stop
# or:
cargo pgrx stop pg17

# Check that nothing's still on :8080
ss -tlnp | grep 8080

# Then: docker compose up -d
```

Diagnose by running `ss -tlnp | grep 8080` — whichever `users:(...)` it prints tells you who owns the port.

### 9. `notify-debouncer-full` re-exports `notify` but the `Watcher` trait is NOT in scope by default

`new_debouncer(...)` returns a `Debouncer<T, C>` whose `.watcher()` method returns `&mut T`. To actually call `.watch(path, recursive_mode)` on that watcher you need the `notify::Watcher` trait in scope — the method isn't inherent. The trait is re-exported via `notify_debouncer_full::notify::Watcher`, but rustc's error message (E0599 "no method named `watch`") doesn't mention it. Hit in Session 3 Component B.

**Fix:**
```rust
use notify_debouncer_full::notify::{EventKind, RecursiveMode, Watcher};
```

### 10. `pg-web dev` log tailing hardcodes the compose service name `postgres`

`dev.rs::spawn_logs_tail` shells out to `docker compose logs -f --no-log-prefix postgres`. The scaffolded `docker-compose.yml` names the service `postgres`; if a user renames it, `--logs` goes silently quiet (no lines) instead of erroring. The scaffold template is the contract — don't rename without updating `dev.rs`. A future enhancement could parse `docker-compose.yml` to discover the service at runtime.

