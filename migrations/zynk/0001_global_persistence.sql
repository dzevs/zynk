CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    runtime_session_id TEXT NOT NULL,
    socket_namespace TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    tab_id TEXT NOT NULL,
    topic TEXT NULL,
    created_at TEXT NOT NULL,
    last_message_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','closed','archived')),
    conversation_seq INTEGER NOT NULL DEFAULT 0,
    meta_json TEXT NOT NULL DEFAULT '{}'
);

CREATE UNIQUE INDEX idx_conversations_active_scope
    ON conversations(runtime_session_id, socket_namespace, workspace_id, tab_id)
    WHERE status = 'active';

CREATE INDEX idx_conversations_socket_tab
    ON conversations(socket_namespace, workspace_id, tab_id, status);

CREATE TABLE conversation_participants (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    agent_label TEXT NOT NULL,
    pane_id TEXT NULL,
    terminal_id TEXT NULL,
    terminal_instance_id TEXT NULL,
    agent_session_source TEXT NULL,
    agent_session_kind TEXT NULL,
    agent_session_value TEXT NULL,
    participant_key TEXT NOT NULL,
    joined_at TEXT NOT NULL,
    left_at TEXT NULL,
    UNIQUE(conversation_id, participant_key)
);

CREATE INDEX idx_participants_conversation_agent
    ON conversation_participants(conversation_id, agent_label);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    conversation_seq INTEGER NOT NULL,
    delivery_seq INTEGER NOT NULL DEFAULT 0,
    derived_parent_id TEXT NULL REFERENCES messages(id),
    runtime_session_id TEXT NOT NULL,
    socket_namespace TEXT NOT NULL,
    created_at TEXT NOT NULL,
    target_arg TEXT NOT NULL,
    from_participant_id TEXT NOT NULL REFERENCES conversation_participants(id),
    to_participant_id TEXT NOT NULL REFERENCES conversation_participants(id),
    type TEXT NULL,
    body TEXT NOT NULL,
    body_hash TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    tab_id TEXT NOT NULL,
    cwd TEXT NULL,
    foreground_cwd TEXT NULL,
    branch TEXT NULL,
    git_sha TEXT NULL,
    protocol_json TEXT NOT NULL DEFAULT '{}',
    meta_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(conversation_id, conversation_seq)
);

CREATE INDEX idx_messages_runtime_created
    ON messages(runtime_session_id, socket_namespace, created_at);

CREATE INDEX idx_messages_conversation_sender_seq
    ON messages(conversation_id, from_participant_id, conversation_seq);

CREATE TABLE delivery_events (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('drafted','submitted','received','processed','failed')),
    proof_source TEXT NOT NULL CHECK (proof_source IN ('pane.send_text','pane.send_input','pane.submit','integration','operator','system.recovery')),
    zynk_event_id TEXT NULL,
    seq INTEGER NOT NULL,
    timestamp TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(message_id, seq)
);

CREATE INDEX idx_delivery_events_message_seq
    ON delivery_events(message_id, seq);

CREATE VIRTUAL TABLE messages_fts USING fts5(
    body,
    type,
    branch,
    cwd,
    target_arg,
    content='messages',
    content_rowid='rowid',
    tokenize='unicode61'
);
