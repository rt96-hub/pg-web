//! `pg-web migrate apply` — run raw-SQL migration files against a DB and
//! record what's been applied in `pgweb.migrations`.
//!
//! Phase 1 scope:
//! - File-name identity (no checksum).
//! - Applied in lexical filename order.
//! - One transaction per file (partial failure = full rollback of that file;
//!   earlier successful files stay applied).
//! - Idempotent: re-running after a clean run is a no-op.
//!
//! Declarative schema diffing (`migrate create`) is deferred to Phase 2.5.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use postgres::Client;

use crate::db;

#[derive(Debug, Default, Clone)]
pub struct MigrateSummary {
    /// Names of migrations applied during this run (in apply order).
    pub applied: Vec<String>,
    /// Names of migrations that were already in the ledger and got skipped.
    pub skipped: Vec<String>,
}

/// Read `<app_dir>/migrations/*.sql`, compare against `pgweb.migrations`,
/// apply anything new in sorted order. Each file runs in its own transaction
/// that also inserts the ledger row — either both land or neither does.
pub fn apply(app_dir: &Path, url: &str) -> Result<MigrateSummary> {
    let migrations_dir = app_dir.join("migrations");
    if !migrations_dir.is_dir() {
        bail!(
            "no migrations/ directory in {}; run `pg-web init` first",
            app_dir.display()
        );
    }

    let files = discover(&migrations_dir)?;

    let mut client = db::connect(url, "migrate")?;

    let applied_set = load_applied(&mut client)?;
    let mut summary = MigrateSummary::default();

    for (name, sql) in &files {
        if applied_set.contains(name) {
            summary.skipped.push(name.clone());
            continue;
        }

        let mut tx = client
            .transaction()
            .with_context(|| format!("begin tx for {name}"))?;
        tx.batch_execute(sql)
            .with_context(|| format!("executing {name}"))?;
        tx.execute(
            "INSERT INTO pgweb.migrations (name) VALUES ($1)",
            &[&name.as_str()],
        )
        .with_context(|| format!("recording {name} in ledger"))?;
        tx.commit()
            .with_context(|| format!("committing {name}"))?;

        summary.applied.push(name.clone());
    }

    Ok(summary)
}

/// List `.sql` files in `migrations_dir`, sorted by filename. Non-SQL files
/// are skipped silently (so `.gitkeep`, README, etc. are fine).
pub fn discover(migrations_dir: &Path) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    let entries = fs::read_dir(migrations_dir)
        .with_context(|| format!("reading {}", migrations_dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("iterating {}", migrations_dir.display()))?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 filename in {}", migrations_dir.display()))?
            .to_string();
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        out.push((name, content));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn load_applied(client: &mut Client) -> Result<HashSet<String>> {
    let rows = client
        .query("SELECT name FROM pgweb.migrations", &[])
        .context("reading pgweb.migrations (is the extension installed?)")?;
    Ok(rows.into_iter().map(|r| r.get::<_, String>(0)).collect())
}

/// Pure helper: given all discovered migration filenames in apply order and
/// the set of names already in the ledger, return the ones still pending.
/// Order is preserved. Exposed for unit testing.
pub fn pending<'a>(all: &'a [String], applied: &HashSet<String>) -> Vec<&'a String> {
    all.iter().filter(|n| !applied.contains(*n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pending_returns_empty_when_all_applied() {
        let all = s(&["0001_a.sql", "0002_b.sql"]);
        let applied: HashSet<String> = all.iter().cloned().collect();
        assert!(pending(&all, &applied).is_empty());
    }

    #[test]
    fn pending_returns_all_when_ledger_empty() {
        let all = s(&["0001_a.sql", "0002_b.sql"]);
        let applied: HashSet<String> = HashSet::new();
        let got: Vec<&String> = pending(&all, &applied);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], "0001_a.sql");
        assert_eq!(got[1], "0002_b.sql");
    }

    #[test]
    fn pending_preserves_input_order() {
        let all = s(&["0001.sql", "0002.sql", "0003.sql", "0004.sql"]);
        let mut applied = HashSet::new();
        applied.insert("0001.sql".to_string());
        applied.insert("0003.sql".to_string());
        let got: Vec<&String> = pending(&all, &applied);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], "0002.sql");
        assert_eq!(got[1], "0004.sql");
    }

    #[test]
    fn discover_sorts_by_filename() {
        let dir = tempfile::tempdir().unwrap();
        for name in &["0003_c.sql", "0001_a.sql", "0002_b.sql"] {
            fs::write(dir.path().join(name), format!("-- {name}")).unwrap();
        }
        let got = discover(dir.path()).unwrap();
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["0001_a.sql", "0002_b.sql", "0003_c.sql"]);
    }

    #[test]
    fn discover_skips_non_sql_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("0001_init.sql"), "-- sql").unwrap();
        fs::write(dir.path().join("README.md"), "# notes").unwrap();
        fs::write(dir.path().join(".gitkeep"), "").unwrap();
        let got = discover(dir.path()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "0001_init.sql");
    }

    #[test]
    fn discover_reads_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("0001_init.sql"),
            "CREATE TABLE todos (id bigserial);",
        )
        .unwrap();
        let got = discover(dir.path()).unwrap();
        assert_eq!(got[0].1, "CREATE TABLE todos (id bigserial);");
    }
}
