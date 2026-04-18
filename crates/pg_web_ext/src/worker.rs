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

    logging::init();

    let addr = SocketAddr::from(([0, 0, 0, 0], HTTP_PORT));

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("pg-web-rt")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!(error = %e, "failed to build tokio runtime");
            return;
        }
    };

    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(addr = %addr, error = %e, "bind failed");
                return;
            }
        };

        info!(addr = %addr, "listening");

        let app = http::app();

        if let Err(e) = axum::serve(listener, app).await {
            error!(error = %e, "server exited with error");
        }
    });
}
