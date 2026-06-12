//! Route resolution + dispatch via SPI.
//!
//! One HTTP request → one `BackgroundWorker::transaction` → SPI lookups and
//! handler call, rolled back on any error.
//!
//! Two dispatch modes, chosen from `pgweb.routes.template_path`:
//! - non-NULL → handler returns `json`, Tera renders the template with it.
//! - NULL     → handler returns `text`, bytes are sent as-is.
//!
//! ## Pattern matching
//!
//! `pgweb.routes.path_pattern` stores one of:
//! - A static path: `/posts/new`.
//! - A dynamic pattern with `:name` captures: `/posts/:id`, `/users/:user/posts/:post`.
//!
//! Captures are **derived from the pattern at match time** — there is no
//! `path_captures` column. This keeps the pattern string the single source
//! of truth and eliminates the drift risk of a denormalized captures column
//! (see ROADMAP decision log 2026-04-20). Captures are always strings; the
//! handler is responsible for casting / validating them (e.g., `(req->'path_params'->>'id')::bigint`).
//!
//! Matching uses a **naïve specificity-sorted scan**: fetch all routes for
//! the method, parse patterns into segments, sort by (static-segment count
//! desc, capture-segment count asc, length desc) so `/posts/new` beats
//! `/posts/:id`, then iterate and take the first match. At Phase 1 route
//! counts (<100 per app) this is invisible; we revisit if route count
//! exceeds ~1000 or router match becomes a measured hot path. Alternatives
//! (trie, `RegexSet`) are rejected as premature. See decision log 2026-04-20.
//!
//! ## 404 fallback
//!
//! On route miss: fall back to `method='404'` row with `path_pattern='/'`.
//! Phase 1 supports root-scoped fallbacks only. If no fallback exists,
//! serve a hardcoded minimal 404.

use std::collections::BTreeMap;

use pgrx::bgworkers::BackgroundWorker;
use pgrx::Spi;
use serde_json::{Map, Value};

use crate::errors::ServeError;
use crate::templating;

/// What the HTTP layer turns into a response.
pub enum ServeOutcome {
    /// Dynamic response (page, fragment, JSON API, redirect, etc.).
    /// Status may come from the handler envelope or the router (200/404).
    /// content_type None → http.rs default "text/html; charset=utf-8".
    /// headers/cookies populated only when a response envelope was returned.
    Response {
        status: u16,
        body: String,
        content_type: Option<String>,
        headers: Vec<(String, String)>,
        cookies: Vec<String>,
    },
    /// Static asset response (CSS, JS, images, …). Bytes + content-type
    /// + ETag from `pgweb.assets`. `http.rs` owns the Cache-Control
    ///   header and the `If-None-Match` → 304 conversion.
    Asset {
        body: Vec<u8>,
        content_type: String,
        etag: String,
    },
    /// Internal error — typed so `http.rs` can render either a rich dev
    /// page (env=development) or a generic 500 (env=production).
    Error(ServeError),
}

/// Default 404 body when no user-provided `pages/_404` template exists.
const DEFAULT_NOT_FOUND_BODY: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Not found</title></head><body><h1>404 — Not found</h1>\
<p>No route matches this path.</p></body></html>";

pub fn serve(method: &str, path: &str, req: Value) -> ServeOutcome {
    let method = method.to_string();
    let path = path.to_string();
    BackgroundWorker::transaction(move || serve_in_tx(&method, &path, req))
}

fn serve_in_tx(method: &str, path: &str, mut req: Value) -> ServeOutcome {
    match lookup_route(method, path) {
        Err(e) => return ServeOutcome::Error(e),
        Ok(Some(matched)) => {
            inject_path_params(&mut req, &matched.path_params);
            return render_route(&matched.route, &req, 200);
        }
        Ok(None) => {}
    }

    // GET-only: if no page route matched, try static assets. Pages win
    // over assets by design — user-defined routes are more specific.
    if method == "GET" {
        match lookup_asset(path) {
            Err(e) => return ServeOutcome::Error(e),
            Ok(Some(asset)) => {
                return ServeOutcome::Asset {
                    body: asset.content,
                    content_type: asset.content_type,
                    etag: asset.etag,
                };
            }
            Ok(None) => {}
        }
    }

    // Route + asset miss — try the root-scoped 404 fallback.
    match lookup_fallback(path) {
        Err(e) => ServeOutcome::Error(e),
        Ok(Some(route)) => render_route(&route, &req, 404),
        Ok(None) => ServeOutcome::Response {
            status: 404,
            body: DEFAULT_NOT_FOUND_BODY.to_string(),
            content_type: None,
            headers: vec![],
            cookies: vec![],
        },
    }
}

struct Asset {
    content: Vec<u8>,
    content_type: String,
    etag: String,
}

