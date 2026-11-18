CREATE TABLE task_reviews (
    submission_run INTEGER PRIMARY KEY REFERENCES runs (id) ON DELETE CASCADE,
    task_id INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    reviewer INTEGER NOT NULL REFERENCES agents (id),
    revisions TEXT NOT NULL,
    implementation_result TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'submitted',
    verdict_run INTEGER REFERENCES runs (id) ON DELETE SET NULL,
    verdict TEXT,
    feedback TEXT
);
CREATE UNIQUE INDEX task_reviews_one_pending ON task_reviews (task_id) WHERE state = 'pending';
