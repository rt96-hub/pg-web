# 013 — Response Contract v2: status codes, headers, cookies, redirects, and content negotiation

**Status:** ✅ Completed (moved to `prompts/completed/` 2026-06-12)
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Prerequisites:** none (this is itself a prerequisite for 014 auth, 017 uploads, 020 site-v2, and prompt 005 JSON/MCP)
**Context:** Today a pg-web handler can only influence the *body* of a response. Status code is fixed by the framework (200 / 404 / 500), the content type is hardcoded to `text/html; charset=utf-8`, and there is no channel at all for response headers, cookies, or redirects. Phase 2 (auth, sessions, CSRF) and several other planned features assume a mechanism — `Set-Cookie`, `Location`, `Content-Type: application/json`, `Cache-Control` — that simply does not exist yet. This prompt designs that mechanism: the **response contract v2**.

---

## Summary

Every dynamic response pg-web emits is currently shaped by one arm of `http::handle` that hardcodes `text/html; charset=utf-8` and takes its status (200 or 404) from the router, never from the handler (`crates/pg_web_ext/src/http.rs:121-135`). The router's `ServeOutcome::Response` variant carries only `{ status: u16, body: String }` (`crates/pg_web_ext/src/router.rs:46-60`), and `render_route` can only return a body string plus a framework-chosen status (`crates/pg_web_ext/src/router.rs:189-221`). The PL/pgSQL wrapper that invokes user code casts the handler result to `text` and returns that single scalar (`crates/pg_web_ext/src/schema.rs:131-163`, executing `format('SELECT (%s($1))::text', ...)`). There is no place — in the handler return value, the route row, or the wrapper — for a handler to express anything other than a body.

This work order proposes a **response contract v2**: a backward-compatible way for a SQL handler to optionally return a richer response (status, headers, cookies, redirect, explicit content type) while leaving the existing bare-`json` and bare-`text` returns working **byte-for-byte unchanged**. The design centres on a reserved **response envelope** (the wire format the router detects) plus a set of **SQL helper functions** (the ergonomic surface app authors actually call), mirroring how `pgweb.html_escape` / `pgweb.setting` already wrap raw operations in the install SQL.

The change is the enabling layer beneath at least four planned features and is therefore scheduled as a keystone, not a leaf.

## Why this matters now

This is not a speculative nicety — four concrete, already-specced pieces of work are blocked on it:

1. **Blocks auth (Phase 2).** `docs/internal/sessions/session_6.md` Track A specifies a login POST handler that "sets cookie via `Set-Cookie` header in the response" (`session_6.md:49`), and the CSRF cross-cutting track sets a `pgweb_csrf` cookie "on every response" (`session_6.md:111`). Both are impossible today: a handler has no way to emit a header of any kind, let alone `Set-Cookie`. Cookie sessions — the foundation of the entire Phase 2 auth story — cannot be built until this lands.
2. **Blocks JSON APIs.** `docs/APP-LAYOUT.md` advertises "JSON APIs" as a first-class `.sql`-only use case (the pipeline table at `APP-LAYOUT.md:19`, and again at `APP-LAYOUT.md:218`), but a handler that `RETURNS text` emitting a JSON string is still served with `Content-Type: text/html; charset=utf-8` (`http.rs:131`). `prompts/005_json_api_and_mcp_support.md` documents this exact docs-vs-reality gap and explicitly asks for a design. **This prompt is the enabling layer beneath 005** — once a handler can set `application/json` and arbitrary headers, 005's JSON-surface and MCP work has something to build on.
3. **Blocks redirects / Post-Redirect-Get.** There is no 302/303 + `Location` path anywhere. Plain-form (non-HTMX) POST flows — login, signup, "create then redirect to the new resource" — cannot do the standard PRG dance. HTMX flows paper over this with `hx-*` swaps, but the moment a real browser form submits without HTMX, the framework has no answer.
4. **No control over caching or status for dynamic pages.** A handler cannot set `Cache-Control` on a dynamic page (only static assets get cache headers, computed in `render_asset`, `http.rs:158-194`), cannot return 201 / 204 / 4xx for an API, and cannot set the CSRF cookie Phase 2 needs. The framework's status vocabulary for handler-driven routes is "200, or 404 if it was the fallback" — nothing else.

