-- Demo app schema. Single table for the todo list.
--
-- `title` has a NOT NULL + length-of-trim CHECK so empty submissions
-- surface a `check_violation` that the POST handler can translate into
-- a user-visible error response.

CREATE TABLE public.todos (
    id         bigserial PRIMARY KEY,
    title      text NOT NULL CHECK (length(trim(title)) > 0),
    done       boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX todos_created_at_idx ON public.todos (created_at DESC);
