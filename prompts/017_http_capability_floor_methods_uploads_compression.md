# 017 — HTTP capability floor: full method set, file uploads, compression & range, configurable limits

**Status:** Open work order — closes the "demo app → real app" capability gap
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from external codebase analysis, 2026-06-11)
**Prerequisites:** 013 (response contract) helps for upload responses; large-file uploads relate to the deferred pg_largeobject streaming work
**Context:** pg-web v0.2.0 closes Phase 1 with a clean HTML/HTMX serving path, but the HTTP layer only speaks GET and POST, never parses a file upload, and ships every asset as an uncompressed, un-seekable BYTEA blob from memory. Those three holes are exactly what separates the todo demo from a real CRUD app (deletes, avatars, attachments, media), and the limits that gate them are hardcoded constants rather than configuration. This work order raises the HTTP capability floor in three staged sub-items plus a configuration sub-item, each filename-derived so the directory-as-route invariant holds.

---

## Summary

Three concrete capabilities are missing from the request/response pipeline, and one cross-cutting limitation makes two of them awkward:

- **A — Method set.** Only `index` (GET) and `post` (POST) stems are allowed; `put`/`patch`/`delete`/`head`/`options` are explicitly bailed at push time as "Phase 2+." HTMX's native `hx-delete`/`hx-put`/`hx-patch` are therefore unusable, and the demo routes deletion through a `POST /todos/delete/` workaround.
- **B — File uploads.** The body parser only understands `application/x-www-form-urlencoded`; `multipart/form-data` yields an empty `body` object. There is no story for avatars, attachments, or blog images.
- **C — Compression & range.** Assets are served whole from memory with ETag/Cache-Control but no `Content-Encoding` and no `Range`/`Accept-Ranges`, so large text assets aren't compressed and media can't be seeked.
- **D — Limits.** The 2 MiB body cap and 20 MiB asset cap are hardcoded constants (plus a schema CHECK). Upload routes need higher caps; nothing is configurable.

This prompt asks a future agent to deliver a **staged plan** — methods first (cheap, high-value), then uploads (the big one), then compression/range (polish) — each sub-item naming the touched files, the contract changes, and the companion-app coverage that proves it. **Lean:** ship A and D together (D is a small prerequisite for B), then B, then C; do not let C block A or B.

The non-negotiable framing: every new method stays **filename-derived** (no second routing mechanism), and uploads must respect the one-request-one-SPI-transaction boundary. Where those collide with "stream a 50 MiB file into Postgres," the design has to say so explicitly.

## Why this matters now

The directory-as-route model is the framework's whole value proposition, and it's already proven for GET/POST. The cost of *not* having the rest of the method set is paid directly by app authors: a delete is the single most common mutation in a CRUD UI, and today it can only be expressed as a POST to a contrived sub-path (`pages/todos/delete/post.sql` — see `examples/todo/pages/todos/delete/post.sql:9`), with HTMX wired as `hx-post` instead of the idiomatic `hx-delete`. That's a paper cut on the framework's most-walked path.

Uploads are table stakes for the "real app" story. The moment someone builds a blog, a profile page, or any attachment flow, they hit a wall: the body parser drops the multipart payload on the floor and the handler sees `{}`. There is no documented escape hatch. This is also the **enabling feature for image dogfooding** in the site-v2 work (prompt 020) — the docs site can't dogfood image handling until uploads exist.

Compression and range are lower-urgency but they're where "serve a 4 MB hero PNG" and "let a browser scrub a screencast" live. They also interact with the single-threaded worker (CPU cost of per-request compression — cross-ref prompt 015) and with the Caddy layer in front (which may already compress — avoid double-compression). Getting the design recorded now, even if implementation lands last, keeps the asset path coherent.

All four are squarely Phase-2-shaped HTTP-surface work, but they are *capability floor*, not new subsystems: they extend the existing dispatch and asset paths rather than introducing auth, jobs, or a dashboard.

## The gaps (evidence)

Read these before designing anything. Line numbers are against git `918f40b`.

