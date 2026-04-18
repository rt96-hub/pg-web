//! Filesystem → route/template/handler derivation.
//!
//! The convention is formally specified in `docs/APP-LAYOUT.md`. In short:
//! every directory under `pages/` is a URL route; files inside are named
//! after the HTTP method they handle (`index` for GET, `post` for POST).
//! `.html` is a Tera template, `.sql` is a Postgres function.
//!
//! The public surface is `scan()` which walks a tree and yields one
//! `RouteEntry` per `(directory, method)` pair, pairing up `<stem>.html`
//! and `<stem>.sql` siblings into a single entry.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use walkdir::WalkDir;

/// One (method, route, handler, template) quadruple — the unit of work
/// for `pg-web push`. Each entry corresponds to a single row in
/// `pgweb.routes` plus optionally a row in `pgweb.templates` and/or a
/// `CREATE OR REPLACE FUNCTION` statement from a handler file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    /// HTTP method, uppercased. Phase 1: "GET" or "POST".
    pub method: String,
    /// URL path pattern this entry serves.
    pub route: String,
    /// Fully-qualified SQL function name, e.g. `pgweb.pages__todos__post`.
    pub handler_name: String,
    /// Storage key for the template row in `pgweb.templates` when the
    /// entry has an HTML file; `None` for raw-text handlers.
    pub template_path: Option<String>,
    /// Absolute filesystem path of the `.html` file (for reading).
    pub html_path: Option<PathBuf>,
    /// Absolute filesystem path of the `.sql` file (for reading).
    pub sql_path: Option<PathBuf>,
}

impl RouteEntry {
    /// True when both a template and a handler file exist — the default
    /// JSON-through-Tera pipeline.
    pub fn is_full(&self) -> bool {
        self.html_path.is_some() && self.sql_path.is_some()
    }
    /// True when only a template exists — static page, no SPI call.
    pub fn is_static(&self) -> bool {
        self.html_path.is_some() && self.sql_path.is_none()
    }
    /// True when only a handler exists — raw-text return, bytes-as-is.
    pub fn is_raw_text(&self) -> bool {
        self.html_path.is_none() && self.sql_path.is_some()
    }
}

/// Walk `pages_dir` and yield one entry per `(directory, method-stem)` pair.
/// Returns entries sorted by `(method, route)` for deterministic push order.
pub fn scan(pages_dir: &Path) -> Result<Vec<RouteEntry>> {
    if !pages_dir.is_dir() {
        bail!("pages directory does not exist: {}", pages_dir.display());
    }

    // Group candidate files by (parent_dir_relative, stem) → (html_path, sql_path).
    let mut by_key: BTreeMap<(PathBuf, String), (Option<PathBuf>, Option<PathBuf>)> =
        BTreeMap::new();

    for entry in WalkDir::new(pages_dir).sort_by_file_name() {
        let entry = entry.context("walking pages/")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some("html") => "html",
            Some("sql") => "sql",
            _ => continue, // .md / .gitkeep / etc. are silently ignored
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("non-UTF-8 filename: {}", path.display()))?
            .to_string();

        validate_stem(&stem, path)?;

        let parent_rel = path
            .parent()
            .and_then(|p| p.strip_prefix(pages_dir).ok())
            .ok_or_else(|| anyhow!("file outside pages/: {}", path.display()))?
            .to_path_buf();

        let slot = by_key.entry((parent_rel, stem)).or_default();
        match ext {
            "html" => slot.0 = Some(path.to_path_buf()),
            "sql" => slot.1 = Some(path.to_path_buf()),
            _ => unreachable!(),
        }
    }

    let mut out: Vec<RouteEntry> = by_key
        .into_iter()
        .map(|((parent_rel, stem), (html_path, sql_path))| {
            let segments = path_segments(&parent_rel);
            let method = method_for_stem(&stem).to_string();
            let route = build_route(&segments);
            let handler_name = build_handler_name(&segments, &stem);
            let template_path = html_path
                .as_ref()
                .map(|_| build_template_path(&segments, &stem));
            RouteEntry {
                method,
                route,
                handler_name,
                template_path,
                html_path,
                sql_path,
            }
        })
        .collect();

    // Sort by (method, route) so push order is deterministic and readable.
    out.sort_by(|a, b| (a.method.as_str(), a.route.as_str())
        .cmp(&(b.method.as_str(), b.route.as_str())));

    Ok(out)
}

