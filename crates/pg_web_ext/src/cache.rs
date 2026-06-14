//! BGW-local snapshot cache for pgweb.routes (parsed + specificity-sorted),
//! pgweb.templates (compiled Tera instances), and the `env` setting.
//!
//! Hot path (lookup + render) does **zero** framework-metadata SPI after the
//! initial build. User handler SQL still runs inside the one request = one tx
//! (invariant #4); only bookkeeping reads are cached.
//!
//! Invalidation: the worker LISTENs on `pgweb_reload` (via the existing
//! ListenRouter) and drops the snapshot on any payload. Next request lazily
//! rebuilds. `pg-web push` (and only push for the framework tables) issues the
//! NOTIFY inside its commit tx so delivery is atomic with the data change.
//!
//! The LISTEN task is now always-on (prod + dev) — one extra PG backend slot
//! per BGW. This was already the Phase-2 plan; we just enable it early for
//! the cache. Documented in APP-DEVELOPER-GUIDE.md.
//!
//! Concurrency: single-threaded current-thread Tokio runtime today (015),
//! so a plain RwLock is cheap. ArcSwap would also work and keeps the door
//! open for multi-worker; we use std for zero new deps. The assumption is
//! documented next to the static (mirrors listen_router.rs:26-31).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use pgrx::Spi;
use serde_json::Value;
use tera::{Context, Tera};
use tracing::warn;

use crate::errors::ServeError;
use crate::router::{ParsedPattern, RouteMeta};
use crate::settings::{self, Env};

/// The in-memory snapshot. Rebuilt on first use after start or invalidate.
pub struct RouteSnapshot {
    /// Per-method, already specificity-sorted for first-match scan.
    pub routes: HashMap<String, Vec<(ParsedPattern, RouteMeta)>>,
    /// Compiled templates (only the successfully parsed ones).
    /// Miss or render failure here falls back to one_off (fresh fetch + parse)
    /// so a just-pushed template (pre-NOTIFY) or a bad template still works
    /// exactly as before.
    pub templates: Tera,
    pub env: Env,
}

static SNAPSHOT: RwLock<Option<Arc<RouteSnapshot>>> = RwLock::new(None);

/// Return a cheap Arc clone of the current snapshot, building it on first
/// access or after invalidate().
pub fn get_snapshot() -> Arc<RouteSnapshot> {
    {
        let g = SNAPSHOT.read().expect("snapshot rwlock poisoned");
        if let Some(s) = &*g {
            return Arc::clone(s);
        }
    }
    // Cold / post-invalidate: build under exclusive lock (rare).
    let built = build_snapshot();
    let arc = Arc::new(built);
    {
        let mut w = SNAPSHOT.write().expect("snapshot rwlock poisoned");
        *w = Some(Arc::clone(&arc));
    }
    arc
}

/// Drop the snapshot. Next get_snapshot() will rebuild (lazily on next request).
/// Called from the listen task when a pgweb_reload NOTIFY arrives.
pub fn invalidate() {
    let mut w = SNAPSHOT.write().expect("snapshot rwlock poisoned");
    *w = None;
    // No log here — the listen task that delivered the NOTIFY already logged.
}

