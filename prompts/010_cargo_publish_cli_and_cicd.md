# 010 — Cargo / crates.io publishing for the CLI + CI/CD automation (including release integration)

**Status:** Handoff prompt — ready to execute  
**Priority:** High (required for "getting it on cargo so it can be used directly")  
**Date / Context:** Post v0.2.0, fully open source decision (MIT OR Apache-2.0), preparing for public launch. We want normal Rust developers to be able to do `cargo install pg-web` and immediately get the `pg-web` binary that drives the entire developer experience.

## Critical Background (Read These First)

- `CLAUDE.md` — invariants (especially extension ↔ CLI are strictly decoupled; the extension has zero filesystem code).
- Current crate layout:
  - Workspace root `Cargo.toml` (version 0.2.0, license = "MIT OR Apache-2.0", repository set).
  - `crates/pg_web_cli/Cargo.toml` — package name `pg_web_cli`, `[[bin]] name = "pg-web"`.
  - `crates/pg_web_ext/Cargo.toml` — cdylib for pgrx, not intended for normal crates.io consumption.
- Existing CI:
  - `.github/workflows/ci.yml` — runs the full 5-tier `scripts/test-all.sh` (which builds the Docker image) on every push/PR. Caches pgrx + Rust.
  - `.github/workflows/release.yml` — on `v*` tags, re-uses CI, then builds + pushes the Docker image `pgweb/postgres:<tag>` and `:latest` (guarded by `DOCKERHUB_TOKEN` secret).
- Dockerfile already bakes the CLI binary into the image (`/usr/local/bin/pg-web`) for the "CLI inside the container" story (F.3).
- `scripts/test-all.sh`, `scripts/build-image.sh`, `scripts/smoke-cli.sh`.
- `docs/OVERVIEW.md`, `docs/DEPLOYMENT.md`, `examples/todo/`.
- The extension (`pg_web_ext`) is **never** installed via `cargo install` by end users. It is delivered exclusively via the `pgweb/postgres` Docker image (or manual `cargo pgrx install` for framework developers). The only thing we publish to crates.io is the **CLI**.

## The Desired End-User Experience

```bash
# The ideal, simple command we are aiming for
cargo install pg-web

# Then the user immediately has:
pg-web --version
pg-web init my-app --template todo
pg-web dev
pg-web push
pg-web migrate apply
# etc.
```

After this work:
- `cargo install pg-web` must succeed and place a working `pg-web` binary in PATH.
- The published crate on crates.io must have excellent metadata (description, readme, license, keywords, categories, repository, documentation link once we have the site).
- Publishing must be safe and automated.

## What Must NOT Happen

- Do **not** attempt to publish `pg_web_ext` as a normal crate. It is a pgrx cdylib. Users do not `cargo add` it.
- Do not change the extension name (`pg_web_ext` for `CREATE EXTENSION` and `shared_preload_libraries`).
- Do not break the existing Docker image build that also ships the CLI inside the container.
- Do not require users to have a full pgrx dev environment just to use the CLI.

## Goals for This Prompt

1. Make the CLI publishable under the friendly name `pg-web` on crates.io so `cargo install pg-web` works.
2. Add all required / recommended `[package]` metadata so the crates.io page looks professional.
3. Integrate publishing into CI/CD so that:
   - On every PR / push we at least do a `--dry-run` publish (or a check that would publish).
   - On version tags (`v*`) we perform a real publish to crates.io (after the existing test + Docker image steps succeed).
4. Keep the release process simple and safe (secrets, guards, manual approval if desired).
5. Update documentation (especially the new root README from prompt 009) to describe the `cargo install` path accurately, while still explaining that the **runtime** (the HTTP server) comes from the Docker image.
6. Make sure `cargo install pg-web` produces a binary that can at least run `init`, `check`, and basic stack commands against a reachable Postgres (full E2E still requires Docker for most users).

## Success Criteria

- `cargo search pg-web` (or direct crates.io link) shows a nice page for the CLI with correct description, license, repo, etc.
- A fresh machine with only Rust installed can run `cargo install pg-web` and then successfully execute `pg-web init ...`, `pg-web check`, and `pg-web --help`.
- On a `git tag vX.Y.Z && git push --tags`, after the existing test job and Docker image publish succeed, the CLI crate is published to crates.io under the matching version.
- The published crate version matches the workspace version and the Docker image tag.
- No accidental publishes from forks or PRs.
- The workspace still builds and all existing tests (including the Docker E2E tier) continue to pass.
- Documentation (README + relevant docs/ files) clearly explains the split: "Install the CLI with cargo. Run your apps with the pgweb/postgres Docker image."
- `cargo publish --dry-run -p pg-web` (or equivalent) passes cleanly in CI.

