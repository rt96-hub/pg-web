//! `pg-web init <name>` — scaffold a new pg-web app directory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::templates;

/// Scaffold a new pg-web app at `path` with `app_name` baked into the
/// generated templates. The directory must not already exist.
pub fn init(path: &Path, app_name: &str) -> Result<()> {
    if path.exists() {
        bail!("{} already exists — refusing to overwrite", path.display());
    }
    if app_name.is_empty() {
        bail!("app name must not be empty");
    }

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
    write(path, "public/.gitkeep", "")?;
    write(path, "migrations/.gitkeep", "")?;

    Ok(())
}

/// Return the list of paths (relative to the project root) that `init`
/// creates. Tests use this to assert structure; the CLI uses it to print
/// a tree summary.
pub fn scaffolded_paths() -> Vec<&'static str> {
    vec![
        "pages/index.html",
        "pages/index.sql",
        "pgweb.toml",
        "docker-compose.yml",
        "Caddyfile",
        ".gitignore",
        "public/.gitkeep",
        "migrations/.gitkeep",
    ]
}

fn write(root: &Path, rel: &str, content: &str) -> Result<()> {
    let dest = PathBuf::from(root).join(rel);
    fs::write(&dest, content).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}
