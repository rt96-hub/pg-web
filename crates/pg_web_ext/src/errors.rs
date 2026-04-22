//! Typed catalog for every request-serving failure mode.
//!
//! # Adding a new error
//!
//! 1. Add a variant to [`ServeError`] with the fields the error page needs.
//! 2. Add one arm each to [`ServeError::code`], [`ServeError::title`], and
//!    [`ServeError::remedy`] (the compiler's exhaustiveness check forces
//!    you to; miss one and the crate won't build).
//! 3. Extend [`ServeError::render_dev_page`] with the variant-specific
//!    "Detail" fields you want the developer to see.
//!
//! The catalog is intentionally flat and explicit — no trait-object
//! dispatch, no error macros. Easier to read, easier to grep for a code
//! when it shows up in a bug report.
//!
//! # Codes
//!
//! Codes follow `PGWEB_E<nnn>_<SHORT_NAME>`. The number is stable; the
//! short name can be refined without breaking code-based searches. The
//! `E999_OTHER` escape hatch exists for anything we haven't classified —
//! every landing in `Other` is a hint that we should add a real variant
//! for it.

use serde_json::Value;

/// Every request-serving failure the framework knows how to talk about.
#[derive(Debug, Clone)]
pub enum ServeError {
    /// The route's `handler_name` doesn't resolve to a function in `pg_proc`.
    /// Usually a typo in the user's `.sql` that `pg-web push` also catches;
    /// surfacing it here covers the case where the route existed before push
    /// added the validator.
    HandlerMissing {
        handler_name: String,
        route: String,
    },
    /// Handler function exists but its signature isn't `(req json) RETURNS
    /// json|text`. Shouldn't usually reach runtime thanks to push's
    /// validator, but the catalog covers it.
    HandlerSignatureMismatch {
        handler_name: String,
        expected: String,
        actual: String,
    },
    /// A SQL exception raised inside the handler. All of PG's structured
    /// error fields go here so the dev page can show them verbatim.
    HandlerSqlException {
        handler_name: String,
        sqlstate: String,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
        context: Option<String>,
    },
    /// A full-mode route (template + handler) whose handler returned text
    /// that doesn't parse as JSON, so Tera can't accept it as context.
    HandlerReturnNotJson {
        handler_name: String,
        raw: String,
        parse_error: String,
    },
    /// Route references a `template_path` that doesn't exist in
    /// `pgweb.templates`. Usually a push-ordering bug.
    TemplateMissing {
        template_path: String,
    },
    /// Tera can't parse the template source. Syntax error, bad tag,
    /// mismatched block, etc.
    TemplateParseError {
        template_path: String,
        message: String,
        /// Line number from Tera's error when available.
        line: Option<u32>,
    },
    /// Tera parsed but choked on render — typically an undefined variable,
    /// unknown filter, or a field access against a non-object.
    TemplateRenderError {
        template_path: String,
        message: String,
        /// Name of the missing variable when the error was "variable X not
        /// found"; None for other render failures.
        missing_var: Option<String>,
    },
    /// A `pgweb.routes.path_pattern` that can't be parsed at match time.
    /// Should be impossible after Component C's scanner validation, but
    /// having the variant means a malformed row stored by hand produces
    /// a legible error instead of mysterious 404s.
    RoutePatternMalformed {
        pattern: String,
        reason: String,
    },
    /// Escape hatch for anything not yet classified. Every `Other` is a
    /// hint that we should extend the catalog.
    Other {
        message: String,
    },
}

