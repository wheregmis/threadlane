-- Performing a stored call you approved (Plan.md §9.4): the intent is
-- recorded before the effect and the outcome after, so the call takes effect
-- at most once across runs (invariant 5).
ALTER TABLE decisions ADD COLUMN perform_started_at INTEGER;
ALTER TABLE decisions ADD COLUMN outcome TEXT;
