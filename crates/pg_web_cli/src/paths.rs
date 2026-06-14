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
    /// HTTP method, uppercased. Supported: GET (via index), POST, PUT, PATCH, DELETE.
    /// HEAD and OPTIONS are auto-derived by the extension (not authorable stems).
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

        let parent_rel = path
            .parent()
            .and_then(|p| p.strip_prefix(pages_dir).ok())
            .ok_or_else(|| anyhow!("file outside pages/: {}", path.display()))?
            .to_path_buf();

        validate_stem(&stem, &parent_rel, path)?;

        let slot = by_key.entry((parent_rel, stem)).or_default();
        match ext {
            "html" => slot.0 = Some(path.to_path_buf()),
            "sql" => slot.1 = Some(path.to_path_buf()),
            _ => unreachable!(),
        }
    }

    let mut out: Vec<RouteEntry> = Vec::with_capacity(by_key.len());
    for ((parent_rel, stem), (html_path, sql_path)) in by_key {
        let path_for_err = html_path
            .as_ref()
            .or(sql_path.as_ref())
            .expect("entry has at least one of html/sql path");
        // Two views of the directory components: `raw_segments` is the
        // verbatim filesystem form (with `[id]` brackets intact, used for
        // the template storage key), `segments` is validated + typed
        // (Static / Capture, used for the URL pattern and SQL handler name).
        let raw_segments = path_segments(&parent_rel);
        let segments = parse_segments(&parent_rel)
            .with_context(|| format!("{}: invalid path segment", path_for_err.display()))?;
        let method = method_for_stem(&stem).to_string();
        let route = build_route(&segments);
        let handler_name = build_handler_name(&segments, &stem);
        let template_path = html_path
            .as_ref()
            .map(|_| build_template_path(&raw_segments, &stem));
        out.push(RouteEntry {
            method,
            route,
            handler_name,
            template_path,
            html_path,
            sql_path,
        });
    }

    // Sort by (method, route) so push order is deterministic and readable.
    out.sort_by(|a, b| (a.method.as_str(), a.route.as_str())
        .cmp(&(b.method.as_str(), b.route.as_str())));

    Ok(out)
}

