# pg-web — Internal / Maintainer Documentation

This directory holds material intended for **framework maintainers and project historians**, not for people building applications with pg-web.

## Who should read here

- You are contributing code, docs, or architecture to the pg-web crates themselves.
- You need the deep history, agent instructions, or per-session working notes.
- You are onboarding as a maintainer and need the cold-start handoff or WSL/pgrx environment details.

## What lives here

- `CLAUDE.md` (copied at repo root for easy agent discovery; authoritative copy can live here too)
- `DEVELOPER-GUIDE.md` — maintainer environment, pgrx workflows, packaging, common pitfalls
- `HANDOFF.md` — cold-start instructions for moving development to a new machine
- `sessions/` — detailed per-session plans, recaps, validation playbooks, and decision notes (gold for archaeology, noisy for first-time visitors)
- `prompts/` (if moved) — numbered technical-debt / improvement prompts used during development

## For app developers (most visitors)

Start at the repo root `README.md`, then:

- `docs/OVERVIEW.md` (current state snapshot)
- `docs/VISION.md`
- `docs/APP-DEVELOPER-GUIDE.md`
- `docs/TUTORIAL.md`
- `docs/APP-LAYOUT.md`
- `docs/DEPLOYMENT.md`
- `docs/ROADMAP.md` (high-level phases + decision log)

The public surface is deliberately kept free of "Session N" ticket language and internal scaffolding so a developer who just ran `cargo install pg-web` can orient in < 2 minutes.

## Cross-links

Public docs link only to other public docs or the root README. Internal files are referenced from `CONTRIBUTING.md` and `CLAUDE.md`.

## Do not delete

Historical detail is never removed — only relocated and labeled so the first impression for a GitHub visitor or crates.io browser is clean and professional.
