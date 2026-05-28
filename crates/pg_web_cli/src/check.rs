//! `pg-web check` — offline project validator.
//!
//! Every up-front check `pg-web push` does, but without a database
//! connection. Target use cases: pre-commit hook and CI gate — fast
//! feedback on layout errors, template parse errors, and SQL syntax
//! errors before a DB even has to be available.
//!
//! Deliberately out of scope for v0.1:
//! - **Semantic SQL validation** (column existence, type checks, RLS
//!   rules). Postgres has to do that; `check` catches the layer above.
//! - **PL/pgSQL function body parsing** beyond the outer `CREATE
//!   FUNCTION` wrapper. The `$$...$$` payload is opaque to our parser
//!   (sqlparser only matches the wrapper; the body is validated by
//!   Postgres at function-invocation time).
//! - **Return-type consistency** between a `.html` sibling and the
//!   handler signature. Valuable, but requires SQL-AST walking past
//!   the function wrapper; deferred.
//! - **Rich literals inside `COMMENT ON ...` statements**. We use a
//!   small tolerant statement splitter so that dollar-quoted strings
//!   (`$$...$$`, `$tag$...$tag$`) and adjacent string literals
//!   (`'foo' 'bar'`) are accepted in top-level `COMMENT ON TABLE /
//!   COLUMN / ...` statements. These are overwhelmingly used for
//!   high-quality, self-documenting schema comments and are fully
//!   supported by real PostgreSQL. All other statements remain under
//!   the strict sqlparser. (See `split_sql_statements` and
//!   `validate_sql_with_tolerant_comments`.)
//!
//! The offline SQL checks are intentionally approximate. They exist to
//! catch typos, unbalanced constructs, and obviously malformed DDL
//! before a DB is available. `pg-web migrate apply` (and `push`) against
//! a real Postgres instance remain the source of truth.
//!
//! Opt-in DB check: `--url <pg-connection>` adds a migration-ledger
//! drift comparison against `pgweb.migrations`. When supplied,
//! `check` does touch the DB — that's explicit, and the default
//! offline surface stays intact.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crate::db;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use walkdir::WalkDir;

use crate::{migrate, paths};

/// A top-level SQL statement as classified by our tolerant splitter.
/// The splitter understands dollar-quoted strings ($$ and $tag$), single-quoted
/// strings (including '' escapes), double-quoted identifiers, /* */ block comments,
/// and -- line comments so that statement boundaries (;) are only recognized
/// at the true top level.
#[derive(Debug, Clone)]
struct SqlStatement {
    /// The original text of this statement (including trailing semicolon if present).
    text: String,
    /// True if this statement is a top-level `COMMENT ON ...` (after trimming
    /// leading whitespace and stripping leading line/block comments).
    /// These are allowed to contain rich dollar-quoted and adjacent literals
    /// that sqlparser's Postgres dialect currently rejects.
    is_comment_on: bool,
}

