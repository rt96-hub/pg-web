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
use crate::migrate;

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
    pub assets_upserted: usize,
    pub assets_deleted: usize,
    /// Number of migration files that were applied during this push.
    /// Always 0 unless `PushOptions::with_migrate` was set AND there
    /// were pending migrations; under `dry_run` reports what WOULD be
    /// applied but doesn't run them.
    pub migrations_applied: usize,
    /// Names of migrations actually applied (or, under dry-run, that
    /// would be). Useful for the CLI to echo file-by-file.
    pub migrations_applied_names: Vec<String>,
    /// True when push was invoked with `--dry-run`. Callers render
    /// their summary with a "[dry-run]" prefix; the DB transaction
    /// was rolled back so nothing here persisted.
    pub dry_run: bool,
}

/// Knobs for `push_with_options`. Defaults match the historic `push()`
/// behavior exactly (no dry-run, no migrate), so existing callers that
/// use `push()` see no change.
#[derive(Debug, Default, Clone, Copy)]
pub struct PushOptions {
    /// When true, do every normal step inside the transaction but
    /// ROLLBACK at the end instead of COMMIT. No DB-side effect, no
    /// `pgweb.deployments` row, no migrations applied. The summary
    /// still reports what WOULD have happened.
    pub dry_run: bool,
    /// When true, any pending migrations get applied before push
    /// starts. Without this flag, push refuses to run if migrations
    /// are pending — the `column does not exist` class of failure is
    /// almost always "push preceded its migration."
    pub with_migrate: bool,
}

/// Hard cap on per-asset size. Matches the `CHECK` in schema.rs so the
/// CLI catches oversized files before the DB does — the error is much
/// clearer when it names the offending file.
const MAX_ASSET_BYTES: u64 = 2 * 1024 * 1024;

/// Sync the filesystem app under `app_dir` into the Postgres at `url`.
///
/// # Connectivity note (pre-v0.1)
///
/// This function opens a direct libpq connection to `url`. In practice
/// that means **the DB must be reachable from wherever the CLI runs** —
/// typically `localhost:5432` against a local Docker stack. Pushing to a
/// remote production stack requires either:
///
/// 1. Exposing the remote's `:5432` to the internet (**don't** — the
///    scaffolded docker-compose publishes it with a dev password for
///    local-loopback convenience only; it MUST be removed before prod).
/// 2. SSH-tunneling: `ssh -L 5432:localhost:5432 deploy@vps` then
///    pointing push at `postgres://…@localhost:5432/app`.
/// 3. Putting the VPS on a private overlay (Tailscale / WireGuard) and
///    using the overlay address.
/// 4. SSHing in and running `pg-web push` on the server itself (requires
///    the CLI to be present there — see Session 4 Component F.3).
///
/// The automated path — `pg-web push --target <name>` with SSH-tunnel
/// plumbing — is tracked as Session 4 Component F.2. Until it ships,
/// users handle the tunnel themselves. See `docs/DEPLOYMENT.md`.
pub fn push(app_dir: &Path, url: &str) -> Result<PushSummary> {
    push_with_options(app_dir, url, PushOptions::default())
}