impl ServeError {
    /// Stable machine-readable code. Numeric portion never changes once
    /// published; short name can be refined in place.
    pub fn code(&self) -> &'static str {
        match self {
            Self::HandlerMissing { .. } => "PGWEB_E001_HANDLER_MISSING",
            Self::HandlerSignatureMismatch { .. } => "PGWEB_E002_HANDLER_SIGNATURE",
            Self::HandlerSqlException { .. } => "PGWEB_E003_HANDLER_SQL_EXCEPTION",
            Self::HandlerReturnNotJson { .. } => "PGWEB_E004_HANDLER_RETURN_NOT_JSON",
            Self::TemplateMissing { .. } => "PGWEB_E005_TEMPLATE_MISSING",
            Self::TemplateParseError { .. } => "PGWEB_E006_TEMPLATE_PARSE",
            Self::TemplateRenderError { .. } => "PGWEB_E007_TEMPLATE_RENDER",
            Self::RoutePatternMalformed { .. } => "PGWEB_E008_ROUTE_PATTERN_MALFORMED",
            Self::Other { .. } => "PGWEB_E999_OTHER",
        }
    }

    /// Short human-readable title for the error banner.
    pub fn title(&self) -> &'static str {
        match self {
            Self::HandlerMissing { .. } => "Handler function not found",
            Self::HandlerSignatureMismatch { .. } => "Handler signature doesn't match",
            Self::HandlerSqlException { .. } => "SQL exception inside handler",
            Self::HandlerReturnNotJson { .. } => "Handler return isn't valid JSON",
            Self::TemplateMissing { .. } => "Template not in pgweb.templates",
            Self::TemplateParseError { .. } => "Template failed to parse",
            Self::TemplateRenderError { .. } => "Template failed to render",
            Self::RoutePatternMalformed { .. } => "Route pattern malformed",
            Self::Other { .. } => "Unclassified error",
        }
    }

    /// Suggested fix. Kept short — the goal is to hand the dev a starting
    /// point, not a tutorial.
    pub fn remedy(&self) -> &'static str {
        match self {
            Self::HandlerMissing { .. } => {
                "Run `pg-web push` again — push validates that every route's handler function \
                 exists before committing, and would surface this before it could reach runtime. \
                 If you just edited the .sql file, check that the CREATE FUNCTION name matches \
                 exactly what the router looked up (case-sensitive, double-underscore-separated)."
            }
            Self::HandlerSignatureMismatch { .. } => {
                "The handler must be `(req json) RETURNS json` for full-mode routes (`.html` + \
                 `.sql` pair) or `(req json) RETURNS text` for raw-text routes (`.sql` only). \
                 Fix the CREATE FUNCTION signature to match."
            }
            Self::HandlerSqlException { .. } => {
                "A SQL statement inside your handler raised — the SQLSTATE and message below are \
                 Postgres's own. Common causes: constraint violations (wrap in a PL/pgSQL \
                 EXCEPTION block if you want to render them specially), missing tables (did you \
                 run `pg-web migrate apply`?), NULL where a cast expected a value."
            }
            Self::HandlerReturnNotJson { .. } => {
                "Full-mode routes (`.html` + `.sql`) must return JSON so Tera can render the \
                 template. Wrap your SELECT in `json_build_object(...)` or return via \
                 `to_jsonb(...)`, or delete the `.html` sibling to switch this route to \
                 raw-text mode."
            }
            Self::TemplateMissing { .. } => {
                "The route points at a template_path that isn't in `pgweb.templates`. Re-run \
                 `pg-web push` to re-sync templates from the filesystem."
            }
            Self::TemplateParseError { .. } => {
                "Tera couldn't parse the template. Common causes: unclosed `{% if %}` / `{% for %}` \
                 blocks, mismatched `{{` / `}}`, unknown tag names. `pg-web push` validates \
                 templates at push time (Session 3 Component D) — if this error reached runtime, \
                 the extension was running code pushed before validation landed, or something \
                 bypassed push."
            }
            Self::TemplateRenderError { .. } => {
                "Tera rendered the template but tripped on a variable or filter. If the detail \
                 below says 'variable X not found', your handler's returned JSON is missing a \
                 field the template references. Cross-check the `{{ ... }}` placeholders against \
                 the `json_build_object` (or equivalent) in your handler."
            }
            Self::RoutePatternMalformed { .. } => {
                "A `pgweb.routes.path_pattern` doesn't parse under the dynamic-route syntax \
                 (`:name` captures only). Re-running `pg-web push` from a clean checkout \
                 normally fixes this. If you edited `pgweb.routes` by hand, revert the bad row."
            }
            Self::Other { .. } => {
                "This error doesn't have a typed catalog entry yet. If you're seeing it often, \
                 consider opening an issue with the request context so we can classify it."
            }
        }
    }

    /// Short log line: `code: one-liner with identifying context`.
    pub fn log_line(&self) -> String {
        match self {
            Self::HandlerMissing {
                handler_name,
                route,
            } => format!("{}: {handler_name} (route {route})", self.code()),
            Self::HandlerSignatureMismatch {
                handler_name,
                expected,
                actual,
            } => format!(
                "{}: {handler_name} has ({actual}); expected ({expected})",
                self.code()
            ),
            Self::HandlerSqlException {
                handler_name,
                sqlstate,
                message,
                ..
            } => format!(
                "{}: in {handler_name} — SQLSTATE {sqlstate}: {message}",
                self.code()
            ),
            Self::HandlerReturnNotJson {
                handler_name,
                parse_error,
                ..
            } => format!("{}: {handler_name} — {parse_error}", self.code()),
            Self::TemplateMissing { template_path } => {
                format!("{}: {template_path}", self.code())
            }
            Self::TemplateParseError {
                template_path,
                message,
                ..
            } => format!("{}: {template_path} — {message}", self.code()),
            Self::TemplateRenderError {
                template_path,
                message,
                ..
            } => format!("{}: {template_path} — {message}", self.code()),
            Self::RoutePatternMalformed { pattern, reason } => {
                format!("{}: '{pattern}' — {reason}", self.code())
            }
            Self::Other { message } => format!("{}: {message}", self.code()),
        }
    }

    /// Build an HTML dev-mode error page. Hand-rendered (no Tera dependency)
    /// so a broken template can't crash the renderer recursively. All
    /// variant-specific fields are emitted through `escape_html` so user
    /// content is safe to embed.
    pub fn render_dev_page(&self, req: &Value, status: u16) -> String {
        let code = self.code();
        let title = self.title();
        let remedy = self.remedy();
        let context_rows = self.context_rows();
        let detail_block = self.detail_block();
        let req_pretty =
            serde_json::to_string_pretty(req).unwrap_or_else(|_| "<unavailable>".into());

        let context_html = if context_rows.is_empty() {
            String::new()
        } else {
            let mut s = String::from("<section><h2>Context</h2><dl>");
            for (k, v) in &context_rows {
                s.push_str(&format!(
                    "<dt>{}</dt><dd>{}</dd>",
                    escape_html(k),
                    escape_html(v)
                ));
            }
            s.push_str("</dl></section>");
            s
        };
        let detail_html = if detail_block.is_empty() {
            String::new()
        } else {
            format!(
                "<section><h2>Detail</h2><pre>{}</pre></section>",
                escape_html(&detail_block)
            )
        };

        format!(
            r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>pg-web · {code} · {status_title}</title>
<style>
* {{ box-sizing: border-box; }}
body {{ font-family: system-ui, -apple-system, sans-serif; margin: 0; background: #1a1a1a; color: #e6e6e6; line-height: 1.4; }}
header {{ background: #c62828; color: #fff; padding: 1rem 1.5rem; }}
header h1 {{ margin: 0 0 0.15rem 0; font-size: 1.05rem; font-weight: 600; }}
header .meta {{ font-family: ui-monospace, monospace; font-size: 0.82rem; opacity: 0.92; }}
main {{ padding: 1.5rem; max-width: 960px; margin: 0 auto; }}
section {{ background: #252525; border-radius: 4px; padding: 0.9rem 1.1rem; margin-bottom: 0.9rem; }}
section h2 {{ margin: 0 0 0.5rem 0; font-size: 0.78rem; letter-spacing: 0.05em; text-transform: uppercase; color: #9e9e9e; font-weight: 600; }}
dl {{ margin: 0; }}
dl dt {{ font-family: ui-monospace, monospace; color: #9ec5ff; font-size: 0.82rem; margin-top: 0.3rem; }}
dl dt:first-child {{ margin-top: 0; }}
dl dd {{ margin: 0.1rem 0 0.1rem 0; font-family: ui-monospace, monospace; font-size: 0.88rem; white-space: pre-wrap; word-break: break-word; }}
pre {{ background: #111; color: #ddd; padding: 0.85rem 1rem; border-radius: 3px; overflow-x: auto; font-size: 0.82rem; margin: 0; white-space: pre-wrap; word-break: break-word; }}
.remedy {{ background: #1e3a1e; border-left: 3px solid #66bb6a; }}
.remedy h2 {{ color: #aed581; }}
.remedy p {{ margin: 0; }}
.footnote {{ color: #777; font-size: 0.78rem; padding: 0 1.5rem 1.5rem; max-width: 960px; margin: 0 auto; }}
</style>
</head>
<body>
<header>
<h1>{title}</h1>
<div class="meta">{code} · HTTP {status}</div>
</header>
<main>
{context_html}
{detail_html}
<section class="remedy">
<h2>How to fix</h2>
<p>{remedy}</p>
</section>
<section>
<h2>Request (req)</h2>
<pre>{req_pretty}</pre>
</section>
</main>
<p class="footnote">Shown because <code>pgweb.settings.env</code> is <code>development</code>. Flip <code>[server] env = "production"</code> in <code>pgweb.toml</code> and re-run <code>pg-web push</code> to serve a generic 500 here instead.</p>
</body>
</html>"#,
            code = escape_html(code),
            title = escape_html(title),
            status = status,
            status_title = status,
            context_html = context_html,
            detail_html = detail_html,
            remedy = escape_html(remedy),
            req_pretty = escape_html(&req_pretty),
        )
    }

    fn context_rows(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::HandlerMissing {
                handler_name,
                route,
            } => vec![
                ("route", route.clone()),
                ("expected handler", handler_name.clone()),
            ],
            Self::HandlerSignatureMismatch {
                handler_name,
                expected,
                actual,
            } => vec![
                ("handler", handler_name.clone()),
                ("expected signature", expected.clone()),
                ("actual signature", actual.clone()),
            ],
            Self::HandlerSqlException {
                handler_name,
                sqlstate,
                ..
            } => vec![
                ("handler", handler_name.clone()),
                ("SQLSTATE", sqlstate.clone()),
            ],
            Self::HandlerReturnNotJson { handler_name, .. } => {
                vec![("handler", handler_name.clone())]
            }
            Self::TemplateMissing { template_path } => {
                vec![("template_path", template_path.clone())]
            }
            Self::TemplateParseError {
                template_path,
                line,
                ..
            } => {
                let mut v = vec![("template_path", template_path.clone())];
                if let Some(l) = line {
                    v.push(("line", l.to_string()));
                }
                v
            }
            Self::TemplateRenderError {
                template_path,
                missing_var,
                ..
            } => {
                let mut v = vec![("template_path", template_path.clone())];
                if let Some(var) = missing_var {
                    v.push(("missing variable", var.clone()));
                }
                v
            }
            Self::RoutePatternMalformed { pattern, .. } => {
                vec![("pattern", pattern.clone())]
            }
            Self::Other { .. } => Vec::new(),
        }
    }

    fn detail_block(&self) -> String {
        match self {
            Self::HandlerMissing { .. } | Self::HandlerSignatureMismatch { .. } => String::new(),
            Self::HandlerSqlException {
                message,
                detail,
                hint,
                context,
                ..
            } => {
                let mut s = format!("MESSAGE: {message}\n");
                if let Some(d) = detail {
                    s.push_str(&format!("DETAIL:  {d}\n"));
                }
                if let Some(h) = hint {
                    s.push_str(&format!("HINT:    {h}\n"));
                }
                if let Some(c) = context {
                    s.push_str(&format!("CONTEXT: {c}\n"));
                }
                s
            }
            Self::HandlerReturnNotJson {
                raw, parse_error, ..
            } => format!("parse error: {parse_error}\n\nreturned bytes:\n{raw}"),
            Self::TemplateMissing { .. } => String::new(),
            Self::TemplateParseError { message, .. } => message.clone(),
            Self::TemplateRenderError { message, .. } => message.clone(),
            Self::RoutePatternMalformed { reason, .. } => reason.clone(),
            Self::Other { message } => message.clone(),
        }
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn any_req() -> Value {
        json!({"body": {}, "query": {}, "method": "GET", "path": "/x", "path_params": {}})
    }

    #[test]
    fn code_is_stable_per_variant() {
        assert_eq!(
            ServeError::HandlerMissing {
                handler_name: "x".into(),
                route: "/x".into()
            }
            .code(),
            "PGWEB_E001_HANDLER_MISSING"
        );
        assert_eq!(
            ServeError::TemplateParseError {
                template_path: "pages/index.html".into(),
                message: "unclosed {% if %}".into(),
                line: Some(4),
            }
            .code(),
            "PGWEB_E006_TEMPLATE_PARSE"
        );
        assert_eq!(
            ServeError::Other {
                message: "?".into()
            }
            .code(),
            "PGWEB_E999_OTHER"
        );
    }

    #[test]
    fn dev_page_contains_code_title_and_remedy() {
        let e = ServeError::TemplateParseError {
            template_path: "pages/index.html".into(),
            message: "unclosed {% if %} at line 4".into(),
            line: Some(4),
        };
        let html = e.render_dev_page(&any_req(), 500);
        assert!(html.contains("PGWEB_E006_TEMPLATE_PARSE"));
        assert!(html.contains("Template failed to parse"));
        assert!(html.contains("How to fix"));
        assert!(html.contains("pages/index.html"));
        assert!(html.contains("unclosed {% if %}") || html.contains("unclosed"));
    }

    #[test]
    fn dev_page_escapes_user_content() {
        let e = ServeError::HandlerReturnNotJson {
            handler_name: "pgweb.pages__x".into(),
            raw: "<script>alert(1)</script>".into(),
            parse_error: "expected object".into(),
        };
        let html = e.render_dev_page(&any_req(), 500);
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn dev_page_renders_sql_exception_fields() {
        let e = ServeError::HandlerSqlException {
            handler_name: "pgweb.pages__todos__post".into(),
            sqlstate: "23514".into(),
            message: "new row violates check constraint \"todos_title_check\"".into(),
            detail: Some("Failing row contains (1, , f)".into()),
            hint: Some("Use a non-empty title.".into()),
            context: None,
        };
        let html = e.render_dev_page(&any_req(), 500);
        assert!(html.contains("SQLSTATE"));
        assert!(html.contains("23514"));
        assert!(html.contains("check constraint"));
        assert!(html.contains("DETAIL"));
        assert!(html.contains("HINT"));
    }

    #[test]
    fn log_line_includes_code() {
        let e = ServeError::HandlerMissing {
            handler_name: "pgweb.pages__missing".into(),
            route: "/missing".into(),
        };
        let line = e.log_line();
        assert!(line.starts_with("PGWEB_E001"));
        assert!(line.contains("pgweb.pages__missing"));
        assert!(line.contains("/missing"));
    }

    #[test]
    fn escape_html_handles_all_five() {
        assert_eq!(escape_html("& < > \" '"), "&amp; &lt; &gt; &quot; &#39;");
    }
}