/// Reject filenames that aren't one of the Phase 1 method stems. Error
/// messages point the user at the fix (move file into a subdirectory).
fn validate_stem(stem: &str, path: &Path) -> Result<()> {
    match stem {
        "index" | "post" => Ok(()),
        "get" => bail!(
            "{}: 'get' is reserved — use 'index' for GET handlers",
            path.display()
        ),
        "put" | "patch" | "delete" | "head" | "options" => bail!(
            "{}: '{stem}' is reserved for Phase 2+; Phase 1 supports 'index' (GET) and 'post' (POST) only",
            path.display()
        ),
        other => bail!(
            "{}: '{other}' is not a recognized method filename. \
             To add a nested route, move the file into a subdirectory: \
             pages/.../{other}/index.<ext>",
            path.display()
        ),
    }
}

fn method_for_stem(stem: &str) -> &'static str {
    match stem {
        "index" => "GET",
        "post" => "POST",
        _ => unreachable!("validate_stem should have rejected this"),
    }
}

/// Split a Path into its Normal components as strings. Non-Normal
/// components (Prefix/RootDir/CurDir/ParentDir) are filtered out —
/// they shouldn't appear for a path that was stripped of its `pages/`
/// prefix, and if they do we'd rather drop them than panic.
fn path_segments(rel: &Path) -> Vec<String> {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str().map(|s| s.to_string()),
            _ => None,
        })
        .collect()
}

/// Build the URL path from directory segments. Empty segments → `/`.
/// The method stem never appears in the URL — `index` is the GET default;
/// other stems are just method markers, not path pieces.
fn build_route(segments: &[String]) -> String {
    if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    }
}

/// Build the fully-qualified SQL handler function name. Format:
/// `pgweb.pages__<seg>__<seg>__...__<stem>`. The stem is always included
/// so GET handlers are `pgweb.pages__<path>__index` (distinct from POST
/// handlers `pgweb.pages__<path>__post`).
fn build_handler_name(segments: &[String], stem: &str) -> String {
    if segments.is_empty() {
        format!("pgweb.pages__{stem}")
    } else {
        format!("pgweb.pages__{}__{stem}", segments.join("__"))
    }
}

