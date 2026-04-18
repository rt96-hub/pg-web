//! `pg-web push` — sync a local pg-web app directory into a running Postgres.
//!
//! One transaction; walks `pages/` via `paths::scan()`, then for each route:
//!   1. Execute the handler SQL file if present, or synthesize a trivial
//!      `RETURNS json` handler for HTML-only (static) routes.
//!   2. Upsert the template row (when an `.html` exists).
//!   3. Upsert `pgweb.routes` with method derived from the filename
//!      (`index` → GET, `post` → POST) and `template_path` NULL for
//!      raw-text routes.
//!
//! Commit → the extension's next request sees the updated state.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use postgres::{Client, NoTls};

use crate::paths;

/// What `push` changed. Returned so callers can assert / display it.
#[derive(Debug, Default, Clone)]
pub struct PushSummary {
    pub sql_files_executed: usize,
    pub templates_upserted: usize,
    pub routes_upserted: usize,
    pub synthesized_handlers: usize,
}

pub fn push(app_dir: &Path, url: &str) -> Result<PushSummary> {
    let pages_dir = app_dir.join("pages");
    if !pages_dir.is_dir() {
        bail!(
            "no pages/ directory in {}; run `pg-web init` first",
            app_dir.display()
        );
    }

    // Validate the whole tree before touching the DB.
    let entries = paths::scan(&pages_dir)?;

    let mut client =
        Client::connect(url, NoTls).with_context(|| format!("connecting to {url}"))?;
    let mut tx = client.transaction()?;

    let mut summary = PushSummary::default();

    for entry in &entries {
        // 1. Handler SQL: execute user-written file or synthesize a no-op
        //    for static pages (HTML-only routes).
        if let Some(sql_path) = &entry.sql_path {
            let sql = fs::read_to_string(sql_path)
                .with_context(|| format!("reading {}", sql_path.display()))?;
            tx.batch_execute(&sql)
                .with_context(|| format!("executing {}", sql_path.display()))?;
            summary.sql_files_executed += 1;
        } else if entry.html_path.is_some() {
            // Static route — no user .sql. Synthesize a trivial handler so the
            // router's uniform `SELECT (handler(req::json))::text` call path
            // has something to bind to. Returns `{}` so Tera renders the
            // template with an empty context.
            let synth = format!(
                "CREATE OR REPLACE FUNCTION {}(req json) RETURNS json \
                 LANGUAGE sql IMMUTABLE AS $$ SELECT '{{}}'::json $$",
                entry.handler_name
            );
            tx.batch_execute(&synth)
                .with_context(|| format!("synthesizing handler {}", entry.handler_name))?;
            summary.synthesized_handlers += 1;
        }

        // 2. Template: upsert only when the route has an HTML file.
        if let (Some(html_path), Some(template_path)) =
            (&entry.html_path, &entry.template_path)
        {
            let content = fs::read_to_string(html_path)
                .with_context(|| format!("reading {}", html_path.display()))?;
            tx.execute(
                "INSERT INTO pgweb.templates (template_path, content) \
                 VALUES ($1, $2) \
                 ON CONFLICT (template_path) DO UPDATE \
                   SET content = EXCLUDED.content",
                &[template_path, &content],
            )
            .with_context(|| format!("upsert template {template_path}"))?;
            summary.templates_upserted += 1;
        }

        // 3. Route row: method + path + handler + (nullable) template.
        tx.execute(
            "INSERT INTO pgweb.routes (method, path_pattern, handler_name, template_path) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (method, path_pattern) DO UPDATE \
               SET handler_name = EXCLUDED.handler_name, \
                   template_path = EXCLUDED.template_path",
            &[
                &entry.method,
                &entry.route,
                &entry.handler_name,
                &entry.template_path,
            ],
        )
        .with_context(|| {
            format!(
                "upsert route {} {}",
                entry.method, entry.route
            )
        })?;
        summary.routes_upserted += 1;
    }

    tx.commit()?;

    Ok(summary)
}
