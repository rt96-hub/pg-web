# Handoff — moving development to a new machine

Last updated: 2026-04-25 (end of Session 5 / `v0.2.0` + Session 6 spec drafted).

This file is the cold-start orientation for picking pg-web back up on a fresh box. Read top to bottom on first arrival; everything else is linked from here.

---

## What this repo is

A single Cargo workspace containing both halves of the framework:

```
pg-web/
├── crates/
│   ├── pg_web_ext/        # The Postgres extension (cdylib via pgrx)
│   └── pg_web_cli/        # The `pg-web` CLI (init / push / dev / migrate / env / check / up / down)
├── docs/                  # All documentation (start with OVERVIEW.md)
├── examples/todo/         # Companion app — the acceptance gate for every framework feature
├── docker/                # Image entrypoint scripts
├── scripts/               # test-all.sh, test-http.sh, build-image.sh, smoke-cli.sh
├── Dockerfile             # Builds pgweb/postgres:latest (PG 17 + extension + CLI)
└── Cargo.toml             # Workspace root
```

Yes — one repo, both crates. The extension and CLI evolve together; they talk via framework-table upserts in `pgweb.*`, never via a shared crate or RPC.

## Where the project is right now

- **Released:** `v0.2.0` (2026-04-25). Tag not yet pushed; the CI release workflow fires on `git tag vX.Y.Z && git push origin vX.Y.Z`.
  It runs the full test suite (incl. `cargo publish --dry-run -p pg-web`), builds+pushes the Docker image (if DOCKERHUB_* secrets present), then publishes the CLI crate to crates.io (if CARGO_REGISTRY_TOKEN secret present). See `.github/workflows/release.yml` for exact guards + order.
- **Status:** all five test tiers green at 230 Rust tests + 19-section black-box smoke.
- **Feature surface (`v0.2.0`):** full Phase-1 framework — schema in `pgweb.*`, BGW HTTP on `:8080`, directory-as-route layout, `(req json) RETURNS json|text` handler contract, dynamic routes, dev-mode error page, browser livereload via SSE, static assets (BYTEA, 20 MiB cap, fingerprinted URLs + `Cache-Control: immutable` in production), CLI bundled in the Docker image, push retry on concurrent DDL with `pg_stat_activity` diagnostic, `application_name` tagging on every CLI connection.
- **Next session (drafted, not started):** Phase 2 — auth + RLS bridge + realtime SSE — spec lives at `docs/sessions/session_6.md`. 20 open design questions are waiting on user decisions before implementation starts.

For the full picture in 30 seconds: `docs/OVERVIEW.md`. For phase-by-phase context: `docs/ROADMAP.md`. For the in-flight Phase 2 plan: `docs/sessions/session_6.md`.

---

## Bootstrap on a fresh machine

The reference dev environment is **WSL2 Ubuntu-22.04** with a dedicated `pgweb` user. Native Windows pgrx development is painful and not supported; macOS/Linux native works but our scripts assume the `pgweb`-user-in-WSL layout.

### 1. WSL2 + dedicated user

```bash
# As your normal Windows admin
wsl --install -d Ubuntu-22.04

# Inside WSL, as root
apt update && apt install -y build-essential libclang-dev libreadline-dev \
    zlib1g-dev flex bison libxml2-dev libxslt1-dev libssl-dev pkg-config \
    ccache patchelf curl git docker.io iproute2

# Create the project user and switch to it
useradd -m -s /bin/bash pgweb
usermod -aG docker pgweb
su - pgweb
```

Postgres's `initdb` refuses to run as root, so all pg-web work happens under the `pgweb` user. `/home/pgweb/pg-web` is the canonical project path; `/home/pgweb/.pgrx/` holds the local PG installs.

### 2. Rust toolchain + pgrx

As `pgweb`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
source ~/.cargo/env

cargo install --locked cargo-pgrx --version =0.18.0
cargo pgrx init --pg17 download
echo "shared_preload_libraries = 'pg_web_ext'" >> ~/.pgrx/data-17/postgresql.conf
```

`cargo-pgrx 0.18.0` is pinned to match the `pgrx` version in `Cargo.toml`. If the toolchain version drifts, expect `cargo pgrx install` to fail.

`shared_preload_libraries` is what makes the BGW load at PG startup — without it, the HTTP server never binds.

### 3. Clone the repo

```bash
cd ~
git clone https://github.com/rt96-hub/pg-web.git
cd pg-web
```

The repo is **private** — clone needs authentication. Two options:

- **`gh` CLI (recommended):** `gh auth login` once, then `gh repo clone rt96-hub/pg-web ~/pg-web` does the rest.
- **HTTPS with PAT:** generate a personal access token (Settings → Developer settings → Tokens, `repo` scope), then `git clone https://<token>@github.com/rt96-hub/pg-web.git`. Don't persist the token in the URL — `git remote set-url origin https://github.com/rt96-hub/pg-web.git` after the initial clone, then let `gh auth setup-git` or a credential helper handle subsequent pushes.

If you're picking up where Session 5 left off, the latest commit subject should be `chore(scripts): tier-2a port-shadow preflight + tier-3 auto-rebuild` (followed by the session_6.md draft commit). `git log --oneline | head -10` orients quickly.

### 4. First run — verify everything works end-to-end

```bash
# Build the runtime image (cold: 5-10 min; subsequent builds layer-cached)
bash scripts/build-image.sh

# Full five-tier suite — tier 1 SQL, tier 2a HTTP smoke, tier 2b CLI, tier 3 docker E2E, tier 4 black-box smoke
bash scripts/test-all.sh
```

