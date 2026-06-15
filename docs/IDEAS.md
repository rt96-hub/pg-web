# pg-web — Ideas & Future Explorations

This file captures speculative, longer-term, or "nice to have" directions that are not yet scoped into a phase or prompt. They are recorded so good thinking isn't lost when context fades.

Entries are dated and should reference related prompts, roadmap items, and invariants.

---

## Realtime data connections are a core expectation (cross-worker fan-out for DB → all UIs) (2026-06-14)

**Owner direction (2026-06-14):** pg-web apps must be *fully supportive of real time data connections from day one*. If the database is updated (by a handler, a trigger, an external process, `pg-web push`, or anything else that can emit `NOTIFY`), the UI must reflect that change for **all** connected users, no matter which worker serves their HTTP connection or their long-lived SSE stream. The multi-worker concurrency model (SO_REUSEPORT + K BGWs) is required for throughput and head-of-line isolation, *but the design is forbidden from breaking this "everyone sees the update" property*.

This is why the 015 prompt was updated in lockstep: the concurrency work order now carries a hard requirement that the livereload mechanism (and by extension the reusable ListenRouter + per-worker `run_listen_loop`) must deliver events across *all* workers. Postgres's native LISTEN/NOTIFY already does the cross-process broadcast; each worker running its own listener + local in-memory broadcast to the clients that landed on it via the kernel is the concrete path that satisfies the rule without introducing shared memory or a sidecar broker.

**Future refinement (Phase 2 and beyond, explicitly deferred):** the initial carrier can be coarse (full page reload or CSS cache-bust for livereload today). Real app realtime will need to send *only the specific HTML elements / fragments that changed* and must be careful never to overwrite transient client state:
- Do not clobber values in `<input>`, `<textarea>`, or other form controls that the user is actively editing.
- Preserve focus, selection ranges, scroll position, and any in-flight HTMX/JS state.
- Prefer morphing / oob-swap / fine-grained replacement strategies (e.g. Idiomorph or targeted `hx-swap-oob`) over blanket `location.reload()` or wholesale DOM replacement.

The architecture (channel-aware ListenRouter per worker, triggered from DB NOTIFYs, reusable for both framework livereload/cache and app `pgweb_app_*` channels) is the invariant that makes the careful, non-destructive story possible later. The 015 concurrency design must keep this path open and correct under K workers.

**References & related:**
- `prompts/015_concurrency_throughput_and_benchmark.md` (the primary source of truth for the "every worker must listen" rule, the rejection of "K=1 in dev for livereload simplicity", updated current-behavior text, acceptance criteria, research tasks, and open questions around fan-out + macOS parity).
- `docs/ROADMAP.md` (Phase 2: "App-level realtime subscriptions via SSE" reuses the Session-4 ListenRouter; "Handler-side `NOTIFY` helper").
- `docs/BENCHMARKS.md` (records the Step-1 numbers; future re-runs under multi-worker must also exercise cross-worker notify delivery).
- `crates/pg_web_ext/src/{listen_router.rs,livereload.rs,worker.rs}` (the per-BGW listener + ListenRouter; note that the listen task is now always-on, not dev-gated, precisely so cache + realtime work everywhere).
- Current livereload implementation (injected script, `/_pgweb/livereload` SSE, `pg-web dev` post-push `NOTIFY`) is the v1 proof-of-concept for the larger expectation.
- Invariant #4 (one request = one SPI tx) and #7 (async only inside BGWs) still hold; the fan-out work is all inside the worker processes.

**Why this is recorded in IDEAS now:**
The 015 prompt (and the benchmark harness) were originally framed around pure throughput + HOLB. The owner call-out makes the *realtime contract with app developers* a load-bearing constraint on that work. By writing it here and cross-linking into the active prompt, we ensure that future sessions touching workers, the listen loop, SO_REUSEPORT, or SSE paths treat "updates reach everyone" as non-negotiable rather than a nice-to-have that can be satisfied by "just run with K=1 in dev."

