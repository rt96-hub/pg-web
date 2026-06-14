# 026 — In-browser dev dashboard

**Status:** Future / interesting DX enhancement (listed under Phase 4 in current ROADMAP)  
**Date opened:** 2026-06-13  
**Author:** Discussion with maintainer (2026-06-13)  
**Prerequisites:** 018.1 (health/readiness endpoints) would be nice context but not strictly required.  
**Related:** Original 018 (deferred logging/metrics were partially motivated by feeding a dashboard); prompt 006 (dev access logs); CLAUDE.md Phase discipline rules; ROADMAP Phase 4 observability section.

---

## Summary

There is interest in a **visual, in-browser dev dashboard** — a hosted page (or set of pages) that becomes available during development and surfaces useful information about the running pg-web app in a friendly, glanceable way.

The goal is richer developer experience *inside the browser* during `pg-web dev` / local iteration, without forcing the developer to constantly switch between their code editor, terminal logs, or CLI commands.

Key themes from current thinking:
- **Dev-only** (env-gated or explicitly disabled in production, similar to livereload, to avoid any exposure on the hosted site or in customer deployments).
- Visual and useful: sitemaps / route overviews, status checks, migration history, current settings, etc.
- Built primarily against existing framework state (`pgweb.routes`, `pgweb.migrations`, `pgweb.templates`, `pgweb.settings`, `pgweb.deployments`, etc.) rather than requiring new persistent logging or metrics tables.
- HTMX-driven (consistent with the rest of the framework) and served directly by the extension via the `_pgweb/*` reserved namespace.
- Lightweight and fun to use — a "nice visual manner" that feels like a bonus during development rather than production observability infrastructure.

This idea has existed in the roadmap for a while under slightly different names and motivations. The discussion in mid-June 2026 refreshed it with a stronger emphasis on visual dev DX and decoupling from the heavier request-log + metrics work.

---

## Prior plans and references

- **docs/ROADMAP.md (Phase 4 — Observability / dashboard)**:
  - "In-browser dev dashboard at `/_pgweb/admin` — HTMX against the BGW; shows routes, templates, recent requests."
  - Related items in the same section include "Request log + slow-request capture — `pgweb.request_log` with sampling" (explicitly noted in the original 018 as the durable backing store for the dashboard).

- **Original prompts/018_extension_upgrades_and_observability.md** (before the 018.1 / 018.2 split):
  - Positioned a sampled `pgweb.request_log` table as "the durable, queryable tier behind a future `/_pgweb/dashboard`".
  - Dashboard was one of the motivators for the broader lifecycle/observability work order.

- **CLAUDE.md**:
  - Explicit Phase discipline: "Do not add Phase 2+ features (auth/RLS, job queues, **dashboard**) into Phase 1 code paths. Stage them properly."
  - We are still in Phase 1 (Synchronous Core) focus.

- Other mentions:
  - `docs/OVERVIEW.md`, `docs/APP-DEVELOPER-GUIDE.md`, and `site/pages/roadmap/index.html` all reference the dashboard as a Phase 4 item.
  - Some older notes also use the name `/_pgweb/dashboard` (minor naming inconsistency to resolve later).

The original 018 also tied dashboard needs to the deferred request logging work (see the cleaned-up `prompts/018_extension_upgrades_and_observability.md`, which now holds only the remaining future tasks around access logs and metrics).

---

## Current thinking (June 2026 discussion)

The maintainer likes the core idea of a **hosted web page in dev mode** that provides a ton of details in a visual, easy-to-consume format:

- Sitemaps / complete route overview (what paths exist, which have templates vs. raw handlers, dynamic segments, etc.).
- Checks (surface the new health/readiness endpoints from 018.1, app-level sanity, perhaps migration status).
- Migration overview (what has been applied, any pending?).
- Other runtime state presented nicely (current env/settings, recent deployments from the ledger, asset counts, etc.).
- Interactions via HTMX where it adds value (expand a route to see its handler source? quick status refreshes?).

Important guardrails:
- Strictly dev-only. It must not be a vector for information disclosure in production or on the public pg-web.dev site.
- Does **not** require the full production request logging or metrics surface to be valuable. Start by reading what the framework already knows.
- Better dev visibility might come from a combination of this dashboard + continued improvements to `pg-web dev` output (via prompt 006) + richer per-app logging that individual handlers can produce.
- Avoid over-engineering into a full token-protected admin UI or production observability tool (those can stay further out or be explicitly non-goals for v1 of this).

This feels like high-leverage DX polish rather than core infrastructure. It could make the "live in the browser while developing" story significantly nicer.

---

## Instructions for the implementing agent / future session

**This is still early and intentionally lightweight.**

**Before you expand this prompt significantly, produce a detailed design doc, choose data models, write any code, or update many files, you must stop and plan with the maintainer (Robert).**

Use discussion (and tools such as `ask_user_question` if it helps surface options cleanly) to co-refine:

- Exact first-cut feature list and priorities (sitemaps first? checks? migrations? something else?).
- The public path(s) — `/_pgweb/admin`, `/_pgweb/dev`, `/_pgweb/dashboard`, or another convention?
- How much (if any) new state or tables are truly needed vs. pure reads of existing `pgweb.*` tables + extension internals.
- Relationship to 018.1 health/readiness (should the dashboard prominently surface those?).
- Gating mechanism (purely `env == 'development'` like livereload, or an explicit `pgweb.toml` / settings flag?).
- Visual vs. utilitarian balance and HTMX patterns to use.
- Companion-app requirements (per CLAUDE.md: every feature must be exercised in `examples/todo/` with substantial explanatory comments that teach the pattern).
- Whether any of the deferred work in the current 018 (request logging / metrics) should feed the dashboard, or whether they stay fully separate.
- Interaction with prompt 006 and any CLI-side dev experience improvements.
- Phase discipline — confirm this stays out of Phase 1 paths.

After alignment, update this prompt (and the roadmap) with the agreed scope, then proceed.

Treat the current text as a starting point and signal of interest, not a locked spec.

---

## High-level shape (for orientation only — refine with the maintainer)

- Framework-reserved routes under the `_pgweb/*` namespace (mounted the same way as livereload and the 018.1 health endpoints).
- Primarily read-only SPI queries inside the normal per-request transaction model.
- HTMX + small amount of vanilla JS for a pleasant experience (no heavy frontend build).
- Dev-only: returns 404 (or a minimal disabled page) when not in development, or when explicitly turned off.
- Good comments everywhere, especially in any new handlers and in the `examples/todo/` demo flow.

No need to solve production admin UI, auth, or full observability as part of the first version.

---

*Every time this prompt is picked up, re-read the latest CLAUDE.md, the current state of 018 (the remaining deferred tasks), 018.1, the ROADMAP dashboard section, and prompt 006 before writing anything new.*