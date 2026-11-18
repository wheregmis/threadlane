-- Threadlane hub schema, version 1.
--
-- One database per hub; no table has a space column (Plan.md §2.4).
-- Only SQL that SQLite also accepts, so the file can always be opened by
-- SQLite itself (Plan.md §18.1). Times are integer milliseconds since the Unix
-- epoch. JSON is stored as TEXT. Booleans are INTEGER 0 or 1.

-- Organization (Plan.md §2.4) ------------------------------------------------

CREATE TABLE space (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    name TEXT NOT NULL,
    mode_ceiling TEXT NOT NULL,
    quiet_hours TEXT,
    channels TEXT NOT NULL
);

CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    is_general INTEGER NOT NULL DEFAULT 0,
    mode_ceiling TEXT,
    max_parallel_tasks INTEGER NOT NULL,
    paused INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX projects_one_general ON projects (is_general) WHERE is_general = 1;

CREATE TABLE repositories (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    name TEXT NOT NULL,
    remote_url TEXT NOT NULL UNIQUE,
    default_branch TEXT NOT NULL,
    protected_branches TEXT NOT NULL DEFAULT '[]',
    UNIQUE (project_id, name)
);

CREATE TABLE agents (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    role TEXT NOT NULL,
    instructions TEXT NOT NULL DEFAULT '',
    backends TEXT NOT NULL DEFAULT '[]',
    models TEXT NOT NULL DEFAULT '{}',
    tools TEXT NOT NULL DEFAULT '{}',
    requirements TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE bindings (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    agent_id INTEGER NOT NULL REFERENCES agents (id),
    mode TEXT NOT NULL,
    is_lead INTEGER NOT NULL DEFAULT 0,
    models TEXT NOT NULL DEFAULT '{}',
    max_parallel INTEGER NOT NULL,
    overrides TEXT NOT NULL DEFAULT '{}',
    UNIQUE (project_id, agent_id)
);
CREATE UNIQUE INDEX bindings_one_lead ON bindings (project_id) WHERE is_lead = 1;

CREATE TABLE standing_rules (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    agent_id INTEGER NOT NULL REFERENCES agents (id),
    tool TEXT NOT NULL,
    pattern TEXT NOT NULL,
    tier INTEGER NOT NULL,
    created_from_decision INTEGER,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
CREATE INDEX standing_rules_scope ON standing_rules (project_id, agent_id, tool);

CREATE TABLE project_machines (
    project_id INTEGER NOT NULL REFERENCES projects (id),
    machine_id INTEGER NOT NULL REFERENCES machines (id),
    PRIMARY KEY (project_id, machine_id)
);

-- Inbox (Plan.md §6.2) -------------------------------------------------------

CREATE TABLE lanes (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    kind TEXT NOT NULL,
    task_id INTEGER,
    lease_owner INTEGER,
    lease_expires_at INTEGER,
    lease_epoch INTEGER NOT NULL DEFAULT 0,
    backoff_until INTEGER,
    last_machine_id INTEGER
);
CREATE UNIQUE INDEX lanes_one_lead ON lanes (project_id) WHERE kind = 'lead';
CREATE UNIQUE INDEX lanes_one_per_task ON lanes (task_id) WHERE task_id IS NOT NULL;
CREATE INDEX lanes_lease ON lanes (lease_owner, lease_expires_at);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    lane_id INTEGER NOT NULL REFERENCES lanes (id),
    source TEXT NOT NULL,
    source_event_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    trust TEXT NOT NULL,
    requested INTEGER NOT NULL,
    session_ref INTEGER,
    decision_ref INTEGER,
    reply_to_ref INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    status_reason TEXT,
    claimed_by_run INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    received_at INTEGER NOT NULL,
    UNIQUE (source, source_event_id)
);
CREATE INDEX events_lane_status ON events (lane_id, status, id);
CREATE INDEX events_claimed_by_run ON events (claimed_by_run);

-- Sessions and runs (Plan.md §4) ---------------------------------------------

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    lane_id INTEGER NOT NULL REFERENCES lanes (id),
    agent_id INTEGER,
    backend TEXT,
    native_session_id TEXT,
    machine_id INTEGER,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL
);
CREATE INDEX sessions_lane ON sessions (lane_id, status, last_active_at);

