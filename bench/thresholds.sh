#!/usr/bin/env bash
# thresholds.sh — bench regression gate knobs + per-tier baseline tables (030).
#
# Sourced (never executed) by bench/run.sh. Kept in its own file (029 open-Q2
# lean) so the per-platform baselines are easy to re-capture and review without
# touching the harness logic. bash 3.2 safe (macOS default bash): NO associative
# arrays — the per-tier/per-workload tables are plain `case` lookups.
#
# ── The gate, in one paragraph ──────────────────────────────────────────────
# On a HEALTHY server every workload is ~100% success with real p50/p99 (the
# Results tables in docs/BENCHMARKS.md), so the gate can finally mean something.
# Two layers:
#   1. SUCCESS FLOOR (always on, every workload, default >= 99%). This is the
#      cheap, stable, platform-INDEPENDENT check — healthy is ~100%, a dead /
#      crash-looping / not-serving worker is ~0%. It is the single most valuable
#      check and is exactly what would have caught the 016 worker-self-termination
#      regression the moment it landed (it sailed through 028/029 as "expected 0%").
#   2. p99 CEILINGS + successful-req/s FLOORS (opt-in via BENCH_STRICT=1). These
#      are platform-DEPENDENT (a 1-vCPU VPS, a Linux CI box, and an Apple-Silicon
#      dev Mac all differ), so enforcing single-platform numbers by DEFAULT would
#      manufacture the very cross-platform "flakiness" CLAUDE.md/029 forbid
#      ("a non-green default run is a real bug, not flakiness"). They are therefore
#      implemented, env-tunable, and enforced only once you have re-baselined for
#      your platform and set BENCH_STRICT=1 (030 open-Q4 + B.1 #6). The baselines
#      below are one Apple-Silicon / Docker-Desktop run (2026-06-15, post-729eb93).
#
# Re-baselining: run `RUN_BENCH=1 scripts/test-all.sh` on the target platform,
# read the per-workload p99 / req/s from the bench table, and update the two
# `case` blocks below (then BENCH_STRICT=1 enforces them with the margins).

# ── knobs (env-overridable) ─────────────────────────────────────────────────
# Success floor (percent). Applies to EVERY workload, always. The historical
# BENCH_MIN_STATIC_SUCCESS (the old >=1% placeholder, a-static only) is honoured
# as a back-compat alias when explicitly set, but the default is now a real floor.
BENCH_MIN_SUCCESS="${BENCH_MIN_SUCCESS:-${BENCH_MIN_STATIC_SUCCESS:-99}}"

# p99 ceiling = baseline * BENCH_P99_MARGIN. Start generous (030 open-Q1: x2-3;
# the c=128 / 10k legs are the noisiest — the baselines below already carry extra
# headroom for them). Tighten once reproducible across several runs on a platform.
BENCH_P99_MARGIN="${BENCH_P99_MARGIN:-3}"

# successful-req/s floor = baseline * BENCH_RPS_FLOOR_FRAC. 0.5 = "a regression
# that halves successful throughput fails the gate" — comfortably above run-to-run
# variance (which is a few percent), well below a real throughput regression.
BENCH_RPS_FLOOR_FRAC="${BENCH_RPS_FLOOR_FRAC:-0.5}"

# Enforce the p99 ceilings + req/s floors (layer 2). Off by default (success
# floor + infra only). BENCH_SELFTEST forces this on.
BENCH_STRICT="${BENCH_STRICT:-}"

