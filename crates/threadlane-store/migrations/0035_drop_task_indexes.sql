-- Task progress files are no longer auto-indexed into tasks/INDEX.md.
-- Remove existing task index snapshots so historical index bloat is reclaimed.
DELETE FROM memory_files WHERE path GLOB 'projects/*/tasks/INDEX.md';
DELETE FROM memory_versions WHERE path GLOB 'projects/*/tasks/INDEX.md';
