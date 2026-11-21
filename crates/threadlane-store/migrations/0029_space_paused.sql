-- A paused space grants no lane until it is resumed; its events queue (#654).
ALTER TABLE space ADD COLUMN paused INTEGER NOT NULL DEFAULT 0;
