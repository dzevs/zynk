-- ADR 0014: a receipt's provenance is renamed `integration` -> `pane_tree`, because
-- that is what the server can actually prove. `received` means a process inside the
-- receiver pane's tree reported ingestion of exactly these ids under the same UID; it
-- was never a proof that the integration binary ran, and the stored value said
-- otherwise.
--
-- `proof_source` carries a CHECK constraint, and SQLite cannot alter one in place, so
-- `delivery_events` is rebuilt: new table with the new value in the constraint, rows
-- copied with the old value rewritten, old table dropped, new one renamed into place.
-- The index goes with the dropped table and is recreated. Column list, types, defaults,
-- the `messages` foreign key and the `UNIQUE(message_id, seq)` guarantee are otherwise
-- byte-for-byte the ones migration 0001 created. Additive by the rule that matters:
-- migration 0001 is NOT edited, so existing databases keep their checksums.

CREATE TABLE delivery_events_pane_tree (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('drafted','submitted','received','processed','failed')),
    proof_source TEXT NOT NULL CHECK (proof_source IN ('pane.send_text','pane.send_input','pane.submit','pane_tree','operator','system.recovery')),
    zynk_event_id TEXT NULL,
    seq INTEGER NOT NULL,
    timestamp TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(message_id, seq)
);

INSERT INTO delivery_events_pane_tree
    (id, message_id, event_type, proof_source, zynk_event_id, seq, timestamp, payload_json)
SELECT
    id,
    message_id,
    event_type,
    CASE proof_source WHEN 'integration' THEN 'pane_tree' ELSE proof_source END,
    zynk_event_id,
    seq,
    timestamp,
    payload_json
FROM delivery_events;

DROP TABLE delivery_events;

ALTER TABLE delivery_events_pane_tree RENAME TO delivery_events;

CREATE INDEX idx_delivery_events_message_seq
    ON delivery_events(message_id, seq);