/// Split `content` into top-level statements while correctly skipping over
/// all forms of string literals and comments that can contain semicolons,
/// dollar signs, or the word "COMMENT".
///
/// This is deliberately a small, zero-dependency scanner. It is *not* a full
/// SQL parser — its only job is to let us treat `COMMENT ON ...` statements
/// specially while still running the real sqlparser on everything else.
fn split_sql_statements(content: &str) -> Vec<SqlStatement> {
    let mut stmts = Vec::new();
    let mut current = String::new();

    let mut chars = content.char_indices().peekable();

    // State
    let mut in_single = false;
    let mut in_double = false;
    let mut in_block_comment = false;
    let mut dollar_tag: Option<String> = None;
    let mut in_line_comment = false;

    while let Some((i, c)) = chars.next() {
        // --- Line comment (--) until end of line ---
        if !in_single
            && !in_double
            && !in_block_comment
            && !in_line_comment
            && c == '-'
            && chars.peek().is_some_and(|&(_, next)| next == '-')
        {
            in_line_comment = true;
            current.push_str("--");
            // consume the second -
            if let Some((_, _)) = chars.next() {}
            continue;
        }
        if in_line_comment {
            current.push(c);
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }

        // --- Block comment /* ... */ ---
        if !in_single
            && !in_double
            && !in_block_comment
            && c == '/'
            && chars.peek().is_some_and(|&(_, next)| next == '*')
        {
            in_block_comment = true;
            current.push_str("/*");
            if let Some((_, _)) = chars.next() {}
            continue;
        }
        if in_block_comment {
            current.push(c);
            if c == '*' && chars.peek().is_some_and(|&(_, next)| next == '/') {
                current.push('/');
                if let Some((_, _)) = chars.next() {}
                in_block_comment = false;
            }
            continue;
        }

        // --- Dollar-quoted strings ($$ or $tag$) ---
        if !in_single && !in_double && !in_block_comment && c == '$' {
            if let Some(current_tag) = &dollar_tag {
                // We are inside a dollar-quoted region. Only look for its closer.
                if let Some((tag, len)) = detect_dollar_tag_at(&content[i..]) {
                    if tag == *current_tag {
                        current.push_str(&content[i..i + len]);
                        for _ in 0..(len - 1) {
                            chars.next();
                        }
                        dollar_tag = None;
                        continue;
                    }
                }
                // Otherwise this $ (or $something) is just literal content inside the region.
            } else {
                // Not inside a dollar region — try to open one.
                if let Some((tag, len)) = detect_dollar_tag_at(&content[i..]) {
                    dollar_tag = Some(tag);
                    current.push_str(&content[i..i + len]);
                    for _ in 0..(len - 1) {
                        chars.next();
                    }
                    continue;
                }
            }
        }

        if dollar_tag.is_some() {
            current.push(c);
            continue;
        }

        // --- Single-quoted strings (with '' escape) ---
        if c == '\'' && !in_double && !in_block_comment {
            current.push('\'');
            if in_single {
                // Look for escaped ''
                if chars.peek().is_some_and(|&(_, next)| next == '\'') {
                    current.push('\'');
                    chars.next();
                } else {
                    in_single = false;
                }
            } else {
                in_single = true;
            }
            continue;
        }

        // --- Double-quoted identifiers ---
        if c == '"' && !in_single && !in_block_comment {
            in_double = !in_double;
            current.push('"');
            continue;
        }

        if in_single || in_double {
            current.push(c);
            continue;
        }

        // --- Statement terminator at true top level ---
        if c == ';' {
            current.push(';');
            let stmt = finalize_statement(&current);
            if let Some(s) = stmt {
                stmts.push(s);
            }
            current.clear();
            continue;
        }

        current.push(c);
    }

    // Trailing content (files commonly omit the final semicolon)
    if !current.trim().is_empty() {
        if let Some(s) = finalize_statement(&current) {
            stmts.push(s);
        }
    }

    stmts
}

/// Detect a dollar quote opening/closing at the start of `s` (`$$` or `$tag$`).
/// Returns (tag_without_dollars, total_bytes_consumed_including_the_two_dollars).
fn detect_dollar_tag_at(s: &str) -> Option<(String, usize)> {
    if !s.starts_with('$') {
        return None;
    }
    // Find the next $ that closes the opening delimiter
    for (j, ch) in s.char_indices().skip(1) {
        if ch == '$' {
            let tag = s[1..j].to_string();
            return Some((tag, j + 1)); // length of $tag$
        }
    }
    None
}

/// Turn an accumulated statement buffer into a SqlStatement (or None if only whitespace).
fn finalize_statement(text: &str) -> Option<SqlStatement> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip leading line comments and block comments to decide if this is a COMMENT statement.
    let mut probe = trimmed;
    loop {
        let before = probe;
        probe = probe.trim_start();
        if probe.starts_with("--") {
            if let Some(nl) = probe.find('\n') {
                probe = &probe[nl + 1..];
                continue;
            } else {
                probe = "";
            }
        } else if probe.starts_with("/*") {
            if let Some(end) = probe.find("*/") {
                probe = &probe[end + 2..];
                continue;
            } else {
                probe = "";
            }
        } else {
            break;
        }
        if probe == before {
            break;
        }
    }
    probe = probe.trim_start();

    // If after stripping leading comments there's nothing left that looks like SQL,
    // this was just a trailing / standalone comment block — drop it.
    if probe.is_empty() {
        return None;
    }

    let lower = probe.to_ascii_lowercase();
    let is_comment_on = lower.starts_with("comment on ");

    Some(SqlStatement {
        text: text.to_string(),
        is_comment_on,
    })
}

