-- Workload (a): static template render, no table read.
-- Isolates Tera + HTTP/SPI framing overhead. Returns a tiny constant JSON.

CREATE OR REPLACE FUNCTION pgweb.pages__bench__static__index(req json) RETURNS json AS $$
  SELECT json_build_object(
    'msg', 'hello from bench static',
    'ts',  now()::text,
    'v',   1
  )
$$ LANGUAGE sql STABLE;
