::pgrx::pg_module_magic!(name, version);

mod http;
mod logging;
mod schema;
mod worker;

/// Called by Postgres when our shared library is first loaded.
///
/// Triggers when `CREATE EXTENSION pg_web_ext` runs (or when the library is
/// listed in `shared_preload_libraries` at postmaster startup — production
/// path). We register the HTTP background worker here; the postmaster then
/// forks a dedicated process and invokes `worker::pg_web_worker_main`.
///
/// `extern "C-unwind"` is required in pgrx 0.18 — matches `#[pg_guard]` macro
/// expectations. A plain `extern "C"` here causes a rustc ICE.
#[pgrx::pg_guard]
pub extern "C-unwind" fn _PG_init() {
    use pgrx::bgworkers::BackgroundWorkerBuilder;

    BackgroundWorkerBuilder::new("pg_web_worker")
        .set_library("pg_web_ext")
        .set_function("pg_web_worker_main")
        .set_argument(None)
        .enable_spi_access()
        .load();
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