`test-all.sh` auto-rebuilds the Docker image if extension source has changed since the last build (Session 5 feature). If anything fails, the script bails with a tier label so the diagnostic is obvious.

Expected end-state: `All tests passed.` Anything else is a real find.

### 5. Try the demo by hand

```bash
cd /tmp
~/pg-web/target/debug/pg-web init demo
cd demo
~/pg-web/target/debug/pg-web up        # docker compose up -d under the hood
~/pg-web/target/debug/pg-web migrate apply
~/pg-web/target/debug/pg-web push
curl http://localhost:8080/
~/pg-web/target/debug/pg-web down
```

Edit `demo/pages/index.html` or `demo/pages/index.sql`, re-run `push`, refresh. That's the dev loop.

For the full validation playbook including L / F.3 / H / I checks, walk through `docs/sessions/session_5_validation.md`.

---

## Cross-machine gotchas (the short list)

A longer (but curated) list of gotchas lives in `docs/internal/DEVELOPER-GUIDE.md`. Many environment-specific bring-up issues are WSL/Git-Bash specific and have been trimmed from the guide — see the git history or `docs/internal/sessions/` if you're on a similar setup and hit something weird. The ones you'll trip on first in a typical mixed pgrx + Docker workflow:

| Symptom | Cause | Fix |
|---|---|---|
| `initdb: cannot be run as root` | PG safety check | Use the `pgweb` user, not root |
| `libpq.so.5: cannot open shared object file` | RPATH baked at compile time | `patchelf --set-rpath` (rare; pitfall #4) |
| `:8080` already in use | Stale `pg-web up` container or pgrx dev PG conflict | `docker ps` → `docker stop <name>` (pitfalls #8, #18) |
| `tee` reports success when script failed | Pipeline exit-code masking | Capture exit separately: `cmd > log; echo EXIT=$?; tail log` (pitfall #16) |
| `cargo: command not found` (when called from non-interactive shells) | cargo not in default PATH | Use `/home/pgweb/.cargo/bin/cargo` explicitly (pitfall #12) |
| Git Bash on Windows eats `$?` through `wsl -- bash -c '...'` | Outer shell expands `$` before passing | Escape: `\$?` (pitfall #14) |

---

## Open work pointers

- **Phase 2 spec** — `docs/sessions/session_6.md`. Draft with 20 open questions on auth/RLS/realtime/CSRF; needs user decisions before implementation.
- **Session 5 deferred:**
  - `pg-web push --target <name>` SSH-tunneled remote deploy (F.2) — design intact in `docs/sessions/session_5.md` § F.2; waits on real remote infra.
  - True `pg_largeobject` streaming for assets >20 MiB — Phase 2+ work.
- **Roadmap-locked but not started:**
  - `pg-web migrate create` — native-Rust SQL diff against `schema/*.sql`. Approach locked 2026-04-25 (ROADMAP commit `552aa04`); implementation punted.
- **Parking lot** (no phase yet): SSH deploy sidecar, LLM-native knowledge base + agent skill, app testing framework (`pg-web test`). All in `docs/ROADMAP.md` § Parking lot.

---

## Workflow conventions

Captured in the per-session `docs/sessions/` files and in your auto-memory; the load-bearing rules:

- **No `Co-Authored-By: Claude ...` trailer on commits.** Conventional-style subjects: `feat(cli):`, `fix:`, `docs(dev-guide):`, etc.
- **Auto-pilot through implementation; stop only for design decisions** (Session 5 refinement). Land a component cleanly, verify all five tiers green, commit, move to the next. Session-end deliverable is an `expected-behaviors` doc the user can walk through.
- **Companion-app coverage per feature** — `examples/todo/` is the acceptance gate. New framework features need a corresponding demo path.
- **Bias toward *why* in inline comments**, not *what*. Well-named symbols document themselves.
- **`pgweb.pages__*(json) RETURNS json|text`** is the reserved push-managed namespace — user helpers must use a different signature pattern.
- **Docker image bakes install SQL + the .so + the CLI binary** — `scripts/test-all.sh` now auto-rebuilds when extension source / Dockerfile / CLI source changes (Session 5 feature). `scripts/build-image.sh` is only for framework developers changing the extension. End users never run it — they get the published `pgweb/postgres` image from Docker Hub via `cargo install pg-web` + `pg-web up`.

## Release process & tokens (post-010)

Tagging a `v*` version triggers:

1. Full CI (test-all + dry-run publish check).
2. Docker image build + optional push to Docker Hub (`DOCKERHUB_USERNAME` + `DOCKERHUB_TOKEN`).
3. CLI crate publish to crates.io (`CARGO_REGISTRY_TOKEN`).

The two publish jobs are independent (one can succeed while the other skips if its secret is absent). Configure the secrets in GitHub repo → Settings → Secrets and variables → Actions.

- `CARGO_REGISTRY_TOKEN`: crates.io API token (https://crates.io/settings/tokens) with **publish** permission. The account must control the `pg-web` crate name. Use the minimal-scope token.
- `DOCKERHUB_*`: for `pgweb/postgres` image.

Never commit tokens. The workflows are written to no-op gracefully when secrets are missing (useful for forks and pre-config validation). Update CHANGELOG.md + the version in `Cargo.toml` (workspace) before tagging. The published crate version, image tag, and workspace version must match.
