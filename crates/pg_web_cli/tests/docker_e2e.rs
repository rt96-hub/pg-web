//! Tier 3 end-to-end test. Boots the current test image (`rtaylor96/pg-web:latest`
//! while the permanent `pgweb/postgres` namespace is pending) in a container,
//! runs `migrate apply` + `push` against `examples/todo/`, exercises the
//! full CRUD flow over HTTP.
//!
//! Gated with `#[ignore]` so the default `cargo test` stays fast. The
//! script `scripts/test-all.sh` opts in via `-- --ignored` on a scoped
//! `--test docker_e2e` invocation.
//!
//! Preconditions (hard failure if missing):
//! - Docker daemon reachable (`docker --version` succeeds).
//! - Image `rtaylor96/pg-web:latest` exists locally (`docker image inspect`).
//!
//! When those aren't satisfied, this test panics with instructions rather
//! than skipping — the Docker image is a shipped artifact, so silent-skip
//! would give false confidence.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use testcontainers::core::{ExecCommand, IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::SyncRunner;
use testcontainers::{GenericImage, ImageExt};

const IMAGE: &str = "rtaylor96/pg-web";
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
             \n    PGWEB_IMAGE=rtaylor96/pg-web:latest bash scripts/build-image.sh\n"
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

fn todo_app_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/todo")
}