/// Full-featured variant. Behavior with `PushOptions::default()` is
/// identical to `push(app_dir, url)` — `push` is a thin wrapper around
/// this. New callers wanting dry-run / with-migrate semantics should
/// call this directly.
pub fn push_with_options(
    app_dir: &Path,
    url: &str,
    opts: PushOptions,
) -> Result<PushSummary> {
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

    // Walk `public/` for static assets — same fail-loud philosophy. We
    // hash + read once here so the DB transaction is just inserts.
    let assets = scan_public(app_dir)?;

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

    // --- Migration gate (F.1) -----------------------------------------
    // Check for pending migrations BEFORE push's own transaction opens.
    // If pending and !with_migrate: bail, push would likely fail later
    // against a schema that hasn't caught up. If pending and with_migrate
    // and !dry_run: apply them now (migrate::apply opens + commits its
    // own transactions per file). If dry_run: report what would apply
    // but don't touch the DB.
    let pending = pending_migrations(&mut client, app_dir)?;
    let mut migrations_applied = 0usize;
    let mut migrations_applied_names: Vec<String> = Vec::new();
    if !pending.is_empty() {
        if !opts.with_migrate {
            bail!(
                "pending migrations: {}. \
                 Run `pg-web migrate apply` first, or re-run with `pg-web push --with-migrate`.",
                pending.join(", ")
            );
        }
        if opts.dry_run {
            // Don't apply. Report as "would apply".
            migrations_applied = pending.len();
            migrations_applied_names = pending.clone();
        } else {
            let applied = migrate::apply(app_dir, url)
                .context("applying pending migrations before push")?;
            migrations_applied = applied.applied.len();
            migrations_applied_names = applied.applied;
        }
    }

    let mut tx = client.transaction()?;

    let mut summary = PushSummary::default();
    summary.dry_run = opts.dry_run;
    summary.migrations_applied = migrations_applied;
    summary.migrations_applied_names = migrations_applied_names;

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

    // Phase 3.5 — static assets. Upsert every file walked from `public/`,
    // then delete rows whose path isn't in the expected set.
    let expected_asset_paths: HashSet<String> =
        assets.iter().map(|a| a.url_path.clone()).collect();
    for a in &assets {
        upsert_asset(&mut tx, a)?;
        summary.assets_upserted += 1;
    }
    summary.assets_deleted = reconcile_assets(&mut tx, &expected_asset_paths)?;

    // Phase 4 — sync runtime settings. `pgweb.settings.env` becomes
    // whatever `[server].env` is in pgweb.toml, so deploying a new
    // image from the same source tree doesn't have to carry any
    // environment variable alongside.
    if let Some(env) = toml_cfg.server.env.as_deref() {
        sync_env(&mut tx, env)?;
        summary.env_synced = Some(env.to_string());
    }

    // Phase 5 — deployments ledger (F.1). One row per successful push
    // with a snapshot of what we just shipped. Under dry_run we still
    // insert (so the row is visible to any in-tx SELECT below), but
    // the tx then rolls back, so the row never persists.
    //
    // file_count counts FILES FROM DISK touched by this push (routes
    // from pages/ + static assets from public/), not DB-side upserts.
    // That's the signal ops actually want: "how many files did this
    // deploy bring across?"
    let file_count: i32 = (entries.len() + assets.len()).try_into().unwrap_or(i32::MAX);
    let migrations_applied_i32: i32 =
        summary.migrations_applied.try_into().unwrap_or(i32::MAX);
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let host_opt: Option<&str> = if host.is_empty() { None } else { Some(&host) };
    tx.execute(
        "INSERT INTO pgweb.deployments (from_host, file_count, migrations_applied) \
         VALUES ($1, $2, $3)",
        &[&host_opt, &file_count, &migrations_applied_i32],
    )
    .context("inserting pgweb.deployments ledger row")?;

    if opts.dry_run {
        // Rollback explicitly. Drop would implicitly roll back too, but
        // being explicit makes intent visible to future maintainers.
        tx.rollback()
            .context("rolling back dry-run transaction")?;
    } else {
        tx.commit().context("committing push transaction")?;
    }

    Ok(summary)
}

/// Compute which `migrations/*.sql` files exist locally but aren't yet
/// in `pgweb.migrations`. Used by `push_with_options` to decide whether
/// to bail, apply, or (under dry-run) report.
fn pending_migrations(client: &mut Client, app_dir: &Path) -> Result<Vec<String>> {
    let migrations_dir = app_dir.join("migrations");
    if !migrations_dir.is_dir() {
        return Ok(Vec::new());
    }

    let local = migrate::discover(&migrations_dir)?;
    if local.is_empty() {
        return Ok(Vec::new());
    }

    // Use a short-lived read-only query. Running outside push's main tx
    // so a NoSuchTable situation (pre-extension-install DB) would fail
    // here with a clearer error than deep inside push.
    let rows = client
        .query("SELECT name FROM pgweb.migrations", &[])
        .context("reading pgweb.migrations (is pg_web_ext installed?)")?;
    let applied: HashSet<String> = rows.into_iter().map(|r| r.get::<_, String>(0)).collect();

    Ok(local
        .into_iter()
        .filter_map(|(name, _sql)| if applied.contains(&name) { None } else { Some(name) })
        .collect())
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

/// One asset discovered under `public/`, already read + hashed in
/// memory. Kept tiny; we don't hold refs to the filesystem after scan.
#[derive(Debug, Clone)]
struct Asset {
    /// URL path the browser uses, e.g. `/styles.css`. Leading slash
    /// always present. Subdirs preserved: `public/img/logo.png` →
    /// `/img/logo.png`.
    url_path: String,
    content: Vec<u8>,
    content_type: String,
    /// HTTP ETag value in its exact on-the-wire form (double-quoted).
    etag: String,
}

/// Walk `<app_dir>/public/` recursively, hashing each file into an
/// `Asset` record. Returns an empty vec if `public/` is missing or
/// empty — a route-only app is valid. Errors on any file exceeding
/// the 2 MiB cap so the user gets a clear name-the-file error instead
/// of the CHECK-constraint bounce-back from Postgres.
fn scan_public(app_dir: &Path) -> Result<Vec<Asset>> {
    use walkdir::WalkDir;

    let public_dir = app_dir.join("public");
    if !public_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(&public_dir).sort_by_file_name() {
        let entry = entry.context("walking public/")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        // `.gitkeep` is allowed by init to keep the empty dir in git,
        // but pushing it as a route is silly — skip.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == ".gitkeep" {
            continue;
        }

        let meta = entry.metadata().with_context(|| format!("stat {}", path.display()))?;
        if meta.len() > MAX_ASSET_BYTES {
            bail!(
                "{}: asset is {} bytes (cap is {} bytes / 2 MiB). \
                 Larger assets via pg_largeobject are deferred to M1.4.",
                path.display(),
                meta.len(),
                MAX_ASSET_BYTES,
            );
        }

        let rel = path
            .strip_prefix(&public_dir)
            .unwrap_or(path)
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF-8 path: {}", path.display()))?
            .replace('\\', "/");
        let url_path = format!("/{rel}");

        let content =
            fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .as_ref()
            .to_string();
        let etag = blake3_etag(&content);

        out.push(Asset {
            url_path,
            content,
            content_type,
            etag,
        });
    }
    Ok(out)
}

