-- A deleted agent keeps its row, so finished tasks still name it.
ALTER TABLE agents ADD COLUMN deleted_at INTEGER;
