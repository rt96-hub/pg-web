//! Protected platform health/readiness probes (`/_pgweb/health`, `/_pgweb/readiness`).
//!
//! These are hard-mounted in Axum (like livereload) **above** the user router
//! fallback. They are:
//! - Unconditionally available (no env gate, no user-route override possible).
//! - Intentionally trivial and cheap (one SPI round-trip inside the normal
//!   per-request BGW transaction; no second listener, no extra threads).
//! - The correct target for Dockerfile HEALTHCHECK, load-balancers, and
//!   orchestrator probes. Pointing probes at a user route (or even the seeded
//!   `/`) was the footgun that motivated this work.
//!
//! The public conventional endpoints (`/health`, `/readiness`) are separate:
//! they are seeded as overridable defaults in the bootstrap (see schema.rs)
//! and may be suppressed via `pgweb.settings.health_enabled` / `readiness_enabled`.
//! User routes for those paths always win regardless of the flags.
//!
//! Body format is deliberately small JSON today. Richer envelopes, version
//! pinning (post 018.2), or additional checks can evolve later without
//! changing the mount contract.

use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use pgrx::bgworkers::BackgroundWorker;
use pgrx::Spi;

/// GET `/_pgweb/health` — liveness probe.
///
/// Answers: "is the pg-web background worker serving HTTP and able to
/// perform a trivial SPI operation against its backend?"
///
/// Returns 200 + {"status":"ok"} on success.
/// Returns 503 + {"status":"error"} if the SPI probe fails (BGW alive but
/// DB or extension in a bad state).
///
/// This is the target the Dockerfile HEALTHCHECK (and real infra) should use.
/// It is unaffected by user handlers, slow `GET /`, or a user-overridden
/// `/health` that 500s.
pub async fn serve_health() -> Response {
    // One cheap transaction. SELECT 1 exercises the SPI path the request
    // handlers use without depending on any user tables or seeded routes.
    let ok = BackgroundWorker::transaction(|| Spi::get_one::<i32>("SELECT 1").is_ok());

    if ok {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"ok"}"#,
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"error"}"#,
        )
            .into_response()
    }
}

/// GET `/_pgweb/readiness` — readiness probe.
///
/// Today (018.1): liveness + a cheap catalog check that the framework schema
/// (`pgweb`) and its core table (`routes`) are present. This is sufficient to
/// tell orchestrators "the extension created its objects and basic serving
/// state exists."
///
/// Once 018.2 (extension upgrade scripts + ALTER EXTENSION) lands, this can
/// grow a lightweight version-sanity check (e.g. against a meta row or
/// pg_available_extensions) without changing the HTTP contract.
///
/// Returns 200 + {"status":"ok"} on success, 503 otherwise.
pub async fn serve_readiness() -> Response {
    let ok = BackgroundWorker::transaction(|| {
        // Fast path: if even SELECT 1 fails we are not live.
        if Spi::get_one::<i32>("SELECT 1").is_err() {
            return false;
        }

        // Framework schema present (the bootstrap extension_sql block ran).
        let schema_ok: Option<bool> = Spi::get_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.schemata \
             WHERE schema_name = 'pgweb')",
        )
        .ok()
        .flatten();

        if !schema_ok.unwrap_or(false) {
            return false;
        }

        // A core table exists (routes is created early in the same block).
        let routes_ok: Option<bool> = Spi::get_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = 'pgweb' AND table_name = 'routes')",
        )
        .ok()
        .flatten();

        routes_ok.unwrap_or(false)
    });

    if ok {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"ok"}"#,
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"error"}"#,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    // Pure unit tests are minimal here; the real coverage is via the
    // HTTP smoke (tier 2a), docker_e2e (tier 3), and smoke-cli (tier 4)
    // which now assert the protected endpoints plus override/disable
    // behavior. The handlers are deliberately side-effect free and tiny.
}
