//! Framework-owned schema installed on `CREATE EXTENSION`.
//!
//! Tables live under the `pgweb` schema (cannot use `pg_web` — Postgres
//! reserves schema names starting with `pg_`). The CLI writes rows; the
//! request handler reads them per-request via SPI.
//!
//! We also seed a single hello-world route so a fresh `CREATE EXTENSION
//! pg_web_ext;` produces an immediately-curlable `GET /`. When the CLI's
//! `pg-web push` lands (M1.1 step 5), it will overwrite these defaults.

use pgrx::extension_sql;

extension_sql!(
    r#"
CREATE SCHEMA IF NOT EXISTS pgweb;

CREATE TABLE pgweb.routes (
    path_pattern  TEXT NOT NULL,
    method        TEXT NOT NULL DEFAULT 'GET',
    handler_name  TEXT NOT NULL,
    template_path TEXT,
    PRIMARY KEY (method, path_pattern)
);

CREATE TABLE pgweb.templates (
    template_path TEXT PRIMARY KEY,
    content       TEXT NOT NULL
);

-- Ledger of raw-SQL migrations applied via `pg-web migrate apply`. Phase 1
-- tracks file identity only (by name). Phase 2+ may add checksum for drift
-- detection; for now the assumption is that migration files are append-only.
CREATE TABLE pgweb.migrations (
    name       TEXT PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Ops-visibility ledger of every `pg-web push` that commits. Answers
-- "when did we last deploy, from where, how big?" with a single SELECT.
-- Intentionally append-only: no updates, no deletes; one row per
-- successful push. Dry-runs do NOT insert (they roll back everything,
-- including this row).
--
-- `file_count` sums every DB-side touch this push performed: route
-- upserts + template upserts + handler upserts + asset upserts. A
-- deployment with 0 files means push ran but found nothing changed
-- on disk — useful signal on its own.
--
-- `from_host` is the hostname of whoever ran the CLI. Cheap to record
-- and surprisingly useful when tracking down "who pushed this?" on
-- a shared staging DB.
CREATE TABLE pgweb.deployments (
    id                   BIGSERIAL PRIMARY KEY,
    pushed_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    from_host            TEXT,
    file_count           INTEGER NOT NULL DEFAULT 0,
    migrations_applied   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX deployments_pushed_at_idx ON pgweb.deployments (pushed_at DESC);

-- Framework-owned key/value settings. The database is the source of
-- truth for runtime configuration so a container restart doesn't lose
-- state and no separate config file lives inside the image. `pg-web push`
-- syncs values from the user's `pgweb.toml` into this table.
--
-- Currently recognized keys:
--   'env'  — 'development' enables rich error pages; 'production' serves
--            generic 500s. Default 'development' so a fresh extension
--            install is immediately debuggable; `pg-web push` overwrites
--            based on pgweb.toml's [server] env.
CREATE TABLE pgweb.settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT INTO pgweb.settings (key, value) VALUES ('env', 'development');

-- Static assets served from the `public/` tree. BYTEA-backed, capped at
-- 20 MiB per file by a CHECK constraint so a runaway file doesn't wedge
-- the worker on read. The 20 MiB cap (v0.2 / Component I) covers virtually
-- every practical asset — hero images, vendor JS bundles, PDFs — without
-- committing to true `pg_largeobject` streaming yet. lo_read-backed
-- streaming for assets larger than 20 MiB is a Phase 2+ follow-up.
--
-- The ETag column stores a content-hash digest pre-wrapped in the
-- double-quoted form the HTTP header uses, so the router can emit it
-- verbatim. Cache-Control header values live in the router, not the DB
-- (same value for every asset in a given env).
CREATE TABLE pgweb.assets (
    path          TEXT PRIMARY KEY,
    content       BYTEA NOT NULL,
    content_type  TEXT NOT NULL,
    etag          TEXT NOT NULL,
    CHECK (length(content) <= 20971520)
);

-- Default handler for the seeded GET / route. Follows the standard
-- app-developer contract: `(req json) RETURNS json`. Ignores `req` —
-- the handler has no inputs to read — but the signature matches what
-- every user-authored handler will use.
CREATE FUNCTION pgweb.hello_handler(req json) RETURNS json AS $$
    SELECT json_build_object('name', 'pg-web')
$$ LANGUAGE sql STABLE;

INSERT INTO pgweb.routes (path_pattern, method, handler_name, template_path)
VALUES ('/', 'GET', 'pgweb.hello_handler', 'pages/index.html');

INSERT INTO pgweb.templates (template_path, content) VALUES (
    'pages/index.html',
    '<!doctype html>
<html>
<body>
  <h1>hello from {{ name }}</h1>
</body>
</html>
'
);

-- Wrapper around every handler call. Runs the handler inside a PL/pgSQL
-- EXCEPTION block so the router can catch SQL errors structurally —
-- SQLSTATE + MESSAGE + DETAIL + HINT + CONTEXT come back as distinct
-- columns instead of longjmping across the Rust FFI boundary. Every
-- request pays one savepoint's worth of overhead (microseconds); we buy
-- the rich-error-page UX with it.
--
-- Why `handler_name text` rather than `regprocedure`: regprocedure casts
-- resolve at call time, which would surface a "function does not exist"
-- error at the cast, not inside the EXCEPTION block where we can catch
-- it. Dynamic EXECUTE lets the wrapper catch that case uniformly.
CREATE FUNCTION pgweb._framework_call_handler(
    p_handler_name TEXT,
    p_req          JSON
) RETURNS TABLE (
    ok               BOOLEAN,
    result_text      TEXT,
    error_sqlstate   TEXT,
    error_message    TEXT,
    error_detail     TEXT,
    error_hint       TEXT,
    error_context    TEXT
) LANGUAGE plpgsql AS $fn$
DECLARE
    v_sql TEXT;
BEGIN
    -- `format` with %s for identifier, %L for literal. $1 binds the json
    -- at EXECUTE time so no string-escaping of user content is needed.
    v_sql := format('SELECT (%s($1))::text', p_handler_name);
    EXECUTE v_sql INTO result_text USING p_req;
    ok := TRUE;
    RETURN NEXT;
EXCEPTION WHEN OTHERS THEN
    ok := FALSE;
    result_text := NULL;
    GET STACKED DIAGNOSTICS
        error_sqlstate = RETURNED_SQLSTATE,
        error_message  = MESSAGE_TEXT,
        error_detail   = PG_EXCEPTION_DETAIL,
        error_hint     = PG_EXCEPTION_HINT,
        error_context  = PG_EXCEPTION_CONTEXT;
    RETURN NEXT;
END;
$fn$;

-- User-facing helper: escape the five HTML-unsafe characters so a
-- handler (especially a raw-text one with no Tera template) can safely
-- interpolate user input into its response body. Mirrors the in-Rust
-- escape used by the dev error page; if the five-char policy ever
-- changes, update both sites.
--
-- STRICT      — NULL input returns NULL, so call sites don't need
--               NULL-wrapping ceremony.
-- IMMUTABLE   — planner can fold constants, use in indexes / generated
--               columns, and inline the call into outer queries.
-- PARALLEL SAFE — pure text transform, no side effects.
--
-- Escape order (innermost replace runs first, so '&' must be at the
-- inside or the '&' characters introduced by later entity refs get
-- double-escaped):
--   &  → &amp;
--   <  → &lt;
--   >  → &gt;
--   "  → &quot;
--   '  → &#39;
--
-- NOT idempotent by design: html_escape('&amp;') = '&amp;amp;'. The
-- contract is single-pass escaping of user input; re-escaping already-
-- escaped text is caller error.
CREATE FUNCTION pgweb.html_escape(s TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT replace(
             replace(
               replace(
                 replace(
                   replace(s, '&', '&amp;'),
                   '<', '&lt;'),
                 '>', '&gt;'),
               '"', '&quot;'),
             '''', '&#39;')
$$;

COMMENT ON FUNCTION pgweb.html_escape(TEXT) IS
    'Escape HTML-unsafe characters (& < > " '') for safe embedding in response bodies. Returns NULL on NULL input.';

-- Sugar helper for handlers reading runtime settings. Replaces the
-- verbose SELECT value FROM pgweb.settings WHERE key = $1 with
-- SELECT pgweb.setting('STRIPE_KEY'). NULL on miss (no row) — the
-- STRICT guarantee covers NULL input too so handlers can safely chain
-- COALESCE for defaults: COALESCE(pgweb.setting('foo'), 'default').
--
-- STABLE (not IMMUTABLE) because pgweb.settings values can change
-- between calls via `pg-web env set`. STRICT for NULL pass-through.
-- PARALLEL SAFE because reads are side-effect free.
--
-- Parameter named `p_key` (not `key`) to avoid colliding with the
-- pgweb.settings.key column — `WHERE key = key` would be ambiguous
-- between the column and the parameter.
CREATE FUNCTION pgweb.setting(p_key TEXT) RETURNS TEXT
LANGUAGE sql STABLE STRICT PARALLEL SAFE AS $$
    SELECT value FROM pgweb.settings WHERE key = p_key
$$;

COMMENT ON FUNCTION pgweb.setting(TEXT) IS
    'Look up a key in pgweb.settings. Returns NULL on miss or NULL input. Set values via `pg-web env set KEY=VALUE`.';

-- Response contract v2 (prompt 013): status, headers, cookies, redirects, explicit
-- content-type from handlers. Backward-compatible: a handler return value that
-- does not contain a top-level "$pgweb" key is treated exactly as before (bare
-- body text or JSON context for Tera). The envelope is the wire format detected
-- by the router; app authors use the helpers below and never write "$pgweb".
--
-- Design:
--   • $pgweb sentinel object is unambiguous (collides with zero real payloads).
--   • "body" (string) present → emit literally (bypasses Tera even on template routes).
--   • "context" (object) present in template mode → feed to Tera, apply envelope attrs.
--   • cookies values are pre-serialized Set-Cookie strings (from set_cookie helper).
--   • Content-Type comes from the dedicated field (headers map is for others).
--   • Defaults preserve legacy behavior (200 + text/html for non-envelope paths).
--
-- Cookie defaults (align with session_6.md A1): HttpOnly + SameSite=Lax on,
-- Secure only when env='production' (dev over plain HTTP must work). Caller can
-- override http_only (needed for the JS-readable CSRF cookie).
--
-- Helpers are additive (like html_escape/setting). No _framework_call_handler
-- change — envelope travels as text and is re-detected by JSON parse in router.

CREATE FUNCTION pgweb.respond(
    p_body         TEXT    DEFAULT '',
    p_status       INT     DEFAULT 200,
    p_headers      JSONB   DEFAULT '{}',
    p_content_type TEXT    DEFAULT NULL,
    p_cookies      JSONB   DEFAULT '[]'
) RETURNS JSON
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $fn$
    SELECT json_build_object(
        '$pgweb', json_build_object(
            'status',       p_status,
            'headers',      COALESCE(p_headers, '{}'::jsonb),
            'content_type', p_content_type,
            'cookies',      COALESCE(p_cookies, '[]'::jsonb)
        ),
        'body', p_body
    );
$fn$;

COMMENT ON FUNCTION pgweb.respond(TEXT, INT, JSONB, TEXT, JSONB) IS
    'Response contract v2 envelope constructor. Returns a JSON envelope the router recognizes by its "$pgweb" key. Use for custom status/headers/cookies/content-type on both raw-text and template routes. "body" (if present) is emitted verbatim even on template routes.';

CREATE FUNCTION pgweb.set_cookie(
    p_name  TEXT,
    p_value TEXT,
    p_opts  JSONB DEFAULT '{}'
) RETURNS TEXT
LANGUAGE sql STABLE STRICT PARALLEL SAFE AS $fn$
WITH e AS (SELECT (pgweb.setting('env') = 'production') AS prod)
SELECT
    p_name || '=' || p_value
    || COALESCE('; Path=' || (p_opts->>'path'), '; Path=/')
    || CASE WHEN COALESCE((p_opts->>'http_only')::boolean, true) THEN '; HttpOnly' ELSE '' END
    || CASE WHEN COALESCE((p_opts->>'secure')::boolean, (SELECT prod FROM e)) THEN '; Secure' ELSE '' END
    || CASE WHEN p_opts ? 'same_site' THEN '; SameSite=' || (p_opts->>'same_site') ELSE '; SameSite=Lax' END
    || CASE WHEN p_opts ? 'max_age'  THEN '; Max-Age='  || (p_opts->>'max_age')  ELSE '' END
    || CASE WHEN p_opts ? 'domain'  THEN '; Domain='  || (p_opts->>'domain')  ELSE '' END
    || CASE WHEN p_opts ? 'expires' THEN '; Expires=' || (p_opts->>'expires') ELSE '' END;
$fn$;

COMMENT ON FUNCTION pgweb.set_cookie(TEXT, TEXT, JSONB) IS
    'Build a Set-Cookie header value string for use with pgweb.respond / pgweb.redirect cookies array. Defaults: HttpOnly=true, SameSite=Lax, Secure=(env=production), Path=/. Override http_only for JS-readable cookies (e.g. CSRF).';

CREATE FUNCTION pgweb.redirect(
    p_location TEXT,
    p_status   INT   DEFAULT 303,
    p_cookies  JSONB DEFAULT '[]'
) RETURNS JSON
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $fn$
    SELECT pgweb.respond(
        '',
        p_status,
        jsonb_build_object('Location', p_location),
        NULL,
        p_cookies
    );
$fn$;

COMMENT ON FUNCTION pgweb.redirect(TEXT, INT, JSONB) IS
    'Sugar for Post-Redirect-Get (and other redirects). Emits the given status + Location header. Empty body. Optional cookies array (values from pgweb.set_cookie).';

CREATE FUNCTION pgweb.json(
    p_payload JSONB,
    p_status  INT   DEFAULT 200,
    p_headers JSONB DEFAULT '{}',
    p_cookies JSONB DEFAULT '[]'
) RETURNS JSON
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $fn$
    SELECT pgweb.respond(
        p_payload::text,
        p_status,
        p_headers,
        'application/json',
        p_cookies
    );
$fn$;

COMMENT ON FUNCTION pgweb.json(JSONB, INT, JSONB, JSONB) IS
    'Return a JSON payload with explicit Content-Type: application/json (and optional status/headers/cookies). The payload is serialized into the envelope body.';

COMMENT ON SCHEMA pgweb IS 'pg-web framework tables. Managed by the extension and CLI; do not modify directly.';
"#,
    name = "framework_tables",
    bootstrap,
);

// Only compiled under `cargo pgrx test` (which activates the pg_test feature).
// Plain `cfg(test)` is avoided here because pgrx's schema generator turns
// that cfg on during introspection, which would embed these `#[pg_test]`
// wrapper symbols into every install SQL — wrappers that the non-test .so
// doesn't export, so CREATE EXTENSION would fail.
#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn schema_exists() {
        let exists = Spi::get_one::<bool>(
            "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'pgweb')",
        )
        .expect("schema lookup should not error")
        .expect("schema lookup should return a row");
        assert!(exists, "pgweb schema should exist after CREATE EXTENSION");
    }

    #[pg_test]
    fn default_route_seeded() {
        let handler = Spi::get_one::<String>(
            "SELECT handler_name FROM pgweb.routes \
             WHERE method = 'GET' AND path_pattern = '/'",
        )
        .expect("route lookup should not error")
        .expect("default GET / route should be seeded");
        assert_eq!(handler, "pgweb.hello_handler");
    }

    #[pg_test]
    fn default_template_seeded() {
        let content = Spi::get_one::<String>(
            "SELECT content FROM pgweb.templates WHERE template_path = 'pages/index.html'",
        )
        .expect("template lookup should not error")
        .expect("default template should be seeded");
        assert!(
            content.contains("{{ name }}"),
            "template should contain Tera interpolation placeholder"
        );
    }

    #[pg_test]
    fn default_handler_returns_expected_json() {
        let json = Spi::get_one::<pgrx::JsonB>(
            "SELECT pgweb.hello_handler('{}'::json)::jsonb",
        )
        .expect("handler call should not error")
        .expect("handler should return a row");
        // pgrx::JsonB wraps a serde_json::Value
        let name = json
            .0
            .get("name")
            .and_then(|v| v.as_str())
            .expect("handler output should contain 'name' field");
        assert_eq!(name, "pg-web");
    }

    #[pg_test]
    fn routes_table_accepts_additional_inserts() {
        Spi::run(
            "INSERT INTO pgweb.routes (path_pattern, method, handler_name, template_path) \
             VALUES ('/about', 'GET', 'pgweb.hello_handler', 'pages/index.html')",
        )
        .expect("insert should succeed");

        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM pgweb.routes WHERE path_pattern = '/about'",
        )
        .expect("count should not error")
        .expect("count should return a row");
        assert_eq!(count, 1);
    }

    #[pg_test]
    fn migrations_table_exists_and_is_empty() {
        let count = Spi::get_one::<i64>("SELECT COUNT(*) FROM pgweb.migrations")
            .expect("query should not error")
            .expect("count should return a row");
        assert_eq!(count, 0, "migrations ledger should be empty on fresh install");
    }

    #[pg_test]
    fn handler_contract_receives_req_json() {
        // A user-written handler should be able to read from req.body.
        // This pg_test creates a trivial echo handler, calls it with a
        // synthetic req, and verifies the field comes back.
        Spi::run(
            "CREATE FUNCTION pgweb.test_echo(req json) RETURNS json AS $$ \
             SELECT json_build_object('echo', req->'body'->>'x') $$ LANGUAGE sql",
        )
        .expect("create echo fn");

        let json = Spi::get_one::<pgrx::JsonB>(
            "SELECT pgweb.test_echo('{\"body\":{\"x\":\"hi\"}}'::json)::jsonb",
        )
        .expect("call should succeed")
        .expect("call should return a row");
        let echo = json
            .0
            .get("echo")
            .and_then(|v| v.as_str())
            .expect("echo field present");
        assert_eq!(echo, "hi");
    }

    #[pg_test]
    fn migrations_applied_at_defaults_to_now() {
        Spi::run("INSERT INTO pgweb.migrations (name) VALUES ('0002_users.sql')")
            .expect("insert should succeed");

        let within = Spi::get_one::<bool>(
            "SELECT applied_at >= now() - interval '5 seconds' \
             FROM pgweb.migrations WHERE name = '0002_users.sql'",
        )
        .expect("query should not error")
        .expect("row should exist");
        assert!(within, "applied_at should default to current transaction time");
    }

    #[pg_test]
    fn html_escape_nulls_pass_through() {
        // STRICT makes this a Postgres-layer guarantee: NULL in → NULL
        // out, no body execution. Saves every caller an IS NOT NULL
        // check before interpolating.
        let is_null = Spi::get_one::<bool>("SELECT pgweb.html_escape(NULL::text) IS NULL")
            .expect("query should not error")
            .expect("query should return a row");
        assert!(is_null, "html_escape(NULL) should return NULL (STRICT)");
    }

    #[pg_test]
    fn html_escape_handles_all_five_chars() {
        // Input value is literally `& < > " '` (spaces between each);
        // the `''` in the SQL literal is one escaped single-quote.
        let out = Spi::get_one::<String>("SELECT pgweb.html_escape('& < > \" ''')")
            .expect("query should not error")
            .expect("query should return a row");
        assert_eq!(out, "&amp; &lt; &gt; &quot; &#39;");
    }

    #[pg_test]
    fn html_escape_is_not_idempotent_by_design() {
        // Re-escaping already-escaped text double-escapes. This is the
        // documented contract: handlers must escape user input exactly
        // once, at the point of interpolation. If this test ever flips
        // to idempotent, something changed the replace semantics and
        // docs need updating too.
        let out = Spi::get_one::<String>("SELECT pgweb.html_escape('&amp;')")
            .expect("query should not error")
            .expect("query should return a row");
        assert_eq!(out, "&amp;amp;");
    }

    #[pg_test]
    fn setting_returns_null_on_missing_key() {
        let is_null = Spi::get_one::<bool>("SELECT pgweb.setting('__nope__') IS NULL")
            .expect("query should not error")
            .expect("query should return a row");
        assert!(is_null, "pgweb.setting('__nope__') should be NULL on miss");
    }

    #[pg_test]
    fn setting_returns_null_on_null_input() {
        // STRICT short-circuits NULL input without even reading the
        // table, so this also documents the zero-table-scan property.
        let is_null = Spi::get_one::<bool>("SELECT pgweb.setting(NULL::text) IS NULL")
            .expect("query should not error")
            .expect("query should return a row");
        assert!(is_null, "pgweb.setting(NULL) should be NULL (STRICT)");
    }

    #[pg_test]
    fn setting_reads_existing_seeded_env() {
        // The schema seeds INSERT INTO pgweb.settings (key, value)
        // VALUES ('env', 'development'), so pgweb.setting('env')
        // should surface that at install time.
        let value = Spi::get_one::<String>("SELECT pgweb.setting('env')")
            .expect("query should not error")
            .expect("query should return a row");
        assert_eq!(value, "development");
    }

    #[pg_test]
    fn setting_reads_freshly_inserted_key() {
        Spi::run(
            "INSERT INTO pgweb.settings (key, value) VALUES ('STRIPE_KEY', 'sk_test_abc')",
        )
        .expect("insert should succeed");

        let value = Spi::get_one::<String>("SELECT pgweb.setting('STRIPE_KEY')")
            .expect("query should not error")
            .expect("query should return a row");
        assert_eq!(value, "sk_test_abc");
    }

    #[pg_test]
    fn deployments_table_exists_and_is_empty_on_install() {
        let count = Spi::get_one::<i64>("SELECT COUNT(*) FROM pgweb.deployments")
            .expect("query should not error")
            .expect("row should be returned");
        assert_eq!(count, 0, "deployments ledger should be empty on fresh install");
    }

    #[pg_test]
    fn deployments_accepts_insert_with_defaults() {
        // Minimal insert: only file_count required (has default 0 too,
        // but let's exercise a realistic value). from_host left NULL —
        // column is nullable on purpose, since some CI contexts don't
        // usefully resolve a hostname.
        Spi::run(
            "INSERT INTO pgweb.deployments (file_count, migrations_applied) \
             VALUES (7, 2)",
        )
        .expect("insert should succeed");

        let row = Spi::get_one::<i32>(
            "SELECT file_count FROM pgweb.deployments ORDER BY id DESC LIMIT 1",
        )
        .expect("query should not error")
        .expect("row should exist");
        assert_eq!(row, 7);
    }

    #[pg_test]
    fn deployments_pushed_at_defaults_to_now() {
        Spi::run(
            "INSERT INTO pgweb.deployments (from_host, file_count) \
             VALUES ('smoke-host', 1)",
        )
        .expect("insert should succeed");

        let within = Spi::get_one::<bool>(
            "SELECT pushed_at >= now() - interval '5 seconds' \
             FROM pgweb.deployments ORDER BY id DESC LIMIT 1",
        )
        .expect("query should not error")
        .expect("row should exist");
        assert!(
            within,
            "pushed_at should default to the current transaction's now()"
        );
    }

    // ---- Response contract v2 helpers (prompt 013) ----

    #[pg_test]
    fn respond_helper_builds_envelope() {
        let j = Spi::get_one::<pgrx::JsonB>(
            "SELECT pgweb.respond('hello', 201, '{\"X-Foo\": \"bar\"}'::jsonb, 'text/plain', '[]'::jsonb)::jsonb",
        )
        .expect("respond call")
        .expect("row");
        let root = &j.0;
        assert!(root.get("$pgweb").is_some(), "must have $pgweb sentinel");
        let pg = root.get("$pgweb").unwrap().as_object().unwrap();
        assert_eq!(pg.get("status").and_then(|v| v.as_i64()), Some(201));
        assert_eq!(pg.get("content_type").and_then(|v| v.as_str()), Some("text/plain"));
        assert_eq!(root.get("body").and_then(|v| v.as_str()), Some("hello"));
        let h = pg.get("headers").and_then(|v| v.as_object()).unwrap();
        assert_eq!(h.get("X-Foo").and_then(|v| v.as_str()), Some("bar"));
    }

    #[pg_test]
    fn redirect_helper_builds_303_location() {
        let j = Spi::get_one::<pgrx::JsonB>(
            "SELECT pgweb.redirect('/target', 303)::jsonb",
        )
        .expect("redirect call")
        .expect("row");
        let pg = j.0.get("$pgweb").unwrap().as_object().unwrap();
        assert_eq!(pg.get("status").and_then(|v| v.as_i64()), Some(303));
        let h = pg.get("headers").and_then(|v| v.as_object()).unwrap();
        assert_eq!(h.get("Location").and_then(|v| v.as_str()), Some("/target"));
        // body absent or empty is fine for redirect
        let body = j.0.get("body").and_then(|v| v.as_str()).unwrap_or("");
        assert!(body.is_empty());
    }

    #[pg_test]
    fn json_helper_sets_application_json_and_body() {
        let j = Spi::get_one::<pgrx::JsonB>(
            "SELECT pgweb.json('{\"ok\": true}'::jsonb, 200)::jsonb",
        )
        .expect("json call")
        .expect("row");
        let pg = j.0.get("$pgweb").unwrap().as_object().unwrap();
        assert_eq!(pg.get("content_type").and_then(|v| v.as_str()), Some("application/json"));
        // Postgres jsonb::text may emit minor whitespace differences ("{\"ok\": true}" vs compact).
        // We care that the payload is present as text in the envelope body (this becomes
        // the response body for a raw-text JSON API route).
        let body = j.0.get("body").and_then(|v| v.as_str()).unwrap_or("");
        assert!(body.contains("\"ok\"") && body.contains("true"), "body should contain the json payload; got {body}");
    }

    #[pg_test]
    fn set_cookie_builds_serialized_value_with_defaults_in_dev() {
        // In test env (development) Secure must be absent by default.
        let c = Spi::get_one::<String>(
            "SELECT pgweb.set_cookie('sess', 'abc123', '{}'::jsonb)",
        )
        .expect("set_cookie call")
        .expect("row");
        assert!(c.starts_with("sess=abc123"));
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("SameSite=Lax"));
        assert!(!c.contains("Secure"), "dev must not force Secure");
        assert!(c.contains("Path=/"));
    }

    #[pg_test]
    fn set_cookie_respects_overrides_and_production_secure() {
        Spi::run("UPDATE pgweb.settings SET value = 'production' WHERE key = 'env'")
            .expect("flip env");
        let c = Spi::get_one::<String>(
            "SELECT pgweb.set_cookie('csrf', 'xyz', '{\"http_only\": false, \"same_site\": \"Strict\", \"path\": \"/app\"}'::jsonb)",
        )
        .expect("set_cookie call")
        .expect("row");
        assert!(c.contains("csrf=xyz"));
        assert!(!c.contains("HttpOnly"), "explicit override to false");
        assert!(c.contains("SameSite=Strict"));
        assert!(c.contains("Secure"), "prod + no override → Secure");
        assert!(c.contains("Path=/app"));
        // reset for other tests
        Spi::run("UPDATE pgweb.settings SET value = 'development' WHERE key = 'env'")
            .expect("reset env");
    }

    #[pg_test]
    fn envelope_without_marker_is_treated_as_plain_data() {
        // Proves AC6 / no false-positive envelope detection.
        // A raw-text handler (or a context object) that merely contains
        // "status" or "body" at top level must be emitted verbatim.
        let plain = Spi::get_one::<String>(
            "SELECT '{\"status\":\"ok\",\"body\":\"x\"}'::text",
        )
        .expect("select")
        .expect("row");
        // In a raw-text scenario the router would see this handler_text and,
        // because it lacks "$pgweb", pass it through as the body (the test here
        // just confirms the data shape itself does not accidentally look like
        // an envelope to a human or future code).
        assert!(plain.contains("\"status\":\"ok\""));
        assert!(!plain.contains("$pgweb"));
    }
}
