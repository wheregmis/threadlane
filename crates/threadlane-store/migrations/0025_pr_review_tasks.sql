CREATE TABLE pr_review_tasks (
    task_id INTEGER PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    pull_url TEXT NOT NULL,
    head TEXT NOT NULL
);
CREATE INDEX pr_review_tasks_head ON pr_review_tasks(pull_url, head);