When this graduates further it can move into ROADMAP Phase 2 acceptance criteria or a dedicated realtime prompt, but the concurrency foundation must already be right.

*End of entry. The 015 prompt is the place that currently enforces the "multi-worker + cross-worker delivery" pairing.*

---

## CLI-driven Framework Upgrades, Version Pinning, and Configurable Auto-Backup (2026-06-13)

**Owner refinements (2026-06-13):**
- `pg-web up` and `pg-web dev` should **pull the latest (or pinned) version first, then freeze it there** for the lifetime of that local stack. This gives a stable local environment while still making it easy to get updates when you explicitly want them.
- Rollback strategy is deferred ("we can figure that out later").
- The CLI should generally **avoid mutating the user's `docker-compose.yml`** (users own their deployment config). If a dedicated command ever edits config, it must have strong confirmation. In practice it may rarely need to.
- **Every `pg-web` command should eventually be able to target a remote** (via the same connection resolution used by `push`). This enables GitHub Actions, CI, or a laptop to drive backups, upgrades, etc. against a hosted database.
- As much output as possible should be machine-readable (JSON) for AI agents, scripts, and dashboards.
- Basic upgrade methods belong in core. Sophisticated zero-downtime paths are expected to live on a hosted/self-hosted platform. Core stays focused on simple, reliable, restart-based upgrades.
- For "whole project" backups: rely on git/GitHub for compose, Caddyfile, and other system files for now. Long-term goal is to pull as many text files (and even media assets) into the database in a suitably safe way so the full project is rebootable from DB + sources. DB backups can eventually be scheduled and pointed at S3-style storage. When using the SSH/remote path, stream/port the backup back to the local machine.

**Original context:** Explored immediately after completing 018.2 (real ALTER EXTENSION upgrade scripts + policy + test tier). The conversation highlighted the current manual, error-prone DX for moving a deployed app to a new pg-web framework version (`docker compose pull && up -d` + manual `pg_dump` + `ALTER EXTENSION`). Users wanted something closer to the ergonomics of `pg-web push` for content changes, but for the framework/runtime itself.

**Related:**
- 018.2 (extension upgrade path, additive/destructive policy, restart cost documentation)
- 018.1 (health & readiness probes — valuable for post-restart gating)
- ROADMAP Phase 4 items on `pg-web backup` / `pg-web restore`, code-only export, and source-tree-in-DB
- Prompt 021 (remote `--target` / deploy sections in `pgweb.toml`)
- F.2 / F.3 (remote push + CLI bundled in image)
- Current `pgweb.toml` shape (`[server]`, `[database]`, `[dev]`)
- Invariant: Extension ↔ CLI are strictly decoupled (no shared logic beyond the `pgweb.*` tables). Framework upgrades always require a Postgres restart because the HTTP worker is a BGW.

### Vision
Make upgrading the *framework* (the Docker image + extension version) a first-class, CLI-orchestrated operation with safety rails, while keeping the fundamental architectural truth (restart is required) visible and non-magical.

Core commands like `pg-web up` / `pg-web dev` will **pull the appropriate image first (respecting any `[pgweb].version` pin), then freeze that version** for the duration of the local stack. Explicit `pg-web upgrade` (or future remote-targeted commands) is how you intentionally move to a newer framework version.

Users should be able to say, effectively:

> "Pin the pg-web version my app is compatible with. `pg-web up` gives me a stable local environment on that version. When I want to move to a newer one (in prod or locally), I run one command. It will back up my entire world (because everything lives in Postgres), pull the right image, restart safely, run the ALTER EXTENSION, wait for health, and tell me what happened."

"Drop it in" means: existing apps (with their own `docker-compose.yml`, Caddyfile, etc.) can adopt the improved experience with minimal or zero changes to their deployment artifacts. New `pg-web init` apps get the nice defaults automatically.

