-- Bench schema for throughput/concurrency workloads.
-- Single table exercising indexed read + write paths (mirrors the spirit of
-- examples/todo without contorting the demo app itself).

CREATE TABLE public.bench_todos (
    id         bigserial PRIMARY KEY,
    title      text NOT NULL CHECK (length(trim(title)) > 0),
    done       boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Index used by the list queries (ORDER BY id or created_at).
CREATE INDEX bench_todos_created_at_idx ON public.bench_todos (created_at DESC);
CREATE INDEX bench_todos_id_idx ON public.bench_todos (id);