#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
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
        .expect("start test image container (rtaylor96/pg-web)");

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

    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    // Apply migrations then push the demo app into the fresh DB.
    let todo_app = todo_app_dir();
    pg_web_cli::migrate::apply(&todo_app, &db_url).expect("migrate apply");
    pg_web_cli::push::push(&todo_app, &db_url).expect("push");

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
    // Successful insert clears any prior error via an OOB-swapped empty
    // #form-error div (see pages/todos/post.html).
    assert!(
        body.contains(r#"id="form-error""#) && body.contains(r#"hx-swap-oob="true""#),
        "success response should include the OOB-clear div, got: {body}"
    );

    // The first insert gets id=1 on a clean DB.
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("buy milk"),
        "index should now include the new todo, got: {body}"
    );

    // --- Validation UX: empty / whitespace-only title → inline error ---
    // The table's CHECK (length(trim(title)) > 0) is caught by the
    // handler's EXCEPTION WHEN check_violation; response is 200 with an
    // OOB error fragment rather than 500 + dev error page. Exercises
    // M1.4 Component B (form-validation pattern).
    let body = post_form(&client, &base_url, "/todos", "title=");
    assert!(
        body.contains("Title cannot be empty"),
        "empty title should return inline error, got: {body}"
    );
    assert!(
        body.contains(r#"id="form-error""#) && body.contains(r#"hx-swap-oob="true""#),
        "error response should use OOB swap to #form-error, got: {body}"
    );
    assert!(
        !body.contains("PGWEB_E003"),
        "empty title must NOT surface the dev error page, got: {body}"
    );
    assert!(
        !body.contains("internal server error"),
        "empty title must NOT surface a generic 500, got: {body}"
    );

    // Whitespace-only title should also trip the CHECK — `trim()` in the
    // handler collapses "   " to "" before insert.
    let body = post_form(&client, &base_url, "/todos", "title=+++");
    assert!(
        body.contains("Title cannot be empty"),
        "whitespace-only title should also return inline error, got: {body}"
    );
    assert!(
        !body.contains("PGWEB_E003"),
        "whitespace-only title must NOT surface the dev error page, got: {body}"
    );

    // Verify the failed inserts didn't leave orphan rows. An empty-title
    // row would render as <span class="title"></span>; the existing
    // "buy milk" row has content there. Matching the empty-span form is
    // the tight signal for "the CHECK actually blocked the insert."
    let body = get(&client, &base_url, "/");
    assert!(
        !body.contains(r#"<span class="title"></span>"#),
        "failed inserts should not create empty-title rows, got: {body}"
    );

    // --- Dynamic route: /todos/:id detail view ---
    // Numeric id → actual row. Exercises path_params populated by the
    // router and the capture-named SQL handler pgweb.pages__todos__$id__index.
    let body = get(&client, &base_url, "/todos/1");
    assert!(
        body.contains("todo #1"),
        "detail view should show the numeric id, got: {body}"
    );
    assert!(
        body.contains("buy milk"),
        "detail view should show the title, got: {body}"
    );
    assert!(
        body.contains("pending"),
        "detail view should show pre-toggle status, got: {body}"
    );

    // Non-numeric id → matches the capture but no DB row; falls through
    // to the "not found" branch in the template. Proves captures accept
    // any URL segment; the handler decides what's valid. Tera auto-escapes
    // the captured id inside {{ id }}, so the quoted id in the template
    // shows up as &quot;all&quot; in the HTML body.
    let body = get(&client, &base_url, "/todos/all");
    assert!(
        body.contains("not found") && body.contains("&quot;all&quot;"),
        "detail view for non-numeric id should render not-found with the echoed id, got: {body}"
    );

    // Non-existent numeric id → also not-found.
    let body = get(&client, &base_url, "/todos/999");
    assert!(
        body.contains("not found"),
        "detail view for missing id should render not-found, got: {body}"
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

/// Tier 3 Component G coverage: the livereload chain end-to-end.
///
/// 1. Pushed app has the `<script src="/_pgweb/livereload.js">` tag
///    injected in dev mode.
/// 2. `GET /_pgweb/livereload.js` returns the JS stub content.
/// 3. Open an SSE connection to `/_pgweb/livereload`; issue `NOTIFY
///    pgweb_livereload` via a direct PG connection (standing in for
///    `pg-web dev`'s post-push hook); assert the SSE stream carries
///    the payload.
///
/// Runs in one container to amortize startup cost.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn livereload_sse_chain_end_to_end() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());
    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    pg_web_cli::push::push(tmp.path(), &db_url).expect("push");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    // 1. Script is auto-injected in dev mode.
    let home = get(&client, &base_url, "/");
    assert!(
        home.contains("data-pgweb-livereload"),
        "rendered HTML should have the livereload script injected: {home}"
    );
    assert!(
        home.contains("/_pgweb/livereload.js"),
        "injected script should point at /_pgweb/livereload.js: {home}"
    );

    // 2. The JS stub is served.
    let js = get(&client, &base_url, "/_pgweb/livereload.js");
    assert!(
        js.contains("EventSource") && js.contains("/_pgweb/livereload"),
        "livereload.js should contain an EventSource to /_pgweb/livereload: {js}"
    );

    // 3. Open an SSE stream on a background thread, fire NOTIFY from
    //    this thread, confirm the event body lands.
    //
    // Use a non-blocking reqwest Response with a bounded read. The test
    // runs quickly; no need for sophisticated stream parsing — we just
    // scan the first N bytes for the expected event/data frame.
    use std::io::Read;
    use std::sync::mpsc;
    use std::thread;

    let (tx, rx) = mpsc::channel::<String>();
    let sse_url = format!("{base_url}/_pgweb/livereload");
    let client_for_sse = client.clone();
    thread::spawn(move || {
        let resp = client_for_sse
            .get(&sse_url)
            .send()
            .expect("open SSE stream");
        assert_eq!(resp.status(), 200, "SSE endpoint should be 200 in dev mode");
        assert!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.starts_with("text/event-stream"))
                .unwrap_or(false),
            "SSE endpoint should return text/event-stream content-type"
        );
        // Read up to 512 bytes — enough to capture the full event frame.
        let mut body = resp;
        let mut buf = [0u8; 512];
        let n = body.read(&mut buf).unwrap_or(0);
        let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
    });

    // Give the SSE handler a tick to register + issue LISTEN on the BGW
    // side. 500 ms is comfortably more than the typical broadcast-
    // subscribe round-trip + NOTIFY delivery.
    thread::sleep(Duration::from_millis(500));

    // Fire NOTIFY from a separate PG connection, standing in for what
    // `pg-web dev`'s post-push hook will do in production.
    let mut pg = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
    pg.batch_execute(r#"NOTIFY pgweb_livereload, '{"kind":"css"}'"#)
        .expect("NOTIFY");

    // Wait for the SSE thread to report what it read. 3 s buffer for
    // slow CI — actual delivery takes single-digit ms.
    let chunk = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("SSE thread never delivered bytes");

    assert!(
        chunk.contains("event: reload"),
        "SSE stream should carry the `reload` event type: {chunk:?}"
    );
    assert!(
        chunk.contains("\"kind\":\"css\""),
        "SSE stream should carry the NOTIFY payload: {chunk:?}"
    );
}

/// Tier 3 F.1 coverage: migration gate, `--with-migrate`, `--dry-run`,
/// and the `pgweb.deployments` ledger. One container, multiple pushes,
/// asserts DB-side state between each. Kept as one test to amortize the
/// container spin-up cost; the assertions still make each F.1 invariant
/// explicit.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn push_f1_dry_run_with_migrate_and_deployments() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());
    let app_dir = tmp.path();

    let mut pg = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();

    // Assert fresh install has an empty deployments ledger.
    let row: i64 = pg
        .query_one("SELECT COUNT(*) FROM pgweb.deployments", &[])
        .unwrap()
        .get(0);
    assert_eq!(row, 0, "deployments ledger should be empty on fresh DB");

    // --- 1. Plain push() refuses when migrations are pending. ----------
    let err = pg_web_cli::push::push(app_dir, &db_url)
        .expect_err("plain push should refuse pending migrations");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("pending migrations"),
        "error should name the situation: {msg}"
    );
    assert!(
        msg.contains("0001_create_todos.sql"),
        "error should list the pending filename(s): {msg}"
    );
    assert!(
        msg.contains("--with-migrate"),
        "error should point at the fix flag: {msg}"
    );

    // Nothing was inserted — ledger still empty.
    let row: i64 = pg
        .query_one("SELECT COUNT(*) FROM pgweb.deployments", &[])
        .unwrap()
        .get(0);
    assert_eq!(row, 0, "failed push must not insert a deployments row");

    // --- 2. push --with-migrate applies + pushes in one call. ----------
    let summary = pg_web_cli::push::push_with_options(
        app_dir,
        &db_url,
        pg_web_cli::push::PushOptions {
            with_migrate: true,
            dry_run: false,
        },
    )
    .expect("push --with-migrate");
    assert_eq!(summary.migrations_applied, 1);
    assert_eq!(
        summary.migrations_applied_names,
        vec!["0001_create_todos.sql".to_string()]
    );
    assert!(!summary.dry_run);
    assert!(summary.routes_upserted >= 1);

    // Ledger has exactly one row with sane values.
    let row = pg
        .query_one(
            "SELECT file_count, migrations_applied, from_host \
             FROM pgweb.deployments ORDER BY id DESC LIMIT 1",
            &[],
        )
        .unwrap();
    let file_count: i32 = row.get(0);
    let migrations_applied: i32 = row.get(1);
    let from_host: Option<String> = row.get(2);
    assert!(file_count > 0, "file_count should include demo files");
    assert_eq!(migrations_applied, 1);
    assert!(from_host.is_some(), "from_host should be captured");

    // --- 3. Second real push logs a second ledger row. -----------------
    let summary = pg_web_cli::push::push(app_dir, &db_url).expect("second push");
    assert_eq!(summary.migrations_applied, 0, "no pending migrations now");
    let count: i64 = pg
        .query_one("SELECT COUNT(*) FROM pgweb.deployments", &[])
        .unwrap()
        .get(0);
    assert_eq!(count, 2, "second push inserts a second ledger row");

    // --- 4. Dry-run push rolls back the ledger insert too. --------------
    let summary = pg_web_cli::push::push_with_options(
        app_dir,
        &db_url,
        pg_web_cli::push::PushOptions {
            dry_run: true,
            with_migrate: false,
        },
    )
    .expect("dry-run push");
    assert!(summary.dry_run);
    let count: i64 = pg
        .query_one("SELECT COUNT(*) FROM pgweb.deployments", &[])
        .unwrap()
        .get(0);
    assert_eq!(
        count, 2,
        "dry-run must NOT add a deployments row — transaction rolled back"
    );

    // --- 5. Dry-run with pending migrations reports without applying. ---
    // Add a fake second migration to disk. Dry-run + with-migrate
    // should say "would apply" and not touch pgweb.migrations.
    let new_mig = app_dir
        .join("migrations")
        .join("0002_add_column.sql");
    fs::write(
        &new_mig,
        "ALTER TABLE public.todos ADD COLUMN description text;",
    )
    .unwrap();

    let summary = pg_web_cli::push::push_with_options(
        app_dir,
        &db_url,
        pg_web_cli::push::PushOptions {
            with_migrate: true,
            dry_run: true,
        },
    )
    .expect("dry-run with_migrate push");
    assert!(summary.dry_run);
    assert_eq!(summary.migrations_applied, 1, "reports the would-apply");
    assert_eq!(
        summary.migrations_applied_names,
        vec!["0002_add_column.sql".to_string()]
    );

    // pgweb.migrations should still have only 0001 — 0002 was NOT applied.
    let rows = pg
        .query("SELECT name FROM pgweb.migrations ORDER BY name", &[])
        .unwrap();
    let names: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
    assert_eq!(
        names,
        vec!["0001_create_todos.sql".to_string()],
        "dry-run + with-migrate must NOT actually apply migrations"
    );

    // And the todos table still doesn't have a description column.
    let col: Option<String> = pg
        .query_opt(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = 'todos' \
               AND column_name = 'description'",
            &[],
        )
        .unwrap()
        .map(|r| r.get(0));
    assert!(
        col.is_none(),
        "dry-run must leave the target schema untouched"
    );
}

