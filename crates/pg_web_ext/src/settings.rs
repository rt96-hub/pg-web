//! Framework-owned runtime settings, read via SPI.
//!
//! The `pgweb.settings` table is the single source of truth for runtime
//! config — database state, not postgresql.conf or env vars. Rationale:
//!
//! - `pg-web push` can write it without privilege escalation.
//! - A container restart can't lose it (it's data).
//! - `pg-web dev` can temporarily override by UPSERT and put it back on exit.
//! - `SELECT * FROM pgweb.settings` is how you debug why prod is in dev mode.
//!
//! Reads are one SPI hit per request (microseconds against in-memory
//! shared buffers). If that ever shows up in a profile, a BGW-local cache
//! with invalidation is the next step — but don't build it pre-emptively.

use pgrx::Spi;

/// Runtime environment. Controls how the router surfaces errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    /// Rich dev error pages with full context, remedy, and `req` dump.
    Development,
    /// Generic 500 body — no internal details in the response.
    Production,
}

impl Env {
    pub fn from_value(v: &str) -> Self {
        match v {
            "production" | "prod" => Self::Production,
            _ => Self::Development,
        }
    }
}

/// Read the current `env` setting. Any lookup error (missing row, SPI
/// hiccup) defaults to Production — the conservative choice, since a
/// failed lookup shouldn't be an excuse to leak internals.
pub fn current_env() -> Env {
    match Spi::get_one::<String>("SELECT value FROM pgweb.settings WHERE key = 'env' LIMIT 1") {
        Ok(Some(v)) => Env::from_value(&v),
        Ok(None) | Err(_) => Env::Production,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_parse_production_variants() {
        assert_eq!(Env::from_value("production"), Env::Production);
        assert_eq!(Env::from_value("prod"), Env::Production);
    }

    #[test]
    fn env_parse_development_is_default_for_anything_else() {
        assert_eq!(Env::from_value("development"), Env::Development);
        assert_eq!(Env::from_value("dev"), Env::Development);
        assert_eq!(Env::from_value(""), Env::Development);
        assert_eq!(Env::from_value("whatever"), Env::Development);
    }

}
