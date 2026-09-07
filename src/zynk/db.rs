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
/// - `Empty`   — a valid SQLite file with no user tables (or only an EMPTY `_sqlx_migrations`
///   ledger, i.e. a native init in progress/aborted): native init migrates.
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
    // Serialize classify + migrate across PROCESSES (see `InitLock`): two zynk processes opening a
    // fresh shared DB at once (two named-session servers, or a CLI command racing a starting server)
    // must not observe each other's half-initialized state.
    let _init_lock = InitLock::acquire(path).await?;
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

/// Exclusive advisory lock beside the DB (`<db>.init-lock`) that serializes first-time initialization
/// across processes. sqlx creates its `_sqlx_migrations` ledger before the first migration commits, so
/// without this a second opener could see a ledger-only DB, classify it as FOREIGN (ADR 0008) and exit —
/// the named-session startup race. The lock spans only the open (milliseconds once the DB exists) and the
/// OS releases it if the holder dies. The lock file itself carries no data.
struct InitLock(std::fs::File);

/// Upper bound on waiting for another process's first-time initialization (a fresh DB migrates in
/// well under a second; this only caps a pathological holder).
const INIT_LOCK_DEADLINE: Duration = Duration::from_secs(30);
const INIT_LOCK_POLL: Duration = Duration::from_millis(10);

fn init_lock_path(db_path: &Path) -> std::path::PathBuf {
    let file_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "zynk.db".to_string());
    db_path.with_file_name(format!("{file_name}.init-lock"))
}

impl InitLock {
    async fn acquire(db_path: &Path) -> Result<Self, DbError> {
        Self::acquire_with_deadline(db_path, INIT_LOCK_DEADLINE).await
    }

    /// Cooperative acquisition: `try_lock` + an async sleep instead of a blocking `lock()`, so two
    /// openers polled on ONE executor cannot deadlock (the holder must be able to resume and release)
    /// and the wait is bounded by `deadline`.
    async fn acquire_with_deadline(db_path: &Path, deadline: Duration) -> Result<Self, DbError> {
        let lock_path = init_lock_path(db_path);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= deadline {
                        return Err(DbError::new(
                            "db_init_lock_timeout",
                            format!(
                                "zynk: another zynk process has held the database init lock at {} \
                                 for more than {deadline:?}; the lock is released when that process \
                                 finishes initializing or exits — retry afterwards",
                                lock_path.display()
                            ),
                        ));
                    }
                    tokio::time::sleep(INIT_LOCK_POLL).await;
                }
                Err(std::fs::TryLockError::Error(err)) => return Err(err.into()),
            }
        }
    }
}

impl Drop for InitLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Read the user-table set of an OPEN connection (excludes sqlite/fts internals).
async fn user_table_names(conn: &mut SqliteConnection) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query(
        // GLOB (literal `_`), not LIKE (`_` = any one character): this only shapes the DISPLAY list
        // — classification uses `all_user_table_names` — but the patterns must not over-match.
        "SELECT name FROM sqlite_master \
         WHERE type='table' \
           AND name NOT GLOB 'sqlite_*' \
           AND name NOT GLOB '*_fts' \
           AND name NOT GLOB '*_fts_*' \
           AND name NOT GLOB '*_data' \
           AND name NOT GLOB '*_idx' \
           AND name NOT GLOB '*_content' \
           AND name NOT GLOB '*_docsize' \
           AND name NOT GLOB '*_config' \
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
    // Decide on the COMPLETE table set. `user_table_names` is a DISPLAY view that hides FTS-shadow
    // and similarly suffixed names (`*_data`, `*_config`, …); a foreign DB whose tables happen to carry
    // those suffixes must still fail closed, so the display view never drives this decision.
    let all_tables = all_user_table_names(conn).await?;
    if all_tables.is_empty() {
        return Ok(DbClassification::Empty);
    }
    // sqlx creates its `_sqlx_migrations` ledger BEFORE the first migration's transaction commits.
    // A DB whose ONLY table is that ledger — with NO recorded migrations — is a native init in
    // progress (another zynk process) or an aborted one: there is no foreign data to protect, so it is
    // `Empty`, not `Foreign`. A ledger that already RECORDS migrations but lacks our tables stays
    // Foreign (unknown lineage → fail closed, ADR 0008).
    if all_tables.len() == 1 && all_tables[0] == "_sqlx_migrations" {
        let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&mut *conn)
            .await?;
        if recorded == 0 {
            return Ok(DbClassification::Empty);
        }
    }
    let has_migrations = all_tables.iter().any(|name| name == "_sqlx_migrations");
    let has_all_lineage = NATIVE_LINEAGE_TABLES
        .iter()
        .all(|t| all_tables.iter().any(|name| name == t));
    if has_migrations && has_all_lineage {
        return Ok(DbClassification::Native);
    }
    let tables = user_table_names(conn).await?;
    Ok(DbClassification::Foreign {
        tables: if tables.is_empty() {
            all_tables
        } else {
            tables
        },
    })
}