/// Fetch a single row from `pgweb.assets` by exact path match. Static
/// assets are keyed by their URL path (`/styles.css`) — the CLI push
/// derives this from the filesystem under `public/`.
fn lookup_asset(path: &str) -> Result<Option<Asset>, ServeError> {
    let query = format!(
        "SELECT content, content_type, etag FROM pgweb.assets \
         WHERE path = {} LIMIT 1",
        quote_literal(path)
    );
    Spi::connect(|client| -> Result<Option<Asset>, ServeError> {
        let table = client
            .select(&query, None, &[])
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?;
        let Some(row) = table.into_iter().next() else {
            return Ok(None);
        };
        let content: Vec<u8> = row
            .get(1)
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?
            .ok_or_else(|| ServeError::Other {
                message: "null content in pgweb.assets".into(),
            })?;
        let content_type: String = row
            .get(2)
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?
            .ok_or_else(|| ServeError::Other {
                message: "null content_type in pgweb.assets".into(),
            })?;
        let etag: String = row
            .get(3)
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?
            .ok_or_else(|| ServeError::Other {
                message: "null etag in pgweb.assets".into(),
            })?;
        Ok(Some(Asset {
            content,
            content_type,
            etag,
        }))
    })
}

/// Route path attached to the current request, used for error context
/// when variants don't carry their own route (e.g., HandlerSqlException).
/// Derived from `req.path` for simplicity.
fn req_path(req: &Value) -> String {
    req.get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_string()
}

/// Overwrite `req.path_params` with the captures from the matched route.
/// `req` always arrives with an empty `path_params` object from the HTTP
/// layer, keeping the contract shape uniform across all request types.
fn inject_path_params(req: &mut Value, params: &BTreeMap<String, String>) {
    if let Value::Object(m) = req {
        let mut obj = Map::with_capacity(params.len());
        for (k, v) in params {
            obj.insert(k.clone(), Value::String(v.clone()));
        }
        m.insert("path_params".to_string(), Value::Object(obj));
    }
}

/// Parsed response envelope (prompt 013). Present only when the handler return
/// text is a JSON object containing the reserved top-level "$pgweb" key.
/// All fields optional; absent ones fall back to framework defaults or the
/// `status` argument passed by the router (200 or 404 for fallbacks).
#[derive(Debug, Default)]
struct ResponseEnvelope {
    status: Option<u16>,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    cookies: Vec<String>,
    /// If Some, emit this string literally (bypass Tera even on template routes).
    body: Option<String>,
    /// If present (and no "body"), and this is a template route, render Tera
    /// with this value as context instead of the whole return value.
    context: Option<Value>,
}

/// Cheap probe + structured extract for the v2 response envelope.
/// Returns None for any legacy bare-JSON or bare-text return (including
/// a top-level JSON object that happens to contain "status" or "body" keys
/// but *not* the "$pgweb" sentinel). This guarantees byte-for-byte
/// compatibility for all pre-013 handlers.
fn parse_response_envelope(handler_text: &str) -> Option<ResponseEnvelope> {
    let val: Value = serde_json::from_str(handler_text).ok()?;
    let obj = val.as_object()?;
    let pgweb = obj.get("$pgweb")?.as_object()?;

    let mut env = ResponseEnvelope::default();

    if let Some(s) = pgweb.get("status").and_then(|v| v.as_u64()).and_then(|u| u16::try_from(u).ok()) {
        env.status = Some(s);
    }
    if let Some(ct) = pgweb.get("content_type").and_then(|v| v.as_str()) {
        env.content_type = Some(ct.to_string());
    }
    if let Some(h) = pgweb.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in h {
            if let Some(vs) = v.as_str() {
                env.headers.push((k.clone(), vs.to_string()));
            }
        }
    }
    if let Some(cs) = pgweb.get("cookies").and_then(|v| v.as_array()) {
        for c in cs {
            if let Some(s) = c.as_str() {
                env.cookies.push(s.to_string());
            }
        }
    }
    if let Some(b) = obj.get("body") {
        env.body = Some(b.as_str().unwrap_or("").to_string());
    }
    if let Some(c) = obj.get("context") {
        // Only store if it is an object (Tera context must be); otherwise ignore
        // so a weird "context": "string" doesn't explode later.
        if c.is_object() || c.is_null() {
            env.context = Some(c.clone());
        }
    }
    Some(env)
}