## Concrete Changes & Tasks

### 1. Crate Metadata & Naming (in `crates/pg_web_cli/Cargo.toml`)

- Change the package name from `pg_web_cli` to `pg-web` (this is what users will type in `cargo install pg-web` and what appears on crates.io).
- Add / expand the `[package]` section with (at minimum):
  - `description` — short, compelling, matches the vision ("PostgreSQL as a self-contained web application platform. The CLI for scaffolding, developing, and deploying pg-web apps.")
  - `readme` — point at the root `README.md` (or a dedicated one if we decide on a different strategy).
  - `license` (already inherited) — "MIT OR Apache-2.0"
  - `repository` (already inherited)
  - `homepage` — once we have `https://pg-web.dev`
  - `documentation` — link to the docs site or the best guide.
  - `keywords` — e.g. ["postgres", "web", "htmx", "tera", "pgrx", "fullstack", "cli"]
  - `categories` — appropriate ones from crates.io (command-line-utilities, web-programming, database, etc.)
  - `authors` or rely on the repo (optional but nice)
  - `rust-version` if we want to declare a minimum (check current MSRV from CI/toolchain).
- Keep the `[[bin]] name = "pg-web"` exactly as-is. The published package name and the binary name can (and should) differ from the directory name.
- The internal lib name (`pg_web_cli`) can stay for now or be updated for consistency — it doesn't affect the published artifact much.
- Update the workspace root `Cargo.toml` if any fields need to move up (version, edition, license, repository are already there via `[workspace.package]`).

### 2. Make the Root README Suitable for crates.io

- The `readme` field usually points at the root `README.md`. Make sure it contains a good "this is the CLI for pg-web" section near the top and the full quickstart.
- Crates.io will render the first few hundred characters + any `<!-- cargo readme -->` style markers if we use the `cargo-readme` tool, but a clean hand-written README is fine.
- Coordinate with prompt 009 (docs cleanup) — the README created there must serve both GitHub visitors and crates.io visitors.

### 3. CI/CD Changes — Publishing the CLI

We already have a solid pattern in `release.yml` (tag-driven, re-uses CI, secret-guarded publish).

Recommended approach (extend the existing release flow):

