# pg-web — Test-Suite Machine Setup

How to configure a machine so that **all five tiers of `scripts/test-all.sh` actually start and run**. Companion to `docs/TESTING.md` (what the tiers test) and `docs/internal/DEVELOPER-GUIDE.md` (dev loop). This doc exists because the most common harness failure is *environmental* — a tier silently skipping or dying before it runs a single test — and the fixes are not discoverable from the test output alone.

Scope: macOS (Apple Silicon) dev machines and Linux CI. The original WSL2 bring-up lives in `docs/internal/HANDOFF.md`; this doc supersedes it for the testing-environment parts.

Last verified green: 2026-06-13 (prompt 025 harness hardening; see § Verified configuration record).

## What each tier needs from the machine

| Tier | Command | Machine requirements |
|---|---|---|
| 1. SQL / pgrx | `cargo pgrx test pg17` | Compiled PG under `~/.pgrx/17.*/pgrx-install/` + entry in `~/.pgrx/config.toml`. Built by `cargo pgrx init` (see ICU gotcha below). |
| 2a. HTTP smoke | `scripts/test-http.sh` | Same as tier 1, **plus** `~/.pgrx/data-17/postgresql.conf` containing `shared_preload_libraries = 'pg_web_ext'`, plus host ports **8080** and **28817** free. |
| 2b. CLI | `cargo test -p pg-web` | Docker daemon reachable (testcontainers pulls `postgres:16` on first run). No pgrx needed. |
| 3. Docker E2E | `cargo test -p pg-web --test docker_e2e -- --ignored` | Docker + the test image `rtaylor96/pg-web:latest`. `test-all.sh` auto-builds/rebuilds it when sources are newer (`scripts/build-image.sh`). |
| 4. CLI smoke | `scripts/smoke-cli.sh` | Docker + same image + `target/debug/pg-web` binary (tier 2b's build produces it) + host ports **8080** and **5432** free. |

`scripts/test-all.sh` prints an "Environment sanity" banner up front telling you whether Docker is reachable and whether a usable `pg_config` exists for `PG_MAJOR`. If that banner is wrong, fix the machine first — nothing below it will improve.

## One-time macOS setup (Apple Silicon)

1. **Xcode Command Line Tools** — `xcode-select --install` (provides clang, make, headers).
2. **Rust** — rustup stable (workspace floor is 1.95).
3. **Homebrew packages**:

   ```bash
   brew install icu4c pkg-config
   ```

   `icu4c` is required to compile PostgreSQL ≥ 16 from source (ICU became a default configure dependency in PG 16). `pkg-config` is how configure finds it.
4. **cargo-pgrx**, pinned to the workspace's pgrx version (`pgrx = "=0.18.0"` in `crates/pg_web_ext/Cargo.toml`):

   ```bash
   cargo install --locked cargo-pgrx --version =0.18.0
   ```
5. **Build the bundled Postgres major (17).** This is the step with the trap — read the ICU section below before running it:

   ```bash
   PKG_CONFIG_PATH="$(brew --prefix icu4c)/lib/pkgconfig" \
     cargo pgrx init --pg17 download
   ```

   ~4 minutes on an M-series. Only the bundled image major is a correctness target (decision 2026-06-12, ROADMAP § Decision log); add `--pg15 download --pg16 download` only if you want to run tiers against older majors for curiosity — the `pg15`/`pg16` cargo features merely need to compile. Do **not** run bare `cargo pgrx init` — it downloads and compiles *all* pgrx-supported majors (13–18).
6. **Register the background worker in each dev data dir** (created by step 5):

   ```bash
   echo "shared_preload_libraries = 'pg_web_ext'" >> ~/.pgrx/data-17/postgresql.conf
   # repeat for data-15 / data-16 only if you initialized those majors
   ```

   Required for tier 2a and any `cargo pgrx run` workflow — the HTTP worker is a static BGW registered at postmaster start; `CREATE EXTENSION` alone never starts it. (`cargo pgrx test` is unaffected: the `#[pg_test]` harness builds its own test cluster and injects conf options itself.)
7. **Create the tier-2a dev database.** `scripts/test-http.sh` connects to a database named `pg_web_ext` on the dev instance and assumes it exists — it is normally a side effect of running `cargo pgrx run pg17` once (pgrx creates a DB named after the crate). On a data dir where that never happened, tier 2a dies with `FATAL: database "pg_web_ext" does not exist`. Either run `cargo pgrx run pg17` once and quit psql, or create it directly:

   ```bash
   ~/.pgrx/17.*/pgrx-install/bin/pg_ctl -D ~/.pgrx/data-17 -l ~/.pgrx/17.log start  # if not already up
   ~/.pgrx/17.*/pgrx-install/bin/createdb -h localhost -p 28817 pg_web_ext
   ```

   (Port = 28800 + major: 28815/28816 for the other data dirs if you run tier 2a against them.)
8. **Docker Desktop** — install, start, and leave running. The test image is built automatically on first `scripts/test-all.sh`.

## The ICU gotcha (the day-one failure this doc exists for)

**Symptom.** `scripts/test-all.sh` says:

```
~/.pgrx pg17: NO usable pg_config (Tier 1 + 2a will print guidance and be skipped)
```

and `cargo pgrx init --pg17 download` fails with:

```
configure: error: ICU library not found
```

**Cause.** Homebrew's `icu4c` is **keg-only** — it is installed under `/opt/homebrew/opt/icu4c@NN/` but *not* linked into pkg-config's default search path. PostgreSQL ≥ 16 builds with ICU by default, so its `configure` runs `pkg-config --exists icu-uc icu-i18n`, finds nothing, and aborts. PG 13–15 build fine without it, which makes the failure look version-specific and mysterious: an init run will succeed for 15, succeed-or-skip 16 depending on history, and die on 17.

**Aggravation.** A failed `cargo pgrx init` for version N is *destructive*: init removes `~/.pgrx/<N>/` and re-unpacks the source tarball before configuring, so a previously working install for that major is clobbered and left as a bare source tree. (Exactly this happened on 2026-06-12: a working 17 install was wiped by an init run that lacked the env var.) The other majors are untouched.

**Fix.** Always run init with `PKG_CONFIG_PATH` pointing at the keg:

```bash
PKG_CONFIG_PATH="$(brew --prefix icu4c)/lib/pkgconfig" \
  cargo pgrx init --pg15 download --pg16 download --pg17 download
```

Notes:

- `brew --prefix icu4c` resolves the versioned keg (`/opt/homebrew/opt/icu4c@78` today), so the command survives ICU major bumps.
- Harmless for PG 15, required for 16/17 (and 18 if it's ever added).
- Re-running init is cheap for already-built versions (it revalidates the existing `pgrx-install` rather than rebuilding) and only compiles what's missing or broken.
- Versions already built on the machine can be re-registered without rebuilding by passing their pg_config path instead of `download`, e.g. `--pg15 ~/.pgrx/15.18/pgrx-install/bin/pg_config`.
- Consider exporting the variable in your shell profile if you re-init often; it has no effect on anything but configure-time library discovery.

## Diagnosing a broken `~/.pgrx`

When the sanity banner reports no usable pg_config, look at the install tree directly:

```bash
ls ~/.pgrx/                                  # one dir per PG version + data-* + config.toml
ls ~/.pgrx/17.*/pgrx-install/bin/pg_config   # missing ⇒ that version never finished building
cat ~/.pgrx/config.toml                      # which majors cargo-pgrx will actually use
```

A version directory that contains `configure`, `src/`, `GNUmakefile.in` etc. but **no `pgrx-install/`** is an interrupted or failed source build. The reason is recorded in its config.log:

```bash
grep "configure: error" ~/.pgrx/17.*/config.log   # e.g. "ICU library not found"
```

If `config.log` shows no error and a `GNUmakefile` exists, the failure was mid-`make` (look for disk space, killed process). Either way the recovery is the same: re-run the init command above for the affected version.

Tier-2a-specific failure signatures (from `scripts/test-http.sh`):

| Error | Meaning | Fix |
|---|---|---|
| `FATAL: database "pg_web_ext" does not exist` | Dev PG is fine; the dev DB was never created on this data dir | Setup step 7 (`createdb` on :28817) |
| `TIMEOUT: :8080 did not open within 15s` | BGW didn't bind. Machine causes: `shared_preload_libraries` missing from `data-17/postgresql.conf`, or the `.so` install failed. **App cause:** the worker itself is crash-looping — the script dumps the dev PG log tail, so read it (e.g. a `FATAL: role … is not permitted to log in` loop is extension code failing at `connect_worker_to_spi`, not a setup problem) | Setup step 6; check `~/.pgrx/17.log` |
| `:8080 is held by PID … not the dev PG's pg_web_worker` | Port shadowed by a leftover container or another app | § Docker side / Port hygiene |

Other state worth knowing:

- `~/.pgrx/data-<major>/` are the **dev-instance** data dirs (used by `cargo pgrx run` and tier 2a). They survive re-init and minor-version rebuilds (same major ⇒ on-disk compatible). The `shared_preload_libraries` line lives here and also survives.
- `#[pg_test]` runs (tier 1) do not touch `data-<major>`; they use their own throwaway cluster.
- The dev PG for pg17 listens on **28817** (pgrx convention: 28800 + major) and its BGW binds **8080**. `test-all.sh` stops the dev PG before tier 4 to free 8080; `scripts/test-http.sh` restarts it on demand.

## Docker side (tiers 2b, 3, 4)

- **Daemon must be running.** Tiers 3 and 4 hard-fail by design when Docker or the image is missing (no silent skip — the image is the shipped artifact).
- **Images involved:**
  - `postgres:16` — pulled automatically by testcontainers for tier 2b's hermetic CLI integration tests.
  - `rtaylor96/pg-web:latest` — the all-in-one runtime image (temporary namespace until the `pgweb/` Docker Hub org lands). Tier 3 boots it via testcontainers on random host ports; tier 4 boots it via `pg-web up` (docker compose) on :5432/:8080.
- **Auto-rebuild:** `test-all.sh` compares the image's created-time against the mtimes of `crates/*/src`, `Dockerfile`, `.dockerignore`, `docker/init-pgweb.sh`, `Cargo.toml`, `Cargo.lock`, and rebuilds when sources are newer. A from-scratch build compiles Rust + the extension inside the container (~10–20 min cold, layer-cached after). Knobs: `REBUILD_IMAGE=1` forces, `SKIP_IMAGE_CHECK=1` skips (bring-your-own-image).
- **Port hygiene.** Tier 4 publishes 5432 + 8080 on the host. Before a run, check for squatters:

  ```bash
  lsof -nP -iTCP:8080 -sTCP:LISTEN; lsof -nP -iTCP:5432 -sTCP:LISTEN
  docker ps --format 'table {{.Names}}\t{{.Image}}\t{{.Ports}}'
  ```

  Usual suspects: a leftover `pg-web up` stack from another app dir (`pg-web down` it), the pgrx dev PG's BGW (`test-all.sh` stops it automatically; manually: `~/.pgrx/17.*/pgrx-install/bin/pg_ctl -D ~/.pgrx/data-17 -m immediate stop`), and orphaned testcontainers from a crashed E2E run (`docker rm -f <name>`; normally ryuk reaps them, but a hard-killed test run can leave one).

## macOS sleep silently freezes long runs (now guarded)

**Symptom.** An unattended `scripts/test-all.sh` run takes 60–90 minutes when the sum of its logged phases is ~20–35; nothing in any log shows a stall or error. On 2026-06-12 a run sat "running" for 78 minutes of wall clock — `pmset -g log` showed the Mac had entered **Maintenance Sleep three times mid-run** (~38 minutes frozen), waking only on keyboard activity.

**Why it's invisible.** Sleep freezes every process *and pauses the monotonic clocks* that cargo/libtest/`Instant::now()` use, so all self-reported durations ("finished in 794.16s") look perfectly normal afterwards. Only wall-clock comparison (or `pmset -g log | grep -E "Entering Sleep|Wake from"`) reveals it. Background jobs launched with `nohup`/`&` hold no power assertion, so the display sleeping is enough to take the whole run down with it.

**Guard.** `scripts/test-all.sh` now re-execs itself under `caffeinate -is` on macOS (no-op on Linux/CI, gated by the `PGWEB_CAFFEINATED` env var to prevent re-exec loops). For other long-running work — `bench/run.sh`, manual `cargo pgrx test` marathons, image builds — wrap them yourself: `caffeinate -is bash bench/run.sh`. To verify the assertion is held: `pmset -g assertions | grep -i caffeinate`.

## Harness-integrity fixes (prompt 025, landed 2026-06-13)

The unconditional `docker compose pull` in `pg-web up` (stack.rs) and the mtime-only staleness check have been fixed:

- `stack.rs:up` now does `docker image inspect` first and only pulls when the image is *absent* locally. Fresh-machine UX is identical (first `pg-web up` or after `docker image rm` still shows the pull progress); subsequent `up`s after a local `test-all.sh` build no longer clobber the tag.
- `smoke-cli.sh` now snapshots the expected image ID at preflight and asserts (hard-fail) after `up` that the running compose stack's postgres container is using exactly that ID. This makes tier 4 a true validator of the artifact under test.
- `ensure_image_fresh` (test-all.sh) keeps the mtime fast-path but now also computes a content hash (`sha256sum` aggregate over the watched inputs + examples/) and compares it to the `pgweb.src_hash` LABEL baked by the build. Rebuilds on hash mismatch (covers git checkout noise, re-tags, published images that lack our label, and content edits that didn't advance mtime).
- `Dockerfile` and `build-image.sh` now pass and embed the hash via BuildKit `--build-arg` + `LABEL pgweb.src_hash=...`.
- `STRICT=1` (auto when `CI` is set) turns any tier failure (including soft 1/2a/3) into a non-zero final exit while still running later tiers for signal. A one-line per-tier status table is always printed at the end.
- Canary preflight + `docker logs` on timeout in both the bash harness (before the 13) and in `wait_for_http` panics (enriched with tail) so a broken worker fails the suite in <90 s with the crash reason visible.
- Tier 2a is now self-healing: `test-http.sh` does an idempotent `createdb` for `pg_web_ext` and will append `shared_preload_libraries` + bounce PG if the line is missing after a fresh `cargo pgrx init`.
- `TEST_TS=1` pipes all script output through a timestamp stamper. Per-tier start/end/dur are always recorded.

The "pull clobber" / "tier4 validated the wrong artifact" hazard is **resolved**. Tier 4 now means "the image we just built/tested is good".

Detection (forensics only; harness no longer needs it):
```bash
docker image inspect rtaylor96/pg-web:latest --format '{{.RepoDigests}} {{.Config.Labels}}'
```

## Environment knobs

| Variable | Default | Effect |
|---|---|---|
| `PG_MAJOR` | `17` | Postgres major for tiers 1/2a (`PG_MAJOR=16 scripts/test-all.sh`). |
| `TEST_IMAGE` / `PGWEB_IMAGE` | `rtaylor96/pg-web:latest` | Image tag used by test-all / build-image (and expected by docker_e2e + smoke). |
| `REBUILD_IMAGE` | unset | `1` ⇒ force image rebuild before tier 3. |
| `SKIP_IMAGE_CHECK` | unset | `1` ⇒ skip the staleness check entirely. |
| `RUN_BENCH` | unset | `1` ⇒ append the 015 benchmark harness (unconstrained + 1 vCPU/2 GiB tiers). Needs clean :5432/:8080. |
| `SMOKE_DIR` | `/tmp/pg-web-smoke` | Tier 4 scaffold dir (wiped each run). |
| `PG_VERSION` | auto-detected | Override the exact PG minor `test-http.sh` targets (it globs `~/.pgrx/<major>.*` and picks the newest). |

## Known flakes and expected failures

- **Tier 4: dev-watcher repush timeout** — the one documented non-blocking flake (see `CLAUDE.md` § Session rituals and prompt 82dfb22). A timeout in the dev-mode watcher repush flow; re-run the smoke if it trips.
- App-level test failures are a different category from this doc's concern: if all five tiers *start*, the machine is configured correctly, and red tests are code work, not setup work.

## CI pipeline notes (GitHub Actions)

The `docs/TESTING.md` § CI integration step list is the skeleton. Corrections and additions from bringing this up in practice:

1. **Linux deps must include ICU**: `libicu-dev` belongs in the apt list alongside `libclang-dev flex bison libreadline-dev zlib1g-dev libssl-dev pkg-config` — same root cause as the macOS gotcha above, same configure error if missing. (On Linux, pkg-config finds the system ICU without extra env once the -dev package is present.)
2. **Run as non-root** — `initdb` (and therefore `cargo pgrx init` and the test clusters) refuses root. Create a user, `chown` the workspace and `~/.pgrx`.
3. **Cache `~/.pgrx`** (~2 GiB; keying on cargo-pgrx version + PG majors). Cold init is the dominant cost (~20–60 min on shared runners); cached it's seconds. Cache `~/.cargo` and `target/` separately.
4. **Init only what the job tests**: `cargo pgrx init --pg17 download` — since 2026-06-12 only the bundled major is a correctness target, so there is no multi-major test matrix. Caveat if CI keeps compile-checking the legacy `pg15`/`pg16` features: pgrx's build script still needs a registered `pg_config` for each feature it compiles, so those checks require initializing the older majors too (~4 min each, cacheable) — or just drop the legacy-feature checks until the flags are removed.
5. **Append `shared_preload_libraries`** to each `~/.pgrx/data-*/postgresql.conf` after init (step 6 above) — needed by tier 2a.
6. **Docker**: `ubuntu-latest` runners have a working daemon; tiers 2b/3/4 work as-is. The image build adds ~10–20 min uncached — cache it via a registry or `docker buildx --cache-to/--cache-from`, or build it once per workflow and share via artifact.
7. **macOS runners are not suitable for the full suite** (no Docker). They can run tiers 1 + 2a only; use a Linux runner for the canonical all-tier job.
8. **Invoke the suite as CI does**: `scripts/test-all.sh` is the single entrypoint; it exits non-zero on hard tier failures. Tier-1/2a pgrx problems print guidance and continue (so one broken leg doesn't mask Docker-tier signal), but CI should treat their `[SKIPPED/FAILED]` lines as failures — grep the log or assert on the final summary line `All tiers completed successfully.`

## Verified configuration record

**2026-06-12 — Robert's MacBook (Apple Silicon), full suite brought back to green-running state.**

| Component | Version |
|---|---|
| macOS | Darwin 25.5.0 (arm64), Xcode CLT clang 21.0.0 |
| rustc / cargo | 1.95.0 |
| cargo-pgrx | 0.18.0 (matches `pgrx = "=0.18.0"`) |
| PostgreSQL under `~/.pgrx` | 15.18, 16.14, 17.10 (all `--enable-debug --enable-cassert`, ICU on) |
| Homebrew | icu4c@78 (keg-only), pkg-config 2.5.1 |
| Docker Desktop | 4.73.0 (engine 29.4.3, linux/arm64) |
| Images | `rtaylor96/pg-web:latest` (auto-rebuilt by test-all), `postgres:16` (testcontainers) |

What was actually wrong on this machine that day (three independent issues, found in order):

1. A `cargo pgrx init` run without `PKG_CONFIG_PATH` had destroyed the working PG 17 install (ICU configure failure, after the version dir was already wiped) → tier 1 never started. Fixed with the init command from § The ICU gotcha.
2. The dev data dir had never had the `pg_web_ext` database created (no `cargo pgrx run` since initdb) → tier 2a died at its psql step. Fixed with `createdb -h localhost -p 28817 pg_web_ext` (setup step 7).
3. An orphaned testcontainer from a crashed earlier E2E run was still holding resources → removed with `docker rm -f`; plus `shared_preload_libraries` appended to the freshly-initdb'd data-15/16.

Verification runs that day (the WIP tree had a known app-level bug — the prompt-014 `pgweb_app` role is created `NOLOGIN` but the BGW connects as it without `BGWORKER_BYPASS_ROLELOGINCHECK`, so the worker crash-loops and `:8080` never opens; those reds are *expected* until that lands):

| Run | Tier 1 | Tier 2a | Tier 2b | Tier 3 | Tier 4 | Wall clock |
|---|---|---|---|---|---|---|
| 1 | ✅ 89 | ❌ missing dev DB (machine — fixed mid-run) | ✅ 150 | ran, 0/13 (app WIP) | ✅ 19 sections* | ~40 min (image rebuild) |
| 2 | ✅ 89 | ran, ❌ :8080 (app WIP — machine fix verified) | ✅ 150 | ran, 0/13 (app WIP) | ✅ 19 sections* | 78 min — **38 min was Maintenance Sleep** (→ caffeinate guard) |
| 3 (caffeinated, per-line timestamps) | ✅ 89 (13 s) | ran, ❌ :8080 (app WIP) | ✅ 150 (<1 s warm) | ran, 0/13 in 794 s (app WIP) | ✅ 19 sections* (7 s) | **13 m 54 s** |

\* Tier 4 "green" in all three runs was against the **Docker Hub image**, not the locally built one — the pull-clobber gotcha above, visible in run 3's timestamped log (`13:37:00` image built → `13:50:15` tier 4 pulls and re-tags from Hub). Treat it accordingly until `stack.rs` is fixed. (Resolved by prompt 025.)

### Post-025 verification (harness hardening run, 2026-06-13)
After the integrity + speed + self-heal + observability changes:

| Run | Tier 1 | Tier 2a | Tier 2b | Tier 3 | Tier 4 | Wall clock | Notes |
|---|---|---|---|---|---|---|---|
| 2026-06-13 hardening (caffeinated, TEST_TS=1, STRICT=1) | ✅ 91 (3 s) | ✅ self-healed (2 s) | ✅ 130 (<1 s) | 12 pass + 1 flake (25 s) | ✅ 19 + integrity assert (8 s) | ~2 min (warm layers) | Full matrix under STRICT; canary passed (no 30 s abort); src_hash + BuildKit caches used; smoke postcondition "using the expected local image ID" asserted; only failure = documented dev_watcher_repushes_on_save (tier 3 E2E, allowed per prompt 024/025). |

Machine-fix takeaway (updated): target warm all-green ≤ 5 min. A deliberately broken-worker tree now fails tier 3 in < 90 s (canary + logs) instead of ~13 min of repeated timeouts. The single-command `scripts/test-all.sh` and `STRICT=1 scripts/test-all.sh` are both required to be green (modulo the documented watcher flake) before claiming prompt 025 complete.

Update the table with real wall times + per-tier after the final acceptance run.
