-- How many calls each standing rule has allowed (docs/milestone-9.md §2,
-- "count uses"). Rules from before have allowed none.
ALTER TABLE standing_rules ADD COLUMN uses INTEGER NOT NULL DEFAULT 0;
