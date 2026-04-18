//! File-path → URL-pattern + SQL-identifier conversion.
//!
//! Centralized in its own module so path-mapping rules have one place to
//! live, one place to test. All functions are pure.
//!
//! Conventions (M1.1 — static routes only):
//! - `pages/index.html`           → route `/`           + handler `pgweb.pages__index`
//! - `pages/about.html`           → route `/about`      + handler `pgweb.pages__about`
//! - `pages/posts/index.html`     → route `/posts`      + handler `pgweb.pages__posts__index`
//! - `pages/posts/comments.html`  → route `/posts/comments` + handler `pgweb.pages__posts__comments`
//!
//! Dynamic `[id]` segments land in M1.2.

/// Convert a path relative to `pages/` into a URL route pattern.
///
/// Input is expected to use forward slashes. Strips the `.html` suffix.
pub fn route_for(rel_path: &str) -> String {
    let rel = rel_path.replace('\\', "/");
    let stem = rel.strip_suffix(".html").unwrap_or(&rel);

    if stem == "index" {
        return "/".to_string();
    }
    if let Some(parent) = stem.strip_suffix("/index") {
        return format!("/{parent}");
    }
    format!("/{stem}")
}

/// Convert a path relative to `pages/` into a fully-qualified SQL handler
/// function name under the `pgweb` schema.
///
/// Slashes become `__` because they're invalid in SQL identifiers and double
/// underscore is unambiguous (filesystem paths cannot contain `__` followed by
/// a slash in a way that collides).
pub fn handler_for(rel_path: &str) -> String {
    let rel = rel_path.replace('\\', "/");
    let stem = rel.strip_suffix(".html").unwrap_or(&rel);
    let normalized = stem.replace('/', "__");
    format!("pgweb.pages__{normalized}")
}

/// Template storage key for a given pages-relative path. Keeps the `pages/`
/// prefix to match what the extension reads via SPI.
pub fn template_path_for(rel_path: &str) -> String {
    format!("pages/{}", rel_path.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_root() {
        assert_eq!(route_for("index.html"), "/");
    }

    #[test]
    fn route_top_level() {
        assert_eq!(route_for("about.html"), "/about");
    }

    #[test]
    fn route_nested_index() {
        assert_eq!(route_for("posts/index.html"), "/posts");
    }

    #[test]
    fn route_nested_leaf() {
        assert_eq!(route_for("posts/comments.html"), "/posts/comments");
    }

    #[test]
    fn route_deeply_nested() {
        assert_eq!(route_for("a/b/c.html"), "/a/b/c");
    }

    #[test]
    fn route_handles_windows_backslashes() {
        assert_eq!(route_for("posts\\comments.html"), "/posts/comments");
    }

    #[test]
    fn handler_root() {
        assert_eq!(handler_for("index.html"), "pgweb.pages__index");
    }

    #[test]
    fn handler_nested() {
        assert_eq!(handler_for("posts/index.html"), "pgweb.pages__posts__index");
    }

    #[test]
    fn handler_deeply_nested() {
        assert_eq!(handler_for("a/b/c.html"), "pgweb.pages__a__b__c");
    }

    #[test]
    fn template_path_preserves_pages_prefix() {
        assert_eq!(template_path_for("index.html"), "pages/index.html");
        assert_eq!(template_path_for("posts/comments.html"), "pages/posts/comments.html");
    }
}
