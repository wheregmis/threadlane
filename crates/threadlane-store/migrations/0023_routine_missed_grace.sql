-- Existing routines retain their catch-up behavior.
ALTER TABLE timers ADD COLUMN missed_grace_minutes INTEGER
    CHECK (missed_grace_minutes IS NULL OR
           (typeof(missed_grace_minutes) = 'integer' AND missed_grace_minutes BETWEEN 1 AND 1440));
