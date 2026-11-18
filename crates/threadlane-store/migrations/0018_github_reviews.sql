CREATE TABLE github_reviews (
    submission INTEGER NOT NULL REFERENCES task_reviews (submission_run) ON DELETE CASCADE,
    pull_url TEXT NOT NULL,
    revision TEXT NOT NULL,
    body TEXT NOT NULL,
    delivery_key TEXT NOT NULL DEFAULT (lower(hex(randomblob(16)))),
    PRIMARY KEY (submission, pull_url)
);
