//! `pg-web env set/unset/list` — manage runtime settings in
//! `pgweb.settings`.
//!
//! Two consumers share the table:
//! - The framework (key `env`, synced from `pgweb.toml [server].env` on
//!   every `pg-web push`).
//! - Apps, via this CLI + the `pgweb.setting(key)` SQL helper.
//!
//! We reject push-managed keys here so CLI writes don't get silently
//! reverted on the next push. Keys beyond that are free-form text.

use anyhow::{bail, Context, Result};
use postgres::{Client, NoTls};

/// Keys synced from `pgweb.toml` by `pg-web push`. Setting them via the
/// CLI would be silently overwritten on the next push — reject up-front
/// and point the user at the toml.
const PUSH_MANAGED_KEYS: &[&str] = &["env"];

/// One row from `pgweb.settings`. Returned from [`list`] so callers
/// decide output formatting (human-readable in main.rs, assertions in
/// tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

/// Upsert `(key, value)` into `pgweb.settings`. Rejects push-managed
/// keys and empty keys up-front.
pub fn set(url: &str, key: &str, value: &str) -> Result<()> {
    validate_key(key)?;
    reject_reserved(key)?;
    let mut client =
        Client::connect(url, NoTls).with_context(|| format!("connecting to {url}"))?;
    client
        .execute(
            "INSERT INTO pgweb.settings (key, value) VALUES ($1, $2) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[&key, &value],
        )
        .context("upserting pgweb.settings")?;
    Ok(())
}

/// Delete `key` from `pgweb.settings`. Returns true if a row was
/// actually deleted; false means the key wasn't there (idempotent
/// no-op rather than an error, so `pg-web env unset` can be safely
/// reused in scripts).
pub fn unset(url: &str, key: &str) -> Result<bool> {
    validate_key(key)?;
    reject_reserved(key)?;
    let mut client =
        Client::connect(url, NoTls).with_context(|| format!("connecting to {url}"))?;
    let n = client
        .execute("DELETE FROM pgweb.settings WHERE key = $1", &[&key])
        .context("deleting from pgweb.settings")?;
    Ok(n > 0)
}

/// Read all rows from `pgweb.settings` in alphabetical key order.
pub fn list(url: &str) -> Result<Vec<EnvEntry>> {
    let mut client =
        Client::connect(url, NoTls).with_context(|| format!("connecting to {url}"))?;
    let rows = client
        .query("SELECT key, value FROM pgweb.settings ORDER BY key", &[])
        .context("selecting from pgweb.settings")?;
    Ok(rows
        .into_iter()
        .map(|r| EnvEntry {
            key: r.get(0),
            value: r.get(1),
        })
        .collect())
}

/// Parse `KEY=VALUE` from a single CLI argument. Splits on the FIRST
/// `=` so values can contain further `=` characters (e.g. connection
/// strings with `?sslmode=require`).
pub fn parse_pair(input: &str) -> Result<(String, String)> {
    let (key, value) = input.split_once('=').ok_or_else(|| {
        anyhow::anyhow!(
            "expected KEY=VALUE, got {input:?}. Example: \
             pg-web env set STRIPE_KEY=sk_test_abc"
        )
    })?;
    Ok((key.to_string(), value.to_string()))
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("empty key — env set / unset requires a non-empty KEY");
    }
    Ok(())
}

fn reject_reserved(key: &str) -> Result<()> {
    if PUSH_MANAGED_KEYS.contains(&key) {
        bail!(
            "{key:?} is synced from pgweb.toml [server].{key} on every \
             `pg-web push`; a CLI write would be reverted on next push. \
             Edit pgweb.toml and re-push instead."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pair_happy_path() {
        let (k, v) = parse_pair("FOO=bar").unwrap();
        assert_eq!(k, "FOO");
        assert_eq!(v, "bar");
    }

    #[test]
    fn parse_pair_splits_on_first_equals() {
        // Values commonly contain '=' — connection strings with
        // ?sslmode=require, base64 tokens, etc. split_once keeps
        // everything after the first '=' in the value.
        let (k, v) = parse_pair("URL=postgres://u:p@h/db?sslmode=require").unwrap();
        assert_eq!(k, "URL");
        assert_eq!(v, "postgres://u:p@h/db?sslmode=require");
    }

    #[test]
    fn parse_pair_empty_value_is_allowed() {
        // `FOO=` → value is "". Useful for "clear this flag" semantics
        // without deleting the row. Distinct from unset.
        let (k, v) = parse_pair("FOO=").unwrap();
        assert_eq!(k, "FOO");
        assert_eq!(v, "");
    }

    #[test]
    fn parse_pair_rejects_missing_equals() {
        let err = parse_pair("FOO").unwrap_err().to_string();
        assert!(err.contains("expected KEY=VALUE"), "err = {err}");
        assert!(err.contains("FOO"), "err should echo the input: {err}");
    }

    #[test]
    fn parse_pair_empty_key_passes_parser_but_validator_rejects() {
        // parse_pair is a pure split — it returns ("", "bar") without
        // complaint. validate_key is the gate that stops it reaching
        // the DB. Tested here so the split-then-validate ordering is
        // explicit.
        let (k, _v) = parse_pair("=bar").unwrap();
        assert_eq!(k, "");
        assert!(validate_key(&k).is_err());
    }

    #[test]
    fn validate_key_accepts_reasonable_keys() {
        assert!(validate_key("STRIPE_KEY").is_ok());
        assert!(validate_key("database_password").is_ok());
        assert!(validate_key("a").is_ok());
    }

    #[test]
    fn reject_reserved_rejects_push_managed() {
        let err = reject_reserved("env").unwrap_err().to_string();
        assert!(err.contains("pgweb.toml"), "err should point at toml: {err}");
    }

    #[test]
    fn reject_reserved_allows_user_keys() {
        assert!(reject_reserved("STRIPE_KEY").is_ok());
        assert!(reject_reserved("my_key").is_ok());
        assert!(reject_reserved("environment").is_ok()); // not exactly "env"
    }
}
