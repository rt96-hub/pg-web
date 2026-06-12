-- Workload (b): the "fetch and render" path from VISION claim.
-- Single indexed read (json_agg over STABLE function) + Tera render.
-- Mirrors examples/todo/pages/index.sql but against bench_todos and a
-- dedicated route so the todo demo is not contorted.

CREATE OR REPLACE FUNCTION pgweb.pages__bench__todos__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'todos', COALESCE(
      (SELECT json_agg(
         json_build_object('id', id, 'title', title, 'done', done)
         ORDER BY id
       ) FROM public.bench_todos),
      '[]'::json
    )
  )
$$ LANGUAGE sql STABLE;