/// Every user table (only SQLite's own `sqlite_*` internals excluded) — the set classification must
/// reason about, as opposed to the display view in `user_table_names`.
async fn all_user_table_names(conn: &mut SqliteConnection) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query(
        // GLOB, not LIKE: in LIKE `_` is a one-character wildcard, so 'sqlite_%' would also hide a
        // user table such as `sqliteCustomer`. SQLite's reserved prefix is literally `sqlite_`.
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*' ORDER BY name",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("name").ok())
        .collect())
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

    /// sqlx creates its `_sqlx_migrations` ledger BEFORE the first migration's transaction commits.
    /// A DB whose only user table is that (empty) ledger is a native init in progress or an aborted
    /// init — there is no foreign data to protect, so it must classify as Empty, not Foreign.
    const SQLX_LEDGER_DDL: &str = "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, \
         description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
         success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)";

    #[test]
    fn classify_empty_sqlx_ledger_only_db_as_empty() {
        let path = tmp_db("classify-ledger-only");
        plant_foreign_db(&path, SQLX_LEDGER_DDL);
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Empty
        );
        // ...and opening it completes the native init instead of failing closed.
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
    }

    #[test]
    fn classify_sqlx_ledger_with_rows_but_no_native_tables_as_foreign() {
        // ADR 0008 boundary: a ledger that already RECORDS migrations but has none of our tables is
        // an unknown/corrupted lineage — keep failing closed.
        let path = tmp_db("classify-ledger-rows");
        plant_foreign_db(
            &path,
            &format!(
                "{SQLX_LEDGER_DDL}; INSERT INTO _sqlx_migrations \
                 (version, description, success, checksum, execution_time) \
                 VALUES (1, 'other app', 1, x'00', 0)"
            ),
        );
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { tables } => assert_eq!(tables, vec!["_sqlx_migrations"]),
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    #[test]
    fn foreign_db_with_empty_ledger_and_suffix_filtered_tables_fails_closed() {
        // ADR 0008 regression (Gate-2 P1): `user_table_names` hides `*_data` / `*_config` / FTS-shadow
        // names for DISPLAY; classification must still see them. An empty sqlx ledger next to real
        // foreign data is FOREIGN — never migrated — and the bytes stay identical.
        let path = tmp_db("classify-ledger-plus-filtered");
        plant_foreign_db(
            &path,
            &format!(
                "{SQLX_LEDGER_DDL}; \
                 CREATE TABLE customer_data (secret TEXT); INSERT INTO customer_data VALUES ('s'); \
                 CREATE TABLE app_config (k TEXT, v TEXT); INSERT INTO app_config VALUES ('k', 'v')"
            ),
        );
        let before = std::fs::read(&path).unwrap();
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
        let err = block_on(open_migrated_at(&path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict", "{}", err.message);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "foreign DB must not be modified"
        );
    }

    #[test]
    fn foreign_db_with_only_suffix_filtered_table_fails_closed() {
        // Same class, no ledger at all: a DB whose only table is hidden by the display filter used to
        // classify as Empty and get migrated.
        let path = tmp_db("classify-only-filtered");
        plant_foreign_db(
            &path,
            "CREATE TABLE customer_data (secret TEXT); INSERT INTO customer_data VALUES ('s')",
        );
        let before = std::fs::read(&path).unwrap();
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
        let err = block_on(open_migrated_at(&path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict", "{}", err.message);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn concurrent_opens_on_one_runtime_do_not_deadlock() {
        // Gate-2 P2: two opens of a fresh DB joined on ONE current-thread runtime. A blocking lock
        // inside the async opener deadlocks this (the first future holds the lock across awaits, the
        // second blocks the only thread). The init lock must be acquired cooperatively.
        let path = tmp_db("init-same-runtime");
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let result = block_on(async {
                let (a, b) = tokio::join!(
                    open_migrated_at_without_recovery(&worker_path),
                    open_migrated_at_without_recovery(&worker_path)
                );
                a?;
                b?;
                Ok::<(), DbError>(())
            });
            let _ = tx.send(result);
        });
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(result) => result.unwrap(),
            Err(_) => {
                panic!("two opens on one runtime deadlocked: the init lock must be cooperative")
            }
        }
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
    }

    #[test]
    fn init_lock_contention_is_bounded() {
        // Gate-2 P2: an init lock held by another process must not stall an opener forever.
        let path = tmp_db("init-lock-contention");
        let lock_path = init_lock_path(&path);
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        holder.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let result = block_on(InitLock::acquire_with_deadline(
                &worker_path,
                Duration::from_millis(300),
            ))
            .map(|_guard| ());
            let _ = tx.send(result);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Err(err)) => assert_eq!(err.code, "db_init_lock_timeout", "{}", err.message),
            Ok(Ok(())) => panic!("acquired an init lock that another holder owns"),
            Err(_) => panic!("init-lock wait is unbounded"),
        }
        holder.unlock().unwrap();
    }

    #[test]
    fn foreign_table_named_with_sqlite_prefix_lookalike_fails_closed() {
        // Gate-2 round-2 P1: `LIKE 'sqlite_%'` treats `_` as a one-character wildcard, so a user table
        // named `sqliteCustomer` was dropped from the table set and — next to an empty ledger — the DB
        // classified Empty and got migrated. The reserved prefix must be matched literally.
        for (tag, ddl) in [
            (
                "sqlite-lookalike-with-ledger",
                format!(
                    "{SQLX_LEDGER_DDL}; CREATE TABLE sqliteCustomer (secret TEXT); \
                     INSERT INTO sqliteCustomer VALUES ('s')"
                ),
            ),
            (
                "sqlite-lookalike-no-ledger",
                "CREATE TABLE sqliteCustomer (secret TEXT); INSERT INTO sqliteCustomer VALUES ('s')"
                    .to_string(),
            ),
        ] {
            let path = tmp_db(tag);
            plant_foreign_db(&path, &ddl);
            let before = std::fs::read(&path).unwrap();
            match block_on(classify_db_at(&path)).unwrap() {
                DbClassification::Foreign { tables } => {
                    assert!(tables.iter().any(|t| t == "sqliteCustomer"), "{tag}: {tables:?}")
                }
                other => panic!("{tag}: expected Foreign, got {other:?}"),
            }
            let err = block_on(open_migrated_at(&path)).unwrap_err();
            assert_eq!(err.code, "db_foreign_conflict", "{tag}: {}", err.message);
            assert_eq!(std::fs::read(&path).unwrap(), before, "{tag}: bytes changed");
        }
    }

    #[test]
    fn concurrent_first_opens_of_a_fresh_db_all_succeed() {
        // Regression for the named-session startup race: two zynk servers sharing a data home and
        // starting at once used to make the second one see the half-initialized DB (ledger only)
        // as FOREIGN and exit. Init is now serialized by an advisory lock beside the DB.
        let path = tmp_db("classify-concurrent-init");
        let _ = std::fs::remove_file(&path);
        let openers = 6;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(openers));
        let handles: Vec<_> = (0..openers)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    block_on(open_migrated_at_without_recovery(&path)).map(|_| ())
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
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
