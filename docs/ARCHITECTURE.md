# pg-web — Architecture

This document is the authoritative engineering spec. Code should match this document, or this document should be updated in the same commit.

## The two crates

```
pg-web/
└── crates/
    ├── pg_web_ext/       # PostgreSQL extension (Rust cdylib via pgrx)
    └── pg_web_cli/       # Developer CLI (`pg-web` binary)
```

- **`pg_web_ext`** runs *inside* Postgres. Loaded at postmaster startup as a shared library, spawns a background worker, handles all HTTP traffic. Has zero filesystem code.
- **`pg_web_cli`** runs on the developer's laptop or in CI. Talks to Postgres over the standard :5432 wire protocol. Never receives HTTP. Has zero request-handling code.

The two artifacts are **strictly decoupled**. They synchronize state only through SQL upserts into framework-owned tables. No shared library crate. No ambient state. Each artifact must be buildable and releasable independently.

## Inside the extension (`pg_web_ext`)

Three top-level components:

### 1. The background worker

When Postgres's `postmaster` boots, it reads the extension's `_PG_init()` and registers a background worker via `BackgroundWorkerBuilder`. The postmaster forks a dedicated OS process for this worker — detached from any client connection, with its own SPI context and shared memory attachment.

Inside that process, the extension boots a Rust async HTTP server bound to `:8080` (configurable via the `pgweb.port` GUC). The server owns its own Tokio runtime, which is started explicitly inside the worker's `pg_main` function — **not** via the `#[tokio::main]` attribute, because the worker entry point is called by Postgres's background-worker machinery, not by a Rust `main()`.

**HTTP library: Axum** (locked 2026-04-17).

Rationale: our routing is not compile-time `Router::new().route(...)` — our routes live in the database and are resolved per-request via SPI. So we use Axum as a **thin shell**:

- A single `fallback` handler catches every request.
- That handler opens the SPI transaction, looks up the route in `pgweb.routes`, runs the handler SQL, renders via Tera, and returns.
- Tower middleware wraps each request with (a) a tracing span + request ID, (b) the SPI transaction boundary, (c) graceful shutdown.
- Axum's extractors are used lightly: `Method`, `Path` (raw string), `Query`, `HeaderMap`. We do not use route-parameter extractors because routes aren't known at compile time.

Framework logic lives in our own modules (`router.rs`, `handler.rs`, `templating.rs`, `assets.rs`). Axum is imported only at the edges. If Axum ever gets in our way, migrating to raw Hyper is a one-day job because the surface area is small.

Alternatives considered:
- **Hyper raw.** More predictable (~15 deps vs ~60), but every request handler has to hand-roll URL parsing, query parsing, and header manipulation. For our small HTTP surface that's busywork, not valuable control.
- **Actix-web.** Excellent performance but maintenance/governance uncertainty; less composable with Tower middleware.

Critical: async Tokio code lives only inside the background worker. `#[pg_extern]` functions run on Postgres's synchronous backend threads and must not call `.await`.

### 2. The SPI bridge

The worker never opens a TCP connection back to Postgres. Every SQL operation uses **SPI** (Server Programming Interface) — a C API that `pgrx` wraps safely in Rust:

```rust
Spi::connect(|client| -> Result<_, pgrx::spi::Error> {
    let row = client
        .select(
            "SELECT handler_name, template_path FROM pgweb.routes \
             WHERE method = $1 AND path_pattern = $2",
            Some(1),
            Some(vec![
                (PgBuiltInOids::TEXTOID.oid(), method.into_datum()),
                (PgBuiltInOids::TEXTOID.oid(), path.into_datum()),
            ]),
        )?
        .first();
    // ...
})?;
```

SPI runs against Postgres's in-memory shared buffers. No network. No pooling. No auth handshake. Orders of magnitude faster than a `libpq` round-trip.

### 3. The templating engine

Tera is compiled directly into the extension. On each request, the worker:

1. Fetches the raw HTML template string from `pgweb.templates` via SPI.
2. Fetches the JSON payload from the developer's SQL handler via SPI.
3. Calls `Tera::one_off(template, &context, auto_escape=true)`.
4. Ships the rendered string.

