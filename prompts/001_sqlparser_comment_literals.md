# 001 — Fix `pg-web check` SQL parser limitations on dollar-quoted and escaped literals in COMMENT statements

**Status:** Open improvement / future session prompt  
**Date opened:** 2026-05-27  
**Discovered in:** Real-world usage of pg-web by the trucking-carriers application (external project)

---

## Summary

`pg-web check` rejects perfectly valid PostgreSQL `COMMENT ON TABLE` / `COMMENT ON COLUMN` statements that use rich, production-quality documentation containing:

- Standard Postgres dollar-quoted string literals (`$$ ... $$` and tagged variants)
- Chained/adjacent single-quoted string literals
- Single-quoted strings containing apostrophes (properly escaped as `''` per SQL standard)

The same SQL passes `pg-web migrate apply` without any modification because migrations are executed directly against a real Postgres server.

## Detailed Description

### Where the check happens

Location: `crates/pg_web_cli/src/check.rs`

- `check_migration_sql()` (lines ~174–208) walks `migrations/*.sql`
- Every file is fed in full to `sqlparser::parser::Parser::parse_sql(&PostgreSqlDialect {}, &content)`
- Any parse error becomes a `Finding` in `report.migrations`
- A non-clean report causes `pg-web check` to exit non-zero (intended as a strict pre-commit / CI gate)

The handler-SQL path (`check_handler_sql`) uses the identical parser for `pages/**/*.sql`, though COMMENT statements are far more common (and valuable) inside migration files.

### Why sqlparser rejects the SQL

The `sqlparser` crate (pinned at `0.52` in `crates/pg_web_cli/Cargo.toml`) is a pure-Rust, zero-system-dependency parser. This was a deliberate architectural choice made in Session 4 / M1.4 E:

> "sqlparser is zero-system-deps. 'Good enough for catching typos + unbalanced parens + malformed DDL' is the explicit v0.1 bar. Upgrade path flagged if we ever need libpg_query-level strictness."

Its PostgreSQL dialect has good but incomplete coverage of the full Postgres literal grammar, particularly:

- Dollar-quoted strings in arbitrary expression / DDL contexts (especially inside `COMMENT ON ... IS $$...$$`)
- Adjacent string literal concatenation (`'foo' 'bar'`)
- Certain escape forms inside string literals when they appear in specific DDL commands

These are all first-class, documented PostgreSQL features and are heavily used by developers who want readable, multi-line, or apostrophe-containing documentation without ugly escaping.

### Real symptom (trucking-carriers)

Developers wrote high-quality migration files containing blocks such as:

```sql
COMMENT ON TABLE carriers IS $$
Comprehensive carrier master table.

Supports:
- Multiple contact methods
- O'Brien Logistics style names (apostrophes)
- Regional rate cards
$$;

COMMENT ON COLUMN carriers.name IS 'O''Reilly''s Preferred Carrier';
```

`pg-web check` reported parse errors on these files.  
`pg-web migrate apply` succeeded cleanly.  
No data or schema corruption occurred.

## Why This Matters

1. **Documentation is a first-class citizen.** Good `COMMENT ON` statements are the primary way teams keep schema knowledge alive inside the database itself (`\d+`, `information_schema`, pgAdmin, generated ERDs, etc.).

2. **The offline check is a core developer-experience promise.** One of the advertised benefits of `pg-web check` is "fast feedback before you even have a database." Forcing developers to degrade their comments to satisfy an approximate parser undermines that promise.

3. **Migrations are special.** Unlike ad-hoc SQL or handler functions, migrations are written once, reviewed, applied in order, and then become immutable history. They are the single best place in a pg-web app for rich, permanent documentation.

4. **The check is intentionally not semantic.** We already accept that `sqlparser` cannot validate function bodies, RLS policies, or complex expressions. Treating rich literals in COMMENTs the same way (as a known approximation gap) is consistent — but the current UX does not communicate the distinction.

## Proposed Solutions (brainstorm, in rough order of preference)

### 1. Better error messaging + explicit "parser limitation" category (lowest risk, high value)
- When `Parser::parse_sql` fails on a migration file, attempt a secondary lightweight scan for dollar-quote balance or known patterns.
- If the only errors appear inside `COMMENT ON ... IS` contexts, emit a distinct finding type or message:
  > "Migration 0012_rich_comments.sql: sqlparser rejected dollar-quoted literal (known limitation of the offline parser). The SQL is valid PostgreSQL. `migrate apply` will succeed. To silence this finding for this file, add `-- pg-web-check: lenient-comments`."
