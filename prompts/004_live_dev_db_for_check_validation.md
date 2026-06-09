# 004 — Leverage running local dev Postgres (from `pg-web up`) for deep, semantic SQL validation in `pg-web check`

**Status:** High-value Phase 1 polish / v0.2 candidate  
**Priority:** High (directly addresses real developer friction discovered through adversarial testing)  
**Context:** Follow-up to prompts 001 and 003. Triggered by extended conversation about the fundamental weakness of the current offline-only `pg-web check`.

## The Core Problem (in the user's own words, lightly edited)

> "so we literally are only checking this to see if we are properly creating a function, not making sure the function even works (the important part?)"
>
> [User pastes a real handler]
>
> ```sql
> CREATE OR REPLACE FUNCTION pgweb.pages__index(req json) RETURNS json AS $$
>   SELECT json_build_object(
>     'total_carriers', to_char(COUNT(*), 'FM9,999,999,999')
>   )
>   FROM public.fmcsa_census;
> $$ LANGUAGE sql STABLE;
> ```
>
> They deliberately inserted random garbage, broken keywords, references to non-existent tables/columns/functions, etc. into both migration files **and** handler `.sql` files.
>
> `pg-web check` reported **zero findings**.
>
> The user correctly observed:
> - The current `sqlparser`-based checker only does a shallow parse of the outer `CREATE FUNCTION` wrapper (or top-level DDL).
> - Anything inside `$$ ... $$` is opaque.
> - No semantic validation (does this table exist? does this column exist? is this function call valid? would this query plan?) ever occurs.
> - Real validation only happens later at `pg-web push` time (or at runtime).

This is not a bug in the implementation of prompts 001/003 — it is the **fundamental limitation** of the "pure offline, zero-dependency" design chosen in Session 4 / M1.4.

## Current State (Deep Code Map)

### 1. `pg-web check` (offline path) — `crates/pg_web_cli/src/check.rs`
- `check_handler_sql()` and `check_migration_sql()` both call the shared `validate_sql_with_tolerant_comments()`.
- That function uses a hand-written `split_sql_statements()` state machine (excellent for dollar quotes, comments, strings) + selective calls to `sqlparser::Parser::parse_sql(&PostgreSqlDialect {}, ...)`.
- After prompt 003, certain statements are marked `is_trusted` and completely bypass the parser:
  - `COMMENT ON ...`
  - `CREATE EXTENSION ...`
  - `CREATE [UNIQUE] INDEX ...` (including opclass syntax)
- Even for non-trusted statements, only the **top-level statement text** is passed to sqlparser. The body inside any `$$ ... $$` is never extracted or analyzed.
- When `--url` is supplied, the *only* additional work is `check_ledger_drift()` (pure presence comparison against `pgweb.migrations`). No SQL re-validation against the live catalog occurs.
- Result: `pg-web check` is excellent at catching typos in the *wrapper* and some structural mistakes. It is intentionally weak on everything that matters inside real application logic.

### 2. Real validation already exists in two places (strong precedent)
- **`pg-web push`** (`push.rs`):
  - Executes the raw handler `.sql` via `tx.batch_execute(&sql)` inside a real transaction.
  - Then calls `validate_handler()` which introspects `pg_proc` / `pg_get_function_arguments` etc. to prove the expected `pgweb.pages__*` function was actually created with the right signature.
  - Any error → full rollback. This is the "source of truth" moment.
- **`pg-web dev`** (`dev.rs:387`):
  - Already does a **shift-left preflight** for changed handler SQL files:
    ```rust
    fn preflight_sql(client: &mut Client, sql: &str) -> Result<()> {
        let mut tx = client.transaction()?;
        tx.batch_execute(sql)?;
        tx.rollback()?;
        Ok(())
    }
    ```
  - This is executed **before** the real push. A syntax or planning error from Postgres itself surfaces immediately, and the live routes are left untouched.
  - This is the exact pattern the user is asking to generalize into `pg-web check`.

### 3. Local dev environment
- `pg-web up` (`stack.rs`) runs the official `pgweb/postgres:latest` image.
- Postgres is exposed on `localhost:5432`.
- Defaults (from `templates.rs`): user `postgres`, password `devpassword`, database `app`.
- The same `DATABASE_URL` is used by `push`, `dev`, `migrate apply`, and (optionally) `check --url`.
- `pg-web dev` already connects to this URL for its per-save preflight.

