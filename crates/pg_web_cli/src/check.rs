//! `pg-web check` — offline project validator.
//!
//! Every up-front check `pg-web push` does, but without a database
//! connection. Target use cases: pre-commit hook and CI gate — fast
//! feedback on layout errors, template parse errors, and SQL syntax
//! errors before a DB even has to be available.
//!
//! Deliberately out of scope for v0.1:
//! - **Semantic SQL validation** (column existence, type checks, RLS
//!   rules). Postgres has to do that; `check` catches the layer above.
//! - **PL/pgSQL function body parsing** beyond the outer `CREATE
//!   FUNCTION` wrapper. The `$$...$$` payload is opaque to our parser
//!   (sqlparser only matches the wrapper; the body is validated by
//!   Postgres at function-invocation time).
//! - **Return-type consistency** between a `.html` sibling and the
//!   handler signature. Valuable, but requires SQL-AST walking past
//!   the function wrapper; deferred.
//!
//! Opt-in DB check: `--url <pg-connection>` adds a migration-ledger
//! drift comparison against `pgweb.migrations`. When supplied,
//! `check` does touch the DB — that's explicit, and the default
//! offline surface stays intact.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crate::db;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use walkdir::WalkDir;

use crate::{migrate, paths};

/// Grouped findings. Callers decide how to format — tests inspect the
/// vectors directly; the CLI prints per-group.
#[derive(Debug, Default)]
pub struct CheckReport {
    pub layout: Vec<Finding>,
    pub templates: Vec<Finding>,
    pub sql: Vec<Finding>,
    pub migrations: Vec<Finding>,
    pub ledger: Vec<Finding>,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub path: PathBuf,
    pub message: String,
}

impl CheckReport {
    pub fn is_clean(&self) -> bool {
        self.total() == 0
    }

    pub fn total(&self) -> usize {
        self.layout.len()
            + self.templates.len()
            + self.sql.len()
            + self.migrations.len()
            + self.ledger.len()
    }
}

/// Run every check pass and return a grouped report. `url` is opt-in;
/// supplying it enables the ledger-drift pass which does require a DB.
pub fn check(app_dir: &Path, url: Option<&str>) -> Result<CheckReport> {
    let mut report = CheckReport::default();

    check_layout(app_dir, &mut report);
    check_templates(app_dir, &mut report)?;
    check_handler_sql(app_dir, &mut report)?;
    check_migration_sql(app_dir, &mut report)?;
    check_migration_order(app_dir, &mut report)?;

    if let Some(url) = url {
        check_ledger_drift(app_dir, url, &mut report)?;
    }

    Ok(report)
}

/// Run `paths::scan` which already enforces the layout spec
/// (directory-as-route, reserved stems, no flat HTML at root, no 404
/// in subdirs, etc.). Any error becomes one top-level finding on the
/// pages/ directory — callers re-run scan to get file-level detail if
/// needed.
fn check_layout(app_dir: &Path, report: &mut CheckReport) {
    let pages = app_dir.join("pages");
    if !pages.exists() {
        return;
    }
    if let Err(e) = paths::scan(&pages) {
        report.layout.push(Finding {
            path: pages,
            message: format!("{e:#}"),
        });
    }
}

