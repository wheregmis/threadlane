-- Only identifiers are retained. Project deletion also removes its review state.
CREATE TABLE dream_batches (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    event_id INTEGER NOT NULL,
    since_at INTEGER NOT NULL,
    since_run INTEGER NOT NULL,
    run_floor INTEGER NOT NULL DEFAULT 0,
    memory_id INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE dream_items (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    last_review INTEGER NOT NULL DEFAULT 0,
    seen_run INTEGER,
    PRIMARY KEY (project_id, kind, item_id)
);
CREATE INDEX dream_items_page ON dream_items(project_id, kind, last_review, item_id);