This is **not** about hiding the restart or making framework upgrades zero-downtime in core (that would violate the model and is expected to be a hosted-platform capability). It is about removing the boilerplate, foot-guns, and tribal knowledge around "how do I safely take a new pg-web release?" while making every command remote-capable.

### Proposed Surface

#### 1. Version Pinning in `pgweb.toml`
```toml
[pgweb]
# The framework version this project is developed/tested against and
# should normally run. The CLI (and future `pg-web up`) can use this
# to select image tags, warn on drift, drive upgrades, etc.
# Accepts exact ("0.3.1") or prefix ("0.3") semantics.
version = "0.3"

# Future: allow different pins per deploy target?
# [pgweb.deploy.prod]
# version = "0.3"
```

This lives alongside the existing `[server]`, `[database]`, `[dev]` sections. Precedent exists for target-specific sections (see deferred prompt 021 `[deploy.<name>]`).

#### 2. New / Enhanced Commands

- `pg-web backup [--out FILE] [--format custom|plain] [--compress]`
  - Thin, opinionated wrapper around `pg_dump`.
  - Resolves connection the same way `pg-web push` does (respects `pgweb.toml` + env + `--url`).
  - Sensible defaults: custom format (fast restore, selective restore), timestamped filename, stored in `./backups/` (created if needed, added to `.gitignore` by `init`).
  - Returns a portable artifact that contains *everything* (schema + user data + all `pgweb.*` framework state + assets).
  - This is the operational backup already called for in ROADMAP Phase 4; we just make the framework upgrade flow a heavy consumer of it.

- `pg-web upgrade [options]`
  Options (all optional, sensible defaults):
  - `--version <ver>` — target a specific version (overrides pin).
  - `--target <name>` — future remote target (builds on F.2).
  - `--dry-run` — show the plan (what would be backed up, which image tag, what ALTER would do) without doing it.
  - `--yes` / `-y` — skip confirmation prompts.
  - `--no-backup` — explicit opt-out for this run (even if config says to backup).
  - `--backup-only` — just do the backup step (useful for cron or pre-release rituals).

  Behavior outline (core path):
  1. Resolve desired version (CLI flag > `pgweb.toml` `[pgweb].version` > "latest" with strong warning). Remote targeting is supported for every command.
  2. Discover current running version (via `pgweb.ext_version()`).
  3. If same as desired → no-op (or still offer to re-apply latest scripts for safety).
  4. If backup is configured for upgrades (or `--backup` flag), run `pg-web backup` (streams back to local when run over SSH/remote path) with a name that includes "pre-upgrade-to-<ver>" and the current timestamp. Output is available in both human and JSON form.
  5. The CLI does **not** mutate the user's `docker-compose.yml` in the normal case. It prints the exact commands the user (or their CI) can run, or offers an explicit "apply suggested compose changes" step with confirmation when useful.
  6. Execute the equivalent of `docker compose pull` (for the specific tag) + `docker compose up -d` (user or orchestrator runs this; CLI can drive it when it has the necessary context).
  7. Wait for the protected readiness probe (`/_pgweb/readiness` from 018.1) with a generous but bounded timeout + rich diagnostics (container logs on failure). JSON output available.
  8. Run `ALTER EXTENSION pg_web_ext UPDATE;` (or `UPDATE TO 'the-ver';`) over the resolved connection (works locally or remotely).
  9. Re-check health + a couple of user routes.
  10. Record the upgrade event (echo + machine-readable). Suggest tagging in git or noting in the deployments ledger.
  11. On any failure after the backup step: clearly tell the user "you have backup X, here is the exact `pg_restore` + old image tag command to roll back". Rollback UX details are deferred.

  All major steps produce structured JSON output where it makes sense (for scripts, AI agents, and dashboards).

