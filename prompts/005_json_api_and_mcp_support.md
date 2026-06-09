# 005 — JSON API responses and MCP tool exposure for agent-driven and automated workflows

**Status:** Open diagnostic / strategic exploration prompt  
**Date opened:** 2026-05-28  
**Context:** Emergent need identified through user interest in using pg-web applications as programmable backends and memory layers for AI agents, automation systems, research tooling, and internal agent swarms — beyond the current HTML/HTMX-centric design.

---

## Summary

pg-web is deliberately and successfully architected as an **HTML-first, hypermedia-driven framework**. SQL handlers return either JSON (fed to Tera templates for full pages or HTMX fragments) or raw text; the HTTP layer in the extension always emits `text/html` (or asset content-types) for dynamic responses. This model delivers an exceptionally clean "SQL is the business logic + Tera is the glue" developer experience for traditional web applications.

A growing class of use cases wants the **same application** (or a closely related surface of it) to also act as a clean, sparse **JSON API backend** and/or to expose selected functionality as **MCP (Model Context Protocol) tools** consumable by AI agents and automated systems. These consumers do not want (and often cannot usefully consume) HTML, Tera-rendered pages, or hypermedia responses.

The current architecture provides no first-class path for this. The documentation even advertises "JSON APIs" as a legitimate use case for `.sql`-only raw-text routes (`docs/APP-LAYOUT.md`), yet the implementation hard-codes HTML content types and offers no mechanism for custom response headers, content negotiation, or non-HTML response shapes.

This prompt asks a future agent (or human maintainer) to **research the full request/response pipeline**, map the constraints, enumerate multiple architectural directions, surface the strategic trade-offs, and — crucially — **discuss options with Robert** before any design or implementation work begins. The goal is shared understanding and a set of well-scoped follow-up prompts rather than premature code.

## Detailed Description

### The current architectural reality (HTML/HTMX-centric by design)

The entire serving path is optimized for the hypermedia contract:

- **Filesystem convention** (`pages/<segments>/<method>.sql` + optional sibling `.html`): codified in `crates/pg_web_cli/src/paths.rs` (`RouteEntry`, `scan()`, `is_raw_text()`, `is_full()`, etc.) and enforced at push time.
- **Route storage**: `pgweb.routes` table (`crates/pg_web_ext/src/schema.rs`) with nullable `template_path`. Non-NULL → JSON handler result + Tera render. NULL → raw `text` return from handler, passed through verbatim.
- **Dispatch** (`crates/pg_web_ext/src/router.rs`):
  - `serve()` / `serve_in_tx()` → `lookup_route()` → `render_route()`
  - `call_handler()` always invokes the user function as `(req json) RETURNS json|text` via the `_framework_call_handler` wrapper (which returns the result as text).
  - `ServeOutcome::Response { status, body }` for both templated and raw-text dynamic routes.
  - `ServeOutcome::Asset { ... }` only for `public/` static files (which do carry their true `content_type` from `pgweb.assets`).
- **HTTP shaping** (`crates/pg_web_ext/src/http.rs`):
  - The single `handle()` fallback always sets `Content-Type: text/html; charset=utf-8` for `ServeOutcome::Response`.
  - Livereload injection and dev error pages are also HTML-only paths.
  - No `Accept` header inspection. No per-response header control. No `application/json` branch anywhere in the dynamic path.
- **CLI validation** (`crates/pg_web_cli/src/push.rs`): `validate_handler()` enforces the exact `RETURNS json` vs `RETURNS text` rule based solely on presence/absence of a sibling template. No other response-type metadata exists.

This design is intentional and highly successful for its primary mission (see `docs/VISION.md`, `CLAUDE.md`, `docs/ARCHITECTURE.md`). Tera + HTMX + SQL gives developers an extremely productive loop with almost zero client-side JavaScript. The "Zero-Proxy" value proposition is built on this tight integration.

### The emerging need (general and strategic)

Users (and potential users) want pg-web applications to participate in **agentic and automated ecosystems** as:

- Clean, predictable **JSON backends** that agent frameworks, scripts, internal tools, and other services can call without parsing HTML or fighting template output.
- **MCP servers / tool providers**: exposing a curated set of the application's capabilities (queries, actions, state mutations) as discoverable, typed tools that LLM agents can invoke directly via the Model Context Protocol (stdio, HTTP+SSE, or future transports).
- **Memory / state layers** for agent swarms: the Postgres database (with its rich schema, RLS, triggers, and the pg-web handler surface) becomes the durable, queryable, transactional brain that multiple agents read from and act upon.
- **Research and automation tooling** where the same core business logic (written once in SQL) powers both a human-facing web UI *and* a machine/agent interface.

Key strategic reasons this matters broadly (not tied to any single downstream project):

