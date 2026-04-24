//! Parses the bundled `examples/todo/pages/` tree through `paths::scan`
//! to keep the reference app honest: any future accidental rename,
//! flat `.html`, reserved stem, or orphaned template fails this test
//! instead of surprising someone at `pg-web push` time.

use std::path::PathBuf;

fn todo_pages() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/todo/pages")
}

#[test]
fn todo_pages_scans_cleanly() {
    let entries = pg_web_cli::paths::scan(&todo_pages())
        .expect("examples/todo/pages should parse under the current layout spec");

    // Collect as (method, route) for assertion — order-agnostic beyond that.
    let mut pairs: Vec<(String, String)> = entries
        .iter()
        .map(|e| (e.method.clone(), e.route.clone()))
        .collect();
    pairs.sort();

    assert_eq!(
        pairs,
        vec![
            // _404 stem becomes method='404' with path_pattern='/'.
            ("404".to_string(), "/".to_string()),
            ("GET".to_string(), "/".to_string()),
            // Dynamic route: [id] in the filesystem → :id in the pattern.
            ("GET".to_string(), "/todos/:id".to_string()),
            ("POST".to_string(), "/todos".to_string()),
            ("POST".to_string(), "/todos/delete".to_string()),
            ("POST".to_string(), "/todos/toggle".to_string()),
        ]
    );
}

#[test]
fn todo_pages_modes_are_as_documented() {
    let entries = pg_web_cli::paths::scan(&todo_pages()).unwrap();

    let by_key = |method: &str, route: &str| {
        entries
            .iter()
            .find(|e| e.method == method && e.route == route)
            .unwrap_or_else(|| panic!("expected {method} {route}"))
    };

    // Index: dynamic mode (both files).
    assert!(by_key("GET", "/").is_full());
    // GET /todos/:id: dynamic mode (detail-view template + handler using capture).
    assert!(by_key("GET", "/todos/:id").is_full());
    // POST /todos: dynamic mode (fragment template + handler).
    assert!(by_key("POST", "/todos").is_full());
    // POST /todos/toggle: dynamic mode (shared <li> template + handler).
    assert!(by_key("POST", "/todos/toggle").is_full());
    // POST /todos/delete: raw-text mode (no sibling .html).
    assert!(by_key("POST", "/todos/delete").is_raw_text());
    // _404: static mode (template only, no handler — push will synthesize).
    assert!(by_key("404", "/").is_static());
}

#[test]
fn todo_handler_names_match_spec() {
    let entries = pg_web_cli::paths::scan(&todo_pages()).unwrap();
    for e in &entries {
        let expected = match (e.method.as_str(), e.route.as_str()) {
            ("GET", "/") => "pgweb.pages__index",
            // [id] on disk → $id in the PG handler name to keep it visually
            // distinct from a literal directory named `id`.
            ("GET", "/todos/:id") => "pgweb.pages__todos__$id__index",
            ("POST", "/todos") => "pgweb.pages__todos__post",
            ("POST", "/todos/toggle") => "pgweb.pages__todos__toggle__post",
            ("POST", "/todos/delete") => "pgweb.pages__todos__delete__post",
            ("404", "/") => "pgweb.pages___404",
            other => panic!("unexpected route {other:?}"),
        };
        assert_eq!(
            e.handler_name, expected,
            "handler name for {} {}",
            e.method, e.route
        );
    }
}
