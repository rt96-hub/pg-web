# Session 5 — User validation playbook

What to manually try after each component lands so you can satisfy
yourself the framework behaves as designed. Component sections are
self-contained: you can run any one alone after a fresh `pg-web up`.

Rough setup baseline assumed: the Docker stack has been bootstrapped
once (`pg-web up` + `pg-web migrate apply` + `pg-web push` against
`examples/todo/`). All commands are run from the `pgweb` user inside
WSL2 unless noted.

---

## L. Push retry on serialization conflict

### What changed

- Every CLI subcommand that opens a Postgres connection now tags it via
  `application_name = 'pg-web {verb} (pid={pid}, host={host})'`. Visible
  in `pg_stat_activity.application_name`.
- `pg-web push`'s transaction body is wrapped in a 3-attempt jittered
  retry. Triggered by SQLSTATE 40001 (serialization failure) or the
  literal `tuple concurrently updated` message that concurrent DDL
  raises (XX000 internal error).
- On retry exhaustion, push opens a fresh diagnostic connection,
  queries `pg_stat_activity` for sibling `pg-web *` clients, and
  attaches a per-row "stop with: kill PID" (same host) or
  `pg_terminate_backend(PID)` (remote host) suggestion to the error.

### Verify in `pg_stat_activity`

While `pg-web dev` is running:

```sql
SELECT pid, application_name, state
FROM pg_stat_activity
WHERE application_name LIKE 'pg-web %'
ORDER BY backend_start;
```

Expected: at least one row, `application_name` like
`pg-web dev (pid=<NNNN>, host=<your-hostname>)`. The `pid` here is the
**OS pid of your `pg-web dev` process**, not the Postgres backend pid.
That's the actionable target the diagnostic suggests for `kill`.

### Verify the retry path under contention

Open two terminals against the same DB and run two pushes back-to-back:

```bash
# Terminal A — keep it pushing in a tight loop
cd ~/pg-web/examples/todo
while true; do ../../target/debug/pg-web push; done

# Terminal B — same loop, same app
cd ~/pg-web/examples/todo
while true; do ../../target/debug/pg-web push; done
```

Both should keep committing — neither should error out with
`tuple concurrently updated`. Watch `pgweb.deployments`:

```sql
SELECT count(*), max(pushed_at)
FROM pgweb.deployments
WHERE pushed_at > now() - interval '30 seconds';
```

Expected: count grows monotonically, no aborted pushes (each successful
push lands a row).

### Verify the diagnostic on retry exhaustion

Force exhaustion by sleeping inside an interactive transaction that
holds a row lock on `pgweb.routes`, then try a push from a second
terminal. The push will retry 3× and fail with the diagnostic.

Expected error tail:

```
Error: ...
Caused by:
   0: push retried 3 times against concurrent DDL
   1: concurrent `pg-web` connections detected. Stop these to clear the conflict:
        - pg-web dev (pid=12345, host=mymachine) (backend pid 67890) — same host; stop with: kill 12345
   2: tuple concurrently updated
```

If the racing process's `application_name` came in unrecognized format
(e.g. someone connected via `psql`), the diag falls back to:

```
        - psql (backend pid 67890) — unrecognized format; stop with: SELECT pg_terminate_backend(67890); from psql
```

### Sanity: no regressions

- `examples/todo/` still works end-to-end: `pg-web up` → curl `/`
  shows the empty-state, POST a todo, refresh, see the row.
- Single-pusher `pg-web push` is unchanged in latency (the retry
  wrapper is a no-op on the happy path; one tx, one commit).
- `pg-web env list`, `pg-web check`, `pg-web migrate apply` all still
  connect (now via `db::connect`) and work as before.

### Known nuances

- A leftover `pg-web up`-managed container shadowing `:8080` is now
  much easier to spot — `pg_stat_activity` shows the in-container
  worker's connections too. The fix is `docker stop <container>` or
  `pg-web down` from the original app dir.
- The retry helper is only on `pg-web push`. `pg-web migrate apply`
  uses one tx per migration file and isn't typically a DDL-race target;
  no retry there.

---

## F.3. CLI bundled in `pgweb/postgres:latest`

### What changed

- `Dockerfile` builder stage now runs `cargo build --release -p pg_web_cli`
  after `cargo pgrx install`, and the runtime stage copies the binary
  to `/usr/local/bin/pg-web`.
- `.dockerignore` excludes `examples/*` by default but un-ignores
  `examples/todo/` because the CLI's `init.rs` baked it in via
  `include_dir!`. Without it, the CLI build's proc-macro panics at
  build time.

### Verify the binary is in the image

```bash
docker run --rm --entrypoint=/bin/bash pgweb/postgres:latest \
    -c 'pg-web --version'
```

Expected: `pg-web 0.1.0`.

