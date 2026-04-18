# Session 3 — Interactive Dev Loop (M1.2)

**Status:** planned, not started.
**Theme:** turn pg-web from "type two commands after every edit" into a file-watcher-driven dev loop. The CLI also takes over stack lifecycle so developers stop thinking about `docker compose` directly. Dynamic routes + dev error page round out the DX.

By the end of this session, the daily loop is:

```bash
pg-web up           # boots the stack, prints URL, auto-resolves DATABASE_URL
pg-web dev          # watches pages/ and public/; auto-pushes on save; streams logs
# edit .sql/.html; refresh browser; see change
pg-web down         # stops the stack (or leave it; `up` is idempotent)
```

## Prerequisites (shipped in Session 1 + 2)

- Extension serves HTTP with `(req json)` handler contract + template-path dispatch ✅
- CLI `init` / `push` / `migrate apply` ✅
- Docker image + examples/demo app + Docker E2E ✅
- Locked layout spec in `docs/APP-LAYOUT.md` ✅

The spec settled in Session 2 is the contract the watcher re-syncs against. Nothing in this session should need to revise it.

---

## Work breakdown

### A. CLI stack lifecycle — `pg-web up` / `pg-web down`

**New module:** `crates/pg_web_cli/src/stack.rs`.

- `pg-web up [--dir .]` — discover `docker-compose.yml` in `--dir`, shell out to `docker compose up -d`, then poll `:8080` (and `:5432`) until both respond. Print the resolved `DATABASE_URL` so users can copy it. `--detach` default; `--foreground` tails container logs.
- `pg-web down [--dir .] [--volumes]` — `docker compose down`, optional `--volumes` to drop `pgdata`.
- Preflight: check `docker --version` succeeds; bail with install hint otherwise.
- Exit codes: 0 on success, non-zero with clear stderr otherwise.

**Tests:**
- Unit: port-poll helper (pure, deadline-based).
- Hermetic: `up` with missing `docker-compose.yml` → clear error.
- No integration tests that actually boot Docker here — tier 3 E2E in `crates/pg_web_cli/tests/docker_e2e.rs` already covers the full flow; this is a thin wrapper.

### B. CLI `pg-web dev` — file watcher + auto-push

**New module:** `crates/pg_web_cli/src/dev.rs`. Uses `notify` crate for cross-platform file watching.

- `pg-web dev` — ensure `up` (no-op if already running), connect to DB, watch `pages/` and `public/`.
- On any `.sql` or `.html` save: re-run a targeted push for the affected file.
- On `.sql` save: shift-left pre-flight — wrap the file in `BEGIN; <contents>; ROLLBACK;` via the live connection before the real `CREATE OR REPLACE FUNCTION` goes in. If the rollback'd version errored, print the Postgres error and don't commit the real version. The live route keeps working until the developer fixes it.
- On non-recognized files (readmes, dotfiles): no-op, no spam.
- Tail container logs in-band (optional flag `--no-logs`).
- Ctrl-C: clean shutdown.

**Tests:**
- Unit: file-event → action classifier (save `.sql` → push, save `.gitignore` → ignore, etc.).
- Hermetic: dispatch a synthetic event, assert on side effects.
- Full behavior deferred to tier 3 — `docker_e2e.rs` gains a "watcher sees a save and re-pushes" test (starts `dev` in a thread, writes a file, polls for the new content at the HTTP endpoint).

### C. Dynamic route patterns — `[id]` captures

**Spec update:** `docs/APP-LAYOUT.md` gains a "Dynamic segments" section.

- `pages/posts/[id]/index.html` (+ `.sql`) → matches `GET /posts/:id`. The `id` segment from the URL is threaded into `req.path_params` as `{ "id": "42" }`.
- Multiple captures allowed: `pages/users/[user]/posts/[post]/index.html` → `/users/:user/posts/:post`.
- Path-param values are always strings; handlers cast as needed.
- Reserved: `[` and `]` in directory names are the capture markers. Literal brackets in URLs aren't supported.

**Implementation:**
- `paths.rs::scan` recognizes `[name]` directory segments and emits a pattern-form `path_pattern` (e.g., `/posts/:id`) along with a list of capture names.
- `pgweb.routes.path_pattern` stores the pattern (`/posts/:id`, not `/posts/42`). Add a `path_captures` column (TEXT[] or jsonb) listing capture names in order.
- Router match changes: instead of exact-match `path_pattern = $1`, do longest-prefix-match among templated patterns. Simple implementation for Phase 1: store patterns, iterate in rank order (static > single-capture > multi-capture), match each against the request path via a regex or manual segment-by-segment compare.
- Extract capture values → `req.path_params`.

