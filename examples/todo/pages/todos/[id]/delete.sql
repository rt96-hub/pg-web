-- DELETE /todos/:id — remove one todo using a real HTTP method.
--
-- This is the companion-app coverage for prompt 017 sub-item A (full method set).
-- Pattern demonstrated:
--   * Directory-as-route + filename-as-method: `pages/todos/[id]/delete.sql`
--     produces DELETE /todos/:id  (capture [id] in parent dir, stem "delete").
--   * Handler receives id via `req.path_params` (never in body for DELETE here).
--   * Text mode (`.sql` only, no sibling .html): RETURNS text, router sends
--     the bytes verbatim (here the empty string).
--   * HTMX usage: `hx-delete="/todos/42" hx-target="closest li" hx-swap="outerHTML"`
--     plus empty response body causes the targeted <li> to be removed from the DOM
--     (the "collapse" trick). This is the idiomatic replacement for the old
--     Phase-1 workaround POST /todos/delete with body {id} + hx-post.
--
-- Idempotency: the DELETE is unconditional; re-issuing for a missing id is a
-- harmless no-op and still returns '' (so double-click Delete is safe).
--
-- Transaction: the whole request (handler + any implicit work) is one SPI tx.
-- If the handler raised, the DELETE would roll back.
--
-- How a user applies the same pattern:
--   pages/things/[id]/delete.sql   → DELETE /things/:id
--   In the list template: <button hx-delete="/things/{{ thing.id }}" ...>
--   No separate "delete" subfolder needed; the method stem lives beside index.

CREATE OR REPLACE FUNCTION pgweb.pages__todos__$id__delete(req json) RETURNS text AS $$
  DELETE FROM public.todos WHERE id = (req->'path_params'->>'id')::bigint;
  SELECT ''::text;
$$ LANGUAGE sql;
