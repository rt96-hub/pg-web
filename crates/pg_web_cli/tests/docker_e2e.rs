//! Tier 3 end-to-end test. Boots `pgweb/postgres:latest` in a container,
//! runs `migrate apply` + `push` against `examples/demo/`, exercises the
//! full CRUD flow over HTTP.
//!
//! Gated with `#[ignore]` so the default `cargo test` stays fast. The
//! script `scripts/test-all.sh` opts in via `-- --ignored` on a scoped
//! `--test docker_e2e` invocation.
//!
//! Preconditions (hard failure if missing):
//! - Docker daemon reachable (`docker --version` succeeds).
//! - Image `pgweb/postgres:latest` exists locally (`docker image inspect`).
//!
//! When those aren't satisfied, this test panics with instructions rather
//! than skipping — the Docker image is a shipped artifact, so silent-skip
//! would give false confidence.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::SyncRunner;
use testcontainers::{GenericImage, ImageExt};

const IMAGE: &str = "pgweb/postgres";
const TAG: &str = "latest";
const POSTGRES_PASSWORD: &str = "testpw";
const POSTGRES_DB: &str = "app";

/// Preflight: both Docker and the image must be present. Panic with a
/// clear remediation path if not.
fn preflight_or_panic() {
    let docker_ok = Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !docker_ok {
        panic!(
            "tier 3 E2E requires Docker. Install Docker and confirm \
             `docker --version` succeeds, then re-run."
        );
    }

    let image_ok = Command::new("docker")
        .args(["image", "inspect", &format!("{IMAGE}:{TAG}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !image_ok {
        panic!(
            "tier 3 E2E requires image `{IMAGE}:{TAG}`. Build it with:\n\
             \n    bash scripts/build-image.sh\n"
        );
    }
}

/// Poll `base_url/` until it returns any response (200 or 404 both count —
/// 404 before `push` is expected because the seeded route has been
/// replaced). Panic after the deadline.
fn wait_for_http(base_url: &str, deadline: Instant) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();
    loop {
        if let Ok(_resp) = client.get(format!("{base_url}/")).send() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("HTTP server not ready at {base_url} within deadline");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/demo")
}

#[test]
#[ignore = "tier 3 E2E — Docker + pgweb/postgres:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg_web_cli \
            --test docker_e2e -- --ignored`."]
fn full_todo_crud_flow() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        // Wait for Postgres to log its "ready to accept connections" message —
        // the extension's BGW binds :8080 shortly after; we poll :8080 below.
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));

    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start pgweb/postgres container");

    let pg_host_port = container
        .get_host_port_ipv4(5432)
        .expect("host port for 5432");
    let http_host_port = container
        .get_host_port_ipv4(8080)
        .expect("host port for 8080");

    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");

    wait_for_http(&base_url, Instant::now() + Duration::from_secs(30));

    // Apply migrations then push the demo app into the fresh DB.
    let demo = demo_dir();
    pg_web_cli::migrate::apply(&demo, &db_url).expect("migrate apply");
    pg_web_cli::push::push(&demo, &db_url).expect("push");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build http client");

    // --- Empty state ---
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("No todos yet"),
        "empty state expected, got: {body}"
    );

    // --- Create a todo ---
    let body = post_form(&client, &base_url, "/todos", "title=buy+milk");
    assert!(
        body.contains("buy milk"),
        "new <li> fragment should contain the title, got: {body}"
    );

    // The first insert gets id=1 on a clean DB.
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("buy milk"),
        "index should now include the new todo, got: {body}"
    );

    // --- Toggle ---
    let body = post_form(&client, &base_url, "/todos/toggle", "id=1");
    assert!(
        body.contains("done"),
        "toggle response should mark row done, got: {body}"
    );
    assert!(
        body.contains("buy milk"),
        "toggle response should retain title, got: {body}"
    );

    // --- Delete ---
    let resp = client
        .post(format!("{base_url}/todos/delete"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("id=1")
        .send()
        .expect("POST /todos/delete");
    assert_eq!(resp.status(), 200);
    let body = resp.text().unwrap();
    assert!(
        body.trim().is_empty(),
        "delete should return empty body, got: {body:?}"
    );

    // --- Back to empty ---
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("No todos yet"),
        "list should return to empty after delete, got: {body}"
    );

    // --- Custom 404 ---
    let resp = client
        .get(format!("{base_url}/this-definitely-does-not-exist"))
        .send()
        .expect("GET missing");
    assert_eq!(resp.status(), 404);
    let body = resp.text().unwrap();
    assert!(body.contains("404"), "custom 404 body: {body}");
    assert!(
        body.contains("Back to todos"),
        "custom 404 should link home: {body}"
    );
}

fn get(client: &reqwest::blocking::Client, base: &str, path: &str) -> String {
    let resp = client
        .get(format!("{base}{path}"))
        .send()
        .unwrap_or_else(|e| panic!("GET {path} failed: {e}"));
    assert_eq!(resp.status(), 200, "GET {path} status");
    resp.text().unwrap()
}

fn post_form(client: &reqwest::blocking::Client, base: &str, path: &str, body: &str) -> String {
    let resp = client
        .post(format!("{base}{path}"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.to_string())
        .send()
        .unwrap_or_else(|e| panic!("POST {path} failed: {e}"));
    assert_eq!(resp.status(), 200, "POST {path} status");
    resp.text().unwrap()
}
