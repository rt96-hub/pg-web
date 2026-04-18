//! `pg-web push` — sync a local pg-web app directory into a running Postgres.
//!
//! One transaction over :5432:
//!   1. Execute every `pages/**/*.sql` file against the DB
//!      (they typically contain `CREATE OR REPLACE FUNCTION ...`).
//!   2. UPSERT every `pages/**/*.html` into `pgweb.templates`.
//!   3. UPSERT a matching row in `pgweb.routes` for each HTML file,
//!      deriving route + handler from the filename.
//!
//! Commit → the extension's next request will see the updated state.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use postgres::{Client, NoTls};
use walkdir::WalkDir;

use crate::paths;

/// Summary of what `push` changed. Returned so callers (CLI, future tests)
/// can display / assert on the result.
#[derive(Debug, Default, Clone)]
pub struct PushSummary {
    pub sql_files_executed: usize,
    pub templates_upserted: usize,
    pub routes_upserted: usize,
}

pub fn push(app_dir: &Path, url: &str) -> Result<PushSummary> {
    let pages_dir = app_dir.join("pages");
    if !pages_dir.is_dir() {
        bail!(
            "no pages/ directory in {}; run `pg-web init` first",
            app_dir.display()
        );
    }

    // Enumerate files first so we have a deterministic order and can report
    // counts without half-committing if walking fails mid-transaction.
    let mut sql_files: Vec<(String, String)> = Vec::new(); // (rel, content)
    let mut html_files: BTreeMap<String, String> = BTreeMap::new(); // rel → content
    for entry in WalkDir::new(&pages_dir).sort_by_file_name() {
        let entry = entry.context("walking pages/")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&pages_dir)
            .context("stripping pages/ prefix")?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        match rel.extension().and_then(|e| e.to_str()) {
            Some("sql") => {
                let content = fs::read_to_string(entry.path())
                    .with_context(|| format!("reading {}", entry.path().display()))?;
                sql_files.push((rel_str, content));
            }
            Some("html") => {
                let content = fs::read_to_string(entry.path())
                    .with_context(|| format!("reading {}", entry.path().display()))?;
                html_files.insert(rel_str, content);
            }
            _ => {}
        }
    }

    let mut client = Client::connect(url, NoTls)
        .with_context(|| format!("connecting to {url}"))?;
    let mut tx = client.transaction()?;

    let mut summary = PushSummary::default();

    // Execute all SQL handler definitions first — templates and routes
    // reference the resulting functions.
    for (rel, sql) in &sql_files {
        tx.batch_execute(sql)
            .with_context(|| format!("executing pages/{rel}"))?;
        summary.sql_files_executed += 1;
    }

    for (rel, content) in &html_files {
        let template_path = paths::template_path_for(rel);
        let route = paths::route_for(rel);
        let handler = paths::handler_for(rel);

        tx.execute(
            "INSERT INTO pgweb.templates (template_path, content) \
             VALUES ($1, $2) \
             ON CONFLICT (template_path) DO UPDATE SET content = EXCLUDED.content",
            &[&template_path, &content],
        )
        .with_context(|| format!("upsert template {template_path}"))?;
        summary.templates_upserted += 1;

        tx.execute(
            "INSERT INTO pgweb.routes (method, path_pattern, handler_name, template_path) \
             VALUES ('GET', $1, $2, $3) \
             ON CONFLICT (method, path_pattern) DO UPDATE \
               SET handler_name = EXCLUDED.handler_name, \
                   template_path = EXCLUDED.template_path",
            &[&route, &handler, &template_path],
        )
        .with_context(|| format!("upsert route {route}"))?;
        summary.routes_upserted += 1;
    }

    tx.commit()?;

    Ok(summary)
}
