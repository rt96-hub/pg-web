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
# - Image `pgweb/postgres:latest` exists locally (built by
#   `scripts/build-image.sh`).
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
http() {
    local method="$1" path="$2"
    SMOKE_CODE=$(curl -sS -o "$SMOKE_BODY_FILE" -w "%{http_code}" -X "$method" "$BASE_URL$path")
    SMOKE_BODY=$(cat "$SMOKE_BODY_FILE")
}

# --- preflight ------------------------------------------------------

step "Preflight"
[[ -x "$BIN" ]] || fail "pg-web binary missing: build with \`cargo build -p pg_web_cli\` first"
docker --version >/dev/null || fail "docker not available"
docker image inspect pgweb/postgres:latest >/dev/null \
    || fail "image pgweb/postgres:latest not found — run \`bash scripts/build-image.sh\`"
ok "docker + image + binary all present"

# Wipe stale state.
rm -rf "$SMOKE_DIR"

# --- 1. init + up + push: happy path --------------------------------

step "1. scaffold → up → push"
( cd /tmp && "$BIN" init "$(basename "$SMOKE_DIR")" ) >/dev/null
ok "scaffolded $SMOKE_DIR"

cd "$SMOKE_DIR"
"$BIN" up >/dev/null
ok "stack up"

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
sed -i 's/^env  = "development"/env  = "production"/' "$SMOKE_DIR/pgweb.toml"
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
sed -i 's/^env  = "production"/env  = "development"/' "$SMOKE_DIR/pgweb.toml"

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

# --- done -----------------------------------------------------------

printf "\n\033[1;32m✓ smoke-cli: all assertions passed\033[0m\n"
