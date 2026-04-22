//! `pg-web push` — sync a local pg-web app directory into a running Postgres.
//!
//! One transaction. The filesystem is the source of truth:
//!
//!   1. Walk `pages/` via `paths::scan()`. For each entry: execute the
//!      handler SQL file (or synthesize a trivial `RETURNS json` handler
//!      for HTML-only static pages), upsert the template row (when an
//!      `.html` exists), upsert the `pgweb.routes` row.
//!   2. Validate every expected handler function exists in Postgres with
//!      the signature `(req json) RETURNS json|text` matching the entry's
//!      mode. A user-written `.sql` that typos the function name would
//!      otherwise leave a dangling route and surface as a 500 at request
//!      time; catching it here is the fast-feedback path.
//!   3. Reconcile: delete `pgweb.routes` rows and `pgweb.templates` rows
//!      whose keys are no longer in the filesystem, and drop any
//!      `pgweb.pages__*(json) RETURNS json|text` handler that isn't in
//!      the expected set. **This signature namespace is reserved for
//!      push-managed handlers**; user helpers must use a different
//!      pattern (see `docs/DEVELOPER-GUIDE.md` § Common pitfalls).
//!
//! Any validation or reconcile error rolls the whole transaction back —
//! the live extension keeps serving the last good push until the user
//! fixes the offending file.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use postgres::{Client, NoTls, Transaction};
use serde::Deserialize;

use crate::paths::{self, RouteEntry};

/// Slice of `pgweb.toml` push cares about. Extra sections in the file
/// are ignored silently so the user can add their own without push
/// complaining.
#[derive(Debug, Default, Deserialize)]
struct PushTomlConfig {
    #[serde(default)]
    server: ServerSection,
}

#[derive(Debug, Default, Deserialize)]
struct ServerSection {
    /// `development` | `production`. Controls whether the extension
    /// surfaces rich error pages. Synced into `pgweb.settings` on every push.
    #[serde(default)]
    env: Option<String>,
}

/// What `push` changed. Returned so callers can display a summary.
#[derive(Debug, Default, Clone)]
pub struct PushSummary {
    pub sql_files_executed: usize,
    pub templates_upserted: usize,
    pub routes_upserted: usize,
    pub synthesized_handlers: usize,
    pub routes_deleted: usize,
    pub templates_deleted: usize,
    pub handlers_dropped: usize,
    /// Set when push synced `[server].env` from `pgweb.toml` into
    /// `pgweb.settings`. `None` when pgweb.toml didn't declare an env.
    pub env_synced: Option<String>,
}

pub fn push(app_dir: &Path, url: &str) -> Result<PushSummary> {
    let pages_dir = app_dir.join("pages");
    if !pages_dir.is_dir() {
        bail!(
            "no pages/ directory in {}; run `pg-web init` first",
            app_dir.display()
        );
    }

    let entries = paths::scan(&pages_dir)?;

    // Pre-flight: parse every HTML file as a Tera template before touching
    // the DB. Caught here, a broken `{% if %}` block names the file + line;
    // caught at render time, the user would see a generic 500 (prod) or
    // a dev error page (dev) — either way, better to fail loud at push.
    validate_templates(&entries)?;

    // Parse pgweb.toml once. env gets synced into pgweb.settings below;
    // other sections (database, dev, assets) stay the province of whoever
    // reads them.
    let toml_cfg = read_toml(app_dir)?;

    // Build the expected-state sets up front so the reconcile phase
    // has them ready without re-walking.
    let expected_routes: HashSet<(String, String)> = entries
        .iter()
        .map(|e| (e.method.clone(), e.route.clone()))
        .collect();
    let expected_templates: HashSet<String> = entries
        .iter()
        .filter_map(|e| e.template_path.clone())
        .collect();
    let expected_handlers: HashSet<String> =
        entries.iter().map(|e| e.handler_name.clone()).collect();

    let mut client =
        Client::connect(url, NoTls).with_context(|| format!("connecting to {url}"))?;
    let mut tx = client.transaction()?;

    let mut summary = PushSummary::default();

    // Phase 1 — apply desired state from the filesystem.
    for entry in &entries {
        apply_entry(&mut tx, entry, &mut summary)?;
    }

    // Phase 2 — validate each expected handler actually exists in the
    // DB with the right signature. Catches user typos in CREATE FUNCTION.
    for entry in &entries {
        validate_handler(&mut tx, entry)?;
    }

    // Phase 3 — reconcile: drop DB state that no longer has a backing
    // file. Routes first, then templates, then functions. Order doesn't
    // matter for correctness (no FKs) but this pattern reads top-down
    // from user-visible shape to physical storage.
    summary.routes_deleted = reconcile_routes(&mut tx, &expected_routes)?;
    summary.templates_deleted = reconcile_templates(&mut tx, &expected_templates)?;
    summary.handlers_dropped = reconcile_handlers(&mut tx, &expected_handlers)?;

    // Phase 4 — sync runtime settings. `pgweb.settings.env` becomes
    // whatever `[server].env` is in pgweb.toml, so deploying a new
    // image from the same source tree doesn't have to carry any
    // environment variable alongside.
    if let Some(env) = toml_cfg.server.env.as_deref() {
        sync_env(&mut tx, env)?;
        summary.env_synced = Some(env.to_string());
    }

    tx.commit()?;

    Ok(summary)
}

