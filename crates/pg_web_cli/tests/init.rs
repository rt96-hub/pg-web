//! Integration tests for `pg-web init`.

use std::fs;

use tempfile::tempdir;

#[test]
fn creates_all_expected_paths() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("my-test-app");
    pg_web_cli::init::init(&app, "my-test-app", None).expect("init should succeed");
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
    pg_web_cli::init::init(&app, "my-cool-blog", None).unwrap();
    let sql = fs::read_to_string(app.join("pages/index.sql")).unwrap();
    assert!(sql.contains("'my-cool-blog'"), "sql should contain literal app name: {sql}");
    assert!(!sql.contains("{APP}"), "placeholder should have been substituted: {sql}");
}

#[test]
fn refuses_existing_directory() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("already-here");
    fs::create_dir(&app).unwrap();
    let result = pg_web_cli::init::init(&app, "already-here", None);
    assert!(result.is_err(), "init should refuse to overwrite an existing dir");
}

#[test]
fn refuses_empty_name() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("unused");
    let result = pg_web_cli::init::init(&app, "", None);
    assert!(result.is_err(), "init should refuse an empty app name");
}

#[test]
fn html_template_contains_tera_placeholders() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("placeholder-check");
    pg_web_cli::init::init(&app, "placeholder-check", None).unwrap();
    let html = fs::read_to_string(app.join("pages/index.html")).unwrap();
    assert!(html.contains("{{ title }}"), "template should contain {{{{ title }}}} placeholder");
    assert!(html.contains("{{ app_name }}"));
}

#[test]
fn pgweb_toml_has_expected_keys() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("config-check");
    pg_web_cli::init::init(&app, "config-check", None).unwrap();
    let toml = fs::read_to_string(app.join("pgweb.toml")).unwrap();
    assert!(toml.contains("[server]"));
    assert!(toml.contains("port = 8080"));
    assert!(toml.contains("env  = \"development\""));
}

#[test]
fn minimal_init_scaffolds_readme_with_app_name() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("readme-check");
    pg_web_cli::init::init(&app, "readme-check", None).unwrap();
    let readme = fs::read_to_string(app.join("README.md")).unwrap();
    assert!(readme.starts_with("# readme-check"), "README should title with app name: {readme}");
    assert!(
        readme.contains("pg-web dev"),
        "README should point at the dev workflow: {readme}"
    );
    assert!(
        readme.contains("--template todo"),
        "README should mention the richer template as a next step: {readme}"
    );
    assert!(
        !readme.contains("{APP}"),
        "placeholder should have been substituted: {readme}"
    );
}

#[test]
fn template_todo_extracts_expected_tree() {
    // `--template todo` should materialize the full todo-list tree —
    // same files the todo_layout tests + docker_e2e flow expect to
    // find. If a new file gets added to examples/todo/, this test's
    // keeper list will start flagging it as missing, which is a
    // feature: the scaffold and the repo's reference app stay in lockstep.
    let dir = tempdir().unwrap();
    let app = dir.path().join("my-todos");
    pg_web_cli::init::init(&app, "my-todos", Some("todo")).expect("template init");

    for rel in [
        "pages/index.html",
        "pages/index.sql",
        "pages/_404.html",
        "pages/todos/post.html",
        "pages/todos/post.sql",
        "pages/todos/toggle/post.html",
        "pages/todos/toggle/post.sql",
        "pages/todos/[id]/index.html",
        "pages/todos/[id]/index.sql",
        // 017-A: real method DELETE under the dynamic capture dir (replaces old /todos/delete post workaround)
        "pages/todos/[id]/delete.sql",
        "migrations/0001_create_todos.sql",
        "public/styles.css",
        "pgweb.toml",
        "docker-compose.yml",
        "Caddyfile",
        ".gitignore",
        "README.md",
    ] {
        assert!(
            app.join(rel).exists(),
            "template demo should have produced {rel}"
        );
    }
}

#[test]
fn template_todo_readme_is_app_facing_not_repo_facing() {
    // The bundled examples/todo/README.md targets pg-web repo
    // maintainers — it references `../../target/debug/pg-web` etc.
    // That's useless at an app root. init_from_template skips the
    // bundled README and writes templates::README_TODO in its place.
    let dir = tempdir().unwrap();
    let app = dir.path().join("readme-swap");
    pg_web_cli::init::init(&app, "readme-swap", Some("todo")).unwrap();
    let readme = fs::read_to_string(app.join("README.md")).unwrap();
    assert!(
        readme.starts_with("# readme-swap"),
        "README should lead with the app name, not the demo's hardcoded heading: {readme}"
    );
    assert!(
        !readme.contains("../../target/debug/pg-web"),
        "README should not carry the repo-facing invocation paths: {readme}"
    );
    assert!(
        readme.contains("pg-web migrate apply"),
        "README should document the demo's migrate-apply step: {readme}"
    );
}

#[test]
fn template_todo_includes_validation_handler() {
    // Component B's post.sql catches check_violation — the scaffolded
    // tree must carry that live, not a pre-Component-B snapshot.
    let dir = tempdir().unwrap();
    let app = dir.path().join("validation-check");
    pg_web_cli::init::init(&app, "validation-check", Some("todo")).unwrap();
    let post_sql = fs::read_to_string(app.join("pages/todos/post.sql")).unwrap();
    assert!(
        post_sql.contains("EXCEPTION WHEN check_violation"),
        "demo template should ship the validation pattern: {post_sql}"
    );
}

#[test]
fn unknown_template_errors_with_available_list() {
    let dir = tempdir().unwrap();
    let app = dir.path().join("nope");
    let err = pg_web_cli::init::init(&app, "nope", Some("nonexistent"))
        .expect_err("unknown template should error");
    let msg = err.to_string();
    assert!(msg.contains("unknown template"), "err = {msg}");
    assert!(msg.contains("nonexistent"), "err should echo the bad name: {msg}");
    assert!(
        msg.contains("todo"),
        "err should list available templates: {msg}"
    );
    assert!(
        !app.exists(),
        "no directory should be created when the template lookup fails"
    );
}

#[test]
fn available_templates_lists_todo() {
    let list = pg_web_cli::init::available_templates();
    assert!(list.contains(&"todo"), "todo should be available: {list:?}");
}
