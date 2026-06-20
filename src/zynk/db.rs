//! zynk fork: SQLite connection, migration, and recovery helpers (ADR 0003,
//! foreign-DB guard finalized by ADR 0008).

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{Connection, Executor, Row, SqliteConnection};

static MIGRATOR: Migrator = sqlx::migrate!("migrations/zynk");

/// Tables that uniquely mark a native zynk DB lineage. Combined with the sqlx
/// `_sqlx_migrations` ledger, their presence is our positive native-recognition
/// signal (ADR 0008). A DB that is non-empty but lacks this lineage is FOREIGN.
const NATIVE_LINEAGE_TABLES: &[&str] = &["conversations", "messages", "delivery_events"];

/// Foreign-DB classification at a resolved native path (ADR 0008).
///
/// - `Absent`  — no file (or empty/0-byte): native init may create it.
/// - `Empty`   — a valid SQLite file with no user tables: native init migrates.
/// - `Native`  — recognized native lineage (`_sqlx_migrations` + our tables): open.
/// - `Foreign` — non-empty but NOT recognized native (wrapper-era OR any unknown
///   schema): FAIL CLOSED. Never auto-migrate/overwrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbClassification {
    Absent,
    Empty,
    Native,
    /// Foreign DB; `tables` lists the user tables found (for the branded error).
    Foreign {
        tables: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct DbError {
    pub code: &'static str,
    pub message: String,
}

impl DbError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DbError {}

impl From<sqlx::Error> for DbError {
    fn from(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("5") => {
                DbError::new("persistence_busy", err.to_string())
            }
            _ => DbError::new("db_error", err.to_string()),
        }
    }
}

impl From<std::io::Error> for DbError {
    fn from(err: std::io::Error) -> Self {
        DbError::new("db_io_error", err.to_string())
    }
}

pub fn block_on<T>(future: impl Future<Output = Result<T, DbError>>) -> Result<T, DbError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| DbError::new("tokio_runtime_failed", err.to_string()))?;
    rt.block_on(future)
}

pub async fn open_migrated() -> Result<SqliteConnection, DbError> {
    let path = crate::zynk::db_path::db_path();
    open_migrated_at(&path).await
}

pub async fn open_migrated_for_append() -> Result<SqliteConnection, DbError> {
    let path = crate::zynk::db_path::db_path();
    open_migrated_at_without_recovery(&path).await
}

/// Read-only query opener (M5a). Runs `MIGRATOR` (so the DB stays current) but
/// skips orphan recovery, then sets `PRAGMA query_only = 1` so the connection
/// cannot write. `zynk query` uses this so a read NEVER synthesizes a
/// `failed`/`system.recovery` delivery event (the read-only + receipts-server-
/// authoritative invariant). A pure `read_only(true)` connection cannot apply
/// migrations, so open-write-then-`query_only=1` is the chosen pattern.
///
/// #117 note: because this runs `MIGRATOR` before `query_only=1`, a read on a DB
/// that is BEHIND the current schema migrates it. That is correct for production
/// (a read should never see a stale schema), but it means a read against the live
/// `~/.zynk` would migrate it. We do NOT change that here; the test-build DB
/// isolation guarantees no test ever resolves the read path to the live `~/.zynk`
/// (`db_path::resolve_db_path` redirects the default-home branch to a per-process
/// temp under `cfg(test)`, and every bin-spawning integration test pins
/// `ZYNK_SQLITE_HOME` + scrubs `ZYNK_HOME`). A safe future improvement would be to
/// open read-only without migrating when the DB is already at/after the needed
/// version; left out here to avoid destabilizing the read path.
pub async fn open_query_readonly() -> Result<SqliteConnection, DbError> {
    open_query_readonly_at(&crate::zynk::db_path::db_path()).await
}

pub async fn open_query_readonly_at(path: &Path) -> Result<SqliteConnection, DbError> {
    let mut conn = open_migrated_at_without_recovery(path).await?;
    conn.execute("PRAGMA query_only = 1").await?;
    Ok(conn)
}

pub async fn open_migrated_at(path: &Path) -> Result<SqliteConnection, DbError> {
    let mut conn = open_migrated_at_without_recovery(path).await?;
    recover_orphan_messages(&mut conn).await?;
    Ok(conn)
}