1. **Dual-surface applications without duplication**. The same `pages/` handlers (or a parallel but related set) can serve human users via rich HTML/HTMX and agents via sparse JSON or MCP without forcing every consumer through a rendering pipeline designed for browsers.
2. **Programmable backends for the agent era**. As agent swarms, autonomous workflows, and "AI employees" become common, many organizations will want their internal systems and data models exposed in the exact protocols agents already speak (HTTP JSON + increasingly MCP). pg-web's "everything lives in Postgres, SQL is the logic" model is philosophically a *natural* fit for this — if the HTTP surface can speak the right dialects.
3. **Research tools and internal automation**. Teams building data-heavy research platforms, ETL controllers, monitoring dashboards, or bespoke automation frequently need both a web UI for humans *and* reliable machine interfaces. Forcing the machine path through Tera templates creates friction and accidental coupling.
4. **Long-term extensibility of the framework itself**. Treating JSON APIs and MCP surfaces as optional, first-class, well-supported extension points (rather than "you can hack it with raw text and hope") keeps the core HTML/HTMX contract pure while opening the platform to entirely new classes of consumers and deployment patterns.
5. **Consistency with the "Zero-Proxy" philosophy**. If the database can be the web server, it can also reasonably be (part of) the agent tool server. The question is how cleanly we expose that capability.

Note that `docs/ROADMAP.md` already contains forward-looking discussion of MCP — but focused on *framework documentation consumption by agents writing pg-web apps*, plus speculative runtime data-access MCP ideas. Prompt 005 is about the orthogonal (and more immediate for some users) question of **user applications** exposing JSON/MCP surfaces for *their own* data and logic.

### The documentation vs. reality gap (important to surface)

`docs/APP-LAYOUT.md` (table in "Which half you ship determines the pipeline") explicitly lists under `.sql` only:

> HTMX fragments; **JSON APIs**; no-content

Yet today a handler that does `RETURNS text` and emits a JSON string will still be served with `Content-Type: text/html`. There is no supported way to set `application/json`, custom headers, status codes beyond what the router already supports, or to opt a route into a completely different response contract.

This is not a bug in the current implementation — it is an accurate reflection of the Phase 1 / v0.1–v0.2 design priorities. It does, however, mean that anyone following the documented advice for "JSON APIs" will hit friction immediately.

## Research Tasks for the Reader

A future session working from this prompt must begin with deep, read-only exploration (no implementation until after discussion with Robert). Specific areas to investigate:

1. **Full request/response lifecycle** (start here):
   - `crates/pg_web_ext/src/http.rs` (entire file, especially `handle()`, `render_asset()`, response construction, lack of Accept negotiation)
   - `crates/pg_web_ext/src/router.rs` (entire file: `ServeOutcome` enum, `render_route()`, `call_handler()`, `_framework_call_handler` interaction, how `template_path` nullability controls everything)
   - `crates/pg_web_ext/src/schema.rs` (the `pgweb.routes` and `pgweb.templates` tables, the wrapper function, default hello route)
   - `crates/pg_web_ext/src/errors.rs` (how errors are rendered — also HTML-biased in dev mode)

2. **CLI side and layout contract**:
   - `crates/pg_web_cli/src/paths.rs` (`RouteEntry`, `scan()`, `is_raw_text()` etc., how the three combinations are derived from the filesystem)
   - `crates/pg_web_cli/src/push.rs` (especially `apply_entry()`, `validate_handler()` return-type enforcement, route + template reconciliation)
   - `crates/pg_web_cli/src/check.rs` (whether any of this would affect the offline validator)

3. **Documentation claims and philosophy**:
   - `docs/APP-LAYOUT.md` (the table claiming JSON API support)
   - `docs/APP-DEVELOPER-GUIDE.md` (handler contract section, raw-text examples — note they are almost all HTML fragments today)
   - `docs/ARCHITECTURE.md` (the diagrams and "two dispatch modes" description)
   - `docs/ROADMAP.md` (MCP section + the explicit "Over HTTP JSON is fine if someone wants to build it on top" note under Out of Scope)
   - `docs/VISION.md` and `CLAUDE.md` (the core "SQL + HTML + HTMX + Tera" mission statement — any proposed changes must not drift from this)
   - `examples/todo/` (current patterns for both full pages and raw-text fragments)

4. **External context (MCP)**:
   - Current state of the Model Context Protocol (stdio transport, HTTP/SSE transport, tool definition format, resource exposure, etc.). The reader should be able to speak accurately about what an MCP server surface actually requires from an application.

## Possible Solution Directions (to explore, not to choose)

The prompt deliberately does **not** prescribe a direction. The reader should develop and compare several, including at least:

- **Lightweight response shaping inside the existing model**
  - Allow handlers (or a new convention) to return a small envelope that includes `body`, `content_type`, and perhaps `headers`.
  - Or a magic query parameter / header that opts a request into "JSON mode" (bypassing Tera even if a template exists, and forcing the right Content-Type).
  - Minimal change to the handler contract.