/// Parse every `.html` under `pages/` through Tera. Mirrors push's
/// pre-transaction template validation so the check catches what push
/// would reject.
fn check_templates(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let pages = app_dir.join("pages");
    if !pages.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(&pages) {
        let entry = entry.with_context(|| format!("walking {}", pages.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;

        // Fresh Tera per file: we only care about parse, not cross-file
        // macros / extends. Avoids cross-file contamination of errors.
        let mut tera = tera::Tera::default();
        let name = path.to_string_lossy().to_string();
        if let Err(e) = tera.add_raw_template(&name, &content) {
            report.templates.push(Finding {
                path: path.to_path_buf(),
                message: format!("{e}"),
            });
        }
    }
    Ok(())
}

/// Parse every `.sql` under `pages/` through sqlparser. Dollar-quoted
/// function bodies are treated as opaque strings — we only validate
/// the SQL wrapper, which is where typos like `CRATE FUNCTION` or
/// missing `$$` delimiters live.
fn check_handler_sql(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let pages = app_dir.join("pages");
    if !pages.exists() {
        return Ok(());
    }

    let dialect = PostgreSqlDialect {};

    for entry in WalkDir::new(&pages) {
        let entry = entry.with_context(|| format!("walking {}", pages.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;

        if let Err(e) = Parser::parse_sql(&dialect, &content) {
            report.sql.push(Finding {
                path: path.to_path_buf(),
                message: format!("{e}"),
            });
        }
    }
    Ok(())
}

/// Parse every `migrations/*.sql` through sqlparser. Migrations can
/// carry multi-statement bodies; `parse_sql` handles semicolon
/// separation.
fn check_migration_sql(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let migrations = app_dir.join("migrations");
    if !migrations.exists() {
        return Ok(());
    }

    let dialect = PostgreSqlDialect {};

    for entry in fs::read_dir(&migrations)
        .with_context(|| format!("reading {}", migrations.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;

        if let Err(e) = Parser::parse_sql(&dialect, &content) {
            report.migrations.push(Finding {
                path: path.clone(),
                message: format!("{e}"),
            });
        }
    }
    Ok(())
}

/// Validate migration-filename convention. Rules we enforce:
/// - Every migration's numeric prefix (before the first underscore) is
///   unique. Duplicates are the common pain: `0002_users.sql` and
///   `0002_posts.sql` land by accident, migrate runs them in filesystem
///   order, which may not match git-merge order.
///
/// Files without an underscore are passed through silently — they may
/// be `.gitkeep` or unconventional but not obviously wrong.
fn check_migration_order(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let migrations_dir = app_dir.join("migrations");
    if !migrations_dir.exists() {
        return Ok(());
    }

    let files = migrate::discover(&migrations_dir)?;
    let mut seen_prefixes: HashMap<String, String> = HashMap::new();
    for (name, _) in &files {
        let prefix = match name.split_once('_') {
            Some((p, _)) => p,
            None => continue,
        };
        if let Some(first) = seen_prefixes.insert(prefix.to_string(), name.clone()) {
            report.migrations.push(Finding {
                path: migrations_dir.join(name),
                message: format!(
                    "duplicate migration prefix {prefix:?} — also used by {first:?}. \
                     Rename one; filesystem order is not guaranteed."
                ),
            });
        }
    }
    Ok(())
}

/// Opt-in: compare local `migrations/*.sql` against the `pgweb.migrations`
/// ledger. Reports a finding per divergence.
/// - Applied in DB but missing locally: someone deleted a migration after
///   it shipped. Diagnostic.
/// - Local file NOT applied in DB: normal pre-push state, flagged only
///   so CI can decide whether a migration was forgotten.
fn check_ledger_drift(app_dir: &Path, url: &str, report: &mut CheckReport) -> Result<()> {
    let migrations_dir = app_dir.join("migrations");
    let local: Vec<String> = if migrations_dir.exists() {
        migrate::discover(&migrations_dir)?
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    } else {
        Vec::new()
    };

    let mut client = db::connect(url, "check")?;
    let rows = client
        .query("SELECT name FROM pgweb.migrations", &[])
        .context("reading pgweb.migrations (is the extension installed?)")?;
    let applied: Vec<String> = rows.into_iter().map(|r| r.get::<_, String>(0)).collect();

    for name in &applied {
        if !local.contains(name) {
            report.ledger.push(Finding {
                path: migrations_dir.join(name),
                message: format!(
                    "{name:?} is in pgweb.migrations but missing from migrations/ — \
                     someone deleted a migration file after it was applied. The DB \
                     still carries its effects."
                ),
            });
        }
    }

    for name in &local {
        if !applied.contains(name) {
            report.ledger.push(Finding {
                path: migrations_dir.join(name),
                message: format!(
                    "{name:?} exists locally but is not in pgweb.migrations — \
                     run `pg-web migrate apply` to bring the DB up to date."
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    #[test]
    fn report_is_clean_when_no_findings() {
        let r = CheckReport::default();
        assert!(r.is_clean());
        assert_eq!(r.total(), 0);
    }

    #[test]
    fn report_counts_across_groups() {
        let mut r = CheckReport::default();
        r.layout.push(Finding {
            path: PathBuf::from("a"),
            message: "x".into(),
        });
        r.sql.push(Finding {
            path: PathBuf::from("b"),
            message: "y".into(),
        });
        assert!(!r.is_clean());
        assert_eq!(r.total(), 2);
    }

    #[test]
    fn check_flags_duplicate_migration_prefix() {
        let dir = tempdir().unwrap();
        write(dir.path(), "migrations/0001_users.sql", "SELECT 1;");
        write(dir.path(), "migrations/0001_posts.sql", "SELECT 2;");
        let report = check(dir.path(), None).unwrap();
        assert_eq!(report.migrations.len(), 1, "{:?}", report);
        assert!(report.migrations[0].message.contains("duplicate"));
        assert!(report.migrations[0].message.contains("0001"));
    }

    #[test]
    fn check_flags_bad_sql_in_migration() {
        let dir = tempdir().unwrap();
        write(dir.path(), "migrations/0001_bad.sql", "CRATE TABLE oops;");
        let report = check(dir.path(), None).unwrap();
        assert_eq!(report.migrations.len(), 1, "{:?}", report);
        assert!(report.migrations[0]
            .path
            .to_string_lossy()
            .contains("0001_bad.sql"));
    }

    #[test]
    fn check_flags_bad_tera_template() {
        let dir = tempdir().unwrap();
        // Valid minimal layout so check_layout passes.
        write(
            dir.path(),
            "pages/index.sql",
            "CREATE FUNCTION pgweb.pages__index(req json) RETURNS json AS $$ SELECT '{}'::json $$ LANGUAGE sql;",
        );
        write(
            dir.path(),
            "pages/index.html",
            "{% if x %}unclosed block",
        );
        let report = check(dir.path(), None).unwrap();
        assert_eq!(report.templates.len(), 1, "{:?}", report);
        assert!(report.templates[0]
            .path
            .to_string_lossy()
            .contains("index.html"));
    }

    #[test]
    fn check_accepts_clean_minimal_scaffold() {
        // What `pg-web init` produces should ALWAYS pass `pg-web check`
        // — otherwise the init scaffold is shipping broken defaults.
        let dir = tempdir().unwrap();
        crate::init::init(&dir.path().join("app"), "app", None).unwrap();
        let report = check(&dir.path().join("app"), None).unwrap();
        assert!(
            report.is_clean(),
            "minimal scaffold should be clean: {:#?}",
            report
        );
    }

    #[test]
    fn check_accepts_clean_todo_template() {
        // Same as above but for `--template todo`. If a future edit to
        // examples/todo introduces a parse error, this test catches it.
        let dir = tempdir().unwrap();
        crate::init::init(&dir.path().join("app"), "app", Some("todo")).unwrap();
        let report = check(&dir.path().join("app"), None).unwrap();
        assert!(
            report.is_clean(),
            "todo template should be clean: {:#?}",
            report
        );
    }

    #[test]
    fn check_passes_when_directories_are_missing() {
        // An app dir with no pages/ and no migrations/ should not panic
        // or error — it's just trivially clean.
        let dir = tempdir().unwrap();
        let report = check(dir.path(), None).unwrap();
        assert!(report.is_clean());
    }
}
