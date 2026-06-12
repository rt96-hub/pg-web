# 008 — Set up pg-web.dev as a real, dogfooded pg-web application

**Status:** ✅ Completed (moved to `prompts/completed/` 2026-06-12)  
**Priority:** High (foundational for open source launch + credibility)  
**Owner decision (2026-06):** License stays fully open (MIT OR Apache-2.0). The documentation website for the project **must itself be a pg-web application**. We will dogfood the framework for its own public docs site on the domain the owner already controls (`pg-web.dev`).

## Context & Goals

We are preparing pg-web for open source release. The single best way to prove the framework is real and usable is to have the project's own documentation site run on pg-web.

- Domain: `pg-web.dev` (owner has it).
- The entire site (or the primary content delivery) must be served by a pg-web stack: Postgres + `pg_web_ext` (via the `pgweb/postgres` image), Caddy for TLS, app code in the standard `pages/`, `migrations/`, `public/`, `pgweb.toml` layout.
- Content currently lives in `docs/*.md` (VISION, OVERVIEW, ROADMAP, APP-LAYOUT, APP-DEVELOPER-GUIDE, TUTORIAL, DEPLOYMENT, TESTING, ARCHITECTURE, etc.).
- We want a pleasant, navigable developer documentation experience (table of contents, version notes, search if practical, good mobile, fast loads).
- Dogfooding constraints: use real handlers (some pages may be `.sql` + `.html` for dynamic bits like "latest version", examples, or future interactive demos), static assets from `public/`, follow `docs/APP-LAYOUT.md` exactly.
- Production deployment must use the same pattern normal users will use: the published `pgweb/postgres` image + a small `docker-compose.yml` + Caddyfile.
- The site must be developable locally with `pg-web dev`.

This prompt is **only** for standing up the site as a working pg-web app and getting the core content served. Deep content rewriting / new design system / advanced features can come later.

Read these first (in order):
- `CLAUDE.md` (invariants — especially "every feature ships with a companion-app flow", no premature abstraction, directory-as-route rules).
- `docs/VISION.md`
- `docs/OVERVIEW.md` (current state snapshot)
- `docs/APP-LAYOUT.md` (the law for file → route mapping)
- `docs/APP-DEVELOPER-GUIDE.md`
- `docs/DEPLOYMENT.md`
- `examples/todo/` (the canonical reference implementation + its README and docker-compose + Caddyfile)
- `docs/TUTORIAL.md` (how a normal person is expected to build something)

Also skim the existing `docs/` for content that should be exposed.

## Non-Goals for this prompt

- Do not rewrite all the prose yet (that can be incremental).
- Do not build a fancy JS-heavy frontend or client-side router.
- Do not introduce Phase 2+ features (auth, realtime, jobs) unless they are already shipped.
- The site does **not** have to be "beautiful" on day one — it has to be functional, correct, and dogfooded.
- Do not change the CLI or extension for this work (unless a real bug is discovered while dogfooding).

## Success Criteria

1. There is a new directory (or the existing repo content is used directly) that is a valid pg-web app: `pgweb.toml`, `pages/`, optional `migrations/`, `public/`, `docker-compose.yml`, `Caddyfile`.
2. Running `pg-web dev` (or the manual `up` + `push` flow) locally serves a usable version of the documentation at `http://localhost:8080`.
3. Core public docs are reachable via clean URLs that match the spirit of the existing doc structure (e.g. `/`, `/overview`, `/roadmap`, `/app-layout`, `/tutorial`, `/deployment`, etc.).
4. The same app, when deployed via its `docker-compose.yml` + the `pgweb/postgres:latest` image + Caddy, serves the site over HTTPS on `pg-web.dev`.
5. Static assets (CSS, images, favicons, etc.) are served from `public/` with appropriate caching (content-hash in prod is a bonus but not required for first cut).
6. The site itself exercises real handler patterns (at minimum some pages use the `.sql` + `.html` contract; pure static `.html` pages are allowed and encouraged for most documentation).
7. `pg-web check` passes cleanly on the docs app.
8. A simple README or `docs-site/README.md` inside the app explains how to develop the site and how to deploy it (this becomes the reference for "how we host pg-web.dev").
9. No violation of architectural invariants (one request = one SPI tx, extension/CLI decoupling, etc.).

## High-Level Approach (Recommended)

1. **Create a dedicated app for the site** (recommended over forcing everything into the repo root):
   - `docs-site/` or `website/` at the repo root (or a separate repo that submodules / references the docs — but keeping it in this monorepo is simpler for dogfooding).
   - Inside it: follow the exact layout from `pg-web init --template todo` + `examples/todo/`.
   - Copy/adapt the current `examples/todo/docker-compose.yml` and `Caddyfile` (update names, remove the todo-specific bits).

2. **Content strategy for first cut** (keep it pragmatic):
   - Most pages can be static-first: `pages/<section>/index.html` (pure template, no SQL handler) for the bulk of the markdown content.
   - Convert the best existing `.md` files into clean Tera templates (or even just HTML with minimal Tera includes for nav/header/footer).
   - Use a simple shared layout (`_base.html` or similar via Tera `{% include %}` or by having a common shell).
   - For dynamic / high-value parts: at least one or two real handlers, e.g.:
     - A version banner or "current release" pulled from a small table or `pgweb.settings`.
     - A "try the todo demo" live fragment (if feasible).
     - Future: search or "copy command" buttons that are enhanced with a tiny SQL-backed endpoint.
   - Preserve the authoritative docs in `docs/` in the main repo. The site can either:
     - Copy the rendered content at build/push time, or
     - Treat `docs/` as source and have a simple conversion step (or just hand-maintain the HTML/MD-in-HTML versions for now).
   - The long-term ideal is that the site content is the source of truth for users, while `docs/` in the repo remains the detailed spec for maintainers.

