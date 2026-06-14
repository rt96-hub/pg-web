# pg_web_ext upgrade scripts (018.2)

This directory holds hand-authored `--from--to.sql` scripts for `ALTER EXTENSION pg_web_ext UPDATE`.

pgrx 0.18 (pinned) auto-generates only the base install script (`pg_web_ext--<ver>.sql`) from `extension_sql!(..., bootstrap)` + `#[pg_extern]` items in the Rust crate. It does **not** synthesize upgrade scripts (see pgrx README "Automatic extension schema upgrade scripts" TODO, and cargo-pgrx hello example notes that the author must supply the upgrade SQL).

## Convention

- Files: `pg_web_ext--A.B.C--X.Y.Z.sql` (or patch forms).
- Place the exact DDL that takes an instance whose `pg_extension.extversion` is `A.B.C` to the state of version `X.Y.Z`.
- The Dockerfile copies these (plus the pgrx-generated install) into the image's extension dir; the existing `pg_web_ext--*.sql` wildcard picks them up.
- Postgres builds a graph of available scripts and runs the minimal chain when you `ALTER EXTENSION ... UPDATE` (or `UPDATE TO 'target'`). Multi-step works (e.g. 0.2.0 → 0.4.0 runs 0.2→0.3 + 0.3→0.4 if both scripts present). This is the standard Postgres extension mechanism (see contrib modules in the share dir for real examples).

## Policy (additive by default in Phase 1/2)

See CLAUDE.md (architectural invariants + coding practices) and `docs/DEPLOYMENT.md`.

- **Additive** (new tables, nullable columns, widening CHECK, new SQL functions, new seeds with ON CONFLICT DO NOTHING, new GRANTs, COMMENTS, etc.): safe. Put verbatim DDL in the upgrade script.
- **Destructive** (DROP, narrowing, type changes that can fail on data, column removal): require explicit data-preservation migration steps + breaking note in the script + changelog. Avoid in Phase 1 where possible.
- Worked example of safe additive: the assets CHECK cap 2 MiB → 20 MiB (widening) was done on fresh installs only before this; future such changes must also have the `ALTER TABLE ... DROP CONSTRAINT ...; ALTER TABLE ... ADD CONSTRAINT ... CHECK (length <= 20M);` (or equivalent) in the appropriate upgrade script.
- No downgrade scripts (`ALTER EXTENSION ... UPDATE TO <older>`). Supported rollback = `pg_dump` before upgrade + restore. Documented explicitly.

## Adding a real change

1. Make the change in the main `extension_sql!` bootstrap block in `src/schema.rs` (fresh `CREATE EXTENSION` gets it).
2. Append the *same* DDL (or the precise delta) to the current `--from--to` script (or create the next one when version bumps).
3. The upgrade script must be valid on PG 15/16/17 (per invariant #6).
4. Add/update a test that exercises the upgrade path (see the self-upgrade smoke in docker_e2e + syntax validation for other majors).
5. Update docs, changelog, etc.

## First script

`pg_web_ext--0.2.0--0.3.0.sql` is the skeleton for the next minor (or patch). It is shipped so the packaging + ALTER mechanism can be tested and so the "scripts land in the image" acceptance is met. Real deltas for 0.3.0 will be added to it (or a new file if we introduce intermediate) when the version actually advances.

No changes that require a version bump are forced by 018.2 itself; the infrastructure + policy + test tier are the deliverables. A subsequent change (or the readiness bits in sibling 018.1) can be the first real customer that travels through a real upgrade script rather than "only fresh install."