**Method set is hardcoded to GET/POST.** `crates/pg_web_cli/src/paths.rs:141-170` (`validate_stem`) accepts only `index`, `post`, and root-level `_404`; `crates/pg_web_cli/src/paths.rs:159-162` explicitly bails `put`/`patch`/`delete`/`head`/`options` with "reserved for Phase 2+." `method_for_stem` (`paths.rs:172-179`) only maps `index`→GET, `post`→POST, `_404`→404. The docs codify this: `docs/APP-LAYOUT.md:25-34` lists `put`/`patch`/`delete`/`head`/`options` as "Reserved — rejected until Phase 2+." There are `#[test]` assertions locking the rejection in: `paths.rs:559-564` (`validate_stem_rejects_future_methods`) and `paths.rs:679-685` (`scan_rejects_reserved_stem_nested`).

The router does not special-case any method — it fetches `pgweb.routes WHERE method = <method>` (`crates/pg_web_ext/src/router.rs:382-387`) and matches by pattern. So the *extension* would dispatch a DELETE today if a row existed; the gate is entirely CLI-side (`validate_stem`) plus the absence of a way to author the handler. Note `serve_in_tx` only falls through to static assets `if method == "GET"` (`router.rs:85`), and the 404 fallback is keyed on `method='404'` (`router.rs:470-486`). **HEAD currently has no handling at all** — `HEAD /` finds no `method='HEAD'` route, is not GET so skips the asset branch, and falls through to the 404 fallback. That's wrong: HEAD should mirror GET without a body.

The demo's delete workaround: `examples/todo/pages/todos/delete/post.sql:9` defines `pgweb.pages__todos__delete__post(req json) RETURNS text` reached via `POST /todos/delete`, with the file comment explaining the HTMX `hx-swap="outerHTML"` + empty-body collapse trick. `docs/APP-LAYOUT.md:74-75` documents this layout as the canonical delete pattern.

**No multipart parsing.** `crates/pg_web_ext/src/http.rs:32` sets `const MAX_BODY_BYTES: usize = 2 * 1024 * 1024` (the comment literally says "or file upload (not supported in Phase 1)"). The body branch at `http.rs:73-78` only flags `application/x-www-form-urlencoded`; `http.rs:98-102` parses that into `body_obj` and otherwise produces `Map::new()` — an empty object. `parse_urlencoded` (`http.rs:258-269`) is the only body decoder. The handler `req` shape (`http.rs:109-115`) is `{ body, query, method, path, path_params }`; there is no field for files. The `req` contract is documented at `docs/APP-LAYOUT.md:156-166`.

**No compression, no range.** `render_asset` (`crates/pg_web_ext/src/http.rs:158-194`) emits `Content-Type`, `ETag`, and `Cache-Control` only — no `Content-Encoding`, no `Accept-Ranges`, no `Range` handling. It always returns `200` with the full body (`http.rs:184-193`) or `304` (`http.rs:173-182`). The asset row is read whole into a `Vec<u8>` in `lookup_asset` (`crates/pg_web_ext/src/router.rs:119-164`) and carried through `ServeOutcome::Asset { body, content_type, etag }` (`router.rs:51-56`). Dynamic `ServeOutcome::Response` bodies are likewise un-compressed — the HTML branch hardcodes `Content-Type: text/html; charset=utf-8` (`http.rs:129-134`) with no encoding negotiation.

**Limits are constants.** Body cap: `http.rs:32` (`MAX_BODY_BYTES = 2 MiB`), consumed at `http.rs:87` (`to_bytes(req.into_body(), MAX_BODY_BYTES)`); over-cap returns a 400 (`http.rs:88-96`). Asset cap: schema CHECK `CHECK (length(content) <= 20971520)` at `crates/pg_web_ext/src/schema.rs:95`, mirrored by the CLI const `MAX_ASSET_BYTES: u64 = 20 * 1024 * 1024` at `crates/pg_web_cli/src/push.rs:108` and enforced in `scan_public` at `push.rs:728-737`. Neither is read from `pgweb.toml` or `pgweb.settings`. The `[server]` section of `pgweb.toml` is parsed at `push.rs:47-53` and only carries `env`. `pgweb.settings` is a key/value table (`schema.rs:72-77`) already used for `env`, with a `pgweb.setting(text)` reader (`schema.rs:218-221`) — the natural home for runtime-tunable limits the extension can read per-request.

