//! Axum HTTP layer — thin shell. Delegates to `router::serve` for SPI + Tera.
//!
//! Responsibilities:
//! - Extract method, path, query string, content-type, and body bytes.
//! - Parse `application/x-www-form-urlencoded` bodies (and query strings) into
//!   a JSON object for the handler's `req` argument.
//! - Hand off to `router::serve` with the built `req` and shape the response.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use pgrx::bgworkers::BackgroundWorker;
use serde_json::{json, Map, Value};
use tracing::error;

use crate::errors::ServeError;
use crate::listen_router::ListenRouter;
use crate::livereload;
use crate::router::{self, ServeOutcome};
use crate::settings::{self, Env};

/// Hard cap on request body size — defense against runaway POSTs. Forms are
/// small in practice; anything bigger probably means misuse or file upload
/// (not supported in Phase 1).
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// True when `url`'s last segment matches `*.<hex>.<ext>` with the hex run
/// at least 8 chars — the fingerprint shape `pg-web push` emits in
/// production-mode pushes (Component H). The pattern is strict so a
/// canonical `/styles.minified.css` doesn't accidentally tip into the
/// `Cache-Control: immutable` branch. A duplicate of `push::is_fingerprinted_url`
/// in the CLI; small enough that workspace-shared utility crates would be
/// over-engineering.
fn is_fingerprinted_url(url: &str) -> bool {
    let file = url.rsplit_once('/').map(|(_, f)| f).unwrap_or(url);
    let parts: Vec<&str> = file.split('.').collect();
    if parts.len() < 3 {
        return false;
    }
    let hash_part = parts[parts.len() - 2];
    hash_part.len() >= 8 && hash_part.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn app(listen_router: Arc<ListenRouter>) -> Router {
    // Framework-reserved `/_pgweb/*` routes sit above the fallback so
    // a user's own GET /_pgweb/foo handler (unusual but legal) can
    // still be defined without colliding with livereload internals.
    // SSE carries the ListenRouter as axum state; the JS stub is
    // a static response and doesn't need state.
    let livereload_routes = Router::new()
        .route("/_pgweb/livereload", get(livereload::serve_livereload_sse))
        .with_state(listen_router);
    let static_routes =
        Router::new().route("/_pgweb/livereload.js", get(livereload::serve_livereload_js));

    Router::new()
        .merge(livereload_routes)
        .merge(static_routes)
        .fallback(handle)
}

async fn handle(req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query_str = req.uri().query().unwrap_or("").to_string();
    let is_form = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("application/x-www-form-urlencoded"))
        .unwrap_or(false);
    // Preserve the client's If-None-Match (if any) so an asset lookup can
    // short-circuit to 304 without re-sending bytes.
    let if_none_match = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let body_bytes = match to_bytes(req.into_body(), MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "body read failed");
            return status_plain(
                StatusCode::BAD_REQUEST,
                "request body too large or unreadable\n",
            );
        }
    };

    let body_obj = if is_form {
        parse_urlencoded(std::str::from_utf8(&body_bytes).unwrap_or(""))
    } else {
        Map::new()
    };
    let query_obj = parse_urlencoded(&query_str);

    // `path_params` starts empty; the router overwrites it with captures
    // extracted from the matched dynamic route (e.g., /posts/:id → {id: "42"}).
    // Always-present keeps the handler contract uniform: `req->'path_params'`
    // is never null.
    let req_value = json!({
        "body": Value::Object(body_obj),
        "query": Value::Object(query_obj),
        "method": method,
        "path": path,
        "path_params": Value::Object(Map::new()),
    });

    // Clone what the dev page needs before `router::serve` consumes `req_value`.
    let req_for_dev_page = req_value.clone();

    match router::serve(&method, &path, req_value) {
        ServeOutcome::Response { status, body } => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            // Inject the livereload client into full HTML documents in
            // dev mode. The helper is a no-op in production and on
            // fragment responses (no </body>). Env is fetched via SPI
            // once here per request.
            let env = BackgroundWorker::transaction(settings::current_env);
            let body = livereload::inject_script_if_eligible(body, env);
            (
                code,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                body,
            )
                .into_response()
        }
        ServeOutcome::Asset {
            body,
            content_type,
            etag,
        } => render_asset(&path, body, content_type, etag, if_none_match.as_deref()),
        ServeOutcome::Error(err) => render_error(err, &method, &path, &req_for_dev_page),
    }
}

