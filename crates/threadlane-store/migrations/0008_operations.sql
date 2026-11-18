ALTER TABLE events ADD COLUMN trace_id TEXT NOT NULL DEFAULT '';
ALTER TABLE events ADD COLUMN replayed INTEGER NOT NULL DEFAULT 0;
UPDATE events SET trace_id = 'event-' || id;
CREATE INDEX events_trace ON events (trace_id);

ALTER TABLE runs ADD COLUMN trace_id TEXT NOT NULL DEFAULT '';
UPDATE runs SET trace_id = COALESCE((
    SELECT e.trace_id FROM run_events re JOIN events e ON e.id = re.event_id
    WHERE re.run_id = runs.id ORDER BY e.received_at, e.id LIMIT 1
), 'run-' || id);
CREATE INDEX runs_trace ON runs (trace_id);

ALTER TABLE tool_calls ADD COLUMN trace_id TEXT;
UPDATE tool_calls SET trace_id = (SELECT trace_id FROM runs WHERE id = tool_calls.run_id);
CREATE INDEX tool_calls_trace ON tool_calls (trace_id);

ALTER TABLE decisions ADD COLUMN trace_id TEXT;
UPDATE decisions SET trace_id = (SELECT trace_id FROM runs WHERE id = decisions.run_id);
CREATE INDEX decisions_trace ON decisions (trace_id);

ALTER TABLE outbox ADD COLUMN trace_id TEXT;
UPDATE outbox SET trace_id = (SELECT trace_id FROM runs WHERE id = outbox.run_id);
CREATE INDEX outbox_trace ON outbox (trace_id);
