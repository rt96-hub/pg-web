# pg-web demo — Todos

A working HTMX-driven todo list. This app serves three purposes:

1. **Reference implementation.** "Show me a real pg-web Phase 1 app."
2. **End-to-end test target.** The framework's own CI runs `pg-web push`
   against this directory and exercises every route.
3. **End state of the tutorial.** `docs/TUTORIAL.md` walks a reader
   through building this app from scratch, step by step.

## Run it

Prereqs:

- The Docker image `rtaylor96/pg-web:latest` exists locally. From the
  pg-web repo root: `bash scripts/build-image.sh` (one-time, ~5–10 min
  cold).
- The `pg-web` CLI is built: `cargo build -p pg-web` from the repo
  root, which puts the binary at `target/debug/pg-web`.

Then, from this directory (adjust the path to the `pg-web` binary as
needed):

```bash
../../target/debug/pg-web up
../../target/debug/pg-web migrate apply
../../target/debug/pg-web push

open http://localhost:8080    # or `curl http://localhost:8080/`
```

`pg-web up` starts the Docker Compose stack, waits for Postgres + the
HTTP server to accept connections, and prints the resolved
`DATABASE_URL`. `migrate apply` / `push` then auto-resolve that URL
from `pgweb.toml` + environment, so you don't need to pass `--url`
unless you want to override.

For iterative development, replace the explicit `push` with:

```bash
../../target/debug/pg-web dev
```

which watches `pages/` + `public/`, auto-pushes 200ms after every save
(deduped by Blake3 content hash so no-op writes don't trigger a push),
shift-left-preflights `.sql` changes against the DB before applying
them, and tails the Postgres container logs inline.

Add a todo via the form; toggle and delete the resulting rows via the
`<li>` buttons. Every click is an HTMX request, round-tripped through
Postgres, rendered server-side.

To tear down:

```bash
../../target/debug/pg-web down             # stops the container
../../target/debug/pg-web down --volumes   # also drops the pgdata volume
```

## What's in here

```
examples/todo/
├── migrations/
│   └── 0001_create_todos.sql           # public.todos schema
└── pages/
    ├── index.html                      # GET / — list view + HTMX form
    ├── index.sql                       # GET / — SELECT todos → JSON
    ├── _404.html                       # Static 404 page (no handler)
    ├── health/
    │   ├── index.html                  # GET /health — custom override demo (HTML + glowing status)
    │   └── index.sql                   # GET /health — handler (demonstrates replacing framework default)
    └── todos/
        ├── post.html                   # POST /todos — new-<li> fragment
        ├── post.sql                    # POST /todos — INSERT
        ├── toggle/
        │   ├── post.html               # POST /todos/toggle — updated <li>
        │   └── post.sql                # POST /todos/toggle — UPDATE
        └── [id]/
            ├── index.{html,sql}        # GET /todos/:id detail view
            └── delete.sql              # DELETE /todos/:id — text mode (real HTTP method)
```

Six routes (five user + _404), three modes:

- **Dynamic** (JSON → Tera): `GET /`, `POST /todos`, `POST /todos/toggle`, `GET /todos/:id`, `GET /health`
- **Static** (template, no SQL): `GET /_404` (served on route miss)
- **Raw text** (SQL only, no template): `DELETE /todos/:id` (via pages/todos/[id]/delete.sql) — returns `''`

The delete flow now uses a real `DELETE` (prompt 017-A) instead of the `POST /todos/delete` workaround. The UI buttons use `hx-delete="/todos/{{ id }}"`.

## Teaching material

`docs/TUTORIAL.md` (in the repo root's `docs/`) walks through building
this app from a fresh `pg-web init`. Each section produces a runnable
intermediate state. Finish the tutorial → your app matches this one.

## Health & readiness (018.1)

pg-web ships two surfaces (protected platform probes at `/_pgweb/health` and `/_pgweb/readiness` for infrastructure; conventional overridable `GET /health` and `GET /readiness` for app-level checks).

This app includes a live custom override at `/health` (see `pages/health/index.html` + `index.sql`). The page explains how new projects get default checks and how you can replace them.

The full override pattern, disable flags (`health_enabled = false` etc. in pgweb.toml), and which surface to use for Docker HEALTHCHECK vs. business health are documented with a complete worked example in the README written by `pg-web init --template todo`.

The test suite (http_smoke + docker_e2e) asserts that the protected probes are always 200 and survive a deliberately broken user handler (the original footgun that motivated 018.1).