**Deferred streaming context.** True `pg_largeobject` streaming for assets >20 MiB is documented as Phase 2+: `docs/OVERVIEW.md:70` and `docs/OVERVIEW.md:202`, `docs/ROADMAP.md:50`, and the planned-but-unbuilt `pgweb.assets_large` table at `docs/ARCHITECTURE.md:135` + `docs/ARCHITECTURE.md:148` (`lo_open`/`lo_read` streaming). The 20 MiB BYTEA cap-raise (Component I) was shipped as a stopgap *instead of* streaming. Sub-items B and C both want this work to exist, so they should be designed to dovetail with it rather than around it.

---

## Sub-item A: full HTTP method set

Enable `put`/`patch`/`delete` as filename stems (and auto-handle `head`/`options`), keeping every method filename-derived so the directory-as-route invariant holds — **no second routing mechanism**.

**What changes:**

- `crates/pg_web_cli/src/paths.rs` — `validate_stem` (`:141-170`) accepts `put`/`patch`/`delete`; `method_for_stem` (`:172-179`) maps them to `PUT`/`PATCH`/`DELETE`. Decide whether `head`/`options` are *also* authorable stems or purely auto-derived (see below). Update the `#[test]`s at `:559-564` and `:679-685` (they currently assert rejection) and add positive cases.
- `crates/pg_web_cli/src/push.rs` — no signature logic is method-specific today (`validate_handler` at `:542-602` keys off `template_path` nullability, not method), so this is mostly "does the new method round-trip through `apply_entry`/reconcile." Confirm the `(method, path_pattern)` primary key (`schema.rs:22`) cleanly accommodates multiple methods per directory. Add reconcile coverage.
- `crates/pg_web_ext/src/router.rs` — the SQL dispatch already matches arbitrary methods (`:382-387`). The two method-specific spots to revisit: the static-asset fallback is GET-only (`:85`) — confirm that's still correct (it is; assets are GET/HEAD), and decide HEAD/OPTIONS handling here vs in `http.rs`.
- `crates/pg_web_ext/src/http.rs` — **HEAD** should resolve the GET route and emit identical headers with an empty body (today `HEAD /` falls to the 404 fallback — `router.rs:470`). **OPTIONS** could auto-respond `204` with an `Allow:` header computed from which methods have rows for that path. Both are cheap and make the surface behave like a real HTTP server.
- `docs/APP-LAYOUT.md` — update the method table (`:25-34`) from "Reserved" to supported; add `delete.sql` to the worked HTMX example (`:64-76`); document HEAD/OPTIONS auto-behavior.

**Lean:** make `put`/`patch`/`delete` first-class authorable stems; make `head`/`options` **auto-handled by the extension**, not authorable files (a `head.sql` stem would invite divergence from its GET twin — keep HEAD a guaranteed mirror of GET). If a future app genuinely needs a custom OPTIONS body, that's a later, separate decision.

**Companion-app coverage:** convert the todo delete from `POST /todos/delete/` to a real `pages/todos/[id]/delete.sql` serving `DELETE /todos/:id`, and switch the list UI's delete control from `hx-post` to `hx-delete`. This both exercises the new method *and* deletes the workaround the framework currently ships as canonical (`docs/APP-LAYOUT.md:74-75`).

## Sub-item B: file uploads (multipart)

Parse `multipart/form-data` and give handlers access to uploaded files. The hard constraint: **files can be large, so passing raw bytes inside the `req json` argument is wrong** — it would balloon the handler payload, defeat the point of streaming, and stress the SPI text round-trip (`call_handler` serializes `req` to a string and binds it as a JSON literal — `router.rs:511-520`).

**Design options (compare, then recommend):**

