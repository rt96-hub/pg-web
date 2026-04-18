//! Axum HTTP layer — thin shell. Delegates to `router::serve` for SPI + Tera.
//!
//! Responsibilities:
//! - Extract method, path, query string, content-type, and body bytes.
//! - Parse `application/x-www-form-urlencoded` bodies (and query strings) into
//!   a JSON object for the handler's `req` argument.
//! - Hand off to `router::serve` with the built `req` and shape the response.

use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use serde_json::{json, Map, Value};
use tracing::error;

use crate::router::{self, ServeOutcome};

/// Hard cap on request body size — defense against runaway POSTs. Forms are
/// small in practice; anything bigger probably means misuse or file upload
/// (not supported in Phase 1).
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub fn app() -> Router {
    Router::new().fallback(handle)
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

    let req_value = json!({
        "body": Value::Object(body_obj),
        "query": Value::Object(query_obj),
        "method": method,
        "path": path,
    });

    match router::serve(&method, &path, req_value) {
        ServeOutcome::Response { status, body } => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            (
                code,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                body,
            )
                .into_response()
        }
        ServeOutcome::Error(err) => {
            error!(method = %method, path = %path, error = %err, "handler error");
            status_plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error\n",
            )
        }
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
