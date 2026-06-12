# pg-web — Vision

pg-web turns **PostgreSQL itself into a complete web server and application framework**. The database IS the application.

## The pitch

Today's typical web stack: browser → load balancer → Node/Go/Python app server → ORM → database. Four layers, three network hops, and thousands of lines of glue code shuffling data between them.

pg-web collapses all of that. The HTTP listener runs *inside* a PostgreSQL background worker. The Rust-based web server uses Postgres's Server Programming Interface (SPI) to execute SQL directly against the database's in-memory shared buffers — no TCP, no connection pooling, no per-query auth handshake. Business logic is PL/pgSQL or SQL functions. UI is HTMX-driven HTML templates rendered with Tera. No JavaScript build step required.

One Docker container. One binary. One mental model.

## Why this, why now

- **HTMX has proven hypermedia can match the interactivity of React SPAs** for the 80% of apps that aren't Figma or Google Docs — at a fraction of the code and complexity.
- **pgrx and the Rust-for-Postgres ecosystem** have matured to the point where production-grade Rust can run inside Postgres without writing C bindings.
- **Developers are rediscovering the value of owning the Postgres host.** Managed DBs (RDS, Cloud SQL, Supabase) don't allow custom extensions, but for ambitious teams shipping on VPS or self-hosted infrastructure, the speed and flexibility wins outweigh the ops cost. pg-web doubles down on that shift.
- **Latency is the final frontier.** Modern web apps spend 80-95% of their server time waiting on database round-trips. Zero-hop SPI access is a category-level performance improvement, not a micro-optimization.

## What a developer (and their agents) do

```
my-app/
  pages/
    index.html
    index.sql
    posts/
      [id].html
      [id].sql
  public/
    styles.css
  schema.prisma
  pgweb.toml
```

1. Write `pages/posts/[id].sql` returning a JSON object.
2. Write `pages/posts/[id].html` using Tera syntax against that JSON.
3. `pg-web dev`.
4. Hit `http://localhost:8080/posts/42`.
5. See your HTML rendered with real data, pulled through SPI, rendered by Tera, shipped back — all without leaving the Postgres process.

Longer term, the same zero-config spirit extends to AI agents: an MCP surface + skills gives coding agents first-class access to the live documentation and (eventually) the actual data inside the app.

That's the whole dev loop. No Node install. No ORM learning curve. No build step. Agents that deeply understand the framework (and its data) are dramatically more effective teammates.

## Non-goals

- **Not an ORM.** SQL is the interface, not something to abstract over.
- **Not a SPA framework.** HTMX-driven hypermedia is the contract. If you need full client-side state machines, use React with a traditional backend.
- **Not a TLS termination layer.** Caddy (or Nginx/Traefik) handles TLS in front.
- **Not a managed-DB offering.** Users must own their Postgres host. RDS/Cloud SQL compatibility is explicitly out of scope.
- **Not a framework for cross-database portability.** PostgreSQL-specific. SQLite, MySQL, etc. are not on the roadmap.

## Success criteria (for v1.0)

- A developer can go from `pg-web init` to serving real HTMX traffic in under 5 minutes on a fresh Linux VPS.
- The demo companion app at `examples/todo/` runs the full feature surface on every commit to `main`.
- A 1-vCPU / 2 GiB VPS sustains a few thousand req/s of tiny "fetch and render" traffic at low concurrency with sub-millisecond p50 (and far lower at 10 k-row responses or high concurrency). The single-threaded worker is the measured ceiling for tail latency and isolation under mixed load — see `docs/BENCHMARKS.md` (prompt 015). The old "1 000 req/s (Target — to be benchmarked.)" claim is retired in favor of the published numbers.
- The entire app framework is deployable with `docker compose up` and a 20-line `Caddyfile`.