pub async fn open_migrated_at_without_recovery(path: &Path) -> Result<SqliteConnection, DbError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // ADR 0008 foreign-DB guard: classify FIRST, with a READ-ONLY connection,
    // BEFORE the writable open below. This matters for byte-immutability: the
    // writable `connect_with` applies `journal_mode = WAL`, which rewrites the
    // SQLite file header (bytes 18-19) on connect. Classifying read-only first
    // means a FOREIGN database is never even touched — we fail closed before any
    // mutation. The guard sits in this shared low-level opener, so every PRODUCT
    // open (open_migrated_at, append, query-readonly, workers) is protected; a
    // fresh dev/tmp DB classifies as Absent/Empty and proceeds unchanged.
    match classify_db_at(path).await? {
        DbClassification::Absent | DbClassification::Empty | DbClassification::Native => {}
        DbClassification::Foreign { tables } => return Err(foreign_db_error(path, &tables)),
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_millis(2000))
        .pragma("foreign_keys", "ON")
        .pragma("page_size", "4096");
    let mut conn = SqliteConnection::connect_with(&options).await?;
    apply_pragmas(&mut conn).await?;
    MIGRATOR
        .run(&mut conn)
        .await
        .map_err(|err| DbError::new("migration_failed", err.to_string()))?;
    Ok(conn)
}

/// Read the user-table set of an OPEN connection (excludes sqlite/fts internals).
async fn user_table_names(conn: &mut SqliteConnection) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query(
        "SELECT name FROM sqlite_master \
         WHERE type='table' \
           AND name NOT LIKE 'sqlite_%' \
           AND name NOT LIKE '%_fts' \
           AND name NOT LIKE '%_fts_%' \
           AND name NOT LIKE '%_data' \
           AND name NOT LIKE '%_idx' \
           AND name NOT LIKE '%_content' \
           AND name NOT LIKE '%_docsize' \
           AND name NOT LIKE '%_config' \
         ORDER BY name",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|row| row.try_get::<String, _>("name").ok())
        .collect())
}

/// Classify an OPEN connection (ADR 0008). `_sqlx_migrations` + all native
/// lineage tables ⇒ Native; no user tables ⇒ Empty; otherwise Foreign.
async fn classify_open_conn(conn: &mut SqliteConnection) -> Result<DbClassification, DbError> {
    let tables = user_table_names(conn).await?;
    if tables.is_empty() {
        return Ok(DbClassification::Empty);
    }
    let has_migrations = sqlx::query(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations' LIMIT 1",
    )
    .fetch_optional(&mut *conn)
    .await?
    .is_some();
    let has_all_lineage = NATIVE_LINEAGE_TABLES
        .iter()
        .all(|t| tables.iter().any(|name| name == t));
    if has_migrations && has_all_lineage {
        Ok(DbClassification::Native)
    } else {
        Ok(DbClassification::Foreign { tables })
    }
}