fn render_route(route: &Route, req: &Value, status: u16) -> ServeOutcome {
    let handler_text = match call_handler(&route.handler_name, req) {
        Ok(s) => s,
        Err(e) => return ServeOutcome::Error(e),
    };

    // Response contract v2: detect envelope *before* legacy dispatch.
    // The "$pgweb" sentinel is the only trigger; ordinary JSON (even with
    // "status"/"body" keys) falls through and is handled exactly as before.
    if let Some(env) = parse_response_envelope(&handler_text) {
        let final_status = env.status.unwrap_or(status);
        let final_ct = env.content_type;
        let final_headers = env.headers;
        let final_cookies = env.cookies;

        let final_body = if let Some(b) = env.body {
            // Literal body wins in both modes (supports "render on error path,
            // redirect on success path" from a single template-mode handler).
            b
        } else if let Some(tp) = &route.template_path {
            if let Some(ctx) = env.context {
                let template = match fetch_template(tp) {
                    Ok(t) => t,
                    Err(e) => return ServeOutcome::Error(e),
                };
                match templating::render(tp, &template, &ctx) {
                    Ok(body) => body,
                    Err(e) => return ServeOutcome::Error(e),
                }
            } else {
                // Envelope present for headers/cookies/status but no body/context guidance.
                String::new()
            }
        } else {
            // Raw-text envelope without explicit body → empty body (the
            // interesting part is the status/headers/cookies, e.g. redirects).
            String::new()
        };

        return ServeOutcome::Response {
            status: final_status,
            body: final_body,
            content_type: final_ct,
            headers: final_headers,
            cookies: final_cookies,
        };
    }

    // Legacy path (pre-013 bare json/text returns) — byte-identical behavior.
    match &route.template_path {
        Some(tp) => {
            let template = match fetch_template(tp) {
                Ok(t) => t,
                Err(e) => return ServeOutcome::Error(e),
            };
            let context = match serde_json::from_str::<Value>(&handler_text) {
                Ok(v) => v,
                Err(e) => {
                    return ServeOutcome::Error(ServeError::HandlerReturnNotJson {
                        handler_name: route.handler_name.clone(),
                        raw: handler_text.clone(),
                        parse_error: e.to_string(),
                    })
                }
            };
            match templating::render(tp, &template, &context) {
                Ok(body) => ServeOutcome::Response {
                    status,
                    body,
                    content_type: None,
                    headers: vec![],
                    cookies: vec![],
                },
                Err(e) => ServeOutcome::Error(e),
            }
        }
        None => ServeOutcome::Response {
            status,
            body: handler_text,
            content_type: None,
            headers: vec![],
            cookies: vec![],
        },
    }
}

struct Route {
    handler_name: String,
    template_path: Option<String>,
}

struct MatchedRoute {
    route: Route,
    /// Captures extracted from the URL, keyed by capture name. Empty for
    /// purely-static routes. Ordered so error messages are deterministic.
    path_params: BTreeMap<String, String>,
}

/// One parsed segment of a stored `path_pattern`.
#[derive(Debug, PartialEq, Eq)]
enum PatSeg {
    Static(String),
    /// Owns its name so `matches` can copy into `path_params`.
    Capture(String),
}

/// Cached parse of a pattern string + specificity key for sort.
#[derive(Debug)]
struct ParsedPattern {
    segments: Vec<PatSeg>,
    /// Number of `Static` segments — primary sort key (higher = more specific).
    static_count: usize,
    /// Number of `Capture` segments — secondary sort key (lower = more specific).
    capture_count: usize,
    /// Total segment count — tiebreaker (higher = more specific at equal static/capture counts).
    length: usize,
}

impl ParsedPattern {
    /// Parse a stored pattern like `/posts/:id` into typed segments. Rejects
    /// `:` not at the start of a segment so a malformed pattern snuck into
    /// `pgweb.routes` surfaces as a clear error, not a silent mis-match.
    fn parse(pattern: &str) -> Result<Self, String> {
        let mut segments = Vec::new();
        let mut static_count = 0usize;
        let mut capture_count = 0usize;
        for raw in pattern.split('/').filter(|s| !s.is_empty()) {
            if let Some(name) = raw.strip_prefix(':') {
                if name.is_empty() {
                    return Err(format!(
                        "pattern '{pattern}' has an empty capture segment ':' — expected ':name'"
                    ));
                }
                capture_count += 1;
                segments.push(PatSeg::Capture(name.to_string()));
            } else if raw.contains(':') {
                return Err(format!(
                    "pattern '{pattern}' has a segment '{raw}' with ':' not at its start — \
                     captures must occupy a whole segment (e.g., '/posts/:id')"
                ));
            } else {
                static_count += 1;
                segments.push(PatSeg::Static(raw.to_string()));
            }
        }
        let length = segments.len();
        Ok(Self {
            segments,
            static_count,
            capture_count,
            length,
        })
    }

    /// Test whether this pattern matches the given request segments.
    /// Returns the captures on match, `None` otherwise.
    fn matches(&self, req_segments: &[&str]) -> Option<BTreeMap<String, String>> {
        if self.segments.len() != req_segments.len() {
            return None;
        }
        let mut caps = BTreeMap::new();
        for (pat, req) in self.segments.iter().zip(req_segments.iter()) {
            match pat {
                PatSeg::Static(s) => {
                    if s != req {
                        return None;
                    }
                }
                PatSeg::Capture(name) => {
                    // Per-segment URL piece is never empty here (we filtered
                    // empty splits above) and can't contain '/' — that's a
                    // cross-segment concern. Anything else is a legal value.
                    caps.insert(name.clone(), (*req).to_string());
                }
            }
        }
        Some(caps)
    }
}

/// Split a request path into non-empty URL segments.
fn request_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
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

