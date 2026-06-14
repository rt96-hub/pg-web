-- Custom health endpoint for the todo example app.
--
-- This demonstrates overriding the framework's default /health page.
--
-- New projects (created with `pg-web init` or `pg-web init --template todo`)
-- automatically get sensible defaults at:
--   - /health                 (app-level, overridable)
--   - /_pgweb/health          (protected platform probe, never overridable)
--
-- By adding pages/health/index.html + index.sql here and pushing,
-- the route row in pgweb.routes for GET /health now points at our handler
-- instead of pgweb._default_health_handler. Our page is served.
--
-- The same works for /readiness.
--
-- You can also set health_enabled = false in pgweb.toml to make the
-- framework default "disappear" (falls through to your _404 or 404),
-- while your own custom /health still works.
--
-- This file + the sibling index.html is the living demo inside the
-- official todo companion app.

CREATE OR REPLACE FUNCTION pgweb.pages__health__index(req json) RETURNS json AS $$
  SELECT json_build_object('title', 'Health — overridden in todo app')
$$ LANGUAGE sql STABLE;