/// Parse every `.html` under `pages/` as a Tera template. Syntax errors
/// (unclosed blocks, unknown tags, mismatched braces) surface here —
/// before the DB transaction opens — so the live extension can't end up
/// with a bad template that 500s every request.
fn validate_templates(entries: &[RouteEntry]) -> Result<()> {
    for entry in entries {
        let Some(html_path) = &entry.html_path else {
            continue;
        };
        let source = fs::read_to_string(html_path)
            .with_context(|| format!("reading {}", html_path.display()))?;
        // Tera::new + add_raw_template is the idiomatic parse-only check.
        // An empty dir glob (`""`) leaves Tera with no file-backed templates;
        // we then register this one source under a stable name and let
        // parse errors surface.
        let mut tera = tera::Tera::default();
        if let Err(e) = tera.add_raw_template("__pg_web_push_validate__", &source) {
            bail!(
                "{}: Tera template failed to parse — {e}",
                html_path.display()
            );
        }
    }
    Ok(())
}

fn read_toml(app_dir: &Path) -> Result<PushTomlConfig> {
    let p = app_dir.join("pgweb.toml");
    if !p.is_file() {
        return Ok(PushTomlConfig::default());
    }
    let raw = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str::<PushTomlConfig>(&raw).with_context(|| format!("parsing {}", p.display()))
}

fn sync_env(tx: &mut Transaction<'_>, env: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO pgweb.settings (key, value) VALUES ('env', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        &[&env],
    )
    .with_context(|| format!("upserting pgweb.settings.env = {env}"))?;
    Ok(())
}

