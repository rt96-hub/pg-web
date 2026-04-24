-- POST /todos/toggle — flip the `done` flag on one todo.
--
-- Returns the updated row; Tera renders pages/todos/toggle/post.html
-- (same <li> shape as post.html) which HTMX swaps in via outerHTML on
-- the existing list item.

CREATE OR REPLACE FUNCTION pgweb.pages__todos__toggle__post(req json) RETURNS json AS $$
  UPDATE public.todos
  SET done = NOT done
  WHERE id = (req->'body'->>'id')::bigint
  RETURNING json_build_object(
    'todo', json_build_object('id', id, 'title', title, 'done', done)
  )
$$ LANGUAGE sql;