- `pg-web version` (or enhance existing output)
  - Shows: CLI version, desired framework version (from toml), currently running framework version (from DB), image tag in use (best effort), last upgrade timestamp if we track it.

#### 3. Configurability ("drop it in")

Add to `pgweb.toml`:

```toml
[backup]
# Where to put operational backups (pg_dump of the whole DB).
path = "backups"           # relative to project root; created on demand
retention = 10             # keep the last N timestamped backups (simple policy)
format = "custom"          # or "plain"
compress = true

[backup.before]
framework_upgrade = true   # auto-backup before pg-web upgrade / framework-affecting operations
# future: before_destructive_migration = true, etc.

[pgweb]
version = "0.3"
# upgrade_policy = "prompt"   # or "auto" (very brave), "never"
```

- `pg-web init` (and `--template todo`) can:
  - Add the `[pgweb]` and `[backup]` sections with good defaults.
  - Create a `backups/` directory + update `.gitignore`.
  - Add helpful comments in the generated `docker-compose.yml` about the upgrade flow.
  - Possibly scaffold a tiny `scripts/upgrade.sh` wrapper that calls `pg-web upgrade --yes` for CI or cron use.

This is the "drop it in" part: an existing project can run `pg-web check` (future enhancement) or just manually add the sections; new projects get it for free.

#### Pre / Post Hook Scripts — What Might People Actually Use Them For?

People commonly want hooks around significant operations like framework upgrades or big backups. Realistic examples:

**Pre-upgrade / pre-backup hooks**
- Send a Slack/Discord/Teams notification ("Starting pg-web framework upgrade to 0.3 on prod").
- Put the app into maintenance mode (flip a `pgweb.settings` key or touch a file that a handler respects to return 503s with a nice message).
- Run `pg-web check` or a custom lint as a last-minute gate.
- Snapshot a specific table or run `ANALYZE` for faster restore later.
- Temporarily increase `request_timeout` or other settings for the duration of the upgrade.
- Trigger an external backup of volumes that live outside the DB (e.g. Caddy data dir) if the user still keeps some state there.

**Post-upgrade / post-backup / post-cutover hooks**
- Send success/failure notifications with links to the backup artifact and the upgrade log.
- Warm up caches or run a quick smoke test suite against the new instance.
- Flip maintenance mode off.
- Update an external status page or Datadog monitor.
- Re-index search tables or run data-quality checks that are expensive to do on every request.
- For blue/green cutover scenarios: after traffic is fully on green, trigger a "old instance decommission" job or scale-down.
- Log the event into the project's own audit table or an external system.

Hooks should be simple executable scripts (or commands listed in `pgweb.toml`) that the CLI invokes at well-defined points and whose exit code can influence whether the operation continues. They are "nice to have" and should be optional. Core upgrade/backup commands must remain useful without any hooks.

For now this is speculative — we can start with just printing "consider adding a pre/post hook here" messages and add real hook support only when real users ask for it.

#### 4. Safety & Observability Hooks
- Always require explicit confirmation for anything that will restart Postgres, unless `--yes`.
- Use the protected `/_pgweb/readiness` and `/_pgweb/health` (018.1) to decide when the new container is actually serving.
- Surface the upgrade scripts being used (or at least the from→to transition) so users see *what* is about to run.
- Record the backup path and the ALTER statement that was executed in the output (copy-pastable for auditing).
- Future: integrate with the deployments ledger or a new `pgweb.framework_upgrades` table (written by the extension on successful ALTER? or just by the CLI).

### Challenges & Architectural Tension (updated with owner guidance)

