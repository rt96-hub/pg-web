# 028 — Test & benchmark harness: summary modes, clear step markers, and an un-truncatable result line

**Status:** Open handoff prompt — high priority for dev-loop + agent trustworthiness. Pairs with **029** (idempotency / currently-passing verification), which should land immediately after.
**Date opened:** 2026-06-15
**Author:** Handoff from the owner (observed agent behavior: tests/benches silently failing-or-skipped, `head -200` truncation hiding the real result, no clear per-step confirmation).
**Prerequisites:** None hard. Builds directly on the prompt-025 harness (per-tier status table, content-hash image freshness, canary, `STRICT`/`TEST_TS`). Does **not** require 029, but 029's idempotency verification consumes the clear markers this prompt adds.

---

## Summary

`scripts/test-all.sh` and `bench/run.sh` are correct in *what* they run (five hard-gated tiers + an opt-in benchmark, no silent skips of the Docker tiers). They are bad at *reporting* it. Every tier streams its raw `cargo` / `docker` / `oha` output straight to the terminal — thousands of lines per run. The only compact signal (`print_summary_table`, `scripts/test-all.sh:109-116`) is a handful of lines at the very bottom, **after** all that noise, and it records only an exit-code-derived `PASS`/`FAIL` per tier — never the `x/x passed` counts, never the names of failing tests.

The consequences (all observed by the owner):

- **Agents truncate the output and never see the verdict.** A run is piped through `head -200`, or the tool harness truncates the tool result, and the `test result: FAILED` line (and the summary table) are below the cut. The agent reports "tests pass" because it never saw otherwise.
- **Failures and skips get silently accepted.** A soft-failed tier (1 / 2a / 3 are deliberately non-fatal for dev UX — `scripts/test-all.sh:269-281, 295-304, 373-380`) scrolls past in the noise; the script exits `0` in non-strict mode (`:437-439`); the agent moves on.
- **You cannot confirm a step actually happened.** The Docker image build is run with its output sent to `/dev/null` (`scripts/test-all.sh:181,187,236,258`). There is no "building → built" confirmation pair — just a one-line "rebuilding" notice and then silence until the next tier. When something is *skipped* (image reused, canary path, bench gated off) there is no explicit, greppable line saying so.

This prompt makes the harness **report like a build system**: clear paired start/finish markers for every phase (so each step is provably observed), three output verbosity modes, and a single machine-greppable final result line that is short enough that no reasonable truncation can hide it. It also makes the mandated session-bookend command auto-surface failure detail instead of requiring a second manual run.

It is a **reporting/observability** change. It must not weaken a single gate, change which tiers run, or alter pass/fail semantics — only how they are surfaced.

## Why this matters now

- **The bookend ritual is load-bearing and currently defeatable.** `CLAUDE.md` mandates a full `scripts/test-all.sh` at the start and end of every major task, and a before/after report (`CLAUDE.md:109-129`). That ritual is only as good as the agent's ability to *see* the result. Today the result is the easiest thing in the output to miss.
- **"It passed" must be falsifiable from a few lines.** The fix is a compact, bottom-anchored, ASCII-stable summary block plus a one-line `PGWEB-RESULT …` verdict that the completion report must quote verbatim. An agent that pastes that line cannot accidentally claim green on a red run, and a human scanning the transcript sees the truth instantly.
- **Skips must be loud.** A green claim should require real `x/x` counts for every mandatory tier with `failed=0`. `SKIP`, a missing count, `n/m` with `n<m`, or the absence of the verdict line must all read as **not green** — by construction, not by the reader's vigilance.
- **029 needs observable steps.** Verifying idempotency (029) means watching whether a given run *reused* or *rebuilt* the image, whether the canary served, whether the lock was reclaimed. Those decisions must be explicit, greppable lines — which is exactly what this prompt adds.

## Current behavior (evidence — read before changing anything)

1. **Raw output is streamed; the summary is buried.** Each tier invokes its tool with output going straight to the terminal: Tier 1 `cargo pgrx test` (`scripts/test-all.sh:265`), Tier 2a `bash test-http.sh` (`:291`), Tier 2b `cargo test -p pg-web` (`:309`), Tier 3 `cargo test … docker_e2e -- --ignored --test-threads=1` (`:368`), Tier 4 `bash smoke-cli.sh` (`:398`). The compact `print_summary_table` is only emitted at `:417`, after everything.

