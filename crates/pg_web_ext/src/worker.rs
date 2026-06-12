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

/// Least-privilege role the worker's SPI session connects as (prompt 014).
/// Created by `CREATE EXTENSION pg_web_ext` (see the role block in
/// schema.rs): NOLOGIN + NOSUPERUSER + NOBYPASSRLS + NOCREATEDB +
/// NOCREATEROLE, no password. The worker adopts it despite NOLOGIN via
/// `BGWORKER_BYPASS_ROLELOGINCHECK` (see [`connect_spi_as_serving_role`]).
/// Unrelated to the NOTIFY channel prefix `pgweb_app_<ch>` in
/// listen_router.rs.
const SERVING_ROLE: &str = "pgweb_app";

fn resolve_target_database() -> String {
    std::env::var(TARGET_DATABASE_ENV)
        .or_else(|_| std::env::var("POSTGRES_DB"))
        .unwrap_or_else(|_| FALLBACK_DATABASE.to_string())
}

/// Connect the worker's SPI session to `target_db` as [`SERVING_ROLE`].
///
/// pgrx 0.18's `BackgroundWorker::connect_worker_to_spi` hardcodes
/// `flags = 0`, under which `BackgroundWorkerInitializeConnection` enforces
/// the role's `rolcanlogin` — and our serving role is deliberately NOLOGIN.
/// So on PG 17 we call the `pg_sys` initializer directly with
/// `BGWORKER_BYPASS_ROLELOGINCHECK`. This replicates the pgrx wrapper
/// exactly: the wrapper only asserts `MyBgworkerEntry` is set and marshals
/// the two strings before the same call — it keeps no other internal state
/// (verified against pgrx-0.18.0/src/bgworkers.rs).
///
/// PG 15/16: `BGWORKER_BYPASS_ROLELOGINCHECK` does not exist in those
/// headers (it was added in PG 17), so they keep `flags = 0` and a NOLOGIN
/// serving role FATALs at connect ("role ... is not permitted to log in").
/// Accepted: per the 2026-06-12 version-gate decision, only the bundled
/// image major (PG 17) must be correct at runtime; pg15/pg16 need only
/// compile.
fn connect_spi_as_serving_role(target_db: &str) {
    // Same precondition as the pgrx wrapper: must be inside a registered BGW.
    unsafe {
        assert!(
            !pg_sys::MyBgworkerEntry.is_null(),
            "connect_spi_as_serving_role can only be called from a registered background worker"
        );
    }

    let db = std::ffi::CString::new(target_db).ok();
    let db = db.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
    let user = std::ffi::CString::new(SERVING_ROLE).ok();
    let user = user.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());

    #[cfg(feature = "pg17")]
    let flags = pg_sys::BGWORKER_BYPASS_ROLELOGINCHECK;
    // pg15/pg16: no ROLELOGINCHECK bypass in the headers — compile-only
    // majors; NOLOGIN role means the worker cannot serve there (accepted).
    #[cfg(not(feature = "pg17"))]
    let flags = 0;

    unsafe {
        pg_sys::BackgroundWorkerInitializeConnection(db, user, flags);
    }
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
    // Connect the worker's SPI session as the dedicated least-privilege
    // serving role (prompt 014). This is the hard privilege floor under all
    // user handler execution and under the framework's own per-request SPI
    // calls (route lookup, templates, assets, settings, _framework_call_handler).
    // A1 chosen over SET LOCAL ROLE because the *connection identity* itself
    // is non-superuser; RESET ROLE cannot escalate.
    //
    // The role is NOLOGIN (see the role block in schema.rs): no client
    // (psql/libpq) session can ever authenticate as it, under any pg_hba
    // method. Only this worker can adopt it, because the connect helper
    // below passes BGWORKER_BYPASS_ROLELOGINCHECK (PG 17; pg15/pg16 are
    // compile-only majors — see connect_spi_as_serving_role).
    //
    // The LISTEN loopback (build_listen_conn_str) is a separate
    // tokio-postgres client connection authenticated via POSTGRES_* env /
    // pg_hba; it is unaffected by the SPI role and must keep working for
    // `pg-web dev` livereload.
    //
    // Bootstrap order: the postmaster starts this worker before anyone has
    // necessarily run CREATE EXTENSION, so the role (created by the
    // extension's install SQL) may not exist yet. In that case the connect
    // below raises FATAL ("role \"pgweb_app\" does not exist"), the worker
    // exits, and the postmaster restarts it every 5s (bgw_restart_time)
    // until the extension is installed — a clean self-healing retry loop.
    // The pre-connect log line below sits right above the FATAL in the
    // server log so the crash-loop is self-explanatory.
    pgrx::log!(
        "pg_web_worker: connecting SPI to database \"{target_db}\" as role \
         \"{SERVING_ROLE}\"; if this FATALs with 'role does not exist', run \
         CREATE EXTENSION pg_web_ext in that database (the worker retries \
         every 5s)"
    );
    connect_spi_as_serving_role(&target_db);

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

        // One startup transaction: read env + observe the SPI identity. The
        // identity check is belt-and-braces — connect_spi_as_serving_role
        // FATALs on a missing role rather than falling back to superuser —
        // but logging it makes the privilege floor (014) verifiable from
        // the server log, and a mismatch is loud instead of silent.
        let (env, spi_user) = BackgroundWorker::transaction(|| {
            let env = settings::current_env();
            let who = pgrx::Spi::get_one::<String>("SELECT current_user")
                .ok()
                .flatten()
                .unwrap_or_else(|| "<unknown>".to_string());
            (env, who)
        });
        if spi_user != SERVING_ROLE {
            warn!(
                spi_user = %spi_user,
                expected = SERVING_ROLE,
                "SPI identity is not the expected serving role; the 014 privilege floor is not in effect"
            );
        }
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

        info!(addr = %addr, db = %target_db, role = %spi_user, "listening");

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
