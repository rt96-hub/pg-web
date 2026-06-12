-- GET /seeother — demo of 303 redirect + Location header via the
-- response contract v2 helpers (pgweb.redirect). A raw-text route.
--
-- curl -i http://.../seeother   should return 303 with Location: /
-- This exercises the PRG path that Phase 2 login etc. will rely on.

CREATE OR REPLACE FUNCTION pgweb.pages__seeother__index(req json) RETURNS json AS $$
  SELECT pgweb.redirect('/')
$$ LANGUAGE sql STABLE;