2. **The status table records exit code, not counts.** `record_tier "$name" "PASS|FAIL" "$dur"` (`scripts/test-all.sh:103-108`) is fed a literal `PASS`/`FAIL` derived from `$?` (e.g. `:278-281`). For the hard tiers (2b at `:311`, 4 at `:400`) it always records `PASS` because `set -e` would have aborted otherwise. **Nowhere is the libtest `test result: ok. N passed; M failed` parsed.** The owner's explicit ask — "Tier 1 (x/x passed)" — is not met.

3. **No paired step confirmation.** `ensure_image_fresh` (`scripts/test-all.sh:175-260`) decides reuse-vs-rebuild and, when it rebuilds, calls `build-image.sh >/dev/null` (`:181,187,236,258`). You see "source newer than image — rebuilding" then nothing until it returns. There is no "BUILDING → BUILT" pair, and on the reuse path there is no explicit "image is fresh, REUSED" line at all — silence reads identically to "skipped the check".

4. **The canary and bench gating are semi-silent too.** The canary prints a header line (`:360`) and, on success, nothing definitive (`do_tier3_canary` returns 0 quietly, `:347-349`). The benchmark only runs under `RUN_BENCH=1` (`:406`); when it is gated off there is no line saying "bench: SKIPPED (set RUN_BENCH=1 to include)".

5. **Bash-driven sub-tests have ad-hoc, unparseable output.** `smoke-cli.sh` uses `step "N. …"` / `fail "…"` (`scripts/smoke-cli.sh:29-85`) with 19-ish human-labelled sections (numbered `1`…`19` with the odd `16`/`16a`/`16b`, `:587,606,638`); there is no machine marker to count "19/19 sections" or to name the failing one. `test-http.sh` runs a multi-phase bootstrap (reinstall `.so` `:50-56`, restart PG `:58-62`, self-heal DB `:69-78`, `CREATE EXTENSION` `:81-83`, wait `:8080` `:86-95`, port-shadow preflight `:97-158`, run `http_smoke` `:161`) with no per-phase markers, so a hang there is unlocatable from the captured log.

6. **`bench/run.sh` streams oha and has no compact verdict.** `run_oha` tees full oha output to a file and the terminal (`bench/run.sh:189`); `run_workloads` (`:195-241`) runs ~13 oha invocations plus the HOLB pair. There is no per-workload one-line summary (req/s + p99), no threshold check, and no final `OVERALL` line — so "did the benchmark pass?" is a human judgement call over pages of histograms.

## What to build

### 1. Three output modes (`TEST_MODE`, with `--short` / `--errors` / `--verbose` flags)

Add a `TEST_MODE` env var honoured by **both** `scripts/test-all.sh` and `bench/run.sh` (and a thin flag parser so `scripts/test-all.sh --errors` works too):

- **`errors` — DEFAULT.** Print only the clear step markers (§2) and the compact per-tier results. Capture each phase's full output to a per-run log file (§4). On **any** failure, automatically print the *relevant captured detail for the failing items only* (the cargo `failures:` block, the failing smoke section + its body, the canary `docker logs` tail, the breached bench threshold) — green phases stay one line each. This is the mandated bookend mode: compact when green, self-expanding when red, **without a second run**.
- **`short`.** Markers + compact results only; never auto-expand. For the cleanest possible scan. Failures still show the failing test **names** and `n/m` counts (just not the full trace); always print the path to the per-phase log and the exact `TEST_MODE=verbose` command to get the stream.
- **`verbose`.** Today's behavior: stream all raw `cargo`/`docker`/`oha` output to the terminal, **plus** the markers and summary. For deep debugging.

**Auto-escalation is capture-then-surface, not re-run.** The owner's phrasing is "if there are failures, run the detailed version that exposes the errors." Implement that as: always capture full output once (§4); in `errors` mode, surface the captured failure detail immediately. Do **not** re-execute a failed tier — re-running flaky/expensive Docker tiers is precisely the behavior 029 is trying to eliminate, and a fresh run can mask or change the failure. The captured detail *is* the detailed version, available instantly. If the user wants a fresh streamed run, they invoke `TEST_MODE=verbose`. State this rationale in the script comments and docs.

**Lean:** default `errors`. It is exactly the "compact, but show me what broke" behavior the bookend ritual needs, and it makes the common green run quiet without hiding red ones.

### 2. Clear, paired step markers (the "building → built" requirement)

Every phase emits a **START** line and a terminal **END** line (PASS / FAIL / SKIP / BUILT / REUSED …). Use a human glyph for the terminal **and** a stable ASCII keyword so `grep` works regardless of glyph rendering. Suggested shape (tune freely, but keep the keyword + tier id + counts):

