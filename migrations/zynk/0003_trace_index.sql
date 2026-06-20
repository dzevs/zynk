-- Feature #107 (IM2): partial expression index on the per-message trace_id.
-- The trace_id lives in `messages.meta_json` ONLY (IM1: `{"trace_id": <id>}`);
-- body/body_hash/protocol_json/FTS are UNCHANGED. This index lets the
-- `json_extract(meta_json, '$.trace_id') = ?` prefilter (zynk query --trace, the
-- thread/inbox surfaces, and a future `zynk trace`) resolve a trace without a full
-- scan. Additive + backward-compatible: old rows carry no trace_id, so the partial
-- WHERE clause excludes them from the index entirely (a tiny index over the few
-- traced rows). Applied idempotently by the sqlx Migrator via `_sqlx_migrations`.
CREATE INDEX IF NOT EXISTS idx_messages_trace_id
    ON messages (json_extract(meta_json, '$.trace_id'))
    WHERE json_extract(meta_json, '$.trace_id') IS NOT NULL;
