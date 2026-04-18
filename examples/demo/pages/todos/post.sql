-- POST /todos — create a new todo.
--
-- Inserts and returns the new row so Tera can render pages/todos/post.html
-- (the single-<li> fragment appended to the list via hx-swap="beforeend").
--
-- NULLIF(trim(...), '') turns a whitespace-only title into NULL, which
-- trips the NOT NULL constraint and surfaces a 500 to the browser. Phase
-- 1 has no user-facing validation UX yet — see docs/ROADMAP.md.

CREATE OR REPLACE FUNCTION pgweb.pages__todos__post(req json) RETURNS json AS $$
  INSERT INTO public.todos (title)
  VALUES (NULLIF(trim(req->'body'->>'title'), ''))
  RETURNING json_build_object(
    'todo', json_build_object('id', id, 'title', title, 'done', done)
  )
$$ LANGUAGE sql;
