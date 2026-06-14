#!/usr/bin/env bash
# End-to-end black-box smoke: exercises the full pg-web user flow the
# way a human would run it, asserting on each expected response by
# `grep`. Complements the Rust tier-3 E2E tests by surfacing real CLI
# stdout / HTTP bodies so regressions and gotchas (port conflicts,
# stale docker images, docker-compose service rename, etc.) show up as
# readable output instead of a test-runner one-liner.
#
# Self-contained: scaffolds into `/tmp/pg-web-smoke`, tears down at end
# (or on any failure via trap). Idempotent: an existing smoke dir is
# wiped at start. Safe to run alongside tier 3 — tier 3 uses random
# host ports via testcontainers; this uses the scaffolded :8080/:5432.
#
# Preconditions:
# - Docker daemon reachable (`docker --version` succeeds).
# - Image `rtaylor96/pg-web:latest` (the current test/ dev image) exists locally
#   (built by `scripts/build-image.sh`, or `pg-web up` pulled it).
# - The `pg-web` binary is built at `target/debug/pg-web`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO_ROOT/target/debug/pg-web"
SMOKE_DIR="${SMOKE_DIR:-/tmp/pg-web-smoke}"
BASE_URL="http://localhost:8080"

# --- helpers --------------------------------------------------------

step() {
    printf "\n\033[1;34m▸ %s\033[0m\n" "$*"
}
ok() {
    printf "  \033[32m✓\033[0m %s\n" "$*"
}
fail() {
    printf "  \033[31m✗\033[0m %s\n" "$*" >&2
    exit 1
}

teardown() {
    # Best-effort cleanup. `|| true` everywhere so a partial failure
    # during setup still tries to bring state back to a clean slate.
    if [[ -d "$SMOKE_DIR" ]]; then
        ( cd "$SMOKE_DIR" && "$BIN" down --volumes >/dev/null 2>&1 ) || true
    fi
    rm -rf "$SMOKE_DIR" || true
}
trap teardown EXIT

assert_contains() {
    local body="$1" needle="$2" label="$3"
    if [[ "$body" == *"$needle"* ]]; then
        ok "$label — found $(printf %q "$needle")"
    else
        echo "$body" >&2
        fail "$label — missing $(printf %q "$needle")"
    fi
}

assert_not_contains() {
    local body="$1" needle="$2" label="$3"
    if [[ "$body" == *"$needle"* ]]; then
        echo "$body" >&2
        fail "$label — unexpectedly found $(printf %q "$needle")"
    else
        ok "$label — absent $(printf %q "$needle")"
    fi
}

assert_status() {
    local code="$1" expected="$2" label="$3"
    if [[ "$code" == "$expected" ]]; then
        ok "$label — HTTP $code"
    else
        fail "$label — HTTP $code (expected $expected)"
    fi
}

assert_header_starts() {
    local actual="$1" needle="$2" label="$3"
    if [[ "$actual" == "$needle"* ]]; then
        ok "$label — $actual"
    else
        fail "$label — header was $(printf %q "$actual") (expected prefix $(printf %q "$needle"))"
    fi
}

# Populate globals $SMOKE_CODE and $SMOKE_BODY after each call. Callers:
#   http GET /foo
#   code=$SMOKE_CODE; body=$SMOKE_BODY
# Done this way (no command substitution) because $(http ...) runs in a
# subshell, which would discard the variable assignments.
SMOKE_CODE=""
SMOKE_BODY=""
SMOKE_BODY_FILE="$(mktemp)"

# --- portability helpers (macOS dev + Linux CI) -----------------------
# sed -i is not portable (BSD sed on mac requires -i ''; GNU differs).
# Use a temp backup + rm.
sed_inplace() {
    local expr="$1" file="$2"
    sed -i.tmp "$expr" "$file" && rm -f "${file}.tmp"
}

# timeout(1) not always present on macOS (unless coreutils). Provide fallback.
run_with_timeout() {
    local secs="$1"; shift
    if command -v timeout >/dev/null 2>&1; then
        timeout "$secs" "$@"
    elif command -v gtimeout >/dev/null 2>&1; then
        gtimeout "$secs" "$@"
    else
        # Crude but sufficient for the one caller (SSE header grab that we intentionally interrupt).
        ( "$@" ) & local cpid=$!
        sleep "$secs" || true
        kill "$cpid" 2>/dev/null || true
        wait "$cpid" 2>/dev/null || true
    fi
}

