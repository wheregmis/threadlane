-- Commits a task hands to a subtask it creates (#790): a JSON list of
-- StartPatch, applied when the subtask's branch is first made.
ALTER TABLE tasks ADD COLUMN start_patches TEXT NOT NULL DEFAULT '[]';