3. **Navigation & structure**:
   - Follow `docs/APP-LAYOUT.md` strictly (directories = routes, `index.*` for the GET of that route).
   - Top-level sections that map cleanly: Home, Overview / Getting Started, App Developer Guide (with sub-pages for layout, handlers, tutorial), Reference (ROADMAP, ARCHITECTURE for the curious), Deployment, etc.
   - `_404.html` (static is fine).

4. **Styling & assets**:
   - Start minimal (copy/adapt the CSS approach from `examples/todo/public/styles.css` or make a clean new one).
   - Put all CSS/JS/images under `public/`.
   - Use the existing `pgweb.html_escape` helper where user content is interpolated.
   - Consider content-hash assets later (the framework already supports it when `[server].env = "production"`).

5. **Deployment**:
   - The `docker-compose.yml` for the site should look almost identical to the one in `examples/todo/`.
   - Point DNS for `pg-web.dev` (and `www.` if desired) at the VPS.
   - Caddy handles Let's Encrypt + reverse proxy to the internal `:8080`.
   - Use `pg-web push --with-migrate` (or the in-image CLI) for updates.
   - Document the exact deploy commands in the site's own README.

6. **Local development**:
   - `cd docs-site && pg-web dev` should "just work" once the stack is up.
   - The site app should be able to `pg-web check`.

7. **Versioning / "this docs are for vX"**:
   - Simple static note or a small table for now is fine. We can make it dynamic later.

## Constraints & Invariants (non-negotiable)

- Directory-as-route + filename-as-method (see `docs/APP-LAYOUT.md` and `crates/pg_web_cli/src/paths.rs`).
- Handler contract `(req json) RETURNS json|text`.
- Use the `pgweb/postgres:latest` image for the runtime (do not invent a new base).
- The CLI (`pg-web`) is the only way developers interact with the stack for day-to-day work.
- Every page that returns HTML must go through the normal route + template (or static) path.
- Follow the testing spirit: if you add a new pattern on the docs site, consider whether `examples/todo/` or the smoke tests should also exercise something similar (but don't block the site on this).
- No raw C, no direct filesystem from the extension, one SPI tx per request, etc. (full list in `CLAUDE.md`).

## Concrete Tasks (in rough order)

1. Decide on location: `docs-site/` at repo root (preferred for this phase) or keep everything at root. Document the choice.
2. Scaffold the app using `pg-web init` (or by copying the todo example structure) inside the chosen directory.
3. Port the most important public-facing content:
   - Home / landing (short pitch + quickstart + links to GitHub).
   - Overview / "What is pg-web".
   - Getting started (init + dev + the todo example).
   - Key guides: App Layout, Handler contract, Tutorial highlights.
   - Reference pages (at minimum ROADMAP + DEPLOYMENT summary).
4. Implement navigation (header, sidebar or top nav, footer). Use Tera includes or a layout template.
5. Add at least one real dynamic handler (example ideas: "Current version" component, a small "recent changes" list fed from a tiny table you control, or a "copy-to-clipboard" endpoint that is just a no-op handler returning text).
6. Wire up `public/` for CSS + any images/logos.
7. Make `docker-compose.yml` + `Caddyfile` work for the site (update service names, volume mounts for the site's own code, environment).
8. Add a `README.md` inside the docs-site directory explaining:
   - How to develop it locally (`pg-web dev`).
   - How to deploy updates (the exact `push` / `migrate` commands + Docker context).
   - How the content is synchronized from the main `docs/` (manual copy for v1, or a script — keep it simple).
9. Run the full local flow: `pg-web up`, `migrate apply`, `push`, visit the site, verify routes.
10. Run `pg-web check` and fix any findings.
11. (Stretch) Add a production-like env flag and verify asset caching behavior.
12. Document the live URL and any special notes back in the main repo (e.g. in a new top-level `WEBSITE.md` or in `docs/DEPLOYMENT.md`).

## Out of Scope for First Pass (but note them)

- Full search (can be added later with a simple SQL-backed handler + trigram or tsvector).
- Interactive live demos beyond the existing todo example.
- Multi-version docs (we can add a `v0.2/` subtree or similar later).
- Blog / news section.
- Dark mode, fancy design system, etc.

## Deliverables at End of Work

- A working, committed `docs-site/` (or equivalent) that is a first-class pg-web application.
- It deploys to `https://pg-web.dev` and serves the core documentation.
- Clear instructions so the owner (or future maintainer) can update the site with a `pg-web push`.
- The site must survive `pg-web check` and the spirit of the 5-tier test strategy (even if the site itself is not yet part of the automated test matrix).
- A short note in the main repo root or `docs/` explaining that the public docs are now dogfooded on pg-web itself.

## References & Files You Will Touch

- Main repo `CLAUDE.md`, `docs/APP-LAYOUT.md`, `examples/todo/`
- New files under the docs-site directory
- Possibly updates to `docs/DEPLOYMENT.md` or a new `WEBSITE.md`
- `docker/init-pgweb.sh` and `Dockerfile` only if you discover a real gap while dogfooding (coordinate with owner)

## Tone & Process Notes

- Bias toward shipping a useful site quickly over perfection.
- Every time you are tempted to add framework features to make the site nicer, stop and ask: "Can I do this with the current v0.2 surface?" If not, note it for later phases instead of scope-creeping.
- Use the companion app discipline: if the docs site exercises something the todo app doesn't, consider whether the todo app (or a smoke test) should be updated too — but don't let this block the site launch.
- Commit early and often with clear messages. The site will be public quickly.

When the site is live and `pg-web dev` works against it, the handoff is complete. Update this prompt's status and add a pointer to the live URL and the site's own README.

**End of prompt 008.**