Tera chosen for Jinja2-familiar syntax, mature HTML-auto-escape, and runtime template evaluation (necessary since templates live in the database and change without recompilation). Askama (compile-time) and Minijinja are alternatives we may revisit in v2 for performance benchmarking.

## The request lifecycle

```
Browser GET /todos
       │
       ▼
┌────────────────────────────────────────────────────────────────────┐
│ Rust HTTP worker (port 8080)                                       │
│                                                                    │
│  1. Parse request: method, URL path, query string, body (if any)   │
│     Build req = { "body": {...}, "query": {...},                   │
│                   "method": "GET", "path": "/todos" }              │
│                                                                    │
│  2. Open SPI transaction.                                          │
│                                                                    │
│  3. SPI:  SELECT handler_name, template_path                       │
│           FROM pgweb.routes                                        │
│           WHERE method='GET' AND path_pattern='/todos' LIMIT 1     │
│                                                                    │
│  4. If template_path IS NULL → raw-text mode. Skip step 5.         │
│     Else → fetch template row:                                     │
│     SPI:  SELECT content FROM pgweb.templates                      │
│           WHERE template_path='pages/todos/index.html' LIMIT 1     │
│                                                                    │
│  5. SPI:  SELECT (pgweb.pages__todos__index($1::json))::text       │
│           with $1 = req. Returns either a json string (Tera ctx)   │
│           or a text string (raw HTML/body).                        │
│                                                                    │
│  6. If template_path non-NULL: Tera::one_off(template,             │
│       json_context, auto_escape=true) → rendered HTML string.      │
│     Else: use handler's text output directly.                      │
│                                                                    │
│  7. Commit SPI transaction (rollback if anything in 3-6 threw).    │
│                                                                    │
│  8. HTTP 200, Content-Type: text/html; charset=utf-8, body set.    │
└────────────────────────────────────────────────────────────────────┘
       │
       ▼
Browser renders response
```

**Invariant:** the SPI transaction covers steps 2-7 atomically. Any exception in 3-6 rolls back. No partial state ever commits.

**Note on status:** the `(req json)` handler signature and template-path-driven dispatch land in Session 2 component C. Through M1.1 the handler signature is zero-arg (`pgweb.hello_handler()`) and `template_path` is always non-NULL.

## Framework-owned tables

All live in the `pgweb` schema. Table names are unprefixed inside the schema — the schema name already scopes them. Creation happens in the extension's auto-generated install SQL (`$PGRX_HOME/<ver>/pgrx-install/share/postgresql/extension/pg_web_ext--<ver>.sql`), assembled from `extension_sql!(...)` macros in `crates/pg_web_ext/src/schema.rs`.

| Table | Purpose | Written by | Read by |
|---|---|---|---|
| `pgweb.routes` | `(method, path_pattern)` PK → `handler_name`, nullable `template_path` | CLI `push` | Extension per-request |
| `pgweb.templates` | `template_path` PK → `content` (raw HTML) | CLI `push` | Extension per-request |
| `pgweb.migrations` | `name` PK → `applied_at` — ledger of applied migration files | CLI `migrate apply` | CLI `migrate apply` |
| `pgweb.assets_small` (M1.3) | Asset path → `BYTEA` content + content_type | CLI `push` | Extension per-request |
| `pgweb.assets_large` (M1.3) | Asset path → `pg_largeobject` OID + content_type | CLI `push` | Extension (streamed) |
| `pgweb.jobs` (Phase 3) | Async job queue | SQL handlers | Async worker |
| `pgweb.sessions` (Phase 2) | Session cookies → user IDs | Ext + SQL | Extension |

**`routes.template_path` is nullable.** Non-NULL means the extension fetches the named template and renders the handler's JSON output through Tera. NULL means the handler's text output is sent as-is (raw HTML fragment / no-content response).

User application tables live in `public` (or wherever the developer declares them). pg-web never touches user tables without explicit developer action.

## Subsystems

### Static assets

