::pgrx::pg_module_magic!(name, version);

mod schema;

/// Required by `cargo pgrx test`. Do not remove.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