/// Format the Blake3 content hash as a strong HTTP ETag literal. The
/// stored value is what we want the browser to send back verbatim in
/// `If-None-Match` — keeping the double quotes in-DB means the router
/// can emit headers without any per-request formatting.
fn blake3_etag(content: &[u8]) -> String {
    let hex = blake3::hash(content).to_hex();
    format!("\"{hex}\"")
}

fn upsert_asset(tx: &mut Transaction<'_>, a: &Asset) -> Result<()> {
    tx.execute(
        "INSERT INTO pgweb.assets (path, content, content_type, etag) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (path) DO UPDATE \
           SET content = EXCLUDED.content, \
               content_type = EXCLUDED.content_type, \
               etag = EXCLUDED.etag",
        &[&a.url_path, &a.content, &a.content_type, &a.etag],
    )
    .with_context(|| format!("upsert asset {}", a.url_path))?;
    Ok(())
}

fn reconcile_assets(tx: &mut Transaction<'_>, expected: &HashSet<String>) -> Result<usize> {
    let rows = tx
        .query("SELECT path FROM pgweb.assets", &[])
        .context("listing pgweb.assets for reconcile")?;
    let mut deleted = 0usize;
    for row in rows {
        let path: String = row.get(0);
        if !expected.contains(&path) {
            tx.execute("DELETE FROM pgweb.assets WHERE path = $1", &[&path])
                .with_context(|| format!("deleting stale asset {path}"))?;
            deleted += 1;
        }
    }
    Ok(deleted)
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

    // ---- static asset scan (Component E) ----

    #[test]
    fn scan_public_returns_empty_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        // No public/ subdir.
        let assets = scan_public(dir.path()).unwrap();
        assert!(assets.is_empty());
    }

    #[test]
    fn scan_public_returns_empty_when_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("public")).unwrap();
        let assets = scan_public(dir.path()).unwrap();
        assert!(assets.is_empty());
    }

    #[test]
    fn scan_public_skips_gitkeep() {
        let dir = tempfile::tempdir().unwrap();
        let pub_dir = dir.path().join("public");
        std::fs::create_dir(&pub_dir).unwrap();
        std::fs::write(pub_dir.join(".gitkeep"), "").unwrap();
        let assets = scan_public(dir.path()).unwrap();
        assert!(assets.is_empty());
    }

    #[test]
    fn scan_public_picks_up_flat_and_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        let pub_dir = dir.path().join("public");
        std::fs::create_dir_all(pub_dir.join("img")).unwrap();
        std::fs::write(pub_dir.join("styles.css"), "body{}").unwrap();
        std::fs::write(pub_dir.join("img/logo.png"), b"\x89PNG\r\n").unwrap();

        let mut assets = scan_public(dir.path()).unwrap();
        assets.sort_by(|a, b| a.url_path.cmp(&b.url_path));

        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].url_path, "/img/logo.png");
        assert!(
            assets[0].content_type.starts_with("image/"),
            "png content_type: {}",
            assets[0].content_type
        );
        assert_eq!(assets[1].url_path, "/styles.css");
        assert_eq!(assets[1].content_type, "text/css");
    }

    #[test]
    fn scan_public_rejects_oversized_asset() {
        let dir = tempfile::tempdir().unwrap();
        let pub_dir = dir.path().join("public");
        std::fs::create_dir(&pub_dir).unwrap();
        // 2MiB + 1 byte.
        let big = vec![0u8; (2 * 1024 * 1024) + 1];
        std::fs::write(pub_dir.join("huge.bin"), &big).unwrap();
        let err = scan_public(dir.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("huge.bin"), "error names the file: {msg}");
        assert!(
            msg.contains("2 MiB") || msg.contains(&format!("{}", MAX_ASSET_BYTES)),
            "error mentions the cap: {msg}"
        );
    }

    #[test]
    fn blake3_etag_is_quoted_and_stable() {
        let a = blake3_etag(b"hello");
        let b = blake3_etag(b"hello");
        assert_eq!(a, b, "same bytes → same ETag");
        assert!(a.starts_with('"') && a.ends_with('"'), "etag is double-quoted");
        // Blake3 hex output is 64 chars → 66 chars with quotes.
        assert_eq!(a.len(), 66);

        let c = blake3_etag(b"world");
        assert_ne!(a, c, "different bytes → different ETag");
    }
}
