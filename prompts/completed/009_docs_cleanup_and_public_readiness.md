# 009 — Docs cleanup and public/open-source readiness

**Status:** ✅ Completed — executed Session ~6 / post-v0.2.0 (moved to `prompts/completed/` 2026-06-12)
**Priority:** High (required before credible open source launch)  
**Context:** We have decided to stay fully open source under MIT OR Apache-2.0. The project will be published to crates.io (CLI), GitHub will be the primary home, and `pg-web.dev` will be dogfooded as a real pg-web app (see sibling prompt 008). Before any announcement or external contributors, the documentation tree must be welcoming, scannable, and clearly separate "for users/app developers" from "for framework maintainers / project history."

## Read These First (Mandatory)

- `CLAUDE.md` — the north-star invariants and coding practices. Many of the internal notes live here; they should not be the first thing a new visitor sees.
- `docs/OVERVIEW.md` — the current-state snapshot (regenerate or update at the end of this work if the picture changes).
- `docs/VISION.md`
- `docs/ROADMAP.md`
- `docs/APP-LAYOUT.md`
- `docs/APP-DEVELOPER-GUIDE.md`
- `docs/TUTORIAL.md`
- `HANDOFF.md` (understand the current internal onboarding story)
- Existing `prompts/` directory (style reference)
- `docs/sessions/` (these are gold for history but will be hidden from the public surface)
- Root of the repo + `examples/todo/README.md`

## Current Problems (as of late Session 5 / v0.2.0)

- No root `README.md` at all (critical missing piece for GitHub, crates.io, and first-time visitors).
- `docs/` mixes excellent public material with heavy internal artifacts:
  - `sessions/` (detailed working notes from every past session — extremely valuable for maintainers, noisy and confusing for outsiders).
  - `CLAUDE.md` at repo root (agent instructions; great for us, weird for humans).
  - `HANDOFF.md` (cold-start for the original author moving machines).
  - `prompts/` (specific technical debt / improvement prompts).
  - `DEVELOPER-GUIDE.md` (mostly WSL + pgrx dev environment + packaging gotchas).
- Some docs still contain "Session X" language, internal ticket-style references, or assume the reader is the original developer.
- Content is high quality but not organized for a casual visitor who just did `cargo install pg-web` or clicked the GitHub link.
- No clear "start here for app developers" vs "I want to hack on the framework" split.
- License files exist but there is no top-level `LICENSE` (or dual `LICENSE-MIT` / `LICENSE-APACHE` pointers) that GitHub renders nicely.
- The authoritative docs live in `docs/`, but there is no single friendly index or "table of contents for humans."

## Goals

Create a clean, professional, scannable public documentation surface while preserving all the detailed internal history and agent instructions for future maintainers.

- A new or completely rewritten root `README.md` that serves as the primary front door (pitch + 60-second quickstart + links + "for app developers" vs "contributing").
- Logical split in `docs/`:
  - Public / user-facing docs stay prominent and clean.
  - Internal / historical material is moved or clearly marked so it does not pollute the first impression.
