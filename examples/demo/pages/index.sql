-- GET / — lists todos, oldest first.
--
-- Returns JSON consumed by pages/index.html via Tera. `todos` key maps to
-- the template's `{% for todo in todos %}` loop; empty array when there
-- are no rows (so the template renders the "No todos yet." branch).

CREATE OR REPLACE FUNCTION pgweb.pages__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'todos', COALESCE(
      (SELECT json_agg(
         json_build_object('id', id, 'title', title, 'done', done)
         ORDER BY id
       ) FROM public.todos),
      '[]'::json
    )
  )
$$ LANGUAGE sql STABLE;
