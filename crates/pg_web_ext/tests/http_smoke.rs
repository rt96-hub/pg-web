//! HTTP smoke test — hits a running pg-web worker on :8080.
//!
//! **Precondition:** Postgres must be running with `pg_web_ext` preloaded.
//! The repo's `scripts/test-http.sh` takes care of starting PG, polling
//! for :8080, running this test, and leaving PG running for dev.
//!
//! Running this test standalone:
//! ```
//! $HOME/.pgrx/17.9/pgrx-install/bin/pg_ctl -D $HOME/.pgrx/data-17 \
//!     -l $HOME/.pgrx/17.log start
//! cargo test --test http_smoke -p pg_web_ext --features pg17
//! ```

use std::time::Duration;

const BASE_URL: &str = "http://localhost:8080";

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("http client should build")
}

fn get(path: &str) -> reqwest::blocking::Response {
    client()
        .get(format!("{BASE_URL}{path}"))
        .send()
        .unwrap_or_else(|e| panic!(
            "HTTP request to {path} failed: {e}. \
             Is Postgres running with pg_web_ext preloaded? \
             Try: scripts/test-http.sh"
        ))
}

#[test]
fn root_renders_seeded_template() {
    // The install SQL seeds a route `GET /` → handler `pgweb.hello_handler` →
    // template `pages/index.html` with body `<h1>hello from {{ name }}</h1>`.
    // The handler returns `{"name": "pg-web"}`. So rendering produces HTML
    // containing `<h1>hello from pg-web</h1>`.
    let resp = get("/");
    assert_eq!(resp.status(), 200);
    let ctype = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(ctype.starts_with("text/html"), "unexpected content-type: {ctype}");
    let body = resp.text().unwrap();
    assert!(
        body.contains("<h1>hello from pg-web</h1>"),
        "expected rendered template in body, got: {body}"
    );
}

#[test]
fn unknown_route_returns_404() {
    let resp = get("/definitely/not/a/real/path");
    assert_eq!(resp.status(), 404);
}