# ── per-tier p99 baselines (ms) ─────────────────────────────────────────────
# Echo the healthy-baseline p99 for <tier>/<label>, or nothing (=> no p99 ceiling
# for that leg; the success floor still applies). Values are rounded UP from the
# 2026-06-15 post-fix run for headroom; the x3 margin is applied on top.
# d-fast-under-slow's baseline is intentionally the *slow* HOLB value (~220 ms) —
# that leg is SUPPOSED to be slow; only a regression far beyond it should trip.
bench_baseline_p99() {   # <tier> <label>
    case "$1/$2" in
        unconstrained/a-static-c1)        echo 0.30 ;;
        unconstrained/a-static-c32)       echo 2.0 ;;
        unconstrained/a-static-c128)      echo 8.0 ;;
        unconstrained/b-todos100-c1)      echo 0.5 ;;
        unconstrained/b-todos100-c32)     echo 9.0 ;;
        unconstrained/b-todos100-c128)    echo 40 ;;
        unconstrained/b-todos10k-c1)      echo 20 ;;
        unconstrained/b-todos10k-c32)     echo 550 ;;
        unconstrained/b-todos10k-c128)    echo 2200 ;;
        unconstrained/c-write-c1)         echo 0.30 ;;
        unconstrained/c-write-c8)         echo 0.80 ;;
        unconstrained/b-todos100-c16-pure) echo 5.0 ;;
        unconstrained/d-fast-under-slow)  echo 280 ;;

        1c-2g/a-static-c1)        echo 0.30 ;;
        1c-2g/a-static-c32)       echo 2.0 ;;
        1c-2g/a-static-c128)      echo 8.5 ;;
        1c-2g/b-todos100-c1)      echo 0.5 ;;
        1c-2g/b-todos100-c32)     echo 9.0 ;;
        1c-2g/b-todos100-c128)    echo 45 ;;
        1c-2g/b-todos10k-c1)      echo 20 ;;
        1c-2g/b-todos10k-c32)     echo 550 ;;
        1c-2g/b-todos10k-c128)    echo 2200 ;;
        1c-2g/c-write-c1)         echo 0.30 ;;
        1c-2g/c-write-c8)         echo 0.80 ;;
        1c-2g/b-todos100-c16-pure) echo 6.0 ;;
        1c-2g/d-fast-under-slow)  echo 280 ;;

        # BENCH_SELFTEST probe: a deliberately-slow path (/bench/slow, pg_sleep
        # 0.2) measured against a FAST-path ceiling. ~220 ms observed vs 5 ms
        # baseline (x3 => 15 ms ceiling) => a guaranteed, platform-independent
        # breach that proves the gate is live. See BENCH_SELFTEST in run.sh.
        */selftest-slow-as-fast)  echo 5 ;;
    esac
}

# ── per-tier successful-req/s baselines ─────────────────────────────────────
# Echo the healthy-baseline successful req/s for <tier>/<label>, or nothing
# (=> no req/s floor for that leg). Rounded DOWN from the 2026-06-15 run; the
# BENCH_RPS_FLOOR_FRAC fraction is applied on top.
bench_baseline_rps() {   # <tier> <label>
    case "$1/$2" in
        unconstrained/a-static-c1)        echo 6000 ;;
        unconstrained/a-static-c32)       echo 20000 ;;
        unconstrained/a-static-c128)      echo 20000 ;;
        unconstrained/b-todos100-c1)      echo 2800 ;;
        unconstrained/b-todos100-c32)     echo 4000 ;;
        unconstrained/b-todos100-c128)    echo 4000 ;;
        unconstrained/b-todos10k-c1)      echo 55 ;;
        unconstrained/b-todos10k-c32)     echo 55 ;;
        unconstrained/b-todos10k-c128)    echo 60 ;;
        unconstrained/c-write-c1)         echo 5500 ;;
        unconstrained/c-write-c8)         echo 17000 ;;
        unconstrained/b-todos100-c16-pure) echo 4000 ;;
        unconstrained/d-fast-under-slow)  echo 1100 ;;

        1c-2g/a-static-c1)        echo 6000 ;;
        1c-2g/a-static-c32)       echo 20000 ;;
        1c-2g/a-static-c128)      echo 20000 ;;
        1c-2g/b-todos100-c1)      echo 2800 ;;
        1c-2g/b-todos100-c32)     echo 4000 ;;
        1c-2g/b-todos100-c128)    echo 4000 ;;
        1c-2g/b-todos10k-c1)      echo 55 ;;
        1c-2g/b-todos10k-c32)     echo 55 ;;
        1c-2g/b-todos10k-c128)    echo 60 ;;
        1c-2g/c-write-c1)         echo 5500 ;;
        1c-2g/c-write-c8)         echo 17000 ;;
        1c-2g/b-todos100-c16-pure) echo 4000 ;;
        1c-2g/d-fast-under-slow)  echo 1100 ;;

        # selftest probe: floor it well above the ~1400 req/s the slow path can
        # sustain so the req/s check breaches too (belt and braces with p99).
        */selftest-slow-as-fast)  echo 6000 ;;
    esac
}
