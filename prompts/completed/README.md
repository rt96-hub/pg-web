# Completed prompts

Work orders that have landed in the repo. Kept here so `prompts/` stays focused on active handoffs.

| # | File | Completed | Notes |
|---|---|---|---|
| **001** | `001_sqlparser_comment_literals.md` | 2026-05 (commit `10d415d`) | `pg-web check` accepts dollar-quoted / adjacent-literal `COMMENT ON` via trusted-statement bypass in `check.rs`. |
| **002** | `002_dev_livereload_sse_connection_leak.md` | 2026-05 (commit `11322b0`) | Client `pagehide`/`beforeunload` cleanup + 2h server SSE lifetime; documented in `APP-DEVELOPER-GUIDE.md`. |
| **003** | `003_sql_parser_gin_trgm_opclass_check_failure.md` | 2026-05 (commits `30c1235`, `3c7fe3b`) | `CREATE EXTENSION` / `CREATE INDEX` (incl. `gin_trgm_ops`) trusted in migrations. |
| **008** | `008_docs_site_pgweb_dev_dogfooding.md` | 2026-06 (commit `030c618`) | `site/` dogfooded docs app for pg-web.dev. |
| **009** | `009_docs_cleanup_and_public_readiness.md` | 2026-06 (commit `ce90dc1`) | Root README, LICENSE, CONTRIBUTING; `docs/internal/` split. |
| **010** | `010_cargo_publish_cli_and_cicd.md` | 2026-06 (commits `c1b1c7f` + release workflow) | `cargo install pg-web`; crates.io publish on tag. |
| **013** | `013_response_contract_v2.md` | 2026-06 (commits `b580ff4`, `a24e287`) | `pgweb.respond`/`redirect`/`json`/`set_cookie`; todo companion flows at `/status`, `/seeother`. |

## Still active (not moved)

| # | Why it stays open |
|---|---|
| **004** | Live-DB semantic validation for `pg-web check` not implemented. |
| **005** | JSON surface partly unblocked by 013; MCP / strategic exploration still open. |
| **006** | Per-request access logging in `pg-web dev` not shipped. |
| **007** | Tera boolean-leakage / complex-list-handler DX not addressed. |
| **011** | Ongoing content polish stub for pg-web.dev. |
| **012** | Deploy runbook — operational reference, not a one-shot deliverable. |
| **014** | Role floor + threat model landed (`7240c16`), but **statement_timeout arming** is a documented known gap (`docs/THREAT-MODEL.md`). |
| **015** | Step 1 benchmark harness + `docs/BENCHMARKS.md` done (`a370856`); Step 2 multi-worker design/impl still open. |
| **016–025** | Not started or in progress. |