http() {
    local method="$1" path="$2"
    SMOKE_CODE=$(curl -sS -o "$SMOKE_BODY_FILE" -w "%{http_code}" -X "$method" "$BASE_URL$path")
    SMOKE_BODY=$(cat "$SMOKE_BODY_FILE")
}

# --- preflight ------------------------------------------------------

step "Preflight"
[[ -x "$BIN" ]] || fail "pg-web binary missing: build with \`cargo build -p pg-web\` first"
docker --version >/dev/null || fail "docker not available"
docker image inspect rtaylor96/pg-web:latest >/dev/null \
    || fail "image rtaylor96/pg-web:latest not found — run \`bash scripts/build-image.sh\` (or \`pg-web up\` in an app dir to pull it)"
ok "docker + image + binary all present"

# Early port contention check. :8080 fights are almost always caused by:
# - a stray pgrx dev PG (BGW), or
# - another scripts/test-all.sh / smoke-cli / bench run still holding the port.
# The main test-all.sh now has a flock guard + early stop_pgrx_dev_pg and
# unique SMOKE_DIR. If you see this, run the hygiene steps from CLAUDE.md.
if command -v curl >/dev/null && curl -sf http://localhost:8080/ >/dev/null 2>&1; then
    echo "  WARNING: something is already responding on http://localhost:8080/"
    echo "           (This will likely cause 'port already bound' or wrong content later.)"
    echo "           Recommended hygiene (run in your shell before the gate):"
    echo "             cargo pgrx stop pg17 || true"
    echo "             pkill -f 'test-all.sh|smoke-cli.sh|bench/run.sh' || true"
    echo "             docker ps -q --filter 'name=pg-web' --filter 'name=bench' --filter 'name=smoke' | xargs -r docker rm -f || true"
    echo "           Or ensure only one scripts/test-all.sh is running (the script now uses a lockfile)."
fi

# Snapshot the *local* image ID we intend to test (prompt 025 integrity).
# After `up` we assert the compose stack is running exactly this image,
# not whatever `docker compose pull` (or a prior tag) would have resolved to.
EXPECTED_IMAGE_ID=$(docker image inspect rtaylor96/pg-web:latest --format '{{.Id}}' 2>/dev/null || echo "")

# Wipe stale state.
rm -rf "$SMOKE_DIR"

# --- 1. init + up + push: happy path --------------------------------

step "1. scaffold → up → push"
( cd /tmp && "$BIN" init "$(basename "$SMOKE_DIR")" ) >/dev/null
ok "scaffolded $SMOKE_DIR"

cd "$SMOKE_DIR"
"$BIN" up >/dev/null
ok "stack up"

# Integrity postcondition (prompt 025 #1): the container actually running
# must be the local image ID captured at preflight (i.e. the one
# test-all.sh's ensure_image_fresh just prepared). This hard-fails tier 4
# if an unconditional pull (or manual `pg-web up`) clobbered the tag.
if [[ -n "$EXPECTED_IMAGE_ID" ]]; then
    pg_container=$(docker compose ps -q postgres 2>/dev/null || true)
    if [[ -n "$pg_container" ]]; then
        running_id=$(docker inspect "$pg_container" --format '{{.Image}}' 2>/dev/null || echo "")
        if [[ "$running_id" != "$EXPECTED_IMAGE_ID" ]]; then
            echo "  running container image: $running_id" >&2
            echo "  expected (local build):  $EXPECTED_IMAGE_ID" >&2
            fail "tier 4 is validating the wrong artifact (image ID mismatch — pull clobber?)"
        fi
        ok "stack using the expected local image ID (integrity check passed)"
    fi
fi

push_output=$("$BIN" push 2>&1)
echo "$push_output" | grep -q "env → development" \
    || fail "push should sync env=development, got: $push_output"
ok "push: env synced to development"

# --- 2. happy path GET / --------------------------------------------

step "2. GET / renders the scaffolded template"
http GET /
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "200" "GET /"
assert_contains "$body" "Welcome to $(basename "$SMOKE_DIR")" "scaffolded template rendered"
assert_contains "$body" "pg-web dev" "scaffolded body content present"

# --- 3. 404 fallback ------------------------------------------------

step "3. GET /no-such-route falls through to default 404"
http GET /no-such-route
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "404" "GET /no-such-route"
assert_contains "$body" "404" "default 404 body"

# --- 4. break a handler (runtime SQL exception) → dev error page ----