/// Build the static-asset response. ETag + Cache-Control are always
/// emitted; if the request's `If-None-Match` matches the stored ETag,
/// skip the body and return 304.
///
/// Cache-Control policy:
/// - dev:  `no-cache` — browser always revalidates via ETag, so a
///   saved file shows up on refresh without hard-reload gymnastics.
/// - prod, canonical URL (e.g. `/styles.css`): `public, max-age=0,
///   must-revalidate` — ETag round-trip on every page load.
/// - prod, fingerprinted URL (e.g. `/styles.<hex>.css`): `public,
///   max-age=31536000, immutable` — content-addressed URL means
///   the bytes never change for that URL, so the browser can
///   cache forever without revalidation. Component H.
fn render_asset(
    request_path: &str,
    body: Vec<u8>,
    content_type: String,
    etag: String,
    if_none_match: Option<&str>,
) -> Response {
    let env = BackgroundWorker::transaction(settings::current_env);
    let fingerprinted = is_fingerprinted_url(request_path);
    let cache_control = match (env, fingerprinted) {
        (Env::Development, _) => "no-cache",
        (Env::Production, true) => "public, max-age=31536000, immutable",
        (Env::Production, false) => "public, max-age=0, must-revalidate",
    };

    if if_none_match.map(|v| v == etag).unwrap_or(false) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag.as_str()),
                (header::CACHE_CONTROL, cache_control),
            ],
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.as_str()),
            (header::ETAG, etag.as_str()),
            (header::CACHE_CONTROL, cache_control),
        ],
        body,
    )
        .into_response()
}

/// Branch on `pgweb.settings.env`:
/// - `development` → rich dev error page with code + title + remedy + req dump.
/// - `production`  → generic 500 body; the log still gets the full picture.
fn render_error(err: ServeError, method: &str, path: &str, req: &Value) -> Response {
    // Structured log line always — this is how prod operators see failures.
    error!(method = %method, path = %path, pgweb_error = %err.log_line(), "serve error");

    let env = BackgroundWorker::transaction(settings::current_env);
    match env {
        Env::Development => {
            let body = err.render_dev_page(req, 500);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                body,
            )
                .into_response()
        }
        Env::Production => status_plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal server error\n",
        ),
    }
}

fn status_plain(status: StatusCode, body: &'static str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        Body::from(body),
    )
        .into_response()
}

#[cfg(any(test, feature = "pg_test"))]
mod tests {
    use super::is_fingerprinted_url;

    #[test]
    fn fingerprinted_when_last_segment_has_hex_subextension() {
        assert!(is_fingerprinted_url("/styles.abcd1234.css"));
        assert!(is_fingerprinted_url("/img/logo.deadbeef.png"));
        assert!(is_fingerprinted_url("/js/app.min.12345678.js"));
    }

    #[test]
    fn not_fingerprinted_for_canonical_paths() {
        assert!(!is_fingerprinted_url("/styles.css"));
        assert!(!is_fingerprinted_url("/img/logo.png"));
        // Non-hex middle segment.
        assert!(!is_fingerprinted_url("/styles.minified.css"));
        // Hex but too short to be a fingerprint.
        assert!(!is_fingerprinted_url("/styles.abc.css"));
        // Single segment can't have the *.<hex>.<ext> shape.
        assert!(!is_fingerprinted_url("/foo"));
    }
}

/// Parse `application/x-www-form-urlencoded` content into a string-keyed
/// JSON object. Duplicate keys keep the last value — matches most server
/// frameworks' default and keeps the shape simple for SQL handlers. Empty
/// or malformed input yields an empty object.
fn parse_urlencoded(s: &str) -> Map<String, Value> {
    if s.is_empty() {
        return Map::new();
    }
    match serde_urlencoded::from_str::<Vec<(String, String)>>(s) {
        Ok(pairs) => pairs
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
        Err(_) => Map::new(),
    }
}
