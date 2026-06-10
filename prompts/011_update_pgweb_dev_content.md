# 011 — Content updates and improvements to the live pg-web.dev docs site

**Status:** Handoff prompt — stub (owner will fill the specific content changes)  
**Priority:** Medium (ongoing polish now that the site is live and dogfooded)  
**Context:** Prompt 008 stood up the site as a real pg-web application at `site/`. It is deployed on Hetzner (see the bootstrap script in `site/scripts/` and the operational README). The initial content is a first-cut port of the most important public docs. The owner has a list of content, structure, and polish changes they want to make.

This prompt is intentionally a **lightweight stub**. The mechanical work of editing, testing, and deploying is well understood. The heavy lifting (what exact prose, new pages, design tweaks, etc.) will be filled in by the owner.

## Read These First

- `CLAUDE.md` (invariants, especially companion-app flow and "no premature abstraction")
- `docs/APP-LAYOUT.md` (the law)
- `site/README.md` (the operational guide for this specific app)
- `site/scripts/bootstrap-hetzner.sh` (the reusable stand-up script)
- The current live site at https://pg-web.dev (and its source in `site/pages/`, `site/public/`)
- `examples/todo/` (still the richer dynamic reference)

## Goals (High Level)

Improve the quality, usefulness, and personality of the public documentation that lives on the dogfooded pg-web site.

- Refresh / expand / rewrite sections of the live content.
- Possibly add new pages or reorganize navigation.
- Improve the "this is a real pg-web app" messaging and dogfooding proof points.
- Make the site more welcoming / scannable for someone who just discovered the project.
- Keep the site itself a clean, working pg-web application (no framework changes required unless a real bug is found).

Deep visual redesign, new interactive demos, search, versioning, etc. can be future prompts.

## Success Criteria (Owner to Customize)

The owner will replace this section with the concrete changes they care about. Example skeleton:

1. Home page pitch / quickstart / "you are looking at a real pg-web app" callout has been updated to ...
2. The `/overview` (or equivalent) page now says ...
3. New or improved page at `/whatever` that covers ...
4. Navigation / header / footer has been adjusted so that ...
5. `pg-web check` still passes cleanly on `site/`.
6. Changes have been tested locally with `pg-web dev` (or the manual flow).
7. Changes have been deployed to the live Hetzner instance and are visible at https://pg-web.dev.
8. The site's own `README.md` (if relevant) or any inline notes have been lightly updated.

## High-Level Workflow (Do Not Change Lightly)

1. Work inside `site/`.
2. Follow `docs/APP-LAYOUT.md` for any new routes/pages.
3. Most content should remain static-first (`.html` only) for simplicity and speed.
4. Test locally:
   - `cd site`
   - `../target/debug/pg-web dev` (or the full `up` + `migrate` + `push` if you don't have the binary in PATH)
   - Visit http://localhost:8080
5. Run `pg-web check` (from inside `site/`) before committing.
6. Commit + push the changes to the repo.
7. Deploy using the established Hetzner flow (see prompt 012 for the detailed runbook).
8. Verify on the live site (incognito + hard refresh is your friend; the 308 → HTTPS redirect + Caddy certs are working).

## Constraints

- Stay inside the existing `site/` app layout and deployment story.
- Do not introduce Phase 2+ features just for the docs site.
- Preserve the "this site is itself a pg-web app" dogfooding proof points.
- Keep the authoritative long-form material in the repo's `docs/` tree; the live site is the polished public surface.
- Use the same Docker image + Caddy pattern that normal users are expected to follow.

## What This Prompt Is Not

This is **not** the place to document every single sentence change. The owner will maintain their own list of desired content updates (in this file, in notes, in a separate doc, or directly as edits). The purpose of 011 is simply to have a numbered handoff stub that can be referenced in the future ("we did the big content pass in prompt 011").

## Owner Notes / Specific Changes (Fill This In)

**TODO (owner):** Replace everything below this line with the actual list of content, structure, and polish work you want done.

Examples of things you might list here:

- Rewrite the home page pitch to be shorter / more exciting / emphasize zero-proxy more.
- Add a "Why not just use X?" comparison section.
- Expand the "Getting Started" flow or link more prominently to the tutorial.
- Improve mobile styling / typography in `public/styles.css`.
- Add a small dynamic "last updated" or "current commit" badge using a trivial handler + `pgweb.setting`.
- Create a new `/examples` or `/showcase` page that highlights the todo app more.
- Reorganize the sidebar / top nav.
- Add better 404 content or a "still under construction" note on some pages.
- Update any version numbers, status badges, or "as of v0.2" language.
- ...

(Owner: delete this example list and put your real changes here. Then treat the rest of the prompt as the process guardrails.)

## References

- Live site: https://pg-web.dev
- Source of truth for the app: `site/`
- Deploy runbook: prompt 012 (the detailed sibling)
- How the site was originally stood up: prompt 008
- Reusable stand-up script: `site/scripts/bootstrap-hetzner.sh`
- Current reliable update command (as of the end of the initial deployment work):
  `ssh hetzner "cd /opt/pg-web/site && docker compose exec postgres sh -c \"cd /app && pg-web push --with-migrate\""`

## Tone

- Bias toward shipping visible improvements to visitors over perfection.
- The site is the public face now — small, frequent, testable updates are better than giant rewrites that sit in a branch.
- Every time you are tempted to change the framework to make a docs page nicer, ask whether it can be done with the current surface (and whether the todo example or a smoke test should also get the pattern).

When the specific changes listed in the "Owner Notes" section above are live and look good in both a normal browser and incognito, this prompt can be marked executed.

**End of stub prompt 011.** (Owner: edit the sections above with your actual desired changes, then execute or hand off.)