/// Recursively copy a directory tree, preserving structure. Keeps the
/// watcher test self-contained — we copy `examples/todo` into a tempdir
/// so mutations during the test never touch the checked-in source.
fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

/// Tier 3 watcher test. Starts `dev::watch` against a fresh copy of the
/// demo, then edits `pages/index.html` and polls HTTP until the new
/// content is served — validating the full pipeline: notify watcher →
/// 200ms debounce → Blake3 dedupe (hash map empty → first-pass change) →
/// classify (pages/*.html → Push) → push::push → BGW serves updated HTML.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn dev_watcher_repushes_on_save() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));

    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");

    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");

    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    // Copy examples/todo to a tempdir so edits don't touch the checked-in source.
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());

    // Initial schema + push — matches the normal `pg-web migrate apply && pg-web push` flow.
    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    pg_web_cli::push::push(tmp.path(), &db_url).expect("initial push");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build http client");

    // Baseline — pre-edit, the demo renders "No todos yet".
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("No todos yet"),
        "baseline render should have empty-state text, got: {body}"
    );

    // Spawn the watcher loop in a thread. `watch` drops back to the main
    // thread when `stop` flips true.
    let stop = Arc::new(AtomicBool::new(false));
    let watch_dir = tmp.path().to_path_buf();
    let watch_url = db_url.clone();
    let watch_stop = stop.clone();
    // livereload=true so the same path production uses is exercised —
    // the LISTEN task is env=development-gated in the extension, so
    // even if the NOTIFY fires without a listener this just logs.
    let handle = std::thread::spawn(move || {
        pg_web_cli::dev::watch(&watch_dir, &watch_url, watch_stop, true)
    });

    // Let the watcher install its fs hooks before we edit. 250ms > 200ms
    // debounce window so the first event we want to catch is the edit.
    std::thread::sleep(Duration::from_millis(250));

    // Edit pages/index.html — inject a unique marker in place of the
    // empty-state text so we know the new template was re-synced.
    const MARKER: &str = "WATCHER_E2E_MARKER_8f3c7a";
    let index_html = tmp.path().join("pages/index.html");
    let before = fs::read_to_string(&index_html).unwrap();
    let after = before.replace("No todos yet. Add one above.", MARKER);
    assert_ne!(before, after, "marker replacement should have matched");
    fs::write(&index_html, &after).unwrap();

    // Poll HTTP until the new marker shows up in the rendered body.
    // Deadline covers: debounce (200ms) + push (≪1s) + any HTTP cache lag.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let body = get(&client, &base_url, "/");
        if body.contains(MARKER) {
            break;
        }
        if Instant::now() >= deadline {
            panic!("watcher didn't re-push within 10s; last body: {body}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Shutdown. watch() polls `stop` every 500ms, so join latency is
    // bounded by SHUTDOWN_POLL.
    stop.store(true, Ordering::SeqCst);
    handle.join().expect("watcher thread panic").expect("watcher returned Err");
}

/// Tier 3 reconciliation test. Pushes an app with an extra route, then
/// deletes that route's files on disk and pushes again. The second push
/// must: (a) report non-zero routes_deleted / templates_deleted /
/// handlers_dropped, (b) return 404 on the deleted path, (c) remove the
/// handler function from `pg_proc`. The handler-function drop is what
/// proves the reserved `pgweb.pages__*(json)` namespace is owned by push.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn push_reconciles_deleted_files() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    // Copy the demo and add an extra route we can later delete.
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());

    let extra_dir = tmp.path().join("pages/extra");
    fs::create_dir_all(&extra_dir).unwrap();
    fs::write(
        extra_dir.join("index.html"),
        "<p>extra: {{ value }}</p>\n",
    )
    .unwrap();
    fs::write(
        extra_dir.join("index.sql"),
        "CREATE OR REPLACE FUNCTION pgweb.pages__extra__index(req json) RETURNS json \
         LANGUAGE sql IMMUTABLE AS $$ SELECT json_build_object('value', 'hello') $$;\n",
    )
    .unwrap();

    // First push: migrate + push from the modified copy. Extra route now live.
    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    let first = pg_web_cli::push::push(tmp.path(), &db_url).expect("initial push");
    assert!(first.routes_upserted >= 1);
    assert_eq!(first.routes_deleted, 0, "nothing to delete on first push");

    // Confirm the extra route renders.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body = get(&client, &base_url, "/extra");
    assert!(
        body.contains("extra: hello"),
        "extra route should render its template, got: {body}"
    );

    // Delete the extra route from disk, then push again.
    fs::remove_dir_all(&extra_dir).unwrap();
    let second = pg_web_cli::push::push(tmp.path(), &db_url).expect("reconcile push");
    assert_eq!(
        second.routes_deleted, 1,
        "reconcile should delete 1 stale route, got summary {second:?}"
    );
    assert_eq!(
        second.templates_deleted, 1,
        "reconcile should delete 1 stale template, got summary {second:?}"
    );
    assert_eq!(
        second.handlers_dropped, 1,
        "reconcile should drop 1 stale handler, got summary {second:?}"
    );

    // /extra now returns the custom 404.
    let resp = client.get(format!("{base_url}/extra")).send().unwrap();
    assert_eq!(resp.status(), 404, "deleted route should 404");

    // The handler function is gone from pg_proc too.
    let mut pg = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
    let row = pg
        .query_opt(
            "SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'pgweb' AND p.proname = 'pages__extra__index'",
            &[],
        )
        .unwrap();
    assert!(
        row.is_none(),
        "pgweb.pages__extra__index should be dropped after reconcile"
    );

    // The demo's other routes still serve — reconciliation didn't over-delete.
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("No todos yet"),
        "surviving GET / should still render, got: {body}"
    );
}

