# Contributing to pg-web

Thank you for your interest! pg-web is a small, opinionated project: PostgreSQL *is* the web server. The invariants in `CLAUDE.md` exist to keep the implementation simple, correct, and true to that thesis.

## Before you start

1. Read `CLAUDE.md` (the north-star). It contains the architectural invariants, coding practices, commit style, and "every feature ships with a companion-app flow" rule.
2. Skim the public docs (`docs/APP-DEVELOPER-GUIDE.md`, `docs/APP-LAYOUT.md`, `docs/OVERVIEW.md`) so you understand the experience we are building for app developers.
3. If you are changing framework behavior, you will almost certainly need to add or update a path in `examples/todo/` (the tier-3 / tier-4 acceptance target) and keep `docs/TUTORIAL.md` in sync.

## Development

- Full maintainer setup (WSL2 + dedicated `pgweb` user + pgrx + the five-tier suite) lives in `docs/internal/DEVELOPER-GUIDE.md` and `docs/internal/HANDOFF.md`.
- One-command entry for the test matrix: `scripts/test-all.sh` (requires Docker for tier 3).
- Before any non-trivial PR: run `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and the relevant `cargo pgrx test pgXX`.
- `pg-web check` must still pass against `examples/todo/` (and any new companion flows).

## What "done" looks like

- Implementation in `crates/pg_web_ext/` or `crates/pg_web_cli/`.
- Tier 1 (`#[pg_test]`) or Tier 2 (CLI/HTTP) coverage where appropriate.
- A real exercised path in `examples/todo/` (or the `pg-web.dev` docs-site app) that a human can click through.
- Docs updated (public docs for user-visible behavior; internal notes + sessions/ for rationale).
- `scripts/test-all.sh` green.

Phase discipline matters: do not introduce Phase 2+ concepts (auth, jobs, dashboard) into Phase 1 code paths.

## Commit style

- Short imperative subject (≤72 chars).
- Conventional prefixes: `feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`.
- Body (optional) explains *why*, not just *what*.
- **No `Co-Authored-By: Claude ...` trailers** (or equivalent). The project has opted out.

## Pull requests

- Small and focused is preferred.
- Link the relevant section of `docs/ROADMAP.md` or a session note when the change touches a planned item.
- If the change affects the public surface (CLI UX, handler contract, layout rules, error behavior, deploy story), the PR description should let a newcomer answer "how does this affect someone who just did `cargo install pg-web`?"
- For docs-only changes: still run the check + smoke paths that the changed docs describe.

## Reporting issues

Please include:
- `pg-web --version` (or the image tag)
- `pgweb.settings.env` mode (dev vs prod)
- The smallest reproduction (a `pg-web init` + a few files + the exact request)
- Output of `pg-web check` if relevant

For agent-reported issues or improvement ideas, the long-term plan is an MCP surface + shared board (see `docs/ROADMAP.md` parking lot). For now, open a regular GitHub issue with context.

## Governance / scope

pg-web is currently a single-maintainer project (rt96) with heavy AI-collaborator usage during sessions. We are deliberately small until the Phase 1 contract is rock-solid and the dogfooded `pg-web.dev` site proves the model in public.

We are happy to review well-scoped contributions that respect the invariants. Large new directions (new phases, managed-DB support, client framework integrations, etc.) should be discussed first via issue or the roadmap parking-lot items.

Welcome, and thank you for helping keep the "database is the application" idea crisp and production-grade.
