# pg-web — handoff prompts

Work-order specs for upcoming pg-web work, written in the project's own
`prompts/` house style (status / context / options-with-**Lean** / research
tasks / acceptance criteria / open questions).

**Completed work orders** live in [`completed/`](completed/) (001–003, 008–010, 013 as of 2026-06-12). Everything else in this directory is still active.

They were derived from a full external read of the codebase on **2026-06-11**.
Each prompt's claims were re-verified against the actual source before it was
written; `file:line` citations point into the repo at commit `918f40b`.

## Where things are

- **Source analysis (read this first):** `../../pg-web-analysis.html` — the
  interactive report these prompts came out of (architecture, strengths, graded
  problem findings, competitive landscape, scorecard, recommendations).
- **The repo under review:** cloned read-only at `/tmp/pg-web` (git `918f40b`,
  v0.2.0). Re-clone with `git clone https://github.com/rt96-hub/pg-web /tmp/pg-web`
  if it's been cleared.
- **Project conventions a session should load before starting:** `/tmp/pg-web/CLAUDE.md`
  (architectural invariants + coding rules), `docs/ROADMAP.md` (phases + decision
  log), `docs/internal/sessions/session_6.md` (the drafted Phase 2 spec).

## The prompts

| # | Title | Theme | Severity from analysis |
|---|---|---|---|
| **014** | Execution-role hardening, per-request `statement_timeout`, threat model | Security | Critical |
| **015** | Concurrency & throughput — benchmark first, then multi-worker | Performance | Critical |
| **016** | Request-path caching (templates + routes) + graceful shutdown | Performance | High |
| **017** | HTTP capability floor — full method set, uploads, compression, range | Capability gaps | High |
| **018** | Lifecycle & observability — upgrade scripts, health, metrics, request log | Operations | Medium |
| **019** | Roadmap resequencing — pull backup/export forward + ecosystem motions | Strategy (memo) | — |
| **020** | Site v2 — landing + docs + blog (the flagship Phase-2 dogfood) | Product / dogfood | — |
| **021** | SSH-tunneled remote deploy (`pg-web push --target`) | Deploy / ops | High |
| **022** | Large-asset streaming via `pg_largeobject` (>20 MiB) | Capability gap | Medium-High |
| **023** | Phase 2 core: cookie sessions + RLS bridge + realtime SSE (session_6 tracks) | Phase 2 foundation | Critical |

## Recommended sequencing & dependencies

The single most important finding: **013 and 014 are unacknowledged prerequisites
sitting underneath the drafted Phase 2 (auth/RLS).** Auth needs `Set-Cookie`, which
the response contract doesn't yet allow (013); RLS policies won't enforce while
every handler runs as the Postgres superuser (014). Do these before Phase 2, not
during it.

```
                ┌──────────────────────────────────────────────┐
 FOUNDATION     │ 013 Response contract   014 Privilege floor   │  ← land these first
 (unblock P2)   │   (cookies/redirect/      (non-superuser role, │
                │    content-type/JSON)      timeout, threat model)│
                └───────┬───────────────────────┬────────────────┘
                        │                        │
 PERFORMANCE    015 Concurrency + benchmark   016 Caching + graceful shutdown
 (measure→fix)    (benchmark is a no-dep         (low-risk, high-leverage;
                   first step; ties to 014         reuses existing LISTEN/NOTIFY)
                   timeout & 016 caching)
                        │
 CAPABILITY     017 HTTP floor (methods / multipart uploads / compression / range)
                        │            └── uploads enable images in 020
                        │
                        │ 022 Large-object streaming (full >20 MiB assets)
                        │
 DEPLOY / OPS   018 Lifecycle (health + upgrades first)
                021 SSH-tunneled `push --target` (F.2 — user-flagged, pairs with shipped F.3)
                        │
 STRATEGY       019 Roadmap memo (sequences everything above; promotes backup/export)
                        │
 PHASE 2 CORE   023 Implement session_6 tracks (A auth/cookies, B RLS bridge, C realtime)
                        │            (after 013/014; produces the primitives 020 dogfoods)
                        │
 DOGFOOD        020 Site v2 ── depends on 013 (cookies/redirect), 014 (RLS enforces),
                               017 (image uploads), 022 (large media), and 023 (Phase 2).
                               This is where Phase 2 gets proven on the real site.
```

