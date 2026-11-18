-- The active system timer is the project's dreaming setting.
CREATE UNIQUE INDEX timers_one_active_dream_per_project ON timers (project_id)
    WHERE category = 'dreaming' AND created_by = 'system' AND status = 'active';
