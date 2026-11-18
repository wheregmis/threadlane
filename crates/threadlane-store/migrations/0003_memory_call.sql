-- Memory writes keyed on the call that made them (Plan.md §8.3, invariant
-- 5): a call repeated after a crash, or a stored call performed again after
-- an interruption, finds the versions it wrote and writes nothing more.
ALTER TABLE memory_versions ADD COLUMN written_by_call TEXT;
CREATE INDEX memory_versions_call ON memory_versions (written_by_call, path);
