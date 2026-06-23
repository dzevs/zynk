//! zynk fork (ADR 0002 / ADR 0006) M5b B2 — migration `0002_embedding_index.sql`
//! adds the three plain embedding-index tables (`embedding_models`, `embedding_jobs`,
//! `message_embeddings`) and creates NO `vec0` virtual table. The `vec0` table is
//! created LAZILY by the embedding worker at RUNTIME (a legitimate, separate behavior
//! covered by the `src/zynk/embedding_worker.rs` unit tests, e.g.
//! `worker_embeds_pending_job_to_done`), NOT by the migrator.
//!
//! The invariant under test is "migration 0002 is extension-free / vec0 is out of the
//! migrations" — NOT "no vec0 exists after a server starts" (after server startup the
//! worker may legitimately have created `message_vec_fake_1`). To prove the migration
//! property DETERMINISTICALLY (without racing the server-spawned worker — ARB-M5B-001),
//! this test migrates a temp DB via the IN-PROCESS `zynk zynk query` CLI (M5a): it
//! opens the DB and runs `MIGRATOR` on a fresh file, but NEVER spawns the headless
//! server or the embedding worker (those are only started by `run_server`). So the
//! resulting schema reflects ONLY the migrations.
//!
//! NOTE on the proof: a plain sqlx connection (no sqlite-vec extension loaded) can READ
//! `sqlite_master` even when vec0 virtual tables exist — so the proof is the ABSENCE of
//! any `USING vec0` entry (the migration created no vec0 table), NOT that a plain open
//! would fail. A separate static check asserts the migration FILE declares no vec0.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Executor, Row, SqliteConnection};

fn unique_base() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/zynk-embed-migtest-{}-{nanos}",
        std::process::id()
    ))
}

/// Migrate an isolated temp DB WITHOUT spawning the server/worker, by running the
/// one-shot, in-process `zynk zynk query` CLI (M5a). The query path opens via
/// `open_query_readonly`, which runs `MIGRATOR` on a fresh DB — but it never spawns the
/// headless server or the embedding worker (only `run_server` does), so the worker never
/// runs and the schema reflects ONLY the migrations. Returns `(base_dir, migrated_db)`.
fn migrate_isolated_db() -> (PathBuf, PathBuf) {
    let base = unique_base();
    let config_home = base.join("config");
    let sqlite_home = base.join("sqlite");
    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&sqlite_home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .arg("zynk")
        .arg("query")
        .arg("zzz_migration_probe")
        // Isolate DB-path resolution: an empty XDG_CONFIG_HOME means no `[zynk]
        // sqlite_home` config override, so ZYNK_SQLITE_HOME is authoritative.
        .env("XDG_CONFIG_HOME", &config_home)
        .env("ZYNK_SQLITE_HOME", &sqlite_home)
        .env_remove("ZYNK_HOME")
        .env_remove("ZYNK_SOCKET_PATH")
        .env_remove("ZYNK_CLIENT_SOCKET_PATH")
        .env_remove("ZYNK_STARTUP_CWD")
        .output()
        .expect("run zynk zynk query (migrate)");
    // The query runs MIGRATOR on open; a fresh DB yields an empty result (exit 0). The
    // migration applies regardless of the (empty) query outcome.
    assert!(
        out.status.success(),
        "zynk query (migrate) should succeed on a fresh DB: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let db = sqlite_home.join("zynk.db");
    assert!(
        db.is_file(),
        "zynk query must create + migrate the DB at {}",
        db.display()
    );
    (base, db)
}

/// Re-run the worker-free `zynk zynk query` CLI against an ALREADY-migrated DB (same
/// `XDG_CONFIG_HOME`/`ZYNK_SQLITE_HOME` as `migrate_isolated_db`). The opener runs
/// `MIGRATOR` again on every open; this asserts that re-running over an applied
/// migration set is idempotent (exits 0, no migration error). Returns the CLI status.
fn reopen_migrated_db(base: &Path) -> std::process::ExitStatus {
    let config_home = base.join("config");
    let sqlite_home = base.join("sqlite");
    Command::new(env!("CARGO_BIN_EXE_zynk"))
        .arg("zynk")
        .arg("query")
        .arg("zzz_reopen_probe")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("ZYNK_SQLITE_HOME", &sqlite_home)
        .env_remove("ZYNK_HOME")
        .env_remove("ZYNK_SOCKET_PATH")
        .env_remove("ZYNK_CLIENT_SOCKET_PATH")
        .env_remove("ZYNK_STARTUP_CWD")
        .output()
        .expect("re-run zynk zynk query (re-migrate)")
        .status
}

fn sqlite_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Open the migrated DB with a plain sqlx connection (NO sqlite-vec extension loaded).
async fn open_db(db: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(db)
            .create_if_missing(false),
    )
    .await
    .unwrap()
}