```
PGWEB ▶ tier1   START  cargo pgrx test pg17
PGWEB ✔ tier1   PASS   95/95                                   [13s]
PGWEB ▶ tier2a  START  HTTP smoke (test-http.sh)
PGWEB · tier2a  STEP   reinstall runtime .so → restart PG → CREATE EXTENSION → wait :8080
PGWEB ✔ tier2a  PASS   6/6                                     [16s]
PGWEB ▶ image   START  freshness check (content-hash)
PGWEB · image   STALE  have=abc123… want=def456…  → rebuild required
PGWEB ▶ image   BUILD  rtaylor96/pg-web:latest  (docker build)
PGWEB ✔ image   BUILT  src_hash=def456…                        [8m02s]
PGWEB ▶ tier3   START  canary probe GET /
PGWEB ✔ tier3   CANARY serving                                 [4s]
PGWEB ▶ tier3   START  docker_e2e (14 tests)
PGWEB ✘ tier3   FAIL   12/14  failing: dev_error_page_surfaces_sql_exception_detail, livereload_sse_chain_end_to_end   [142s]
PGWEB ▶ tier4   START  smoke-cli (19 sections)
PGWEB ✔ tier4   PASS   19/19                                   [8s]
PGWEB · bench   SKIP   set RUN_BENCH=1 to include the 015 benchmark
```

Hard requirements on the markers:

