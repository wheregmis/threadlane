CREATE TABLE review_recipe (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL,
    implementation TEXT NOT NULL,
    review TEXT NOT NULL
);
CREATE INDEX run_events_by_event ON run_events(event_id, run_id);
CREATE INDEX events_timer_history ON events(source, kind, id);
