-- The model a run asked its backend for; NULL is the backend's own default.
ALTER TABLE runs ADD COLUMN model TEXT;
