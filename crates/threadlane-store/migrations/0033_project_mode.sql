-- A binding's mode may be NULL: it follows its project (docs/autonomy-rework.md
-- Stage C). SQLite cannot drop NOT NULL in place, so the table is rebuilt.
-- A mode equal to the project's effective ceiling (its own, stricter with the
-- space's) becomes NULL, which acts the same; any other mode, and Off, stays.
CREATE TABLE bindings_new (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    agent_id INTEGER NOT NULL REFERENCES agents (id),
    mode TEXT,
    is_lead INTEGER NOT NULL DEFAULT 0,
    models TEXT NOT NULL DEFAULT '{}',
    max_parallel INTEGER NOT NULL,
    overrides TEXT NOT NULL DEFAULT '{}',
    UNIQUE (project_id, agent_id)
);
INSERT INTO bindings_new (id, project_id, agent_id, mode, is_lead, models, max_parallel, overrides)
SELECT b.id, b.project_id, b.agent_id,
    CASE
        WHEN b.mode <> 'off' AND b.mode = (
            -- instr ranks the modes from the strictest.
            SELECT CASE
                WHEN p.mode_ceiling IS NULL THEN s.mode_ceiling
                WHEN instr('off on_request assisted autonomous', p.mode_ceiling)
                    < instr('off on_request assisted autonomous', s.mode_ceiling)
                    THEN p.mode_ceiling
                ELSE s.mode_ceiling
            END
            FROM projects p, space s
            WHERE p.id = b.project_id AND s.id = 1
        ) THEN NULL
        ELSE b.mode
    END,
    b.is_lead, b.models, b.max_parallel, b.overrides
FROM bindings b;
DROP TABLE bindings;
ALTER TABLE bindings_new RENAME TO bindings;
CREATE UNIQUE INDEX bindings_one_lead ON bindings (project_id) WHERE is_lead = 1;