/// Build the storage key for a template row in `pgweb.templates`.
/// Always starts with `pages/` to match what the extension reads via SPI.
fn build_template_path(segments: &[String], stem: &str) -> String {
    if segments.is_empty() {
        format!("pages/{stem}.html")
    } else {
        format!("pages/{}/{stem}.html", segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    // ---- pure helpers ----

    #[test]
    fn build_route_root() {
        assert_eq!(build_route(&[]), "/");
    }

    #[test]
    fn build_route_single_segment() {
        assert_eq!(build_route(&["todos".into()]), "/todos");
    }

    #[test]
    fn build_route_nested() {
        assert_eq!(
            build_route(&["todos".into(), "toggle".into()]),
            "/todos/toggle"
        );
    }

    #[test]
    fn build_handler_name_root_index() {
        assert_eq!(build_handler_name(&[], "index"), "pgweb.pages__index");
    }

    #[test]
    fn build_handler_name_root_post() {
        assert_eq!(build_handler_name(&[], "post"), "pgweb.pages__post");
    }

    #[test]
    fn build_handler_name_nested_index() {
        assert_eq!(
            build_handler_name(&["todos".into()], "index"),
            "pgweb.pages__todos__index"
        );
    }

    #[test]
    fn build_handler_name_nested_post() {
        assert_eq!(
            build_handler_name(&["todos".into()], "post"),
            "pgweb.pages__todos__post"
        );
    }

    #[test]
    fn build_handler_name_deeply_nested() {
        assert_eq!(
            build_handler_name(&["todos".into(), "toggle".into()], "post"),
            "pgweb.pages__todos__toggle__post"
        );
    }

    #[test]
    fn build_template_path_root() {
        assert_eq!(build_template_path(&[], "index"), "pages/index.html");
    }

    #[test]
    fn build_template_path_nested() {
        assert_eq!(
            build_template_path(&["todos".into()], "post"),
            "pages/todos/post.html"
        );
    }

    #[test]
    fn method_for_stem_known() {
        assert_eq!(method_for_stem("index"), "GET");
        assert_eq!(method_for_stem("post"), "POST");
    }

    // ---- validate_stem ----

    #[test]
    fn validate_stem_accepts_index_and_post() {
        let p = Path::new("pages/x/index.html");
        assert!(validate_stem("index", p).is_ok());
        assert!(validate_stem("post", p).is_ok());
    }

    #[test]
    fn validate_stem_rejects_get_with_hint() {
        let err = validate_stem("get", Path::new("pages/x/get.html")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("'get' is reserved"), "msg: {msg}");
        assert!(msg.contains("index"), "msg: {msg}");
    }

    #[test]
    fn validate_stem_rejects_future_methods() {
        for stem in ["put", "patch", "delete", "head", "options"] {
            let err = validate_stem(stem, Path::new("pages/x/y.sql")).unwrap_err();
            assert!(format!("{err:#}").contains("Phase 2+"));
        }
    }

    #[test]
    fn validate_stem_rejects_arbitrary_name_with_fix_hint() {
        let err = validate_stem("about", Path::new("pages/about.html")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a recognized method filename"), "msg: {msg}");
        assert!(msg.contains("subdirectory"), "msg: {msg}");
    }

    // ---- scan() — walker on real directories ----

    #[test]
    fn scan_empty_pages_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        fs::create_dir_all(&pages).unwrap();
        assert_eq!(scan(&pages).unwrap(), vec![]);
    }

    #[test]
    fn scan_missing_pages_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = scan(&tmp.path().join("pages")).unwrap_err();
        assert!(format!("{err:#}").contains("does not exist"));
    }

    #[test]
    fn scan_root_index_pair_is_full_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "index.html", "<h1>{{ name }}</h1>");
        write(&pages, "index.sql", "CREATE FUNCTION pgweb.pages__index() ...");
        let got = scan(&pages).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].method, "GET");
        assert_eq!(got[0].route, "/");
        assert_eq!(got[0].handler_name, "pgweb.pages__index");
        assert_eq!(got[0].template_path.as_deref(), Some("pages/index.html"));
        assert!(got[0].is_full());
    }

    #[test]
    fn scan_html_only_is_static_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "about/index.html", "<h1>about</h1>");
        let got = scan(&pages).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].route, "/about");
        assert_eq!(got[0].method, "GET");
        assert!(got[0].is_static());
        assert_eq!(got[0].template_path.as_deref(), Some("pages/about/index.html"));
    }

    #[test]
    fn scan_sql_only_is_raw_text_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "todos/toggle/post.sql", "CREATE FUNCTION ...");
        let got = scan(&pages).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].method, "POST");
        assert_eq!(got[0].route, "/todos/toggle");
        assert_eq!(got[0].handler_name, "pgweb.pages__todos__toggle__post");
        assert_eq!(got[0].template_path, None);
        assert!(got[0].is_raw_text());
    }

    #[test]
    fn scan_todo_app_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "index.html", "root");
        write(&pages, "index.sql", "root handler");
        write(&pages, "todos/post.html", "new row");
        write(&pages, "todos/post.sql", "insert");
        write(&pages, "todos/toggle/post.sql", "update");
        write(&pages, "todos/delete/post.sql", "delete");
        let got = scan(&pages).unwrap();
        let summary: Vec<(&str, &str, bool, bool)> = got
            .iter()
            .map(|e| {
                (
                    e.method.as_str(),
                    e.route.as_str(),
                    e.html_path.is_some(),
                    e.sql_path.is_some(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("GET", "/", true, true),
                ("POST", "/todos", true, true),
                ("POST", "/todos/delete", false, true),
                ("POST", "/todos/toggle", false, true),
            ]
        );
    }

    #[test]
    fn scan_rejects_flat_html_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "about.html", "<h1>no</h1>");
        let err = scan(&pages).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("about"), "msg: {msg}");
        assert!(msg.contains("not a recognized method filename"), "msg: {msg}");
    }

    #[test]
    fn scan_rejects_reserved_stem_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "todos/put.sql", "UPDATE ...");
        let err = scan(&pages).unwrap_err();
        assert!(format!("{err:#}").contains("Phase 2+"));
    }

    #[test]
    fn scan_ignores_non_html_non_sql_files() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "index.html", "root");
        write(&pages, "README.md", "# notes"); // must NOT be treated as a route
        write(&pages, "todos/.gitkeep", "");
        let got = scan(&pages).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].route, "/");
    }
}
