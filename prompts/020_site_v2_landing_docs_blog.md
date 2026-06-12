# 020 — Site v2: marketing landing + docs section + blog (the flagship Phase-2 dogfood)

**Status:** Open work order — large; becomes the project's third companion app and Phase 2 proving ground
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Prerequisites:** 013 (response contract — cookies/redirects/content-type), 014 (auth role — RLS actually enforcing), 017 (multipart uploads — for images); Phase 2 auth (session_6) for the blog's authoring side
**Context:** `site/` is already a real, deployed pg-web app (pg-web.dev) but it dogfoods almost nothing dynamic — 7 static `.html` pages plus one trivial JSON→Tera home handler, zero migrations, no auth, no uploads, no dynamic routes. This work order turns it into a three-part flagship — a marketing landing page, a `/docs` section, and an authenticated blog — chosen precisely because the blog forces the site to exercise every hard Phase-2 feature (sessions, RLS, uploads, realtime). The thesis: **the most credible proof a framework is real is that its own public site uses every feature it sells.**

---

## Summary

pg-web.dev is currently a near-static brochure: `site/pages/{overview,layout,tutorial,deployment,roadmap}/index.html` are pure templates, `site/pages/_404.html` is static, and only `site/pages/index.sql` + `site/pages/index.html` run a handler — and that handler (`pgweb.pages__index`, `site/pages/index.sql:7-18`) just returns a hardcoded list of nav links and a note, touching no tables (`STABLE`, no app schema). `site/migrations/` holds only `.gitkeep`. The site proves "pg-web can serve documentation" and nothing more. Phase 1 is closed; v0.2.0 shipped; Phase 2 (auth/RLS/realtime) is specced in `docs/internal/sessions/session_6.md` but not yet built.

This prompt specs **Site v2**: three parts, built in dependency order.

- **Part A — Marketing landing page.** A real `/` landing: hero, zero-proxy pitch, a live `pages/` code sample, the "app you can `pg_dump`" story (cross-ref 019), social proof, quickstart, links into docs + blog. Mostly static/dynamic Tera. Needs nothing new — **build it first.**
- **Part B — Docs section.** A `/docs` tree rendering the project's own documentation with nav/sidebar/search. **Lean:** render Markdown → HTML at push time so authoring stays Markdown but serving stays pure pg-web (flag the small CLI/build-step feature this motivates). Needs nothing new beyond that tooling decision.
- **Part C — Blog with auth + image uploads + (optional) realtime.** THE Phase-2 dogfood. Public read (`/blog`, `/blog/[slug]`), login-gated authoring (`/blog/admin`), image uploads, optional realtime "new post" live-update. Gated on 013, 014, 017, and Phase 2.
- **Part D — The "text editor test."** A browser editor for composing posts, framed explicitly as a *find-the-sharp-edges* stress test of body-size limits, sanitization, autosave, and the single-threaded runtime. **Lean:** Markdown textarea + server-side render+sanitize first; WYSIWYG is a later stress test.

The whole thing keeps the existing deploy story intact (Caddy + `bootstrap-hetzner.sh` + `pg-web push --with-migrate`; see prompts 008/011/012). Each milestone below names exactly which framework feature it proves.

---

## Why this matters now (dogfooding is the credibility engine)

The root `CLAUDE.md` makes this a hard rule, not a nicety: *"Every feature ships with a companion-app flow. If a feature isn't exercised in `examples/todo/`, it isn't done."* Today there are two companion vehicles: `examples/todo/` (CRUD/HTMX reference + tier-3 E2E target) and `site/` (the docs-site dogfood, charter in prompt 008, deploy runbook in 012). Phase 2 introduces sessions, an RLS bridge, CSRF, and realtime SSE — and **none of those have a real public app that uses them.** `examples/todo/` is anonymous and single-user. A scaffold (`pg-web init --template auth`, session_6 Track A) is a fixture, not a proof.

A blog is the smallest honest app that needs the *entire* Phase-2 surface at once:

- It has public readers and authenticated authors → **cookie sessions** (session_6 Track A).
- Authors edit only their own posts; drafts are private → **RLS bridge** (session_6 Track B, `pgweb.user_id` GUC).
- It has non-GET authenticated writes → **CSRF** (session_6 cross-cutting).
- Posts have hero/inline images → **multipart uploads + asset storage** (017).
- "New post published" is a natural live event → **realtime SSE** (session_6 Track C).

If pg-web.dev's own blog runs all of that in production, Phase 2 is *proven*, not merely shipped. That is the entire point of this work order. It is also a deliberate sharp-edge-finder: Part D exists to break things on purpose (large bodies, untrusted HTML, autosave write-amplification) while the maintainer is watching.