- **The image decision is always explicit.** Exactly one of: `image REUSED (fresh, src_hash=…)`, or the `STALE → BUILD → BUILT` triple. Reuse must never be silent — "REUSED" is the proof the freshness check ran and decided. (The owner's literal example: `docker building > docker built`.)
- **The canary is explicit.** `CANARY serving` on success, `CANARY ABORT (/ never answered in 30s)` + the logs tail on failure. No silent success.
- **Skips are loud lines, not absences.** Bench gated off, a soft tier that didn't run, etc. all emit a `SKIP` marker with the reason.
- **Every START has exactly one matching END.** A phase that the script exits/crashes through without an END line is itself a signal (incomplete run). 029 will assert on this.

`build-image.sh` must stop being called with `>/dev/null`. In `verbose` mode its output streams; in `short`/`errors` it is captured to the image build log, with the `BUILD`/`BUILT` markers (and elapsed time) printed regardless. A build *failure* must surface (the docker build error tail) in all modes.

### 3. The un-truncatable result block (the single most important deliverable)

At the very end, after the per-tier table, print a compact **ASCII-only** verdict line plus, on non-green, one pointer line per failing phase:

```
PGWEB-RESULT  tier1=95/95  tier2a=6/6  tier2b=131/131  tier3=12/14  tier4=19/19  bench=skip  OVERALL=FAIL
PGWEB-FAIL    tier3  failing: dev_error_page_surfaces_sql_exception_detail, livereload_sse_chain_end_to_end  (log: /tmp/pg-web-test-all-12345/tier3.log)
```

Rules:

- `OVERALL=PASS` **iff** every mandatory tier reports `x/x` with `failed=0` and no mandatory phase is `SKIP`/missing. Any `FAIL`, any `SKIP` of a mandatory tier, any missing count ⇒ `OVERALL=FAIL`. (Bench is opt-in: `bench=skip` does not by itself make `OVERALL=FAIL`; `bench=fail` does.)
- The counts are **parsed from real output, not hardcoded.** A drop in total count is suspicious but is an open question (§Open questions), not a hard gate here — the hard gate is `failed=0`.
- The block is ≤ ~10 lines on a green run. Even `head -50` of a full run will include it (the markers stream first, this prints last but the whole compact run is well under 100 lines). Keep it ASCII so terminals/transcripts never mangle it.
- `bench/run.sh` prints an analogous `PGWEB-BENCH … OVERALL=ok/fail` line (§5).

### 4. Capture mechanism

- Create a per-run log dir (e.g. `RUN_DIR=/tmp/pg-web-test-all-$$`, or under `bench/results/run-$$/` for bench), print its path in the START banner, and write each phase's combined stdout+stderr to `"$RUN_DIR/<phase>.log"`. In `verbose`, also `tee` to the terminal.
- **Keep the logs after the run** (do not delete on exit) so failures can be inspected post-hoc; 029's startup hygiene reaps old ones. Print the exact log path next to every `FAIL`.
- Run the libtest-based tiers with `--no-fail-fast` so a failure still yields the **full** count and **all** failing names in one pass (today Tier 3 stops reporting after the first failure block is enough to fail). Parse `test result: ok. N passed; M failed; …` (summing across the multiple binaries in Tier 2b) and collect names from the `failures:` section.

### 5. Apply the same treatment to `bench/run.sh`

- Per-workload markers: `PGWEB ▶ bench START <label>` → `PGWEB ✔ bench OK <label> req/s=… p50=… p99=…` (parse from oha's captured output; `bench/run.sh:183-192`).
- A compact end-of-run table (label, req/s, p50, p99) and the **HOLB before/after** (`b-todos100-c16-pure` vs `d-fast-under-slow`) as an explicit two-line comparison — that is the headline result and should never require reading histograms.
- A regression threshold check (generous, to start: e.g. static/100-row `c1` p99 under a documented bound and req/s above a floor) producing `bench=ok|fail` for the `PGWEB-RESULT`/`PGWEB-BENCH` lines. Wire the threshold values + rationale into `docs/BENCHMARKS.md`; keep them loose enough not to false-alarm on the known arm64/Docker variance, tight enough to catch an order-of-magnitude regression.
- Honour `TEST_MODE` identically (errors default; capture oha output; surface only breached workloads on fail).

## Changes to the tests themselves (and their outputs)

The harness should parse what the tools already emit where possible, but two bash-driven tiers need machine-parseable markers so they can be counted and pinpointed:

- **`scripts/smoke-cli.sh`** — rework `step()` / `fail()` (and add a `pass`/section-close) to emit stable markers around each section: e.g. `PGWEB-SMOKE step=<n> name="…" START`, `… OK [dur]`, `… FAIL reason="…"`. Keep the existing human text. Derive the section total programmatically (so "19/19 sections" is computed, not assumed) and **fix the historical odd numbering** (`16`/`16a`/`16b`, `scripts/smoke-cli.sh:587,606,638` — renumber to a clean monotonic sequence; this was flagged in prompt 025 #8). On failure, the failing section number + reason + the captured body must be what the harness surfaces — not the whole 700-line log. Preserve the existing image-ID postcondition assert (`:178-180`).
- **`scripts/test-http.sh`** — emit a `PGWEB · tier2a STEP <phase>` line before each bootstrap phase (reinstall `.so`, restart PG, self-heal DB, `CREATE EXTENSION`, wait `:8080`, port-shadow preflight, run `http_smoke`) so a hang is locatable from markers alone, and surface the inner `http_smoke` libtest count as the tier-2a `x/x`. Treat a bootstrap failure (e.g. the `:8080` timeout at `:88-93`, which already dumps the PG log tail) as a tier failure with that tail captured.
- **`crates/pg_web_cli/tests/docker_e2e.rs`** — no behavioral change required; keep `#[ignore]` + `--ignored`. Ensure the run is `--no-fail-fast` so all failing names appear in one pass, and that `wait_for_http`'s existing `docker logs` tail on timeout (`docker_e2e.rs:79-83`) lands in the captured `tier3.log` and is surfaced in `errors` mode. (Optional nicety: a stable `E2E-CASE <name> OK` line, but libtest's `test … ok`/`FAILED` already suffices for parsing.)
- **Tier 1 / Tier 2b** — pure libtest parsing; no test-code change. Just `--no-fail-fast` + parse.

Do **not** change any assertion, expectation, timeout, or `#[ignore]` to make a red test green. This prompt only changes *reporting*. If a test is genuinely failing, that is 029's (or the relevant feature's) problem, not this one's.

## Documentation updates (required — "update the bibles" rule)

- **`CLAUDE.md`** — update the "Critical startup gate" (`:9-41`) and "Session rituals" (`:109-129`):
  - The mandated bookend command is `scripts/test-all.sh` (default `errors` mode); for perf-touching work, `RUN_BENCH=1 scripts/test-all.sh`. State the three modes and when to use each.
  - **New hard rule:** a completion / bookend report must quote the final `PGWEB-RESULT …` line (and the per-tier table) **verbatim**. A green claim requires `OVERALL=PASS` with real `x/x` (`failed=0`) for every mandatory tier. `SKIP`, missing counts, `n/m` with `n<m`, a missing verdict line, or truncated output all mean **not green** — never report green without the line. (This is the rule that kills "I ran `head -200` and it looked fine.")
  - Note that `errors` mode already contains the failure detail (no second run needed); `TEST_MODE=verbose` is for a fresh stream.
- **`docs/TESTING.md`** — document `TEST_MODE` in the env-knobs area (`:27-31`), the marker vocabulary, the `PGWEB-RESULT` contract, and the per-run log dir. Update the TL;DR (`:5-31`) to mention the compact default.
- **`docs/BENCHMARKS.md`** — document the bench modes, the per-workload compact summary, the HOLB before/after line, and the regression threshold values + rationale (extend the "How to reproduce / regression guard" section, `:106-116`).
- **`docs/internal/TESTING-SETUP.md`** — add the marker/mode reference to the harness section (it already documents the 025 integrity fixes at `:152-171`); record an acceptance run with the new compact output pasted in.

## Acceptance criteria

1. `TEST_MODE` (`errors` default, plus `short`, `verbose`) works in both `scripts/test-all.sh` and `bench/run.sh`, selectable by env var and by `--short`/`--errors`/`--verbose` flags.
2. Every phase emits a paired START/END marker; the image decision is always an explicit `REUSED` or `STALE→BUILD→BUILT` triple (never silent, never `>/dev/null`); the canary, soft-tier non-runs, and bench gating all emit explicit `SKIP`/status lines.
3. The per-tier results show real `x/x passed` counts and, on failure, the failing test **names** — for all five tiers (libtest-parsed for 1/2a/2b/3; section-counted for 4).
4. A single ASCII `PGWEB-RESULT … OVERALL=PASS|FAIL` line is the last substantive output; `OVERALL=PASS` iff every mandatory tier is `x/x` with `failed=0` and no mandatory phase is skipped. A non-green run prints a one-line `PGWEB-FAIL <tier> …` pointer (with log path) per failing phase.
5. In `errors` mode, a failing run auto-surfaces the captured detail for the failing items only (cargo `failures:` block / smoke section + body / canary logs / bench threshold), with green phases staying one line — **and does not re-run any tier**.
6. `bench/run.sh` prints per-workload one-line summaries, an explicit HOLB before/after pair, a threshold check, and a `PGWEB-BENCH … OVERALL=ok|fail` line.
7. `smoke-cli.sh` emits machine-parseable per-section markers, computes its section total, and has clean monotonic section numbering; `test-http.sh` emits per-phase STEP markers and surfaces the inner libtest count.
8. No gate weakened: same five tiers, same hard/soft semantics (or stricter), Docker tiers still mandatory with no silent skips, no test expectation altered. A deliberately-broken tier still fails and is now *more* visible, not less.
9. `CLAUDE.md`, `docs/TESTING.md`, `docs/BENCHMARKS.md`, and `docs/internal/TESTING-SETUP.md` updated per above, including the verbatim-`PGWEB-RESULT` reporting rule.
10. Demonstrated on the current tree: a green `scripts/test-all.sh` run (compact output pasted into the report) **and** a run with one deliberately-failing test showing the auto-surfaced detail + the `OVERALL=FAIL` line. (Use the full single command for the green proof, per the bookend ritual.)

## Open questions

1. **Default mode — `errors` vs `short`?** Lean `errors` (self-expanding on failure) as the mandated bookend default. Is there a case for `short` as default with `errors` reserved for CI? (CI likely wants `verbose` archived to a log artifact regardless.)
2. **Count-drop guard.** Should `PGWEB-RESULT` warn (or fail under `STRICT`) when a tier's *total* count drops vs a checked-in baseline (catching accidentally-`#[ignore]`'d or deleted tests), or is `failed=0` sufficient for now? A baseline file is brittle; a warning-only line may be the sweet spot.
3. **Marker glyphs in CI logs.** Keep the Unicode glyphs (nice in a terminal) alongside the ASCII keywords, or ASCII-only when `CI`/non-TTY is detected? The ASCII keyword is the contract either way.
4. **Bench thresholds.** What are the initial req/s floors and p99 ceilings per workload that catch a real regression without false-alarming on arm64/Docker-Desktop variance? Start loose and tighten once 029 establishes a stable green baseline.
5. **Per-run log retention.** Keep the last N run dirs and let 029's hygiene reap the rest, or always overwrite a single `/tmp/pg-web-test-all-latest/`? (Unique-per-PID dirs are safer for concurrent/sequential runs; retention policy is the question.)

---

*The harness already runs the right things. This makes it impossible to not see the result: clear paired markers for every step, three honest verbosity modes, failure detail that expands itself, and one ASCII verdict line short enough that no truncation can bury it. "It passed" becomes a claim you can falsify from three lines — which is the whole point.*
