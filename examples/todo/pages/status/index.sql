-- GET /status — demo of explicit JSON content-type + envelope via response
-- contract v2 (prompt 013). A raw-text route (no sibling .html) that uses
-- the pgweb.json helper so the response is served with Content-Type:
-- application/json instead of the legacy text/html default.
--
-- This (and /see-other) are the minimal companion flows required by the
-- prompt and CLAUDE.md "every feature ships with a companion-app flow".

CREATE OR REPLACE FUNCTION pgweb.pages__status__index(req json) RETURNS json AS $$
  SELECT pgweb.json(
    jsonb_build_object(
      'status', 'ok',
      'time',   now(),
      'note',   'response contract v2 demo'
    )
  )
$$ LANGUAGE sql STABLE;