/// Apply one filesystem entry to the DB: run (or synthesize) its handler
/// function, upsert the template row, upsert the route row.
fn apply_entry(
    tx: &mut Transaction<'_>,
    entry: &RouteEntry,
    summary: &mut PushSummary,
) -> Result<()> {
    if let Some(sql_path) = &entry.sql_path {
        let sql = fs::read_to_string(sql_path)
            .with_context(|| format!("reading {}", sql_path.display()))?;
        tx.batch_execute(&sql)
            .with_context(|| format!("executing {}", sql_path.display()))?;
        summary.sql_files_executed += 1;
    } else if entry.html_path.is_some() {
        // Static route — no user .sql. Synthesize a trivial handler so
        // the router's uniform `(handler(req))::text` call path has
        // something to bind to. Returns `{}` so Tera renders the template
        // with an empty context.
        let synth = format!(
            "CREATE OR REPLACE FUNCTION {}(req json) RETURNS json \
             LANGUAGE sql IMMUTABLE AS $$ SELECT '{{}}'::json $$",
            entry.handler_name
        );
        tx.batch_execute(&synth)
            .with_context(|| format!("synthesizing handler {}", entry.handler_name))?;
        summary.synthesized_handlers += 1;
    }

    if let (Some(html_path), Some(template_path)) = (&entry.html_path, &entry.template_path) {
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
    .with_context(|| format!("upsert route {} {}", entry.method, entry.route))?;
    summary.routes_upserted += 1;
    Ok(())
}

/// Validate that `entry.handler_name` exists in `pg_proc` with the
/// expected signature. The expected return type is `json` when a
/// sibling template is declared, `text` when the handler ships bytes
/// directly. Missing-function and signature-mismatch both rollback.
fn validate_handler(tx: &mut Transaction<'_>, entry: &RouteEntry) -> Result<()> {
    let proname = entry
        .handler_name
        .strip_prefix("pgweb.")
        .ok_or_else(|| anyhow!("handler {} is not under schema pgweb.", entry.handler_name))?;
    let row = tx
        .query_opt(
            "SELECT pg_catalog.pg_get_function_arguments(p.oid), \
                    pg_catalog.pg_get_function_result(p.oid) \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'pgweb' AND p.proname = $1",
            &[&proname],
        )
        .with_context(|| format!("looking up {} in pg_proc", entry.handler_name))?;

    let source = entry
        .sql_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(synthesized handler)".to_string());

    let Some(row) = row else {
        bail!(
            "handler {} not found after push — did {} actually `CREATE FUNCTION {}(req json)`? \
             Check the function name and argument list.",
            entry.handler_name,
            source,
            entry.handler_name,
        );
    };

    let args: String = row.get(0);
    let rettype: String = row.get(1);

    if args != "req json" {
        bail!(
            "handler {} has signature ({args}) — expected (req json). Fix the argument list in {}.",
            entry.handler_name,
            source,
        );
    }

    let expected_rettype = if entry.template_path.is_some() {
        "json"
    } else {
        "text"
    };
    if rettype != expected_rettype {
        let why = if entry.template_path.is_some() {
            "sibling .html exists, so the JSON → Tera pipeline expects RETURNS json"
        } else {
            "no sibling .html, so raw-text mode expects RETURNS text"
        };
        bail!(
            "handler {} RETURNS {rettype} — {why}. Fix the RETURNS clause in {source}.",
            entry.handler_name,
        );
    }
    Ok(())
}

fn reconcile_routes(
    tx: &mut Transaction<'_>,
    expected: &HashSet<(String, String)>,
) -> Result<usize> {
    let rows = tx
        .query("SELECT method, path_pattern FROM pgweb.routes", &[])
        .context("listing pgweb.routes for reconcile")?;
    let mut deleted = 0usize;
    for row in rows {
        let method: String = row.get(0);
        let path: String = row.get(1);
        if !expected.contains(&(method.clone(), path.clone())) {
            tx.execute(
                "DELETE FROM pgweb.routes WHERE method = $1 AND path_pattern = $2",
                &[&method, &path],
            )
            .with_context(|| format!("deleting stale route {method} {path}"))?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

fn reconcile_templates(tx: &mut Transaction<'_>, expected: &HashSet<String>) -> Result<usize> {
    let rows = tx
        .query("SELECT template_path FROM pgweb.templates", &[])
        .context("listing pgweb.templates for reconcile")?;
    let mut deleted = 0usize;
    for row in rows {
        let template_path: String = row.get(0);
        if !expected.contains(&template_path) {
            tx.execute(
                "DELETE FROM pgweb.templates WHERE template_path = $1",
                &[&template_path],
            )
            .with_context(|| format!("deleting stale template {template_path}"))?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Drop any `pgweb.pages__*(json) RETURNS json|text` function that isn't
/// in the expected set. The `pages__` prefix + `(json)` signature is
/// the reserved push-managed namespace; any helper the user writes with
/// a different signature — `pgweb.helper_x(bigint)`, `pgweb.pages_util(text)`
/// — is left untouched.
fn reconcile_handlers(tx: &mut Transaction<'_>, expected: &HashSet<String>) -> Result<usize> {
    let rows = tx
        .query(
            "SELECT p.proname \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'pgweb' \
               AND p.proname LIKE 'pages\\_\\_%' ESCAPE '\\' \
               AND pg_catalog.pg_get_function_arguments(p.oid) = 'req json' \
               AND pg_catalog.pg_get_function_result(p.oid) IN ('json', 'text')",
            &[],
        )
        .context("listing pgweb.pages__* handlers for reconcile")?;
    let mut dropped = 0usize;
    for row in rows {
        let proname: String = row.get(0);
        if !is_safe_proname(&proname) {
            // Defensive: pg_proc.proname is always a valid SQL identifier,
            // but we interpolate into DROP FUNCTION so we validate anyway.
            continue;
        }
        let fqn = format!("pgweb.{proname}");
        if !expected.contains(&fqn) {
            tx.batch_execute(&format!("DROP FUNCTION pgweb.{proname}(json)"))
                .with_context(|| format!("dropping stale handler {fqn}"))?;
            dropped += 1;
        }
    }
    Ok(dropped)
}

/// Accept Postgres identifier body chars: ASCII letters, digits,
/// underscore, and `$` (used as the capture marker in dynamic-route
/// handler names, e.g., `pages__posts__$id__index`). Belt-and-suspenders
/// validation before string-interpolating a name into `DROP FUNCTION`.
fn is_safe_proname(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_proname_accepts_legal_identifiers() {
        assert!(is_safe_proname("pages__index"));
        assert!(is_safe_proname("pages__todos__toggle__post"));
        assert!(is_safe_proname("pages__a1_b2"));
    }

    #[test]
    fn safe_proname_accepts_dollar_for_capture_markers() {
        // Capture segments in dynamic routes emit `$name` in the handler
        // proname: `pages/posts/[id]/index.sql` → `pages__posts__$id__index`.
        assert!(is_safe_proname("pages__posts__$id__index"));
        assert!(is_safe_proname("pages__users__$user__posts__$post__index"));
    }

    #[test]
    fn safe_proname_rejects_metachars() {
        assert!(!is_safe_proname(""));
        assert!(!is_safe_proname("pages__foo; DROP TABLE users"));
        assert!(!is_safe_proname("pages__foo)--"));
        assert!(!is_safe_proname("pages__\"foo"));
        assert!(!is_safe_proname("pages__ foo"));
    }

    // ---- template validation (Component D) ----

    fn mk_entry(html: &std::path::Path) -> RouteEntry {
        RouteEntry {
            method: "GET".into(),
            route: "/".into(),
            handler_name: "pgweb.pages__index".into(),
            template_path: Some("pages/index.html".into()),
            html_path: Some(html.to_path_buf()),
            sql_path: None,
        }
    }

    #[test]
    fn validate_templates_accepts_well_formed_html() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.html");
        std::fs::write(&p, "<h1>hello {{ name }}</h1>").unwrap();
        let entries = vec![mk_entry(&p)];
        validate_templates(&entries).expect("well-formed template should parse");
    }

    #[test]
    fn validate_templates_rejects_unclosed_block_with_path_in_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("broken.html");
        std::fs::write(&p, "{% if x %}no endif").unwrap();
        let entries = vec![mk_entry(&p)];
        let err = validate_templates(&entries).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("broken.html"),
            "error should name the file: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("tera") || msg.contains("parse"),
            "error should flag it as a template problem: {msg}"
        );
    }

    #[test]
    fn validate_templates_skips_entries_without_html() {
        // raw-text route — no html_path, nothing to parse.
        let entries = vec![RouteEntry {
            method: "POST".into(),
            route: "/x".into(),
            handler_name: "pgweb.pages__x__post".into(),
            template_path: None,
            html_path: None,
            sql_path: Some(std::path::PathBuf::from("dummy.sql")),
        }];
        validate_templates(&entries).expect("raw-text routes have no template to validate");
    }
}
