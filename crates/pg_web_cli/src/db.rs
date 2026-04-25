//! Connection helpers for every CLI subcommand that talks to Postgres.
//!
//! Centralizing connection setup buys two things:
//!
//! 1. Every CLI-initiated backend shows up in `pg_stat_activity` with an
//!    `application_name` that names the verb, the host, and the OS PID
//!    of the CLI process. When push retries on a concurrent-DDL race
//!    (`tuple concurrently updated`) and runs out of attempts, the
//!    diagnostic can list the *other* `pg-web *` connections and tell
//!    the user exactly which process to stop.
//!
//! 2. One spelling of `Client::connect` to update if/when the client
//!    crate or TLS posture changes.
//!
//! The application_name format is `pg-web <verb> (pid=<n>, host=<h>)`.
//! Pid comes first so the value survives Postgres's silent truncation
//! at NAMEDATALEN-1 (63 bytes) — a long FQDN can lose its tail and we
//! still keep the actionable kill target.
//!
//! Connections stay `NoTls` for v0.x: pg-web pushes against localhost
//! today (or an SSH tunnel terminated locally — Component F.2). Once
//! that lands, the SSH layer encrypts over the wire and TLS to the DB
//! itself remains a Phase-2+ concern.

use std::str::FromStr;

use anyhow::{Context, Result};
use postgres::{Client, Config, NoTls};

/// Open a connection tagged for `pg_stat_activity`. `verb` is the
/// CLI subcommand name (`push`, `dev`, `migrate`, `env`, `check`,
/// `stack`) — embedded into the application_name so the diagnostic
/// path can identify sibling pg-web connections at retry time.
pub fn connect(url: &str, verb: &str) -> Result<Client> {
    let mut cfg = Config::from_str(url).with_context(|| format!("parsing {url}"))?;
    cfg.application_name(&application_name(verb));
    cfg.connect(NoTls)
        .with_context(|| format!("connecting to {url}"))
}

/// Build the `application_name` string for a given verb. Public so
/// the retry-exhaustion diagnostic can match on the same shape it
/// emits.
pub fn application_name(verb: &str) -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let pid = std::process::id();
    format!("pg-web {verb} (pid={pid}, host={host})")
}

/// Parsed pieces of an `application_name` produced by [`application_name`].
/// Returned by [`parse_application_name`] for the diagnostic side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTag {
    pub verb: String,
    pub pid: u32,
    pub host: String,
}

/// Try to parse `application_name` back into structured pieces. Tolerant
/// of trailing truncation (PG silently cuts past 63 bytes), so the host
/// suffix may be missing — verb and pid still come back in that case.
pub fn parse_application_name(s: &str) -> Option<ClientTag> {
    let rest = s.strip_prefix("pg-web ")?;
    let (verb, rest) = rest.split_once(" (")?;

    // Strip the trailing ')' if present; tolerate truncation that ate it.
    let inner = rest.strip_suffix(')').unwrap_or(rest);

    // After "pid=" extract digits up to ", host=" (or end-of-string if
    // the host part got truncated entirely, which would be unusual).
    let after_pid = inner.strip_prefix("pid=")?;
    let (pid_str, host) = match after_pid.split_once(", host=") {
        Some((p, h)) => (p, h.to_string()),
        None => (after_pid, String::new()),
    };
    let pid: u32 = pid_str.parse().ok()?;
    Some(ClientTag {
        verb: verb.to_string(),
        pid,
        host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_name_starts_with_pg_web_and_verb() {
        let s = application_name("push");
        assert!(s.starts_with("pg-web push ("), "got: {s}");
        assert!(s.contains("pid="));
        assert!(s.contains("host="));
    }

    #[test]
    fn parse_application_name_round_trips() {
        let s = application_name("dev");
        let tag = parse_application_name(&s).expect("parses");
        assert_eq!(tag.verb, "dev");
        assert_eq!(tag.pid, std::process::id());
        // gethostname() may return empty on misconfigured systems; either
        // way the round-trip should preserve whatever it returned.
        assert_eq!(tag.host, gethostname::gethostname().to_string_lossy());
    }

    #[test]
    fn parse_application_name_handles_truncation_after_host_equals() {
        // PG cuts at NAMEDATALEN-1; a long FQDN can lose its tail and the
        // closing paren. Parser still recovers verb + pid.
        let truncated = "pg-web push (pid=12345, host=really-long-hostname.examp";
        let tag = parse_application_name(truncated).expect("parses truncated");
        assert_eq!(tag.verb, "push");
        assert_eq!(tag.pid, 12345);
        assert_eq!(tag.host, "really-long-hostname.examp");
    }

    #[test]
    fn parse_application_name_rejects_non_pg_web_clients() {
        assert!(parse_application_name("psql").is_none());
        assert!(parse_application_name("pgAdmin 4").is_none());
        assert!(parse_application_name("").is_none());
    }

    #[test]
    fn parse_application_name_rejects_malformed_pid() {
        // "pid=abc" — not a u32. No tag.
        let s = "pg-web push (pid=abc, host=h)";
        assert!(parse_application_name(s).is_none());
    }
}