- **Restart is not optional in core.** Any design must make the restart cost, duration, and blast radius extremely visible. We do not pretend this is like a rolling web deploy. Zero-downtime framework upgrades are acknowledged as likely requiring a hosted platform / custom orchestrator that we control.
- **Remote-everywhere is a first-class goal.** Every command (`backup`, `upgrade`, `push`, `migrate`, `check`, etc.) must be able to target a remote DB. This is how CI, GitHub Actions, and "laptop against prod" will work. The SSH-tunneled remote path should "just work" for driving the full lifecycle.
- **Decoupling invariant.** The CLI must not start depending on extension internals. It can read `pgweb.ext_version()`, run `ALTER EXTENSION`, and do `pg_dump` / backups, but the actual migration logic lives in the shipped upgrade `.sql` files.
- **Compose / deployment config ownership.** The CLI should generally avoid mutating the user's `docker-compose.yml`, `Caddyfile`, etc. It prints the exact commands. Explicit "apply these config suggestions" flows can exist with strong confirmation if we ever need them.
- **Core vs Platform split.** Provide basic, reliable, restart-based upgrade + backup methods in the open-source core. Advanced zero-downtime (blue/green + data delta replay + traffic cutover) is out of reach for core without a controlled hosted/self-hosted platform. We will gain the experience and users needed before building the sophisticated path.
- **Data delta / CDC for zero-downtime.** Deferred for now. Can be handled on a separate system later (DB diff, request logging + imputation, or future realtime machinery). Not required for the basic core upgrade story.
- **Version "properness".** How do we know the image tag that corresponds to a given `pgweb` version? Today tags are `0.3.0` + `latest`. We may need a convention (or a small manifest the CLI can fetch).
- **Old images / very old upgrade scripts.** Don't worry about them (only the owner has them on toy apps).
- **Whole-project backup completeness.** For now, lean on git for compose/Caddy/other system files. Long-term aspiration: pull as much as safely possible (text files + assets) into the DB so a restore + sources is sufficient. Don't over-engineer S3 offload or volume backups yet.

### Open Questions (refreshed with owner feedback)

**Core / local dev behavior**
- How should `pg-web up` / `pg-web dev` surface the "we just pulled version X and are now frozen on it" information to the user? (Nice log message + `pg-web version` output?)
- Should there eventually be an explicit `pg-web freeze` / `pg-web unfreeze` or `pg-web pull-latest-framework` command, or is "delete the containers + `pg-web up` again" sufficient?

**Remote-everywhere**
- What is the minimal set of commands that must work remotely first (`backup`, `upgrade`/`ALTER`, version check, `push`, `migrate`)? 
- How do we handle cases where a command needs local filesystem (e.g. reading a compose file or writing a backup locally when targeting remote) vs. when everything can stay on the remote side?

**Backup strategy & "whole project"**
- For long-term S3-style offload of DB backups: do we add a small `--remote` / `--s3` flag to `pg-web backup`, or keep the CLI focused on producing the dump and let users pipe it (`pg-web backup --format=custom | aws s3 cp - s3://...`)?
- What is the safe, incremental path for pulling more files into the database (compose, Caddyfile, other text config, then later media/assets)? How do we avoid pulling secrets or very large binary blobs that belong elsewhere?
- Should `pg-web backup` (when run over SSH/remote) automatically stream the dump back to the invoking machine by default, or require an explicit flag?

**Pre / post hooks**
- What is the minimal useful hook surface? (Just `pre_upgrade`, `post_upgrade`, `pre_backup`, `post_backup`? Exit code affects continuation?)
- How are hooks declared? Inline in `pgweb.toml`, paths to scripts in the project tree, or both?
- Should hook scripts receive structured context (JSON on stdin or env vars) about the current version, target version, backup path, etc.?

**Compose / deployment config mutation**
- Are there any commands where we *strongly* want the CLI to offer to edit `docker-compose.yml` or `Caddyfile` (e.g. "add a new volume for backups", "update the image tag after an upgrade")? If so, what does the confirmation UX look like?
- Or do we stay purely in "print the diff + the exact sed / manual edit instructions" mode forever?

