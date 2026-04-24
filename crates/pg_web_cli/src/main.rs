//! pg-web CLI binary entry.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "pg-web", version, about = "pg-web CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new pg-web app directory.
    Init {
        /// Directory name for the new app (also used inside generated templates).
        name: String,
        /// Scaffold from a bundled example instead of the minimal hello-world.
        /// Run without the flag first to see what the minimal scaffold looks
        /// like; run with `--template demo` for the full HTMX todo list.
        #[arg(long, value_name = "NAME")]
        template: Option<String>,
    },
    /// Sync the current pg-web app directory into a running Postgres.
    ///
    /// Works today when --url points at a Postgres reachable from the
    /// machine running the CLI (localhost for dev, or a tunneled localhost
    /// like `ssh -L 5432:localhost:5432 deploy@vps` for prod). Pushing to
    /// a non-local --url without a tunnel requires the remote's :5432 to
    /// be publicly reachable — NOT recommended. See docs/DEPLOYMENT.md.
    ///
    /// The automated-tunnel flag `--target <name>` is Session 4 Component
    /// F.2 (not yet implemented); until it lands, tunnel manually or SSH
    /// in and run push on the server.
    Push {
        /// Postgres connection URL. If omitted, resolved from $DATABASE_URL
        /// (or the env var named in `pgweb.toml [database].url_env`), then
        /// falling back to the dev-scaffold default from docker-compose.yml.
        #[arg(long, env = "DATABASE_URL")]
        url: Option<String>,
        /// App directory to push (defaults to cwd).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Manage raw-SQL migrations.
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
    /// Bring the Docker Compose stack up, wait for readiness, print DATABASE_URL.
    Up {
        /// App directory containing docker-compose.yml (defaults to cwd).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Stop the Docker Compose stack.
    Down {
        /// App directory containing docker-compose.yml (defaults to cwd).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Also drop the pgdata volume (destructive — loses all database state).
        #[arg(long)]
        volumes: bool,
    },
    /// Watch pages/ + public/, re-push on save, and tail container logs.
    Dev {
        /// App directory (defaults to cwd).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Don't tail `docker compose logs -f postgres` in-band.
        #[arg(long)]
        no_logs: bool,
    },
    /// Manage key/value settings in `pgweb.settings` (secrets, runtime flags).
    ///
    /// Handlers read values via `SELECT pgweb.setting('KEY')` from SQL.
    /// Values persist across container restarts (they live in the DB,
    /// not in the image). Keys managed by `pgweb.toml` (currently `env`)
    /// are rejected — edit the toml and re-push instead.
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },
}