1. **Stream to storage, hand the handler a descriptor.** Parse multipart in `http.rs`, stream each file part into asset/large-object storage, and inject a *reference* into `req` — e.g. `req.files = [{ field, filename, content_type, size, storage_key }]` — rather than bytes. The handler then links the descriptor to a domain row. This is the only option that scales past a few KB and the only one that composes with the deferred `pg_largeobject` streaming (`docs/ARCHITECTURE.md:135,148`).
2. **Dedicated upload endpoint + staging table.** A framework-owned `POST /_pgweb/upload`-style route writes to a new `pgweb.uploads` staging table and returns a token; the app's real handler claims the token. More moving parts, clearer transaction story, but introduces a second request round-trip.
3. **Size-gated inline base64.** For *tiny* files only (a few KB, under a configurable threshold), base64 the bytes straight into `req.files[].data`. Simple, but a foot-gun above trivial sizes; at best a complement to (1) for things like favicons.

**Lean:** option (1) — stream to storage, pass the handler a descriptor — and explicitly connect it to the deferred true `pg_largeobject` streaming work so uploads and large-asset serving share one storage substrate (`pgweb.assets` BYTEA for small, `pg_largeobject` for large; the descriptor abstracts which). Keep `req.files` an array that is *always present* (empty when no multipart), mirroring the "`body`/`query`/`path_params` are never null" contract (`docs/APP-LAYOUT.md:166`).

**What changes:**

