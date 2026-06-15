//! Background worker entry point.
//!
//! Registered in `lib.rs::_PG_init`. Runs as its own OS process owned by the
//! Postgres postmaster. Boots a Tokio runtime and serves HTTP on :8080.
//!
//! The worker name "pg_web_worker" appears in `pg_stat_activity.backend_type`.

use std::net::SocketAddr;
use std::sync::Arc;

use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
use pgrx::pg_guard;
use pgrx::pg_sys;
use tracing::{error, info, warn};

use crate::listen_router::{self, ListenRouter};
use crate::livereload;
use crate::settings;
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
        // handlers have a stable state type. The LISTEN task is now
        // always-on (prod + dev) so that cache invalidation via
        // pgweb_reload reaches the worker in production deploys.
        // Cost: +1 PG backend slot per BGW (documented).
        let router = ListenRouter::new();
        router.preregister(livereload::LIVERELOAD_CHANNEL);
        const RELOAD_CHANNEL: &str = "pgweb_reload";
        router.preregister(RELOAD_CHANNEL);

        // One startup transaction: read env + observe the SPI identity. The
        // identity check is belt-and-braces — connect_spi_as_serving_role
        // FATALs on a missing role rather than falling back to superuser —
        // but logging it makes the privilege floor (014) verifiable from
        // the server log, and a mismatch is loud instead of silent.
        let (_env, spi_user) = BackgroundWorker::transaction(|| {
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
        let conn_str = build_listen_conn_str(&target_db);
        let router_clone = router.clone();
        let channels = vec![
            livereload::LIVERELOAD_CHANNEL.to_string(),
            RELOAD_CHANNEL.to_string(),
        ];
        tokio::spawn(listen_router::run_listen_loop(
            router_clone,
            conn_str,
            channels,
        ));
        info!("LISTEN task started (cache invalidation + livereload; always-on)");

        // Note: cache invalidation on "pgweb_reload" is now handled directly
        // inside the listen pump (see listen_router.rs) when a NOTIFY for that
        // channel is received. This avoids an extra persistent subscriber task
        // + broadcast receiver at startup, which was one of the contributors
        // to the BGW segfaults we saw in container canaries after making the
        // LISTEN always-on for the cache feature.

        // Warm-up is lazy (on first request) to keep startup as close as
        // possible to the pre-cache sequence and avoid any early-SPI timing
        // interactions observed during bring-up. The first request pays the
        // (still bounded) build cost; subsequent requests and post-push
        // requests are fast. Invalidation still works via the reload channel.
        // (Eager warm-up can be re-enabled once the BGW start sequencing is
        // further hardened.)

        info!(addr = %addr, db = %target_db, role = %spi_user, "listening");

        // Graceful shutdown per prompt 016: honor SIGTERM so in-flight requests
        // drain and the postmaster does not have to escalate to SIGKILL. We also
        // request shutdown on the router so SSE streams close promptly (instead
        // of the 2h hard cap), and we cap the drain window *once shutdown begins*.
        //
        // REGRESSION FIX: the 8s cap must bound ONLY the post-SIGTERM drain, not
        // the whole serve. The previous form wrapped the entire `serve_fut` in
        // `tokio::time::timeout(8s, …)`; but `with_graceful_shutdown` resolves
        // only AFTER SIGTERM, so that timer fired 8s after *startup* and the
        // worker exited. Because the worker then returns cleanly (exit code 0),
        // the postmaster does NOT restart it (despite bgw_restart_time = 5s in
        // lib.rs) — so every deployment's HTTP server silently died 8 seconds
        // after boot. None of the tier-3 E2E tests caught it because each one
        // finishes its HTTP work inside that 8s window; the benchmark's
        // "72%-then-0%" was this, not the documented "single-worker reality".
        //
        // The drain deadline is now armed only after `shutdown_signal` observes
        // SIGTERM (signalled via `drain_tx`): before SIGTERM `drain_rx` never
        // resolves, so the cap arm of the select! is inert and the server runs
        // for the postmaster's whole lifetime; after SIGTERM the server drains
        // in-flight work for at most 8s. Regression-guarded by the tier-3 test
        // `worker_serves_past_drain_cap`.
        let router_for_signal = router.clone();
        // Fires exactly once, when SIGTERM is first observed — i.e. the moment
        // the drain clock should start (NOT at startup).
        let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown = async move {
            shutdown_signal(router_for_signal).await; // returns once SIGTERM seen
            let _ = drain_tx.send(()); // receiver is alive here; ignore send error
        };
        let serve_fut = axum::serve(listener, http::app(router))
            .with_graceful_shutdown(shutdown);

        // Pending until SIGTERM, then a fixed 8s budget. A stuck handler or long
        // SSE can't force the postmaster to SIGKILL, but a healthy idle server is
        // never affected because the sleep never starts before SIGTERM.
        let drain_cap = async move {
            // Err only if the sender dropped without sending — which happens when
            // serve_fut completes on its own first, in which case the select!'s
            // serve arm has already won and this value is irrelevant.
            let _ = drain_rx.await;
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        };

        // `biased`: prefer the serve arm so a drain that completes within budget
        // exits cleanly rather than racing the timer.
        tokio::select! {
            biased;
            res = serve_fut => match res {
                Ok(()) => {}
                Err(e) => error!(error = %e, "server exited with error"),
            },
            _ = drain_cap => warn!("graceful drain exceeded 8s after SIGTERM; exiting anyway"),
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

/// Future that completes when we should stop accepting new work and drain.
/// Polls the pgrx SIGTERM flag (set by the handler armed in _PG_init path)
/// on a short interval. This is the primary path for pg_ctl stop / docker stop
/// under the postmaster. (Direct tokio signal is omitted to avoid handler
/// conflicts with pgrx's attach_signal_handlers that have been observed to
/// produce early segfaults in the BGW.)
///
/// Also calls request_shutdown() on the router so SSE streams (livereload)
/// and other waiters can end promptly instead of waiting out their max lifetime.
async fn shutdown_signal(router: Arc<ListenRouter>) {
    use std::time::Duration;
    use tokio::time::interval;

    let mut iv = interval(Duration::from_millis(150));
    loop {
        iv.tick().await;
        if BackgroundWorker::sigterm_received() {
            info!("SIGTERM received via pgrx (pg_ctl / postmaster path) — graceful drain");
            router.request_shutdown();
            return;
        }
    }
}
