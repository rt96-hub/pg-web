//! Integration tests for `pg-web init`.

use std::fs;

use tempfile::tempdir;

#[test]
fn creates_all_expected_paths() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("my-test-app");
    pg_web_cli::init::init(&app, "my-test-app").expect("init should succeed");
    for rel in pg_web_cli::init::scaffolded_paths() {
        let p = app.join(rel);
        assert!(p.exists(), "expected {} to exist", p.display());
    }
    assert!(app.join("public").is_dir());
    assert!(app.join("migrations").is_dir());
}

#[test]
fn app_name_is_interpolated_into_sql_handler() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("my-cool-blog");
    pg_web_cli::init::init(&app, "my-cool-blog").unwrap();
    let sql = fs::read_to_string(app.join("pages/index.sql")).unwrap();
    assert!(sql.contains("'my-cool-blog'"), "sql should contain literal app name: {sql}");
    assert!(!sql.contains("{APP}"), "placeholder should have been substituted: {sql}");
}

#[test]
fn refuses_existing_directory() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("already-here");
    fs::create_dir(&app).unwrap();
    let result = pg_web_cli::init::init(&app, "already-here");
    assert!(result.is_err(), "init should refuse to overwrite an existing dir");
}

#[test]
fn refuses_empty_name() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("unused");
    let result = pg_web_cli::init::init(&app, "");
    assert!(result.is_err(), "init should refuse an empty app name");
}

#[test]
fn html_template_contains_tera_placeholders() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("placeholder-check");
    pg_web_cli::init::init(&app, "placeholder-check").unwrap();
    let html = fs::read_to_string(app.join("pages/index.html")).unwrap();
    assert!(html.contains("{{ title }}"), "template should contain {{{{ title }}}} placeholder");
    assert!(html.contains("{{ app_name }}"));
}

#[test]
fn pgweb_toml_has_expected_keys() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("config-check");
    pg_web_cli::init::init(&app, "config-check").unwrap();
    let toml = fs::read_to_string(app.join("pgweb.toml")).unwrap();
    assert!(toml.contains("[server]"));
    assert!(toml.contains("port = 8080"));
    assert!(toml.contains("env  = \"development\""));
}