- `crates/pg_web_ext/src/http.rs` — detect `multipart/form-data` alongside the existing urlencoded branch (`:73-78`); parse parts (non-file fields fold into `body` as today, file parts stream to storage); add a `files` key to the `req` shape (`:109-115`). The body-size cap interacts directly with D — multipart routes need a higher ceiling.
- Storage — extend `pgweb.assets` or introduce the `pgweb.assets_large`/`pgweb.uploads` table (`docs/ARCHITECTURE.md:135`). **Transaction boundary (invariant #4):** the file write and the handler's domain write must be in the *same* SPI transaction so a rolled-back request leaves no orphaned blob. `pg_largeobject` writes via `lo_*` are transactional, which helps — but the design must spell out exactly where the multipart parse (in async Axum land) hands off to the synchronous SPI transaction (`router.rs:67-71`). This is the subtle part: the parse happens *before* `router::serve`, so streaming-to-DB-during-parse and one-request-one-transaction are in tension — resolve it deliberately.
- `crates/pg_web_cli/src/paths.rs` / `push.rs` — no new file convention is strictly required (uploads target existing POST/PUT routes), but consider whether a re-serve route for stored uploads needs anything. Likely not.
- `docs/APP-LAYOUT.md` / `docs/APP-DEVELOPER-GUIDE.md` — document the `req.files` shape and a worked "store an avatar, link it to a user row, re-serve it" example.

**Companion-app coverage:** the natural home is the **site-v2 blog (prompt 020)** — upload an image, store it, re-serve it on a post. If site-v2 isn't ready when B lands, add a minimal avatar/attachment flow to `examples/todo/` so the feature is exercised per CLAUDE.md ("if a feature isn't exercised in `examples/todo/`, it isn't done" — `CLAUDE.md:50`).

## Sub-item C: compression & range requests

Add response compression for text-y content and `Range` support for assets, so large assets ship small and media can be seeked. Today `render_asset` (`http.rs:158-194`) does neither.

**What changes:**

- **Compression.** Either `tower-http`'s `CompressionLayer` over the Axum router (`http.rs:51-67` builds the `Router`) or manual `Content-Encoding` in `render_asset` and the HTML `Response` branch (`http.rs:121-135`). Negotiate via `Accept-Encoding`; gzip is the safe floor, brotli the better-ratio option. Decide **pre-compressed-at-push vs per-request:** storing gzip/brotli variants at `pg-web push` time (a column or sibling rows in `pgweb.assets`) trades storage for zero per-request CPU — attractive given the **single-threaded worker** (cross-ref prompt 015, CPU on the one thread). Per-request compression is simpler but taxes the hot path. **Lean:** pre-compress text assets at push time (CSS/JS/SVG/HTML), serve the stored variant when the client accepts it, fall back to identity; leave dynamic HTML responses to per-request compression *only if* measured worthwhile.
- **Range.** Add `Accept-Ranges: bytes` and honor `Range:` in `render_asset`, returning `206 Partial Content` with `Content-Range` for the requested span (and `416` for unsatisfiable ranges). This matters most for media (video/PDF/audio) and is **most valuable once large-object streaming lands** — a `Range` request against an `lo_*`-backed asset can `lo_read` just the requested window instead of loading the whole BYTEA (`router.rs:134` reads the entire `content` column today). For BYTEA assets, slicing the in-memory `Vec<u8>` is trivial but doesn't save the read; note that and design Range to compose with streaming.
- **Caddy interplay (invariant #2).** HTTPS terminates in Caddy out-of-process (`CLAUDE.md:37`), and Caddy can compress. The extension serves plain HTTP behind it. **Avoid double-compression:** decide whether the extension compresses at all when behind Caddy, or whether it sets `Content-Encoding` and Caddy is configured to pass through. Document the recommended `Caddyfile` posture in `docs/DEPLOYMENT.md`. Range likewise: confirm Caddy forwards `Range`/`206` cleanly.

**Companion-app coverage:** serve a compressed CSS/JS asset in the demo (assert `Content-Encoding` in a smoke test) and a `Range`-seekable media asset (a small sample video/PDF) — site-v2 (020) is again the natural showcase for the media side.

## Sub-item D: configurable & per-route limits

Make body limits configurable instead of the hardcoded `MAX_BODY_BYTES` (`http.rs:32`), with upload routes able to opt into higher caps. This is a small but real prerequisite for B (a 2 MiB body cap makes file upload pointless).

**What changes:**

- **Global default.** Add a `[server]` knob in `pgweb.toml` (e.g. `max_body_bytes`), parsed in `push.rs:47-53` alongside `env`, synced into `pgweb.settings` (`schema.rs:72-77`) the way `env` already is (`push.rs:459-467`). The extension reads it per-request via `pgweb.setting()` (`schema.rs:218-221`) — but note `to_bytes` needs the limit *before* the handler runs, so it's read in `http.rs:87`, not inside the handler. Caching the setting (it changes rarely) avoids an SPI hit per request; weigh against the cross-cutting cost.
- **Per-route caps.** Upload routes need a higher ceiling than form routes. Options: a column on `pgweb.routes`, a `pgweb.toml` per-path override, or a convention. Keep it filename/table-derived to honor invariant #3 (extension↔CLI sync only through framework tables). **Lean:** start with a configurable *global* default plus a single elevated cap for routes flagged as upload-capable; full per-route arbitrary limits can wait until a real app needs them.
- **Asset cap.** The 20 MiB asset cap lives in two places that must stay in lockstep — the schema CHECK (`schema.rs:95`) and the CLI const (`push.rs:108`). Making it configurable means the CHECK can't be a static literal; consider whether the asset cap should remain fixed (it's a storage-shape decision tied to BYTEA/TOAST) while only the *body* cap becomes tunable. **Lean:** make the body cap configurable now; leave the asset cap fixed until `pg_largeobject` streaming removes the ceiling entirely.

**Companion-app coverage:** the upload flow from B exercises an elevated body cap; add a `pgweb.toml` setting in `examples/todo/` (or site-v2) and assert a body just over the default is rejected, just under is accepted.

## Research tasks

Read-only, before any code. Be exhaustive; cite `file:line`.

1. **Method dispatch end-to-end.** Confirm the extension's method-agnostic dispatch (`router.rs:382-387`, `:425-465`) and the exact CLI gates (`paths.rs:141-179`). Enumerate every test that asserts the current GET/POST-only behavior so you know what flips (`paths.rs:559-564`, `:679-685`; the `scan_todo_app_layout` expectation at `:635-665`). Verify nothing downstream assumes method ∈ {GET, POST}.
2. **HEAD/OPTIONS today.** Trace `HEAD /` and `OPTIONS /` through `serve_in_tx` (`router.rs:73-108`) and confirm they hit the 404 fallback. Decide the cleanest injection point for auto-HEAD (mirror GET, strip body) and auto-OPTIONS (`Allow:` from `SELECT DISTINCT method FROM pgweb.routes WHERE path_pattern = $1`).
3. **Multipart in the Axum/SPI seam.** Study where the async body read (`http.rs:87`) sits relative to the synchronous SPI transaction (`router.rs:67-71`, invariant #4 and #7 — async only in the worker, never inside `#[pg_extern]`). Determine how to stream a multipart part into `pg_largeobject` while keeping the file write and the handler's domain write in one transaction. This is the crux of B.
4. **Storage substrate.** Read the `pgweb.assets` schema (`schema.rs:90-96`), the planned `pgweb.assets_large` (`docs/ARCHITECTURE.md:135,148`), and the push-side asset path (`push.rs:682-766`, `:832-844`). Decide whether uploads reuse `pgweb.assets`, introduce `pgweb.assets_large`, or add `pgweb.uploads`. Confirm `lo_*` availability under SPI on PG 15/16/17 (invariant #6).
5. **Compression placement & cost.** Compare `tower-http::CompressionLayer` vs manual encoding in `render_asset`. Quantify (or at least reason about) per-request CPU on the single worker thread (cross-ref prompt 015) vs push-time pre-compression storage cost. Inventory which content types are worth compressing.
6. **Caddy posture.** Read `docs/DEPLOYMENT.md` for the current Caddy config and determine the double-compression and Range-forwarding story (invariant #2). Produce the recommended `Caddyfile` directives.
7. **Settings plumbing.** Trace how `env` flows from `pgweb.toml` → `push.rs:459-467` → `pgweb.settings` → `pgweb.setting()` (`schema.rs:218-221`) → `settings::current_env` reads in `http.rs`. That's the template for any new configurable limit. Decide on per-request caching to avoid an SPI hit in the hot path.
8. **Companion-app + prompt 020 fit.** Read `examples/todo/` and the site-v2 plan (prompt 020) to decide where each sub-item's coverage lands. Confirm the delete-method conversion removes, not duplicates, the `POST /todos/delete` workaround.

## Constraints & invariants to respect

- **#3 — Extension ↔ CLI decoupled.** New methods synchronize *only* through `pgweb.routes` (the CLI derives method from the filename and upserts a row; the extension reads it). No shared routing config, no second mechanism. Upload storage tables are framework-owned and written by the CLI / read by the extension — same discipline.
- **#4 — One request = one SPI transaction.** Streaming a large upload into the DB must commit or roll back atomically with the handler's own writes. A multi-MiB `lo_write` followed by a failing handler must leave **no orphaned large object**. The async multipart parse sitting *before* `router::serve` (`http.rs:87` vs `router.rs:67`) is in direct tension with this — the design must resolve exactly where bytes cross into the transaction.
- **#2 — HTTPS out-of-process.** Range and compression concern the plain-HTTP payload the extension emits on :8080; Caddy terminates TLS and may also compress. Avoid double-compression and confirm Caddy forwards `Range`/`206`. Never pull TLS into the extension.
- **#7 — Async only in the background worker.** Multipart streaming uses async Axum machinery; the handler call is synchronous SPI. Don't blur the boundary — no `tokio` inside the SPI-call path.
- **#6 — PG 15/16/17 only.** `lo_*` large-object functions, any new SQL, and the multipart path must work on all three. No pg18-only deps.
- **Directory-as-route holds.** Methods stay filename-derived (`put.sql`/`patch.sql`/`delete.sql`). HEAD/OPTIONS are auto-handled, not new file types. No route table beyond `pgweb.routes`.
- **Phase discipline.** This is capability-floor HTTP work, not a new subsystem — it must not smuggle in auth/RLS, jobs, or a dashboard. Keep the changes inside the existing dispatch and asset paths.

## Acceptance criteria

- [ ] A handler authored as `pages/todos/[id]/delete.sql` serves `DELETE /todos/:id`, and the demo's delete control uses `hx-delete` (the `POST /todos/delete` workaround is removed, not duplicated).
- [ ] `put.sql` and `patch.sql` are accepted by `pg-web push` and dispatch correctly; the `validate_stem` tests assert acceptance, not rejection.
- [ ] `HEAD /<route>` returns the same status and headers as `GET /<route>` with an empty body; `OPTIONS /<route>` returns an `Allow:` header listing the methods defined for that path.
- [ ] A `multipart/form-data` upload of an image is parsed, streamed to storage (not passed as bytes in `req`), exposed to the handler as a `req.files` descriptor, linked to a domain row, and re-served — all within one SPI transaction (a forced handler error leaves no orphaned blob).
- [ ] Large text assets (CSS/JS) are served with `Content-Encoding` (gzip and/or brotli) when the client sends `Accept-Encoding`, with a smoke test asserting the header and a correct decompressed body.
- [ ] A `Range:` request against a media asset returns `206 Partial Content` with a correct `Content-Range`; an unsatisfiable range returns `416`; `Accept-Ranges: bytes` is advertised.
- [ ] The request body limit is configurable via `pgweb.toml` and synced through `pgweb.settings`; a body just over the limit is rejected and just under is accepted; upload-capable routes can use an elevated cap.
- [ ] `docs/APP-LAYOUT.md` reflects the full method set (table + worked example), the `req.files` shape is documented in `docs/APP-LAYOUT.md`/`docs/APP-DEVELOPER-GUIDE.md`, and `docs/DEPLOYMENT.md` documents the Caddy compression/range posture.
- [ ] Every sub-item is exercised by `examples/todo/` and/or the site-v2 blog (prompt 020), per `CLAUDE.md:50`.
- [ ] `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and the relevant `cargo pgrx test pgXX` pass; the double-source asset/body caps stay in lockstep where they remain constant.

## Open questions

1. **Multipart parser crate.** `axum::extract::Multipart` (built on `multer`) vs `multer` directly vs a streaming parser — which gives us bounded-memory, per-part streaming into `lo_write` without buffering the whole part? Does the chosen crate's API let us cross into the SPI transaction at the right moment?
2. **Upload storage model.** Reuse `pgweb.assets` (BYTEA, 20 MiB cap), build the planned `pgweb.assets_large` (`pg_largeobject`), or add a `pgweb.uploads` staging table? Should this *be* the trigger to finally implement the deferred large-object streaming, so uploads and >20 MiB asset serving share one substrate?
3. **Uploaded-file metadata.** Where do filename, content-type, size, checksum, and the link back to the owning domain row live — a generic `pgweb.uploads` table the app references by key, or app-owned columns the handler populates from the descriptor? How much does the framework own vs leave to the app?
4. **Per-route vs global limits — config shape.** A column on `pgweb.routes`, a `pgweb.toml` per-path map, or a filename/convention flag for upload-capable routes? What honors invariant #3 most cleanly while staying ergonomic?
5. **Compression: pre-compute vs per-request.** Pre-compress text assets at push time (storage cost, zero hot-path CPU) or compress per-request on the single worker thread (CPU cost, cross-ref prompt 015)? Brotli, gzip, or both — and what's the `Accept-Encoding` fallback order?
6. **Caddy interplay.** Does the extension compress at all behind Caddy, or set `Content-Encoding` and rely on Caddy pass-through? How do we prevent double-compression and confirm `Range`/`206` survive the proxy hop?
7. **HEAD/OPTIONS scope.** Is auto-HEAD-mirrors-GET (no body) and auto-OPTIONS-with-`Allow` sufficient, or will any real app need an authorable OPTIONS body — and if so, how without inviting a `head.sql`/`options.sql` stem that diverges from its GET twin?
8. **Transaction boundary for big uploads.** A 50 MiB upload streamed into `pg_largeobject` inside one SPI transaction holds a transaction open for the duration — what are the lock/bloat/timeout implications on the single worker, and is there a size beyond which we must reject rather than stream (tying back to D's caps)?

---

After writing, reply with a 1-2 sentence summary.
