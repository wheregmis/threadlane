-- Reserve each question with its fail-closed answer before calling a backend.
CREATE TABLE permission_questions (
    question_key TEXT PRIMARY KEY,
    answer TEXT NOT NULL,
    pending INTEGER NOT NULL,
    decision_id INTEGER REFERENCES decisions(id)
);
ALTER TABLE decisions ADD COLUMN forge_dispatched INTEGER NOT NULL DEFAULT 0;