**Lean:** treat Site v2 as the canonical acceptance gate for Phase 2 — a feature isn't "done" until pg-web.dev uses it in anger.

---

## Current site (evidence)

Read the live app before writing anything. Cited paths are all under `/tmp/pg-web` at git `918f40b`.

**Routes today** (`site/pages/`):

| Route | Files | Mode | Notes |
|---|---|---|---|
| `GET /` | `index.html` + `index.sql` | dynamic (JSON→Tera) | `pgweb.pages__index(req json) RETURNS json` — returns a hardcoded `sections` array + `note`, no tables, `STABLE` (`site/pages/index.sql:7-18`) |
| `GET /overview` | `overview/index.html` | static | pure template |
| `GET /layout` | `layout/index.html` | static | the APP-LAYOUT rules, hand-ported |
| `GET /tutorial` | `tutorial/index.html` | static | |
| `GET /deployment` | `deployment/index.html` | static | |
| `GET /roadmap` | `roadmap/index.html` | static | |
| `_404` | `_404.html` | static | reserved fallback stem |

**What it dogfoods:** static template serving, one trivial JSON→Tera handler, static-asset serving (`site/public/styles.css`), the `_404` fallback. That's it. No migrations (`site/migrations/.gitkeep` is the only file), no app tables, no dynamic-segment routes, no POST handlers, no auth, no uploads, no realtime.

**Infra is already solid and must be preserved:**

