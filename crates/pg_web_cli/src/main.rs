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
        /// like; run with `--template todo` for the full HTMX todo list.
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
        /// Plan-only mode: run every step inside a transaction, then
        /// ROLLBACK instead of COMMIT. The DB sees no changes, no
        /// `pgweb.deployments` row is recorded, and pending migrations
        /// are reported but not applied. Useful in CI to verify a
        /// branch would push cleanly.
        #[arg(long)]
        dry_run: bool,
        /// Apply any pending migrations from `migrations/` before
        /// pushing. Without this flag, push refuses to run when
        /// migrations are pending — the "handler references a column
        /// that doesn't exist yet" class of failure is almost always
        /// "push preceded its migration."
        #[arg(long)]
        with_migrate: bool,
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
        /// Don't emit livereload NOTIFYs after pushes. Browsers still
        /// load the livereload stub (rendered HTML in dev has it) but
        /// the EventSource will just stay quiet. Use when the auto-
        /// reload UX interferes with a heavy-JS app that holds complex
        /// local state you want to preserve across saves.
        #[arg(long)]
        no_livereload: bool,
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
    /// Offline project validator — layout, Tera templates, SQL syntax,
    /// migration order. Designed for pre-commit hooks and CI: runs
    /// without a DB connection by default. Pass `--url` to also check
    /// migration-ledger drift against a running Postgres.
    ///
    /// Exit code 0 means no findings; non-zero means one or more groups
    /// have findings (details printed to stdout).
    Check {
        /// App directory to validate (defaults to cwd).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Optional Postgres URL. Supplying it enables the ledger-drift
        /// pass (`pgweb.migrations` vs. `migrations/*.sql`). The rest
        /// of the check stays offline regardless.
        #[arg(long, env = "DATABASE_URL")]
        url: Option<String>,
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
    /// Values are masked by default (security). Use --show-values to emit
    /// the real contents (e.g. when scripting against a trusted local DB).
    List {
        #[arg(long, env = "DATABASE_URL")]
        url: Option<String>,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Print the actual secret/setting values instead of masking them.
        #[arg(long)]
        show_values: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pg-web: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
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
            if matches!(template.as_deref(), Some("todo")) {
                println!("  pg-web migrate apply");
            }
            println!("  pg-web push");
            println!("  # then hit http://localhost:8080");
        }
        Command::Push {
            url,
            dir,
            dry_run,
            with_migrate,
        } => {
            let url = resolve_url(url, &dir)?;
            let opts = pg_web_cli::push::PushOptions { dry_run, with_migrate };
            let summary = pg_web_cli::push::push_with_options(&dir, &url, opts)?;

            // Dry-run gets a visible marker on every line so it's
            // impossible to misread stdout as "push committed." For
            // real pushes the marker is omitted; the ✓ carries the
            // signal on its own.
            let tag = if summary.dry_run { "[dry-run] " } else { "" };
            let verb = if summary.dry_run { "would push" } else { "pushed" };

            if summary.migrations_applied > 0 {
                let action = if summary.dry_run { "would apply" } else { "applied" };
                println!(
                    "{tag}{action} {} migration(s): {}",
                    summary.migrations_applied,
                    summary.migrations_applied_names.join(", "),
                );
            }

            println!(
                "{tag}✓ {verb} — {} routes, {} templates, {} SQL files",
                summary.routes_upserted,
                summary.templates_upserted,
                summary.sql_files_executed
            );
            if summary.routes_deleted > 0
                || summary.templates_deleted > 0
                || summary.handlers_dropped > 0
            {
                println!(
                    "{tag}  reconciled — dropped {} route(s), {} template(s), {} handler(s) no longer on disk",
                    summary.routes_deleted,
                    summary.templates_deleted,
                    summary.handlers_dropped,
                );
            }
            if summary.assets_upserted > 0 || summary.assets_deleted > 0 {
                println!(
                    "{tag}  assets — {} upserted, {} removed",
                    summary.assets_upserted, summary.assets_deleted
                );
            }
            if let Some(env) = &summary.env_synced {
                println!("{tag}  env → {env} (synced from pgweb.toml → pgweb.settings)");
            }
            if let Some(rt) = &summary.request_timeout_synced {
                println!("{tag}  request_timeout → {rt} (synced from pgweb.toml → pgweb.settings)");
            }
            if summary.dry_run {
                println!("[dry-run] transaction rolled back — no changes committed");
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
        Command::Dev {
            dir,
            no_logs,
            no_livereload,
        } => {
            let opts = pg_web_cli::dev::DevOptions {
                tail_logs: !no_logs,
                livereload: !no_livereload,
            };
            pg_web_cli::dev::dev(&dir, opts)?;
        }
        Command::Check { dir, url } => {
            let report = pg_web_cli::check::check(&dir, url.as_deref())?;
            print_check_report(&report);
            if !report.is_clean() {
                return Ok(ExitCode::FAILURE);
            }
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
            EnvAction::List { url, dir, show_values } => {
                let url = resolve_url(url, &dir)?;
                let entries = pg_web_cli::env::list(&url)?;
                for e in entries {
                    if show_values {
                        println!("{}={}", e.key, e.value);
                    } else {
                        // Mask like many CLIs (show a short prefix for debugging
                        // which key it is without exfiltrating the secret).
                        let masked = if e.value.len() <= 4 {
                            "****".to_string()
                        } else {
                            format!("{}****", &e.value[..4.min(e.value.len())])
                        };
                        println!("{}={}", e.key, masked);
                    }
                }
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}

/// Pretty-print a `CheckReport` to stdout. Empty groups are skipped;
/// all groups present show a header + per-finding lines in
/// `path: message` form so IDE / editor jump-to-file features can pick
/// the path out with a `file:line`-style regex. Final line summarizes
/// the count so CI logs show the gate outcome at a glance.
fn print_check_report(report: &pg_web_cli::check::CheckReport) {
    let groups: &[(&str, &[pg_web_cli::check::Finding])] = &[
        ("Layout", &report.layout),
        ("Templates", &report.templates),
        ("SQL", &report.sql),
        ("Migrations", &report.migrations),
        ("Ledger", &report.ledger),
    ];

    for (name, findings) in groups {
        if findings.is_empty() {
            continue;
        }
        println!("\n{name}:");
        for f in *findings {
            println!("  {}: {}", f.path.display(), f.message);
        }
    }

    if report.is_clean() {
        println!("✓ check passed — no findings");
    } else {
        println!("\n✗ {} finding(s) — fix and re-run", report.total());
    }
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
