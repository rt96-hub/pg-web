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

# --- done -----------------------------------------------------------

printf "\n\033[1;32m✓ smoke-cli: all assertions passed\033[0m\n"