### Verify a push from inside the container

Set up a fresh container with the demo bind-mounted:

```bash
cd ~/pg-web
docker run --rm \
    --name pgw-f3 \
    -e POSTGRES_PASSWORD=testpw \
    -e POSTGRES_DB=app \
    -v $(realpath examples/todo):/app:ro \
    -p 8080:8080 -p 5432:5432 \
    pgweb/postgres:latest &
```

Wait for it to boot (`docker logs pgw-f3 | grep ready`), then run the
deploy from inside:

```bash
docker exec pgw-f3 pg-web migrate apply --dir /app \
    --url postgres://postgres:testpw@127.0.0.1:5432/app
docker exec pgw-f3 pg-web push --dir /app \
    --url postgres://postgres:testpw@127.0.0.1:5432/app
curl -s http://localhost:8080/ | grep -c "No todos yet"
```

Expected: each `docker exec` exits 0; the curl returns 1 (matches the
empty-state line).

### Verify the in-image push lands a sensible deployment ledger row

```bash
docker exec pgw-f3 psql -U postgres -d app \
    -c "SELECT from_host, file_count, migrations_applied FROM pgweb.deployments ORDER BY pushed_at DESC LIMIT 1"
```

Expected: `from_host` is the container's hostname (a 12-character hex
prefix matching `docker inspect pgw-f3 --format '{{.Config.Hostname}}'`),
NOT the dev box's hostname. That's the F.3 value prop in action: the
push ran inside the compose network without the dev box being involved.

Cleanup:

```bash
docker rm -f pgw-f3
```

### Sanity: the CLI from the host still works against the same container

After the in-image push above, run from your normal shell:

```bash
~/pg-web/target/debug/pg-web push --dir ~/pg-web/examples/todo \
    --url postgres://postgres:testpw@127.0.0.1:5432/app
```

Expected: succeeds, lands a second ledger row whose `from_host` IS your
dev box's hostname. The container and host pushers coexist; both work
against the same DB.

---

## H. Content-hash asset filenames

### What changed

- `pg-web push` reads `pgweb.toml [server].env`. When `production` (or
  `prod`), every asset's URL gets fingerprinted: `/styles.css` becomes
  `/styles.<8hex>.css` (Blake3-derived). Templates are rewritten in
  the same step — literal `href="/styles.css"` swaps to
  `href="/styles.<hex>.css"` before the template row is upserted.
- Router emits `Cache-Control: public, max-age=31536000, immutable`
  for any asset request whose path matches the fingerprint shape
  `*.<hex8+>.<ext>$` AND env=production. Canonical paths still get
  `must-revalidate`; dev mode is unchanged.
- `[server].env = "development"` (the default) skips the rewrite and
  stores assets under canonical URLs. The dev iteration loop is
  unaffected.

### Verify a prod-mode push fingerprints assets

```bash
cd /tmp
~/pg-web/target/debug/pg-web init demo-h
cd demo-h
sed -i 's/env  = "development"/env  = "production"/' pgweb.toml
echo 'body { color: black; }' > public/styles.css
mkdir -p pages
echo '<!doctype html><link href="/styles.css">hello' > pages/index.html
~/pg-web/target/debug/pg-web up
~/pg-web/target/debug/pg-web push
```

Then query:

```sql
SELECT path FROM pgweb.assets;
```

Expected: a row like `/styles.abcd1234.css` — fingerprinted. No
`/styles.css` row.

### Verify the rendered template references the hashed URL

```bash
curl -s http://localhost:8080/ | grep -oE '/styles\.[0-9a-f]+\.css'
```

Expected: prints something like `/styles.abcd1234.css`. The literal
`/styles.css` href in the template was rewritten at push time.

### Verify immutable Cache-Control

```bash
curl -sI http://localhost:8080/styles.abcd1234.css | grep -i cache-control
```

(use the actual fingerprint from the previous step)

Expected: `cache-control: public, max-age=31536000, immutable`.

### Verify the canonical URL no longer resolves

```bash
curl -sI http://localhost:8080/styles.css | head -1
```

Expected: `HTTP/1.1 404 Not Found`. In prod mode, only the
fingerprinted URL is registered.

### Switch back to dev mode

```bash
sed -i 's/env  = "production"/env  = "development"/' pgweb.toml
~/pg-web/target/debug/pg-web push
curl -s http://localhost:8080/ | grep -oE 'href="[^"]*"'
```

Expected: `href="/styles.css"` (no fingerprint). Asset row reverted
to `/styles.css` in pgweb.assets too. The push reconciles fully.

### Known limitations (document, don't fix in v0.2)

- Only **double-quoted** attribute values are rewritten. Single-quoted
  (`href='/styles.css'`) or unquoted (`href=/styles.css`) attributes
  stay literal — both are valid HTML but unconventional in templates.
