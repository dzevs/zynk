-- ADR 0015: agent.prompt is a distinct readiness-gated submission route.
-- Widen the CHECK without relabelling history or changing prior migrations.
CREATE TABLE delivery_events_agent_prompt (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('drafted','submitted','received','processed','failed')),
    proof_source TEXT NOT NULL CHECK (proof_source IN ('pane.send_text','pane.send_input','pane.submit','integration','pane_tree','operator','system.recovery','agent.prompt')),
    zynk_event_id TEXT NULL,
    seq INTEGER NOT NULL,
    timestamp TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(message_id, seq)
);

INSERT INTO delivery_events_agent_prompt
    (id, message_id, event_type, proof_source, zynk_event_id, seq, timestamp, payload_json)
SELECT
    id,
    message_id,
    event_type,
    proof_source,
    zynk_event_id,
    seq,
    timestamp,
    payload_json
FROM delivery_events;

DROP TABLE delivery_events;

ALTER TABLE delivery_events_agent_prompt RENAME TO delivery_events;

CREATE INDEX idx_delivery_events_message_seq
    ON delivery_events(message_id, seq);
