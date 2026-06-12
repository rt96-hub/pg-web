-- Workload (c): a write path (POST).
-- Exercises real write transaction + commit on the serving path.
-- The harness truncates bench_todos before/after write runs to bound growth.
-- (Alternative considered: savepoint + rollback inside the handler to keep
-- the table empty while still paying WAL/commit costs for the INSERT; we
-- chose truncate-between-runs for a more realistic write profile.)

CREATE OR REPLACE FUNCTION pgweb.pages__bench__write__post(req json) RETURNS text AS $fn$
DECLARE
  v_title text := 'bench-write-' || (random()*1e9)::bigint::text;
  v_id    bigint;
  v_done  boolean;
BEGIN
  INSERT INTO public.bench_todos (title)
  VALUES (v_title)
  RETURNING id, done INTO v_id, v_done;

  RETURN (json_build_object(
    'success', true,
    'id',      v_id,
    'title',   v_title
  )::text);
END;
$fn$ LANGUAGE plpgsql;