- Add a new job (or reuse/extend) called something like `publish-cli` in the release workflow (or a dedicated `publish.yml`).
- It must run **after** the `test-all` job (the existing one that builds the Docker image and runs the full suite).
- Use the official `cargo` tooling or a trusted action.
- Guard the actual publish with a secret (e.g. `CARGO_REGISTRY_TOKEN` containing a crates.io API token with publish permission for the owner).
- Only publish on real tags (not PRs, not forks).
- Publish **only** the CLI crate: `cargo publish -p pg-web --token $CARGO_REGISTRY_TOKEN` (or equivalent).
- Also run a non-publishing verification step on every PR: something like `cargo publish --dry-run -p pg-web` (this requires the token to be present or use `--dry-run` which often doesn't need auth for the check phase, or we can do `cargo package --list` + a build check).
- Consider a manual "publish" workflow dispatch for emergency/hotfix publishes that bypasses the tag gate (with strong confirmation).
- Update the release.yml comments and the root README to document the new crates.io publishing path alongside the Docker Hub path.

Example skeleton to implement (adapt to exact style of the existing release.yml):

```yaml
publish-cli:
  name: publish pg-web to crates.io
  needs: [test-all, publish-image]   # or just test-all if image and cli are independent enough
  if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - name: Publish to crates.io (guarded)
      if: env.CARGO_REGISTRY_TOKEN != ''
      run: cargo publish -p pg-web --token ${{ secrets.CARGO_REGISTRY_TOKEN }}
      env:
        CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

Also add a dry-run or "would publish" check inside the normal CI job for the CLI crate on every PR.

### 4. Versioning & Tagging Discipline

- The workspace version (currently 0.2.0) drives everything.
- We already have a nice CHANGELOG.md. Keep updating it on releases.
- Tagging `v0.3.0` (or whatever) should eventually result in:
  1. Full test suite (existing).
  2. Docker image published (existing).
  3. `pg-web` crate published to crates.io at the same version (new).
- Document in release process notes (perhaps in a small `RELEASE.md` or in the README) the exact order and what the owner must have configured (Docker Hub + crates.io tokens).

### 5. Documentation & User-Facing Updates

- In the root README (and any "install" section of the docs):
  - Show `cargo install pg-web` as the primary way to get the CLI.
  - Immediately explain the runtime: "The CLI is a management tool. Real applications run inside the `pgweb/postgres` Docker image (which contains both Postgres and the `pg_web_ext` extension)."
  - Give the one-line "production" path using the image.
- Update any references in `docs/DEPLOYMENT.md`, `HANDOFF.md` (internal), `scripts/`, etc. that hard-code the old install instructions.
- Add a note about MSRV / supported Rust versions if we pin one.
- Once published, we can add a crates.io badge.

### 6. Testing the Publish Path

- Use `cargo publish --dry-run -p pg-web` locally and in CI before the real thing.
- After the first real publish, verify on a clean machine (or in a throwaway container) that `cargo install pg-web@<exact-version>` works and the binary behaves.
- The existing `scripts/smoke-cli.sh` and tier-2b/4 tests should continue to cover the CLI surface.
- Consider adding a lightweight "install from crates.io and run basic commands" step in CI (can be slow, so perhaps only on release or as a separate job).

### 7. Other Polish

- Decide whether we want a separate `pg-web-cli` crate name as an alias or redirect (usually not necessary once `pg-web` is claimed).
- Make sure the binary installed via cargo has the same version reporting as the one baked in the Docker image (it should, since they come from the same source).
- If we ever want to publish pre-built binaries (GitHub Releases, Homebrew, etc.) later, the Cargo path is the foundation — document that this is the Rust-native distribution.

## Constraints & Invariants

- The two crates remain strictly decoupled. Publishing the CLI must not create any new dependency from CLI to the extension crate at build time (the current include_dir + Dockerfile approach for templates is fine).
- All existing five-tier tests must keep passing.
- The Docker image build (which also compiles the CLI) must not be broken.
- Publishing must be gated so that a bad tag or a fork cannot publish garbage.
- Follow conventional commit style and the project's existing release notes discipline.
- Because this is the public distribution mechanism, any change here has high visibility — be conservative.

## Order of Operations (Suggested)

1. Update the CLI crate metadata and package name (`pg-web`).
2. Ensure the root README is in good shape for crates.io (coordinate with prompt 009).
3. Add the dry-run / verification step to the normal CI workflow.
4. Extend `release.yml` (or add a small new job) for the real crates.io publish on tags, reusing the test job.
5. Add the necessary secret (`CARGO_REGISTRY_TOKEN`) guidance in the workflow comments and in internal handoff docs.
6. Update all user-facing install instructions.
7. Locally test `cargo package` / dry-run.
8. (When owner is ready) do the first real tag + publish and verify end-to-end on a clean machine.
9. Update CHANGELOG, README badges, and any "installation" sections in the docs.

## Files You Will Almost Certainly Touch

- `crates/pg_web_cli/Cargo.toml`
- `.github/workflows/ci.yml`
- `.github/workflows/release.yml` (or a new publish workflow)
- Root `README.md` (created by the docs cleanup prompt)
- `docs/DEPLOYMENT.md` and possibly `docs/OVERVIEW.md`
- `Cargo.toml` (workspace) if metadata needs lifting
- Possibly `scripts/` or a small `RELEASE.md` for the combined Docker + crates release process

## Deliverables

- A publishable `pg-web` crate on crates.io (first publish can be done as the final step of this work).
- Automated, safe CI/CD that publishes the CLI on the same tags that publish the Docker image.
- Clear, accurate documentation for users about how to obtain the CLI via Cargo and what the relationship is to the runtime Docker image.
- All existing tests and release processes continue to function.
- The owner has the required crates.io token configured (document the exact scopes needed).

## Post-Publish Notes for the Owner

- You will need a crates.io account and an API token with "publish" permission for the `pg-web` crate (or the account that will own it).
- Claim the `pg-web` name early if it is still available.
- After the first publish, monitor the crates.io page and consider adding a "lib" or "bin" classification if needed.
- Future minor versions can be published the same way.

When the CLI is publishable via `cargo install pg-web`, the dry-run and tag-driven publish paths are wired into CI, and the documentation tells the correct story, mark this prompt complete and add a short recap (including the exact first published version).

**End of prompt 010.**