- **Dedicated API surface / new file or directory conventions**
  - `api/` parallel to `pages/` (or `pages/api/` subtree) with its own scanning rules, a `pgweb.api_routes` table, and a completely separate dispatch path that never touches Tera or the HTML content-type logic.
  - New stem conventions (e.g. `index.json.sql` or explicit `api_index.sql`).

- **Content negotiation as a first-class (but optional) concern**
  - Inspect `Accept` header in `http.rs`.
  - Allow a single route/handler to serve multiple representations (HTML for browsers, JSON for agents) based on negotiation.
  - Significant complexity; may conflict with the "one handler per (method, path)" simplicity.

- **MCP as an orthogonal extension**
  - A separate MCP server process (or another Postgres background worker) that speaks the MCP protocol and invokes the *existing* SQL handlers (or a curated subset registered in a new metadata table) as tools.
  - Could live in the CLI (`pg-web mcp serve`), as a sidecar, or as an optional second extension.
  - Keeps the core HTTP/HTML path untouched.

- **Separate extension crate(s)**
  - `pg_web_api` or `pg_web_mcp` as optional companion extensions that can be loaded alongside `pg_web_ext` and provide additional schemas/tables + their own HTTP listeners (or stdio handlers) without polluting the primary HTML surface.

- **Hybrid / long-term evolution**
  - First solve "let a handler cleanly produce JSON with the correct Content-Type and optional custom headers."
  - Later layer MCP tooling on top of that (or directly on the SQL functions).
  - Preserve the invariant that the *default*, zero-config experience remains HTML/HTMX-first.

Each direction has deep implications for:
- The handler contract and `pgweb.routes` schema
- Push-time validation and reconciliation
- Error handling and dev experience
- Documentation and the "three file combinations" mental model
- Performance (another code path through the hot request loop)
- Testing story (new tiers in `docs/TESTING.md`)
- Backwards compatibility

## Instructions for the Next Agent / Session

**Do not write code, do not propose a concrete design doc, and do not pick a winner** until you have:

1. Completed the research tasks above (use as many tool calls as needed; be exhaustive).
2. Written up a clear, balanced comparison of the viable directions (including "do nothing / document the limitation explicitly" as a valid option).
3. **Discussed the findings and trade-offs directly with Robert** (the user). Use the `ask_user_question` capability or equivalent conversational mechanism. Surface the strategic "why" questions as much as the technical ones.
4. Identified a small set of tightly-scoped **follow-up prompts** that would be appropriate *after* a direction is chosen (e.g. "Detailed design for option X", "Prototype of minimal JSON response shaping", "MCP stdio server spike against existing handlers", "Impact analysis on APP-LAYOUT and DEVELOPER-GUIDE", etc.).

**Mandatory reading before any substantial output:**
- This entire prompt.
- The full contents of the files listed in the Research Tasks section.
- The existing prompts 001–004 (for tone and level of rigor expected in diagnostic documents).
- Recent session notes in `docs/sessions/` if they discuss extensibility or future surfaces.

**Strongly recommended:**
- Actually exercise the current raw-text path with a handler that returns a JSON string and observe the exact `Content-Type` and body in curl / browser devtools.
- Experiment (in a scratch SQL file against a running `pg-web up` stack) with what a realistic MCP tool definition and invocation might look like if the underlying capability already existed as a `(req json) RETURNS json` handler.
- Consider the interaction with other future features (auth/RLS in Phase 2, async jobs in Phase 3, observability, etc.).

This work is high-leverage strategic thinking. The framework's core strength is its ruthless focus on a coherent, simple model. Any expansion into JSON/MCP territory must be done with the same clarity and respect for invariants — or it must be explicitly scoped as a deliberate, opt-in second surface with its own rules.

The output of the session that picks up this prompt should leave Robert with a shared mental model and a clear, prioritized list of next concrete prompts (or a decision to defer).

---

**Related files / history**

- `crates/pg_web_ext/src/http.rs`, `router.rs`, `schema.rs`, `errors.rs`, `templating.rs`
- `crates/pg_web_cli/src/paths.rs`, `push.rs`, `check.rs`
- `docs/APP-LAYOUT.md`, `docs/APP-DEVELOPER-GUIDE.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/VISION.md`, `CLAUDE.md`
- `examples/todo/` (current handler and template patterns)
- `docs/TESTING.md` (will need updating for any new surfaces)
- Model Context Protocol specification (external reference)

**Priority:** Strategic / medium-term (not a current correctness or DX blocker for pure HTML apps, but increasingly relevant for the agentic future of many potential users).  
**Risk of change:** High if the core HTML contract or handler ergonomics are altered; low-to-medium if implemented as cleanly separated optional surfaces.

---

*This document is intentionally written as both a design record and a ready-to-use prompt for a future agent session. Feed it (plus the full current state of the files listed above) to the agent when the work is scheduled. The explicit instruction is to research thoroughly, surface options and trade-offs, and discuss with Robert before any implementation-oriented follow-up prompts are authored.*
