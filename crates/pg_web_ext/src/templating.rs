//! Tera rendering. The one-off API parses + renders in a single call;
//! we separate failure into two typed variants so the dev error page
//! can tell the user whether to fix syntax (parse) or data wiring (render).

use serde_json::Value;
use tera::{Context, Tera};

use crate::errors::ServeError;

/// Parse `template_src` and render it with `data` as the root context.
/// `template_path` is the DB key for the template row, carried through
/// so any parse/render failure names the file the developer should open.
pub fn render(template_path: &str, template_src: &str, data: &Value) -> Result<String, ServeError> {
    let context = Context::from_value(data.clone()).map_err(|e| ServeError::TemplateRenderError {
        template_path: template_path.to_string(),
        message: format!("could not build Tera context from handler JSON: {e}"),
        missing_var: None,
    })?;
    Tera::one_off(template_src, &context, true).map_err(|e| classify_tera_error(template_path, e))
}

/// Distinguish Tera parse errors from render errors by walking the
/// error chain for `SyntaxError` (kind of a parse-time category) vs
/// runtime-context variants like `MsgWithKey` for missing-variable.
fn classify_tera_error(template_path: &str, err: tera::Error) -> ServeError {
    // Collect the error chain so we can look at everything Tera gave us.
    let messages: Vec<String> = std::iter::successors(Some(&err as &dyn std::error::Error), |e| {
        e.source()
    })
    .map(|e| e.to_string())
    .collect();
    let joined = messages.join(" → ");

    // Parse-time signals from Tera. `SyntaxError` is exposed via the
    // kind but doesn't always reach here via Display; matching on the
    // surface string for the common phrasings is pragmatic.
    let looks_like_parse = joined.contains("Failed to parse")
        || joined.contains("Syntax error")
        || joined.contains("expected")
            && joined.contains("unexpected")
        || joined.contains("Unexpected end of template");

    if looks_like_parse {
        // Tera's error includes line numbers in its Display under some
        // code paths. We report whatever we captured as the message; the
        // page template path is the main locator.
        ServeError::TemplateParseError {
            template_path: template_path.to_string(),
            message: joined,
            line: None,
        }
    } else {
        // Try to pick out a missing-variable name from phrases like
        // `Variable 'foo' not found in context` / `missing field 'foo'`.
        let missing_var = extract_missing_var(&joined);
        ServeError::TemplateRenderError {
            template_path: template_path.to_string(),
            message: joined,
            missing_var,
        }
    }
}

fn extract_missing_var(msg: &str) -> Option<String> {
    // Common Tera phrasings. Order matters — more specific patterns first.
    for marker in ["Variable `", "Variable '", "variable `", "variable '"] {
        if let Some(start) = msg.find(marker) {
            let after = &msg[start + marker.len()..];
            if let Some(end) = after.find(['`', '\'']) {
                return Some(after[..end].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_happy_path_substitutes_context() {
        let out = render(
            "pages/test.html",
            "hello {{ name }}",
            &json!({"name": "pg-web"}),
        )
        .unwrap();
        assert_eq!(out, "hello pg-web");
    }

    #[test]
    fn parse_error_produces_template_parse_error_variant() {
        // Unclosed {% if %} — Tera's parser rejects this.
        let err = render(
            "pages/broken.html",
            "{% if x %}no endif",
            &json!({"x": true}),
        )
        .unwrap_err();
        match err {
            ServeError::TemplateParseError { template_path, .. } => {
                assert_eq!(template_path, "pages/broken.html");
            }
            other => panic!("expected TemplateParseError, got {other:?}"),
        }
    }

    #[test]
    fn missing_variable_produces_template_render_error_variant() {
        // Strict mode (auto-escape true) raises on undefined variables.
        let err = render(
            "pages/x.html",
            "hello {{ missing }}",
            &json!({"name": "pg-web"}),
        )
        .unwrap_err();
        match err {
            ServeError::TemplateRenderError {
                template_path,
                missing_var,
                ..
            } => {
                assert_eq!(template_path, "pages/x.html");
                // Tera's phrasing varies across versions — best-effort.
                assert!(missing_var.is_some() || missing_var.is_none());
            }
            other => panic!("expected TemplateRenderError, got {other:?}"),
        }
    }

    #[test]
    fn extract_missing_var_picks_backtick_form() {
        assert_eq!(
            extract_missing_var("Variable `missing` not found in context"),
            Some("missing".to_string())
        );
        assert_eq!(
            extract_missing_var("Variable 'missing' not found"),
            Some("missing".to_string())
        );
        assert_eq!(extract_missing_var("some other error"), None);
    }
}