/// Classify the DB at `path` WITHOUT mutating it (ADR 0008). Opens read-only
/// (never `create_if_missing`), so a missing/0-byte file is `Absent`. Used by
/// `zynk db status` and as the basis for the open-time guard.
pub async fn classify_db_at(path: &Path) -> Result<DbClassification, DbError> {
    match std::fs::metadata(path) {
        Err(_) => return Ok(DbClassification::Absent),
        Ok(meta) if meta.len() == 0 => return Ok(DbClassification::Absent),
        Ok(_) => {}
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    let mut conn = SqliteConnection::connect_with(&options).await?;
    let class = classify_open_conn(&mut conn).await?;
    let _ = conn.close().await;
    Ok(class)
}

/// The zynk-branded fail-closed error for a foreign DB at `path`.
pub fn foreign_db_error(path: &Path, tables: &[String]) -> DbError {
    let found = if tables.is_empty() {
        "unrecognized schema".to_string()
    } else {
        format!("found tables: {}", tables.join(", "))
    };
    DbError::new(
        "db_foreign_conflict",
        format!(
            "zynk: refusing to open a non-native database at {} ({found}). \
             zynk will NEVER migrate or overwrite a foreign database. \
             Back up or relocate it first, then let zynk create a native DB — \
             run `zynk db status` to inspect, or `zynk db adopt`/`zynk db backup` \
             to move the existing file aside non-destructively (e.g. \
             {}.wrapper-backup-N).",
            path.display(),
            path.display()
        ),
    )
}

async fn apply_pragmas(conn: &mut SqliteConnection) -> Result<(), DbError> {
    conn.execute("PRAGMA foreign_keys = ON").await?;
    conn.execute("PRAGMA journal_mode = WAL").await?;
    conn.execute("PRAGMA synchronous = NORMAL").await?;
    conn.execute("PRAGMA busy_timeout = 2000").await?;
    conn.execute("PRAGMA page_size = 4096").await?;
    Ok(())
}

pub async fn recover_orphan_messages(conn: &mut SqliteConnection) -> Result<(), DbError> {
    let orphan_ids: Vec<String> = sqlx::query(
        "SELECT id FROM messages WHERE NOT EXISTS (SELECT 1 FROM delivery_events WHERE delivery_events.message_id = messages.id)",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|row| row.try_get::<String, _>("id").ok())
    .collect();

    for message_id in orphan_ids {
        let mut tx = conn.begin().await?;
        let seq_row = sqlx::query(
            "UPDATE messages SET delivery_seq = delivery_seq + 1 WHERE id = ? RETURNING delivery_seq",
        )
        .bind(&message_id)
        .fetch_one(&mut *tx)
        .await?;
        let seq = seq_row.try_get::<i64, _>("delivery_seq")?;
        sqlx::query(
            "INSERT INTO delivery_events (id, message_id, event_type, proof_source, seq, timestamp, payload_json) VALUES (?, ?, 'failed', 'system.recovery', ?, ?, ?)",
        )
        .bind(crate::zynk::message::new_prefixed_id("evt"))
        .bind(&message_id)
        .bind(seq)
        .bind(crate::zynk::message::now_rfc3339())
        .bind(r#"{"recovery":"orphaned_message_without_event"}"#)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_error_display_includes_code() {
        let e = DbError::new("x", "y");
        assert_eq!(e.to_string(), "x: y");
    }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "zynk-{tag}-{}-{}.db",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("t")
        ))
    }

    fn plant_foreign_db(path: &std::path::Path, ddl: &str) {
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true),
            )
            .await?;
            conn.execute(ddl).await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn open_fails_closed_on_wrapper_schema_and_does_not_mutate() {
        // ADR 0008: a wrapper-era schema is FOREIGN — fail closed, branded code,
        // and the foreign bytes MUST be byte-identical afterward.
        let path = tmp_db("wrapper-schema");
        plant_foreign_db(&path, "CREATE TABLE projects (id TEXT PRIMARY KEY)");
        let before = std::fs::read(&path).unwrap();

        let err = block_on(open_migrated_at(&path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict", "{}", err.message);
        assert!(err.message.contains("zynk:"), "branded: {}", err.message);
        assert!(err.message.contains(&path.display().to_string()));

        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "foreign DB must not be modified");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_fails_closed_on_unknown_schema() {
        // ADR 0008: ANY non-empty, non-native schema is FOREIGN (not just
        // known wrapper tables).
        let path = tmp_db("unknown-schema");
        plant_foreign_db(&path, "CREATE TABLE totally_unknown (x INTEGER)");
        let err = block_on(open_migrated_at(&path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn classify_absent_empty_native_foreign() {
        // Absent (no file).
        let absent = tmp_db("classify-absent");
        let _ = std::fs::remove_file(&absent);
        assert_eq!(
            block_on(classify_db_at(&absent)).unwrap(),
            DbClassification::Absent
        );

        // Empty (valid sqlite file, no user tables).
        let empty = tmp_db("classify-empty");
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&empty)
                    .create_if_missing(true),
            )
            .await?;
            // touch the file so it is non-zero but still has no user tables
            conn.execute("PRAGMA user_version = 0").await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        assert_eq!(
            block_on(classify_db_at(&empty)).unwrap(),
            DbClassification::Empty
        );

        // Native (migrate a fresh DB, then classify).
        let native = tmp_db("classify-native");
        block_on(open_migrated_at_without_recovery(&native)).unwrap();
        assert_eq!(
            block_on(classify_db_at(&native)).unwrap(),
            DbClassification::Native
        );

        // Foreign (non-native user table).
        let foreign = tmp_db("classify-foreign");
        plant_foreign_db(&foreign, "CREATE TABLE projects (id TEXT PRIMARY KEY)");
        match block_on(classify_db_at(&foreign)).unwrap() {
            DbClassification::Foreign { tables } => {
                assert!(tables.iter().any(|t| t == "projects"))
            }
            other => panic!("expected Foreign, got {other:?}"),
        }

        for p in [absent, empty, native, foreign] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn fresh_path_initializes_native_then_reopens() {
        // Absent -> native init; reopening an existing native DB succeeds.
        let path = tmp_db("fresh-native");
        let _ = std::fs::remove_file(&path);
        block_on(open_migrated_at(&path)).unwrap();
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
        // Re-open the now-native DB: must succeed (guard passes Native).
        block_on(open_migrated_at(&path)).unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_query_readonly_rejects_writes() {
        // M5a: the query opener sets PRAGMA query_only=1, so a read can never
        // synthesize a recovery/delivery event.
        let path = std::env::temp_dir().join(format!(
            "zynk-query-readonly-test-{}-{}.db",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("test")
        ));
        // Create + migrate via the append opener first (so MIGRATOR has run).
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        let result = block_on(async {
            let mut conn = open_query_readonly_at(&path).await?;
            conn.execute("CREATE TABLE zzz_probe(x INTEGER)").await?;
            Ok::<(), DbError>(())
        });
        assert!(
            result.is_err(),
            "open_query_readonly must reject writes (query_only)"
        );
        let _ = std::fs::remove_file(path);
    }
}
