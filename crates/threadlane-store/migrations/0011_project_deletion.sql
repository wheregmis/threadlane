-- Committed with the deletion and removed after local workspace cleanup succeeds.
CREATE TABLE project_cleanup (
    id INTEGER PRIMARY KEY,
    removal TEXT NOT NULL
);

-- Preserve identifiers and feed cursors when the highest-numbered rows are deleted.
CREATE TABLE deletion_floors (
    name TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
