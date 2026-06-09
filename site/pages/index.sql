-- GET / — landing page for pg-web.dev docs site.
--
-- Exercises the full dynamic (json → Tera) pipeline on the home route.
-- Returns a small list of primary sections (used by the template for the
-- nav grid) plus a proof-of-concept note. No app tables required.

CREATE OR REPLACE FUNCTION pgweb.pages__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'sections', json_build_array(
      json_build_object('path', '/overview',    'title', 'Overview'),
      json_build_object('path', '/layout',     'title', 'App Layout'),
      json_build_object('path', '/tutorial',    'title', 'Tutorial'),
      json_build_object('path', '/deployment',  'title', 'Deployment'),
      json_build_object('path', '/roadmap',     'title', 'Roadmap')
    ),
    'note', 'This documentation site is itself a pg-web application.'
  )
$$ LANGUAGE sql STABLE;
