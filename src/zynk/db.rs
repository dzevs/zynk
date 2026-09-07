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
    // ADR 0008 foreign-DB guard: classify FIRST, with a READ-ONLY, sidecar-safe connection, BEFORE
    // the writable open below. This matters for byte-immutability: the writable `connect_with`
    // applies `journal_mode = WAL`, which rewrites the SQLite file header (bytes 18-19) on connect.
    // Classifying read-only first means a FOREIGN database is never even touched — we fail closed
    // before any mutation. The guard sits in this shared low-level opener, so every PRODUCT open
    // (open_migrated_at, append, query-readonly, workers) is protected.
    //
    // The cross-process init lock (see `InitLock`) is taken ONLY when initialization is actually
    // needed (Absent/Empty): two processes opening a fresh shared DB at once must not observe each
    // other's half-initialized state, and a native DB with PENDING migrations is upgraded under the
    // same lock (sqlx's SQLite migrator has none of its own); only a fully CURRENT native DB opens
    // without waiting on any lock holder. After acquiring, inspect again — the other process may have
    // finished.
    let mut _init_lock = None;
    match inspect_db_at(path).await? {
        // Fully CURRENT native DB: nothing to initialize or migrate, so never wait on a lock holder.
        (DbClassification::Native, false) => {}
        (DbClassification::Foreign { tables }, _) => return Err(foreign_db_error(path, &tables)),
        // Absent, new, or native with PENDING migrations: serialize with every other opener.
        (DbClassification::Absent | DbClassification::Empty | DbClassification::Native, _) => {
            _init_lock = Some(InitLock::acquire(path).await?);
            if let (DbClassification::Foreign { tables }, _) = inspect_db_at(path).await? {
                return Err(foreign_db_error(path, &tables));
            }
        }
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

/// Upper bound on waiting for another process's first-time initialization. A fresh DB migrates in
/// well under a second even under load; this caps a pathological holder, and a server start that hits
/// it is FATAL (never a degraded server), so keep it short enough for `session stop`'s contract.
const INIT_LOCK_DEADLINE: Duration = Duration::from_secs(5);
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
    // GLOB (literal `_`), not LIKE (`_` = any one character): this only shapes the DISPLAY list —
    // classification uses `schema_objects` — but the patterns must not over-match. Names are read as
    // bytes and labeled losslessly when they are not valid UTF-8.
    let rows = sqlx::query(
        "SELECT CAST(name AS BLOB) AS name FROM sqlite_master \
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
    let mut names = Vec::with_capacity(rows.len());
    for row in rows {
        let name = row.try_get::<Vec<u8>, _>("name")?;
        names.push(match String::from_utf8(name) {
            Ok(name) => name,
            Err(err) => format!("<non-utf8 0x{}>", hex(err.as_bytes())),
        });
    }
    Ok(names)
}

/// Classify an OPEN connection (ADR 0008) from the COMPLETE, LOSSLESS schema-object set; positive
/// native recognition is by migration PROVENANCE (our ledger checksums), never by table names.
///
/// All reads happen inside ONE read transaction so the schema objects, the ledger rows and the
/// currentness verdict come from a single SQLite snapshot: without that, a concurrent initializer's
/// first migration could commit between two SELECTs and a genuine native initialization would look
/// like a foreign ledger (Gate-2 round 4, item 2). The transaction is rolled back before returning,
/// so a caller that goes on to wait for the init lock holds no snapshot while waiting.
async fn classify_open_conn(
    conn: &mut SqliteConnection,
) -> Result<(DbClassification, bool), DbError> {
    classify_open_conn_with_hook(conn, &mut || {}).await
}

/// The production wrapper with an interleaving hook that runs between the schema-object read and the
/// ledger read — a no-op in production; tests use it to commit a concurrent initializer at exactly
/// that point and prove the wrapper's read transaction pins ONE snapshot.
async fn classify_open_conn_with_hook(
    conn: &mut SqliteConnection,
    after_schema_read: &mut dyn FnMut(),
) -> Result<(DbClassification, bool), DbError> {
    let mut tx = conn.begin().await?;
    let inspection = classify_snapshot(&mut tx, after_schema_read).await;
    tx.rollback().await?;
    inspection
}

/// The classification proper, evaluated on whatever snapshot `conn` currently holds (a read
/// transaction the caller began — `classify_open_conn` — or, in tests, one pinned deliberately).
async fn classify_snapshot(
    conn: &mut SqliteConnection,
    after_schema_read: &mut dyn FnMut(),
) -> Result<(DbClassification, bool), DbError> {
    // Every schema object counts — tables, views, indexes and triggers — with names read as bytes so
    // a name that is not valid UTF-8 still counts (it is never dropped). `user_table_names` is a
    // DISPLAY view that hides FTS-shadow/suffixed names; it never drives this decision.
    let objects = schema_objects(conn).await?;
    after_schema_read();
    if objects.is_empty() {
        return Ok((DbClassification::Empty, true));
    }
    let ledger_present = objects
        .iter()
        .any(|object| object.kind == "table" && object.name == b"_sqlx_migrations");
    if objects.len() == 1 && ledger_present {
        // sqlx creates its `_sqlx_migrations` ledger BEFORE the first migration's transaction commits.
        // A DB whose ONLY object is that ledger with NO recorded migrations is a native init in
        // progress (another zynk process) or an aborted one: nothing to protect → `Empty`. A ledger
        // that already RECORDS migrations but nothing else is unknown lineage → Foreign.
        if ledger_rows(conn).await?.is_empty() {
            return Ok((DbClassification::Empty, true));
        }
        return Ok((
            DbClassification::Foreign {
                tables: vec!["_sqlx_migrations".to_string()],
            },
            false,
        ));
    }
    if ledger_present {
        if let Some(needs_migration) = native_lineage(conn, &objects).await? {
            return Ok((DbClassification::Native, needs_migration));
        }
    }
    let mut tables = user_table_names(conn).await?;
    if tables.is_empty() {
        tables = objects.iter().map(SchemaObject::display).collect();
    }
    Ok((DbClassification::Foreign { tables }, false))
}

/// Positive native recognition = migration PROVENANCE, never table names. Returns
/// `Some(needs_migration)` when the ledger is ours, `None` otherwise.
///
/// The rule, in order:
///
/// 1. every recorded row must have `success = 1` — a failed/dirty row is an unknown state, not ours;
/// 2. the recorded versions we know must be a valid PREFIX of the built-in migration list and carry OUR
///    checksums — our migrator applies in order, so `{1, 3}` is not a state it can produce;
/// 3. a recorded version we do not know is tolerated only when it is NEWER than everything built in
///    (a database migrated by a newer zynk stays ours; the migrator then reports the mismatch);
/// 4. the lineage tables must exist.
///
/// This recognizes accidental foreign lineage; it is not authentication against a deliberately
/// fabricated ledger (checksums are public), and it does not need to be for ADR 0008's threat model.
async fn native_lineage(
    conn: &mut SqliteConnection,
    objects: &[SchemaObject],
) -> Result<Option<bool>, DbError> {
    let rows = ledger_rows(conn).await?;
    if rows.is_empty() || rows.iter().any(|(_, _, success)| !success) {
        return Ok(None);
    }
    let ours: Vec<_> = MIGRATOR.iter().collect();
    let newest_ours = ours.iter().map(|m| m.version).max().unwrap_or(0);
    let mut known: Vec<i64> = Vec::new();
    for (version, checksum, _) in &rows {
        match ours.iter().find(|m| m.version == *version) {
            Some(m) if m.checksum.as_ref() == checksum.as_slice() => known.push(*version),
            Some(_) => return Ok(None), // known version, foreign checksum
            None if *version > newest_ours => {} // newer zynk
            None => return Ok(None),    // unknown older version interleaved
        }
    }
    known.sort_unstable();
    let expected_prefix: Vec<i64> = ours.iter().take(known.len()).map(|m| m.version).collect();
    if known.is_empty() || known != expected_prefix {
        return Ok(None);
    }
    let lineage_tables = NATIVE_LINEAGE_TABLES.iter().all(|table| {
        objects
            .iter()
            .any(|object| object.kind == "table" && object.name == table.as_bytes())
    });
    if !lineage_tables {
        return Ok(None);
    }
    // Current = every built-in migration is recorded. Only a CURRENT native DB may open without the
    // init lock: sqlx's SQLite migrator has no cross-process lock of its own, so pending upgrades must
    // be serialized exactly like first-time initialization.
    Ok(Some(known.len() < ours.len()))
}

/// One `sqlite_master` row: `kind` is table/index/view/trigger; `name` is the raw byte string.
struct SchemaObject {
    kind: String,
    name: Vec<u8>,
}

impl SchemaObject {
    fn display(&self) -> String {
        match std::str::from_utf8(&self.name) {
            Ok(name) => format!("{}:{name}", self.kind),
            Err(_) => format!("{}:<non-utf8 0x{}>", self.kind, hex(&self.name)),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every schema object of any type except SQLite's own `sqlite_*` internals, losslessly: a decode
/// failure is an error, never a silently dropped row.
async fn schema_objects(conn: &mut SqliteConnection) -> Result<Vec<SchemaObject>, DbError> {
    let rows = sqlx::query(
        "SELECT type, CAST(name AS BLOB) AS name FROM sqlite_master \
         WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut objects = Vec::with_capacity(rows.len());
    for row in rows {
        objects.push(SchemaObject {
            kind: row.try_get::<String, _>("type")?,
            name: row.try_get::<Vec<u8>, _>("name")?,
        });
    }
    Ok(objects)
}

/// `(version, checksum, success)` of every migration the ledger records.
async fn ledger_rows(conn: &mut SqliteConnection) -> Result<Vec<(i64, Vec<u8>, bool)>, DbError> {
    let rows =
        sqlx::query("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push((
            row.try_get::<i64, _>("version")?,
            row.try_get::<Vec<u8>, _>("checksum")?,
            row.try_get::<bool, _>("success")?,
        ));
    }
    Ok(out)
}

/// Classify the DB at `path` WITHOUT mutating it (ADR 0008). Opens read-only
/// (never `create_if_missing`), so a missing/0-byte file is `Absent`. Used by
/// `zynk db status` and as the basis for the open-time guard.
pub async fn classify_db_at(path: &Path) -> Result<DbClassification, DbError> {
    Ok(inspect_db_at(path).await?.0)
}

/// `classify_db_at` plus whether the built-in migrator still has work to do on it (always `true`
/// for Absent/Empty; meaningful for Native). Read-only and sidecar-safe like `classify_db_at`.
async fn inspect_db_at(path: &Path) -> Result<(DbClassification, bool), DbError> {
    match std::fs::metadata(path) {
        Err(_) => return Ok((DbClassification::Absent, true)),
        Ok(meta) if meta.len() == 0 => return Ok((DbClassification::Absent, true)),
        Ok(_) => {}
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    // Durable boundary (ADR 0011): a read-only inspection never modifies EXISTING data bytes — the
    // main database file and any existing `-wal` journal content. To read a WAL-mode database SQLite
    // may create an empty `-wal` and create/update the `-shm` wal-index (reconstructible coordination
    // state); EXCLUSIVE locking mode would avoid that but cannot coexist with a running server's
    // connection, and `immutable=1` would ignore live WAL content.
    let mut conn = SqliteConnection::connect_with(&options).await?;
    let inspection = classify_open_conn(&mut conn).await?;
    let _ = conn.close().await;
    Ok(inspection)
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

    /// Gate-3 G3-DB-001 fixtures: authority must come from PROVENANCE (our migration lineage), never
    /// from table names, and every schema object (any type, any byte string as a name) counts.
    fn assert_fails_closed_unchanged(tag: &str, path: &std::path::Path) {
        let before = std::fs::read(path).unwrap();
        match block_on(classify_db_at(path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("{tag}: expected Foreign, got {other:?}"),
        }
        let err = block_on(open_migrated_at(path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict", "{tag}: {}", err.message);
        assert_eq!(std::fs::read(path).unwrap(), before, "{tag}: bytes changed");
    }

    #[test]
    fn foreign_tables_that_borrow_native_names_fail_closed() {
        // Empty canonical ledger + row-bearing foreign tables NAMED like our lineage tables.
        let path = tmp_db("g3-spoofed-lineage-names");
        plant_foreign_db(
            &path,
            &format!(
                "{SQLX_LEDGER_DDL}; \
                 CREATE TABLE conversations (x TEXT); INSERT INTO conversations VALUES ('a'); \
                 CREATE TABLE messages (x TEXT); INSERT INTO messages VALUES ('b'); \
                 CREATE TABLE delivery_events (x TEXT); INSERT INTO delivery_events VALUES ('c')"
            ),
        );
        assert_fails_closed_unchanged("spoofed-lineage-names", &path);
    }

    #[test]
    fn foreign_non_table_objects_fail_closed() {
        // Empty ledger + ONLY a foreign view, index and trigger (no other table): still foreign schema.
        let path = tmp_db("g3-view-index-trigger");
        plant_foreign_db(
            &path,
            &format!(
                "{SQLX_LEDGER_DDL}; \
                 CREATE VIEW ledger_view AS SELECT version FROM _sqlx_migrations; \
                 CREATE INDEX ledger_idx ON _sqlx_migrations(description); \
                 CREATE TRIGGER ledger_trg AFTER INSERT ON _sqlx_migrations BEGIN SELECT 1; END"
            ),
        );
        assert_fails_closed_unchanged("view-index-trigger", &path);
    }

    #[test]
    fn foreign_table_with_undecodable_name_fails_closed() {
        // Empty ledger + a table whose name is not valid UTF-8 (0xFF + "_hidden"): a name that fails to
        // decode must count as a foreign object, never be silently dropped.
        let path = tmp_db("g3-undecodable-name");
        plant_foreign_db(&path, SQLX_LEDGER_DDL);
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute("CREATE TABLE \"\u{FF}_hidden\" (secret TEXT)")
                .await
                .or_else(|_| Ok::<_, sqlx::Error>(Default::default()))?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        // Rewrite the name bytes to raw 0xFF (the SQL layer only lets us write valid UTF-8).
        let mut bytes = std::fs::read(&path).unwrap();
        let needle: Vec<u8> = "\u{FF}_hidden".as_bytes().to_vec(); // C3 BF 5F ...
        let raw: Vec<u8> = [&[0xFFu8][..], b"_hidden"].concat();
        let mut replaced = 0;
        let mut i = 0;
        while i + needle.len() <= bytes.len() {
            if bytes[i..i + needle.len()] == needle[..] {
                // keep length: pad with a trailing NUL-safe byte the parser tolerates inside quotes
                bytes.splice(
                    i..i + needle.len(),
                    raw.iter().cloned().chain(std::iter::once(b' ')),
                );
                replaced += 1;
                i += raw.len() + 1;
            } else {
                i += 1;
            }
        }
        assert!(replaced > 0, "fixture: name bytes not found in file");
        std::fs::write(&path, &bytes).unwrap();
        assert_fails_closed_unchanged("undecodable-name", &path);
    }

    #[test]
    fn ledger_with_tampered_checksum_is_foreign() {
        // A known version whose checksum is NOT ours is not our lineage.
        let path = tmp_db("g3-tampered-checksum");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute("UPDATE _sqlx_migrations SET checksum = x'00' WHERE version = 1")
                .await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    #[test]
    fn native_db_migrated_by_a_newer_zynk_is_still_native() {
        // Our checksums prove provenance; an additional unknown (newer) version does not make it foreign.
        let path = tmp_db("g3-newer-native");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new().filename(&path).create_if_missing(false),
            )
            .await?;
            conn.execute(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (9999, 'future', 1, x'00', 0)",
            )
            .await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
    }

    #[test]
    fn read_only_classification_leaves_existing_data_bytes_untouched_on_live_wal() {
        // Gate-3 G3-DB-002 / ADR 0011: inspecting a foreign WAL database must not modify its data
        // bytes (main file, -wal); the -shm wal-index is the documented, reconstructible exception.
        let path = tmp_db("g3-wal-sidecars");
        let holder = block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await?;
            conn.execute(
                "CREATE TABLE projects (id TEXT PRIMARY KEY); INSERT INTO projects VALUES ('p')",
            )
            .await?;
            Ok::<SqliteConnection, DbError>(conn)
        })
        .unwrap();
        let shm = path.with_file_name(format!(
            "{}-shm",
            path.file_name().unwrap().to_str().unwrap()
        ));
        let wal = path.with_file_name(format!(
            "{}-wal",
            path.file_name().unwrap().to_str().unwrap()
        ));
        assert!(
            shm.exists() && wal.exists(),
            "fixture needs live WAL sidecars"
        );
        let snapshot = |p: &std::path::Path| std::fs::read(p).unwrap();
        let (db0, wal0) = (snapshot(&path), snapshot(&wal));
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
        assert_eq!(snapshot(&path), db0, "main db bytes changed");
        assert_eq!(snapshot(&wal), wal0, "-wal bytes changed");
        // ADR 0011 boundary: the data-bearing files are untouched; the `-shm` wal-index is
        // reconstructible coordination state and may legitimately change.
        assert!(shm.exists(), "the live -shm sidecar must still exist");
        drop(holder);
    }

    #[test]
    fn read_only_classification_leaves_existing_data_bytes_untouched_on_wal_copy_without_shm() {
        // Gate-3 caveat / ADR 0011: a WAL database copied without its -shm (crash/copy) keeps its
        // db/-wal bytes identical under a read-only inspection; SQLite may rebuild a -shm wal-index
        // from the -wal to read it (the documented reconstructible exception).
        let src = tmp_db("g3-wal-src");
        let holder = block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&src)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await?;
            conn.execute(
                "CREATE TABLE projects (id TEXT PRIMARY KEY); INSERT INTO projects VALUES ('p')",
            )
            .await?;
            Ok::<SqliteConnection, DbError>(conn)
        })
        .unwrap();
        let sidecar = |p: &std::path::Path, suffix: &str| {
            p.with_file_name(format!(
                "{}{suffix}",
                p.file_name().unwrap().to_str().unwrap()
            ))
        };
        let copy = tmp_db("g3-wal-copy");
        std::fs::copy(&src, &copy).unwrap();
        std::fs::copy(sidecar(&src, "-wal"), sidecar(&copy, "-wal")).unwrap();
        drop(holder);
        assert!(!sidecar(&copy, "-shm").exists());
        let (db0, wal0) = (
            std::fs::read(&copy).unwrap(),
            std::fs::read(sidecar(&copy, "-wal")).unwrap(),
        );
        match block_on(classify_db_at(&copy)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
        // ADR 0011: SQLite may rebuild a -shm wal-index from -wal to read the copy; the data bytes
        // below are what must stay identical.
        assert_eq!(std::fs::read(&copy).unwrap(), db0, "main db bytes changed");
        assert_eq!(
            std::fs::read(sidecar(&copy, "-wal")).unwrap(),
            wal0,
            "-wal bytes changed"
        );
    }

    #[test]
    fn native_db_opens_without_waiting_for_a_held_init_lock() {
        // Gate-3 G3-STARTUP-001: the init lock is only for INITIALIZATION; a fully migrated native DB
        // must open promptly even while another process holds the lock.
        let path = tmp_db("g3-native-held-lock");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(init_lock_path(&path))
            .unwrap();
        holder.lock().unwrap();
        let started = std::time::Instant::now();
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "native open waited {:?} on the init lock",
            started.elapsed()
        );
        holder.unlock().unwrap();
    }

    /// A genuine but PARTIALLY migrated native DB: migration 0001 applied by hand from the built-in
    /// migrator (same SQL, same SHA-384 checksum in the ledger), 0002/0003 pending.
    fn plant_partial_native_db(tag: &str) -> std::path::PathBuf {
        let path = tmp_db(tag);
        let first = MIGRATOR
            .iter()
            .find(|m| m.version == 1)
            .expect("migration 0001 exists");
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await?;
            conn.execute(SQLX_LEDGER_DDL).await?;
            conn.execute(first.sql.as_ref()).await?;
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (?, ?, 1, ?, 0)",
            )
            .bind(first.version)
            .bind(first.description.as_ref())
            .bind(first.checksum.as_ref())
            .execute(&mut conn)
            .await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        path
    }

    fn recorded_versions(path: &std::path::Path) -> Vec<i64> {
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .read_only(true),
            )
            .await?;
            let rows = ledger_rows(&mut conn).await?;
            Ok::<Vec<i64>, DbError>(rows.into_iter().map(|(v, _, _)| v).collect())
        })
        .unwrap()
    }

    #[test]
    fn concurrent_opens_of_a_partially_migrated_native_db_all_succeed() {
        // Gate-2 round 4 (Codex): a native DB with PENDING migrations must not bypass the init lock —
        // sqlx's SQLite migrator has no cross-process lock, so racing openers hit a UNIQUE ledger insert.
        let path = plant_partial_native_db("partial-native-concurrent");
        assert_eq!(recorded_versions(&path), vec![1]);
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
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
        let all: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert_eq!(recorded_versions(&path), all);
    }

    #[test]
    fn partially_migrated_native_db_does_not_migrate_while_the_init_lock_is_held() {
        // Only a fully CURRENT native DB may bypass the lock; a pending upgrade must wait for the
        // holder and fail closed on timeout, leaving the ledger untouched.
        let path = plant_partial_native_db("partial-native-held-lock");
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(init_lock_path(&path))
            .unwrap();
        holder.lock().unwrap();
        let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
        assert_eq!(err.code, "db_init_lock_timeout", "{}", err.message);
        assert_eq!(
            recorded_versions(&path),
            vec![1],
            "pending migrations ran despite the held lock"
        );
        holder.unlock().unwrap();
    }

    /// Gate-2 round 4 (a): lineage policy — recorded known versions must be a valid PREFIX of the
    /// built-in list, a `success = 0` row fails closed, and unknown versions are tolerated only when
    /// NEWER than everything built in.
    fn set_ledger_rows(path: &std::path::Path, sql: &str) {
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute(sql).await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn ledger_with_a_version_gap_is_foreign() {
        // Versions {1, 3} recorded (both with our checksums), 2 missing: not a state our migrator can
        // produce, so it is not our lineage.
        let path = tmp_db("g4-lineage-gap");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        set_ledger_rows(&path, "DELETE FROM _sqlx_migrations WHERE version = 2");
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    #[test]
    fn ledger_with_a_dirty_migration_row_is_foreign() {
        let path = tmp_db("g4-lineage-dirty");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        set_ledger_rows(
            &path,
            "UPDATE _sqlx_migrations SET success = 0 WHERE version = 1",
        );
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    #[test]
    fn ledger_with_an_unknown_older_version_is_foreign() {
        // An unknown version BELOW our newest is another lineage interleaved with ours; only strictly
        // newer unknown versions (a newer zynk) are tolerated.
        let path = tmp_db("g4-lineage-unknown-older");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        set_ledger_rows(
            &path,
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES (0, 'other app', 1, x'00', 0)",
        );
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    #[test]
    fn classification_uses_one_snapshot_across_schema_and_ledger_reads() {
        // Gate-2 round 4 (item 2): the schema-object read and the ledger read must come from ONE
        // snapshot. Choreography: the ledger table exists committed; a writer holds migration 0001 +
        // its ledger row UNCOMMITTED; a reader pins a read snapshot; the writer commits; the reader
        // classifies on the pinned snapshot — it must still see "empty ledger only" = Empty, never
        // Foreign { tables: ["_sqlx_migrations"] } (objects from before the commit, rows from after).
        let path = tmp_db("g4-single-snapshot");
        let first = MIGRATOR
            .iter()
            .find(|m| m.version == 1)
            .expect("migration 0001 exists");
        block_on(async {
            let mut setup = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await?;
            setup.execute(SQLX_LEDGER_DDL).await?;
            setup.close().await?;

            let mut writer = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await?;
            let mut pending = writer.begin().await?;
            pending.execute(first.sql.as_ref()).await?;
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (?, ?, 1, ?, 0)",
            )
            .bind(first.version)
            .bind(first.description.as_ref())
            .bind(first.checksum.as_ref())
            .execute(&mut *pending)
            .await?;

            let mut reader = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false)
                    .read_only(true),
            )
            .await?;
            let mut pinned = reader.begin().await?;
            let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master")
                .fetch_one(&mut *pinned)
                .await?;

            pending.commit().await?;

            let (class, _) = classify_snapshot(&mut pinned, &mut || {}).await?;
            assert_eq!(class, DbClassification::Empty, "pinned snapshot must predate the commit");
            pinned.rollback().await?;
            reader.close().await?;
            writer.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        // A fresh inspection sees the committed 0001: native, with 0002/0003 pending.
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
    }

    #[test]
    fn read_only_classification_leaves_existing_data_bytes_untouched_on_checkpointed_wal() {
        // ADR 0011: a cleanly checkpointed WAL database with NO sidecars keeps its main-file bytes
        // identical under inspection; SQLite may create an empty `-wal` and a `-shm` to read it.
        let path = tmp_db("g4-checkpointed-wal");
        plant_foreign_db(&path, "PRAGMA journal_mode = WAL; CREATE TABLE projects (id TEXT PRIMARY KEY); INSERT INTO projects VALUES ('p')");
        let sidecar = |suffix: &str| {
            path.with_file_name(format!(
                "{}{suffix}",
                path.file_name().unwrap().to_str().unwrap()
            ))
        };
        assert!(
            !sidecar("-wal").exists() && !sidecar("-shm").exists(),
            "fixture must start clean"
        );
        let before = std::fs::read(&path).unwrap();
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "main db bytes changed"
        );
        if sidecar("-wal").exists() {
            assert_eq!(
                std::fs::metadata(sidecar("-wal")).unwrap().len(),
                0,
                "a created -wal must be empty"
            );
        }
    }

    /// Shared choreography for the snapshot regressions: the ledger exists committed; a writer thread
    /// holds migration 0001 + its ledger row UNCOMMITTED and commits when told; the inspection under
    /// test runs with a hook that triggers that commit between its schema read and its ledger read.
    fn run_interleaved_inspection(
        tag: &str,
        inspect: impl FnOnce(&mut SqliteConnection, &mut dyn FnMut()) -> (DbClassification, bool),
    ) -> DbClassification {
        let path = tmp_db(tag);
        block_on(async {
            let mut setup = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await?;
            setup.execute(SQLX_LEDGER_DDL).await?;
            setup.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();

        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            block_on(async {
                let first = MIGRATOR.iter().find(|m| m.version == 1).unwrap();
                let mut conn = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(&writer_path)
                        .create_if_missing(false)
                        .journal_mode(SqliteJournalMode::Wal),
                )
                .await?;
                let mut pending = conn.begin().await?;
                pending.execute(first.sql.as_ref()).await?;
                sqlx::query(
                    "INSERT INTO _sqlx_migrations \
                     (version, description, success, checksum, execution_time) VALUES (?, ?, 1, ?, 0)",
                )
                .bind(first.version)
                .bind(first.description.as_ref())
                .bind(first.checksum.as_ref())
                .execute(&mut *pending)
                .await?;
                let _ = done_tx.send(()); // ready: pending transaction open
                go_rx.recv().expect("inspection signals the commit point");
                pending.commit().await?;
                let _ = done_tx.send(()); // committed
                conn.close().await?;
                Ok::<(), DbError>(())
            })
            .unwrap();
        });
        done_rx.recv().unwrap(); // writer's transaction is open

        let mut reader = block_on(async {
            SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false)
                    .read_only(true),
            )
            .await
            .map_err(DbError::from)
        })
        .unwrap();
        let mut commit_now = || {
            go_tx.send(()).unwrap();
            done_rx.recv().unwrap(); // the writer has COMMITTED between our two reads
        };
        let (class, _) = inspect(&mut reader, &mut commit_now);
        writer.join().unwrap();
        class
    }

    #[test]
    fn production_inspection_pins_one_snapshot_under_controlled_interleaving() {
        // Gate-2 round 4 (item 2) — the PRODUCTION wrapper: the writer's 0001 commits between the
        // schema read and the ledger read, and the verdict must still be Empty (one snapshot).
        let class = run_interleaved_inspection("g4-wrapper-snapshot", |reader, hook| {
            block_on(classify_open_conn_with_hook(reader, hook)).unwrap()
        });
        assert_eq!(class, DbClassification::Empty);
    }

    #[test]
    fn inspection_without_the_wrapper_transaction_mixes_snapshots() {
        // Negative control (RED evidence): the same choreography WITHOUT the wrapper's transaction —
        // each SELECT autocommits — sees the pre-commit objects and the post-commit ledger row and
        // wrongly reports Foreign. This is exactly the failure the wrapper's BEGIN prevents.
        let class = run_interleaved_inspection("g4-no-transaction", |reader, hook| {
            block_on(classify_snapshot(reader, hook)).unwrap()
        });
        assert_eq!(
            class,
            DbClassification::Foreign {
                tables: vec!["_sqlx_migrations".to_string()]
            }
        );
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
