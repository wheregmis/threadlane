-- The Reviewer's backend refused its setup: the round waits for the lead's retry (#672).
ALTER TABLE task_reviews ADD COLUMN configuration_failure TEXT;
