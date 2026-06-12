-- Workload (d): deliberately slow handler for head-of-line-blocking demo.
-- Does pg_sleep(0.2). The point of this route is *not* its own perf;
-- it is run at low rate *concurrently* with workload (b) to show that
-- unrelated fast requests' latency distribution craters because everything
-- is serialized on the single SPI thread / single current-thread runtime.
--
-- Returns tiny JSON so Tera path is exercised (or could be raw text; json+tera
-- keeps it comparable to (b)).

CREATE OR REPLACE FUNCTION pgweb.pages__bench__slow__index(req json) RETURNS json AS $$
BEGIN
  PERFORM pg_sleep(0.2);
  RETURN json_build_object('slept', 0.2);
END;
$$ LANGUAGE plpgsql;
