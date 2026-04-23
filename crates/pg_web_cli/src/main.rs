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
        Command::Init { name } => {
            let path = PathBuf::from(&name);
            pg_web_cli::init::init(&path, &name)?;
            println!("✓ scaffolded {}", path.display());
            println!();
            println!("Next steps:");
            println!("  cd {name}");
            println!("  pg-web up");
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
