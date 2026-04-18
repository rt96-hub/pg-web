//! Route resolution + dispatch via SPI.
//!
//! One HTTP request → one `BackgroundWorker::transaction` → SPI lookups and
//! handler call, rolled back on any error.
//!
//! Two dispatch modes, chosen from `pgweb.routes.template_path`:
//! - non-NULL → handler returns `json`, Tera renders the template with it.
//! - NULL     → handler returns `text`, bytes are sent as-is.
//!
//! On lookup miss: fall back to `method='404'` row with longest `path_pattern`
//! prefixing the requested URL. Phase 1 supports root-scoped only
//! (`path_pattern='/'`). If no fallback exists, serve a hardcoded minimal 404.

use pgrx::bgworkers::BackgroundWorker;
use pgrx::Spi;
use serde_json::Value;

use crate::templating;

/// What the HTTP layer turns into a response.
pub enum ServeOutcome {
    /// 2xx or 4xx body with content-type text/html.
    Response { status: u16, body: String },
    /// Internal error — HTTP 500 with a generic body.
    Error(String),
}

/// Default 404 body when no user-provided `pages/_404` template exists.
const DEFAULT_NOT_FOUND_BODY: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Not found</title></head><body><h1>404 — Not found</h1>\
<p>No route matches this path.</p></body></html>";

pub fn serve(method: &str, path: &str, req: Value) -> ServeOutcome {
    let method = method.to_string();
    let path = path.to_string();
    BackgroundWorker::transaction(move || serve_in_tx(&method, &path, &req))
}

fn serve_in_tx(method: &str, path: &str, req: &Value) -> ServeOutcome {
    match lookup_route(method, path) {
        Err(e) => return ServeOutcome::Error(e),
        Ok(Some(route)) => return render_route(&route, req, 200),
        Ok(None) => {}
    }

    // Route miss — try the longest-matching 404 fallback.
    match lookup_fallback(path) {
        Err(e) => ServeOutcome::Error(e),
        Ok(Some(route)) => render_route(&route, req, 404),
        Ok(None) => ServeOutcome::Response {
            status: 404,
            body: DEFAULT_NOT_FOUND_BODY.to_string(),
        },
    }
}

fn render_route(route: &Route, req: &Value, status: u16) -> ServeOutcome {
    let handler_text = match call_handler(&route.handler_name, req) {
        Ok(s) => s,
        Err(e) => return ServeOutcome::Error(e),
    };

    match &route.template_path {
        Some(tp) => {
            let template = match fetch_template(tp) {
                Ok(t) => t,
                Err(e) => return ServeOutcome::Error(e),
            };
            let context = match serde_json::from_str::<Value>(&handler_text) {
                Ok(v) => v,
                Err(e) => {
                    return ServeOutcome::Error(format!(
                        "handler {} did not return valid JSON for Tera context: {e}",
                        route.handler_name
                    ))
                }
            };
            match templating::render(&template, &context) {
                Ok(body) => ServeOutcome::Response { status, body },
                Err(e) => ServeOutcome::Error(e),
            }
        }
        None => ServeOutcome::Response {
            status,
            body: handler_text,
        },
    }
}

struct Route {
    handler_name: String,
    template_path: Option<String>,
}

fn quote_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn is_safe_ident(ident: &str) -> bool {
    if ident.is_empty() || ident.len() > 128 {
        return false;
    }
    let mut dots = 0u32;
    for (i, c) in ident.bytes().enumerate() {
        let ok = matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'_')
            || (i > 0 && c.is_ascii_digit())
            || (c == b'.' && dots == 0 && i > 0);
        if c == b'.' {
            dots += 1;
        }
        if !ok {
            return false;
        }
    }
    true
}

/// `Spi::get_one` on a query matching zero rows returns
/// `Err(SpiError::InvalidPosition)`. Normalize to `Ok(None)`.
fn get_one_optional<T: pgrx::datum::FromDatum + pgrx::datum::IntoDatum>(
    query: &str,
) -> Result<Option<T>, String> {
    match Spi::get_one::<T>(query) {
        Ok(v) => Ok(v),
        Err(pgrx::spi::Error::InvalidPosition) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn lookup_route(method: &str, path: &str) -> Result<Option<Route>, String> {
    // Two separate single-column queries rather than one two-column query:
    // pgrx's Spi::get_one returns a single datum. Multi-column fetches would
    // need Spi::connect + client.select. Per-request overhead is negligible
    // since SPI hits in-memory shared buffers, and both queries share the
    // same tuple in the primary-key index.
    let method_lit = quote_literal(method);
    let path_lit = quote_literal(path);

    let handler_name = match get_one_optional::<String>(&format!(
        "SELECT handler_name FROM pgweb.routes \
         WHERE method = {method_lit} AND path_pattern = {path_lit} LIMIT 1"
    ))? {
        Some(s) => s,
        None => return Ok(None),
    };
    let template_path = get_one_optional::<String>(&format!(
        "SELECT template_path FROM pgweb.routes \
         WHERE method = {method_lit} AND path_pattern = {path_lit} LIMIT 1"
    ))?;
    Ok(Some(Route {
        handler_name,
        template_path,
    }))
}

/// 404 fallback lookup. Phase 1 only supports root-scoped fallbacks
/// (`path_pattern='/'` with `method='404'`). Phase 2+ will extend to
/// longest-prefix-match for per-subtree fallbacks.
fn lookup_fallback(_path: &str) -> Result<Option<Route>, String> {
    let handler_name = match get_one_optional::<String>(
        "SELECT handler_name FROM pgweb.routes \
         WHERE method = '404' AND path_pattern = '/' LIMIT 1",
    )? {
        Some(s) => s,
        None => return Ok(None),
    };
    let template_path = get_one_optional::<String>(
        "SELECT template_path FROM pgweb.routes \
         WHERE method = '404' AND path_pattern = '/' LIMIT 1",
    )?;
    Ok(Some(Route {
        handler_name,
        template_path,
    }))
}

fn fetch_template(template_path: &str) -> Result<String, String> {
    let query = format!(
        "SELECT content FROM pgweb.templates WHERE template_path = {} LIMIT 1",
        quote_literal(template_path)
    );
    match get_one_optional::<String>(&query)? {
        Some(s) => Ok(s),
        None => Err(format!("template not found: {template_path}")),
    }
}

fn call_handler(handler_name: &str, req: &Value) -> Result<String, String> {
    if !is_safe_ident(handler_name) {
        return Err(format!("handler name rejected: {handler_name:?}"));
    }
    let req_json = serde_json::to_string(req).map_err(|e| e.to_string())?;
    let query = format!(
        "SELECT ({handler_name}({}::json))::text AS result",
        quote_literal(&req_json)
    );
    match get_one_optional::<String>(&query)? {
        Some(s) => Ok(s),
        None => Err(format!("handler {handler_name}: returned no row")),
    }
}
