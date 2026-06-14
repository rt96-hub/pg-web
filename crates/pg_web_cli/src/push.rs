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
use postgres::{Client, Transaction};
use serde::Deserialize;

use crate::db;
use crate::paths::{self, RouteEntry};
use crate::migrate;
use crate::retry;

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
    /// Per-request statement timeout (prompt 014). An interval literal such
    /// as "15s", "30s", "1min". Synced into pgweb.settings.request_timeout;
    /// the worker does `SET LOCAL statement_timeout = '...' ` inside each
    /// request transaction. Default (when absent) is "15s" in the extension.
    #[serde(default)]
    request_timeout: Option<String>,
    /// 018.1: when false the seeded default for the public /health is
    /// suppressed (normal router miss behavior). User /health routes win
    /// regardless. The protected /_pgweb/health is never affected.
    #[serde(default)]
    health_enabled: Option<bool>,
    /// Same for /readiness.
    #[serde(default)]
    readiness_enabled: Option<bool>,
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
    /// Set when push synced `[server].request_timeout` (prompt 014).
    pub request_timeout_synced: Option<String>,
    /// 018.1 health/readiness flags (true/false values that were explicitly
    /// present in the pushed pgweb.toml and therefore written to settings).
    pub health_enabled_synced: Option<bool>,
    pub readiness_enabled_synced: Option<bool>,
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
/// clearer when it names the offending file. Bumped from 2 MiB to
/// 20 MiB in v0.2 (Component I) — covers virtually every asset users
/// have outside of video, while staying within BYTEA's comfortable
/// TOAST range. True `pg_largeobject`-backed streaming for >20 MiB
/// assets is Phase 2+ work.
const MAX_ASSET_BYTES: u64 = 20 * 1024 * 1024;

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
    let mut assets = scan_public(app_dir)?;

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

    let mut client = db::connect(url, "push")?;

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

    // Component H — when env=production, fingerprint asset URLs and
    // rewrite literal references in templates. Dev mode keeps the
    // canonical URLs so the iteration loop stays predictable.
    let is_prod = matches!(toml_cfg.server.env.as_deref(), Some("production") | Some("prod"));
    let mut asset_rewrites: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if is_prod {
        for a in assets.iter_mut() {
            let hashed = fingerprint_url(&a.url_path, &a.fingerprint_hex);
            asset_rewrites.insert(a.url_path.clone(), hashed.clone());
            a.url_path = hashed;
        }
    }

    let expected_asset_paths: HashSet<String> =
        assets.iter().map(|a| a.url_path.clone()).collect();
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let host_opt: Option<String> = if host.is_empty() { None } else { Some(host) };
    let migrations_applied_i32: i32 =
        migrations_applied.try_into().unwrap_or(i32::MAX);
    let file_count: i32 = (entries.len() + assets.len()).try_into().unwrap_or(i32::MAX);

    // Wrap the whole transaction in retry::with_retry so a concurrent-DDL
    // race (`tuple concurrently updated` from another `pg-web push` /
    // `pg-web dev` writing `pg_proc` simultaneously, or a 40001
    // serialization failure) re-runs the whole tx with jittered backoff.
    // Each attempt opens a fresh transaction; nothing the host commits
    // survives a rollback, so the retry is safe.
    let tx_result = retry::with_retry(|| {
        let mut tx = client.transaction()?;
        let mut tx_summary = PushSummary::default();

        // Phase 1 — apply desired state from the filesystem.
        for entry in &entries {
            apply_entry(&mut tx, entry, &asset_rewrites, &mut tx_summary)?;
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
        tx_summary.routes_deleted = reconcile_routes(&mut tx, &expected_routes)?;
        tx_summary.templates_deleted = reconcile_templates(&mut tx, &expected_templates)?;
        tx_summary.handlers_dropped = reconcile_handlers(&mut tx, &expected_handlers)?;

        // Phase 3.5 — static assets. Upsert every file walked from `public/`,
        // then delete rows whose path isn't in the expected set.
        for a in &assets {
            upsert_asset(&mut tx, a)?;
            tx_summary.assets_upserted += 1;
        }
        tx_summary.assets_deleted = reconcile_assets(&mut tx, &expected_asset_paths)?;

        // Phase 4 — sync runtime settings. `pgweb.settings.env` becomes
        // whatever `[server].env` is in pgweb.toml, so deploying a new
        // image from the same source tree doesn't have to carry any
        // environment variable alongside.
        if let Some(env) = toml_cfg.server.env.as_deref() {
            sync_env(&mut tx, env)?;
            tx_summary.env_synced = Some(env.to_string());
        }
        if let Some(rt) = toml_cfg.server.request_timeout.as_deref() {
            sync_request_timeout(&mut tx, rt)?;
            tx_summary.request_timeout_synced = Some(rt.to_string());
        }
        if let Some(h) = toml_cfg.server.health_enabled {
            sync_health_enabled(&mut tx, h)?;
            tx_summary.health_enabled_synced = Some(h);
        }
        if let Some(r) = toml_cfg.server.readiness_enabled {
            sync_readiness_enabled(&mut tx, r)?;
            tx_summary.readiness_enabled_synced = Some(r);
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
        tx.execute(
            "INSERT INTO pgweb.deployments (from_host, file_count, migrations_applied) \
             VALUES ($1, $2, $3)",
            &[&host_opt.as_deref(), &file_count, &migrations_applied_i32],
        )
        .context("inserting pgweb.deployments ledger row")?;

        // Fire the cache-invalidation NOTIFY inside the tx. Postgres delivers
        // queued NOTIFYs atomically at COMMIT, so a rolled-back dry-run or
        // errored tx will not signal workers (exactly what we want). Real
        // pushes (including those that only touched env or assets) cause live
        // workers to drop their RouteSnapshot and rebuild on next request.
        // We use a dedicated channel (pgweb_reload) so browser livereload
        // and cache concerns stay separable.
        let _ = tx.execute("NOTIFY pgweb_reload", &[]);

        if opts.dry_run {
            // Rollback explicitly. Drop would implicitly roll back too, but
            // being explicit makes intent visible to future maintainers.
            tx.rollback()
                .context("rolling back dry-run transaction")?;
        } else {
            tx.commit().context("committing push transaction")?;
        }

        Ok(tx_summary)
    });

    let mut summary = match tx_result {
        Ok(s) => s,
        Err(e) => return Err(maybe_attach_concurrent_pusher_diag(e, url)),
    };
    summary.dry_run = opts.dry_run;
    summary.migrations_applied = migrations_applied;
    summary.migrations_applied_names = migrations_applied_names;

    Ok(summary)
}

/// On retry exhaustion (or any other retryable error), best-effort
/// open a fresh diagnostic connection and look up sibling `pg-web *`
/// connections in `pg_stat_activity`. The retry-context message tells
/// the user *what* happened; this tells them *who* to stop, with a
/// concrete `kill <pid>` (same host) or `pg_terminate_backend(<pid>)`
/// (remote host) suggestion. Diagnostic failures are swallowed — the
/// caller still sees the underlying retry error.
fn maybe_attach_concurrent_pusher_diag(err: anyhow::Error, url: &str) -> anyhow::Error {
    if !retry::is_retryable(&err) {
        return err;
    }
    match gather_concurrent_pushers(url) {
        Ok(diag) if !diag.is_empty() => err.context(diag),
        _ => err,
    }
}

/// Connect with a `diag` verb and ask `pg_stat_activity` who else is
/// pushing. Returns a multi-line, ready-to-print summary or empty
/// string if no other pg-web clients are connected.
fn gather_concurrent_pushers(url: &str) -> Result<String> {
    let mut client = db::connect(url, "diag")?;
    let rows = client
        .query(
            "SELECT pid, application_name \
             FROM pg_stat_activity \
             WHERE application_name LIKE 'pg-web %' \
               AND pid <> pg_backend_pid() \
             ORDER BY backend_start",
            &[],
        )
        .context("querying pg_stat_activity for sibling pg-web connections")?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    let local_host = gethostname::gethostname().to_string_lossy().into_owned();
    let mut out = String::from(
        "concurrent `pg-web` connections detected. Stop these to clear the conflict:",
    );
    for row in rows {
        let backend_pid: i32 = row.get(0);
        let app_name: String = row.get(1);
        let line = format_pusher_line(&app_name, backend_pid, &local_host);
        out.push_str("\n  - ");
        out.push_str(&line);
    }
    Ok(out)
}

/// Format one row from `pg_stat_activity` for the diagnostic output.
/// Public-but-untested-via-this-name only because pulling
/// `db::parse_application_name` into a unit test here would require
/// constructing pg_stat_activity rows from scratch; the parser itself
/// is unit-tested in `db::tests`.
fn format_pusher_line(app_name: &str, backend_pid: i32, local_host: &str) -> String {
    match db::parse_application_name(app_name) {
        Some(tag) if tag.host == local_host => format!(
            "{app_name} (backend pid {backend_pid}) — same host; stop with: kill {os_pid}",
            os_pid = tag.pid,
        ),
        Some(tag) => format!(
            "{app_name} (backend pid {backend_pid}) — host {host}; stop with: \
             ssh {host} 'kill {os_pid}', or run \
             SELECT pg_terminate_backend({backend_pid}); from psql",
            host = tag.host,
            os_pid = tag.pid,
        ),
        None => format!(
            "{app_name} (backend pid {backend_pid}) — unrecognized format; stop with: \
             SELECT pg_terminate_backend({backend_pid}); from psql"
        ),
    }
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

fn sync_request_timeout(tx: &mut Transaction<'_>, timeout: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO pgweb.settings (key, value) VALUES ('request_timeout', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        &[&timeout],
    )
    .with_context(|| format!("upserting pgweb.settings.request_timeout = {timeout}"))?;
    Ok(())
}

fn sync_health_enabled(tx: &mut Transaction<'_>, enabled: bool) -> Result<()> {
    let v = if enabled { "true" } else { "false" };
    tx.execute(
        "INSERT INTO pgweb.settings (key, value) VALUES ('health_enabled', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        &[&v],
    )
    .with_context(|| format!("upserting pgweb.settings.health_enabled = {v}"))?;
    Ok(())
}

fn sync_readiness_enabled(tx: &mut Transaction<'_>, enabled: bool) -> Result<()> {
    let v = if enabled { "true" } else { "false" };
    tx.execute(
        "INSERT INTO pgweb.settings (key, value) VALUES ('readiness_enabled', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        &[&v],
    )
    .with_context(|| format!("upserting pgweb.settings.readiness_enabled = {v}"))?;
    Ok(())
}

/// Apply one filesystem entry to the DB: run (or synthesize) its handler
/// function, upsert the template row, upsert the route row. `asset_rewrites`
/// maps canonical asset URLs (`/styles.css`) to their fingerprinted form
/// (`/styles.<hex>.css`); applied to template content before upsert in
/// production-mode pushes. Empty in dev mode — no rewrite.
fn apply_entry(
    tx: &mut Transaction<'_>,
    entry: &RouteEntry,
    asset_rewrites: &std::collections::HashMap<String, String>,
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
        let raw = fs::read_to_string(html_path)
            .with_context(|| format!("reading {}", html_path.display()))?;
        let content = if asset_rewrites.is_empty() {
            raw
        } else {
            rewrite_asset_refs(&raw, asset_rewrites)
        };
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

    // Response contract v2 (prompt 013) relaxation:
    // - template route (sibling .html) → must still be RETURNS json (the envelope
    //   or a bare context object for Tera are both valid JSON).
    // - raw-text route (no .html) → RETURNS text (verbatim body) *or* json
    //   (either a bare JSON string body or a "$pgweb" response envelope).
    // The router disambiguates at runtime via the envelope marker; the CLI
    // only checks the declared SQL return type is plausible for the mode.
    let ok = if entry.template_path.is_some() {
        rettype == "json"
    } else {
        rettype == "json" || rettype == "text"
    };
    if !ok {
        let why = if entry.template_path.is_some() {
            "sibling .html exists, so the JSON → Tera pipeline expects RETURNS json (envelope or bare context)"
        } else {
            "no sibling .html (raw-text mode): RETURNS text for verbatim body, or RETURNS json for a response envelope (pgweb.respond / redirect / json) or a plain JSON body"
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
        .query("SELECT method, path_pattern, handler_name FROM pgweb.routes", &[])
        .context("listing pgweb.routes for reconcile")?;
    let mut deleted = 0usize;
    for row in rows {
        let method: String = row.get(0);
        let path: String = row.get(1);
        let handler_name: String = row.get(2);
        if !expected.contains(&(method.clone(), path.clone())) {
            // Preserve framework-seeded default routes (the _default_* handlers
            // from the extension bootstrap) so that /health and /readiness
            // "just work" after `pg-web push` on a minimal init that doesn't
            // customize them. User-provided routes for those paths will have
            // already replaced the row via the ON CONFLICT upsert in apply_entry.
            if handler_name.starts_with("pgweb._default_") {
                continue;
            }
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
    /// First 8 hex chars of the Blake3 hash of `content`. Used at push
    /// time to build the fingerprinted URL (`/styles.<hex>.css`) when
    /// env=production. Component H. 8 hex chars = 32 bits = ~4 billion
    /// possible values, plenty for realistic app sizes (Vite uses 8).
    fingerprint_hex: String,
}

/// Walk `<app_dir>/public/` recursively, hashing each file into an
/// `Asset` record. Returns an empty vec if `public/` is missing or
/// empty — a route-only app is valid. Errors on any file exceeding
/// the 20 MiB cap so the user gets a clear name-the-file error instead
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
                "{}: asset is {} bytes (cap is {} bytes / 20 MiB). \
                 Larger assets via pg_largeobject streaming remain Phase 2+ work — \
                 host on a CDN until then.",
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
        let hash = blake3::hash(&content);
        let etag = format!("\"{}\"", hash.to_hex());
        let fingerprint_hex = hash.to_hex().as_str()[..8].to_string();

        out.push(Asset {
            url_path,
            content,
            content_type,
            etag,
            fingerprint_hex,
        });
    }
    Ok(out)
}

/// Build the fingerprinted URL for an asset. `/styles.css` + hex `abcd1234`
/// becomes `/styles.abcd1234.css`. Files without an extension or hidden-file
/// patterns (`.gitkeep`-style with leading dot in the basename) get the hex
/// appended as a new last segment instead — the rewrite still produces a
/// stable URL but the asset is unlikely to be referenced from a template
/// either way. Component H.
fn fingerprint_url(canonical: &str, hex: &str) -> String {
    let (dir, file) = match canonical.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", canonical),
    };
    // `rsplit_once('.')` on `styles.css` → ("styles", "css"). On
    // `.gitkeep` → ("", "gitkeep") — the empty stem is a leading-dot
    // file (hidden), append-as-new-segment is the safer rewrite.
    match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            format!("{dir}/{stem}.{hex}.{ext}")
        }
        _ => format!("{dir}/{file}.{hex}"),
    }
}