/// Reject filenames that aren't one of the Phase 1 method stems. Error
/// messages point the user at the fix (move file into a subdirectory,
/// use `index` instead of `get`, etc.).
///
/// `_404` is allowed at the root only in Phase 1 — per-subtree fallbacks
/// land in Phase 2+. `parent_rel` being empty means "at pages/ root."
fn validate_stem(stem: &str, parent_rel: &Path, path: &Path) -> Result<()> {
    match stem {
        "index" | "post" | "put" | "patch" | "delete" => Ok(()),
        "_404" => {
            if parent_rel.as_os_str().is_empty() {
                Ok(())
            } else {
                bail!(
                    "{}: per-subtree `_404` fallbacks land in Phase 2+. \
                     Phase 1 supports `pages/_404.<ext>` at the project root only.",
                    path.display()
                );
            }
        }
        "get" => bail!(
            "{}: 'get' is reserved — use 'index' for GET handlers",
            path.display()
        ),
        "head" | "options" => bail!(
            "{}: '{stem}' is auto-derived by the server (HEAD mirrors GET with no body; \
             OPTIONS returns Allow:); do not author a '{stem}.sql' file",
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
        "put" => "PUT",
        "patch" => "PATCH",
        "delete" => "DELETE",
        "_404" => "404",
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

/// A parsed directory segment in a `pages/` path.
///
/// - `Static(name)` is a literal URL segment.
/// - `Capture(name)` comes from a filesystem directory name wrapped in
///   brackets — `[id]` → `Capture("id")`. The router matches any URL
///   segment against it and threads the string value into
///   `req.path_params`.
///
/// `[` and `]` are reserved: a segment is either a valid capture
/// (`^\[ident\]$`) or must contain no brackets at all. Anything else
/// (unbalanced, nested, or inline brackets) is rejected by
/// `parse_segments`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Static(String),
    Capture(String),
}

/// Parse the directory components of `rel` (a path relative to `pages/`)
/// into typed segments. Errors on any malformed capture syntax so the
/// user gets immediate feedback instead of a runtime routing surprise.
pub fn parse_segments(rel: &Path) -> Result<Vec<Segment>> {
    let raw = path_segments(rel);
    raw.iter().map(|r| parse_segment(r)).collect()
}

fn parse_segment(raw: &str) -> Result<Segment> {
    let has_open = raw.contains('[');
    let has_close = raw.contains(']');

    if !has_open && !has_close {
        return Ok(Segment::Static(raw.to_string()));
    }

    // Any bracket at all forces the whole segment to be a single capture.
    // Inline, nested, or unbalanced brackets are rejected.
    if !(raw.starts_with('[') && raw.ends_with(']')) {
        bail!(
            "segment '{raw}' has inline or unbalanced brackets. \
             Use '[name]' as the whole segment to capture a URL piece, \
             or remove brackets from a literal directory name."
        );
    }
    // starts_with('[') && ends_with(']') with exactly one of each
    let inner = &raw[1..raw.len() - 1];
    if inner.contains('[') || inner.contains(']') {
        bail!(
            "segment '{raw}' has nested brackets. Only one capture per directory: '[name]'."
        );
    }
    validate_capture_name(inner).with_context(|| format!("invalid capture segment '{raw}'"))?;
    Ok(Segment::Capture(inner.to_string()))
}

/// A capture name goes into both the URL pattern (`:name`) and the SQL
/// handler function name (`$name`). Constrain to ASCII identifier
/// syntax so both stay clean: start with a letter or underscore; rest
/// letters / digits / underscore. Bound at 63 chars (Postgres identifier
/// limit minus some slack for the `pages__` prefix and stem suffix).
fn validate_capture_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("capture name is empty");
    }
    if name.len() > 63 {
        bail!("capture name '{name}' is {} chars; max 63", name.len());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        bail!(
            "capture name '{name}' must start with an ASCII letter or underscore; got '{first}'"
        );
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            bail!(
                "capture name '{name}' may only contain ASCII letters, digits, or underscore; \
                 found '{c}'"
            );
        }
    }
    Ok(())
}

/// Build the URL pattern from parsed segments. Captures emit `:name`
/// (router's match form); statics emit verbatim. Empty segments → `/`.
fn build_route(segments: &[Segment]) -> String {
    if segments.is_empty() {
        "/".to_string()
    } else {
        let parts: Vec<String> = segments
            .iter()
            .map(|s| match s {
                Segment::Static(n) => n.clone(),
                Segment::Capture(n) => format!(":{n}"),
            })
            .collect();
        format!("/{}", parts.join("/"))
    }
}

