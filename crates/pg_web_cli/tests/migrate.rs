//! Hermetic tests for `pg-web migrate apply` — things that don't need a
//! running DB. Full-DB idempotency gets exercised by the Docker E2E tier.

use std::fs;

use tempfile::tempdir;

#[test]
fn refuses_when_migrations_dir_missing() {
    let dir = tempdir().unwrap();
    // app dir exists but has no migrations/ subdir
    let result = pg_web_cli::migrate::apply(dir.path(), "postgres://unused:5432/unused");
    let err = result.expect_err("migrate apply should fail without migrations/");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("migrations/"),
        "error should mention missing migrations/: {msg}"
    );
    assert!(
        msg.contains("pg-web init"),
        "error should hint at `pg-web init`: {msg}"
    );
}

#[test]
fn refuses_bad_connection_url() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("app");
    pg_web_cli::init::init(&app, "dummy", None).unwrap();
    // seed a migration file so we get past discovery to the connect step
    fs::write(app.join("migrations/0001_init.sql"), "SELECT 1;").unwrap();

    let result = pg_web_cli::migrate::apply(&app, "postgres://nobody@127.0.0.1:1/nope");
    assert!(result.is_err(), "connection to bogus URL should fail");
}

#[test]
fn discover_picks_up_init_scaffolded_gitkeep_without_treating_it_as_migration() {
    // `pg-web init` scaffolds `migrations/.gitkeep`. discover() should ignore it.
    let dir = tempdir().unwrap();
    let app = dir.path().join("app");
    pg_web_cli::init::init(&app, "scaffold-test", None).unwrap();
    let found = pg_web_cli::migrate::discover(&app.join("migrations")).unwrap();
    assert!(
        found.is_empty(),
        "fresh scaffold has no migrations, got {found:?}"
    );
}