CREATE TABLE runs (
    id INTEGER PRIMARY KEY,
    lane_id INTEGER NOT NULL REFERENCES lanes (id),
    session_id INTEGER NOT NULL REFERENCES sessions (id),
    machine_id INTEGER NOT NULL,
    lease_epoch INTEGER NOT NULL,
    agent_id INTEGER,
    backend TEXT,
    account_label TEXT,
    status TEXT NOT NULL,
    outcome TEXT,
    rationale TEXT,
    error TEXT,
    usage TEXT,
    started_at INTEGER NOT NULL,
    ended_at INTEGER
);
-- Invariant 2: at most one active run per lane.
CREATE UNIQUE INDEX runs_one_active_per_lane ON runs (lane_id) WHERE status = 'active';

CREATE TABLE run_events (
    run_id INTEGER NOT NULL REFERENCES runs (id),
    event_id INTEGER NOT NULL REFERENCES events (id),
    PRIMARY KEY (run_id, event_id)
);

-- Calls to the system's tools; at most once per key within a run (Plan.md §4.6).
CREATE TABLE tool_calls (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES runs (id),
    call_key TEXT NOT NULL,
    tool TEXT NOT NULL,
    arguments TEXT NOT NULL,
    safe_to_repeat INTEGER NOT NULL,
    status TEXT NOT NULL,
    result TEXT,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    UNIQUE (run_id, call_key)
);

-- The mirrored event stream, appended in order under the lease (Plan.md §4.6).
CREATE TABLE journal (
    run_id INTEGER NOT NULL REFERENCES runs (id),
    seq INTEGER NOT NULL,
    at INTEGER NOT NULL,
    entry TEXT NOT NULL,
    PRIMARY KEY (run_id, seq)
);

-- Tasks (Plan.md §5.2, §5.5) -------------------------------------------------

