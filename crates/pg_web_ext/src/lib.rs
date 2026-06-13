// `ServeError` carries enough structured context (SQLSTATE, message, detail,
// hint, context, template path, req dump helpers) to render a developer-
// friendly error page. That makes it ~144 bytes, which trips
// clippy::result_large_err. Boxing it would push an allocation onto every
// `?` in the hot path even though errors are the cold path. Allow it here.
#![allow(clippy::result_large_err)]

::pgrx::pg_module_magic!(name, version);

mod errors;
mod http;
mod listen_router;
mod livereload;
mod logging;
mod request_timeout;
mod router;
mod schema;
mod settings;
mod templating;
mod worker;

/// Called by Postgres when our shared library is first loaded.
///
/// Two load paths:
/// - **shared_preload_libraries** (production): postmaster is still starting,
///   we register a *static* worker via `.load()`. Worker auto-starts with
///   postmaster and lives for its lifetime.
/// - **CREATE EXTENSION** (dev / ad-hoc): we're running inside a regular
///   backend. Static registration is a no-op here; we need `.load_dynamic()`
///   to ask the postmaster to fork the worker now. The worker is detached
///   from our backend — it survives after psql exits.
///
/// `extern "C-unwind"` is required in pgrx 0.18 — a plain `extern "C"` causes
/// a rustc ICE from the `#[pg_guard]` macro.
#[pgrx::pg_guard]
pub extern "C-unwind" fn _PG_init() {
    use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};
    use std::time::Duration;

    let builder = BackgroundWorkerBuilder::new("pg_web_worker")
        .set_library("pg_web_ext")
        .set_function("pg_web_worker_main")
        .set_argument(None)
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(Some(Duration::from_secs(5)))
        .enable_spi_access();

    let in_shared_preload =
        unsafe { pgrx::pg_sys::process_shared_preload_libraries_in_progress };

    if in_shared_preload {
        pgrx::log!("pg_web_ext: registering static background worker (shared_preload_libraries)");
        builder.load();
    } else {
        pgrx::log!("pg_web_ext: registering dynamic background worker (CREATE EXTENSION)");
        let builder = builder.set_notify_pid(unsafe { pgrx::pg_sys::MyProcPid });
        match builder.load_dynamic() {
            Ok(_handle) => {
                pgrx::log!("pg_web_ext: background worker registration queued");
            }
            Err(e) => {
                pgrx::warning!(
                    "pg_web_ext: failed to register background worker: {:?} \
                     (check max_worker_processes GUC)",
                    e
                );
            }
        }
    }
}

/// Required by `cargo pgrx test`. Do not remove.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
