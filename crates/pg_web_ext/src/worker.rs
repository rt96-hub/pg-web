//! Background worker entry point.
//!
//! Registered in `lib.rs::_PG_init`. Runs as its own OS process owned by the
//! Postgres postmaster. Boots a Tokio runtime and serves HTTP on :8080.
//!
//! The worker name "pg_web_worker" appears in `pg_stat_activity.backend_type`.

use std::net::SocketAddr;

use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
use pgrx::pg_guard;
use pgrx::pg_sys;
use tracing::{error, info};

use crate::{http, logging};

/// Port the HTTP server binds. Hardcoded for M1.1; will become a GUC later.
const HTTP_PORT: u16 = 8080;

/// Env var that selects the database the worker connects to for SPI.
/// Docker deployments set this to match `POSTGRES_DB`. Dev via
/// `cargo pgrx run` falls through to the default below.
const TARGET_DATABASE_ENV: &str = "PGWEB_DATABASE";

/// Fallback database name when neither `PGWEB_DATABASE` nor `POSTGRES_DB`
/// are set. Matches pgrx's dev default so `cargo pgrx run pg17` works
/// without any extra configuration.
const FALLBACK_DATABASE: &str = "pg_web_ext";

fn resolve_target_database() -> String {
    std::env::var(TARGET_DATABASE_ENV)
        .or_else(|_| std::env::var("POSTGRES_DB"))
        .unwrap_or_else(|_| FALLBACK_DATABASE.to_string())
}

/// Entry point for the background worker process.
///
/// `extern "C-unwind"` (not `extern "C"`) — pgrx 0.18's `#[pg_guard]` expects
/// unwinding to propagate across the FFI boundary. `#[unsafe(no_mangle)]` is
/// the Rust 1.82+ form required by the pg_guard macro expansion.
#[pg_guard]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_web_worker_main(_arg: pg_sys::Datum) {
    // Let Postgres wake us on SIGHUP (reload) and SIGTERM (shutdown).
    BackgroundWorker::attach_signal_handlers(
        SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM,
    );

    // Attach this OS thread to a Postgres backend connection on the target DB.
    // Required before any `Spi::*` call. Only this thread will have SPI access —
    // hence the single-threaded Tokio runtime below.
    let target_db = resolve_target_database();
    BackgroundWorker::connect_worker_to_spi(Some(&target_db), None);

    logging::init();

    // Single-threaded current-thread runtime: all async tasks run on this thread,
    // the one with SPI attached. A multi-threaded runtime would let tasks migrate
    // to worker threads that lack SPI access, causing panics on any SQL call.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!(error = %e, "failed to build tokio runtime");
            return;
        }
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], HTTP_PORT));

    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(addr = %addr, error = %e, "bind failed");
                return;
            }
        };

        info!(addr = %addr, db = %target_db, "listening");

        if let Err(e) = axum::serve(listener, http::app()).await {
            error!(error = %e, "server exited with error");
        }
    });
}
