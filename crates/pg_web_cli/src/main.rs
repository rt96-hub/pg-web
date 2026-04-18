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
            println!("  # then hit http://localhost:8080 after the container is healthy");
        }
    }
    Ok(())
}