**Zero-downtime / blue-green / hosted platform**
- High-level speculation only (for now): when we do build a no-downtime framework upgrade path as a hosted or self-hosted platform service, what are the minimal primitives the core must expose to make that platform possible? (Reliable `pgweb.ext_version()`, clean backup/restore, ability to run `ALTER EXTENSION` on a fresh instance, health/readiness probes, remote commandability, etc.)
- How would a platform-orchestrated cutover actually work at a high level? (Backup → bring up green on new version → some form of data catch-up → traffic flip via Caddy or external LB → decommission blue.) What does the user experience look like from the outside?
- Is there a useful "poor man's" zero-downtime story we could offer in core without full dual-running (e.g. using read replicas if the user has them, or a short maintenance window + very fast restore)?

**Machine-readable output & AI / automation friendliness**
- Which commands should prioritize rich JSON output (`--json` flag or always when stdout is not a tty)?
- Should we have a `pg-web plan upgrade` (or similar) that emits a machine-readable plan without side effects?
- How do we make the experience excellent for agents (good structured errors, clear next-step suggestions, stable command surfaces)?

**General / deferred**
- Rollback UX (simple path and any future blue/green path) — figure out later once we have real usage.
- Data delta / CDC mechanism for advanced cutovers — can be invented on a separate system later if needed.
- Testing strategy for platform-level features — will be addressed when we have more experience and users (and a self-hosted platform to test against).
- Old pre-018.2 images with no upgrade scripts — not a concern (owner-only toy apps).

### Why This Is Worth Capturing Now

- 018.2 removed the biggest lie in the deployment story ("just run ALTER EXTENSION, the script will be there").
- The existence of `pgweb.ext_version()`, protected health probes, the upgrade test tier, and the bundled CLI give us real primitives.
- Remote-everywhere + JSON output makes the CLI automation- and AI-friendly from day one.
- Basic core upgrades + the clear "advanced zero-downtime lives on a hosted platform" split keeps the open-source project focused while still giving a credible path for users who need more.
- The "whole project in the database" aspiration (with pragmatic reliance on git for system files today) is a natural evolution of the existing "one dump = everything" philosophy.

This direction respects the core invariants (especially restart cost and CLI/extension decoupling) while dramatically improving the "I just want to take the latest improvements" story and making remote/CI-driven operations first-class.

### Expanded "Whole Project" Snapshots (storing the entire thing)

The strong "everything lives in Postgres" thesis (one `pg_dump` = whole app) is already one of pg-web's most compelling properties. We can push it much further toward a complete, self-contained replica of a project.

Current/future building blocks (see ROADMAP Phase 4):
- Operational backup (`pg-web backup`): full `pg_dump` capturing schema + all user data + every `pgweb.*` framework table (routes, templates, assets, deployments, settings, secrets via the accessor, etc.) + any extension-created objects.
- Code-only export: just the app surface (routes + templates + assets + handler functions + non-reserved settings).
- Source-tree-in-DB (`pgweb.sources`): mirror the working tree (and optionally full `.git/` objects + refs) into the database during/after `pg-web push` (or via a dedicated `pg-web sync-source` step). A `pg_dump` then contains the *full source history*.

**Anything else worth capturing to "store the whole thing"?**

- Deployment surface: the exact `docker-compose.yml`, `Caddyfile`, any custom `docker-entrypoint-initdb.d` scripts, `.env` templates, `nginx.conf` or other reverse-proxy files the user maintains.
- Project metadata at backup time: the pinned `[pgweb].version`, the exact Docker image digest used, list of applied migrations + their checksums (if we add them), current `pgweb.ext_version()`, health/readiness flag states.
- Non-secret configuration and scaffolding produced by `pg-web init` (README, `.gitignore`, any example scripts).
- In the far future: encrypted blobs for secrets (if users opt in), volume snapshots references (for the Caddy data/config volumes), or even a manifest of "sidecar" services the user runs alongside.
- Audit trail: the full `pgweb.deployments` ledger + any future request log / framework upgrade history.