step "4. handler raises → dev error page"
mkdir -p "$SMOKE_DIR/pages/boom"
cat > "$SMOKE_DIR/pages/boom/index.html" <<'HTML'
<!doctype html>
<p>never renders</p>
HTML
cat > "$SMOKE_DIR/pages/boom/index.sql" <<'SQL'
CREATE OR REPLACE FUNCTION pgweb.pages__boom__index(req json) RETURNS json AS $$
  SELECT json_build_object('x', 1 / 0)
$$ LANGUAGE sql;
SQL
"$BIN" push >/dev/null
ok "push accepted boom route"

http GET /boom
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "500" "GET /boom"
assert_contains "$body" "PGWEB_E003_HANDLER_SQL_EXCEPTION" "error code"
assert_contains "$body" "SQL exception inside handler" "error title"
assert_contains "$body" "22012" "SQLSTATE for division_by_zero"
assert_contains "$body" "division by zero" "PG message"
assert_contains "$body" "pgweb.pages__boom__index" "handler name in context"
assert_contains "$body" "How to fix" "remedy section rendered"

# --- 5. break a Tera template → push rejects at parse time ----------

step "5. broken Tera template → push rejected, live state preserved"
mkdir -p "$SMOKE_DIR/pages/mangled"
cat > "$SMOKE_DIR/pages/mangled/index.html" <<'HTML'
{% if flag %}
  <p>unclosed block —
HTML
cat > "$SMOKE_DIR/pages/mangled/index.sql" <<'SQL'
CREATE OR REPLACE FUNCTION pgweb.pages__mangled__index(req json) RETURNS json AS $$
  SELECT '{}'::json
$$ LANGUAGE sql STABLE;
SQL

set +e
push_output=$("$BIN" push 2>&1)
rc=$?
set -e
[[ "$rc" -ne 0 ]] || fail "push should have rejected the broken template, but exit=0"
assert_contains "$push_output" "pages/mangled/index.html" "error names the broken file"
assert_contains "$push_output" "Tera template failed to parse" "error flags parse problem"

# Clean the broken pages so subsequent pushes can succeed, and verify
# the previously-live routes still work (push rolled back cleanly).
rm -rf "$SMOKE_DIR/pages/mangled"

http GET /
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "200" "GET / after rolled-back push"
assert_contains "$body" "Welcome to $(basename "$SMOKE_DIR")" "live / still scaffolded"

http GET /boom
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "500" "GET /boom after rolled-back push"
assert_contains "$body" "PGWEB_E003_HANDLER_SQL_EXCEPTION" "boom still surfaces dev page"

# --- 6. flip to production → dev page gone, generic 500 --------------

step "6. flip [server] env to production → generic 500"
sed_inplace 's/^env  = "development"/env  = "production"/' "$SMOKE_DIR/pgweb.toml"
grep -q '^env  = "production"' "$SMOKE_DIR/pgweb.toml" || fail "couldn't flip pgweb.toml"
push_output=$("$BIN" push 2>&1)
echo "$push_output" | grep -q "env → production" \
    || fail "push should sync env=production, got: $push_output"
ok "push: env synced to production"

http GET /boom
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "500" "GET /boom in production"
assert_contains "$body" "internal server error" "prod body is generic"
assert_not_contains "$body" "PGWEB_E003" "prod body does NOT leak error code"
assert_not_contains "$body" "SQLSTATE" "prod body does NOT leak SQLSTATE"
assert_not_contains "$body" "division by zero" "prod body does NOT leak PG message"
assert_not_contains "$body" "pgweb.pages__boom__index" "prod body does NOT leak handler name"

# --- 7. static assets: push, serve, revalidate, reconcile ------------

step "7. static asset served with ETag + If-None-Match revalidation"
# Restore env=development so subsequent pushes in dev mode don't leak
# generic 500s if something goes wrong here.
sed_inplace 's/^env  = "production"/env  = "development"/' "$SMOKE_DIR/pgweb.toml"

mkdir -p "$SMOKE_DIR/public"
printf 'body{background:#fafafa;color:#333}' > "$SMOKE_DIR/public/smoke.css"

push_output=$("$BIN" push 2>&1)
echo "$push_output" | grep -q "assets — 1 upserted" \
    || fail "push should have reported 1 asset upserted, got: $push_output"
ok "push: 1 asset upserted"