/// Tier 3 push-time template validation. Drop a .html with an unclosed
/// `{% if %}` block into `pages/` and push — the pre-DB Tera parse check
/// must reject it with the file path in the error, without touching
/// the live extension's state.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn push_rejects_broken_tera_template() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());

    // Prime: apply migrations + initial good push so there's live state.
    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    pg_web_cli::push::push(tmp.path(), &db_url).expect("initial good push");

    // Inject a broken template under a new route.
    let broken_dir = tmp.path().join("pages/mangled");
    fs::create_dir_all(&broken_dir).unwrap();
    fs::write(
        broken_dir.join("index.html"),
        "{% if whatever %}\n<p>no endif",
    )
    .unwrap();
    fs::write(
        broken_dir.join("index.sql"),
        "CREATE OR REPLACE FUNCTION pgweb.pages__mangled__index(req json) RETURNS json AS $$ \
         SELECT '{}'::json $$ LANGUAGE sql STABLE;\n",
    )
    .unwrap();

    let err = pg_web_cli::push::push(tmp.path(), &db_url)
        .expect_err("push should refuse a broken Tera template");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("mangled/index.html") || msg.contains("mangled\\index.html"),
        "error should name the file, got: {msg}"
    );
    assert!(
        msg.to_lowercase().contains("tera") || msg.to_lowercase().contains("parse"),
        "error should flag as a template parse issue, got: {msg}"
    );

    // Live site untouched — the initial-push state still serves.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("No todos yet"),
        "rolled-back push should leave the prior template intact, got: {body}"
    );
}