/// Validate SQL content using the tolerant splitter + selective sqlparser.
///
/// - `COMMENT ON ...` statements are accepted as-is (they will be validated
///   by real Postgres on `migrate apply`). This lets developers use rich
///   dollar-quoted and adjacent-string documentation without the check
///   forcing them to degrade it.
/// - All other statements are passed through the real sqlparser so that
///   typos (`CRATE TABLE`), unbalanced constructs, etc. are still caught.
fn validate_sql_with_tolerant_comments(
    content: &str,
    dialect: &PostgreSqlDialect,
) -> Option<String> {
    let stmts = split_sql_statements(content);

    for stmt in stmts {
        if stmt.is_comment_on {
            // Rich documentation comments are explicitly allowed.
            // Real Postgres will reject truly malformed ones at apply time.
            continue;
        }
        if let Err(e) = Parser::parse_sql(dialect, &stmt.text) {
            return Some(format!("{e}"));
        }
    }
    None
}

/// Grouped findings. Callers decide how to format — tests inspect the
/// vectors directly; the CLI prints per-group.
#[derive(Debug, Default)]
pub struct CheckReport {
    pub layout: Vec<Finding>,
    pub templates: Vec<Finding>,
    pub sql: Vec<Finding>,
    pub migrations: Vec<Finding>,
    pub ledger: Vec<Finding>,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub path: PathBuf,
    pub message: String,
}

impl CheckReport {
    pub fn is_clean(&self) -> bool {
        self.total() == 0
    }

    pub fn total(&self) -> usize {
        self.layout.len()
            + self.templates.len()
            + self.sql.len()
            + self.migrations.len()
            + self.ledger.len()
    }
}

/// Run every check pass and return a grouped report. `url` is opt-in;
/// supplying it enables the ledger-drift pass which does require a DB.
pub fn check(app_dir: &Path, url: Option<&str>) -> Result<CheckReport> {
    let mut report = CheckReport::default();

    check_layout(app_dir, &mut report);
    check_templates(app_dir, &mut report)?;
    check_handler_sql(app_dir, &mut report)?;
    check_migration_sql(app_dir, &mut report)?;
    check_migration_order(app_dir, &mut report)?;

    if let Some(url) = url {
        check_ledger_drift(app_dir, url, &mut report)?;
    }

    Ok(report)
}

/// Run `paths::scan` which already enforces the layout spec
/// (directory-as-route, reserved stems, no flat HTML at root, no 404
/// in subdirs, etc.). Any error becomes one top-level finding on the
/// pages/ directory — callers re-run scan to get file-level detail if
/// needed.
fn check_layout(app_dir: &Path, report: &mut CheckReport) {
    let pages = app_dir.join("pages");
    if !pages.exists() {
        return;
    }
    if let Err(e) = paths::scan(&pages) {
        report.layout.push(Finding {
            path: pages,
            message: format!("{e:#}"),
        });
    }
}