Ideal outcome: from a single backup artifact (or a small set of artifacts) + the `pgweb.sources` data, you can:
1. `pg_restore` into a fresh Postgres.
2. Restore the source tree (via a `pg-web restore-sources` or direct table export).
3. `docker compose up`.
4. Have a byte-for-byte identical (or fully auditable) running replica, including git blame history for every handler and template.

This turns "pg_dump" into a true "project export" that is runnable anywhere a pg-web image exists. It also makes blue/green or disaster-recovery scenarios much stronger.

Storage cost and sync strategy for the git history part remain the big open questions already noted in ROADMAP (working tree only vs. full packfiles? lazy vs. eager mirroring during push?).

### Zero-Downtime Framework Upgrade Paths (as an Addon or Self-Hosted Service)

The fundamental limit documented in 018.2 and `docs/DEPLOYMENT.md` is real: because the HTTP worker *is* a Postgres background worker, replacing the `.so` (i.e., taking a new pg-web framework version) requires restarting the Postgres postmaster in that container. `ALTER EXTENSION` can migrate the schema safely for additive changes, but the process itself involves downtime for the web tier.

A higher-level "no-downtime framework upgrade" story could exist as an *optional addon*, companion tool, or even a small self-hosted "pg-web ops" product:

High-level flow (inspired by the blue/green idea):