### 4. Explicit design decisions that created this situation
- From `check.rs` module docs and `Cargo.toml`:
  > "sqlparser is zero-system-deps. 'Good enough for catching typos + unbalanced parens + malformed DDL' is the explicit v0.1 bar."
- `pg_query` (the Rust binding to the real Postgres parser via `libpg_query`) was explicitly rejected for the default path because it requires a C toolchain + longer builds.
- Deep semantic validation was declared out of scope for the offline `check` command.

## Why This Matters

For a framework whose entire value proposition is "you write SQL, we get out of the way," the current `pg-web check` gives developers a dangerously false sense of safety on the most important files in the project (the handler SQL that actually implements business logic).

Developers who follow the documented workflow (`pg-web up` → edit → `pg-web dev` or manual push) already have a running, schema-aware Postgres instance 95% of the time during local development. It is extremely frustrating that the advertised "pre-commit / CI gate" command ignores that perfectly good source of truth.

The gap between "check passes" and "push will succeed / the route will work" is currently much larger than the documentation and UX imply.

## Research: How Can We Do Better When a Live DB Is Available?

### Proven Safe Validation Techniques (Postgres)

When we have a connection, we can do far more than sqlparser without mutating state:

1. **Classic transactional execution (already used in dev preflight)**
   - `BEGIN; <paste entire file content>; ROLLBACK;`
   - Catches syntax errors, planning errors, type errors, missing objects, etc.
   - Used successfully today in `dev.rs`.

2. **Safer "DO block with early RETURN" pattern** (often preferred over raw BEGIN/ROLLBACK for complex scripts)
   ```sql
   DO $v$ BEGIN RETURN;
      -- paste the entire migration or handler SQL here
   END $v$ LANGUAGE plpgsql;
   ```
   - The `RETURN` makes everything after it unreachable.
   - Still exercises the full parser + analyzer + planner against the live catalog.
   - Avoids some (but not all) side-effect risks of raw execution.

3. **PREPARE + DEALLOCATE**
   - Excellent for individual DML/SELECT statements.
   - Does full semantic analysis + planning without executing.

4. **EXPLAIN (without ANALYZE)**
   - `EXPLAIN (VERBOSE, COSTS OFF) <query>;`
   - Very strong for SELECT/INSERT/UPDATE/DELETE.

5. **Catalog introspection**
   - After a successful transactional execution (or as a complement), query `pg_catalog` / `information_schema` / `pg_depend` to do symbol resolution, unused column detection, etc.

6. **plpgsql_check extension** (optional but powerful)
   - If installed in the dev DB: `SELECT * FROM plpgsql_check_function('pgweb.pages__foo(req json)');`
   - Does static analysis of PL/pgSQL bodies far beyond what normal `CREATE FUNCTION` does.

### Hybrid / Two-Tier Models Used by Real Tools

- Many modern tools (Atlas, Bytebase, sql-lint in connected mode, Prisma with introspection, etc.) do **cheap static first**, then **rich live validation** when a dev DB or shadow DB is available.
- `pg-web dev` already demonstrates the hybrid: cheap hash + watcher → expensive live preflight (only on changed SQL) → full push.

### The `pg_query` / libpg_query Path (for the offline case)

- Gives the *actual* Postgres parser as a library (parse tree, not just "does it parse?").
- Can be used for much deeper static analysis (symbol collection, even limited type checking if you feed it schema metadata).
- Cost: C toolchain, larger binaries, build time, version pinning to a specific Postgres major.

## Open Design Questions (the prompt should explore all of these deeply)

1. **Detection strategy**
   - Should `--url` automatically upgrade `check` into "live validation mode"?
   - Should we auto-detect a local dev stack (e.g., by trying to connect to the default dev URL, or by looking for a running `pgweb/postgres` container)?
   - New explicit flag (`--live`, `--deep`, `--use-db`)?

2. **Scope**
   - Apply live validation to handler SQL only? Or also to migrations?
   - For migrations: do we need to apply pending migrations first (inside the validation tx)? In what order?
   - Should we validate *all* migrations on disk, or only the ones that would be considered "new"?

3. **What exactly gets validated in live mode?**
   - Just "does `BEGIN; <file>; ROLLBACK;` succeed?"
   - Deeper symbol analysis (table/column/function existence with good error messages)?
   - Planning cost / obvious performance red flags via `EXPLAIN`?
   - Function body analysis via `plpgsql_check` when available?