/// Parse every `.html` under `pages/` through Tera. Mirrors push's
/// pre-transaction template validation so the check catches what push
/// would reject.
fn check_templates(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let pages = app_dir.join("pages");
    if !pages.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(&pages) {
        let entry = entry.with_context(|| format!("walking {}", pages.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;

        // Fresh Tera per file: we only care about parse, not cross-file
        // macros / extends. Avoids cross-file contamination of errors.
        let mut tera = tera::Tera::default();
        let name = path.to_string_lossy().to_string();
        if let Err(e) = tera.add_raw_template(&name, &content) {
            report.templates.push(Finding {
                path: path.to_path_buf(),
                message: format!("{e}"),
            });
        }
    }
    Ok(())
}

/// Parse every `.sql` under `pages/` through sqlparser.
///
/// Dollar-quoted function bodies are treated as opaque (only the wrapper
/// is validated). `COMMENT ON ...` statements are also treated specially:
/// they are allowed to contain dollar-quoted strings (`$$...$$`, `$tag$...$tag$`)
/// and adjacent string literals (`'foo' 'bar'`) that sqlparser's current
/// Postgres dialect rejects, even though real PostgreSQL accepts them.
/// These are overwhelmingly used for high-quality schema documentation.
/// Real syntax errors in any non-COMMENT statement are still reported.
fn check_handler_sql(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let pages = app_dir.join("pages");
    if !pages.exists() {
        return Ok(());
    }

    let dialect = PostgreSqlDialect {};

    for entry in WalkDir::new(&pages) {
        let entry = entry.with_context(|| format!("walking {}", pages.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;

        if let Some(err) = validate_sql_with_tolerant_comments(&content, &dialect) {
            report.sql.push(Finding {
                path: path.to_path_buf(),
                message: err,
            });
        }
    }
    Ok(())
}

/// Parse every `migrations/*.sql` through sqlparser.
///
/// See `check_handler_sql` for the special handling of dollar-quoted
/// and adjacent literals inside `COMMENT ON ...` statements.
fn check_migration_sql(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let migrations = app_dir.join("migrations");
    if !migrations.exists() {
        return Ok(());
    }

    let dialect = PostgreSqlDialect {};

    for entry in fs::read_dir(&migrations)
        .with_context(|| format!("reading {}", migrations.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;

        if let Some(err) = validate_sql_with_tolerant_comments(&content, &dialect) {
            report.migrations.push(Finding {
                path: path.clone(),
                message: err,
            });
        }
    }
    Ok(())
}

/// Validate migration-filename convention. Rules we enforce:
/// - Every migration's numeric prefix (before the first underscore) is
///   unique. Duplicates are the common pain: `0002_users.sql` and
///   `0002_posts.sql` land by accident, migrate runs them in filesystem
///   order, which may not match git-merge order.
///
/// Files without an underscore are passed through silently — they may
/// be `.gitkeep` or unconventional but not obviously wrong.
fn check_migration_order(app_dir: &Path, report: &mut CheckReport) -> Result<()> {
    let migrations_dir = app_dir.join("migrations");
    if !migrations_dir.exists() {
        return Ok(());
    }

    let files = migrate::discover(&migrations_dir)?;
    let mut seen_prefixes: HashMap<String, String> = HashMap::new();
    for (name, _) in &files {
        let prefix = match name.split_once('_') {
            Some((p, _)) => p,
            None => continue,
        };
        if let Some(first) = seen_prefixes.insert(prefix.to_string(), name.clone()) {
            report.migrations.push(Finding {
                path: migrations_dir.join(name),
                message: format!(
                    "duplicate migration prefix {prefix:?} — also used by {first:?}. \
                     Rename one; filesystem order is not guaranteed."
                ),
            });
        }
    }
    Ok(())
}

/// Opt-in: compare local `migrations/*.sql` against the `pgweb.migrations`
/// ledger. Reports a finding per divergence.
/// - Applied in DB but missing locally: someone deleted a migration after
///   it shipped. Diagnostic.
/// - Local file NOT applied in DB: normal pre-push state, flagged only
///   so CI can decide whether a migration was forgotten.
fn check_ledger_drift(app_dir: &Path, url: &str, report: &mut CheckReport) -> Result<()> {
    let migrations_dir = app_dir.join("migrations");
    let local: Vec<String> = if migrations_dir.exists() {
        migrate::discover(&migrations_dir)?
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    } else {
        Vec::new()
    };

    let mut client = db::connect(url, "check")?;
    let rows = client
        .query("SELECT name FROM pgweb.migrations", &[])
        .context("reading pgweb.migrations (is the extension installed?)")?;
    let applied: Vec<String> = rows.into_iter().map(|r| r.get::<_, String>(0)).collect();

    for name in &applied {
        if !local.contains(name) {
            report.ledger.push(Finding {
                path: migrations_dir.join(name),
                message: format!(
                    "{name:?} is in pgweb.migrations but missing from migrations/ — \
                     someone deleted a migration file after it was applied. The DB \
                     still carries its effects."
                ),
            });
        }
    }

    for name in &local {
        if !applied.contains(name) {
            report.ledger.push(Finding {
                path: migrations_dir.join(name),
                message: format!(
                    "{name:?} exists locally but is not in pgweb.migrations — \
                     run `pg-web migrate apply` to bring the DB up to date."
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    #[test]
    fn report_is_clean_when_no_findings() {
        let r = CheckReport::default();
        assert!(r.is_clean());
        assert_eq!(r.total(), 0);
    }

    #[test]
    fn report_counts_across_groups() {
        let mut r = CheckReport::default();
        r.layout.push(Finding {
            path: PathBuf::from("a"),
            message: "x".into(),
        });
        r.sql.push(Finding {
            path: PathBuf::from("b"),
            message: "y".into(),
        });
        assert!(!r.is_clean());
        assert_eq!(r.total(), 2);
    }

    #[test]
    fn check_flags_duplicate_migration_prefix() {
        let dir = tempdir().unwrap();
        write(dir.path(), "migrations/0001_users.sql", "SELECT 1;");
        write(dir.path(), "migrations/0001_posts.sql", "SELECT 2;");
        let report = check(dir.path(), None).unwrap();
        assert_eq!(report.migrations.len(), 1, "{:?}", report);
        assert!(report.migrations[0].message.contains("duplicate"));
        assert!(report.migrations[0].message.contains("0001"));
    }

    #[test]
    fn check_flags_bad_sql_in_migration() {
        let dir = tempdir().unwrap();
        write(dir.path(), "migrations/0001_bad.sql", "CRATE TABLE oops;");
        let report = check(dir.path(), None).unwrap();
        assert_eq!(report.migrations.len(), 1, "{:?}", report);
        assert!(report.migrations[0]
            .path
            .to_string_lossy()
            .contains("0001_bad.sql"));
    }

    #[test]
    fn check_flags_bad_tera_template() {
        let dir = tempdir().unwrap();
        // Valid minimal layout so check_layout passes.
        write(
            dir.path(),
            "pages/index.sql",
            "CREATE FUNCTION pgweb.pages__index(req json) RETURNS json AS $$ SELECT '{}'::json $$ LANGUAGE sql;",
        );
        write(
            dir.path(),
            "pages/index.html",
            "{% if x %}unclosed block",
        );
        let report = check(dir.path(), None).unwrap();
        assert_eq!(report.templates.len(), 1, "{:?}", report);
        assert!(report.templates[0]
            .path
            .to_string_lossy()
            .contains("index.html"));
    }

    #[test]
    fn check_accepts_clean_minimal_scaffold() {
        // What `pg-web init` produces should ALWAYS pass `pg-web check`
        // — otherwise the init scaffold is shipping broken defaults.
        let dir = tempdir().unwrap();
        crate::init::init(&dir.path().join("app"), "app", None).unwrap();
        let report = check(&dir.path().join("app"), None).unwrap();
        assert!(
            report.is_clean(),
            "minimal scaffold should be clean: {:#?}",
            report
        );
    }

    #[test]
    fn check_accepts_clean_todo_template() {
        // Same as above but for `--template todo`. If a future edit to
        // examples/todo introduces a parse error, this test catches it.
        let dir = tempdir().unwrap();
        crate::init::init(&dir.path().join("app"), "app", Some("todo")).unwrap();
        let report = check(&dir.path().join("app"), None).unwrap();
        assert!(
            report.is_clean(),
            "todo template should be clean: {:#?}",
            report
        );
    }

    #[test]
    fn check_passes_when_directories_are_missing() {
        // An app dir with no pages/ and no migrations/ should not panic
        // or error — it's just trivially clean.
        let dir = tempdir().unwrap();
        let report = check(dir.path(), None).unwrap();
        assert!(report.is_clean());
    }

    // ---------------------------------------------------------------------
    // Tests for the tolerant statement splitter + rich COMMENT handling
    // ---------------------------------------------------------------------

    #[test]
    fn split_sql_statements_handles_dollar_and_adjacent_literals() {
        let sql = r#"
CREATE TABLE carriers (id bigserial PRIMARY KEY, name text);

COMMENT ON TABLE carriers IS $$
Comprehensive carrier master table.

Supports:
- Multiple contact methods
- O'Brien Logistics
- Regional rate cards
$$;

COMMENT ON COLUMN carriers.name IS 'O''Reilly''s Preferred' ' Carrier';

ALTER TABLE carriers ADD COLUMN region text;
"#;

        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 4, "expected 4 top-level statements, got {}", stmts.len());

        assert!(!stmts[0].is_comment_on);
        assert!(stmts[1].is_comment_on, "dollar-quoted COMMENT should be recognized");
        assert!(stmts[2].is_comment_on, "adjacent-literal COMMENT should be recognized");
        assert!(!stmts[3].is_comment_on);
    }

    #[test]
    fn split_sql_statements_respects_quotes_and_comments() {
        // ; inside strings, dollar quotes, block comments, and line comments
        // must NOT terminate statements.
        let sql = r#"
CREATE FUNCTION f() RETURNS void AS $$
  -- this ; semicolon is inside a dollar quote
  SELECT 1;
$$ LANGUAGE sql;

COMMENT ON FUNCTION f IS $tag$multi-line
with ; semicolons and 'quotes' inside$tag$;

/* block comment containing ;
   CREATE TABLE fake ...; */
CREATE TABLE real (id int);

/* another */ -- line comment with ;
SELECT 42;   -- final real statement
"#;

        let stmts = split_sql_statements(sql);
        // We expect: CREATE FUNCTION, COMMENT, CREATE TABLE, SELECT
        assert_eq!(stmts.len(), 4, "got: {:#?}", stmts);
        assert!(stmts[0].text.contains("CREATE FUNCTION"));
        assert!(stmts[1].is_comment_on);
        assert!(stmts[2].text.contains("CREATE TABLE real"));
        assert!(stmts[3].text.contains("SELECT 42"));
    }

    #[test]
    fn check_accepts_rich_comment_literals_in_migrations() {
        // This is the exact pattern reported from real-world usage
        // (trucking-carriers). It must produce a completely clean report.
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "migrations/0001_carriers.sql",
            r#"
-- Real-world example that previously caused false-positive parse failures.
CREATE TABLE public.carriers (
    id   bigserial PRIMARY KEY,
    name text NOT NULL
);

COMMENT ON TABLE public.carriers IS $$
Comprehensive carrier master table.

Supports:
- Multiple contact methods
- O'Brien Logistics style names (apostrophes)
- Regional rate cards with "quotes"
$$;

COMMENT ON COLUMN public.carriers.name IS 'O''Reilly''s Preferred' ' Carrier';

ALTER TABLE public.carriers ADD COLUMN region text;
"#,
        );

        let report = check(dir.path(), None).unwrap();
        assert!(
            report.is_clean(),
            "rich dollar-quoted + adjacent-literal COMMENTs must be accepted: {:#?}",
            report
        );
    }

    #[test]
    fn check_still_flags_real_syntax_errors_outside_comments() {
        // A real typo in a non-COMMENT statement must still be reported.
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "migrations/0001_bad.sql",
            r#"
CREATE TABLE ok (id int);
CRATE TABLE oops;  -- typo here
COMMENT ON TABLE ok IS $$this is fine$$;
"#,
        );

        let report = check(dir.path(), None).unwrap();
        assert_eq!(report.migrations.len(), 1, "{:?}", report);
        assert!(report.migrations[0].message.to_lowercase().contains("crate"));
    }

    #[test]
    fn check_accepts_function_bodies_with_dollar_quotes() {
        // The CREATE FUNCTION wrapper must still be strictly validated,
        // but the body (including any COMMENT text inside it) is opaque.
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "migrations/0001_func.sql",
            r#"
CREATE OR REPLACE FUNCTION pgweb.pages__demo(req json)
RETURNS json AS $outer$
    -- Even a dollar-quoted string inside the body is fine.
    -- We use a *different* tag for the inner literal to avoid delimiter collision
    -- (standard practice when nesting dollar quotes).
    SELECT json_build_object('doc', $inner$inner docs with 'quotes'$inner$);
$outer$ LANGUAGE sql STABLE;

COMMENT ON FUNCTION pgweb.pages__demo IS $$handler for /demo$$;
"#,
        );

        let report = check(dir.path(), None).unwrap();
        assert!(report.is_clean(), "function body + rich comment must be clean: {:#?}", report);
    }
}