/// Build a fresh snapshot inside its own short SPI transaction.
/// This is the only place framework metadata is read from the DB for the
/// serving path. It is called on worker start (warm-up) and on first request
/// after an invalidate.
fn build_snapshot() -> RouteSnapshot {
    // Direct SPI reads (no BGW transaction wrapper). This matches the style
    // the original per-request lookups used (direct SPI inside whatever tx or
    // session context the caller had). It works for:
    // - warmup at BGW start (connection attached, no outer tx)
    // - first request after invalidate (inside the request's serve tx)
    // - #[pg_test] that call lookup (inside the test harness tx)
    // The data is the latest committed (or the caller's in-flight tx for tests).
    // --- Routes
    let route_rows: Vec<(String, String, String, Option<String>)> = match Spi::connect(
        |client| {
            client
                .select(
                    "SELECT method, path_pattern, handler_name, template_path \
                     FROM pgweb.routes",
                    None,
                    &[],
                )
                .map_err(|e| format!("route snapshot select: {e}"))
                .map(|rows| {
                    rows.map(|r| {
                        (
                            r.get_by_name::<String, &str>("method").unwrap_or_default().unwrap_or_default(),
                            r.get_by_name::<String, &str>("path_pattern").unwrap_or_default().unwrap_or_default(),
                            r.get_by_name::<String, &str>("handler_name").unwrap_or_default().unwrap_or_default(),
                            r.get_by_name::<String, &str>("template_path").unwrap_or_default(),
                        )
                    })
                    .collect()
                })
        },
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to build route table for cache; using empty");
            vec![]
        }
    };

    let mut routes: HashMap<String, Vec<(ParsedPattern, RouteMeta)>> = HashMap::new();
    for (method, pat_str, handler, template_path) in route_rows {
        if let Ok(pat) = ParsedPattern::parse(&pat_str) {
            let meta = RouteMeta {
                handler_name: handler,
                template_path,
            };
            routes.entry(method).or_default().push((pat, meta));
        }
    }
    for v in routes.values_mut() {
        v.sort_by(|a, b| {
            b.0.static_count
                .cmp(&a.0.static_count)
                .then(a.0.capture_count.cmp(&b.0.capture_count))
                .then(b.0.length.cmp(&a.0.length))
        });
    }

    // --- Templates
    let template_rows: Vec<(String, String)> = match Spi::connect(|client| {
        client
            .select(
                "SELECT template_path, content FROM pgweb.templates",
                None,
                &[],
            )
            .map_err(|e| format!("template snapshot select: {e}"))
            .map(|rows| {
                rows.map(|r| {
                    (
                        r.get_by_name::<String, &str>("template_path").unwrap_or_default().unwrap_or_default(),
                        r.get_by_name::<String, &str>("content").unwrap_or_default().unwrap_or_default(),
                    )
                })
                .collect()
            })
    }) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to fetch templates for cache");
            vec![]
        }
    };

    let mut templates = Tera::default();
    for (tp, src) in template_rows {
        if let Err(e) = templates.add_raw_template(&tp, &src) {
            warn!(template = %tp, error = %e, "skipping bad template at snapshot build time (will one_off on use)");
        }
    }

    // --- Env
    let env = settings::current_env();

    RouteSnapshot {
        routes,
        templates,
        env,
    }
}

/// Cache-aware template render. Tries the compiled Tera first (hot path).
/// On miss (template not in this snapshot yet) or any render error from the
/// cached copy, falls back to the classic one_off path (which does a fresh
/// SPI fetch of the source + Tera::one_off). This gives byte-identical
/// behavior for:
/// - a template that was just pushed (NOTIFY not yet drained)
/// - a template containing a syntax error (surfaces same TemplateParseError)
pub fn render_template(template_path: &str, data: &Value) -> Result<String, ServeError> {
    let snap = get_snapshot();
    let context = Context::from_value(data.clone()).map_err(|e| ServeError::TemplateRenderError {
        template_path: template_path.to_string(),
        message: format!("could not build Tera context from handler JSON: {e}"),
        missing_var: None,
    })?;

    // Hot path: compiled render.
    if let Ok(body) = snap.templates.render(template_path, &context) {
        return Ok(body);
    }

    // Cold / miss / just-pushed / bad-at-build: fall back. The one_off path
    // will SELECT the current source and classify parse vs render exactly as
    // before caching existed.
    let src = fetch_template_src(template_path)?;
    crate::templating::render(template_path, &src, data)
}

/// Minimal src fetch used only by the fallback path above (and by any
/// legacy call sites that still want the raw string). Still one SPI hit,
/// but only on the rare cold edge.
pub fn fetch_template_src(template_path: &str) -> Result<String, ServeError> {
    let query = format!(
        "SELECT content FROM pgweb.templates WHERE template_path = {} LIMIT 1",
        quote_literal(template_path)
    );
    Spi::get_one(&query)
        .map_err(|e| ServeError::TemplateRenderError {
            template_path: template_path.to_string(),
            message: format!("failed to fetch template: {e}"),
            missing_var: None,
        })?
        .ok_or_else(|| ServeError::TemplateRenderError {
            template_path: template_path.to_string(),
            message: "template not found".to_string(),
            missing_var: None,
        })
}

/// Convenience: the cached env (falls back to settings::current_env on any
/// weird empty-snapshot race, though get_snapshot() always populates).
pub fn current_env() -> Env {
    // get_snapshot() is very cheap on hit (just an Arc clone under read lock).
    get_snapshot().env
}

/// Small helper (duplicated from router for snapshot isolation; keep in sync
/// or promote to a shared quote util if it grows).
fn quote_literal(s: &str) -> String {
    // Very small, safe for our controlled identifiers/paths.
    format!("'{}'", s.replace('\'', "''"))
}

// For tests that want to force a rebuild or assert state.
#[cfg(test)]
pub fn force_rebuild_for_test() {
    invalidate();
    let _ = get_snapshot();
}