Doing this now, before 014/017/020 and before the Phase 2 auth tracks start, means those features inherit a clean contract instead of each inventing its own escape hatch.

## Current behavior (evidence)

Read these before designing anything. Line numbers are against git `918f40b`.

**The HTTP layer hardcodes everything about a dynamic response except the body.**
`crates/pg_web_ext/src/http.rs:120-143` — the `match router::serve(...)` block. The `ServeOutcome::Response { status, body }` arm:
- converts the router's `u16` status with `StatusCode::from_u16(status).unwrap_or(StatusCode::OK)` (`http.rs:122`),
- runs livereload injection on the body (`http.rs:127-128`),
- and builds the response with a **literal** `[(header::CONTENT_TYPE, "text/html; charset=utf-8")]` (`http.rs:129-134`).

There is no header map, no cookie list, no content-type variable. The only dynamic responses that carry a true content type are static assets, via `render_asset` (`http.rs:136-140`, `158-194`), which reads `content_type` from `pgweb.assets`.

**The router's outcome type has no header/status-from-handler channel.**
`crates/pg_web_ext/src/router.rs:45-60` — `ServeOutcome` is:
```rust
pub enum ServeOutcome {
    Response { status: u16, body: String },
    Asset { body: Vec<u8>, content_type: String, etag: String },
    Error(ServeError),
}
```
The `status` on `Response` is set by the *router* (`render_route(&matched.route, &req, 200)` at `router.rs:78`; `404` at `router.rs:102`/`103`), never by the handler.

**`render_route` returns body + status only, branching on `template_path`.**
`crates/pg_web_ext/src/router.rs:189-221`. Two modes, exactly as the dispatch doc-comment at the top of the file describes (`router.rs:6-8`):
- `template_path` is `Some` → parse `handler_text` as JSON, feed Tera, `ServeOutcome::Response { status, body }`.
- `template_path` is `None` → `ServeOutcome::Response { status, body: handler_text }` (raw text verbatim).

Note the JSON-parse path at `router.rs:201-210`: in template mode, the handler's text **must** parse as JSON or you get `ServeError::HandlerReturnNotJson`. Any envelope design has to decide how it coexists with this parse.

**The handler wrapper returns the result cast to `text` — a single scalar.**
`crates/pg_web_ext/src/schema.rs:131-163` — `pgweb._framework_call_handler(p_handler_name TEXT, p_req JSON)` returns a `TABLE(ok, result_text, error_sqlstate, error_message, error_detail, error_hint, error_context)`. The happy path is `format('SELECT (%s($1))::text', p_handler_name)` → `EXECUTE ... INTO result_text` (`schema.rs:148-149`). The router reads that one `result_text` column (`router.rs:539-542`). There is **no** structured response coming back from the handler — just a text blob. This is the single most important constraint: whatever the handler produces has to survive a `::text` round-trip (or the wrapper has to change).

**The CLI's push-time validator enforces a strict two-way return-type rule.**
`crates/pg_web_cli/src/push.rs:542-602` — `validate_handler` looks up the handler in `pg_proc` and requires `RETURNS json` when a sibling `.html` exists (template mode) or `RETURNS text` when it doesn't (raw-text mode), with a hard `bail!` on mismatch (`push.rs:585-600`). There is **no third response-shape channel** — the heuristic is binary, derived solely from template presence. The reserved push-managed namespace is `pgweb.pages__*(json) RETURNS json|text` (`push.rs:646-680`, `session_6.md:22`); reconcile only drops handlers matching `RETURNS json|text` (`push.rs:658-660`), so any new return type interacts with reconcile too.

**Livereload injection is already correctly gated.**
`crates/pg_web_ext/src/livereload.rs:209-231` — `inject_script_if_eligible` is a no-op unless env is development (`livereload.rs:210`), the body isn't already marked (`livereload.rs:216`), and the body contains `</body>` (`livereload.rs:221`). So it already only touches full HTML documents in dev. The envelope design must make sure that when a handler sets `Content-Type: application/json` (or any non-HTML type), this injection is **also** skipped — today the gate is purely string-shape-based (`</body>` presence), which happens to be safe but is not content-type-aware.