# First request: full body + ETag header. `-D FILE` writes response
# headers; we parse ETag / Content-Type / Cache-Control out of it.
code=$(curl -sS -o /tmp/smoke-css -D /tmp/smoke-hdr -w "%{http_code}" "$BASE_URL/smoke.css")
assert_status "$code" "200" "GET /smoke.css"
# awk strips the trailing \r\n and gives us just the header value.
etag=$(awk 'BEGIN{IGNORECASE=1} /^etag:/ {sub(/^[Ee][Tt][Aa][Gg]: */, ""); sub(/\r$/, ""); print}' /tmp/smoke-hdr)
ctype=$(awk 'BEGIN{IGNORECASE=1} /^content-type:/ {sub(/^[Cc][Oo][Nn][Tt][Ee][Nn][Tt]-[Tt][Yy][Pp][Ee]: */, ""); sub(/\r$/, ""); print}' /tmp/smoke-hdr)
cctl=$(awk 'BEGIN{IGNORECASE=1} /^cache-control:/ {sub(/^[Cc][Aa][Cc][Hh][Ee]-[Cc][Oo][Nn][Tt][Rr][Oo][Ll]: */, ""); sub(/\r$/, ""); print}' /tmp/smoke-hdr)
assert_header_starts "$ctype" "text/css" "content-type"
[[ "$etag" =~ ^\".+\"$ ]] && ok "etag is double-quoted: $etag" \
    || fail "etag not double-quoted: $etag"
[[ -n "$cctl" ]] && ok "cache-control present: $cctl" \
    || fail "cache-control header missing"
body=$(cat /tmp/smoke-css)
assert_contains "$body" "background:#fafafa" "CSS body served verbatim"

# Revalidation: same ETag in If-None-Match → 304, empty body.
code=$(curl -sS -o /tmp/smoke-304 -w "%{http_code}" \
    -H "If-None-Match: $etag" "$BASE_URL/smoke.css")
assert_status "$code" "304" "GET /smoke.css with matching If-None-Match"
[[ ! -s /tmp/smoke-304 ]] && ok "304 body is empty" \
    || fail "304 body should be empty, got: $(cat /tmp/smoke-304)"

# Mismatched ETag → full body again.
mismatch=$(curl -sS -o /dev/null -w "%{http_code}" \
    -H "If-None-Match: \"something-else\"" "$BASE_URL/smoke.css")
assert_status "$mismatch" "200" "GET /smoke.css with non-matching If-None-Match"

# Delete the file, push, asset should be reconciled away.
rm "$SMOKE_DIR/public/smoke.css"
push_output=$("$BIN" push 2>&1)
echo "$push_output" | grep -q "assets — 0 upserted, 1 removed" \
    || fail "push should have removed 1 asset, got: $push_output"
ok "push: 1 asset reconciled away"

code=$(curl -sS -o /dev/null -w "%{http_code}" "$BASE_URL/smoke.css")
assert_status "$code" "404" "GET /smoke.css after delete+push"

# --- 8. pgweb.html_escape renders safely end-to-end -----------------

step "8. pgweb.html_escape escapes user input at the SQL layer"
# Raw-text handler (no .html sibling). The route returns text directly;
# Tera never sees the bytes, so whatever `pgweb.html_escape` produces is
# literally what the client gets. That's the tight end-to-end check: if
# the helper isn't installed, missing, or wrong, the response body
# differs from the expected entity string.
mkdir -p "$SMOKE_DIR/pages/escape"
cat > "$SMOKE_DIR/pages/escape/index.sql" <<'SQL'
CREATE OR REPLACE FUNCTION pgweb.pages__escape__index(req json) RETURNS text AS $$
    SELECT pgweb.html_escape(req->'query'->>'in')
$$ LANGUAGE sql STABLE;
SQL
"$BIN" push >/dev/null
ok "push accepted /escape raw-text route"

# URL-encoded payload is <script>alert('x')</script>.
http GET "/escape?in=%3Cscript%3Ealert(%27x%27)%3C%2Fscript%3E"
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "200" "GET /escape with html-ish input"
assert_contains "$body" "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;" \
    "html_escape produced fully-escaped entities"
assert_not_contains "$body" "<script>" "raw <script> tag absent from body"
assert_not_contains "$body" "alert('x')" "raw single-quoted alert absent from body"

# --- 9. form-validation pattern: EXCEPTION → inline error ------------

step "9. handler catches check_violation → inline error (200, not 500)"
# Pure smoke setup (no migrations / no table): the handler uses RAISE
# with SQLSTATE 23514 (check_violation) to simulate the same failure a
# real table CHECK would cause, then catches it in its EXCEPTION block
# and returns an inline error fragment. Exercises the Component B
# pattern end-to-end in the black-box smoke layer.
mkdir -p "$SMOKE_DIR/pages/echoform"
cat > "$SMOKE_DIR/pages/echoform/post.sql" <<'SQL'
CREATE OR REPLACE FUNCTION pgweb.pages__echoform__post(req json) RETURNS text AS $fn$
DECLARE
  v_body text := trim(COALESCE(req->'body'->>'body', ''));
BEGIN
  IF length(v_body) = 0 THEN
    RAISE EXCEPTION 'body cannot be empty' USING ERRCODE = '23514';
  END IF;
  RETURN '<p class="ok">Saved: ' || pgweb.html_escape(v_body) || '</p>';
EXCEPTION WHEN check_violation THEN
  RETURN '<p id="form-error" hx-swap-oob="true">Body cannot be empty.</p>';
END;
$fn$ LANGUAGE plpgsql;
SQL
"$BIN" push >/dev/null
ok "push accepted /echoform raw-text handler"

# Happy path: non-empty body survives, gets html_escape'd before echo.
code=$(curl -sS -o /tmp/smoke-echoform-ok \
    -w "%{http_code}" \
    -X POST \
    -H 'Content-Type: application/x-www-form-urlencoded' \
    --data 'body=hello%20%3Cworld%3E' \
    "$BASE_URL/echoform")
assert_status "$code" "200" "POST /echoform with real body"
body=$(cat /tmp/smoke-echoform-ok)
assert_contains "$body" "Saved: hello &lt;world&gt;" "happy path echoes escaped"
assert_not_contains "$body" "<world>" "raw <world> tag absent"

# Empty body: handler RAISEs, catches, returns inline error.
code=$(curl -sS -o /tmp/smoke-echoform-err \
    -w "%{http_code}" \
    -X POST \
    -H 'Content-Type: application/x-www-form-urlencoded' \
    --data 'body=' \
    "$BASE_URL/echoform")
assert_status "$code" "200" "POST /echoform with empty body (NOT 500)"
body=$(cat /tmp/smoke-echoform-err)
assert_contains "$body" "Body cannot be empty" "inline error rendered"
assert_contains "$body" 'hx-swap-oob="true"' "error targets OOB swap"
assert_not_contains "$body" "PGWEB_E003" "no dev error page leaked"
assert_not_contains "$body" "SQLSTATE" "no SQLSTATE leaked"

# Whitespace-only body: trim() path, same check_violation, same handling.
code=$(curl -sS -o /tmp/smoke-echoform-ws \
    -w "%{http_code}" \
    -X POST \
    -H 'Content-Type: application/x-www-form-urlencoded' \
    --data 'body=%20%20%20' \
    "$BASE_URL/echoform")
assert_status "$code" "200" "POST /echoform with whitespace-only body"
body=$(cat /tmp/smoke-echoform-ws)
assert_contains "$body" "Body cannot be empty" "whitespace trimmed + caught"

# --- 10. env set/list/unset + pgweb.setting() handler read -----------

step "10. env set → list → handler reads via pgweb.setting() → unset"

# Rejection of push-managed keys: setting `env` via CLI must error,
# point at pgweb.toml, and NOT touch the DB.
set +e
err_out=$("$BIN" env set "env=production" 2>&1)
rc=$?
set -e
[[ "$rc" -ne 0 ]] || fail "env set env=production should have exited non-zero, got 0"
assert_contains "$err_out" "pgweb.toml" "reserved-key error points at pgweb.toml"

# Happy path: set a user key, verify list shows it, handler reads it.
"$BIN" env set "SMOKE_TOKEN=sk_test_abc_xyz" >/dev/null
ok "env set SMOKE_TOKEN"

list_out=$("$BIN" env list)
# Note: `pg-web env list` may redact values for secret safety (e.g. "SMOKE_TOKEN=sk_t****").
# We only assert key *names* appear in the presentation here; the real value round-trip
# is proven by the handler read via pgweb.setting() in the subsequent HTTP request.
assert_contains "$list_out" "SMOKE_TOKEN" "env list shows the new key (name)"
# Framework-managed keys should still be listed (even if their value is shown redacted).
assert_contains "$list_out" "env=" "env list still shows framework-managed env (name)"

# Handler consumes it via pgweb.setting(). Raw-text route returns the
# value verbatim so we can assert on it directly.
mkdir -p "$SMOKE_DIR/pages/config"
cat > "$SMOKE_DIR/pages/config/index.sql" <<'SQL'
CREATE OR REPLACE FUNCTION pgweb.pages__config__index(req json) RETURNS text AS $$
    SELECT COALESCE(pgweb.setting('SMOKE_TOKEN'), '(unset)')
$$ LANGUAGE sql STABLE;
SQL
"$BIN" push >/dev/null
ok "push accepted /config handler"

http GET /config
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "200" "GET /config"
assert_contains "$body" "sk_test_abc_xyz" "handler read SMOKE_TOKEN via pgweb.setting()"
assert_not_contains "$body" "(unset)" "COALESCE fallback NOT triggered"

# Overwrite path: set overwrites existing value.
"$BIN" env set "SMOKE_TOKEN=sk_live_updated" >/dev/null
http GET /config
body="$SMOKE_BODY"
assert_contains "$body" "sk_live_updated" "env set overwrote prior value"
assert_not_contains "$body" "sk_test_abc_xyz" "prior value absent after overwrite"

# Unset: row gone, handler falls through COALESCE.
"$BIN" env unset SMOKE_TOKEN >/dev/null
list_out=$("$BIN" env list)
assert_not_contains "$list_out" "SMOKE_TOKEN" "env list no longer shows unset key"
http GET /config
body="$SMOKE_BODY"
assert_contains "$body" "(unset)" "COALESCE fallback triggers after unset"

# Idempotent unset: second call reports no-op, still exits 0.
unset_again=$("$BIN" env unset SMOKE_TOKEN)
assert_contains "$unset_again" "no-op" "repeat unset is idempotent no-op"

# --- 11. push --dry-run + pgweb.deployments ledger (F.1) -------------

# Helper: count rows in pgweb.deployments via docker compose exec. -tAc
# gives a bare numeric line (no header, no padding) so the shell can
# compare it directly.
count_deployments() {
    ( cd "$SMOKE_DIR" && docker compose exec -T postgres \
        psql -U postgres -d app -tAc \
        "SELECT COUNT(*) FROM pgweb.deployments" )
}

step "11. pgweb.deployments ledger records each committed push"
before_deploys=$(count_deployments)
# All prior sections did real pushes — ledger should be populated.
[[ "$before_deploys" -gt 0 ]] && ok "deployments ledger has $before_deploys row(s) from prior pushes" \
    || fail "deployments ledger empty after many pushes — schema or insert path broken?"

step "12. push --dry-run: output tagged, transaction rolled back"
dry_out=$("$BIN" push --dry-run 2>&1)
assert_contains "$dry_out" "[dry-run]" "dry-run output carries the tag"
assert_contains "$dry_out" "rolled back" "explicit rollback message present"
assert_contains "$dry_out" "would push" "verb is conditional, not past-tense"

after_dry=$(count_deployments)
[[ "$after_dry" == "$before_deploys" ]] && ok "dry-run did NOT insert a deployments row ($after_dry unchanged)" \
    || fail "dry-run inserted a row ($before_deploys → $after_dry) — rollback broken"

step "13. push without --with-migrate refuses pending migrations"
# Drop a trivial, valid migration into the scaffold. Without
# --with-migrate, push must refuse and NOT run the migration.
mkdir -p "$SMOKE_DIR/migrations"
cat > "$SMOKE_DIR/migrations/0001_smoke_test.sql" <<'SQL'
CREATE TABLE IF NOT EXISTS public.smoke_test (id int);
SQL

set +e
err_out=$("$BIN" push 2>&1)
rc=$?
set -e
[[ "$rc" -ne 0 ]] || fail "push should refuse pending migrations, got exit $rc"
assert_contains "$err_out" "pending migrations" "error identifies the class"
assert_contains "$err_out" "0001_smoke_test.sql" "error names the offending file"
assert_contains "$err_out" "--with-migrate" "error points at the fix flag"

# pgweb.migrations must be untouched.
mig_count=$( ( cd "$SMOKE_DIR" && docker compose exec -T postgres \
    psql -U postgres -d app -tAc \
    "SELECT COUNT(*) FROM pgweb.migrations WHERE name = '0001_smoke_test.sql'" ) )
[[ "$mig_count" == "0" ]] && ok "migrations ledger untouched by refused push" \
    || fail "refused push ran the migration anyway ($mig_count rows)"

step "14. push --with-migrate applies + pushes + logs a ledger row"
before_mig=$( ( cd "$SMOKE_DIR" && docker compose exec -T postgres \
    psql -U postgres -d app -tAc "SELECT COUNT(*) FROM pgweb.migrations" ) )
before_deploys=$(count_deployments)

push_out=$("$BIN" push --with-migrate 2>&1)
assert_contains "$push_out" "applied 1 migration" "summary reports the migration"
assert_contains "$push_out" "0001_smoke_test.sql" "summary names it"

after_mig=$( ( cd "$SMOKE_DIR" && docker compose exec -T postgres \
    psql -U postgres -d app -tAc "SELECT COUNT(*) FROM pgweb.migrations" ) )
after_deploys=$(count_deployments)

[[ "$after_mig" -eq $((before_mig + 1)) ]] && ok "migrations ledger gained exactly 1 row" \
    || fail "migrations ledger: expected $((before_mig + 1)), got $after_mig"
[[ "$after_deploys" -eq $((before_deploys + 1)) ]] && ok "deployments ledger gained exactly 1 row" \
    || fail "deployments ledger: expected $((before_deploys + 1)), got $after_deploys"

# Verify the latest deployment row has the expected shape.
last_row=$( ( cd "$SMOKE_DIR" && docker compose exec -T postgres \
    psql -U postgres -d app -tAc \
    "SELECT migrations_applied, (from_host IS NOT NULL)::text, (file_count > 0)::text \
     FROM pgweb.deployments ORDER BY id DESC LIMIT 1" ) )
# Expected shape: "1|true|true" — 1 migration applied, from_host populated,
# file_count > 0. (boolean::text renders as 'true'/'false', not 't'/'f'.)
[[ "$last_row" == "1|true|true" ]] && ok "latest row: migrations_applied, from_host, file_count all populated" \
    || fail "latest row shape unexpected: $last_row"

# Clean up the test migration so subsequent sections see a clean state.
rm "$SMOKE_DIR/migrations/0001_smoke_test.sql"

# --- 15. pg-web check — offline validator -----------------------------

step "15. pg-web check on the live smoke app (clean state) → exit 0"
# The scaffold went through init + up + push + a bunch of edits. The
# minimal scaffold SHOULD check clean; this is also a regression guard
# on section 1's `init` output.
set +e
check_out=$("$BIN" check 2>&1)
rc=$?
set -e
[[ "$rc" -eq 0 ]] || fail "check should exit 0 on clean scaffold, got $rc; output:\n$check_out"
assert_contains "$check_out" "no findings" "clean scaffold reports no findings"

step "16. pg-web check catches a broken migration (SQL parse) → exit 1"
# Drop a typo migration into migrations/ and confirm check flags it
# with a diagnostic, non-zero exit, and does NOT touch the DB.
mkdir -p "$SMOKE_DIR/migrations"
cat > "$SMOKE_DIR/migrations/0999_typo.sql" <<'SQL'
CRATE TABLE oops (id int);
SQL

set +e
check_out=$("$BIN" check 2>&1)
rc=$?
set -e
[[ "$rc" -ne 0 ]] || fail "check should exit non-zero on bad migration, got 0; output:\n$check_out"
assert_contains "$check_out" "0999_typo.sql" "finding names the offending file"
assert_contains "$check_out" "CRATE" "diagnostic surfaces the parser's unexpected token"

# Clean up so subsequent sections (if any) see a clean state.
rm "$SMOKE_DIR/migrations/0999_typo.sql"

step "16a. pg-web check accepts rich dollar-quoted + adjacent literal COMMENTs → exit 0"
# This is the regression case from the original sqlparser limitation report.
# Migrations with high-quality documentation using $$...$$, $tag$...$tag$,
# and adjacent string literals ('foo' 'bar') must not cause false-positive
# parse errors. Real Postgres accepts them; the offline check must too.
cat > "$SMOKE_DIR/migrations/0001_rich_comments.sql" <<'SQL'
CREATE TABLE carriers (
    id   bigserial PRIMARY KEY,
    name text NOT NULL
);

COMMENT ON TABLE carriers IS $$
Comprehensive carrier master table.

Supports:
- Multiple contact methods
- O'Brien Logistics style names (apostrophes)
- Regional rate cards
$$;

COMMENT ON COLUMN carriers.name IS 'O''Reilly''s Preferred' ' Carrier';
SQL

set +e
check_out=$("$BIN" check 2>&1)
rc=$?
set -e
[[ "$rc" -eq 0 ]] || fail "check should exit 0 on rich COMMENT migration, got $rc; output:\n$check_out"
assert_contains "$check_out" "no findings" "rich dollar-quoted COMMENTs produce clean check"

rm "$SMOKE_DIR/migrations/0001_rich_comments.sql"

step "16b. pg-web check accepts extension DDL + opclass indexes (pg_trgm style) → exit 0"
# Regression for prompt 003 / sibling app. A migration containing
# CREATE EXTENSION + CREATE INDEX ... USING gin (col gin_trgm_ops)
# (and CREATE UNIQUE INDEX) must not produce parser findings.
cat > "$SMOKE_DIR/migrations/0002_smoke_trgm.sql" <<'SQL'
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS carriers_name_trgm_idx
  ON public.carriers USING gin (name gin_trgm_ops);

CREATE UNIQUE INDEX IF NOT EXISTS carriers_email_uidx
  ON public.carriers (email);
SQL

set +e
check_out=$("$BIN" check 2>&1)
rc=$?
set -e
[[ "$rc" -eq 0 ]] || fail "check should exit 0 on extension+opclass migration, got $rc; output:\n$check_out"
assert_contains "$check_out" "no findings" "extension DDL + gin_trgm_ops indexes produce clean check"

rm "$SMOKE_DIR/migrations/0002_smoke_trgm.sql"

step "17. livereload: script auto-injected, JS stub served, SSE returns 200"
# Script injection into the rendered scaffold homepage.
http GET /
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "200" "GET /"
assert_contains "$body" "data-pgweb-livereload" "livereload <script> auto-injected"
assert_contains "$body" "/_pgweb/livereload.js" "injected script points at the right URL"
assert_contains "$body" "</script></body>" "injection landed immediately before </body>"

# JS stub content.
http GET /_pgweb/livereload.js
code="$SMOKE_CODE"; body="$SMOKE_BODY"
assert_status "$code" "200" "GET /_pgweb/livereload.js"
assert_contains "$body" "EventSource" "stub uses native EventSource"
assert_contains "$body" "/_pgweb/livereload" "stub subscribes to the right endpoint"

# SSE endpoint in dev mode. curl holds the connection open until
# `timeout` kills it; we just want to capture the response headers.
# `|| true` because curl exits 28 (timed out) even on success here —
# the interesting signal is the content-type header, not the exit.
run_with_timeout 2 curl -sS -o /dev/null -D /tmp/sse-hdr \
    "$BASE_URL/_pgweb/livereload" || true
ct=$(awk 'BEGIN{IGNORECASE=1} /^content-type:/ {sub(/^[Cc][Oo][Nn][Tt][Ee][Nn][Tt]-[Tt][Yy][Pp][Ee]: */, ""); sub(/\r$/, ""); print}' /tmp/sse-hdr)
assert_header_starts "$ct" "text/event-stream" "SSE content-type"

# 18-20: flip to production, assert everything goes silent.
step "18. livereload: production mode — SSE 404s, script NOT injected"
sed_inplace 's/^env  = "development"/env  = "production"/' "$SMOKE_DIR/pgweb.toml"
"$BIN" push >/dev/null

http GET /
body="$SMOKE_BODY"
assert_not_contains "$body" "data-pgweb-livereload" "production: NO script injection"

code=$(curl -sS -o /dev/null -w "%{http_code}" "$BASE_URL/_pgweb/livereload")
assert_status "$code" "404" "production: SSE endpoint 404s"

# Restore dev mode for any subsequent section.
sed_inplace 's/^env  = "production"/env  = "development"/' "$SMOKE_DIR/pgweb.toml"
"$BIN" push >/dev/null

step "19. pg-web check catches a broken Tera template → exit 1"
mkdir -p "$SMOKE_DIR/pages/checktpl"
cat > "$SMOKE_DIR/pages/checktpl/index.html" <<'HTML'
{% if x %}unclosed block
HTML
cat > "$SMOKE_DIR/pages/checktpl/index.sql" <<'SQL'
CREATE OR REPLACE FUNCTION pgweb.pages__checktpl__index(req json) RETURNS json AS $$
    SELECT '{}'::json
$$ LANGUAGE sql STABLE;
SQL

set +e
check_out=$("$BIN" check 2>&1)
rc=$?
set -e
[[ "$rc" -ne 0 ]] || fail "check should exit non-zero on bad Tera, got 0; output:\n$check_out"
assert_contains "$check_out" "checktpl/index.html" "finding names the offending template"

rm -rf "$SMOKE_DIR/pages/checktpl"

# --- done -----------------------------------------------------------

printf "\n\033[1;32m✓ smoke-cli: all assertions passed\033[0m\n"