CREATE TABLE tasks (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    lane_id INTEGER NOT NULL UNIQUE REFERENCES lanes (id),
    agent_id INTEGER NOT NULL REFERENCES agents (id),
    title TEXT NOT NULL,
    goal TEXT NOT NULL,
    repositories TEXT NOT NULL DEFAULT '[]',
    parent_task_id INTEGER REFERENCES tasks (id),
    origin TEXT NOT NULL,
    status TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    created_by_call TEXT UNIQUE,
    last_machine_id INTEGER,
    waiting_reason TEXT,
    result TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX tasks_project_status ON tasks (project_id, status);

CREATE TABLE task_dependencies (
    task_id INTEGER NOT NULL REFERENCES tasks (id),
    depends_on INTEGER NOT NULL REFERENCES tasks (id),
    PRIMARY KEY (task_id, depends_on)
);

CREATE TABLE checkpoints (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks (id),
    run_id INTEGER NOT NULL REFERENCES runs (id),
    repository_id INTEGER NOT NULL REFERENCES repositories (id),
    base_commit TEXT NOT NULL,
    branch TEXT NOT NULL,
    bundle_ref TEXT NOT NULL,
    bundle_sha256 TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX checkpoints_task ON checkpoints (task_id, repository_id, id);

-- Machines and accounts (Plan.md §5.3) ---------------------------------------

CREATE TABLE machines (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    endpoint_id TEXT NOT NULL UNIQUE,
    is_hub INTEGER NOT NULL DEFAULT 0,
    trust TEXT NOT NULL,
    status TEXT NOT NULL,
    last_seen INTEGER,
    capabilities TEXT NOT NULL DEFAULT '{}',
    backends TEXT NOT NULL DEFAULT '{}',
    gh_login INTEGER NOT NULL DEFAULT 0,
    max_parallel INTEGER NOT NULL,
    load INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE checkouts (
    machine_id INTEGER NOT NULL REFERENCES machines (id),
    repository_id INTEGER NOT NULL REFERENCES repositories (id),
    last_fetched_at INTEGER,
    PRIMARY KEY (machine_id, repository_id)
);

CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    label TEXT NOT NULL UNIQUE,
    backend TEXT NOT NULL,
    max_parallel INTEGER,
    cooldown_until INTEGER
);

CREATE TABLE machine_accounts (
    machine_id INTEGER NOT NULL REFERENCES machines (id),
    account_id INTEGER NOT NULL REFERENCES accounts (id),
    PRIMARY KEY (machine_id, account_id)
);

-- Admitted transport keys and one-time join codes (Plan.md §3.5).
CREATE TABLE admitted_keys (
    endpoint_id TEXT PRIMARY KEY,
    role TEXT NOT NULL,
    name TEXT NOT NULL,
    admitted_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE TABLE join_codes (
    secret_hash TEXT PRIMARY KEY,
    role TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
);

-- Timers (Plan.md §7.2) ------------------------------------------------------

CREATE TABLE timers (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    lane_id INTEGER NOT NULL REFERENCES lanes (id),
    agent_id INTEGER,
    task_ref INTEGER,
    category TEXT NOT NULL,
    due_at INTEGER NOT NULL,
    recurrence TEXT,
    instruction TEXT NOT NULL,
    status TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_by_call TEXT UNIQUE,
    last_fired_at INTEGER,
    fire_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX timers_due ON timers (status, due_at);

-- Memory (Plan.md §8) --------------------------------------------------------

CREATE TABLE memory_files (
    path TEXT PRIMARY KEY,
    version INTEGER NOT NULL,
    content TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    written_by_run INTEGER,
    written_at INTEGER NOT NULL
);

CREATE TABLE memory_versions (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    version INTEGER NOT NULL,
    content TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    written_by_run INTEGER,
    written_at INTEGER NOT NULL,
    UNIQUE (path, version)
);

-- Decisions (Plan.md §9.4) ---------------------------------------------------

CREATE TABLE decisions (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id),
    lane_id INTEGER NOT NULL REFERENCES lanes (id),
    task_id INTEGER,
    session_id INTEGER,
    run_id INTEGER,
    tool_call_id TEXT,
    tool TEXT NOT NULL,
    arguments TEXT NOT NULL,
    arguments_hash TEXT NOT NULL,
    kind TEXT NOT NULL,
    question TEXT NOT NULL,
    context TEXT NOT NULL DEFAULT '',
    options TEXT NOT NULL,
    recommended INTEGER,
    offers_standing_rule INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    answer TEXT,
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    answered_at INTEGER,
    UNIQUE (run_id, tool_call_id)
);
CREATE INDEX decisions_lane_status ON decisions (lane_id, status);

-- Outbox (Plan.md §10.1) -----------------------------------------------------

CREATE TABLE outbox (
    id INTEGER PRIMARY KEY,
    project_id INTEGER,
    lane_id INTEGER,
    session_id INTEGER,
    run_id INTEGER,
    tool_call_id TEXT UNIQUE,
    kind TEXT NOT NULL,
    decision_id INTEGER,
    channel TEXT NOT NULL,
    body TEXT NOT NULL,
    urgency TEXT NOT NULL,
    status TEXT NOT NULL,
    not_before INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    channel_message_id TEXT,
    created_at INTEGER NOT NULL,
    sent_at INTEGER,
    -- Order of delivery, set when sent; the client's feed cursor (Plan.md §11.2).
    delivery_seq INTEGER
);
CREATE INDEX outbox_deliverable ON outbox (status, not_before, id);
CREATE INDEX outbox_project_sent ON outbox (project_id, status, id);
CREATE UNIQUE INDEX outbox_delivery_seq ON outbox (delivery_seq) WHERE delivery_seq IS NOT NULL;
CREATE INDEX outbox_run ON outbox (run_id, kind);

-- Pulling sources keep a cursor per source (Plan.md §6.6).
CREATE TABLE source_cursors (
    source TEXT PRIMARY KEY,
    cursor TEXT,
    updated_at INTEGER NOT NULL
);
