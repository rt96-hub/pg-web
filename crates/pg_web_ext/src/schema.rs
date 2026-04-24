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
-- 2 MiB per file by a CHECK constraint so a runaway file doesn't wedge
-- the worker on read. Larger-file support via pg_largeobject with SPI
-- streaming is deferred to M1.4 — practical web assets (CSS / JS / small
-- icons) fit well under 2 MiB.
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
    CHECK (length(content) <= 2097152)
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
}
