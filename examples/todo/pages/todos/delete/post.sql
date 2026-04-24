-- POST /todos/delete — remove one todo.
--
-- Text mode (no sibling .html). HTMX's hx-swap="outerHTML" on a zero-length
-- response body collapses the <li> element out of the list. Two statements:
-- DELETE without RETURNING, then a SELECT that produces the empty return
-- value regardless of whether the DELETE matched a row. This keeps the
-- endpoint idempotent — double-click Delete and the second call is a no-op.

CREATE OR REPLACE FUNCTION pgweb.pages__todos__delete__post(req json) RETURNS text AS $$
  DELETE FROM public.todos WHERE id = (req->'body'->>'id')::bigint;
  SELECT ''::text;
$$ LANGUAGE sql;