Suggested order of execution:

1. **015 benchmark step only** — it has no dependencies and turns the unverified
   "1,000 req/s" VISION claim into a published number. Do it early so later wins
   are measured, not asserted.
2. **013 Response Contract v2**, then **014 privilege floor + threat model** — the
   Phase-2 unblockers. Highest leverage in the set.
3. **016 caching + graceful shutdown** — cheap, contract-free reliability/perf win;
   reuses the `ListenRouter` that already exists.
4. **017 capability floor** — methods first (small), then uploads (enables 020's
   images), then compression/range.
5. **021 SSH-tunneled remote push (`--target`)** — the main remaining user-flagged
   item from Session 5 (F.2). Pairs with already-shipped F.3 (CLI in image). Can
   run in parallel with much of the above once 013/014 are far enough for any
   auth-related remote testing.
6. **022 large-object streaming** — completes the deferred half of Session 5 I
   (true >20 MiB support). Benefits from 017 Range work; enables richer media in 020.
7. **018 lifecycle/observability** — pull the `/_pgweb/health` endpoint and the
   `ALTER EXTENSION` upgrade convention forward; both are urgent and small.
8. **015 multi-worker design** — once the benchmark shows where the ceiling is.
9. **023 Phase 2 core (session_6 tracks A/B/C)** — the actual implementation of
   cookie sessions, RLS bridge, and realtime. Must follow 013+014; produces the
   primitives that 020 dogfoods.
10. **019** is a maintainer memo to review up front (it frames all of the above) and
    **020** is the capstone dogfood that consumes 013/014/017/022/023 + the rest
    of Phase 2. This is where Phase 2 gets proven on the real site.

## Notes for the implementing session

- These are *proposals with recommended leans*, not settled decisions — each ends
  with open questions for the maintainer. Treat the **Lean:** lines as the
  analyst's recommendation, not a mandate.
- The prompts forward-reference each other by number (e.g. 014 cites 013). Those
  numbers don't exist in the repo's `prompts/` yet (it stops at 012) — copy these
  into `prompts/` if/when the project adopts them.
- 021–023 were added after a review of the detailed designs in
  `docs/internal/sessions/session_5.md` (F.2 remote deploy + the deferred half of I)
  and `docs/internal/sessions/session_6.md` (the full three-track Phase 2 spec).
  They were not yet expressed as numbered handoff prompts in the original 013–020
  set even though the underlying designs and open questions were already written.
- Several prompts also surfaced incidental repo defects worth fixing on sight:
  a ROADMAP vs session_6 contradiction on "SET LOCAL ROLE" vs "GUC" (014),
  secrets documented as GUCs but implemented as a `pgweb.settings` table (014),
  a dead `/app-layout` link in `site/pages/_404.html` (should be `/layout`) and
  duplicated `<nav>` markup across site pages (020), and a `DEPLOYMENT.md` that
  already documents an `ALTER EXTENSION` upgrade path that doesn't exist yet (018).
- Respect the project's stated non-goals (no ORM, no managed-DB support, HTMX-first,
  Postgres-only, no GraphQL) — none of these prompts should violate them; flag it
  if an implementation seems to.

---
*Generated 2026-06-11 from `pg-web-analysis.html`. Repo @ `918f40b`.*
*021–023 added after review of `session_5.md` (deferred F.2 + large-object streaming) and `session_6.md` (Phase 2 tracks); these designs existed but had not yet been turned into numbered handoff prompts in the 013–020 set.*
