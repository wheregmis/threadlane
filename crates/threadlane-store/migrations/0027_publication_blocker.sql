-- Why a task cannot publish, kept across runs until a repository is attached.
ALTER TABLE tasks ADD COLUMN publication_blocker TEXT;