4. **Performance & UX**
   - Live validation will be slower than pure sqlparser. How do we communicate this?
   - Should it be opt-in even when a URL is supplied?
   - Caching? (e.g., only re-validate files whose content hash changed since last successful check)

5. **Error message quality**
   - The errors will now be real Postgres errors (much better than sqlparser's).
   - How do we attribute them cleanly back to the file + approximate line?

6. **Interaction with the existing tolerant mechanism**
   - Do we still need/want the `is_trusted` bypass when we have a live DB?
   - Or does a live DB make the trusted carve-outs unnecessary?

7. **Failure modes**
   - What if the dev DB schema is behind the local migrations? (Very common.)
   - What if the user has RLS or complex permissions in dev?
   - What about statements that cannot run in a transaction (`CREATE INDEX CONCURRENTLY`, etc.)?

8. **Implementation location**
   - Extend the existing `validate_sql_with_tolerant_comments`?
   - New module `live_validation.rs`?
   - Should some of this logic be shared with the preflight in `dev.rs`?

9. **Future evolution**
   - Once we have live validation working locally, the natural next question is "can we do something similar in CI without a full prod clone?" (shadow DBs, schema snapshots, etc.). Leave hooks for that.

## What "Done" Looks Like (for the agent who implements this)

- `pg-web check` (when given a working `--url` that points at a dev database that has the right extensions and a reasonably up-to-date schema) can catch the class of errors the user was testing for:
  - References to non-existent tables/columns in handler SELECTs.
  - Broken function calls inside `$$` bodies.
  - Type mismatches.
  - Syntax errors deep inside PL/pgSQL handlers.
- The offline-only path (no `--url`, or no reachable DB) continues to behave exactly as it does today (or with only minor message improvements).
- Clear documentation (both in `--help` and in `APP-DEVELOPER-GUIDE.md`) explaining the two modes and the recommended local workflow: `pg-web up` + `pg-web check --url "$DATABASE_URL"` (or auto-detection).
- The existing `dev` preflight and `push` validation continue to work and are not regressed.
- Good error messages that name the file and give actionable Postgres diagnostics.
- At least one new test (probably using testcontainers or the existing Docker E2E harness) that proves live validation catches a real semantic error that pure sqlparser misses.

## Instructions for the Next Agent

You have full permission to:
- Read every file in `crates/pg_web_cli/src/`.
- Read all docs in `docs/`.
- Experiment locally with `pg-web up` + a toy app + deliberate broken SQL.
- Use the sibling `trucking-carriers` repo (or `examples/todo`) as a realistic test subject.
- Run any Postgres SQL experiments against a local `pg-web up` stack.

**Mandatory reading before writing code:**
1. This entire prompt.
2. `crates/pg_web_cli/src/check.rs` (the whole file, especially the tolerant splitter and how `--url` is currently used).
3. `crates/pg_web_cli/src/dev.rs` (the `preflight_sql` implementation and surrounding logic).
4. `crates/pg_web_cli/src/push.rs` (the transactional execution model and `validate_handler`).
5. `crates/pg_web_cli/src/stack.rs` + `templates.rs` (how the local dev DB is configured and discovered).
6. `crates/pg_web_cli/src/migrate.rs` (how migrations are discovered and applied).
7. The research notes embedded in this prompt (the DO block technique, PREPARE, EXPLAIN, plpgsql_check, Atlas-style hybrid approaches, pg_query tradeoffs).

**Strongly recommended:**
- Spend time actually running broken SQL through the various validation techniques against a real `pg-web up` stack and observe the exact error messages Postgres produces.
- Prototype (in a scratch branch or `cargo test`) both the classic `BEGIN; ROLLBACK;` approach and the `DO $$ BEGIN RETURN; ... END $$` approach on realistic handler + migration content.
- Think about the DX first: what should the happy path and the error output look like for a developer who just saved a broken handler while `pg-web dev` + a DB are running?

This is high-leverage work. Closing the gap between "check says it's fine" and "the route actually works" would make the framework feel dramatically more solid during the normal local development loop.

Good luck. The user who discovered this through deliberate testing will be very grateful if we make the tool finally live up to its promise when the dev database is right there.