- `site/pgweb.toml` — `[server] port=8080`, `env="development"` (flipped to `production` on the box by `bootstrap-hetzner.sh:68`); `[dev] watch_paths=["pages","public"]`.
- `site/docker-compose.yml` — single `pgweb/postgres:latest` container; commented-out `caddy` service for prod.
- `site/Caddyfile` — `pg-web.dev { reverse_proxy postgres:8080 }` + `www.` redirect. TLS is out-of-process (architectural invariant #2).
- `site/scripts/bootstrap-hetzner.sh` — the reusable VPS stand-up (installs Docker, clones repo to `/opt/pg-web`, writes prod compose with Caddy on + DB ports off, runs first `pg-web push --with-migrate`).
- Deploy runbook: prompt 012; content-change stub: prompt 011; original charter: prompt 008 (also committed verbatim at `site/008_docs_site_pgweb_dev_dogfooding.md`).

**Real defects/inconsistencies found while reading — fold fixes into Part A/B so v2 starts clean:**

1. **Dead nav link.** `site/pages/_404.html:30` links to `/app-layout`, but the route is `/layout` (the directory is `site/pages/layout/`). The 404 page's own suggestion 404s. Fix when rewriting.
2. **Doc/route drift.** `site/README.md` documents the tree as `app-layout/index.html`, but the actual directory is `layout/`. Reconcile.
3. **Asset reference is unfingerprinted in source.** Every page links `/styles.css` (e.g. `site/pages/index.html:7`), not `/styles.<hash>.css`. Content-hashing happens at *push time* in production (CLAUDE.md § Session 5 "H content-hash assets + immutable cache"), so the source-level `/styles.css` is expected — but verify the rewrite actually fires in prod and that v2's new pages/assets participate. The brief's "already live: pg-web.dev serves `/styles.<hash>.css`" refers to the deployed, rewritten output, not the repo source.
4. **GitHub org.** Source links point at `github.com/rt96-hub/pg-web` (e.g. `site/pages/index.html:18`). Keep consistent across all new pages.

---

## Target information architecture

```
pg-web.dev/
├── /                         Part A — marketing landing (dynamic Tera)
├── /docs                     Part B — docs index / overview
│   ├── /docs/overview        (migrated from /overview)
│   ├── /docs/app-layout      (migrated from /layout; fixes the slug)
│   ├── /docs/tutorial
│   ├── /docs/deployment
│   ├── /docs/roadmap
│   └── /docs/<more>          rendered from docs/*.md (see Part B)
├── /blog                     Part C — public post index (dynamic)
│   └── /blog/[slug]          public post page (dynamic-segment route)
├── /blog/admin               Part C — auth-gated dashboard (list own posts)
│   ├── /blog/admin/new       compose (Part D editor)
│   └── /blog/admin/[id]/edit edit own post (RLS-enforced)
├── /login  /logout           Part C — session auth (session_6 Track A)
└── _404                      keep; fix the dead link
```

**Redirect note (needs 013):** moving `/overview` → `/docs/overview` etc. wants 301s so existing links/SEO don't break. 301s are a response-contract concern (`Location` header + 3xx status) — that's prompt 013. **Lean:** until 013 lands, either keep the old top-level routes as thin static pages that link to the new `/docs/*` home, or hold the docs-tree move until 013 (Part B can ship under `/docs/*` with the old routes still present and cross-linked). Don't hand-roll redirects in handler text before the contract exists.

---

## Part A — marketing landing page

**Goal:** replace the current functional-but-plain `/` with a real landing page that sells the framework and *visibly* dogfoods it.

**Mode:** dynamic Tera (`index.html` + `index.sql`), building directly on the existing `pgweb.pages__index` handler (`site/pages/index.sql`). Low risk, high polish, needs nothing new.

**Sections (develop the copy, keep it hypermedia-first):**

1. **Hero** — "PostgreSQL is your web server." Subhead: the zero-proxy one-liner. The existing `<h1>` (`site/pages/index.html:23`) and callout ("You are viewing a real pg-web application", `:25-28`) are the seed — keep that self-referential proof and make it the centerpiece.
2. **The zero-proxy pitch** — no Node/Python/Go tier; one Docker image; one SPI transaction per request; the DB *is* the app. Pull language from `docs/VISION.md` and CLAUDE.md § Mission.
3. **Live code sample** — show the `pages/` layout (directory-as-route, `index.sql` + `index.html`). **Lean:** render it from the handler's JSON so the page literally demonstrates the JSON→Tera pipeline it's describing (the current home already does this for the nav grid — extend the pattern).
4. **The "app you can `pg_dump`" artifact story** — the whole app (routes, templates, handlers, data) lives in Postgres tables (`pgweb.routes`, `pgweb.templates`, `pgweb.assets`), so `pg_dump` captures the entire running site. **Cross-ref prompt 019** for the artifact framing; don't re-derive it here.
5. **Social proof / dogfooding proof points** — "this page, these docs, and the blog you're reading all run on pg-web." Link the companion apps: `examples/todo/` and the blog itself.
6. **Install / quickstart** — keep the existing 5-step block (`site/pages/index.html:34-51`): `cargo install pg-web` → image → `pg-web init --template todo` → `up`/`migrate`/`push` → open. Verify against the current CLI verbs before shipping.
7. **Footer nav into `/docs` and `/blog`.**

**Dogfoods:** dynamic JSON→Tera on the highest-traffic route; static-asset serving + (prod) fingerprinted CSS; the existing `(req json) RETURNS json` contract. **Proves nothing Phase-2** — that's deliberate; it's the zero-dependency warm-up.

---

## Part B — docs section

**Goal:** a `/docs` tree with real navigation (sidebar + top nav), good mobile, and ideally search, rendering the project's documentation.

**The core design question — where does Markdown→HTML happen?** Three options:

- **(1) Push-time render (Markdown → HTML template).** A build step converts `docs/*.md` (or `site/content/*.md`) into `pages/docs/<slug>/index.html` Tera templates before `pg-web push`. Authoring stays Markdown; serving is pure static pg-web (zero SQL per page). **Lean: this one.** It keeps the runtime pure and the content authorable. It does, however, motivate a small new piece of tooling — either a `pg-web` CLI feature (e.g. a `docs`/`render` pre-push hook, or a generalized "render `content/**/*.md` into `pages/**`" step) or a site-local build script committed under `site/scripts/`. **Flag this explicitly to the maintainer**: it's the one place Part B might touch the CLI, and per CLAUDE.md (extension/CLI decoupling, no premature abstraction) it should be a CLI-only convenience that emits ordinary `pages/` files — the extension and the route/template contract stay untouched. A committed `site/scripts/render-docs.sh` (pandoc/comrak/`markdown` → HTML fragment, wrapped in the shared layout) is the smallest version and needs *no* framework change at all — prefer that for v1.
- **(2) Store raw Markdown, render dynamically.** A `docs` table holds Markdown; a handler renders to HTML per request. Costs a Markdown renderer reachable from SQL (PL/pgSQL or a `pgweb`-side helper that doesn't exist) and an SPI call per page view. Heavier, and there's no Markdown-render primitive in the framework today. Reject unless a "render Markdown" need appears elsewhere.
- **(3) Keep hand-written `.html` like today.** Zero tooling, but authoring is painful and the content drifts from `docs/*.md` (already happening — see `site/README.md` § Content synchronization, which admits hand-porting with no sync step). Acceptable only as the migration baseline.

**Lean:** option (1) via a committed site-local render script (no CLI change for v1); revisit a first-class CLI feature only if a second app wants the same thing (companion-app discipline cuts both ways).

**Navigation & layout:** introduce a shared layout (Tera `{% include %}` for header/sidebar/footer) — the current pages duplicate the full `<nav>` in every file (compare `site/pages/index.html:10-20` vs `site/pages/overview/index.html:10-20`). v2 should factor that into one partial. Sidebar lists the docs tree; mark the active page (the static pages already use `class="active"`, e.g. `site/pages/overview/index.html:13`).

**Search (optional for v1):** a SQL-backed search is a natural *future* dynamic dogfood — `tsvector`/`pg_trgm` over a docs-content table with a tiny `GET /docs/search` handler returning an HTMX fragment. **Lean:** ship docs without search first; add search as a follow-up dynamic handler (it's a nice, low-risk Phase-1-surface dogfood). Note `pg_trgm` opclass handling has had CLI-validation sharp edges historically (prompt 003) — if search lands, exercise `pg-web check` against it.

**Dogfoods:** static-asset serving + fingerprinted CSS at scale; the shared-layout/`include` pattern; (if search) a `tsvector`/`trgm` dynamic handler. **Proves nothing Phase-2** — still warm-up, but it's the polish that makes the site worth visiting.

---

## Part C — blog (with auth + image uploads + rich editor)

THE Phase-2 dogfood. This is where the site starts exercising features that don't exist yet, so it is strictly gated (see Acceptance criteria + the build plan). Develop it as four sub-capabilities.

### C.1 — Public read (`/blog`, `/blog/[slug]`)

- `GET /blog` — dynamic index listing **published** posts (newest first). `pages/blog/index.html` + `index.sql`; handler `SELECT`s from `posts` and returns `json_agg`. Directly parallels `examples/todo/pages/index.sql` (the `json_build_object` + `COALESCE(json_agg(...), '[]')` pattern).
- `GET /blog/[slug]` — dynamic-segment route. `pages/blog/[slug]/index.html` + `index.sql`; handler reads `req->'path_params'->>'slug'` and renders one post or a not-found branch. This is exactly the `examples/todo/pages/todos/[id]/index.sql` pattern (`WHERE id::text = req->'path_params'->>'id'`, forgiving non-match → null → "not found" template branch) — reuse it verbatim, keyed on `slug` instead of `id`. Capture rules and handler-name derivation (`pgweb.pages__blog__$slug__index`) are in `docs/APP-LAYOUT.md` § Dynamic segments.
- Specificity note: static beats dynamic, so `pages/blog/index.html` (the `/blog` index) and `pages/blog/[slug]/` coexist cleanly (APP-LAYOUT § Specificity).

**Dogfoods (Phase-1 surface, but new for this site):** dynamic-segment routing + the JSON→Tera pipeline against a *real* app table — the first time pg-web.dev has app data at all. This sub-part can ship the moment the `posts` table exists; it does **not** require Phase 2 (anonymous public read).

### C.2 — Authenticated authoring (`/blog/admin`)

Login-gated create/edit/publish. **This is the Track A + Track B dogfood.**

- **Auth (session_6 Track A).** `/login` (`index.html` + `post.sql`) calls `pgweb.password_verify` + `pgweb.session_create` and sets the session cookie; `/logout` (`post.sql`) calls `pgweb.session_revoke` + clears it. These mirror `pg-web init --template auth` (session_6 Track A "scaffolds"). **Hard dependency on 013:** setting `Set-Cookie` and doing a redirect-after-login (303 → `/blog/admin`) is a *response-contract* capability the dynamic path does not have today (the HTTP layer hardcodes `text/html` and offers no header/redirect control — see prompt 005's analysis of `crates/pg_web_ext/src/http.rs`). State this plainly: **no cookies/redirects → no login → no admin.**
- **Authorization via RLS (session_6 Track B).** `posts.author_id` references `users.id`. The worker sets `SET LOCAL pgweb.user_id = '<id>'` after session validation (session_6 Track B), and RLS policies enforce:
  - read published to everyone: `USING (status = 'published' OR author_id = pgweb.current_user_id())`
  - edit/delete only your own: `USING (author_id = pgweb.current_user_id())` for `UPDATE`/`DELETE`
  - drafts visible only to their author (same `current_user_id()` check)
  Use the `pgweb.current_user_id()` helper (session_6 Track B) which returns NULL gracefully for anonymous. **Hard dependency on 014:** RLS only actually *enforces* if the request runs under a role that is subject to RLS (not a `BYPASSRLS`/superuser-ish connection). 014 is "auth role — RLS actually enforcing." Without it, the policies are decorative. Say so.
- **CSRF (session_6 cross-cutting).** Every non-GET admin action (create/edit/publish/delete) needs the double-submit token. session_6 auto-injects a combined `/_pgweb/pgweb.js` and validates `X-CSRF-Token`/`csrf_token`. The admin forms must render the token (Tera filter `{{ csrf_token | safe }}` per session_6 CSRF3) or rely on the injected HTMX header script. GET pages and anonymous requests skip CSRF by design.
- **Forms.** Reuse the `examples/todo/pages/todos/post.sql` validation pattern: `RETURNS json` handler that always succeeds, returns `{success, ...}` or `{success:false, error}`, with the template (`post.html`) branching to render either the saved-post confirmation or an OOB-swapped inline error (`hx-swap-oob`). The `EXCEPTION WHEN check_violation` → friendly error idiom transfers directly to "slug must be unique / title required."
- **State model.** `posts.status` in `('draft','published')` (+ `published_at`). Publish is a POST that flips status (and stamps `published_at`); draft autosave (Part D) writes content without publishing.

**Dogfoods:** cookie sessions (A), the `pgweb.user_id` GUC + RLS enforcement (B), CSRF (cross-cutting), redirect-after-POST + Set-Cookie (013), the existing form-validation + OOB-error pattern. This is the milestone that *proves Phase 2 auth/RLS in production.*

### C.3 — Image uploads ("dogfood images")

Authors upload a hero image and inline images for posts. **The 017 dogfood.**

- **Upload flow (HTMX).** An `<input type="file">` in the admin editor POSTs multipart to e.g. `POST /blog/admin/upload`; the handler stores the bytes and returns JSON/fragment with the asset URL, which the editor inserts into the post body (Markdown `![](url)` or an `<img src>`). 017 is "multipart uploads" — this is the first real consumer. **Hard dependency on 017** (and 013 for the response shape).
- **Where images live.** Two stores exist/are-planned: `pgweb.assets` (BYTEA, the `public/`-style path, content-hashed + immutable-cached) and `pg_largeobject` streaming for big blobs (CLAUDE.md lists the **1 MiB BYTEA-vs-largeobject cutoff as an open, un-benchmarked decision**; the asset cap was raised to **20 MiB** per session_6 § state). **Develop:** small images (hero thumbnails, inline screenshots) → `pgweb.assets` BYTEA with fingerprinting (reuse the existing `/styles.<hash>.css` machinery); anything over the cutoff → `pg_largeobject` streaming. **Lean:** start BYTEA-only with a conservative size limit (well under the 20 MiB cap), defer the large-object path to a stretch goal, and use this app to *finally benchmark* the cutoff (close the open CLAUDE.md decision with real data — that's a genuine deliverable, not just a feature).
- **How posts reference images.** Store the asset URL/hash in the post body (Markdown) and/or a `posts.hero_asset` FK to the asset row. Rendering a post resolves those to fingerprinted URLs served by the existing static-asset path.
- **Upload UX.** HTMX `hx-post` + `hx-encoding="multipart/form-data"` on the file input; server returns the inserted-image fragment or an inline error (oversize/wrong-type) using the same OOB-error pattern as C.2.

**Dogfoods:** multipart upload (017), asset storage + fingerprinting, and — if the large path is exercised — `pg_largeobject` streaming. Also produces the benchmark that closes the 1 MiB-cutoff open decision.

### C.4 — Realtime (optional stretch)

A "new post published" live update. **The Track C dogfood — explicitly stretch.**

- On publish, the handler calls `pgweb.notify_app('blog.public', jsonb_build_object('post_id', ...))` (session_6 Track C). The `/blog` index subscribes via the auto-mounted `GET /_pgweb/subscribe/blog.public` (`text/event-stream`), and an HTMX `hx-ext="sse"` element live-prepends the new post (or pings a refetch). Reuses the existing channel-aware `ListenRouter` (Session 4 G, built to be Phase-2-reusable per session_6).
- Use the `.public` channel (anyone, including anonymous, may subscribe — session_6 channel convention). Respect the 8 kB NOTIFY cap (session_6 C3): NOTIFY the post id only, let the client refetch.
- Deploy note: Caddy needs SSE-friendly buffering (`flush_interval -1`); session_6 § Risks flags this — verify the existing livereload SSE already handles it on the box.

**Mark as stretch.** The blog is a complete, honest Phase-2 dogfood without it (sessions + RLS + uploads). Realtime is the cherry that also exercises Track C; land it only after C.1–C.3 are solid.

---

## Part D — the "text editor test"

A rich-text/Markdown editor for composing posts **in the browser**, framed deliberately as a *test of pg-web's limits* — as much a find-the-sharp-edges exercise as a feature. Spell out what it stresses and why each is a real risk:

- **Large request bodies.** Editor content can be big. The dynamic request path has a **body cap (referenced as 2 MiB) in `crates/pg_web_ext/src/http.rs`** — verify the exact current value and where it's enforced — and 017 includes configurable-limits work. A long post + inline base64 (if the editor ever inlines images) blows past 2 MiB fast. **Test:** POST a deliberately huge body and confirm the failure mode is a clean 413-style error, not a panic or a truncated write. This is the headline sharp edge.
- **Content sanitization / escaping (a genuine open security question).** Rich content is untrusted HTML → stored XSS if rendered raw. `pgweb.html_escape()` exists (CLAUDE.md M1.4) but *escaping is not sanitizing* — you can't escape a post body you intend to render as HTML. There is **no HTML-sanitization story in the framework today**, and this is a real open question, not a solved one. **Lean (and the safe default):** author in **Markdown**, render server-side to a *restricted* HTML subset, and sanitize on the way in or out (allowlist tags/attrs). Decide where the sanitizer runs — push-time, a `pgweb`-side helper, or a vendored client lib (client-side sanitization is never sufficient alone). Flag to the maintainer that **introducing a sanitizer is a framework-level security decision** (which library/approach, where it runs) that this app forces — it should be made deliberately, ideally with the same rigor as session_6's CSRF threat-model section.
- **Draft autosave.** Frequent POSTs (every few seconds while typing) stress the write path and the **single-threaded BGW runtime** (cross-ref 015 on runtime/throughput; session_6 § Risks notes SPI is sync and parks the request but the tokio runtime keeps serving). **Test:** autosave under a slow `crypt()`/concurrent load and confirm latency stays sane; this is a deliberate write-amplification probe. **Lean:** debounce hard (client-side), and make autosave a cheap `UPDATE` of a single draft row — not a new row per save.
- **Preview rendering.** "Preview" round-trips Markdown → server → sanitized HTML through the handler/response contract — another exercise of 013 and the render path.
- **The editor's own JS/CSS as static assets.** Per VISION non-goals (no JS build step required) and CLAUDE.md (HTMX-first, no SPA), the editor's JavaScript is allowed **only as a vendored static asset** under `site/public/` (e.g. a small Markdown editor like a textarea-enhancer), served by the normal static path. No bundler, no framework, no client router. The app stays hypermedia-first; the editor is progressive enhancement over a plain `<textarea>`.

**Lean:** start with a **Markdown `<textarea>` + server-side render+sanitize** (boring, correct, ships). Treat full WYSIWYG as a later, separate stress test once the boring version is proven. Say plainly in the spec that Part D's *purpose* is to surface the framework's limits under realistic content load — if it finds a sharp edge (body cap UX, missing sanitizer, autosave write storms), that's a successful outcome that feeds new prompts, not a blocker.

---

## What each part dogfoods / proves

| Part / milestone | Framework feature proved | Gating prereq |
|---|---|---|
| A — landing | dynamic JSON→Tera on `/`, static assets, fingerprinted CSS (prod) | none (current surface) |
| B — docs | shared-layout `include`, static serving at scale; (opt) `tsvector`/`trgm` search | none; doc-render tooling decision |
| C.1 — public blog | dynamic-segment routing (`[slug]`), JSON→Tera against a real app table | `posts` table only |
| C.2 — admin/auth | cookie sessions (A), `pgweb.user_id` + RLS *enforcement* (B), CSRF, redirect+Set-Cookie | **013, 014, Phase 2** |
| C.3 — uploads | multipart upload, asset storage + fingerprinting, (opt) `pg_largeobject` | **017** (+013); closes 1 MiB-cutoff decision |
| C.4 — realtime | SSE subscribe + `notify_app` + `ListenRouter` reuse | **Phase 2 Track C** (stretch) |
| D — editor test | body-limit behavior, sanitization story, autosave write path, single-threaded runtime under load | 013, 017, 015 (context) |

This table is the Phase-2 acceptance map: when every "proved" cell is live on pg-web.dev, Phase 2 is dogfood-complete.

---

## Research tasks

A session picking this up should, read-only first:

1. **Confirm the current site exactly** (cited above): walk all of `site/pages/`, `site/public/styles.css`, `site/pgweb.toml`, `site/Caddyfile`, `site/docker-compose.yml`, `site/scripts/bootstrap-hetzner.sh`, `site/README.md`. Confirm the dead `/app-layout` link (`site/pages/_404.html:30`) and the README/route drift.
2. **Re-read the reuse patterns in `examples/todo/`**: the `[id]` capture handler (`pages/todos/[id]/index.sql`), the plpgsql `EXCEPTION WHEN check_violation` validation + OOB-error template (`pages/todos/post.sql` + `post.html`), the text-mode delete (`pages/todos/delete/post.sql`), and the migration (`migrations/0001_create_todos.sql`). These are the templates for `/blog/[slug]` and every admin form.
3. **Read `docs/APP-LAYOUT.md` in full** — especially § Dynamic segments (capture syntax, `$slug` handler-name derivation, static-beats-dynamic specificity) and § "What `pg-web push` writes" (static-only page synthesis).
4. **Read `docs/internal/sessions/session_6.md` in full** — Tracks A/B/C surfaces, the cross-cutting CSRF section, the `pgweb.user_id` invariant (#9), and the component shipping order. This is the contract the blog consumes. **Note its status: DRAFT, open questions unclosed** — confirm with the maintainer which Track A/B/C open questions (cookie `Secure` default A1, session expiry A5, channel format C1, etc.) are settled before building against them.
5. **Verify the response-contract assumptions** by reading `crates/pg_web_ext/src/http.rs` and `crates/pg_web_ext/src/router.rs` (per prompt 005's research map): confirm there is still no Set-Cookie/redirect/content-type control on the dynamic path, and confirm the exact request-body cap (the "2 MiB" figure) and where it's enforced. These define what 013/017 must deliver before C.2/C.3/D can start.
6. **Locate prompts 013, 014, 015, 017, 019** (not yet present in `prompts/` at `918f40b` — the dir stops at 012) and confirm their scope/status with the maintainer; this spec forward-references them as named prereqs.
7. **Decide the docs-render mechanism** (Part B): prototype the smallest committed `site/scripts/render-docs.sh` (Markdown → HTML fragment in the shared layout) and confirm it needs no CLI/extension change.
8. **Sanitization survey** (Part D): identify candidate approaches (allowlist HTML sanitizer location: push-time vs `pgweb`-side helper vs vendored client lib) and bring options to the maintainer — this is a deliberate security decision, not an implementation detail.

---

## Constraints & invariants to respect

From the root `CLAUDE.md` (cite these in any PR description):

- **This must be a NORMAL pg-web app.** Directory-as-route, filename-as-method, `(req json) RETURNS json|text` handlers, dispatch via `template_path` nullability (`docs/APP-LAYOUT.md`). No new route conventions.
- **HTMX-first, no SPA, no JS build step** (VISION non-goals). The editor's JavaScript is allowed **only as a vendored static asset** in `site/public/`; the app stays hypermedia-first (progressive enhancement over `<textarea>`). No bundler, no client router.
- **Invariant #4: one HTTP request = one SPI transaction.** Every handler (login, save, upload, autosave) runs in exactly one SPI tx that commits on 2xx or rolls back on error. No multi-tx requests; no leaked transactions.
- **Invariant #2: HTTPS is out-of-process.** Caddy terminates TLS; pg-web speaks plain HTTP on :8080. Don't add TLS to the extension. Keep `site/Caddyfile` as the only TLS surface.
- **Invariant #3: extension ↔ CLI decoupling.** The Part B doc-render tooling lives in the CLI/site scripts and emits ordinary `pages/` files; it must not add HTTP logic to the CLI or filesystem logic to the extension. **Lean:** prefer the site-local script over a CLI feature until a second app needs it.
- **Invariant #9 (Phase 2): `pgweb.user_id` is the "who is this request" contract.** Set via `SET LOCAL pgweb.user_id` after session validation; NULL = anonymous; RLS reads it via `pgweb.current_user_id()`. The blog's authorization is built entirely on this.
- **Companion-app rule.** Every framework feature must be exercised by a real app before it's "done." **This site is meant to BE that proof for Phase 2** — that's the whole point of the work order, so the bar is: if pg-web.dev's blog doesn't use a Phase-2 feature, that feature isn't dogfood-complete.
- **Keep the deploy story intact.** Caddy + `site/scripts/bootstrap-hetzner.sh` + `pg-web push --with-migrate` (runbook: prompt 012). New migrations for `users`/`posts` go through `pg-web migrate apply`; routes/templates/assets through `push`. Don't invent a new deploy path.
- **Phase discipline.** Don't pull Phase-2 code into the app before the Phase-2 *framework* features land — Parts A/B/C.1 are buildable now; C.2/C.3/C.4/D wait for their gates.

---

## Acceptance criteria

1. **pg-web.dev serves a real landing page** at `/` — hero, zero-proxy pitch, live `pages/` code sample, `pg_dump`-artifact story, quickstart, links into `/docs` and `/blog` — rendered through the JSON→Tera handler (extends `pgweb.pages__index`).
2. **`/docs` renders the documentation** with a shared layout (header + sidebar + footer via `include`), active-page highlighting, good mobile; docs content is authored in Markdown and rendered to pure pg-web pages (push-time/site-script render), with the old `/overview`/`/layout` routes either migrated or cross-linked.
3. **`/blog` lists published posts** and **`/blog/[slug]` renders a single post**, both as dynamic handlers reading the `posts` table; a non-existent slug renders a not-found branch (no 500).
4. **An author can log in** (session cookie set via the 013 response contract), and is redirected (303) to `/blog/admin`; **logout** clears the cookie.
5. **An author can create a post** and is redirected after save; **an author can edit only their own posts** — RLS (enforced under the 014 role) returns zero rows / 403 for another author's post; **drafts are invisible** to everyone but their author.
6. **CSRF is enforced** on every non-GET admin action (token present → 200, missing/mismatched → 403); GET and anonymous requests skip it.
7. **An author can upload an image** (multipart, 017) that is stored as an asset and **appears in a rendered post**, served via the fingerprinted static-asset path; oversize/wrong-type uploads return a clean inline error, not a crash.
8. **The editor handles a large post body** within configured limits — a deliberately huge body fails cleanly (413-style), and a normal long post saves and renders; **autosave** works as a debounced single-row `UPDATE` without write storms.
9. **Rich content is sanitized** — a post containing `<script>`/`onerror=` payloads renders inert (no stored XSS); the sanitization approach and where it runs are documented.
10. **The whole thing deploys via the existing flow** — `pg-web migrate apply` (for `users`/`posts`) + `pg-web push --with-migrate` on the Hetzner box (prompt 012), behind Caddy; `pg-web check` passes cleanly on `site/`.
11. **Every Phase-2 feature is exercised here in production** — sessions, RLS, CSRF, uploads, and (if landed) realtime — satisfying the companion-app rule for Phase 2.
12. **The spec documents which prompts gate which milestones** — 013 (cookies/redirects/content-type) and 014 (RLS-enforcing role) gate C.2; 017 gates C.3; Phase-2 Track C gates the C.4 stretch; and the build plan is dependency-ordered (A → B → C.1 → C.2 → C.3 → C.4 → D).

---

## Open questions

1. **Docs Markdown-render location.** Push-time site-script (Lean) vs a first-class `pg-web` CLI render feature vs dynamic SQL-side rendering? When (if ever) does a second consumer justify promoting the script to a CLI feature?
2. **Editor library vs plain textarea.** Ship a Markdown `<textarea>` (Lean) or vendor a lightweight editor (e.g. an EasyMDE-class enhancer) as a static asset? At what point does WYSIWYG justify its sharp edges, and does any WYSIWYG choice stay hypermedia-first?
3. **Image storage threshold.** Where exactly is the BYTEA→`pg_largeobject` cutoff (the un-benchmarked 1 MiB CLAUDE.md decision)? Can this app's real uploads close that decision with data, and what conservative per-image cap ships for v1 (well under the 20 MiB asset cap)?
4. **Draft/publish state model.** `status enum('draft','published')` + `published_at` — is that enough, or do we need scheduled posts / `archived` / per-post visibility beyond RLS? How does autosave interact with publish (single draft row vs revisions)?
5. **Sanitization library/approach (security decision).** Allowlist sanitizer — where does it run (push-time, a new `pgweb`-side helper, vendored client lib)? Markdown-only restricted subset vs sanitized rich HTML? This forces a framework-level call; who owns it and with what threat model (mirror session_6's CSRF rigor)?
6. **Docs source of truth.** Does `/docs` render from the repo's `docs/*.md` directly (single source, but couples site builds to maintainer docs) or from hand-authored `site/content/*.md` (decoupled, but drifts)? The current site already hand-ports with no sync (`site/README.md`) — pick a direction and kill the drift.
7. **SEO / meta for marketing.** Open Graph / Twitter-card meta, canonical URLs, sitemap, and the `/overview`→`/docs/overview` 301s — all want the 013 response contract (custom headers/redirects). How much SEO ships in v1 vs waits for 013?
8. **session_6 open questions that block the blog.** Several Track A/B/C questions are still open in the DRAFT (cookie `Secure`-in-prod-only A1, sliding vs hard session expiry A5, channel-name format C1, combined `pgweb.js` injection CSRF1). Which must be settled before C.2/C.4 can be built, and are any of them things the blog itself should help decide by being the first real consumer?
9. **Admin surface scope.** Single-author (just the maintainer) or true multi-author with signup? Multi-author makes RLS isolation a *visible* feature (stronger proof) but adds the signup/`users`-management surface. Lean: seed one author for v1, keep the schema multi-author-ready.