/// Tier 3 dev error page. Register a handler that raises at runtime
/// (division by zero), set env=development, hit the route, and assert
/// the response is the rich dev page (code, title, SQLSTATE, req dump).
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn dev_error_page_surfaces_sql_exception_detail() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());

    // Stamp a deliberately-exploding handler onto a fresh route. Full-mode
    // (.html + .sql) so we go through the JSON → Tera pipeline — proving
    // the error surfaces before template rendering even gets involved.
    let boom_dir = tmp.path().join("pages/boom");
    fs::create_dir_all(&boom_dir).unwrap();
    fs::write(
        boom_dir.join("index.html"),
        "<p>will never render</p>\n",
    )
    .unwrap();
    fs::write(
        boom_dir.join("index.sql"),
        "CREATE OR REPLACE FUNCTION pgweb.pages__boom__index(req json) RETURNS json AS $$ \
         SELECT json_build_object('x', 1 / 0) $$ LANGUAGE sql;\n",
    )
    .unwrap();

    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    pg_web_cli::push::push(tmp.path(), &db_url).expect("push with boom route");

    // Ensure env=development (the install-SQL seed default; belt-and-suspenders).
    let mut pg = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
    pg.execute(
        "UPDATE pgweb.settings SET value = 'development' WHERE key = 'env'",
        &[],
    )
    .unwrap();

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client.get(format!("{base_url}/boom")).send().unwrap();
    assert_eq!(resp.status(), 500, "handler error should surface as 500");
    let body = resp.text().unwrap();
    assert!(
        body.contains("PGWEB_E003_HANDLER_SQL_EXCEPTION"),
        "dev page should include the error code, got: {body}"
    );
    assert!(
        body.contains("SQL exception inside handler"),
        "dev page should include the title, got: {body}"
    );
    assert!(
        body.contains("22012"),
        "dev page should include the SQLSTATE for division_by_zero, got: {body}"
    );
    assert!(
        body.contains("How to fix"),
        "dev page should include the remedy section, got: {body}"
    );
    assert!(
        body.contains("pgweb.pages__boom__index"),
        "dev page should name the handler, got: {body}"
    );

    // Flip to production and confirm the generic 500 hides internals.
    pg.execute(
        "UPDATE pgweb.settings SET value = 'production' WHERE key = 'env'",
        &[],
    )
    .unwrap();
    let resp = client.get(format!("{base_url}/boom")).send().unwrap();
    assert_eq!(resp.status(), 500);
    let body = resp.text().unwrap();
    assert!(
        !body.contains("PGWEB_E003"),
        "prod body must not leak error codes, got: {body}"
    );
    assert!(
        !body.contains("SQLSTATE"),
        "prod body must not leak SQLSTATE, got: {body}"
    );
    assert!(
        body.contains("internal server error"),
        "prod body should be the generic message, got: {body}"
    );
}

/// Tier 3 static-asset flow. Pushes the demo (which now ships a
/// `public/styles.css`), then:
/// - GET /styles.css returns 200 with content-type text/css, cached
///   headers (ETag + Cache-Control), and the stylesheet bytes.
/// - Re-requesting with the advertised ETag in `If-None-Match` returns
///   304 Not Modified with no body.
/// - Deleting the file and re-pushing removes the asset from the DB;
///   the next request 404s.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn static_asset_serves_with_etag_and_revalidates() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());

    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    let summary = pg_web_cli::push::push(tmp.path(), &db_url).expect("push");
    assert!(
        summary.assets_upserted >= 1,
        "push should have synced at least one asset (demo ships public/styles.css), got summary {summary:?}"
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: full asset.
    let resp = client.get(format!("{base_url}/styles.css")).send().unwrap();
    assert_eq!(resp.status(), 200, "asset should serve 200");
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ctype.starts_with("text/css"),
        "content-type should be text/css, got {ctype}"
    );
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        etag.starts_with('"') && etag.ends_with('"') && etag.len() > 2,
        "ETag should be a non-empty double-quoted string, got {etag}"
    );
    assert!(
        resp.headers().get("cache-control").is_some(),
        "Cache-Control should be set"
    );
    let body = resp.text().unwrap();
    assert!(
        body.contains("font-family") && body.contains("system-ui"),
        "CSS body should contain the stylesheet content, got first 80 bytes: {}",
        &body.chars().take(80).collect::<String>()
    );

    // Revalidation with matching ETag: 304, no body.
    let resp = client
        .get(format!("{base_url}/styles.css"))
        .header("If-None-Match", &etag)
        .send()
        .unwrap();
    assert_eq!(resp.status(), 304, "matching If-None-Match should 304");
    let body = resp.text().unwrap();
    assert!(
        body.is_empty(),
        "304 body should be empty, got: {body:?}"
    );

    // Mismatched If-None-Match: full body again.
    let resp = client
        .get(format!("{base_url}/styles.css"))
        .header("If-None-Match", "\"not-the-etag\"")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200, "non-matching If-None-Match → 200");

    // Delete the file from disk, re-push, the asset row should be reconciled
    // away and the request should 404.
    std::fs::remove_file(tmp.path().join("public/styles.css")).unwrap();
    let summary = pg_web_cli::push::push(tmp.path(), &db_url).expect("reconcile push");
    assert_eq!(
        summary.assets_deleted, 1,
        "reconcile should drop exactly 1 stale asset, got {summary:?}"
    );
    let resp = client.get(format!("{base_url}/styles.css")).send().unwrap();
    assert_eq!(
        resp.status(),
        404,
        "after reconcile, asset should be gone — got status {}",
        resp.status()
    );
}

