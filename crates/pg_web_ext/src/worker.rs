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
use tracing::{error, info, warn};

use crate::listen_router::{self, ListenRouter};
use crate::livereload;
use crate::settings::{self, Env};
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

        // Build the shared LISTEN fan-out. Always exists so HTTP
        // handlers have a stable state type, but the actual LISTEN
        // task only starts in dev mode (see below). Prod = zero
        // extra Postgres backend slots from this machinery.
        let router = ListenRouter::new();
        router.preregister(livereload::LIVERELOAD_CHANNEL);

        let env = BackgroundWorker::transaction(settings::current_env);
        if env == Env::Development {
            let conn_str = build_listen_conn_str(&target_db);
            let router_clone = router.clone();
            let channels = vec![livereload::LIVERELOAD_CHANNEL.to_string()];
            tokio::spawn(listen_router::run_listen_loop(
                router_clone,
                conn_str,
                channels,
            ));
            info!("livereload LISTEN task started (env=development)");
        } else {
            info!("livereload LISTEN task skipped (env=production)");
        }

        info!(addr = %addr, db = %target_db, "listening");

        if let Err(e) = axum::serve(listener, http::app(router)).await {
            error!(error = %e, "server exited with error");
        }
    });
}

/// Build the tokio-postgres connection string the livereload LISTEN
/// task uses to reach the same Postgres instance we're running inside.
///
/// Resolves:
/// - port from `pg_sys::PostPortNumber` (whatever Postgres is listening
///   on; works for both pgrx dev and docker),
/// - user / password from `POSTGRES_USER` / `POSTGRES_PASSWORD` env,
///   falling back to `postgres` / empty (pgrx dev uses trust on
///   loopback so the password is usually unneeded).
///
/// Host is always `127.0.0.1` — loopback round-trip through the TCP
/// stack is cheap and avoids any unix-socket path discovery dance.
fn build_listen_conn_str(db: &str) -> String {
    // SAFETY: PostPortNumber is a Postgres C global; reading an int
    // without synchronization is fine in practice (postmaster sets it
    // at startup and nothing mutates it at runtime).
    let port = unsafe { pg_sys::PostPortNumber };
    let user = std::env::var("POSTGRES_USER").unwrap_or_else(|_| "postgres".to_string());
    let password = std::env::var("POSTGRES_PASSWORD").unwrap_or_default();
    if password.is_empty() {
        warn!(
            "POSTGRES_PASSWORD unset; livereload LISTEN will only work if pg_hba \
             allows trust/peer auth from 127.0.0.1"
        );
    }
    format!(
        "host=127.0.0.1 port={port} user={user} dbname={db} password={password}"
    )
}
