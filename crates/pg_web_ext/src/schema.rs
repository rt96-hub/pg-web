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
}