**Existing apps that must keep working byte-for-byte.**
- `examples/todo/` — `pages/index.{html,sql}` (JSON→Tera), `pages/todos/post.{html,sql}` (JSON→Tera fragment), `pages/todos/toggle/post.sql` + `pages/todos/delete/post.sql` (raw text), `pages/todos/[id]/index.{html,sql}` (dynamic). Both dispatch modes, plus the `_404`.
- `site/` — `pages/index.{html,sql}` plus several static `.html`-only pages. The docs site dogfoods the framework.

Any change is **additive**: every one of these handlers returns a bare `json` or bare `text` value today and must continue to render identically.

## Proposed direction (options)

The three options below are not mutually exclusive — A and B are complementary (wire format + ergonomics), and C is an orthogonal "where does the signal come from" question. Each ends with a lean.

### Option A — response envelope return shape

Let a handler optionally return a JSON object with a reserved marker that the router recognizes as "this is a response envelope, not a body." Absence of the marker = today's behavior exactly.

Two candidate shapes:

- **Namespaced marker:**
  ```json
  { "$pgweb": { "status": 303, "headers": { "Location": "/posts/42" }, "cookies": [ ... ], "content_type": "application/json" }, "body": ... }
  ```
- **Flat reserved keys:**
  ```json
  { "status": 303, "headers": { ... }, "cookies": [ ... ], "body": ... }
  ```

The namespaced form (`$pgweb`) is far safer: a flat `{ "status": ..., "body": ... }` collides with any app whose legitimate JSON payload happens to have top-level `status`/`body` keys (a REST API returning `{"status":"ok"}` is extremely common). The `$pgweb` sentinel is vanishingly unlikely to appear in real data, and `$`-prefixed keys read as clearly framework-reserved.

The envelope must interact cleanly with both dispatch modes:
- **Raw-text mode** (`template_path` NULL): `body` is a string sent verbatim, with the envelope's `content_type` / `headers` / `cookies` / `status` applied. This is the JSON-API and redirect case.
- **Tera mode** (`template_path` non-NULL): the envelope needs a way to say *either* "here is a literal body string, skip Tera" *or* "render the template, and here is the context to render it with, but also apply these headers/status/cookies." The cleanest encoding: if `body` is present it's used literally; if instead a reserved `context` (or `render`) key is present, the router feeds that to Tera and wraps the result with the envelope's status/headers/cookies. A redirect from a template-mode route (e.g. login POST that has an `.html` sibling for the error case but wants to 303 on success) is exactly why this matters.

Detection rule: the router parses the handler text as JSON (it already does this in template mode at `router.rs:201`); if the parsed value is an object containing the `$pgweb` key, treat it as an envelope. Otherwise, fall through to today's path. In raw-text mode the router does *not* currently JSON-parse — so it would need a cheap "does this start with `{` and parse as an object with `$pgweb`" probe before deciding, or the envelope is only recognized when the handler opts in via a helper (see Option B) that the CLI knows about.

**Lean:** Ship the **namespaced `$pgweb` envelope** as the wire format. It is unambiguous, additive (no marker = unchanged behavior), and expressive enough for status + headers + multiple cookies + redirect + explicit content type, in both dispatch modes. The `body`-vs-`context` distinction inside the envelope cleanly resolves the Tera interaction.

### Option B — SQL helper functions in the install SQL

Hand-assembling the reserved JSON shape in every handler is error-prone and ugly. Ship constructor helpers in the extension's install SQL (alongside `pgweb.html_escape` / `pgweb.setting`, `schema.rs:189-224`) so app authors never type `$pgweb` by hand:

