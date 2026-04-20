//! `pg-web up` / `pg-web down` — stack lifecycle commands.
//!
//! Thin wrapper over `docker compose`. The goal is to hide the compose
//! lifecycle and DATABASE_URL plumbing so day-to-day dev work is just
//! `pg-web up` → `pg-web dev` → `pg-web down`. Mirrors the `next dev` /
//! `rails server` UX goal from the 2026-04-18 decision log entry.
//!
//! Shape:
//!
//! - `up(app_dir)` — preflight `docker`, shell out to `docker compose up -d`,
//!   TCP-poll `:5432` and `:8080` until both accept connections, then resolve
//!   and return `DATABASE_URL` from `pgweb.toml` + the environment.
//! - `down(app_dir, drop_volumes)` — `docker compose down [-v]`.
//!
//! End-to-end Docker boot is covered by the existing tier 3 E2E test
//! (`crates/pg_web_cli/tests/docker_e2e.rs`); this module's unit tests only
//! exercise the pure helpers (port polling, DATABASE_URL resolution, compose
//! file discovery).

use std::fs;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

// Dev defaults — must stay in sync with `templates::DOCKER_COMPOSE`.
// When no DATABASE_URL env var is set, we build a dev URL from these.
const DEV_HOST: &str = "localhost";
const DEV_HTTP_PORT: u16 = 8080;
const DEV_PG_PORT: u16 = 5432;
const DEV_POSTGRES_USER: &str = "postgres";
const DEV_POSTGRES_PASSWORD: &str = "devpassword";
const DEV_POSTGRES_DB: &str = "app";

/// Total deadline for `up` readiness polling. Generous enough for a cold
/// image pull + Postgres initdb on a fresh machine; tightens into ~1s on
/// a warm cache.
const UP_READY_DEADLINE: Duration = Duration::from_secs(120);
/// Per-connect timeout + inter-attempt sleep. Short so a hanging dial
/// can't exhaust the readiness deadline alone.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Minimal `pgweb.toml` view — we only need `[database].url_env` here.
/// Other sections (server, dev, assets, runtime) are parsed by the
/// commands that care about them.
#[derive(Debug, Default, Deserialize)]
struct PgWebConfig {
    #[serde(default)]
    database: DatabaseConfig,
}

#[derive(Debug, Default, Deserialize)]
struct DatabaseConfig {
    /// Name of the env var holding the connection string. Defaults to
    /// `DATABASE_URL`. Init scaffold writes this literal value.
    #[serde(default)]
    url_env: Option<String>,
}

/// `pg-web up` — bring the compose stack up, block until HTTP + Postgres
/// are reachable, and return the resolved DATABASE_URL for display.
pub fn up(app_dir: &Path) -> Result<String> {
    preflight_docker()?;
    let compose = ensure_compose_file(app_dir)?;

    let status = Command::new("docker")
        .args(["compose", "-f"])
        .arg(&compose)
        .args(["up", "-d"])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("spawning `docker compose up -d`")?;
    if !status.success() {
        bail!("`docker compose up -d` failed (exit {:?})", status.code());
    }

    let deadline = Instant::now() + UP_READY_DEADLINE;
    poll_tcp(&format!("{DEV_HOST}:{DEV_PG_PORT}"), deadline)
        .context("Postgres didn't accept connections on :5432 within deadline")?;
    poll_tcp(&format!("{DEV_HOST}:{DEV_HTTP_PORT}"), deadline)
        .context("pg-web HTTP didn't accept connections on :8080 within deadline")?;

    resolve_database_url(app_dir, |k| std::env::var(k).ok())
}

/// `pg-web down` — stop the compose stack. When `drop_volumes` is true,
/// pass `-v` to also delete the `pgdata` volume (destructive: loses the DB).
pub fn down(app_dir: &Path, drop_volumes: bool) -> Result<()> {
    preflight_docker()?;
    let compose = ensure_compose_file(app_dir)?;

    let mut cmd = Command::new("docker");
    cmd.args(["compose", "-f"]).arg(&compose).arg("down");
    if drop_volumes {
        cmd.arg("-v");
    }
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("spawning `docker compose down`")?;
    if !status.success() {
        bail!("`docker compose down` failed (exit {:?})", status.code());
    }
    Ok(())
}

/// Verify `docker --version` executes. Clear install hint on `ENOENT` so
/// fresh machines get a real error instead of a cryptic spawn failure.
fn preflight_docker() -> Result<()> {
    match Command::new("docker")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!(
            "`docker --version` exited with {:?}; is your Docker install healthy?",
            s.code()
        ),
        Err(e) if e.kind() == ErrorKind::NotFound => bail!(
            "`docker` not found on PATH. Install Docker (Desktop on Windows/Mac, Engine on Linux) before running pg-web up/down."
        ),
        Err(e) => Err(anyhow!(e).context("running `docker --version`")),
    }
}

/// Return the full path to `<app_dir>/docker-compose.yml` if present,
/// else a clear error pointing back at `pg-web init`.
pub fn ensure_compose_file(app_dir: &Path) -> Result<PathBuf> {
    let p = app_dir.join("docker-compose.yml");
    if !p.is_file() {
        bail!(
            "no docker-compose.yml in {} — run `pg-web init` or pass --dir",
            app_dir.display()
        );
    }
    Ok(p)
}