/// Tier 3 validation-failure test. A handler `.sql` file whose SQL
/// doesn't actually define the expected function should fail push with
/// a clear error and leave the DB unchanged — the live extension keeps
/// serving whatever was there before.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn push_rejects_missing_handler_function() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());

    // Apply migrations + push the good demo so there's live state to protect.
    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    pg_web_cli::push::push(tmp.path(), &db_url).expect("initial good push");

    // Add a broken route — the .sql file creates a wrongly-named function
    // instead of the one the router will expect.
    let broken_dir = tmp.path().join("pages/broken");
    fs::create_dir_all(&broken_dir).unwrap();
    fs::write(
        broken_dir.join("index.html"),
        "<p>broken: {{ value }}</p>\n",
    )
    .unwrap();
    fs::write(
        broken_dir.join("index.sql"),
        "CREATE OR REPLACE FUNCTION pgweb.pages__broken__typo(req json) RETURNS json \
         LANGUAGE sql IMMUTABLE AS $$ SELECT '{}'::json $$;\n",
    )
    .unwrap();

    let err = pg_web_cli::push::push(tmp.path(), &db_url)
        .expect_err("push should reject missing-handler route");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("pages__broken__index") && msg.contains("not found after push"),
        "error should point at the missing handler, got: {msg}"
    );

    // The live site still renders — rollback worked.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("No todos yet"),
        "rolled-back push should leave the prior state intact, got: {body}"
    );
    let resp = client.get(format!("{base_url}/broken")).send().unwrap();
    assert_eq!(
        resp.status(),
        404,
        "broken route should never have been committed"
    );
}

/// Tier 3 concurrency test (Component L). Multiple `pg-web push`
/// processes against the same DB used to fail the loser with
/// `tuple concurrently updated` from the racing `CREATE OR REPLACE
/// FUNCTION` calls (the real bug the user hit during Session 4
/// validation when a forgotten `pg-web dev` raced a new one). The
/// retry wrapper in `push_with_options` should make every concurrent
/// push eventually succeed; the assertion is "all pushers return Ok
/// AND every push lands a `pgweb.deployments` row."
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn concurrent_pushes_all_commit() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    let todo_app = todo_app_dir();
    pg_web_cli::migrate::apply(&todo_app, &db_url).expect("migrate apply");
    // Initial push so all subsequent pushes are pure CREATE OR REPLACE
    // updates against existing pg_proc rows — that's where concurrent
    // DDL races, not on first-time CREATEs.
    pg_web_cli::push::push(&todo_app, &db_url).expect("seed push");

    // Snapshot ledger before the concurrent burst so we can verify exactly
    // N rows landed.
    let mut pg = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
    let pre: i64 = pg
        .query_one("SELECT count(*) FROM pgweb.deployments", &[])
        .unwrap()
        .get(0);

    // Three concurrent pushers against the same app. Three is enough to
    // make racing on `CREATE OR REPLACE FUNCTION` likely without making
    // the test painfully slow on weak runners.
    const PUSHERS: usize = 3;
    let app = todo_app.clone();
    let url = db_url.clone();
    let handles: Vec<_> = (0..PUSHERS)
        .map(|_| {
            let app = app.clone();
            let url = url.clone();
            std::thread::spawn(move || pg_web_cli::push::push(&app, &url))
        })
        .collect();

    let mut failures = Vec::new();
    for h in handles {
        match h.join().expect("thread join") {
            Ok(_) => {}
            Err(e) => failures.push(format!("{e:#}")),
        }
    }
    assert!(
        failures.is_empty(),
        "all {PUSHERS} concurrent pushes should have committed; failures: {failures:#?}"
    );

    let post: i64 = pg
        .query_one("SELECT count(*) FROM pgweb.deployments", &[])
        .unwrap()
        .get(0);
    assert_eq!(
        post - pre,
        PUSHERS as i64,
        "every successful push lands a pgweb.deployments row"
    );
}