- `pgweb.respond(body TEXT, status INT DEFAULT 200, headers JSONB DEFAULT '{}', content_type TEXT DEFAULT NULL) RETURNS json` — the general constructor; builds the envelope.
- `pgweb.redirect(location TEXT, status INT DEFAULT 303) RETURNS json` — sugar for the PRG case; sets `Location` + status, empty body.
- `pgweb.json(payload JSONB, status INT DEFAULT 200) RETURNS json` — serialize `payload`, set `Content-Type: application/json`, wrap in an envelope.
- `pgweb.set_cookie(name TEXT, value TEXT, opts JSONB DEFAULT '{}') RETURNS jsonb` — builds one cookie spec (path/max-age/HttpOnly/SameSite/Secure/expires); composable into `respond`'s cookie list. Could also be designed to *merge into* an existing envelope so a handler can chain `pgweb.set_cookie(... , pgweb.redirect('/'))`.

These keep handler code readable:
```sql
-- login POST: verify, set session cookie, redirect home
SELECT pgweb.redirect('/dashboard')
       || pgweb.set_cookie('pgweb_session', pgweb.session_create(u.id), '{"http_only": true, "same_site": "Lax"}')
FROM users u WHERE ...
```
(exact composition syntax TBD — the point is the author never writes the `$pgweb` literal.)

These also give the CLI a fighting chance at validation: a handler that calls `pgweb.respond`/`redirect`/`json` is declaring "I return an envelope," which `pg-web check` and `validate_handler` can key off (see Detailed design notes).

**Lean:** Ship **both A and B**, exactly as the repo already does for `html_escape` (raw operation) wrapped by ergonomic intent. The envelope (A) is the wire format the router understands; the helpers (B) are the surface app authors touch. This mirrors the established pattern and keeps the reserved JSON shape an implementation detail.

### Option C — new per-route metadata column / response-mode vs deriving from the return value

The alternative to detecting the envelope from the return value is to add a `response_mode` (or `content_type`) column to `pgweb.routes`, populated by the CLI from some new filesystem signal (a frontmatter directive, a `.json.sql` stem, etc.), and have the router consult it.

This runs directly against two explicit, logged project decisions:
- **2026-04-18** — "POST return contract: dispatch via `template_path` nullability… **No new schema column, no per-route flag; filesystem is source of truth.** Alternatives (per-route `skip_template` bool, `pg_proc.prorettype` lookup each request) rejected as either redundant with filesystem state or a per-request performance cost." (`docs/ROADMAP.md:320`)
- **2026-04-20** — "Dynamic route captures derived from pattern, not stored… a denormalized column introduces drift risk… for zero measurable gain." (`docs/ROADMAP.md:324`)

A `response_mode` column is precisely the kind of redundant, drift-prone routes-table metadata both decisions rejected. The return value already carries full information about what the response should be; a column would duplicate that and risk silent disagreement between the route row and the handler body.

**Lean:** **Derive everything from the return value; add no new `pgweb.routes` column.** The envelope is self-describing. This keeps the filesystem (and the handler) the single source of truth, consistent with 2026-04-18 and 2026-04-20. The CLI's job stays "validate the handler's return type matches its declared shape," not "record a response mode."

## Detailed design notes

