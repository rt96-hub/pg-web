# 003 — sqlparser in `pg-web check` rejects valid Postgres extension opclass syntax (gin_trgm_ops etc.)

**Status:** Ready for implementation  
**Priority:** Medium-High  
**Triggered by:** Sibling app `trucking-carriers` (the primary real-world consumer of the framework) adding its first extension-dependent DDL in `migrations/0002_add_fleet_size_ints_and_trgm_indexes.sql`

## Reproduction (exact, minimal)

From the `trucking-carriers` repo:

1. Run from the `trucking-carriers` directory:
   ```bash
   ../pg-web/target/debug/pg-web check
   ```

2. Observe the failure on the 0002 migration (which contains, among other things):
   ```sql
   CREATE EXTENSION IF NOT EXISTS pg_trgm;

   CREATE INDEX IF NOT EXISTS fmcsa_census_legal_name_trgm_idx
     ON public.fmcsa_census USING gin (legal_name gin_trgm_ops);

   CREATE INDEX IF NOT EXISTS fmcsa_census_dba_name_trgm_idx
     ON public.fmcsa_census USING gin (dba_name gin_trgm_ops);
   ```

**Observed error:**
```
Migrations:
  ./migrations/0002_add_fleet_size_ints_and_trgm_indexes.sql: sql parser error: Expected: ), found: gin_trgm_ops at Line: 8, Column: 48
✗ 1 finding(s) — fix and re-run
```

- `pg-web migrate apply` succeeds cleanly (on a Postgres that has run `CREATE EXTENSION pg_trgm`).
- The same SQL works in `psql`.
- Only the offline `pg-web check` command hard-fails.

This is the second time the same sibling app has hit a limitation in the offline SQL validator (see `prompts/001_sqlparser_comment_literals.md` for the previous rich `COMMENT ON` case).

---

## Root Cause (Confirmed by Deep Investigation)

The offline validator (`pg-web check`) performs **pure-Rust** SQL parsing using `sqlparser = "0.52"` + `PostgreSqlDialect`. It has a small, hand-written tolerant splitter whose only special case is `COMMENT ON ...` statements (added after the 001 incident to allow rich dollar-quoted documentation).

**Everything else** — including all `CREATE INDEX`, `CREATE EXTENSION`, `ALTER TABLE`, etc. — is passed verbatim to `Parser::parse_sql(dialect, &stmt.text)`.

`sqlparser`'s Postgres grammar does not understand the `opclass` syntax that appears after a column inside `USING gin (col opclass)` (or equivalent for GiST, SP-GiST, etc.). When it sees the bare identifier `gin_trgm_ops` instead of `)` or `,`, it emits the exact error above.

### Why only `check` is affected (important nuance)
- `migrate apply` does **zero** parsing — it simply does `batch_execute()` of the raw .sql files against a real Postgres connection (one transaction per file).
- `push` does **not** invoke the SQL validator for migrations or page handlers in its normal path (it only does Tera preflight on templates + a pending-migration gate + live execution of handler functions inside a transaction).
- The noisy hard failure therefore comes primarily from the standalone `pg-web check` command (the thing advertised as the pre-commit / CI gate).

This is a classic case of "our offline approximation is stricter than reality."

---

## Exact Code Map & Architecture (from deep dive)

**Primary file (everything lives here):**
- `crates/pg_web_cli/src/check.rs` (~836 lines)

Key pieces inside it:
- `split_sql_statements()` — robust zero-dependency char-by-char state machine that correctly tracks single/double quotes, block/line comments, and dollar-quoted strings (`$$` and `$tag$...$tag$`). This machinery was the main deliverable of the previous 001 fix from the same sibling app.
- `finalize_statement()` — trims leading comments and sets `is_comment_on = lower.starts_with("comment on ")`.
- `validate_sql_with_tolerant_comments()` — the decision point: `COMMENT ON` statements are accepted as-is; everything else must parse cleanly through sqlparser.
- `check_migration_sql()` and `check_handler_sql()` — the two call sites that feed content into the validator.
- `check()` — the public entry point that orchestrates layout, templates, SQL, migration order, and optional ledger drift.
- `CheckReport` / `Finding` + `print_check_report()` in `main.rs`.