/// Tier 3 image-bundle test (Component F.3). The CLI is built into
/// `rtaylor96/pg-web:latest` at `/usr/local/bin/pg-web`, so users can
/// `docker compose exec postgres pg-web push --dir /app` from inside
/// the compose network without publishing :5432 to the host. Two
/// asserts: (1) `pg-web --version` succeeds with the expected version
/// string, (2) `pg-web push` against a bind-mounted demo + the
/// in-container `127.0.0.1:5432` results in a working HTTP response.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn cli_in_image_can_push_from_inside() {
    preflight_or_panic();

    let host_app = todo_app_dir().canonicalize().expect("canonical app path");
    let host_app_str = host_app.to_string_lossy().into_owned();
    let mount = Mount::bind_mount(host_app_str, "/app".to_string())
        .with_access_mode(testcontainers::core::AccessMode::ReadOnly);

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .with_mount(mount)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    // 1. Bare --version invocation. Proves the binary is on PATH and
    //    runs cleanly. clap's auto-generated --version emits
    //    "pg-web <version>" — assert both halves.
    let mut version_res = container
        .exec(ExecCommand::new(vec![
            "pg-web".to_string(),
            "--version".to_string(),
        ]))
        .expect("exec pg-web --version");
    let version_stdout = version_res.stdout_to_vec().expect("--version stdout");
    let version_text = String::from_utf8_lossy(&version_stdout);
    assert!(
        version_text.contains("pg-web") && version_text.contains("0."),
        "expected pg-web <version> on stdout, got: {version_text:?}"
    );
    assert_eq!(
        version_res.exit_code().expect("--version exit"),
        Some(0),
        "pg-web --version must exit 0; stdout: {version_text:?}"
    );

    // 2. Push the demo from inside the container against the
    //    in-network 127.0.0.1:5432. This is the F.3 value prop:
    //    deploys can run from inside the compose network without ever
    //    exposing PG to the host network.
    //
    //    POSTGRES_DB and POSTGRES_PASSWORD are set above; default
    //    user `postgres` matches the base image's behavior.
    let in_db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:5432/{POSTGRES_DB}"
    );
    let mut migrate_res = container
        .exec(ExecCommand::new(vec![
            "pg-web".to_string(),
            "migrate".to_string(),
            "apply".to_string(),
            "--dir".to_string(),
            "/app".to_string(),
            "--url".to_string(),
            in_db_url.clone(),
        ]))
        .expect("exec migrate apply");
    let migrate_err = String::from_utf8_lossy(
        &migrate_res.stderr_to_vec().expect("migrate stderr"),
    )
    .into_owned();
    assert_eq!(
        migrate_res.exit_code().expect("migrate exit"),
        Some(0),
        "in-image migrate apply must exit 0; stderr: {migrate_err}"
    );

    let mut push_res = container
        .exec(ExecCommand::new(vec![
            "pg-web".to_string(),
            "push".to_string(),
            "--dir".to_string(),
            "/app".to_string(),
            "--url".to_string(),
            in_db_url.clone(),
        ]))
        .expect("exec push");
    let push_err = String::from_utf8_lossy(&push_res.stderr_to_vec().expect("push stderr"))
        .into_owned();
    assert_eq!(
        push_res.exit_code().expect("push exit"),
        Some(0),
        "in-image push must exit 0; stderr: {push_err}"
    );

    // 3. HTTP probe — the demo's index renders.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains("No todos yet"),
        "in-image push should serve the demo's empty state, got: {body}"
    );

    // 4. Sanity: the deployment ledger should record this push as
    //    coming from inside the container — `from_host` is the
    //    container's hostname, NOT the dev box's hostname.
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let host_db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let mut pg = postgres::Client::connect(&host_db_url, postgres::NoTls).unwrap();
    let from_host: Option<String> = pg
        .query_one(
            "SELECT from_host FROM pgweb.deployments ORDER BY pushed_at DESC LIMIT 1",
            &[],
        )
        .unwrap()
        .get(0);
    let local_host = gethostname::gethostname().to_string_lossy().into_owned();
    let from_host = from_host.expect("from_host populated for in-image push");
    assert_ne!(
        from_host, local_host,
        "from_host should be the container's hostname, not the dev box's: {from_host} vs {local_host}"
    );
}

