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

/// Database the worker connects to for SPI.
///
/// TODO(M1.4): read from a `pgweb.database` GUC so production deployments
/// can point the worker at the user's application database. Hardcoded for
/// M1.1 because `pg_web_ext` is pgrx's default dev DB name.
const TARGET_DATABASE: &str = "pg_web_ext";

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

    // Attach this OS thread to a Postgres backend connection on TARGET_DATABASE.
    // Required before any `Spi::*` call. Only this thread will have SPI access —
    // hence the single-threaded Tokio runtime below.
    BackgroundWorker::connect_worker_to_spi(Some(TARGET_DATABASE), None);

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

        info!(addr = %addr, db = TARGET_DATABASE, "listening");

        if let Err(e) = axum::serve(listener, http::app()).await {
            error!(error = %e, "server exited with error");
        }
    });
}