- Every public doc should feel written for someone who is **not** the original author.
- The docs that the new `pg-web.dev` site (prompt 008) will surface should be the best, most polished versions.
- `pg-web check` (when run against any example or the docs-site app) should stay happy.
- Preserve the decision log and deep rationale that lives in ROADMAP + sessions (just don't make visitors read the sessions first).

## Success Criteria

1. A high-quality root `README.md` exists. It is the file GitHub and crates.io will show. It contains:
   - Short compelling pitch (drawn from VISION + OVERVIEW).
   - "Get started in < 5 minutes" flow using the published CLI + the Docker image.
   - Clear distinction: "I want to build an app with pg-web" vs "I want to contribute to pg-web or understand the internals."
   - Links to the best public docs, the GitHub repo, the live `pg-web.dev` site, the todo example, and the tutorial.
   - License summary + "fully open source (MIT/Apache-2.0)".
   - Current status ("v0.2.0 — Phase 1 complete, Phase 2 in planning").
   - Badges if we have them (CI, crates.io once published, Docker pulls, etc.).

2. `docs/` is reorganized so the first things a visitor sees when they open the folder (or the rendered GitHub view) are the user guides:
   - Recommended top-level public docs: OVERVIEW (or a new INDEX.md), VISION (short), APP-DEVELOPER-GUIDE, TUTORIAL, APP-LAYOUT, DEPLOYMENT, ROADMAP (high-level view), TESTING (for the curious).
   - Internal material is moved into `docs/internal/` (or `docs/maintainer/`) or left at root with clear "Internal / Maintainer only" front-matter:
     - `sessions/` → `docs/internal/sessions/`
     - `CLAUDE.md` → keep at root or move to `docs/internal/CLAUDE.md` (and reference it from CONTRIBUTING).
     - `HANDOFF.md` → `docs/internal/HANDOFF.md`
     - `prompts/` → `docs/internal/prompts/` (or keep as-is; they are already numbered technical notes)
     - `DEVELOPER-GUIDE.md` → `docs/internal/DEVELOPER-GUIDE.md` (rename or add a big "Maintainer Environment" header if kept prominent).

3. A new or updated `CONTRIBUTING.md` at the repo root (or in `.github/`) that points new contributors at the right docs (CLAUDE.md + internal developer guide + testing story) without forcing casual users to read them.

4. All public-facing markdown has been lightly edited for:
   - Audience (assume a developer who just discovered the project).
   - Removal of heavy "Session 3 / Component G" style references unless they add value (move the detailed history to the internal folder).
   - Clear "last updated" or version notes where relevant.
   - Consistent headings, code blocks, and links.

5. A `LICENSE` file (or clear dual-license pointers) at the root so GitHub shows the license badge nicely. (Current `LICENSE-MIT` and `LICENSE-APACHE` can stay; add a top-level `LICENSE` that says "Dual-licensed under MIT OR Apache-2.0 — see the two files" or use the common combined header.)

6. The public docs are in a state where they can be consumed by the docs-site app (prompt 008) with minimal additional transformation.

7. `README.md` + top public docs pass a quick "newcomer test": someone unfamiliar with the project can answer "what is this?", "how do I try it?", "where is the tutorial?", and "how do I deploy something real?" in under two minutes of reading.

## Recommended Folder Shape After Cleanup (example — adapt as needed)

```
pg-web/
├── README.md                  # NEW — the public front door
├── CONTRIBUTING.md            # NEW or moved
├── LICENSE                    # NEW or symlink-style
├── LICENSE-MIT
├── LICENSE-APACHE
├── CLAUDE.md                  # Keep (internal)
├── HANDOFF.md                 # Move or mark internal
├── Cargo.toml
├── docs/
│   ├── INDEX.md or OVERVIEW.md (public entry)
│   ├── VISION.md
│   ├── APP-DEVELOPER-GUIDE.md
│   ├── TUTORIAL.md
│   ├── APP-LAYOUT.md
│   ├── DEPLOYMENT.md
│   ├── ROADMAP.md
│   ├── ARCHITECTURE.md        # still useful publicly for the curious
│   ├── TESTING.md
│   └── internal/              # NEW
│       ├── CLAUDE.md (copy or move)
│       ├── DEVELOPER-GUIDE.md
│       ├── HANDOFF.md
│       ├── sessions/
│       └── prompts/
├── examples/todo/
├── prompts/                   # Can stay at root or move under internal later
└── ...
```

## Concrete Work Items

1. **Create the root `README.md`** (biggest single deliverable).
   - Draw the pitch heavily from `docs/VISION.md` + the 30-second picture in `OVERVIEW.md`.
   - Include the exact "try it" commands that will work after the Cargo work (prompt 010): `cargo install pg-web`, `pg-web init ...`, `pg-web dev`.
   - Mention the Docker image path for production.
   - Link to `pg-web.dev` (once the site from prompt 008 is live) as the friendly human docs.
   - Link to `docs/TUTORIAL.md` and `examples/todo/`.
   - Mention the companion app discipline and the five-tier test story at a high level.
   - End with "Status", "License", "Contributing", and "Links".

2. **Reorganize `docs/`**:
   - Create `docs/internal/`.
   - Move or copy the internal-heavy files.
   - Update any cross-links that break.
   - Add a short `docs/internal/README.md` that explains what lives here and who should read it.

3. **Add root `CONTRIBUTING.md`**:
   - Short "thank you" + "read CLAUDE.md first if you want to change the framework".
   - Point to the testing strategy.
   - Link to the internal developer guide.
   - Note the "every feature must be exercised in the companion app or the docs site" expectation.
   - Conventional commits, no Co-Authored-By trailers (per existing convention).

4. **License presentation**:
   - Add a top-level `LICENSE` file with the standard dual-license boilerplate text pointing at the two existing files.
   - Or keep the two files and add a clear note in README + a root `LICENSE` that GitHub recognizes.

5. **Polish pass on public docs**:
   - Go through the main user-facing files and remove or footnote heavy internal jargon.
   - Ensure every doc has a clear "who this is for" sentence near the top where helpful.
   - Update any "as of Session X" dates to calendar dates or "v0.2.0" references.
   - Make sure ROADMAP still accurately reflects the current phase split (Phase 1 complete + polish, Phase 2 auth/realtime next, etc.).

6. **Root-level hygiene**:
   - Decide what stays at the absolute root (README, CONTRIBUTING, LICENSE*, CLAUDE.md, Cargo files, Dockerfile, scripts/, docker/, examples/, docs/, prompts/?).
   - Add a `.github/` entry or note if issue/PR templates would help (can be minimal for first release).

7. **Validation**:
   - After changes, a fresh clone + `cargo install` of the (future) CLI should let someone follow the README and get to a running app.
   - Run `pg-web check` against the todo example (it must still pass).
   - Spot-check that internal history is still findable for maintainers (e.g. the full decision log in ROADMAP + sessions/ under internal/).

8. **Tie-in to other work**:
   - Coordinate with prompt 008 (the docs site) — the cleaned public docs become the source material for what is served on `pg-web.dev`.
   - Coordinate with prompt 010 (Cargo) — the README must describe the `cargo install pg-web` experience accurately, including the fact that the runtime comes from the Docker image, not from the crate itself.

## Constraints

- Do **not** delete historical information — only relocate or clearly label it.
- Keep the spirit and detail of the existing high-quality docs (VISION, ROADMAP decision log, APP-LAYOUT spec, etc.).
- Follow the same "no premature abstraction" and "tests / companion app next to changes" mindset from CLAUDE.md even when editing docs.
- The cleanup must make the project look professional and approachable to a first-time Rust + Postgres developer.
- Do not invent new top-level documentation formats unless they are trivial (Markdown + the existing structure is fine).

## Deliverables

- New `README.md` at repo root.
- New or updated `CONTRIBUTING.md`.
- `LICENSE` (or equivalent clear dual-license presentation at root).
- Reorganized `docs/` with `internal/` (or equivalent) containing the noisy history and agent-specific files.
- Light editorial pass on the main public docs so they read as written for outsiders.
- Any necessary link fixes and a short note (in the new README or a `CHANGES.md` entry) describing the docs reorganization.
- The public docs are now in a state that can be directly used (or lightly transformed) by the dogfooded `pg-web.dev` site.

## Tone

Be ruthless about first-impression cleanliness while being respectful of the excellent detailed work that already exists. The goal is "someone lands on the GitHub repo and immediately feels 'this is a real, well-documented project I can trust'."

When done, update the status of this prompt and leave a short recap of what moved where.

## Execution recap (docs cleanup)

**Root front door (new):**
- `README.md` — pitch (VISION + 30s picture), 5-min `cargo install pg-web` + `pg-web init --template todo` + `pg-web up` flow (notes Docker image supplies runtime), clear "build an app" vs "contribute/internals" split, links to TUTORIAL / examples/todo / pg-web.dev / GitHub, v0.2.0 status, dual-license note, badges placeholders.
- `CONTRIBUTING.md` — points new contributors at CLAUDE.md first, internal/DEVELOPER-GUIDE + TESTING, companion-app rule, conventional commits, no Co-Authored trailers.
- `LICENSE` — dual-license pointer to the existing LICENSE-MIT / LICENSE-APACHE (GitHub badge friendly).

**docs/ reorganization:**
- Created `docs/internal/`.
- Moved: `sessions/` → `docs/internal/sessions/` (7 files preserved), `DEVELOPER-GUIDE.md` → `docs/internal/DEVELOPER-GUIDE.md`, `HANDOFF.md` → `docs/internal/HANDOFF.md`.
- Added `docs/internal/README.md` (explains who should read here vs public docs).
- `CLAUDE.md` kept at repo root (agent discovery + referenced from CONTRIBUTING) with its internal layout example lightly refreshed.
- `prompts/` left at root (as allowed; technical notes, not first-impression noise).
- Public top level in `docs/`: OVERVIEW.md (entry), VISION.md, APP-DEVELOPER-GUIDE.md, TUTORIAL.md, APP-LAYOUT.md, DEPLOYMENT.md, ROADMAP.md, ARCHITECTURE.md, TESTING.md (all feel written for a newcomer who just discovered the crate).

**Polish performed on public docs:**
- Audience / "who this is for" notes added or strengthened near tops.
- Heavy "Session X / M1.x Component Y / in-flight" ticket language removed or footnoted; replaced with calendar/v0.2.0/Phase 1 complete language where it helped scannability.
- Install paths in TUTORIAL + APP-DEVELOPER-GUIDE updated to `cargo install pg-web` + image reality (aspirational until prompt 010 lands, but accurate per the sibling prompt).
- Outdated "coming in M1.4", "Session 4 Component F.2", old test counts, and "DEVELOPER-GUIDE at docs/" links fixed.
- DEPLOYMENT remote-push section refreshed (F.2 still deferred).
- TESTING.md numbers + matrix intro synced to current OVERVIEW v0.2 reality (230 tests, 5 tiers, companion-app rule).
- Cross-links after the move were repaired in the public surface (internal sessions/ references only appear in ROADMAP history / internal files, which is correct).
- OVERVIEW "Try it", "Dev loop", "Not in v0.2", code map, and gotchas link updated.
- Last-updated / status banners point at v0.2.0 + `docs/internal/sessions/`.

**Validation:**
- `target/debug/pg-web check` against `examples/todo/` → "✓ check passed — no findings".
- `cargo check --workspace` clean.
- `cargo clippy --workspace -- -D warnings` clean (exit 0).
- Internal material still findable (e.g. full decision log lives in ROADMAP + moved sessions/).
- New public surface is scannable: a fresh reader can answer "what is this?", "how do I try it?", "where is the tutorial?", "how do I deploy?" in well under 2 minutes from README.
- Docs now in good shape for direct (or light-transform) consumption by the `pg-web.dev` dogfood site (prompt 008).

**Root hygiene:** README/CONTRIBUTING/LICENSE* at top; CLAUDE.md at root (intentional); prompts/ at root; .github/ (ci.yml + release.yml) already present; no new top-level formats invented. Everything else (Cargo, Dockerfile, scripts/, docker/, examples/, docs/) stayed.

**Deliverables complete per the prompt.** The project now presents a professional, welcoming first impression on GitHub / crates.io while preserving every bit of detailed internal history for maintainers.

**End of prompt 009.**