fn table_names(db: &Path) -> Vec<String> {
    sqlite_runtime().block_on(async {
        let mut conn = open_db(db).await;
        sqlx::query("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").unwrap())
            .collect()
    })
}

#[test]
fn migration_0002_adds_embedding_tables_without_vec0() {
    let (base, db) = migrate_isolated_db();

    let tables = table_names(&db);

    // The three net-new embedding-index tables exist.
    for expected in ["embedding_models", "embedding_jobs", "message_embeddings"] {
        assert!(
            tables.iter().any(|t| t == expected),
            "expected table `{expected}` after migration 0002; got {tables:?}"
        );
    }

    // The M2 tables are still present + intact.
    for expected in [
        "messages",
        "conversations",
        "conversation_participants",
        "delivery_events",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "expected M2 table `{expected}` to still exist; got {tables:?}"
        );
    }
    assert!(
        tables.iter().any(|t| t == "messages_fts"),
        "expected M2 fts table `messages_fts` to still exist; got {tables:?}"
    );

    // The migrator created NO vec0 table: no `message_vec*` table name, and no
    // `sqlite_master` entry whose SQL uses `vec0`. Because this DB was migrated by the
    // worker-free `zynk query` path, the worker never ran — so this is deterministic.
    assert!(
        !tables.iter().any(|t| t.starts_with("message_vec")),
        "migration 0002 must NOT create a vec0 table; found {tables:?}"
    );
    let vec0_entries: i64 = sqlite_runtime().block_on(async {
        let mut conn = open_db(&db).await;
        sqlx::query("SELECT COUNT(*) AS n FROM sqlite_master WHERE sql LIKE '%USING vec0%'")
            .fetch_one(&mut conn)
            .await
            .unwrap()
            .try_get("n")
            .unwrap()
    });
    assert_eq!(
        vec0_entries, 0,
        "migration 0002 must NOT create any `USING vec0` virtual table"
    );

    // The status/enqueue index exists.
    let index_count: i64 = sqlite_runtime().block_on(async {
        let mut conn = open_db(&db).await;
        sqlx::query(
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='index' AND name='idx_embedding_jobs_status'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
    });
    assert_eq!(
        index_count, 1,
        "expected idx_embedding_jobs_status index to exist"
    );

    let _ = fs::remove_dir_all(&base);
}

/// Static guard (no DB): the migration FILE itself declares no vec0 / virtual table —
/// the vec0 table is created lazily by the worker, never by the migrator (ADR 0006).
#[test]
fn migration_0002_file_declares_no_virtual_table() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations/zynk/0002_embedding_index.sql"
    );
    let sql = fs::read_to_string(path).expect("read migration 0002");
    let lc = sql.to_lowercase();
    assert!(
        !lc.contains("using vec0"),
        "migration 0002 must not declare a `USING vec0` table (vec0 is worker-lazy)"
    );
    // Check the STATEMENT, not a comment mention: the file's comment legitimately
    // explains that "the vec0 virtual table is created lazily by the worker", so we
    // assert no `CREATE VIRTUAL TABLE` statement exists (not just the words).
    assert!(
        !lc.contains("create virtual table"),
        "migration 0002 must contain no `CREATE VIRTUAL TABLE` (vec0 is created lazily by the worker)"
    );
}

#[test]
fn embedding_jobs_status_check_rejects_bogus() {
    let (base, db) = migrate_isolated_db();

    // Turn FK enforcement OFF for this connection so the ONLY barrier left is the
    // `status` CHECK constraint — this isolates the CHECK as the rejection cause
    // (otherwise the missing `message_id` FK could be what rejects the row).
    let (ok_valid, err_bogus) = sqlite_runtime().block_on(async {
        let mut conn = open_db(&db).await;
        conn.execute("PRAGMA foreign_keys = OFF").await.unwrap();

        // A valid status inserts fine (proves the table + columns are usable).
        let ok_valid = sqlx::query(
            "INSERT INTO embedding_jobs (id, message_id, model_id, status, enqueued_at) VALUES (?, ?, ?, 'pending', ?)",
        )
        .bind("job-ok")
        .bind("msg-missing")
        .bind("model-x")
        .bind("2026-01-01T00:00:00Z")
        .execute(&mut conn)
        .await;

        // A bogus status is rejected by the CHECK constraint alone.
        let err_bogus = sqlx::query(
            "INSERT INTO embedding_jobs (id, message_id, model_id, status, enqueued_at) VALUES (?, ?, ?, 'bogus', ?)",
        )
        .bind("job-bogus")
        .bind("msg-missing")
        .bind("model-x")
        .bind("2026-01-01T00:00:00Z")
        .execute(&mut conn)
        .await;

        (ok_valid, err_bogus)
    });

    assert!(
        ok_valid.is_ok(),
        "embedding_jobs must accept a valid status with FKs off: {ok_valid:?}"
    );
    assert!(
        err_bogus.is_err(),
        "embedding_jobs must reject status='bogus' (CHECK constraint)"
    );

    let _ = fs::remove_dir_all(&base);
}