**Other relevant locations:**
- `crates/pg_web_cli/Cargo.toml:61` — `sqlparser = "0.52"` (with comment explaining the deliberate zero-dependency choice and the rejected `pg_query` alternative).
- `crates/pg_web_cli/src/main.rs` — CLI wiring for the `check` subcommand.
- No other files in the CLI (or the extension) perform offline SQL parsing for user migrations.

**Test coverage status:**
- The splitter has good unit tests inside `check.rs` (dollar quotes, adjacent literals, comments containing `;`, rich `COMMENT ON` cases, etc.).
- There is **zero** coverage for `CREATE INDEX ... USING ...` with opclasses, `CREATE EXTENSION`, or any extension DDL patterns.
- The `examples/todo/migrations/0001_create_todos.sql` only contains a plain btree index — this is why the gap was never caught internally.

**Documentation references:**
- `docs/APP-DEVELOPER-GUIDE.md` positions `pg-web check` as the pre-commit/CI companion and describes migrations as the correct home for schema + indexes.
- The module docs at the top of `check.rs` already acknowledge that the validator is deliberately approximate and that real Postgres remains the source of truth.

---

## Why This Matters

Real applications following the documented "migrations own schema and indexes" rule will naturally need extension DDL (pg_trgm for search, pgvector, PostGIS, pgcrypto, timescaledb, etc.). The current "tolerant comments only" policy is too narrow and creates exactly the kind of friction the framework is trying to avoid.

This is the second time the primary consumer app has hit this class of problem.

---

## Recommended Fix Direction (Primary Recommendation)

**Broaden the existing tolerant mechanism** (the same pragmatic pattern that already solved the rich `COMMENT ON` case after prompt 001).

### Best insertion points (all inside `check.rs`)
- Extend `finalize_statement()` (or introduce a small helper) to also mark statements as "lenient/trusted" when they are top-level `CREATE EXTENSION ...`, `CREATE INDEX ...` (especially those containing `USING gin|gist|...`), and a small set of related extension DDL.
- In `validate_sql_with_tolerant_comments()`, skip strict parsing for lenient statements (at minimum for the migration path; handlers can stay stricter).
- Consider a light migration-vs-handler distinction so that `pages/**/*.sql` remains under tighter scrutiny while `migrations/` gets the pragmatic carve-out.
- Improve the error message for migration findings when a parse still fails.
- Add a regression test (a minimal `pg_trgm` or equivalent index in the todo example migrations, or a dedicated test case) + update `smoke-cli.sh`.
- Update `docs/APP-DEVELOPER-GUIDE.md` and the module docs in `check.rs` with the new policy and the recommended workflow: "If `check` complains about extension DDL but `migrate apply` succeeds, you are fine."

This approach reuses battle-tested infrastructure, keeps the spirit of the current design, has minimal risk, and directly solves the reported pain.

Alternative / complementary options (make migration findings advisory by default, improve messaging only, or pursue upstream grammar improvements in sqlparser-rs) are viable but less satisfying than making the common case "just work."

---

## What "Done" Looks Like

- `pg-web check` no longer emits hard errors on valid, real-Postgres-working extension DDL patterns that commonly appear in migrations (`CREATE INDEX ... USING ...` with opclasses, `CREATE EXTENSION`, etc.).
- Strict checking for actual typos and malformed core DDL is preserved (especially for handler SQL).
- Clear documentation of the (still approximate) policy.
- At least one regression test using a realistic extension index pattern.
- The handoff prompt + this investigation are no longer needed because the gap is closed.

---

## Reproduction & Investigation Notes for the Next Agent

You have full permission to read the sibling `trucking-carriers` repository while working on this. The canonical reproduction is the file `migrations/0002_add_fleet_size_ints_and_trgm_indexes.sql` in that repo (it was written following the exact guidance in `APP-DEVELOPER-GUIDE.md`).

Start by reading:
- This prompt in full.
- `crates/pg_web_cli/src/check.rs` (the entire file).
- The previous related prompt: `prompts/001_sqlparser_comment_literals.md`.

The subagent that produced the detailed code map in this document stayed strictly read-only and made 51 tool calls. Its findings are already folded into this prompt.

---

This is high-leverage polish. Fixing the narrowness of the offline validator will make `pg-web check` actually pleasant and trustworthy on real applications instead of something people learn to ignore.

Good luck.