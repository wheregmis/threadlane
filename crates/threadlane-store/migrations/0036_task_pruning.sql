-- Tiered task compaction and pruning support.
ALTER TABLE tasks ADD COLUMN compacted_at INTEGER;
CREATE INDEX tasks_compaction ON tasks (project_id, status, compacted_at);
