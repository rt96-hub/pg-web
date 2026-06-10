# 012 — Deploying updates to the live pg-web.dev documentation site

**Status:** Handoff prompt — detailed runbook  
**Priority:** High (this is the ongoing "how we keep the dogfooded site fresh" process)  
**Context:** The `site/` directory is a real pg-web application. It was initially stood up by prompt 008 and is deployed on Hetzner using the reusable bootstrap script in `site/scripts/bootstrap-hetzner.sh`. The owner (or future maintainers) will regularly edit content, structure, or assets in `site/`. This prompt documents the exact, reliable process for getting those changes live without drama.

This is the **detailed, repeatable runbook**. It should be the first thing someone looks at when they are about to push a content change to https://pg-web.dev.

## Read These First (Every Time)

- `site/README.md` — the operational bible for this specific app.
- `docs/APP-LAYOUT.md` — directory = route, filename = method, etc.
- `CLAUDE.md` — especially the companion-app rule and "test changes the way a normal person would."
- `site/scripts/bootstrap-hetzner.sh` — the current canonical way to stand up a similar environment.
- The current compose + Caddyfile in `site/` (they have the production mount for the in-container CLI).

## The Two Environments

1. **Local development** (your laptop)
   - Full `pg-web dev` or manual `up` + `push` against a local Docker stack.
   - Fast feedback, live reload, `pg-web check`.

2. **Production** (the Hetzner VPS at `/opt/pg-web/site`)
   - The real `pgweb/postgres:latest` container (or a locally built one if the published image is not yet available).
   - Source code lives on the host at `/opt/pg-web/site`.
   - We use a bind mount (`.:/app:ro`) + `working_dir` so the in-image `pg-web` binary can see the files.
   - Caddy is in front for TLS + reverse proxy.
   - Updates are done by pulling the repo on the box and running the in-container CLI.

Never edit files directly on the VPS for content work. Always go through git.

## Local Development Workflow (Do This First)

```bash
cd site

# Preferred (fast iteration + browser live-reload)
../target/debug/pg-web dev
# or, once published: pg-web dev

# Alternative explicit flow
../target/debug/pg-web up
../target/debug/pg-web migrate apply   # usually a no-op for content-only changes
../target/debug/pg-web push

# Always run this before committing content changes
../target/debug/pg-web check
```

- Visit http://localhost:8080 (or whatever port you have).
- Use incognito + hard refresh when testing styling or redirects.
- The home page and other routes should render exactly as they will on the live site (modulo the `env=development` error page vs production).

Make small, reviewable changes. Run `pg-web check`. Commit.

## Committing & Pushing Changes

Standard git flow:

```bash
git add site/   # or the specific files you touched
git commit -m "docs(site): short description of the content change"
git push
```

The live site is just another consumer of the repo. No special branch or deploy tag is required for content (unlike framework releases).

## Deploying to Production (the Hetzner box)

This is the reliable command sequence as of the end of the initial deployment work.

1. SSH to the box (you have `ssh hetzner` or equivalent configured).

2. Pull the latest code:

   ```bash
   cd /opt/pg-web/site
   git pull --ff-only
   ```

3. (Optional but recommended) Quick local sanity check inside the container:

   ```bash
   docker compose exec postgres pg-web check || true
   ```

4. Deploy the new routes/templates/assets:

   ```bash
   docker compose exec postgres sh -c "cd /app && pg-web push --with-migrate"
   ```

   The `sh -c "cd /app && ..."` (or `-w /app`) is required because the source is mounted at `/app` inside the container. The bind mount was added precisely so the in-image CLI can see `pages/`, `public/`, and `pgweb.toml`.

5. Verify:

   ```bash
   docker compose ps
   # Quick content check from inside the box
   curl -s -H "Host: pg-web.dev" http://localhost:8080/ | head -c 500
   ```

6. From your own machine (or any external machine), confirm:

   ```bash
   curl -I -H "Host: pg-web.dev" http://$(dig pg-web.dev +short)/
   # Expect 308 redirect to HTTPS

   curl -k -I https://pg-web.dev
   # Expect 200 once the cert is happy

   # Or just open https://pg-web.dev in an incognito window
   ```

