-- M5b: embedding index metadata. Additive; does NOT alter messages or messages_fts.
-- The sqlite-vec vec0 virtual table is created lazily by the embedding worker
-- (it requires the loaded extension), NOT here, to keep the migrator extension-free.

CREATE TABLE IF NOT EXISTS embedding_models (
    id          TEXT PRIMARY KEY,             -- e.g. "multilingual-e5-small@1"
    provider    TEXT NOT NULL,                -- "fastembed" | "fake"
    model_name  TEXT NOT NULL,                -- "intfloat/multilingual-e5-small"
    dim         INTEGER NOT NULL,             -- 384 | 1024
    normalize   INTEGER NOT NULL DEFAULT 1,
    vec_table   TEXT NOT NULL,                -- the vec0 table name for this model
    created_at  TEXT NOT NULL,
    meta_json   TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS embedding_jobs (
    id           TEXT PRIMARY KEY,
    message_id   TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    model_id     TEXT NOT NULL,               -- target model (not FK-enforced: model rows may post-date jobs)
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending','running','done','failed')),
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT NULL,
    enqueued_at  TEXT NOT NULL,
    started_at   TEXT NULL,
    finished_at  TEXT NULL,
    UNIQUE(message_id, model_id)
);
CREATE INDEX IF NOT EXISTS idx_embedding_jobs_status ON embedding_jobs(status, enqueued_at);

CREATE TABLE IF NOT EXISTS message_embeddings (
    message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    model_id    TEXT NOT NULL,
    vec_rowid   INTEGER NOT NULL,             -- rowid in the model's vec0 table (= messages.rowid by construction)
    text_hash   TEXT NULL,                    -- sha256 of the embedded text; NULL-ok now (bodies are write-once)
    created_at  TEXT NOT NULL,
    PRIMARY KEY (message_id, model_id)
);