**1. `_framework_call_handler` must return structured data, or the envelope must survive `::text`.**
The wrapper currently does `SELECT (handler($1))::text` and hands back one `result_text` (`schema.rs:148-149`, `router.rs:539-542`). Two viable paths:
- **(a) Keep the text channel.** A handler returning `json` (Tera mode) or `text` (raw mode) already round-trips through `::text` fine — a `json` envelope cast to `text` is just its serialization, which the router can re-parse. This is the *least invasive* option: the wrapper is unchanged, and the router gains an "is this text an envelope?" probe. The cost is the router re-parsing JSON it may have already parsed.
- **(b) Widen the wrapper.** Change `_framework_call_handler` to also return, say, a `result_json JSON` column (or add OUT columns for status/headers), so the structured response comes back typed instead of re-parsed. Cleaner typing, but it's a schema-level change to the wrapper, touches the reserved-namespace contract, and must stay compatible with both PG 15/16/17 (CLAUDE.md invariant #6).

  **Lean within this note:** start with (a) — keep the wrapper returning text, detect the envelope by parsing in the router. Revisit (b) only if re-parsing shows up in a measured hot path (it won't at Phase 1 scale).

**2. `http.rs` must map the envelope to an Axum `Response`.**
The `ServeOutcome::Response` arm (`http.rs:121-135`) needs to grow from "(status, fixed content-type, body)" to "(status, header map, cookie list, body)". Concretely:
- `ServeOutcome::Response` gains fields — e.g. `Response { status: u16, body: String, content_type: String, headers: Vec<(String,String)>, cookies: Vec<String> }` — or a new `ServeOutcome::RichResponse { ... }` variant so the common path stays cheap. (Lean: extend the existing variant with defaulted fields; one path is simpler than two.)
- Default `content_type` stays `text/html; charset=utf-8` so today's responses are byte-identical.
- Multiple `Set-Cookie` headers: Axum/`http` supports appending multiple header values; build them with `HeaderMap::append`, not `insert`, so two cookies (session + CSRF, per `session_6.md:111`) both go out.
- Status: take it from the envelope when present, else the router's framework status (200/404). `StatusCode::from_u16` already tolerates arbitrary codes (`http.rs:122`).

**3. Content negotiation: honor `Accept`, or explicit-only?**
`http.rs` does not inspect `Accept` today (the only header it reads inbound are `Content-Type` for form detection at `http.rs:73-78` and `If-None-Match` at `http.rs:81-85`). Two designs: (a) inspect `Accept` and let one route serve HTML to browsers and JSON to agents; (b) explicit-only — the handler decides the content type via the envelope, full stop. Negotiation conflicts with the framework's "one handler per (method, path)" simplicity and adds a branch to the hot path for a v1 nobody has asked for in concrete terms yet (005 wants a *clean way to produce JSON*, not auto-negotiation).

**Lean:** **explicit-only for v1.** Simpler, predictable, no `Accept` parsing, no surprise representation switching. Leave negotiation as a documented future extension (it can be layered on later without breaking the explicit path).

**4. Cookie attribute defaults must match the Phase 2 auth spec.**
`pgweb.set_cookie` defaults should align with `session_6.md` open question A1 (`session_6.md:56`): `HttpOnly` and `SameSite=Lax` on by default; **`Secure` only when env=production** (so local-dev HTTP isn't blocked — env is already read per-request via `settings::current_env`, `http.rs:127`,`165`,`203`). The CSRF cookie is the documented exception: `HttpOnly: false` so JS can read it (`session_6.md:111`,`163`), so `set_cookie` must let the caller override `HttpOnly`. Don't bake the session/CSRF specifics into the helper — make the attributes parameters with these defaults.

**5. Header allow/deny policy — handlers must not break the framework.**
A handler must not be able to set hop-by-hop headers (`Connection`, `Transfer-Encoding`, `Keep-Alive`, `Upgrade`, `TE`, `Trailer`, `Proxy-Authenticate`) or override framework-managed safety/correctness headers. Decide: an **allowlist** (only known-safe headers pass) or a **denylist** (everything passes except a blocked set). Allowlist is safer but friction-heavy for legitimate custom headers (`X-*`, `Cache-Control`, `Location`, `Content-Type`, `Content-Disposition`); denylist is friendlier but must be exhaustive. Either way, `Content-Length` and `Content-Type` are framework-computed (the latter from the envelope's `content_type`), and `Set-Cookie` goes through the cookie channel, not the raw header map.

**Lean within this note:** denylist the hop-by-hop set + framework-reserved (`Content-Length`, `Transfer-Encoding`); let everything else through. Document the blocked set. This keeps the common case (set `Cache-Control`, set `X-Foo`) zero-friction.

**6. `pg-web check` and `validate_handler` must evolve.**
The binary `RETURNS json` (template) vs `RETURNS text` (no template) rule (`push.rs:585-600`) gets fuzzier because an envelope is *also* `json`. Cases:
- Raw-text route (`template_path` NULL) that returns an envelope: the handler now `RETURNS json`, not `text` — which today's validator would reject. Either relax raw-text routes to accept `json` (and let the router detect envelope-vs-plain at runtime), or require envelope-returning handlers to be recognizable (e.g. they call `pgweb.respond`/`json`/`redirect`, or use a stem/suffix convention the CLI keys off). Static-analysis of "does this function return an envelope" from `pg_proc` alone is not feasible; the signal has to be the **return type** plus, ideally, a convention.
- Template route that returns an envelope (to set a cookie *and* render): also `RETURNS json` — which already passes, but the validator can't tell the difference between "JSON context for Tera" and "envelope." That's fine if the router handles both at runtime (envelope detected by `$pgweb` marker; otherwise treated as Tera context).

**Lean within this note:** relax `validate_handler` so a raw-text route may declare `RETURNS json` (envelope mode) **or** `RETURNS text` (verbatim mode), and let the router disambiguate at request time via the `$pgweb` marker. Keep the loud error only for the genuinely-wrong cases (e.g. a template route returning `text`). Update `reconcile_handlers` (`push.rs:651-680`) to keep covering `json|text` — it already does.

**7. Livereload injection must remain HTML-only — confirm it stays gated.**
`inject_script_if_eligible` (`livereload.rs:209-231`) already skips non-`</body>` bodies and non-dev envs. When an envelope sets a non-HTML content type, the injection must not run even if the body somehow contains `</body>`. Wire the content type into the eligibility check: only inject when `content_type` starts with `text/html` **and** the body has `</body>` **and** env is dev. This is a small tightening of the existing gate (`http.rs:127-128` is where the env is fetched and the call is made — pass the resolved content type alongside `env`).

**8. Error path stays HTML in dev.**
The dev error page and prod 500 (`render_error`, `http.rs:199-219`) are independent of this work — an envelope only applies to *successful* handler returns. A handler that raises a SQL exception still flows through `ServeError` → `render_error` unchanged. Confirm the dev error page (`err.render_dev_page`, `http.rs:206`) still renders for envelope-returning handlers that throw.

## Migration & backward compatibility

Backward compatibility is **mandatory** — there are live apps (`examples/todo/`, `site/`) whose handlers all return bare `json` or bare `text`.

- **No marker = no change.** A handler returning `{"name":"pg-web"}` (the seeded `hello_handler`, `schema.rs:102-104`) or `'<li>done</li>'` must produce a byte-identical response to today. The router only diverts to envelope handling when it sees the `$pgweb` sentinel.
- **Default content type unchanged.** Absent an envelope `content_type`, the response stays `text/html; charset=utf-8` (`http.rs:131`).
- **Default status unchanged.** Absent an envelope `status`, the framework status (200, or 404 for the fallback) is used (`router.rs:78`,`102`).
- **CLI compatibility (invariant #3).** Any new behavior is detected from the handler return value, not from new metadata, so the extension↔CLI sync surface (`pgweb.routes` / `pgweb.templates` / `pgweb.assets`) does **not** grow a column. If `validate_handler` relaxes raw-text routes to accept `RETURNS json`, that is purely a loosened check, not a new sync field — an older extension paired with a newer CLI (and vice versa) still agree on the table shapes.
- **Helpers are additive install SQL.** `pgweb.respond` / `redirect` / `json` / `set_cookie` are new `CREATE FUNCTION`s in the bootstrap block (`schema.rs:13`, the `framework_tables` extension_sql) — additive, like `html_escape`/`setting` were. They version with the extension; a `CREATE EXTENSION ... UPDATE` path may be needed if the project ships extension upgrades (check whether one exists).
- **Phase discipline.** This is plumbing that *enables* Phase 2, but the response contract itself is arguably Phase-1-completing infrastructure (it makes the documented "JSON APIs" claim true). Land it as its own change with no auth/session logic riding along — the cookie *mechanism*, not cookie *sessions*.

## Research tasks for the implementing session

Read-only first; understand the full pipeline before touching anything.

1. **Request/response lifecycle (start here):**
   - `crates/pg_web_ext/src/http.rs` — entire `handle` (`:69-143`), the `ServeOutcome::Response` arm (`:121-135`), `render_asset` (`:158-194`), `render_error` (`:199-219`), `status_plain` (`:221-228`). Note every place a header or status is set.
   - `crates/pg_web_ext/src/router.rs` — `ServeOutcome` (`:45-60`), `render_route` (`:189-221`), `serve`/`serve_in_tx` (`:67-108`), `call_handler` (`:505-578`). Trace how `status` is chosen and where `handler_text` comes from.
   - `crates/pg_web_ext/src/schema.rs` — `_framework_call_handler` (`:131-163`), the existing helper functions `html_escape` (`:189-203`) and `setting` (`:218-224`) as the pattern to mirror.
   - `crates/pg_web_ext/src/livereload.rs` — `inject_script_if_eligible` (`:209-231`).
   - `crates/pg_web_ext/src/errors.rs` — how `ServeError` renders (the dev page is HTML-biased; confirm it's untouched by this work).
2. **CLI side:**
   - `crates/pg_web_cli/src/push.rs` — `validate_handler` (`:542-602`), `reconcile_handlers` (`:651-680`), `apply_entry` (`:474-536`).
   - `crates/pg_web_cli/src/paths.rs` — `RouteEntry`, `scan`, how the three file-combinations are derived (referenced throughout `push.rs`).
   - `crates/pg_web_cli/src/check.rs` — whether the offline validator needs to learn the new shapes.
3. **Specs & decisions:**
   - `docs/internal/sessions/session_6.md` — Track A cookie/login (`:33-62`), CSRF (`:108-121`), A1 cookie attrs (`:56`). This is the primary consumer; design to its needs.
   - `docs/APP-LAYOUT.md` — the pipeline table (`:13-19`), the "JSON API" claims (`:218`), handler contract (`:141-210`). These docs will need updating.
   - `prompts/005_json_api_and_mcp_support.md` — the JSON/MCP design that sits *on top* of this layer.
   - `docs/ROADMAP.md` — decision log `2026-04-18` (`:318-322`) and `2026-04-20` (`:324-325`), the "no redundant column / filesystem is source of truth" rulings.
   - `CLAUDE.md` — invariants #3, #4, #6, #7.
4. **Exercise the current behavior** against a running `pg-web up` stack: a `RETURNS text` handler emitting a JSON string, observe `Content-Type: text/html` in `curl -i`. Then prototype the envelope and confirm a 303 + `Location`, an `application/json` body, and a `Set-Cookie` come out correctly.
5. **External:** confirm Axum/`http` multiple-`Set-Cookie` semantics (`HeaderMap::append`) and that `StatusCode::from_u16` covers the codes you intend to allow (303, 201, 204, 4xx).

## Constraints & invariants to respect

From `CLAUDE.md` "Architectural invariants — DO NOT VIOLATE":

- **#4 — One HTTP request = one SPI transaction.** The envelope is produced *inside* the handler call, which is inside `BackgroundWorker::transaction` (`router.rs:70`). Building headers/cookies must not open a second transaction or a new SPI connection. Cookie *signing* (Phase 2) happens in SQL inside the same tx.
- **#3 — Extension ↔ CLI strictly decoupled.** No shared library beyond dumb types; synchronize only via framework-owned tables. This work adds **no** new sync column — the response shape is carried in the handler return value, not in `pgweb.routes`. (This is also why Option C is rejected.)
- **#7 — Async only in the BGW.** Header/cookie construction is synchronous SQL + synchronous Rust mapping in `http.rs`. No `tokio` inside `#[pg_extern]`/handler paths.
- **#2 / HTTPS out-of-process.** The `Secure` cookie attribute is set by the framework based on env, but TLS itself is Caddy's job — don't infer "is this HTTPS" from a connection the extension terminated (it never terminates TLS).
- **#6 — PG 15/16/17 only.** Any new install-SQL function (`respond`/`redirect`/`json`/`set_cookie`) and any `_framework_call_handler` change must work on all three. No pg18-only features.
- **Additive / byte-for-byte.** Existing `examples/todo` and `site` handlers must render identically. This is the acceptance gate, not a nice-to-have.
- **Companion-app coverage.** Per CLAUDE.md "every feature ships with a companion-app flow" — the feature isn't done until `examples/todo/` exercises it (`CLAUDE.md:50`).

## Acceptance criteria

1. `examples/todo/` and `site/` render **byte-for-byte identically** before and after the change — every existing bare-`json` and bare-`text` handler is unaffected (verified by the tier-3 smoke and a before/after `curl -i` diff on representative routes).
2. A handler can return a **303 redirect** with a `Location` header (via `pgweb.redirect('/path')`), and a non-HTMX browser form POST following it lands on the target — the Post-Redirect-Get flow works.
3. A handler can set **`Content-Type: application/json`** and an arbitrary custom header (e.g. `Cache-Control`, `X-Request-Id`) on a `.sql`-only route, and `curl -i` shows them — the documented "JSON APIs" use case (`APP-LAYOUT.md:218`) is now true.
4. A handler can **emit `Set-Cookie`**, including **two cookies in one response** (session + CSRF shape), both appearing as separate `Set-Cookie` headers.
5. Cookie attribute defaults match the Phase 2 spec: `HttpOnly` + `SameSite=Lax` by default, `Secure` only when `env=production` (`session_6.md:56`), with `HttpOnly` overridable for the CSRF-cookie case.
6. The router **detects the envelope only via the `$pgweb` marker**; a handler returning ordinary JSON with no marker is treated exactly as today (no false-positive envelope handling), proven by a test returning `{"status":"ok","body":"x"}` as plain data.
7. A **template-mode route** can return an envelope that both renders its Tera template *and* sets a header/cookie/status (the login-error-page-renders-but-also-sets-cookie case).
8. **Hop-by-hop / framework-reserved headers are rejected or ignored** when a handler tries to set them (e.g. `Transfer-Encoding`, `Content-Length`), with a clear error or documented silent drop.
9. **`pg-web check` and `pg-web push` understand the new shapes** — a raw-text route whose handler `RETURNS json` (envelope mode) validates instead of failing the old binary check; genuinely-wrong return types still error loudly.
10. **Livereload injection does not run on non-HTML responses** even in dev; the **dev error page still renders** for an envelope-returning handler that raises a SQL exception.
11. **Companion-app coverage:** `examples/todo/` gains at least one flow exercising the new contract (e.g. a JSON endpoint and/or a redirect-after-POST), per CLAUDE.md. New `#[pg_test]` coverage for envelope construction/detection and for each helper function.

## Open questions

1. **Marker shape and namespace.** Is `$pgweb` the right sentinel key, or should it be something even less collision-prone (e.g. `$pgweb$` / a UUID-ish constant)? And inside the envelope, is `body` vs `context`/`render` the right way to distinguish "literal body" from "render this template with this context"?
2. **Wrapper: keep `::text` or widen `_framework_call_handler`?** Re-parse the envelope from `result_text` in the router (zero wrapper change), or add a typed `result_json` / OUT columns to the wrapper (cleaner, but a reserved-namespace schema change)? Performance at Phase 1 scale almost certainly favors the former — confirm.
3. **Header policy: allowlist or denylist?** And what is the exact blocked set? Where exactly does enforcement live — in `http.rs` when mapping to the Axum response, or earlier?
4. **`validate_handler` relaxation.** Should a raw-text route be allowed to declare `RETURNS json` (envelope) freely, with runtime disambiguation — or should envelope-returning handlers be made statically recognizable (helper-call detection, or a stem/suffix convention) so `pg-web check` can be precise? How does this interact with the reserved `pgweb.pages__*(json) RETURNS json|text` namespace and `reconcile_handlers`?
5. **Cookie helper composition.** What's the ergonomic shape for combining `set_cookie` with `redirect`/`respond` in SQL — JSONB concatenation (`||`), a variadic `respond(..., VARIADIC cookies)`, or `set_cookie` taking-and-returning an envelope so calls chain? Pick the one that reads cleanly in a `LANGUAGE sql` one-liner.
6. **Content negotiation.** Confirm explicit-only for v1 (no `Accept` parsing). If negotiation is ever wanted, does the envelope design leave room to add it without breaking the explicit path?
7. **Status-code allowlist.** Should the framework restrict which status codes a handler may set (e.g. block 1xx and bare 101/Upgrade), or accept any `u16` that `StatusCode::from_u16` allows? Are there codes (304, 101) that conflict with framework behavior (asset 304s, no WebSocket upgrade in Phase 1)?
8. **Extension upgrade path.** Do the new install-SQL helpers require a `CREATE EXTENSION ... UPDATE` migration script for existing installs, or is the project still install-from-scratch only at this stage? Confirm whether an extension-version-upgrade mechanism exists before assuming the helpers ship "for free."