- **Files < 1 MiB** (CSS, JS, small SVG): stored in `BYTEA` column in `assets_small`. Served with aggressive caching: `Cache-Control: public, max-age=31536000, immutable` once content-hashed.
- **Files ≥ 1 MiB** (images, fonts, video): stored in Postgres's native `pg_largeobject` system. Streamed out via SPI `lo_open` / `lo_read` so memory usage stays bounded regardless of file size.
- Cutoff configurable in `pgweb.toml` under `[assets] large_cutoff_bytes`. Default 1048576.

### Secrets management

Never stored in `.env` files on production hosts. Developers inject them as custom Postgres GUCs:

```
pg-web env set STRIPE_SECRET_KEY=sk_live_...
```

Which invokes:

```sql
ALTER DATABASE myapp SET pgweb.STRIPE_SECRET_KEY = 'sk_live_...';
```

SQL handlers read them via:

```sql
SELECT current_setting('pgweb.STRIPE_SECRET_KEY');
```

GUCs live in Postgres's configuration memory. Cleared on server restart unless set at ALTER DATABASE / ALTER ROLE level. Not encrypted at rest (acknowledged trade-off — they're accessible to anyone with `pg_read_all_settings`, which in practice means anyone with DB access).

### HTMX form validation

Delegate to Postgres constraints. Handlers follow the standard `(req json) RETURNS json|text` contract and catch specific exceptions:

```sql
-- pages/signup/post.sql
CREATE OR REPLACE FUNCTION pgweb.pages__signup__post(req json) RETURNS json AS $$
DECLARE
  v_email text := req->'body'->>'email';
  v_pw    text := req->'body'->>'password';
BEGIN
  INSERT INTO users(email, password_hash)
  VALUES (v_email, crypt(v_pw, gen_salt('bf', 12)));
  RETURN json_build_object('ok', true);
EXCEPTION
  WHEN unique_violation THEN
    RETURN json_build_object('ok', false, 'error', 'Email already taken');
  WHEN check_violation THEN
    RETURN json_build_object('ok', false, 'error', 'Invalid input');
END;
$$ LANGUAGE plpgsql;
```

`pages/signup/post.html` branches on `{% if ok %}` vs `{% else %}`. `hx-swap-oob="true"` inside the template lets HTMX update multiple page regions from one response.

For responses that are a single HTML fragment with no user-interpolated content, skip the `.html` entirely and return `text` directly — router sends bytes as-is.

### Logging

- App-level: developers use `RAISE NOTICE` / `RAISE LOG` in PL/pgSQL handlers.
- The Rust worker registers an SPI notice handler that captures each `NoticeResponse` / `NOTICE` message.
- Messages are formatted as structured JSON (timestamp, level, file, line, message, request_id) and written to stdout.
- Docker's log driver picks up stdout; Datadog/CloudWatch/Loki collect from there.

### Error handling

Two modes, selected by the `pgweb.env` GUC (`development` or `production`):

- **Production:** fatal SQL exception → HTTP 500 with a generic opaque error page. Stack traces suppressed.
- **Development:** fatal SQL exception → HTTP 500 with a styled debug page inspired by Laravel Ignition and Rails's error pages. Includes:
  - Failing `.sql` file path and line number
  - Postgres error code (`SQLSTATE`)
  - `MESSAGE`, `DETAIL`, `HINT`, `CONTEXT` from the Postgres error
  - Transaction state at failure
  - Recent request trace

The dev error page is served by the extension directly — not dependent on any user-defined template.

## Configuration (`pgweb.toml`)

Lives at the app root. Loaded by the CLI, pushed into the database as GUCs on `pg-web dev` / `push`.

```toml
[server]
port = 8080
env = "development"  # or "production"

[database]
# Connection string template; CLI uses this to connect when pushing
url_env = "DATABASE_URL"

[assets]
large_cutoff_bytes = 1048576
cache_control_static = "public, max-age=31536000, immutable"

[dev]
watch_paths = ["pages", "public"]
```

## Version compatibility

- Postgres 15, 16, 17 only.
- pgrx 0.18.x pinned.
- Rust stable (1.95+ at time of writing).
- One minor release of the extension may support multiple PG majors; ALTER EXTENSION scripts handle schema changes across minor versions.