/// Poll `addr` (host:port) until a TCP connection succeeds or the deadline
/// passes. Pure — knows nothing about Docker; just "can I reach this?".
/// Per-attempt timeout is `POLL_INTERVAL` so a slow/hanging dial doesn't
/// exhaust the whole deadline in one attempt.
pub fn poll_tcp(addr: &str, deadline: Instant) -> Result<()> {
    let sock: SocketAddr = addr
        .to_socket_addrs()
        .with_context(|| format!("resolving {addr}"))?
        .next()
        .ok_or_else(|| anyhow!("no addresses resolved for {addr}"))?;

    loop {
        match TcpStream::connect_timeout(&sock, POLL_INTERVAL) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for {addr} (last attempt: {e})");
                }
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Resolve the database URL a user should connect with after `up`. Reads
/// `<app_dir>/pgweb.toml` for `[database].url_env` (default `DATABASE_URL`),
/// looks up that env var via the injected closure, and falls back to the
/// dev-scaffold defaults if unset. Closure-based so tests don't touch the
/// real process env.
pub fn resolve_database_url<F>(app_dir: &Path, env_lookup: F) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    let toml_path = app_dir.join("pgweb.toml");
    let cfg = if toml_path.is_file() {
        let raw = fs::read_to_string(&toml_path)
            .with_context(|| format!("reading {}", toml_path.display()))?;
        toml::from_str::<PgWebConfig>(&raw)
            .with_context(|| format!("parsing {}", toml_path.display()))?
    } else {
        PgWebConfig::default()
    };

    let var_name = cfg
        .database
        .url_env
        .as_deref()
        .unwrap_or("DATABASE_URL");

    if let Some(v) = env_lookup(var_name) {
        if !v.is_empty() {
            return Ok(v);
        }
    }

    // Dev fallback — matches the password/user/db in `templates::DOCKER_COMPOSE`.
    Ok(format!(
        "postgres://{DEV_POSTGRES_USER}:{DEV_POSTGRES_PASSWORD}@{DEV_HOST}:{DEV_PG_PORT}/{DEV_POSTGRES_DB}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn bind_ephemeral() -> (TcpListener, u16) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        (l, port)
    }

    #[test]
    fn poll_tcp_succeeds_when_listener_is_live() {
        let (_l, port) = bind_ephemeral();
        let deadline = Instant::now() + Duration::from_secs(2);
        poll_tcp(&format!("127.0.0.1:{port}"), deadline).expect("should connect");
    }

    #[test]
    fn poll_tcp_times_out_on_closed_port() {
        // Bind to get a guaranteed-free port, then drop the listener so
        // the port is free (and connects refuse fast on Linux).
        let (listener, port) = bind_ephemeral();
        drop(listener);
        let deadline = Instant::now() + Duration::from_millis(400);
        let err = poll_tcp(&format!("127.0.0.1:{port}"), deadline)
            .expect_err("should have timed out");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out") && msg.contains(&port.to_string()),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn ensure_compose_file_errors_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = ensure_compose_file(dir.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("docker-compose.yml"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn ensure_compose_file_returns_path_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let compose = dir.path().join("docker-compose.yml");
        fs::write(&compose, "services: {}\n").unwrap();
        let got = ensure_compose_file(dir.path()).unwrap();
        assert_eq!(got, compose);
    }

    #[test]
    fn resolve_database_url_uses_env_var_when_set() {
        let dir = tempfile::tempdir().unwrap();
        // No pgweb.toml → defaults url_env=DATABASE_URL.
        let url = resolve_database_url(dir.path(), |k| {
            if k == "DATABASE_URL" {
                Some("postgres://app@db:5432/prod".to_string())
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(url, "postgres://app@db:5432/prod");
    }

    #[test]
    fn resolve_database_url_falls_back_to_dev_default_when_env_unset() {
        let dir = tempfile::tempdir().unwrap();
        let url = resolve_database_url(dir.path(), |_| None).unwrap();
        assert_eq!(
            url,
            "postgres://postgres:devpassword@localhost:5432/app"
        );
    }

    #[test]
    fn resolve_database_url_ignores_empty_env_value() {
        let dir = tempfile::tempdir().unwrap();
        let url = resolve_database_url(dir.path(), |_| Some(String::new())).unwrap();
        assert_eq!(
            url,
            "postgres://postgres:devpassword@localhost:5432/app"
        );
    }

    #[test]
    fn resolve_database_url_honors_custom_url_env_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pgweb.toml"),
            r#"[database]
url_env = "MY_DB"
"#,
        )
        .unwrap();
        let url = resolve_database_url(dir.path(), |k| {
            if k == "MY_DB" {
                Some("postgres://x@y/z".to_string())
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(url, "postgres://x@y/z");
    }

    #[test]
    fn resolve_database_url_handles_missing_database_section() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pgweb.toml"),
            "[server]\nport = 8080\n",
        )
        .unwrap();
        let url = resolve_database_url(dir.path(), |_| None).unwrap();
        assert_eq!(
            url,
            "postgres://postgres:devpassword@localhost:5432/app"
        );
    }

    #[test]
    fn resolve_database_url_errors_on_malformed_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pgweb.toml"), "not = = valid\n").unwrap();
        let err = resolve_database_url(dir.path(), |_| None).unwrap_err();
        assert!(format!("{err:#}").contains("pgweb.toml"));
    }
}
