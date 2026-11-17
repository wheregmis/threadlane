ALTER TABLE space ADD COLUMN delivery_rate INTEGER;
ALTER TABLE space ADD COLUMN digest_minutes INTEGER;
ALTER TABLE outbox ADD COLUMN pacing_hold TEXT;
ALTER TABLE outbox ADD COLUMN digest_id INTEGER REFERENCES outbox(id);
CREATE INDEX outbox_digest ON outbox(digest_id) WHERE digest_id IS NOT NULL;
CREATE INDEX outbox_recent_paced ON outbox(sent_at, delivery_seq)
    WHERE status = 'sent' AND kind = 'message' AND urgency != 'high';
ALTER TABLE lanes ADD COLUMN stopped INTEGER NOT NULL DEFAULT 0;
ALTER TABLE lanes ADD COLUMN cancelled_epoch INTEGER;