/// Tier 3 fingerprinted-asset test (Component H). When `pgweb.toml`
/// declares `[server].env = "production"`, push rewrites template
/// references like `<link href="/styles.css">` to fingerprinted URLs
/// (`/styles.<hex>.css`) and stores the asset under that URL. The
/// router then emits `Cache-Control: public, max-age=31536000,
/// immutable` for fingerprinted GETs while keeping `must-revalidate`
/// semantics for any unhashed legacy URL that's still requested.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn fingerprinted_assets_get_immutable_cache_control() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    // Copy the demo into a tempdir so we can flip its env to production
    // without touching the checked-in source.
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());
    let toml_path = tmp.path().join("pgweb.toml");
    let toml_contents = fs::read_to_string(&toml_path).unwrap();
    let prod_toml =
        toml_contents.replace("env  = \"development\"", "env  = \"production\"");
    fs::write(&toml_path, &prod_toml).unwrap();

    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    pg_web_cli::push::push(tmp.path(), &db_url).expect("prod push");

    // 1. The asset row in pgweb.assets sits under a fingerprinted URL.
    let mut pg = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
    let asset_path: String = pg
        .query_one(
            "SELECT path FROM pgweb.assets WHERE path LIKE '/styles.%.css'",
            &[],
        )
        .expect("fingerprinted styles asset row exists")
        .get(0);
    assert!(
        asset_path.starts_with("/styles.") && asset_path.ends_with(".css"),
        "expected fingerprinted /styles.<hex>.css, got: {asset_path}"
    );
    // No row left under the canonical URL.
    let canonical_count: i64 = pg
        .query_one(
            "SELECT count(*) FROM pgweb.assets WHERE path = '/styles.css'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(
        canonical_count, 0,
        "canonical /styles.css should not be present in prod-mode push"
    );

    // 2. The rendered template references the fingerprinted URL too —
    //    the push-time HTML rewrite caught the literal href.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body = get(&client, &base_url, "/");
    assert!(
        body.contains(&format!("href=\"{}\"", asset_path)),
        "rendered template should reference {asset_path}, got body: {body}"
    );
    assert!(
        !body.contains("href=\"/styles.css\""),
        "canonical /styles.css href should have been rewritten, got body: {body}"
    );

    // 3. The fingerprinted asset comes back with immutable Cache-Control.
    let resp = client
        .get(format!("{base_url}{asset_path}"))
        .send()
        .expect("GET fingerprinted asset");
    assert_eq!(resp.status(), 200);
    let cc = resp
        .headers()
        .get("cache-control")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        cc.contains("immutable"),
        "fingerprinted asset must serve `immutable` Cache-Control, got: {cc:?}"
    );
    assert!(
        cc.contains("max-age=31536000"),
        "fingerprinted asset must serve a year-long max-age, got: {cc:?}"
    );

    // 4. Sanity: the canonical URL no longer resolves — the asset is
    //    only registered under its fingerprinted path now.
    let resp = client
        .get(format!("{base_url}/styles.css"))
        .send()
        .expect("GET canonical asset");
    assert_eq!(
        resp.status(),
        404,
        "canonical /styles.css should 404 after prod-mode push"
    );
}

/// Tier 3 large-asset cap test (Component I). v0.2 raises the BYTEA
/// cap from 2 MiB to 20 MiB; this test pushes a 5 MiB asset (which
/// would have been rejected outright at v0.1) and verifies the full
/// payload is served byte-for-byte. True `pg_largeobject` streaming
/// for assets >20 MiB remains Phase-2+ work.
#[test]
#[ignore = "tier 3 E2E — Docker + rtaylor96/pg-web:latest required. \
            Run via scripts/test-all.sh or `cargo test -p pg-web \
            --test docker_e2e -- --ignored`."]
fn large_asset_below_new_cap_round_trips() {
    preflight_or_panic();

    let image = GenericImage::new(IMAGE, TAG)
        .with_exposed_port(5432.tcp())
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = image
        .with_env_var("POSTGRES_PASSWORD", POSTGRES_PASSWORD)
        .with_env_var("POSTGRES_DB", POSTGRES_DB)
        .start()
        .expect("start test image container (rtaylor96/pg-web)");
    let pg_host_port = container.get_host_port_ipv4(5432).expect("5432 host port");
    let http_host_port = container.get_host_port_ipv4(8080).expect("8080 host port");
    let db_url = format!(
        "postgres://postgres:{POSTGRES_PASSWORD}@127.0.0.1:{pg_host_port}/{POSTGRES_DB}"
    );
    let base_url = format!("http://127.0.0.1:{http_host_port}");
    wait_for_http(&base_url, Instant::now() + Duration::from_secs(60));

    // Copy demo, drop a 5 MiB file under public/ (random-ish bytes so
    // accidental compression-with-ETag aliasing doesn't cause the round
    // trip to silently dedupe).
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_tree(&todo_app_dir(), tmp.path());
    let pub_dir = tmp.path().join("public");
    fs::create_dir_all(&pub_dir).unwrap();
    // Pseudo-random fill via xor pattern — deterministic, not all-zeros
    // (which BYTEA could potentially store more compactly).
    let payload: Vec<u8> = (0..5 * 1024 * 1024_usize)
        .map(|i| ((i as u32).wrapping_mul(2654435761) & 0xff) as u8)
        .collect();
    fs::write(pub_dir.join("hero.bin"), &payload).unwrap();

    pg_web_cli::migrate::apply(tmp.path(), &db_url).expect("migrate apply");
    pg_web_cli::push::push(tmp.path(), &db_url).expect("push with 5 MiB asset");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let resp = client
        .get(format!("{base_url}/hero.bin"))
        .send()
        .expect("GET 5 MiB asset");
    assert_eq!(resp.status(), 200);
    let returned = resp.bytes().expect("body bytes").to_vec();
    assert_eq!(
        returned.len(),
        payload.len(),
        "round-trip length matches"
    );
    assert_eq!(returned, payload, "round-trip bytes match");
}
