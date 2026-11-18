ALTER TABLE machines ADD COLUMN slot_limit INTEGER CHECK (slot_limit > 0);
