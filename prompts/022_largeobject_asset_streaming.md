# 022 — Large-asset streaming via `pg_largeobject` (true support for assets > 20 MiB)

**Status:** Open work order — capability gap; the deferred half of Session 5 Component I
**Date opened:** 2026-06-11
**Author:** Handoff prompt (derived from Session 5 design + external analysis, 2026-06-11)
**Prerequisites:** 017 (HTTP capability floor, especially Range requests) is helpful but not strictly required; the 20 MiB BYTEA cap-raise (Session 5 I) is already shipped and becomes the fast path.
**Context:** In Session 5 the team shipped only the "cap-raise variant" of the planned large-asset feature: the `pgweb.assets` CHECK constraint and CLI `MAX_ASSET_BYTES` were raised from 2 MiB to 20 MiB. True `pg_largeobject` + `lo_read` / `lo_write` streaming for assets larger than the cap was explicitly punted ("True streaming for >20 MiB assets remains Phase 2+ work"). The original design, open question (Axum + SPI transaction lifetime during streaming), and risk flag are preserved in `docs/internal/sessions/session_5.md` § I and the retrospective.

20 MiB covers "virtually every practical asset" for most apps, but real blogs, media sites, PDF repositories, or hero videos hit the wall. The design work was already done; only the streaming implementation + the async/SPI seam remain.

---

## Summary

Today any asset whose on-disk size exceeds the configured cap (currently 20 MiB) is rejected at push time with a message that points to `pg_largeobject` streaming as future work. When accepted, assets are read entirely into a `Vec<u8>` in the router (`lookup_asset`), carried in `ServeOutcome::Asset`, and emitted whole from `render_asset` in `http.rs`.

The missing piece is:
- On push: for files ≥ cutoff, `lo_create` a new large object, stream the bytes in with `lo_write` / `lo_put` inside the same transaction as the `pgweb.assets_large` (or equivalent) metadata row, then reconcile can `lo_unlink` on delete.
- On serve: for large assets, open the LO with `lo_open`, stream chunks (e.g. 64 KiB) via `lo_read` while the SPI transaction is alive, and hand an async body to Axum instead of buffering everything first.
- A new `ServeOutcome::StreamingAsset` (or extension of the existing one) so the HTTP layer can decide between "buffered in memory" and "stream from LO".

The 20 MiB BYTEA path remains the fast path for the common case.

## Why this matters now

- It is the only remaining part of the original Session 5 "I" item that was risk-flagged and punted with a clear fallback.
- The handoff prompt at the bottom of `session_5.md` lists "(stretch) True `pg_largeobject` streaming" as one of the two items for the next session once remote infra exists for F.2.
- 020 (Site v2 blog) and any future media-heavy app will eventually want hero images, inline screenshots, or downloadable PDFs that comfortably exceed 20 MiB. Without streaming they must be hosted on a CDN and referenced by URL — which works, but defeats the "everything lives in one `pg_dump`" story that is pg-web's strongest differentiator.
- The HTTP Range work in 017 becomes much more valuable once large objects can actually be range-requested without loading the whole blob.

## Current behavior (evidence)

- `crates/pg_web_cli/src/push.rs:728-737` (and the `MAX_ASSET_BYTES` constant) and `crates/pg_web_ext/src/schema.rs:95` (the CHECK on `pgweb.assets.content`) enforce the 20 MiB limit.
- Push error for oversized files explicitly says: "Larger assets via pg_largeobject streaming remain Phase 2+ work — host on a CDN until then."
- `lookup_asset` (`router.rs:119-164`) always does a full `SELECT content FROM pgweb.assets` and returns the whole byte vector.
- `render_asset` (`http.rs:158-194`) always returns a complete body (200 or 304); there is no streaming `ServeOutcome` variant and no `lo_*` calls anywhere in the extension.
- `docs/OVERVIEW.md`, `ROADMAP.md`, `APP-DEVELOPER-GUIDE.md`, and the Session 5 recap all describe the current state as "cap-raise only; true streaming is Phase 2+".

## Proposed direction (options)

**Lean:** Implement the design that was already written for Session 5 I (new `pgweb.assets_large` or reuse/extend the metadata table, `lo_create`/`lo_write` on push, `lo_open`/`lo_read` streaming on serve, new `ServeOutcome` variant). Keep the 20 MiB BYTEA path as the default fast path. Make the cutoff configurable (default 20 MiB or 1 MiB — whatever the team decides now that we have real usage data).

The big open question from the original plan was "can we hold the SPI transaction open across an async Axum streaming body?" If that turns out to be painful, the fallback is still "buffer up to N MiB in memory and document the ceiling," but we should try the proper streaming path first now that we have more experience with the worker.

## Detailed design notes

1. **Metadata table.** The original plan used a separate `pgweb.assets_large(path PK, oid OID, content_type, etag)`. We can also consider adding an `oid` column (nullable) to the existing `pgweb.assets` table and a `is_large` flag or just look at presence of the oid. Either way, the router's asset lookup must try the fast BYTEA path first, then fall back to the LO path.

