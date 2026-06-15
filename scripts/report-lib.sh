#!/usr/bin/env bash
# report-lib.sh — shared reporting helpers for the pg-web test/bench harness.
#
# Sourced (never executed) by scripts/test-all.sh, bench/run.sh,
# scripts/test-http.sh. Provides the prompt-028 reporting contract:
#
#   - TEST_MODE resolution (errors | short | verbose)
#   - paired START/END step markers with a stable ASCII keyword + a human glyph
#   - libtest count parsing (sum across binaries) + failing-name collection
#   - failure-detail surfacing helpers (errors mode)
#
# Design rule (prompt 028): this changes *reporting only*. It must never alter
# which tiers run, their pass/fail semantics, or any test expectation.
#
# The MARKER CONTRACT is the stable ASCII keyword (START / PASS / FAIL / SKIP /
# STEP / STALE / BUILD / BUILT / REUSED / CANARY) plus the phase id. The unicode
# glyph is decoration only; grep on the keyword, never the glyph.

# --- glyphs (decoration) vs keywords (the contract) -------------------------
# Unicode glyphs only when writing to an interactive TTY and not under CI; ASCII
# fallback otherwise (pipes, files, CI logs) so transcripts never mangle them.
# Force ASCII anywhere with PGWEB_ASCII=1. The keyword is identical either way.
if [[ -t 1 && -z "${CI:-}" && "${PGWEB_ASCII:-}" != "1" ]]; then
    G_START='▶'; G_OK='✔'; G_FAIL='✘'; G_SKIP='·'; G_INFO='·'
else
    G_START='>'; G_OK='+'; G_FAIL='x'; G_SKIP='-'; G_INFO='-'
fi

# pgweb_mark <glyph> <phase> <KEYWORD> <detail> [dur]
# The single low-level emitter. Fixed-width phase/keyword columns keep the
# stream scannable; the optional [dur] is appended right-aligned-ish.
pgweb_mark() {
    local glyph="$1" phase="$2" kw="$3" detail="$4" dur="${5:-}"
    if [[ -n "$dur" ]]; then
        printf 'PGWEB %s %-6s %-6s %s  [%s]\n' "$glyph" "$phase" "$kw" "$detail" "$dur"
    else
        printf 'PGWEB %s %-6s %-6s %s\n' "$glyph" "$phase" "$kw" "$detail"
    fi
}

# Common paired markers. Every START must get exactly one terminal marker
# (PASS/FAIL/SKIP/BUILT/REUSED/CANARY); a START with no matching END means the
# run crashed through a phase (029 asserts on this).
mk_start() { pgweb_mark "$G_START" "$1" "START" "$2"; }            # phase, detail
mk_step()  { pgweb_mark "$G_INFO"  "$1" "STEP"  "$2"; }            # phase, detail
mk_pass()  { pgweb_mark "$G_OK"    "$1" "PASS"  "$2" "${3:-}"; }   # phase, counts, [dur]
mk_fail()  { pgweb_mark "$G_FAIL"  "$1" "FAIL"  "$2" "${3:-}"; }   # phase, detail, [dur]
mk_skip()  { pgweb_mark "$G_SKIP"  "$1" "SKIP"  "$2"; }            # phase, reason

# Free-form keyword markers (image REUSED/STALE/BUILD/BUILT, tier3 CANARY).
# mk_ok uses the green glyph; mk_note uses the neutral info glyph.
mk_ok()    { pgweb_mark "$G_OK"    "$1" "$2" "$3" "${4:-}"; }      # phase, KEYWORD, detail, [dur]
mk_note()  { pgweb_mark "$G_INFO"  "$1" "$2" "$3" "${4:-}"; }      # phase, KEYWORD, detail, [dur]
mk_build() { pgweb_mark "$G_START" "$1" "BUILD" "$2"; }            # phase, detail

# --- libtest parsing --------------------------------------------------------
# Sum "test result: ok. N passed; M failed; ..." across every binary in a log.
# Echoes "PASSED FAILED". Robust to multiple binaries (tier 2b) + the trailing
# "0 passed" lines from empty test binaries.
parse_libtest_counts() {
    local log="$1"
    # LC_ALL=C: captured logs can contain NUL / non-UTF-8 bytes (docker, curl,
    # cargo); a multibyte locale can make awk choke. C locale = byte-wise + safe.
    LC_ALL=C awk '
        /^test result:/ {
            for (i = 1; i <= NF; i++) {
                if ($i == "passed;")      p += $(i-1) + 0
                else if ($i == "failed;") f += $(i-1) + 0
            }
        }
        END { printf "%d %d", p, f }
    ' "$log" 2>/dev/null || echo "0 0"
}

# Collect failing test names from a libtest log. libtest prints exactly one
# "---- <name> stdout ----" block per failure (even on panic), regardless of
# the double "failures:" summary layout, so that line is the most robust anchor.
# Echoes a comma-separated, de-duplicated list (empty if none).
collect_failure_names() {
    local log="$1"
    # -a: captured logs contain NUL bytes (docker/curl), so grep would otherwise
    # treat them as binary and skip them; LC_ALL=C avoids multibyte-locale errors.
    LC_ALL=C grep -ahoE '^---- [A-Za-z0-9_:<>, ]+ stdout ----' "$log" 2>/dev/null \
        | LC_ALL=C sed -E 's/^---- (.+) stdout ----$/\1/' \
        | sort -u | tr '\n' ',' | sed 's/,$//' || true
}

# --- failure-detail surfacing (errors mode) ---------------------------------
# These print the *captured* detail for a failing phase — never a re-run.
# Green phases stay one line; only red phases self-expand.

# Surface just the libtest "failures:" dumps (panic output + the name list).
surface_libtest_failures() {
    local log="$1" maxlines="${2:-160}"
    echo "    ---- captured failure detail ($log) ----"
    LC_ALL=C awk '/^failures:$/{p=1} p{print}' "$log" 2>/dev/null | head -n "$maxlines" | LC_ALL=C sed 's/^/    /'
    echo "    ---- end (full log: $log) ----"
}

# Surface a tail of an arbitrary captured log (smoke section body, canary, etc.).
surface_log_tail() {
    local label="$1" log="$2" maxlines="${3:-50}"
    echo "    ---- captured detail: $label ($log) ----"
    [[ -f "$log" ]] && tail -n "$maxlines" "$log" 2>/dev/null | LC_ALL=C sed 's/^/    /'
    echo "    ---- end (full log: $log) ----"
}
