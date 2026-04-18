//! Framework-owned schema installed on `CREATE EXTENSION`.
//!
//! Tables live under the `pgweb` schema (cannot use `pg_web` — Postgres
//! reserves schema names starting with `pg_`). The CLI writes rows; the
//! request handler reads them per-request via SPI.

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

COMMENT ON SCHEMA pgweb IS 'pg-web framework tables. Managed by the extension and CLI; do not modify directly.';
"#,
    name = "framework_tables",
    bootstrap,
);

#[cfg(any(test, feature = "pg_test"))]
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
    fn routes_table_is_empty() {
        let count = Spi::get_one::<i64>("SELECT COUNT(*) FROM pgweb.routes")
            .expect("count should not error")
            .expect("count should return a row");
        assert_eq!(count, 0);
    }

    #[pg_test]
    fn templates_table_is_empty() {
        let count = Spi::get_one::<i64>("SELECT COUNT(*) FROM pgweb.templates")
            .expect("count should not error")
            .expect("count should return a row");
        assert_eq!(count, 0);
    }

    #[pg_test]
    fn routes_table_accepts_insert_and_upsert() {
        Spi::run(
            "INSERT INTO pgweb.routes (path_pattern, handler_name, template_path) \
             VALUES ('/', 'home', 'pages/index.html')",
        )
        .expect("insert should succeed");

        let handler = Spi::get_one::<String>(
            "SELECT handler_name FROM pgweb.routes WHERE path_pattern = '/'",
        )
        .expect("select should not error")
        .expect("select should return a row");
        assert_eq!(handler, "home");
    }
}
