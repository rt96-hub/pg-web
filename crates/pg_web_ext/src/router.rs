//! Route resolution via SPI.
//!
//! Single-threaded Tokio + `BackgroundWorker::connect_worker_to_spi`:
//! synchronous SPI calls are safe inside the async handler because it
//! runs on the SPI-attached main thread.
//!
//! Parameterized queries use Rust-side escaping rather than `DatumWithOid`
//! arrays — rustc 1.95 hits an ICE on the latter in this crate. Revisit
//! when M1.2 dynamic routes land or the pgrx/rustc issue is resolved.

use pgrx::bgworkers::BackgroundWorker;
use pgrx::Spi;
use serde_json::Value;

use crate::templating;

pub enum ServeOutcome {
    Html(String),
    NotFound,
    Error(String),
}

pub fn serve(method: &str, path: &str) -> ServeOutcome {
    // One HTTP request → one Postgres transaction. Mandatory in a BGW
    // context: `Spi::*` asserts there's an active transaction + snapshot,
    // and `BackgroundWorker::transaction` is what sets those up.
    let method = method.to_string();
    let path = path.to_string();
    BackgroundWorker::transaction(move || serve_in_tx(&method, &path))
}

fn serve_in_tx(method: &str, path: &str) -> ServeOutcome {
    let route = match lookup_route(method, path) {
        Ok(Some(r)) => r,
        Ok(None) => return ServeOutcome::NotFound,
        Err(e) => return ServeOutcome::Error(e),
    };
    let template = match fetch_template(&route.template_path) {
        Ok(t) => t,
        Err(e) => return ServeOutcome::Error(e),
    };
    let data = match call_handler(&route.handler_name) {
        Ok(d) => d,
        Err(e) => return ServeOutcome::Error(e),
    };
    match templating::render(&template, &data) {
        Ok(html) => ServeOutcome::Html(html),
        Err(e) => ServeOutcome::Error(e),
    }
}

struct Route {
    handler_name: String,
    template_path: String,
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

/// `Spi::get_one` on a query that matched zero rows returns
/// `Err(SpiError::InvalidPosition)` rather than `Ok(None)` — quirk of how
/// the tuple table is positioned. Normalize: treat "no rows" as `Ok(None)`,
/// anything else as a real error.
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
    let handler_query = format!(
        "SELECT handler_name FROM pgweb.routes WHERE method = {} AND path_pattern = {} LIMIT 1",
        quote_literal(method),
        quote_literal(path)
    );
    let handler_name = match get_one_optional::<String>(&handler_query)? {
        Some(s) => s,
        None => return Ok(None),
    };
    let template_query = format!(
        "SELECT template_path FROM pgweb.routes WHERE method = {} AND path_pattern = {} LIMIT 1",
        quote_literal(method),
        quote_literal(path)
    );
    let template_path = match get_one_optional::<String>(&template_query)? {
        Some(s) => s,
        None => return Err("route row disappeared between queries".to_string()),
    };
    Ok(Some(Route { handler_name, template_path }))
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

fn call_handler(handler_name: &str) -> Result<Value, String> {
    if !is_safe_ident(handler_name) {
        return Err(format!("handler name rejected: {handler_name:?}"));
    }
    let query = format!("SELECT ({handler_name}())::text AS result");
    let json_str = match get_one_optional::<String>(&query)? {
        Some(s) => s,
        None => return Err(format!("handler {handler_name}: returned no row")),
    };
    serde_json::from_str::<Value>(&json_str).map_err(|e| e.to_string())
}
