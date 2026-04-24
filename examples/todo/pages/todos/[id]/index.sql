-- GET /todos/:id — todo detail view.
--
-- Captures from the URL come in as strings under `req.path_params`. The
-- handler stays forgiving: any URL segment matches (123, "all", "abc"),
-- but only a numeric segment corresponding to an actual todos row
-- renders the populated detail. Everything else (non-numeric, non-existent
-- id) falls through to the "not found" branch in index.html with `todo`
-- null. No cast → no exceptions → same-shape JSON either way.

CREATE OR REPLACE FUNCTION pgweb.pages__todos__$id__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'id',   req->'path_params'->>'id',
    'todo', (
      SELECT to_json(t) FROM (
        SELECT id, title, done FROM todos
        WHERE id::text = req->'path_params'->>'id'
      ) t
    )
  )
$$ LANGUAGE sql STABLE;