1. Take a full "whole project" backup (operational dump + sources + deployment artifacts + metadata, using the mechanisms above).
2. Instantiate an *equivalent green environment* on the *new* pg-web version:
   - New container(s) running the target image tag.
   - Fresh DB (or a new schema / separate database on a larger instance) restored from the backup.
   - Full source tree restored so handlers, templates, and assets are identical.
   - The green side performs its own `ALTER EXTENSION` (or starts fresh on the new version's install SQL).
3. Capture the *delta* of new/changed data that arrived on the blue (old) side since the backup cutoff point. This is the technically interesting part:
   - Could be built on future realtime subscriptions (Phase 2), logical decoding / replication slots, a lightweight change-data-capture table that handlers write to, or even a temporary dual-write mode during the cutover window.
   - Re-apply the delta to the green DB (idempotently where possible).
4. Once green is warm, consistent, and passing health/readiness probes, perform an atomic traffic cutover:
   - Update the Caddyfile (or external LB / DNS) to point at the green instance.
   - Caddy reload (or equivalent) — very fast for most setups.
5. Optional soak period on green, then decommission blue (or keep it for instant rollback).
6. Record the cutover as a special "framework upgrade event" (ties into the deployments ledger or a new table).

**Why this is a "nice addon" rather than core open-source Phase 1/2 work:**
- It requires running two full app instances (2× resources during transition).
- Data diff/replay logic is non-trivial and would benefit from Phase 2 realtime + job queue primitives.
- Traffic redirection lives in the user's reverse proxy / LB layer (Caddy today).
- It turns the "single DB is the source of truth" model into a temporarily dual-DB model for the duration of the upgrade.
- Safety, split-brain prevention on writes, session/cookie continuity (Phase 2), livereload channel migration, etc. add real complexity.
- Many users will be perfectly happy with the documented "backup + restart + ALTER" path, especially on small-to-medium VPSes.

**Possible shapes for delivery:**
- A `pg-web blue-green` or `pg-web cutover` subcommand (future extended CLI).
- A small orchestrator container / sidecar the user can `docker compose` up that coordinates the two sides.
- A self-hosted "pg-web platform" service (the commercial / "pro" angle) that manages instance fleets, backups, version pins, and cutovers for multiple apps.
- Integration with external tools (Patroni, CloudNativePG, or simple compose profiles) for the DB side.

This pairs *extremely* well with the full project-in-DB snapshot work: the backup + sources gives you a perfect, auditable seed for the green side.

It also gives a credible answer to "how do I ever do zero-downtime framework upgrades?" without claiming the core single-worker model magically supports it.

### Updated Open Questions (incorporating full-project snapshots and no-downtime cutovers)

1. Should `pg-web up` / `pg-web dev` (the local stack commands) also honor the `[pgweb].version` pin and automatically pull the right image, or is that surprising?
2. Rollback UX: `pg-web rollback --to <backup-file> --image-tag <old>` (for the simple restart path) or a full "flip traffic back to previous blue" for the blue/green addon?
3. How much should the CLI mutate the user's compose file vs. always being a "driver" that prints the right `docker compose` (or orchestrator) invocation?
4. Remote targets: once F.2 / deploy sections land, can `pg-web upgrade --target prod` drive the backup (over the tunnel), tell the remote host what image to pull, and orchestrate the ALTER (or full blue/green sequence)?
5. Should there be a machine-readable "upgrade plan" output (JSON) for CI / dashboards / the future no-downtime orchestrator?
6. Backup storage & "whole project" artifacts: local disk first for operational dumps. How do we version, compress, and transport the combination of dump + `pgweb.sources` + compose/Caddy files? Later support for S3 / object storage via a small plugin or documented wrapper?
7. Pre/post hooks: allow `pre_framework_upgrade` and `post_framework_upgrade` (or `pre_cutover` / `post_cutover`) scripts in the project? Or keep the core simple and let users wrap the commands?
8. How do we handle the case where the user has *not* adopted the new upgrade scripts yet (very old image)? The CLI (and any blue/green orchestrator) can detect the absence of `pgweb.ext_version()` or old extension version and give tailored advice or refuse the advanced path.
9. Data delta / CDC strategy for no-downtime cutovers: rely on future Phase 2 realtime subscriptions + a replay log? Logical replication slots on the blue DB? A lightweight "write journal" table that every handler (or a framework wrapper) appends to? How do we make the delta capture reliable without adding unacceptable latency or storage cost on the primary path?
10. Resource & consistency model for blue/green: do we require a completely separate DB for green, or can we use the same Postgres cluster with a second database/schema + careful cutover of the `pgweb` namespace? What happens to in-flight long-poll/SSE connections, cookies (Phase 2), and any in-memory BGW state (livereload channels, etc.) during the traffic flip?
11. Traffic director requirements: the story assumes the user (or the addon) can atomically repoint Caddy (or an external LB) at the new instance. How much of this should pg-web itself try to automate vs. just document the Caddyfile swap + reload pattern?
12. Commercial / self-hosted service boundary: which pieces (full snapshots, blue/green orchestration, version fleet management, automated delta replay) feel like they belong in core open-source vs. a paid "pg-web ops" companion that users can self-host or subscribe to?
13. Testing surface for the advanced paths: the existing 018.2 self-upgrade smoke (single instance) is great. How do we exercise the dual-instance + delta replay + cutover flow in the harness without making Tier 3 take 10× longer or requiring multiple coordinated containers per test?

### Why These Directions Are Worth Capturing Now

- 018.2 gave us trustworthy in-place schema migration for the simple path.
- The "whole project" snapshot vision (already partially in ROADMAP) becomes dramatically more powerful when combined with framework version pins and the ability to instantiate clean "green" replicas.
- The no-downtime cutover idea directly addresses the most common objection to the architecture ("but what if I need to upgrade the framework without downtime?"). Recording it as an explicit addon/service keeps the core model honest while still offering an escape hatch for users who need it.
- These ideas naturally pull forward work on backup/restore, sources mirroring, health/readiness gating, and remote targets — they act as forcing functions for the rest of the ops story.

---

*End of entry. When this graduates to a real work order, turn the strongest parts into a numbered prompt (with the usual "before you start, run the full test bookends" gate). The blue/green + full snapshot directions are especially good candidates for a dedicated future prompt once Phase 2 realtime primitives exist.*
