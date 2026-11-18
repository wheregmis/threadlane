-- The space lead's ask a project lead started this task for, so its result's reply reaches the space lead.
ALTER TABLE tasks ADD COLUMN space_lead_event INTEGER;