/// Accept a conservative subset of Postgres identifiers: letters, digits,
/// underscore, and (in non-leading position) `$` for capture markers and
/// `.` for the single `schema.name` split. Digits and `$` are banned at
/// position 0 per PG identifier rules.
fn is_safe_ident(ident: &str) -> bool {
    if ident.is_empty() || ident.len() > 128 {
        return false;
    }
    let mut dots = 0u32;
    for (i, c) in ident.bytes().enumerate() {
        let ok = matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'_')
            || (i > 0 && (c.is_ascii_digit() || c == b'$'))
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
) -> Result<Option<T>, ServeError> {
    match Spi::get_one::<T>(query) {
        Ok(v) => Ok(v),
        Err(pgrx::spi::Error::InvalidPosition) => Ok(None),
        Err(e) => Err(ServeError::Other {
            message: e.to_string(),
        }),
    }
}

struct RouteRow {
    path_pattern: String,
    handler_name: String,
    template_path: Option<String>,
}

/// Multi-row fetch: all routes for the given method. Pattern parsing +
/// specificity sort + match happen in Rust so we can emit clear errors
/// if any stored pattern is malformed.
fn fetch_method_routes(method: &str) -> Result<Vec<RouteRow>, ServeError> {
    let method_lit = quote_literal(method);
    let query = format!(
        "SELECT path_pattern, handler_name, template_path \
         FROM pgweb.routes WHERE method = {method_lit}"
    );
    Spi::connect(|client| -> Result<Vec<RouteRow>, ServeError> {
        let table = client
            .select(&query, None, &[])
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?;
        let mut out = Vec::new();
        for row in table {
            let path_pattern: String = row
                .get(1)
                .map_err(|e| ServeError::Other {
                    message: e.to_string(),
                })?
                .ok_or_else(|| ServeError::Other {
                    message: "null path_pattern in pgweb.routes".into(),
                })?;
            let handler_name: String = row
                .get(2)
                .map_err(|e| ServeError::Other {
                    message: e.to_string(),
                })?
                .ok_or_else(|| ServeError::Other {
                    message: "null handler_name in pgweb.routes".into(),
                })?;
            let template_path: Option<String> = row.get(3).map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?;
            out.push(RouteRow {
                path_pattern,
                handler_name,
                template_path,
            });
        }
        Ok(out)
    })
}

fn lookup_route(method: &str, path: &str) -> Result<Option<MatchedRoute>, ServeError> {
    let rows = fetch_method_routes(method)?;

    // Parse each pattern once. Any malformed pattern surfaces here rather
    // than as a silent mis-match at HTTP time.
    let mut parsed: Vec<(RouteRow, ParsedPattern)> = Vec::with_capacity(rows.len());
    for row in rows {
        let pat = ParsedPattern::parse(&row.path_pattern).map_err(|reason| {
            ServeError::RoutePatternMalformed {
                pattern: row.path_pattern.clone(),
                reason,
            }
        })?;
        parsed.push((row, pat));
    }

    // Sort by specificity descending: static-count desc, capture-count asc,
    // length desc. The sort is stable so duplicate keys retain insertion
    // (DB) order, which is fine since the primary key (method, path_pattern)
    // prevents literal duplicates.
    parsed.sort_by(|a, b| {
        b.1.static_count
            .cmp(&a.1.static_count)
            .then(a.1.capture_count.cmp(&b.1.capture_count))
            .then(b.1.length.cmp(&a.1.length))
    });

    let req_segs = request_segments(path);
    for (row, pat) in &parsed {
        if let Some(caps) = pat.matches(&req_segs) {
            return Ok(Some(MatchedRoute {
                route: Route {
                    handler_name: row.handler_name.clone(),
                    template_path: row.template_path.clone(),
                },
                path_params: caps,
            }));
        }
    }
    Ok(None)
}