- Add a top-level note in check output: "Some findings may be offline-parser limitations rather than true syntax errors."
- Wire a `pgweb.toml` setting under `[check]` (e.g. `sql_parser = "lenient" | "strict"`).

This can ship quickly and immediately improves the experience without changing parsing behavior.

### 2. Statement-granular or comment-aware relaxation in the migration path
- Split migration content into top-level statements (respecting `$$` and `'` more carefully than a naive split).
- For statements that are purely `COMMENT ON ...`, use a tolerant literal extractor or skip full AST parsing.
- Keep the strict parser for all `CREATE`, `ALTER`, `INSERT`, `UPDATE`, etc. statements.
- This preserves the spirit of "catch real typos" while allowing the documentation use case that developers actually need.

### 3. Opt-in trusted / skip markers (pragmatic escape hatch)
- Recognize magic comments inside `.sql` files:
  - `-- pg-web-check: skip` (whole file)
  - `-- pg-web-check: trusted` (on the line before a statement)
- Useful not just for this issue but for any future parser gaps, generated SQL, or advanced Postgres syntax.

### 4. Hybrid / upgrade parser strategy (longer term)
- Keep sqlparser as the fast, zero-dep default.
- Behind a Cargo feature flag (or auto-detected when cmake is present), offer `pg_query` / `libpg_query` for users who want near-exact Postgres parsing.
- Document the tradeoff clearly in DEVELOPER-GUIDE and the check module docs (echoing the Session 4 rationale).
- Potentially contribute the missing dollar-quoted literal cases upstream to sqlparser-rs.

### 5. Two-mode behavior when `--url` is supplied
- When the user runs `pg-web check --url ...`, use the real database to validate migration SQL (via a `BEGIN; ... ROLLBACK;` or `pg_parse` equivalent) for the ledger-drift pass anyway.
- Pure-offline mode (no `--url`) remains the fast sqlparser path with clearer "approximate" labeling.

## Current Workaround (as used in trucking-carriers)

The project **kept the rich dollar-quoted and apostrophe-containing comments exactly as written**.

They:

- Continue to author high-quality, self-documenting migrations.
- Run `pg-web migrate apply` (always succeeds).
- Run `pg-web check` as part of their local / CI workflow.
- Mentally (and in code review) treat any "sqlparser parse error" findings that mention `COMMENT` or dollar quotes as **known false positives**.
- Do not simplify or remove documentation to make the tool happy.

This is acceptable for now because the check remains useful for catching real typos elsewhere, and the migration application path is the source of truth.

## Actionable Next Steps for a Future Session

- [ ] Add a regression-style test in `check.rs` that documents the current limitation (a migration containing a realistic `COMMENT ON ... IS $$...$$` block should either be accepted or produce a clearly-labeled "parser limitation" finding).
- [ ] Update the module-level docs at the top of `check.rs` (the big comment block) to call out this known gap alongside the existing notes about function bodies and return-type checking.
- [ ] Prototype improved error messaging (solution #1) — the highest-ROI change.
- [ ] Decide on and implement one primary mitigation (messaging + flag, or statement relaxation).
- [ ] Update `docs/ROADMAP.md` (add under Phase 1 polish or a new "Developer Experience / Tooling" subsection) and `docs/DEVELOPER-GUIDE.md` if new pitfalls arise.
- [ ] Ensure `examples/todo/migrations/` and the init templates continue to produce clean `pg-web check` results (they currently use very light comments).
- [ ] Consider whether the same leniency policy should apply to `pages/**/*.sql` handler files (probably yes for consistency, even if less common).

**Related files / history**
- `crates/pg_web_cli/src/check.rs` (core logic + tests)
- `crates/pg_web_cli/Cargo.toml` (sqlparser = "0.52")
- `docs/sessions/session_4.md` (Component E decision log — the original sqlparser choice)
- `docs/ROADMAP.md` (mentions of `pg-web check` and parser tradeoffs)
- `docs/DEVELOPER-GUIDE.md` (pitfalls section — good place for "known offline parser gaps")

**Priority:** Medium (DX, not correctness or data safety).  
**Risk of change:** Low-to-medium depending on chosen solution (messaging is very safe).

---

*This document is intentionally written as both a design record and a ready-to-use prompt for a future agent session. Feed it (plus the current state of `check.rs`) to the agent when the work is scheduled.*