#[derive(Subcommand)]
enum MigrateAction {
    /// Apply pending migrations from `<dir>/migrations/` in filename order.
    Apply {
        /// Postgres connection URL. Resolved like `pg-web push --url`.
        #[arg(long, env = "DATABASE_URL")]
        url: Option<String>,
        /// App directory containing `migrations/` (defaults to cwd).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum EnvAction {
    /// Upsert a KEY=VALUE pair into `pgweb.settings`.
    Set {
        /// KEY=VALUE. Split on first `=`; values can contain further `=`.
        pair: String,
        /// Postgres connection URL. Resolved like `pg-web push --url`.
        #[arg(long, env = "DATABASE_URL")]
        url: Option<String>,
        /// App directory (defaults to cwd) — used for URL resolution.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Delete a key from `pgweb.settings`. No-op if the key isn't there.
    Unset {
        /// Key to delete.
        key: String,
        #[arg(long, env = "DATABASE_URL")]
        url: Option<String>,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Print all keys and values from `pgweb.settings` as `KEY=VALUE` lines.
    List {
        #[arg(long, env = "DATABASE_URL")]
        url: Option<String>,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pg-web: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { name, template } => {
            let path = PathBuf::from(&name);
            pg_web_cli::init::init(&path, &name, template.as_deref())?;
            println!("✓ scaffolded {}", path.display());
            println!();
            println!("Next steps:");
            println!("  cd {name}");
            println!("  pg-web up");
            // The demo template needs a migrate before its handlers can
            // run against public.todos; the minimal scaffold has an
            // empty migrations/ and doesn't. Guide the user accordingly.
            if matches!(template.as_deref(), Some("demo")) {
                println!("  pg-web migrate apply");
            }
            println!("  pg-web push");
            println!("  # then hit http://localhost:8080");
        }
        Command::Push { url, dir } => {
            let url = resolve_url(url, &dir)?;
            let summary = pg_web_cli::push::push(&dir, &url)?;
            println!(
                "✓ pushed — {} routes, {} templates, {} SQL files",
                summary.routes_upserted,
                summary.templates_upserted,
                summary.sql_files_executed
            );
            if summary.routes_deleted > 0
                || summary.templates_deleted > 0
                || summary.handlers_dropped > 0
            {
                println!(
                    "  reconciled — dropped {} route(s), {} template(s), {} handler(s) no longer on disk",
                    summary.routes_deleted,
                    summary.templates_deleted,
                    summary.handlers_dropped,
                );
            }
            if summary.assets_upserted > 0 || summary.assets_deleted > 0 {
                println!(
                    "  assets — {} upserted, {} removed",
                    summary.assets_upserted, summary.assets_deleted
                );
            }
            if let Some(env) = &summary.env_synced {
                println!("  env → {env} (synced from pgweb.toml → pgweb.settings)");
            }
        }
        Command::Migrate { action } => match action {
            MigrateAction::Apply { url, dir } => {
                let url = resolve_url(url, &dir)?;
                let summary = pg_web_cli::migrate::apply(&dir, &url)?;
                for name in &summary.applied {
                    println!("✓ applied {name}");
                }
                for name in &summary.skipped {
                    println!("— skipped {name} (already in ledger)");
                }
                println!(
                    "{} applied, {} skipped",
                    summary.applied.len(),
                    summary.skipped.len()
                );
            }
        },
        Command::Up { dir } => {
            let url = pg_web_cli::stack::up(&dir)?;
            println!("✓ stack up");
            println!("  DATABASE_URL={url}");
            println!("  http://localhost:8080");
        }
        Command::Down { dir, volumes } => {
            pg_web_cli::stack::down(&dir, volumes)?;
            if volumes {
                println!("✓ stack down (pgdata volume dropped)");
            } else {
                println!("✓ stack down");
            }
        }
        Command::Dev { dir, no_logs } => {
            pg_web_cli::dev::dev(&dir, !no_logs)?;
        }
        Command::Env { action } => match action {
            EnvAction::Set { pair, url, dir } => {
                let (key, value) = pg_web_cli::env::parse_pair(&pair)?;
                let url = resolve_url(url, &dir)?;
                pg_web_cli::env::set(&url, &key, &value)?;
                // Echo the key only — avoid leaking secret values into
                // terminal history / logs. Use `env list` explicitly if
                // you want to verify the stored value.
                println!("✓ set {key}");
            }
            EnvAction::Unset { key, url, dir } => {
                let url = resolve_url(url, &dir)?;
                if pg_web_cli::env::unset(&url, &key)? {
                    println!("✓ unset {key}");
                } else {
                    println!("— {key} not set (no-op)");
                }
            }
            EnvAction::List { url, dir } => {
                let url = resolve_url(url, &dir)?;
                let entries = pg_web_cli::env::list(&url)?;
                for e in entries {
                    println!("{}={}", e.key, e.value);
                }
            }
        },
    }
    Ok(())
}

/// Pick a DATABASE_URL for commands that need one. Explicit `--url` wins;
/// otherwise defer to `stack::resolve_database_url` which reads pgweb.toml +
/// env + the dev-scaffold fallback.
fn resolve_url(url: Option<String>, dir: &std::path::Path) -> Result<String> {
    match url {
        Some(u) if !u.is_empty() => Ok(u),
        _ => pg_web_cli::stack::resolve_database_url(dir, |k| std::env::var(k).ok()),
    }
}
