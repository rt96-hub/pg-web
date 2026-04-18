//! Tracing setup for the background worker.
//!
//! One line per event. Quiet deps by default. Structured fields.
//! See `docs/DEVELOPER-GUIDE.md` § Common pitfalls for the logging philosophy.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Default filter — quiet deps, info for our own crate.
/// Override at runtime with `RUST_LOG=pg_web_ext=debug,axum=info` etc.
const DEFAULT_FILTER: &str =
    "pg_web_ext=info,axum=warn,tower=warn,tower_http=warn,hyper=warn,tokio=warn";

/// Initialize tracing. Idempotent — safe to call multiple times; only the first wins.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_target(true)
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_file(false)
                .with_line_number(false)
                .compact(),
        )
        .try_init();
}
