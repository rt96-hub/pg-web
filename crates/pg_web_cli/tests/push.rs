//! Hermetic tests for `pg-web push` — things that don't require a running DB.
//!
//! DB-integration tests are deferred until the `rtaylor96/pg-web:latest`
//! Docker image (M1.1 step 6) so they can spin up a hermetic container.
//! For now, verify pre-connection behavior: arg validation, file walking,
//! nice error messages.

use tempfile::tempdir;

#[test]
fn refuses_when_pages_dir_missing() {
    let dir = tempdir().unwrap();
    // app dir exists but has no pages/ subdir
    let result = pg_web_cli::push::push(dir.path(), "postgres://unused:5432/unused");
    let err = result.expect_err("push should fail without pages/");
    let msg = format!("{err:#}");
    assert!(msg.contains("pages/"), "error should mention missing pages/: {msg}");
    assert!(
        msg.contains("pg-web init"),
        "error should hint at `pg-web init`: {msg}"
    );
}

#[test]
fn refuses_bad_connection_url() {
    let dir = tempdir().unwrap();
    // Give it a pages/ dir so we get past the filesystem check and onto connect
    let app = dir.path().join("app");
    pg_web_cli::init::init(&app, "dummy", None).unwrap();

    // Bad host that should not resolve / connect
    let result = pg_web_cli::push::push(&app, "postgres://nobody@127.0.0.1:1/nope");
    assert!(result.is_err(), "connection to bogus URL should fail");
}
