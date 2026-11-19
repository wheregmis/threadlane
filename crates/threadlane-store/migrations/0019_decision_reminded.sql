-- When the one reminder for a decision still waiting went out, so a restart
-- does not send it again.
ALTER TABLE decisions ADD COLUMN reminded_at INTEGER;
