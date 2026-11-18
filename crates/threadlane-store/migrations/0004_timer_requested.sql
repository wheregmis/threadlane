-- Whether a timer's events count as work you requested, taken from its
-- creator when it is made (docs/milestone-8.md §1 decision 3). Rows from
-- before keep the rule they were made under.
ALTER TABLE timers ADD COLUMN requested INTEGER NOT NULL DEFAULT 0;
UPDATE timers SET requested = 1 WHERE created_by IN ('user', 'system') OR category = 'follow_up';
CREATE INDEX timers_project ON timers (project_id, status);