/// Feature #107 (IM2): migration `0003_trace_index.sql` adds the partial expression
/// index `idx_messages_trace_id` on `json_extract(meta_json, '$.trace_id')`. It must
/// apply cleanly on a fresh DB (the worker-free `zynk query` path runs MIGRATOR on
/// open) and leave the M2/M5b tables intact.
#[test]
fn migration_0003_adds_trace_partial_index() {
    let (base, db) = migrate_isolated_db();

    // The partial trace-id index exists after migration.
    let index_count: i64 = sqlite_runtime().block_on(async {
        let mut conn = open_db(&db).await;
        sqlx::query(
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='index' AND name='idx_messages_trace_id'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
    });
    assert_eq!(
        index_count, 1,
        "expected idx_messages_trace_id index to exist after migration 0003"
    );

    // It is a PARTIAL index: its `sql` carries the `WHERE ... IS NOT NULL` predicate, so
    // old rows (no trace_id) are excluded from the index entirely.
    let index_sql: String = sqlite_runtime().block_on(async {
        let mut conn = open_db(&db).await;
        sqlx::query("SELECT sql FROM sqlite_master WHERE name='idx_messages_trace_id'")
            .fetch_one(&mut conn)
            .await
            .unwrap()
            .try_get("sql")
            .unwrap()
    });
    let lc = index_sql.to_lowercase();
    assert!(
        lc.contains("where") && lc.contains("is not null"),
        "idx_messages_trace_id must be a PARTIAL index (WHERE ... IS NOT NULL); got {index_sql:?}"
    );

    // The migration is additive: the messages table is untouched (still present).
    let tables = table_names(&db);
    assert!(
        tables.iter().any(|t| t == "messages"),
        "messages table must still exist after migration 0003; got {tables:?}"
    );

    let _ = fs::remove_dir_all(&base);
}

/// Feature #107 (IM2): re-running the migrator over an ALREADY-migrated DB is
/// idempotent — no error, the index stays exactly once. The sqlx `_sqlx_migrations`
/// ledger means an applied 0003 is not re-run; `CREATE INDEX IF NOT EXISTS` is itself
/// idempotent regardless.
#[test]
fn migration_0003_is_idempotent_on_reopen() {
    let (base, db) = migrate_isolated_db();

    // Re-open the already-migrated DB (runs MIGRATOR again). Must succeed.
    let status = reopen_migrated_db(&base);
    assert!(
        status.success(),
        "re-running the migrator on an already-migrated DB must succeed (idempotent)"
    );

    // The index still exists, exactly once (not duplicated by the re-run).
    let index_count: i64 = sqlite_runtime().block_on(async {
        let mut conn = open_db(&db).await;
        sqlx::query(
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='index' AND name='idx_messages_trace_id'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
    });
    assert_eq!(
        index_count, 1,
        "idx_messages_trace_id must exist exactly once after a re-open (idempotent)"
    );

    let _ = fs::remove_dir_all(&base);
}

/// Static guard (no DB): the migration FILE declares the partial trace index with the
/// `IS NOT NULL` predicate and touches NOTHING else (additive — no ALTER/DROP).
#[test]
fn migration_0003_file_is_additive_partial_index() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations/zynk/0003_trace_index.sql"
    );
    let sql = fs::read_to_string(path).expect("read migration 0003");
    let lc = sql.to_lowercase();
    assert!(
        lc.contains("create index if not exists idx_messages_trace_id"),
        "migration 0003 must create idx_messages_trace_id idempotently"
    );
    assert!(
        lc.contains("json_extract(meta_json, '$.trace_id')"),
        "migration 0003 must index json_extract(meta_json, '$.trace_id')"
    );
    assert!(
        lc.contains("where") && lc.contains("is not null"),
        "migration 0003 must be a PARTIAL index (WHERE ... IS NOT NULL)"
    );
    // Additive: no destructive/altering statements.
    for forbidden in ["alter table", "drop ", "delete from", "update messages"] {
        assert!(
            !lc.contains(forbidden),
            "migration 0003 must be additive; found `{forbidden}`"
        );
    }
}