Caddy will pick up the new static assets and routes on the next request. No container restart is normally needed for pure content changes.

## When You Do Need to Touch Infrastructure Files

If you changed `docker-compose.yml`, `Caddyfile`, `.env`, or anything that affects the container runtime:

- After `git pull` on the box, run:

  ```bash
  docker compose up -d
  ```

- Then do the `push` step above.

- If you changed the bind mount or working directory, you may need to recreate the postgres container:

  ```bash
  docker compose up -d --force-recreate postgres
  ```

Rebuilding the entire `pgweb/postgres` image is almost never required for docs-site work. Only do it if you are deliberately testing a new version of the framework image.

## Updating the Reusable Bootstrap Script

If the deploy flow ever changes (new reliable push command, different mount strategy, new way to run `pg-web check` inside the container, etc.), update `site/scripts/bootstrap-hetzner.sh` as part of the same PR or in a follow-up.

The script is the "how to stand up a fresh copy of this environment" artifact. Keep it in sync with reality.

## Common Gotchas & Tips

- **Browser cache / HSTS / service worker weirdness**: Use incognito + hard refresh (`Cmd+Shift+R` or `Ctrl+Shift+R`). This was the main reason the site "didn't work" in a normal browser window right after the initial DNS flip.
- **308 redirects**: Caddy is intentionally redirecting HTTP → HTTPS. Test the redirect explicitly if something feels off.
- **Cert issuance timing**: On a brand new domain or after long periods, the first request can trigger ACME. Subsequent deploys are instant.
- **"no pages/ directory" error on push**: You forgot the `cd /app` (or `-w /app`) when exec'ing into the container. Use the exact command in the "Deploying to Production" section.
- **Local vs prod env**: `pg-web dev` forces development mode (rich error pages + live reload injection). The live site should be in `production` (see `pgweb.toml` on the box). This is intentional.
- **Assets with content hashing**: In production the push step rewrites references for immutable caching. Test both a normal browser and a hard refresh after deploying CSS/JS/image changes.
- **Small changes are better**: Deploy, look at the live site, iterate. The whole round-trip (edit → local test → git push → Hetzner pull + push) is fast once you're used to it.

## Future Improvements (Parking Lot for This Prompt)

- Make the in-container push command even shorter (alias, Makefile target, or a tiny wrapper script committed in `site/`).
- Add a `site/deploy.sh` or similar that can be run on the box with one command.
- Wire up a GitHub Action that does the pull + push on the VPS (via SSH or the in-image CLI) on pushes to main that touch `site/`. (Only after we have solid secrets / deploy keys.)
- Make the bootstrap script also able to do a "deploy only" mode on an existing box.
- Surface a small "last deployed" or commit SHA on the live site itself (trivial dynamic handler).

## References & Artifacts

- `site/README.md` — the short version of "how to develop and deploy this site".
- `site/scripts/bootstrap-hetzner.sh` — the current reusable stand-up script.
- Prompt 008 — how the site was originally created.
- Prompt 011 — the stub for the actual content changes the owner wants to make.
- `docs/DEPLOYMENT.md` — the general "how normal people deploy pg-web apps" guide (the docs site should stay consistent with it).
- Hetzner box path: `/opt/pg-web/site`
- Current reliable production push (as of creation of this prompt):

  ```bash
  ssh hetzner "cd /opt/pg-web/site && docker compose exec postgres sh -c \"cd /app && pg-web push --with-migrate\""
  ```

## Tone & Process Notes

- Treat the live site like a real production app that happens to be documentation.
- Always test locally first. The only surprise on the VPS should be "it worked exactly like on my laptop."
- Keep the deploy process boring and copy-pasteable. If it requires remembering three different flags or a special incantation, improve the script or the docs.
- Update this prompt (or the site's README) the moment the real deploy flow changes.

When you have made content changes via prompt 011 (or ad-hoc) and successfully deployed them using the process above, you can mark the relevant work as complete and note the date here.

**End of prompt 012.** This is the living deploy runbook for the dogfooded docs site. Keep it accurate.