**Tests:**
- `paths.rs` unit: `[id]` segment recognition, reserved-character handling.
- `#[pg_test]`: insert a dynamic route, look it up with a concrete path, verify captures.
- HTTP smoke: dynamic route + capture surfaces through `req.path_params`.
- Demo extension: add `pages/todos/[id]/index.html` → todo detail view.

### D. Dev error page

When the extension is running in `env = "development"` mode, a fatal SQL exception returns a styled error page instead of generic 500:

- SQLSTATE + message + DETAIL + HINT + CONTEXT (from the PG error)
- File + line of the failing handler (resolved via `pgweb.routes` lookup on handler_name)
- The `req` JSON that triggered the failure
- A stacktrace-ish view of SPI calls in the request

Production mode keeps the current generic 500. Mode selection via `pgweb.env` GUC (defaults to "development" in dev, "production" via `pgweb.toml`).

**Tests:** `#[pg_test]` for the error-page renderer. HTTP smoke test: induce a SQL error (e.g., hit a route pointing at a nonexistent handler), assert on the dev page content.

### E. Static asset serving

Two tiers:

- **Small (< 1 MiB):** `BYTEA` column in `pgweb.assets_small(path PK, content, content_type, etag)`. Served with `Cache-Control: public, max-age=31536000, immutable` when content-hashed.
- **Large (≥ 1 MiB):** `pg_largeobject` OID in `pgweb.assets_large(path PK, oid, content_type)`. Streamed via SPI `lo_read` with a bounded read buffer so memory stays flat regardless of file size.

CLI `pg-web push` walks `public/`; `pg-web dev` syncs on save. Content-type from file extension (ship a small mapping, maybe `mime_guess` crate).

**Cutoff configurable:** `[assets] large_cutoff_bytes` in `pgweb.toml`. Default 1048576.

**Demo enhancement:** pull the inline `<style>` block out of `examples/demo/pages/index.html` into `public/styles.css`, verify the page still renders correctly.

**Tests:** `#[pg_test]` round-trip for both tiers. HTTP smoke for small. Tier 3 demo now hits `public/styles.css` and asserts on content-type + cache-control.

---

## Testing plan (consolidated)

| Tier | What gains coverage                                                                  |
|------|---------------------------------------------------------------------------------------|
| 1 — `#[pg_test]`    | Dynamic route pattern storage + capture extraction. Dev error page renderer. Asset round-trip (BYTEA + pg_largeobject). |
| 2a — HTTP smoke     | GET /posts/42 hits a dynamic handler with `req.path_params.id = "42"`. Dev error page served on handler crash. Small asset served with correct content-type. |
| 2b — CLI            | `paths.rs` recognizes `[id]` dirs. `dev.rs` file-event classifier. `stack.rs` port poller. |
| 3 — Docker E2E      | `up` → `dev` (writes a file in a thread) → HTTP reflects the change. Asset flow. Dynamic route flow. |

Target: 80+ tests green (from 58 today). Not a hard requirement; additive is fine.

## Things deliberately NOT in session 3

- **Published Docker image to registry** — M1.4 release task.
- **`pg-web env set`** (secrets via GUC) — M1.4.
- **`pg-web check`** (lint) — M1.4.
- **`pgweb.html_escape()` helper** — M1.4.
- **User-facing form-validation UX** — M1.4 (depends on dev error page from this session + html_escape from M1.4).
- **Declarative schema diffing** — Phase 2.5.
- **Auth / sessions / RLS** — Phase 2.

## Open design questions to resolve at session start

1. **Dynamic route storage.** Add a `path_captures` column to `pgweb.routes`, or derive from the pattern string at match time? Leaning: derive on the fly — fewer moving parts, router is fast either way for Phase 1 route counts.
2. **Router match order.** Naïve scan through all routes vs. trie / compiled regex? Phase 1 apps have < 100 routes; naïve scan with length-sorted patterns is fine. Revisit if it ever matters.
3. **File-watcher debounce.** Editors write staged files (`.filename.swp`, `filename.new`); notify fires multiple events per "save." Need a small debounce window (100-250 ms?). Resolve at implementation time.
4. **Asset caching.** Hash-based filenames (`styles.abc123.css`) for content-based caching, or just ETag headers + If-None-Match? Simpler for Phase 1: ETag-only, dev mode disables caching, prod returns immutable.

## Suggested order

Components land A → E, each followed by a stop-and-check:

1. **A** — `pg-web up`/`down`. Smallest, independent, nice DX win from day one.
2. **B** — `pg-web dev`. Depends on A (watcher expects stack running).
3. **C** — Dynamic routes. Schema + router + walker all change. Largest of the five.
4. **D** — Dev error page. Independent of A-C; could slot earlier, but relies on error paths that C may expose.
5. **E** — Static assets. Independent but demo enhancement ties it to the final state.

Order can shuffle if a component turns out to be blocked on an earlier one in a way the plan didn't anticipate.