/// Build the fully-qualified SQL handler function name. Format:
/// `pgweb.pages__<seg>__<seg>__...__<stem>`. Capture segments emit
/// `$name` — `$` is a legal character in Postgres identifier bodies
/// and is visually distinct from literal directory names (users aren't
/// going to name a directory `$id`), so the handler name for
/// `pages/posts/[id]/index.sql` cleanly disambiguates from `pages/posts/id/index.sql`.
fn build_handler_name(segments: &[Segment], stem: &str) -> String {
    if segments.is_empty() {
        format!("pgweb.pages__{stem}")
    } else {
        let parts: Vec<String> = segments
            .iter()
            .map(|s| match s {
                Segment::Static(n) => n.clone(),
                Segment::Capture(n) => format!("${n}"),
            })
            .collect();
        format!("pgweb.pages__{}__{stem}", parts.join("__"))
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

    fn s(n: &str) -> Segment {
        Segment::Static(n.into())
    }
    fn c(n: &str) -> Segment {
        Segment::Capture(n.into())
    }

    // ---- pure helpers ----

    #[test]
    fn build_route_root() {
        assert_eq!(build_route(&[]), "/");
    }

    #[test]
    fn build_route_single_segment() {
        assert_eq!(build_route(&[s("todos")]), "/todos");
    }

    #[test]
    fn build_route_nested() {
        assert_eq!(build_route(&[s("todos"), s("toggle")]), "/todos/toggle");
    }

    #[test]
    fn build_route_with_capture_emits_colon_form() {
        assert_eq!(build_route(&[s("posts"), c("id")]), "/posts/:id");
    }

    #[test]
    fn build_route_with_multiple_captures() {
        assert_eq!(
            build_route(&[s("users"), c("user"), s("posts"), c("post")]),
            "/users/:user/posts/:post"
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
            build_handler_name(&[s("todos")], "index"),
            "pgweb.pages__todos__index"
        );
    }

    #[test]
    fn build_handler_name_nested_post() {
        assert_eq!(
            build_handler_name(&[s("todos")], "post"),
            "pgweb.pages__todos__post"
        );
    }

    #[test]
    fn build_handler_name_deeply_nested() {
        assert_eq!(
            build_handler_name(&[s("todos"), s("toggle")], "post"),
            "pgweb.pages__todos__toggle__post"
        );
    }

    #[test]
    fn build_handler_name_uses_dollar_for_captures() {
        // `[id]` on disk → `$id` in the PG function name. Keeps captures
        // visually distinct from literal directory names.
        assert_eq!(
            build_handler_name(&[s("posts"), c("id")], "index"),
            "pgweb.pages__posts__$id__index"
        );
    }

    #[test]
    fn build_handler_name_multiple_captures() {
        assert_eq!(
            build_handler_name(&[s("users"), c("user"), s("posts"), c("post")], "index"),
            "pgweb.pages__users__$user__posts__$post__index"
        );
    }

    // ---- parse_segment / parse_segments ----

    #[test]
    fn parse_segment_static() {
        assert_eq!(parse_segment("posts").unwrap(), s("posts"));
        assert_eq!(parse_segment("todo-detail").unwrap(), s("todo-detail"));
        assert_eq!(parse_segment("v2").unwrap(), s("v2"));
    }

    #[test]
    fn parse_segment_capture() {
        assert_eq!(parse_segment("[id]").unwrap(), c("id"));
        assert_eq!(parse_segment("[user_id]").unwrap(), c("user_id"));
        assert_eq!(parse_segment("[_internal]").unwrap(), c("_internal"));
    }

    #[test]
    fn parse_segment_rejects_inline_brackets() {
        for bad in &["foo[id]", "[id]bar", "prefix[id]suffix"] {
            let err = parse_segment(bad).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("inline or unbalanced"), "for {bad}: {msg}");
        }
    }

    #[test]
    fn parse_segment_rejects_unbalanced_brackets() {
        for bad in &["[id", "id]", "[foo][bar"] {
            assert!(
                parse_segment(bad).is_err(),
                "unbalanced should error: {bad}"
            );
        }
    }

    #[test]
    fn parse_segment_rejects_nested_brackets() {
        let err = parse_segment("[[id]]").unwrap_err();
        assert!(format!("{err:#}").contains("nested"));
    }

    #[test]
    fn parse_segment_rejects_empty_capture() {
        let err = parse_segment("[]").unwrap_err();
        assert!(format!("{err:#}").contains("empty"));
    }

    #[test]
    fn parse_segment_rejects_bad_capture_identifier() {
        for bad in &["[id-x]", "[123]", "[my.id]", "[id space]", "[café]"] {
            assert!(
                parse_segment(bad).is_err(),
                "bad capture identifier should error: {bad}"
            );
        }
    }

    #[test]
    fn parse_segment_rejects_too_long_capture() {
        let long = "a".repeat(64);
        assert!(parse_segment(&format!("[{long}]")).is_err());
    }

    #[test]
    fn parse_segments_mixes_static_and_capture() {
        let got = parse_segments(Path::new("posts/[id]/comments")).unwrap();
        assert_eq!(got, vec![s("posts"), c("id"), s("comments")]);
    }

    #[test]
    fn parse_segments_empty_path_is_empty_list() {
        assert_eq!(parse_segments(Path::new("")).unwrap(), Vec::<Segment>::new());
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
        assert_eq!(method_for_stem("put"), "PUT");
        assert_eq!(method_for_stem("patch"), "PATCH");
        assert_eq!(method_for_stem("delete"), "DELETE");
        assert_eq!(method_for_stem("_404"), "404");
    }

    // ---- validate_stem ----

    fn root() -> &'static Path {
        Path::new("")
    }

    fn nested() -> &'static Path {
        Path::new("todos")
    }

    #[test]
    fn validate_stem_accepts_index_and_post_anywhere() {
        let file = Path::new("pages/x/index.html");
        assert!(validate_stem("index", root(), file).is_ok());
        assert!(validate_stem("post", root(), file).is_ok());
        assert!(validate_stem("index", nested(), file).is_ok());
        assert!(validate_stem("post", nested(), file).is_ok());
    }

    #[test]
    fn validate_stem_accepts_404_at_root_only() {
        let file = Path::new("pages/_404.html");
        assert!(validate_stem("_404", root(), file).is_ok());

        let err = validate_stem("_404", nested(), file).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("per-subtree"), "msg: {msg}");
        assert!(msg.contains("Phase 2+"), "msg: {msg}");
    }

    #[test]
    fn validate_stem_rejects_get_with_hint() {
        let err = validate_stem("get", root(), Path::new("pages/get.html")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("'get' is reserved"), "msg: {msg}");
        assert!(msg.contains("index"), "msg: {msg}");
    }

    #[test]
    fn validate_stem_accepts_mutation_methods() {
        for stem in ["put", "patch", "delete"] {
            assert!(validate_stem(stem, root(), Path::new("pages/x/y.sql")).is_ok(), "stem {stem} should be accepted");
            assert!(validate_stem(stem, nested(), Path::new("pages/todos/x.sql")).is_ok());
        }
    }

    #[test]
    fn validate_stem_rejects_auto_only_methods() {
        for stem in ["head", "options"] {
            let err = validate_stem(stem, root(), Path::new("pages/x/y.sql")).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("auto-derived"), "for {stem}: {msg}");
        }
    }

    #[test]
    fn validate_stem_rejects_arbitrary_name_with_fix_hint() {
        let err =
            validate_stem("about", root(), Path::new("pages/about.html")).unwrap_err();
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
        // Real DELETE via dynamic capture + method stem (prompt 017-A): pages/todos/[id]/delete.sql → DELETE /todos/:id
        write(&pages, "todos/[id]/delete.sql", "delete handler");
        // v2 response contract demo routes (prompt 013 companion coverage)
        write(&pages, "status/index.sql", "json api via pgweb.json");
        write(&pages, "see-other/index.sql", "redirect via pgweb.redirect");
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
                ("DELETE", "/todos/:id", false, true),
                ("GET", "/", true, true),
                ("GET", "/see-other", false, true),
                ("GET", "/status", false, true),
                ("POST", "/todos", true, true),
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
    fn scan_rejects_auto_only_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "todos/head.sql", "HEAD ...");
        let err = scan(&pages).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("auto-derived"), "msg: {msg}");
    }

    #[test]
    fn scan_accepts_404_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "index.html", "<h1>home</h1>");
        write(&pages, "_404.html", "<h1>not found</h1>");
        let got = scan(&pages).unwrap();
        let fallback = got.iter().find(|e| e.method == "404").unwrap();
        assert_eq!(fallback.route, "/");
        assert_eq!(fallback.handler_name, "pgweb.pages___404");
        assert_eq!(fallback.template_path.as_deref(), Some("pages/_404.html"));
    }

    #[test]
    fn scan_rejects_404_in_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        write(&pages, "admin/_404.html", "<h1>nope</h1>");
        let err = scan(&pages).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("per-subtree"), "msg: {msg}");
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