2. **Push side (CLI).**
   - Read the cutoff from `pgweb.toml` (or `pgweb.settings` after sync, but cutoff must be known before the big read).
   - For files ≥ cutoff: `lo_create`, stream the file bytes in (in chunks so we don't hold everything in RAM on the client either), insert the metadata row with the OID, all inside the same push transaction.
   - Reconcile must `lo_unlink` the OID when an asset row is deleted.

3. **Serve side (extension).**
   - In the request transaction, open the large object.
   - Produce a streaming body (Axum `Body` or `Stream` of `Bytes`).
   - The `BackgroundWorker::transaction` closure must remain open for the duration of the stream (or we have to rethink the transaction boundary for streaming responses). This was the exact risk flag in Session 5.
   - Support Range requests (pairs with 017) so a 100 MiB video can be seeked without transferring the whole thing.

4. **Transaction & async boundary (the hard part).**
   - The existing pattern is `BackgroundWorker::transaction(move || { ... })` which runs to completion and then commits/rolls back.
   - Streaming responses want to keep the tx open while chunks are being sent to the client.
   - Possible approaches: (a) keep the whole serve inside the closure and use a channel or generator that the Axum handler drives; (b) accept that very large assets are served outside the normal "one request = one SPI tx" invariant for the data portion only (document the exception carefully); (c) two-phase: metadata tx first, then a separate read tx per chunk (loses atomicity with the request).
   - The prompt should require the implementing session to prototype this and either make it work cleanly or document the chosen compromise.

5. **Cutoff & fast-path policy.** Default should probably stay at the current 20 MiB (or be raised a bit now that we have experience). The cutoff is a storage/performance knob, not a correctness boundary.

6. **Reconciliation & `pg_dump` friendliness.** Large objects are cluster objects. A plain `pg_dump` of the app DB will not capture them unless `--blobs` or the right flags are used. Document this (similar to how secrets are already documented as needing special handling).

## Research tasks for the implementing session

1. Re-read the full original I design and risk discussion in `docs/internal/sessions/session_5.md:159-172` and the retrospective ("Cap-raise > true streaming for v0.2").
2. Prototype the SPI + async streaming story in a small pgrx test or spike. Can a `BackgroundWorker::transaction` closure yield an async stream that Axum can drive while the tx stays open? What happens on client disconnect mid-stream?
3. Decide the exact table shape (`pgweb.assets_large` vs. nullable oid on the main table) and the reconcile + `lo_unlink` story.
4. Map the chunk size, buffering strategy on the read side, and back-pressure behavior.
5. Confirm `lo_*` functions are available and behave the same on PG 15/16/17 under SPI (they should be).
6. Update `pg-web check` or push-time validation to give a useful message when a large file would have been accepted if streaming were enabled.
7. Decide how Range interacts with the LO path (the 017 work will inform this).

## Constraints & invariants to respect

- One HTTP request = one SPI transaction for normal (non-streaming) cases. For true large-object streaming we will almost certainly have to relax or scope this invariant; the prompt must call out the exact relaxation and justify it.
- Extension ↔ CLI strictly decoupled — the decision to stream vs. buffer is driven by size at push time; the extension just sees "this asset row has an OID, go read it via lo_*".
- Zero network hop on the serving path (still SPI only).
- PG 15/16/17 compatibility.
- The 20 MiB (or current cap) BYTEA path must remain the fast, zero-surprise path for the vast majority of assets.
- Companion-app coverage: a >20 MiB asset (or a synthetic large file in tests) must be pushable and retrievable end-to-end, preferably exercised by the site-v2 blog or a new test asset in `examples/todo/`.

## Acceptance criteria

1. A file larger than the configured cutoff (e.g. 25 MiB) can be placed in `public/` and `pg-web push` accepts it (no longer errors with the "host on a CDN" message).
2. The asset is stored via large object (OID visible in the metadata table) rather than BYTEA.
3. A `GET` for the asset streams it back correctly (full body matches the source bytes; `md5sum` or equivalent round-trips).
4. Delete/reconcile of the asset also `lo_unlink`s the underlying large object (no orphaned LOs after `pg-web push` that removes a large file).
5. The existing 20 MiB (or smaller) fast path is completely unaffected — small assets still go through BYTEA, still get ETag/immutable treatment, still round-trip in the tier-3 tests.
6. (If 017 has landed) A `Range:` request against a large streamed asset returns `206 Partial Content` with the correct bytes (without transferring the whole object).
7. Clear documentation (in `APP-DEVELOPER-GUIDE.md`, `DEPLOYMENT.md`, and the error message for the old cap) explaining the cutoff, when large objects are used, and any `pg_dump --blobs` implications.
8. New or extended tier-3 test that pushes a > cutoff synthetic asset and retrieves it (byte-perfect).
9. All five test tiers green; `cargo clippy` clean.
10. The implementation resolves (or explicitly documents the chosen answer to) the original Axum + SPI transaction lifetime question.

## Open questions

1. **Exact table design.** Separate `assets_large` table or extend the main `assets` table with a nullable `lo_oid`? What is the migration/upgrade path from the pure-BYTEA world?
2. **Transaction lifetime for the read side.** Can we keep the single "one request = one SPI tx" model while streaming, or do we have to accept a read-only LO cursor that lives across the response? What are the lock/bloat implications of a long-running tx for a 200 MiB video?
3. **Cutoff default and per-app policy.** Should the default move now that we have more data? Should it be per-route or only global?
4. **`pg_dump` / backup story.** Do we need to document a recommended `pg_dump --blobs` wrapper, or does the future `pg-web backup` command (see 019) handle this automatically?
5. **Write-side streaming on push.** For truly enormous assets (hundreds of MiB), should the CLI also stream instead of reading the whole file into memory before the transaction? (Nice-to-have; not required for v1 of this prompt.)

---

*This is the feature that lets the "your entire app is one `pg_dump`" thesis survive contact with real media. Ship the streaming path that was designed in Session 5, now that we have the luxury of the cap-raise as a proven fast path for the 99% case.*
