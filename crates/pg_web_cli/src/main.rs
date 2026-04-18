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
    Push {
        /// Postgres connection URL, e.g. postgres://user:pw@host:5432/db
        #[arg(long, env = "DATABASE_URL")]
        url: String,
        /// App directory to push (defaults to cwd).
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
            println!("  docker compose up -d");
            println!("  pg-web push --url postgres://postgres:devpassword@localhost:5432/app");
            println!("  # then hit http://localhost:8080");
        }
        Command::Push { url, dir } => {
            let summary = pg_web_cli::push::push(&dir, &url)?;
            println!(
                "✓ pushed — {} routes, {} templates, {} SQL files",
                summary.routes_upserted,
                summary.templates_upserted,
                summary.sql_files_executed
            );
        }
    }
    Ok(())
}
