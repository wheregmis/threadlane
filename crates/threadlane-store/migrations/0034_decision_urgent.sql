-- Whether a decision notifies you; proposals and limitation reports wait in
-- the Inbox's Later group. Every decision before this one notified.
ALTER TABLE decisions ADD COLUMN urgent INTEGER NOT NULL DEFAULT 1;