/// 404 fallback lookup. Phase 1 only supports root-scoped fallbacks
/// (`path_pattern='/'` with `method='404'`). Phase 2+ will extend to
/// longest-prefix-match for per-subtree fallbacks.
fn lookup_fallback(_path: &str) -> Result<Option<Route>, ServeError> {
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

fn fetch_template(template_path: &str) -> Result<String, ServeError> {
    let query = format!(
        "SELECT content FROM pgweb.templates WHERE template_path = {} LIMIT 1",
        quote_literal(template_path)
    );
    match get_one_optional::<String>(&query)? {
        Some(s) => Ok(s),
        None => Err(ServeError::TemplateMissing {
            template_path: template_path.to_string(),
        }),
    }
}

/// Call the user's handler through `pgweb._framework_call_handler`, the
/// PL/pgSQL wrapper that catches `WHEN OTHERS` and returns structured
/// diagnostics. On SQL failure we get SQLSTATE + MESSAGE + DETAIL + HINT
/// + CONTEXT as columns and classify into a typed `ServeError` here.
fn call_handler(handler_name: &str, req: &Value) -> Result<String, ServeError> {
    if !is_safe_ident(handler_name) {
        return Err(ServeError::Other {
            message: format!("rejected handler name: {handler_name:?}"),
        });
    }
    let req_json = serde_json::to_string(req).map_err(|e| ServeError::Other {
        message: e.to_string(),
    })?;
    let query = format!(
        "SELECT ok, result_text, error_sqlstate, error_message, \
                error_detail, error_hint, error_context \
         FROM pgweb._framework_call_handler({}, {}::json)",
        quote_literal(handler_name),
        quote_literal(&req_json),
    );
    Spi::connect(|client| -> Result<String, ServeError> {
        let table = client
            .select(&query, None, &[])
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?;
        let row = table.into_iter().next().ok_or_else(|| ServeError::Other {
            message: "_framework_call_handler returned no row".into(),
        })?;

        let ok: bool = row
            .get(1)
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?
            .unwrap_or(false);

        if ok {
            let result_text: Option<String> = row.get(2).map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?;
            return Ok(result_text.unwrap_or_default());
        }

        // Error path — classify by SQLSTATE.
        let sqlstate: String = row
            .get(3)
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?
            .unwrap_or_default();
        let message: String = row
            .get(4)
            .map_err(|e| ServeError::Other {
                message: e.to_string(),
            })?
            .unwrap_or_default();
        let detail: Option<String> = row.get(5).map_err(|e| ServeError::Other {
            message: e.to_string(),
        })?;
        let hint: Option<String> = row.get(6).map_err(|e| ServeError::Other {
            message: e.to_string(),
        })?;
        let context: Option<String> = row.get(7).map_err(|e| ServeError::Other {
            message: e.to_string(),
        })?;

        Err(classify_handler_error(
            handler_name,
            &sqlstate,
            message,
            detail,
            hint,
            context,
            req,
        ))
    })
}

/// Map a SQLSTATE from the handler's EXCEPTION block to a typed variant.
/// Adding a new dedicated variant for a SQLSTATE: add a match arm here.
fn classify_handler_error(
    handler_name: &str,
    sqlstate: &str,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
    context: Option<String>,
    req: &Value,
) -> ServeError {
    match sqlstate {
        // 42883 undefined_function: the handler function doesn't exist.
        "42883" => ServeError::HandlerMissing {
            handler_name: handler_name.to_string(),
            route: req_path(req),
        },
        // 42P13 invalid_function_definition, 42725 ambiguous_function: wrong signature.
        "42P13" | "42725" => ServeError::HandlerSignatureMismatch {
            handler_name: handler_name.to_string(),
            expected: "(req json) RETURNS json | text".to_string(),
            actual: message,
        },
        _ => ServeError::HandlerSqlException {
            handler_name: handler_name.to_string(),
            sqlstate: sqlstate.to_string(),
            message,
            detail,
            hint,
            context,
        },
    }
}

#[cfg(test)]
mod pure_tests {
    //! Pure-Rust tests for the pattern parser + matcher + specificity sort.
    //! No SPI / PG — runs under `cargo test` without pgrx bootstrap.
    use super::*;

    fn parse(p: &str) -> ParsedPattern {
        ParsedPattern::parse(p).expect("parse")
    }

    #[test]
    fn parse_root_pattern_is_zero_segments() {
        let p = parse("/");
        assert!(p.segments.is_empty());
        assert_eq!(p.static_count, 0);
        assert_eq!(p.capture_count, 0);
    }

    #[test]
    fn parse_static_only_pattern() {
        let p = parse("/posts/new");
        assert_eq!(p.static_count, 2);
        assert_eq!(p.capture_count, 0);
        assert_eq!(p.length, 2);
    }

    #[test]
    fn parse_mixed_static_capture() {
        let p = parse("/posts/:id/comments");
        assert_eq!(p.static_count, 2);
        assert_eq!(p.capture_count, 1);
        assert_eq!(p.length, 3);
        assert!(matches!(p.segments[1], PatSeg::Capture(ref n) if n == "id"));
    }

    #[test]
    fn parse_rejects_empty_capture_name() {
        let err = ParsedPattern::parse("/posts/:").unwrap_err();
        assert!(err.contains("empty capture"));
    }

    #[test]
    fn parse_rejects_colon_not_at_segment_start() {
        let err = ParsedPattern::parse("/posts/id:foo").unwrap_err();
        assert!(err.contains("':'"));
    }

    #[test]
    fn matches_static_exact() {
        let p = parse("/posts/new");
        let caps = p.matches(&["posts", "new"]).expect("should match");
        assert!(caps.is_empty());
    }

    #[test]
    fn matches_rejects_segment_count_mismatch() {
        let p = parse("/posts/:id");
        assert!(p.matches(&["posts"]).is_none());
        assert!(p.matches(&["posts", "1", "extra"]).is_none());
    }

    #[test]
    fn matches_captures_any_string() {
        // The user's ask: `/posts/123` and `/posts/all` must both match
        // `[id]`; the handler decides what to do with the string.
        let p = parse("/posts/:id");
        let caps = p.matches(&["posts", "123"]).expect("numeric should match");
        assert_eq!(caps.get("id").map(String::as_str), Some("123"));
        let caps = p.matches(&["posts", "all"]).expect("literal should match");
        assert_eq!(caps.get("id").map(String::as_str), Some("all"));
        let caps = p
            .matches(&["posts", "hello-world_42"])
            .expect("mixed chars should match");
        assert_eq!(
            caps.get("id").map(String::as_str),
            Some("hello-world_42")
        );
    }

    #[test]
    fn matches_multiple_captures() {
        let p = parse("/users/:user/posts/:post");
        let caps = p
            .matches(&["users", "alice", "posts", "42"])
            .expect("should match");
        assert_eq!(caps.get("user").map(String::as_str), Some("alice"));
        assert_eq!(caps.get("post").map(String::as_str), Some("42"));
    }

    /// Sort a set of patterns by our specificity rule and return them
    /// in order. Used by the tests below to assert ordering without
    /// re-plumbing the full `lookup_route` flow.
    fn sort_patterns(mut items: Vec<ParsedPattern>) -> Vec<String> {
        items.sort_by(|a, b| {
            b.static_count
                .cmp(&a.static_count)
                .then(a.capture_count.cmp(&b.capture_count))
                .then(b.length.cmp(&a.length))
        });
        items
            .into_iter()
            .map(|p| {
                p.segments
                    .iter()
                    .map(|s| match s {
                        PatSeg::Static(n) => n.clone(),
                        PatSeg::Capture(n) => format!(":{n}"),
                    })
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect()
    }

    #[test]
    fn sort_static_beats_capture_at_same_length() {
        // /posts/new (2 static) should come before /posts/:id (1 static, 1 cap).
        let order = sort_patterns(vec![parse("/posts/:id"), parse("/posts/new")]);
        assert_eq!(order, vec!["posts/new", "posts/:id"]);
    }

    #[test]
    fn sort_prefers_fewer_captures_at_same_static() {
        // Both have 1 static segment; the one with 0 captures wins.
        // (Contrived — /foo and /foo/:id — distinct by length anyway, but
        // confirms the tiebreaker when static_count matches.)
        let order = sort_patterns(vec![parse("/foo/:x"), parse("/foo")]);
        // length desc: /foo/:x (2) before /foo (1) if static equal (they're 1 each).
        // But static_count differs: /foo=1 static, /foo/:x=1 static + 1 cap.
        // So /foo goes last because it has fewer segments, despite both having
        // 1 static segment. Actually /foo has length=1, /foo/:x has length=2.
        // Higher static_count first: tied (1 each). Lower capture_count first:
        // /foo (0) before /foo/:x (1). Final order: /foo first.
        assert_eq!(order, vec!["foo", "foo/:x"]);
    }

    #[test]
    fn sort_longer_more_specific_at_same_static_and_cap() {
        // /a/:x/b (2 static, 1 cap, len 3) vs /a/:x (1 static, 1 cap, len 2)
        // Higher static wins: /a/:x/b first.
        let order = sort_patterns(vec![parse("/a/:x"), parse("/a/:x/b")]);
        assert_eq!(order, vec!["a/:x/b", "a/:x"]);
    }

    #[test]
    fn request_segments_strips_leading_and_empty() {
        assert_eq!(request_segments("/posts/42"), vec!["posts", "42"]);
        assert_eq!(request_segments("/"), Vec::<&str>::new());
        assert_eq!(
            request_segments("///posts///42//"),
            vec!["posts", "42"],
            "empty segments from adjacent slashes should be filtered"
        );
    }

    #[test]
    fn inject_path_params_overwrites_empty_object() {
        let mut req = serde_json::json!({
            "body": {},
            "query": {},
            "method": "GET",
            "path": "/posts/42",
            "path_params": {}
        });
        let mut caps = BTreeMap::new();
        caps.insert("id".to_string(), "42".to_string());
        inject_path_params(&mut req, &caps);
        assert_eq!(
            req.get("path_params")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str()),
            Some("42")
        );
    }

    #[test]
    fn is_safe_ident_accepts_dollar_for_captures() {
        assert!(is_safe_ident("pgweb.pages__posts__$id__index"));
        assert!(is_safe_ident("pgweb.pages__users__$user__posts__$post__index"));
    }

    #[test]
    fn is_safe_ident_rejects_leading_dollar_or_digit() {
        assert!(!is_safe_ident("$foo"));
        assert!(!is_safe_ident("1foo"));
    }

    // ---- response envelope parsing (prompt 013) ----

    #[test]
    fn parse_envelope_detects_minimal_redirect() {
        let text = r#"{"$pgweb":{"status":303,"headers":{"Location":"/dash"}}}"#;
        let env = parse_response_envelope(text).expect("should parse as envelope");
        assert_eq!(env.status, Some(303));
        assert_eq!(env.headers, vec![("Location".to_string(), "/dash".to_string())]);
        assert!(env.body.is_none());
    }

    #[test]
    fn parse_envelope_with_body_and_cookies() {
        let text = r#"{"$pgweb":{"content_type":"application/json","cookies":["sess=abc; HttpOnly"]},"body":"{\"ok\":1}"}"#;
        let env = parse_response_envelope(text).expect("envelope");
        assert_eq!(env.content_type.as_deref(), Some("application/json"));
        assert_eq!(env.cookies, vec!["sess=abc; HttpOnly".to_string()]);
        assert_eq!(env.body.as_deref(), Some("{\"ok\":1}"));
    }

    #[test]
    fn parse_envelope_context_for_tera_mode() {
        let text = r#"{"$pgweb":{"status":200},"context":{"err":"bad"}}"#;
        let env = parse_response_envelope(text).expect("envelope");
        assert!(env.body.is_none());
        assert!(env.context.is_some());
    }

    #[test]
    fn parse_plain_json_is_not_envelope() {
        // AC6: ordinary data containing status/body must not be mis-detected.
        let text = r#"{"status":"ok","body":"x"}"#;
        assert!(parse_response_envelope(text).is_none());
    }

    #[test]
    fn parse_non_json_or_bare_text_is_not_envelope() {
        assert!(parse_response_envelope("<li>hi</li>").is_none());
        assert!(parse_response_envelope("not json {").is_none());
    }
}

// SPI-requiring tests. Gated on `pg_test` so `cargo test` (pure Rust) skips
// them; `cargo pgrx test pg17` runs them inside a live Postgres with the
// extension installed. Same gating discipline as schema.rs::tests — see the
// note there for why we avoid plain `cfg(test)`.
//
// Module must be named `tests` (matching schema.rs::tests): pgrx's test
// framework invokes each `#[pg_test]` via `SELECT tests.<fn_name>()` with
// a hardcoded `tests` schema, so other names produce `function <schema>.<name>
// does not exist` at test time. Duplicate `CREATE SCHEMA tests` across files
// is safe because pgrx emits IF NOT EXISTS.
#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn lookup_route_exact_static_match() {
        // Seeded `GET /` route is already in the DB from install SQL.
        let matched = super::lookup_route("GET", "/")
            .expect("lookup should not error")
            .expect("seeded GET / should match");
        assert_eq!(matched.route.handler_name, "pgweb.hello_handler");
        assert!(matched.path_params.is_empty());
    }

    #[pg_test]
    fn lookup_route_dynamic_capture_numeric() {
        // Insert a handler for /posts/:id and a matching route.
        Spi::run(
            "CREATE FUNCTION pgweb.pages__posts__$id__index(req json) RETURNS text AS $$ \
             SELECT req->'path_params'->>'id' $$ LANGUAGE sql",
        )
        .expect("create handler");
        Spi::run(
            "INSERT INTO pgweb.routes (method, path_pattern, handler_name, template_path) \
             VALUES ('GET', '/posts/:id', 'pgweb.pages__posts__$id__index', NULL)",
        )
        .expect("insert dynamic route");

        let matched = super::lookup_route("GET", "/posts/42")
            .expect("lookup should not error")
            .expect("/posts/42 should match /posts/:id");
        assert_eq!(
            matched.route.handler_name,
            "pgweb.pages__posts__$id__index"
        );
        assert_eq!(
            matched.path_params.get("id").map(String::as_str),
            Some("42"),
            "capture 'id' should equal the URL segment"
        );
    }

    #[pg_test]
    fn lookup_route_dynamic_capture_accepts_literal_strings() {
        // /posts/all must match /posts/:id just like /posts/123 does —
        // captures are raw strings and handlers decide what to do with them.
        Spi::run(
            "CREATE FUNCTION pgweb.pages__posts__$id__index(req json) RETURNS text AS $$ \
             SELECT 'x' $$ LANGUAGE sql",
        )
        .expect("create handler");
        Spi::run(
            "INSERT INTO pgweb.routes (method, path_pattern, handler_name, template_path) \
             VALUES ('GET', '/posts/:id', 'pgweb.pages__posts__$id__index', NULL)",
        )
        .expect("insert dynamic route");

        let matched = super::lookup_route("GET", "/posts/all")
            .expect("lookup should not error")
            .expect("/posts/all should match");
        assert_eq!(
            matched.path_params.get("id").map(String::as_str),
            Some("all")
        );
    }

    #[pg_test]
    fn lookup_route_static_beats_capture() {
        // Both /posts/new (static) and /posts/:id (dynamic) exist.
        // /posts/new must resolve to the static handler, not the dynamic one.
        Spi::run(
            "CREATE FUNCTION pgweb.pages__posts__new__index(req json) RETURNS text AS $$ \
             SELECT 'static' $$ LANGUAGE sql",
        )
        .expect("create static handler");
        Spi::run(
            "CREATE FUNCTION pgweb.pages__posts__$id__index(req json) RETURNS text AS $$ \
             SELECT 'dynamic' $$ LANGUAGE sql",
        )
        .expect("create dynamic handler");
        Spi::run(
            "INSERT INTO pgweb.routes (method, path_pattern, handler_name, template_path) \
             VALUES ('GET', '/posts/new', 'pgweb.pages__posts__new__index', NULL), \
                    ('GET', '/posts/:id', 'pgweb.pages__posts__$id__index', NULL)",
        )
        .expect("insert both routes");

        let matched = super::lookup_route("GET", "/posts/new")
            .expect("lookup should not error")
            .expect("/posts/new should match");
        assert_eq!(
            matched.route.handler_name,
            "pgweb.pages__posts__new__index",
            "static /posts/new should beat dynamic /posts/:id"
        );
        assert!(
            matched.path_params.is_empty(),
            "static match should have no captures"
        );
    }

    #[pg_test]
    fn lookup_route_no_match_returns_none() {
        let matched = super::lookup_route("GET", "/nope/no/match").expect("lookup error");
        assert!(matched.is_none());
    }

    // ---- handler-call error classification (Component D) ----

    #[pg_test]
    fn call_handler_missing_function_returns_handler_missing() {
        // No pgweb.pages__ghost exists. The framework wrapper catches the
        // 42883 undefined_function and classify_handler_error maps it.
        let err = super::call_handler(
            "pgweb.pages__ghost",
            &serde_json::json!({
                "body": {}, "query": {}, "method": "GET", "path": "/ghost", "path_params": {}
            }),
        )
        .expect_err("expected HandlerMissing");
        match err {
            crate::errors::ServeError::HandlerMissing { handler_name, route } => {
                assert_eq!(handler_name, "pgweb.pages__ghost");
                assert_eq!(route, "/ghost");
            }
            other => panic!("expected HandlerMissing, got {other:?}"),
        }
    }

    #[pg_test]
    fn call_handler_runtime_sql_error_returns_sql_exception() {
        // Create a handler that divides by zero at execution time.
        pgrx::Spi::run(
            "CREATE FUNCTION pgweb.pages__boom(req json) RETURNS json AS $$ \
             SELECT json_build_object('x', 1/0) $$ LANGUAGE sql",
        )
        .expect("create boom");
        let err = super::call_handler(
            "pgweb.pages__boom",
            &serde_json::json!({
                "body": {}, "query": {}, "method": "GET", "path": "/", "path_params": {}
            }),
        )
        .expect_err("expected HandlerSqlException");
        match err {
            crate::errors::ServeError::HandlerSqlException {
                handler_name,
                sqlstate,
                message,
                ..
            } => {
                assert_eq!(handler_name, "pgweb.pages__boom");
                // 22012 division_by_zero
                assert_eq!(sqlstate, "22012");
                assert!(message.to_lowercase().contains("division"));
            }
            other => panic!("expected HandlerSqlException, got {other:?}"),
        }
    }

    #[pg_test]
    fn call_handler_happy_path_returns_text() {
        pgrx::Spi::run(
            "CREATE FUNCTION pgweb.pages__happy(req json) RETURNS text AS $$ \
             SELECT 'ok' $$ LANGUAGE sql",
        )
        .expect("create happy");
        let s = super::call_handler(
            "pgweb.pages__happy",
            &serde_json::json!({
                "body": {}, "query": {}, "method": "GET", "path": "/", "path_params": {}
            }),
        )
        .expect("happy path should succeed");
        assert_eq!(s, "ok");
    }

    // ---- settings ----

    #[pg_test]
    fn settings_current_env_defaults_to_development_from_seed() {
        let env = crate::settings::current_env();
        assert_eq!(env, crate::settings::Env::Development);
    }

    #[pg_test]
    fn settings_current_env_reads_production_when_set() {
        pgrx::Spi::run(
            "UPDATE pgweb.settings SET value = 'production' WHERE key = 'env'",
        )
        .expect("update env to production");
        let env = crate::settings::current_env();
        assert_eq!(env, crate::settings::Env::Production);
    }

    // ---- static assets (Component E) ----

    #[pg_test]
    fn lookup_asset_returns_none_when_missing() {
        let a = super::lookup_asset("/nothing-here.css").expect("lookup");
        assert!(a.is_none());
    }

    #[pg_test]
    fn lookup_asset_roundtrips_bytes_and_headers() {
        // 'body{}' in hex = 62 6f 64 79 7b 7d.
        pgrx::Spi::run(
            "INSERT INTO pgweb.assets (path, content, content_type, etag) \
             VALUES ('/styles.css', decode('626f64797b7d', 'hex'), 'text/css', '\"abc123\"')",
        )
        .expect("insert asset");
        let a = super::lookup_asset("/styles.css")
            .expect("lookup")
            .expect("row present");
        assert_eq!(a.content, b"body{}".to_vec());
        assert_eq!(a.content_type, "text/css");
        assert_eq!(a.etag, "\"abc123\"");
    }

    #[pg_test]
    fn lookup_asset_returns_svg_bytes_verbatim() {
        // '<svg' = 3c 73 76 67.
        pgrx::Spi::run(
            "INSERT INTO pgweb.assets (path, content, content_type, etag) \
             VALUES ('/logo.svg', decode('3c737667', 'hex'), 'image/svg+xml', '\"svg1\"')",
        )
        .expect("insert svg");
        let a = super::lookup_asset("/logo.svg")
            .expect("lookup")
            .expect("row present");
        assert_eq!(a.content, b"<svg".to_vec());
        assert_eq!(a.content_type, "image/svg+xml");
        assert_eq!(a.etag, "\"svg1\"");
    }

    // Note: we don't test the route-vs-asset precedence via `serve()` at the
    // pg_test layer because `serve` wraps the call in
    // `BackgroundWorker::transaction`, which panics outside a registered
    // BGW. The precedence is covered by the tier-3 smoke: the demo has a
    // page route at `/` + a `public/styles.css` asset, and the smoke asserts
    // the page renders at / while `/styles.css` serves the asset.
}