/// True when `url` looks like a fingerprinted path produced by
/// [`fingerprint_url`] — i.e. the last segment matches `*.<hex>.<ext>$`
/// where the hex run is at least 8 chars (we emit exactly 8). Mirrors
/// the extension's `http::is_fingerprinted_url`; CLI-side currently
/// only tests with it (the actual cache-policy decision happens in the
/// router), so `#[cfg(test)]` keeps it from being dead code in the
/// shipping binary. Keep the two definitions in sync — the spec is
/// "what does push emit." Component H.
#[cfg(test)]
fn is_fingerprinted_url(url: &str) -> bool {
    let file = url.rsplit_once('/').map(|(_, f)| f).unwrap_or(url);
    let parts: Vec<&str> = file.split('.').collect();
    if parts.len() < 3 {
        return false;
    }
    let hash_part = parts[parts.len() - 2];
    hash_part.len() >= 8 && hash_part.chars().all(|c| c.is_ascii_hexdigit())
}

/// Rewrite literal asset references in `html` from canonical URLs to
/// their fingerprinted form. Operates on `"<url>"` substrings — i.e.,
/// the value inside `href="..."` / `src="..."` / `srcset="..."` etc.
/// Single-quoted attributes (`href='...'`) and unquoted attributes
/// (`href=/foo.css`) are NOT rewritten — both are valid HTML but
/// unconventional in templates; document the limitation rather than
/// fight a regex zoo. Dynamic refs (`<img src="{{ user.avatar }}">`)
/// can't be rewritten at push time either. Component H.
fn rewrite_asset_refs(
    html: &str,
    rewrites: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = html.to_string();
    for (canonical, hashed) in rewrites {
        // Wrap with double quotes so we only match attribute-value
        // contexts, not e.g. naked text or comments.
        let from = format!("\"{canonical}\"");
        let to = format!("\"{hashed}\"");
        out = out.replace(&from, &to);
    }
    out
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
        // 20 MiB + 1 byte triggers the cap. 20 MiB is the v0.2 ceiling
        // (Component I); true `pg_largeobject` streaming for larger
        // assets remains a Phase-2+ follow-up.
        let big = vec![0u8; (20 * 1024 * 1024) + 1];
        std::fs::write(pub_dir.join("huge.bin"), &big).unwrap();
        let err = scan_public(dir.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("huge.bin"), "error names the file: {msg}");
        assert!(
            msg.contains("20 MiB") || msg.contains(&format!("{}", MAX_ASSET_BYTES)),
            "error mentions the cap: {msg}"
        );
    }

    #[test]
    fn scan_public_accepts_asset_just_under_cap() {
        // A 5 MiB asset would have been rejected at v0.1's 2 MiB cap;
        // v0.2 (Component I) accepts up to 20 MiB. Lock the new floor
        // by exercising a file the prior cap would have rejected.
        let dir = tempfile::tempdir().unwrap();
        let pub_dir = dir.path().join("public");
        std::fs::create_dir(&pub_dir).unwrap();
        let medium = vec![0u8; 5 * 1024 * 1024];
        std::fs::write(pub_dir.join("hero.png"), &medium).unwrap();
        let assets = scan_public(dir.path()).expect("5 MiB file fits under 20 MiB cap");
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].url_path, "/hero.png");
        assert_eq!(assets[0].content.len(), 5 * 1024 * 1024);
    }

    // ---- fingerprinted asset URLs (Component H) ----

    #[test]
    fn fingerprint_url_inserts_hex_before_extension() {
        assert_eq!(
            fingerprint_url("/styles.css", "abcd1234"),
            "/styles.abcd1234.css"
        );
        assert_eq!(
            fingerprint_url("/img/logo.png", "deadbeef"),
            "/img/logo.deadbeef.png"
        );
        // Multi-dot stems keep the original structure: `app.min.js`
        // becomes `app.min.<hex>.js`. Splits on the LAST dot.
        assert_eq!(
            fingerprint_url("/js/app.min.js", "12345678"),
            "/js/app.min.12345678.js"
        );
    }

    #[test]
    fn fingerprint_url_appends_hex_for_extensionless_or_hidden() {
        // No extension at all — append as new last segment so the URL
        // still changes per content version.
        assert_eq!(
            fingerprint_url("/README", "abcd1234"),
            "/README.abcd1234"
        );
        // Hidden-file pattern — leading dot, empty stem.
        assert_eq!(
            fingerprint_url("/.gitkeep", "abcd1234"),
            "/.gitkeep.abcd1234"
        );
    }

    #[test]
    fn is_fingerprinted_url_matches_fingerprinted_paths() {
        assert!(is_fingerprinted_url("/styles.abcd1234.css"));
        assert!(is_fingerprinted_url("/img/logo.deadbeef.png"));
        assert!(is_fingerprinted_url("/js/app.min.12345678.js"));
    }

    #[test]
    fn is_fingerprinted_url_rejects_canonical_paths() {
        // No middle hex segment.
        assert!(!is_fingerprinted_url("/styles.css"));
        assert!(!is_fingerprinted_url("/img/logo.png"));
        // Middle segment is not hex.
        assert!(!is_fingerprinted_url("/styles.minified.css"));
        // Hex but too short (< 8 chars).
        assert!(!is_fingerprinted_url("/styles.abc.css"));
    }

    #[test]
    fn rewrite_asset_refs_replaces_quoted_attribute_values() {
        let mut map = std::collections::HashMap::new();
        map.insert("/styles.css".to_string(), "/styles.abcd1234.css".to_string());

        let html = r#"<link href="/styles.css" rel="stylesheet">"#;
        let out = rewrite_asset_refs(html, &map);
        assert_eq!(
            out,
            r#"<link href="/styles.abcd1234.css" rel="stylesheet">"#
        );
    }

    #[test]
    fn rewrite_asset_refs_skips_unrelated_strings() {
        let mut map = std::collections::HashMap::new();
        map.insert("/styles.css".to_string(), "/styles.abcd1234.css".to_string());

        // Naked text mentioning the path is intentionally rewritten too — we
        // err on the side of "if the user typed the URL literally, they
        // probably meant to reference it" and keep the rewrite scope simple.
        // The double-quote requirement is the actual filter: prose without
        // quotes is left alone.
        let html = r#"see /styles.css for layout. Also: <a href="/about">about</a>"#;
        let out = rewrite_asset_refs(html, &map);
        assert!(out.contains("/styles.css"), "unquoted prose stays intact: {out}");
        assert!(out.contains(r#""/about""#), "unrelated href untouched: {out}");
    }

    #[test]
    fn rewrite_asset_refs_handles_multiple_assets() {
        let mut map = std::collections::HashMap::new();
        map.insert("/styles.css".to_string(), "/styles.aaaaaaaa.css".to_string());
        map.insert("/img/logo.png".to_string(), "/img/logo.bbbbbbbb.png".to_string());

        let html = r#"<link href="/styles.css"><img src="/img/logo.png">"#;
        let out = rewrite_asset_refs(html, &map);
        assert!(out.contains("/styles.aaaaaaaa.css"));
        assert!(out.contains("/img/logo.bbbbbbbb.png"));
        // No leftover canonical URLs.
        assert!(!out.contains("\"/styles.css\""));
        assert!(!out.contains("\"/img/logo.png\""));
    }

    #[test]
    fn rewrite_asset_refs_no_op_when_map_is_empty() {
        let html = r#"<link href="/styles.css">"#;
        let out = rewrite_asset_refs(html, &std::collections::HashMap::new());
        assert_eq!(out, html);
    }
}
