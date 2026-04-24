//! `pg-web init <name>` — scaffold a new pg-web app directory.
//!
//! Two modes:
//! - **Minimal** (default) — a tiny hello-world app plus a `README.md`
//!   pointing at the docs and at the richer `--template todo` starting
//!   point. The long-standing default since M1.1.
//! - **Template** (`--template <name>`) — extract a full example tree
//!   bundled into the binary at compile time via `include_dir!`. Current
//!   set: `todo` (the HTMX todo list in `examples/todo/`). Adding a
//!   future template (e.g. `blog`) is one `include_dir!` + one match
//!   arm in `lookup_template`.
//!
//! The template README inside `examples/todo/README.md` is repo-facing
//! (references `../../target/debug/pg-web` etc.) and makes no sense at
//! an app's root. We skip that file during extraction and write the
//! app-facing `README_TODO` template instead.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use include_dir::{include_dir, Dir};

use crate::templates;

/// The HTMX todo-list demo — same directory `cargo test` + smoke tests
/// drive, baked into the binary so users without a local checkout of
/// the pg-web repo can scaffold it.
static TODO_TEMPLATE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../examples/todo");

/// Names of files inside a bundled template that `init --template` will
/// NOT copy verbatim. We re-generate an app-facing replacement for each.
/// Kept as a small const list so adding another exception is one line.
const TEMPLATE_SKIP: &[&str] = &["README.md"];

/// Scaffold a new pg-web app at `path` with `app_name` baked into the
/// generated templates. The directory must not already exist. When
/// `template` is `Some(name)`, extract the named bundled example; else
/// lay down the minimal hello-world scaffold.
pub fn init(path: &Path, app_name: &str, template: Option<&str>) -> Result<()> {
    if path.exists() {
        bail!("{} already exists — refusing to overwrite", path.display());
    }
    if app_name.is_empty() {
        bail!("app name must not be empty");
    }

    match template {
        Some(name) => init_from_template(path, app_name, name),
        None => init_minimal(path, app_name),
    }
}

/// Minimal hello-world scaffold: a single GET / route, the usual config
/// files, plus a README that points at the docs + mentions
/// `--template demo` for users who want more code to look at.
fn init_minimal(path: &Path, app_name: &str) -> Result<()> {
    fs::create_dir_all(path.join("pages"))
        .with_context(|| format!("creating {}", path.join("pages").display()))?;
    fs::create_dir_all(path.join("public"))
        .with_context(|| format!("creating {}", path.join("public").display()))?;
    fs::create_dir_all(path.join("migrations"))
        .with_context(|| format!("creating {}", path.join("migrations").display()))?;

    write(path, "pages/index.html", templates::INDEX_HTML)?;
    write(
        path,
        "pages/index.sql",
        &templates::render(templates::INDEX_SQL, app_name),
    )?;
    write(path, "pgweb.toml", templates::PGWEB_TOML)?;
    write(path, "docker-compose.yml", templates::DOCKER_COMPOSE)?;
    write(path, "Caddyfile", templates::CADDYFILE)?;
    write(path, ".gitignore", templates::GITIGNORE)?;
    write(
        path,
        "README.md",
        &templates::render(templates::README_MINIMAL, app_name),
    )?;
    write(path, "public/.gitkeep", "")?;
    write(path, "migrations/.gitkeep", "")?;

    Ok(())
}

/// Extract a named bundled template into `path`. Errors with the list
/// of available template names if `name` is unknown — keeps the user
/// in the loop without needing to consult external docs.
fn init_from_template(path: &Path, app_name: &str, name: &str) -> Result<()> {
    let bundle = lookup_template(name).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown template {name:?}. Available: {}",
            available_templates().join(", ")
        )
    })?;

    fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
    extract_dir(bundle, path)?;

    // Write the app-facing README after extraction so it replaces any
    // repo-facing one skipped by TEMPLATE_SKIP.
    let readme = match name {
        "todo" => templates::README_TODO,
        _ => templates::README_MINIMAL,
    };
    write(path, "README.md", &templates::render(readme, app_name))?;

    Ok(())
}

/// Walk an embedded `Dir` recursively and write each file to the real
/// filesystem under `dest`. Subdirectories are created as needed.
/// Files whose relative path matches `TEMPLATE_SKIP` are silently
/// skipped — we overwrite them ourselves afterwards.
fn extract_dir(src: &Dir<'_>, dest: &Path) -> Result<()> {
    for entry in src.entries() {
        let rel = entry.path();
        let abs = dest.join(rel);

        match entry {
            include_dir::DirEntry::Dir(d) => {
                fs::create_dir_all(&abs)
                    .with_context(|| format!("creating {}", abs.display()))?;
                extract_dir(d, dest)?;
            }
            include_dir::DirEntry::File(f) => {
                let rel_str = rel.to_string_lossy();
                if TEMPLATE_SKIP.iter().any(|s| rel_str == *s) {
                    continue;
                }
                if let Some(parent) = abs.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::write(&abs, f.contents())
                    .with_context(|| format!("writing {}", abs.display()))?;
            }
        }
    }
    Ok(())
}

/// Map a user-facing template name to its embedded directory. Returns
/// `None` for unknown names — caller formats the error.
fn lookup_template(name: &str) -> Option<&'static Dir<'static>> {
    match name {
        "todo" => Some(&TODO_TEMPLATE),
        _ => None,
    }
}

/// The list of template names `--template` accepts today. Exposed so
/// the CLI's error messages + future help text stay in one place.
pub fn available_templates() -> Vec<&'static str> {
    vec!["todo"]
}

/// Return the list of paths (relative to the project root) that plain
/// `init` creates. Tests use this to assert the minimal scaffold; the
/// `--template` path's tree is asserted separately in those tests.
pub fn scaffolded_paths() -> Vec<&'static str> {
    vec![
        "pages/index.html",
        "pages/index.sql",
        "pgweb.toml",
        "docker-compose.yml",
        "Caddyfile",
        ".gitignore",
        "README.md",
        "public/.gitkeep",
        "migrations/.gitkeep",
    ]
}

fn write(root: &Path, rel: &str, content: &str) -> Result<()> {
    let dest = PathBuf::from(root).join(rel);
    fs::write(&dest, content).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}
