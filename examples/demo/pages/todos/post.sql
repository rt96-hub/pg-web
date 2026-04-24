-- POST /todos — create a new todo, or return an inline error on
-- check_violation. Demonstrates the Phase 1 form-validation pattern
-- (see docs/APP-DEVELOPER-GUIDE.md § Forms & validation).
--
-- Handler stays `RETURNS json`; it always succeeds. The `success` flag
-- tells the Tera template (pages/todos/post.html) which branch to
-- render: the <li> fragment for append-to-list OR an OOB-swapped error
-- div targeting #form-error in the index.
--
-- COALESCE(..., '') + trim() normalizes missing / NULL / whitespace-
-- only inputs to empty string. That way the table's CHECK
-- (length(trim(title)) > 0) is the single source of validation truth;
-- we don't duplicate the rule in the handler.

CREATE OR REPLACE FUNCTION pgweb.pages__todos__post(req json) RETURNS json AS $fn$
DECLARE
  v_title text := trim(COALESCE(req->'body'->>'title', ''));
  v_id    bigint;
  v_done  boolean;
BEGIN
  INSERT INTO public.todos (title)
  VALUES (v_title)
  RETURNING id, done INTO v_id, v_done;

  RETURN json_build_object(
    'success', true,
    'todo',    json_build_object('id', v_id, 'title', v_title, 'done', v_done)
  );
EXCEPTION WHEN check_violation THEN
  -- Empty / whitespace-only title rejected by the table's CHECK.
  -- Return a success=false payload so the template renders an inline
  -- error fragment instead of surfacing a 500.
  RETURN json_build_object(
    'success', false,
    'error',   'Title cannot be empty.'
  );
END;
$fn$ LANGUAGE plpgsql;
