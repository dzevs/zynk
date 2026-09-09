-- ADR 0014 renames the provenance a receipt records from `integration` to `pane_tree`,
-- because `pane_tree` is what the server can actually prove: `received` means a process
-- inside the receiver pane's tree, under the same UID, reported ingestion of exactly these
-- ids. It was never a proof that the integration binary ran.
--
-- This migration WIDENS the `proof_source` CHECK so a receipt THIS build records can say
-- `pane_tree`. It deliberately does not touch history. Rows written before the origin check
-- carry `integration`: that server matched the ids and the receiver's hook-authoritative
-- identity, but never checked which process the caller was, so relabelling those rows would
-- assert pane-tree evidence nobody ever collected (Codex Gate-2 `msg_3e339000b75278a4`
-- reproduced exactly that on a real old-server -> new-server upgrade of one database).
-- `integration` therefore stays in the enumeration as the LEGACY, origin-unverified value;
-- the write path never records it again.
--
-- `proof_source` carries a CHECK constraint, and SQLite cannot alter one in place, so
-- `delivery_events` is rebuilt: new table with both values in the constraint, rows copied
-- VERBATIM, old table dropped, new one renamed into place. The index goes with the dropped
-- table and is recreated. Column list, types, defaults, the `messages` foreign key and the
-- `UNIQUE(message_id, seq)` guarantee are otherwise byte-for-byte the ones migration 0001
-- created. Additive by the rule that matters: migration 0001 is NOT edited, so existing
-- databases keep their checksums.

CREATE TABLE delivery_events_pane_tree (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('drafted','submitted','received','processed','failed')),
    proof_source TEXT NOT NULL CHECK (proof_source IN ('pane.send_text','pane.send_input','pane.submit','integration','pane_tree','operator','system.recovery')),
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
    proof_source,
    zynk_event_id,
    seq,
    timestamp,
    payload_json
FROM delivery_events;

DROP TABLE delivery_events;

ALTER TABLE delivery_events_pane_tree RENAME TO delivery_events;

CREATE INDEX idx_delivery_events_message_seq
    ON delivery_events(message_id, seq);