- **Dynamic refs** like `<img src="{{ user.avatar }}">` can't be
  rewritten at push time. Templates that interpolate user data
  bypass the rewrite path entirely.
- Watcher-driven `pg-web dev` runs against a force-set
  `env=development`, so even if `pgweb.toml` says `production`, the
  running server treats the env as dev and won't emit `immutable`
  Cache-Control during a dev session.

---

## I. Larger asset cap (BYTEA 2 MiB → 20 MiB)

### What changed

- `pgweb.assets.content` `CHECK` constraint relaxed from 2 MiB to
  20 MiB. CLI's `MAX_ASSET_BYTES` matches.
- This is **not** the planned `pg_largeobject` streaming feature —
  `lo_read`-backed streaming for assets >20 MiB stays Phase 2+ work.
  v0.2 ships the simple cap-raise, which covers virtually every
  practical asset (hero images, vendor JS bundles, PDFs).

### Verify a >2 MiB asset is accepted

```bash
cd /tmp/demo-h    # or any pg-web app dir
dd if=/dev/urandom of=public/hero.bin bs=1M count=5
~/pg-web/target/debug/pg-web push
curl -I http://localhost:8080/hero.bin | head -3
```

Expected: HTTP 200, `Content-Length: 5242880` (bytes match the input).
Push doesn't reject; CHECK constraint passes.

### Verify the upper cap holds

```bash
dd if=/dev/urandom of=public/too-big.bin bs=1M count=21
~/pg-web/target/debug/pg-web push 2>&1 | head -3
```

Expected output:

```
Error: too-big.bin: asset is 22020096 bytes (cap is 20971520 bytes / 20 MiB).
       Larger assets via pg_largeobject streaming remain Phase 2+ work — host
       on a CDN until then.
```

### Verify byte-perfect round-trip

The tier-3 test does this with a 5 MiB pseudo-random payload and
asserts the served bytes equal the source bytes. To repeat manually:

```bash
md5sum public/hero.bin
curl -s http://localhost:8080/hero.bin | md5sum
```

Expected: identical hashes. Important because BYTEA TOAST + libpq
binary protocol have multiple potential corruption points; the
round-trip is the definitive check.

---

## End-to-end golden path

After running through the per-component checks, the smoke test below
exercises the full v0.2 surface in one go. Use this to validate a
fresh dev box.

```bash
cd /tmp
~/pg-web/target/debug/pg-web init my-app --template todo
cd my-app
sed -i 's/env  = "development"/env  = "production"/' pgweb.toml
~/pg-web/target/debug/pg-web up
~/pg-web/target/debug/pg-web migrate apply
~/pg-web/target/debug/pg-web push
```

Expected at this point:

```bash
# (1) /styles.css href in the rendered template is fingerprinted
curl -s http://localhost:8080/ | grep -oE 'href="[^"]*styles[^"]*"'
# → href="/styles.<8hex>.css"

# (2) the fingerprinted asset serves with immutable Cache-Control
ASSET=$(curl -s http://localhost:8080/ | grep -oE '/styles\.[0-9a-f]+\.css' | head -1)
curl -sI "http://localhost:8080$ASSET" | grep -i cache-control
# → cache-control: public, max-age=31536000, immutable

# (3) the canonical URL no longer resolves
curl -sI http://localhost:8080/styles.css | head -1
# → HTTP/1.1 404 Not Found

# (4) pg_stat_activity shows the host-pushed connection (and any other
#     pg-web client) tagged with verb + os pid + host
docker exec my-app-postgres-1 psql -U postgres -d app -c \
  "SELECT pid, application_name FROM pg_stat_activity WHERE application_name LIKE 'pg-web %'"
# → at least one row, application_name like
#   "pg-web push (pid=<your-pid>, host=<your-hostname>)"

# (5) deployment ledger has a row from this push
docker exec my-app-postgres-1 psql -U postgres -d app -c \
  "SELECT from_host, file_count FROM pgweb.deployments ORDER BY pushed_at DESC LIMIT 1"
# → from_host = your dev box's hostname, file_count > 0

# (6) running push from inside the container ALSO works (F.3)
docker exec my-app-postgres-1 pg-web push --dir /app \
  --url postgres://postgres:devpassword@127.0.0.1:5432/app
# → exits 0; new ledger row whose from_host is the container's hostname

# (7) cleanup
~/pg-web/target/debug/pg-web down
```

If all seven steps print the expected output, v0.2's user-visible
surface works end-to-end. Any divergence is a real find — the
`session_5_validation.md` per-component sections above narrow down
which feature to look at first.

---

## F.2 / true streaming — TBD

These remain Session 6 / Phase 2+ work. No validation steps to run
yet; they'll land here when the components ship.
