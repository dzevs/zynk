//! zynk fork: SQLite connection, migration, and recovery helpers (ADR 0003,
//! foreign-DB guard finalized by ADR 0008).

use std::ffi::{c_int, c_void};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;
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

/// What the built-in migrator would have to do with a database, decided from the ledger.
///
/// - `Current` — every built-in migration is recorded: open without waiting on the init lock.
/// - `Pending` — first-time initialization or an upgrade is needed: serialize under the init lock.
/// - `Newer`   — the ledger records versions this build does not know (a NEWER zynk migrated it):
///   ownership is ours, but this build must not touch it — never "ready".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationState {
    Current,
    Pending,
    Newer(Vec<i64>),
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

/// `open_migrated` without orphan-message recovery: readiness/migration validation only.
pub async fn open_migrated_without_recovery() -> Result<SqliteConnection, DbError> {
    let path = crate::zynk::db_path::db_path();
    open_migrated_at_without_recovery(&path).await
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
    open_migrated_at_with_hook(path, &mut || {}).await
}

/// The opener with a hook that runs after the inspection verdict and before the writable use of the
/// file — a no-op in production; tests use it to swap the file behind `path` and prove the verdict
/// cannot be reused for a different target.
async fn open_migrated_at_with_hook(
    path: &Path,
    after_inspection: &mut dyn FnMut(),
) -> Result<SqliteConnection, DbError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The name SQLite itself will open — and derive `-journal`/`-wal`/`-shm` from — is the configured
    // path with its final-component symlink chain followed. Resolve it ONCE and use it for the guards,
    // the init lock, the identity capture and the connection, so all of them agree on one file (Codex
    // Gate-2 round 9: a stable `zynk.db -> foreign.db` link let the guards inspect `zynk.db-journal`
    // while SQLite replayed `foreign.db-journal`).
    let resolved = sqlite_effective_path(path)?;
    let path = resolved.as_path();
    // Existing data beside an absent/zero-page main file is refused BEFORE any connection exists:
    // SQLite discards a stale `-wal` on the first read of a zero-page database (ADR 0011). A HOT
    // rollback journal is refused before any connection too: a read-write pager would play it back
    // on its first shared lock — before any verdict (Codex Gate-2 round 8).
    refuse_orphan_sidecars(path)?;
    // A journal that looks hot may belong to a zynk initializer mid journal-mode switch (it holds the
    // init lock for that): wait for the lock, then re-check; a journal that is still hot afterwards
    // was left by a crashed or foreign writer and is refused for good.
    let mut early_lock = None;
    if let Err(err) = refuse_hot_journal(path) {
        if err.code != "db_hot_journal" {
            return Err(err);
        }
        let lock = InitLock::acquire(path).await?;
        refuse_orphan_sidecars(path)?;
        refuse_hot_journal(path)?;
        early_lock = Some(lock);
    }
    // ONE connection for inspection AND writing (Gate-3 round 2): the open pins the target file, so a
    // symlink flip or a rename between the verdict and the writable use cannot redirect the writes to
    // a different file — classification authority is never reused for a swapped target. Connecting
    // changes no journal mode (no header write) and checkpoint-on-close is disabled until the open
    // succeeds, so a refused foreign WAL database is never checkpointed into its main file (ADR 0008
    // fails closed before any mutation; ADR 0011's data-byte boundary holds).
    //
    // The cross-process init lock (see `InitLock`) is taken ONLY when initialization or a pending
    // migration is actually needed: an absent/zero-byte database is created only AFTER the lock is
    // held (nothing is created while another opener initializes or a foreign holder blocks us); an
    // existing database is inspected first and, when new or native with PENDING migrations, inspected
    // again on the SAME connection under the lock (sqlx's SQLite migrator has no cross-process lock of
    // its own). Only a fully CURRENT native database opens without waiting on any lock holder.
    let (mut conn, identity, _init_lock, verdict) = if main_file_present(path)? {
        let (mut conn, identity) = connect_pinned(path, false).await?;
        match classify_open_conn(&mut conn).await? {
            (DbClassification::Native, MigrationState::Current) => {
                (conn, identity, early_lock.take(), DbClassification::Native)
            }
            (DbClassification::Foreign { tables }, _) => {
                return refuse(conn, foreign_db_error(path, &tables)).await;
            }
            (DbClassification::Native, MigrationState::Newer(versions)) => {
                return refuse(conn, newer_lineage_error(path, &versions)).await;
            }
            // Empty (ledger-only init in progress/aborted) or native with pending migrations.
            (_, _) => {
                let lock = match early_lock.take() {
                    Some(lock) => lock,
                    None => InitLock::acquire(path).await?,
                };
                match classify_open_conn(&mut conn).await? {
                    (DbClassification::Foreign { tables }, _) => {
                        return refuse(conn, foreign_db_error(path, &tables)).await;
                    }
                    (DbClassification::Native, MigrationState::Newer(versions)) => {
                        return refuse(conn, newer_lineage_error(path, &versions)).await;
                    }
                    (class, _) => (conn, identity, Some(lock), class),
                }
            }
        }
    } else {
        let lock = match early_lock.take() {
            Some(lock) => lock,
            None => InitLock::acquire(path).await?,
        };
        // Another opener may have initialized, or a sidecar/journal appeared, while we waited: the
        // guards run again before this second connect path.
        refuse_orphan_sidecars(path)?;
        refuse_hot_journal(path)?;
        let (mut conn, identity) = connect_pinned(path, true).await?;
        match classify_open_conn(&mut conn).await? {
            (DbClassification::Foreign { tables }, _) => {
                return refuse(conn, foreign_db_error(path, &tables)).await;
            }
            (DbClassification::Native, MigrationState::Newer(versions)) => {
                return refuse(conn, newer_lineage_error(path, &versions)).await;
            }
            (class, _) => (conn, identity, Some(lock), class),
        }
    };
    // A WAL-mode database (every zynk-initialized database) bound its `-wal`/`-shm` to this
    // connection during the inspection read, so no later open happens by name. Otherwise (first-time
    // initialization, or a database externally converted to rollback journaling) the switch to WAL in
    // `apply_pragmas` opens the sidecars by NAME: the name must still refer to the inspected file.
    let wal_bound = journal_mode(&mut conn).await? == "wal";
    after_inspection();
    if !wal_bound && file_identity(path) != identity {
        return refuse(conn, target_changed_error(path)).await;
    }
    apply_pragmas(&mut conn).await?;
    if !wal_bound && file_identity(path) != identity {
        return refuse(conn, target_changed_error(path)).await;
    }
    // Belt and braces: what this connection sees after the switch must still be what was inspected.
    match classify_open_conn(&mut conn).await? {
        (DbClassification::Foreign { tables }, _) => {
            return refuse(conn, foreign_db_error(path, &tables)).await;
        }
        (class, _) if std::mem::discriminant(&class) != std::mem::discriminant(&verdict) => {
            return refuse(conn, target_changed_error(path)).await;
        }
        _ => {}
    }
    MIGRATOR
        .run(&mut conn)
        .await
        .map_err(|err| DbError::new("migration_failed", err.to_string()))?;
    set_checkpoint_on_close(&mut conn, true).await?;
    Ok(conn)
}

/// SQLite follows at most this many links while resolving a path (`SQLITE_MAX_SYMLINK`).
const MAX_SYMLINKS: usize = 100;

/// The file SQLite opens and derives its sidecar names from. SQLite's `unixFullPathname`
/// (`appendAllPathElements` in the bundled 3.46.0) walks EVERY component: it folds `.`/`..` and
/// follows a symlink in any component, directories included. zynk resolves only the FINAL
/// component's symlink chain (a relative target joined with the link's directory, bounded) on top
/// of the absolute path, and adds no lexical normalization: a directory link maps a whole directory,
/// so the main file, its `-journal`/`-wal`/`-shm` and the init lock already coincide through it, and
/// the kernel applies the same resolution to the unresolved prefix on every open. Only a linked
/// FINAL component moves the sidecars away from the configured name — that is what is resolved.
/// An absent final target is returned as is (SQLite creates the database there). Windows' VFS does
/// not follow links, so only the absolute form is taken there.
pub(crate) fn sqlite_effective_path(path: &Path) -> Result<PathBuf, DbError> {
    let io = |what: &str, err: std::io::Error| {
        DbError::new(
            "db_io_error",
            format!(
                "zynk: cannot resolve {} ({what}): {err}",
                printable_path(path)
            ),
        )
    };
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| io("cwd", err))?
            .join(path)
    };
    if !cfg!(unix) {
        return Ok(current);
    }
    for _ in 0..MAX_SYMLINKS {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&current).map_err(|err| io("readlink", err))?;
                current = if target.is_absolute() {
                    target
                } else {
                    match current.parent() {
                        Some(parent) => parent.join(target),
                        None => target,
                    }
                };
            }
            Ok(_) => return Ok(current),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(err) => return Err(io("lstat", err)),
        }
    }
    Err(DbError::new(
        "db_io_error",
        format!(
            "zynk: cannot resolve {}: too many levels of symbolic links",
            printable_path(path)
        ),
    ))
}

/// Whether a non-empty main file exists at `path`. Only NotFound means absent: any other metadata
/// failure fails closed (`db_io_error`) instead of being mistaken for "nothing there".
fn main_file_present(path: &Path) -> Result<bool, DbError> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.len() > 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(DbError::new(
            "db_io_error",
            format!("zynk: cannot inspect {}: {err}", printable_path(path)),
        )),
    }
}

/// A sidecar entry that is a symbolic link fails closed, dangling or not (Gate-3 round 4,
/// SENT-R4-CUTOVER-001): SQLite opens `-journal`/`-wal`/`-shm` by name and would create or write
/// the sidecar THROUGH the link — into a location that is not the database's, or nowhere at all
/// (a dangling link made the first native start fail with "unable to open database file" after
/// `metadata`, which follows links, had read it as absent). `zynk db adopt` moves the entry itself.
fn refuse_sidecar_links(path: &Path) -> Result<(), DbError> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let sidecar = sidecar_path(path, suffix);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(DbError::new(
                    "db_sidecar_link",
                    format!(
                        "zynk: refusing to open a database at {}: {} is a symbolic link, and SQLite \
                         opens sidecars by name (it would follow it). Move the bundle aside with \
                         `zynk db adopt`, or remove the link, then retry (`zynk db status` to inspect).",
                        printable_path(path),
                        printable_path(&sidecar)
                    ),
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(DbError::new(
                    "db_io_error",
                    format!("zynk: cannot inspect {}: {err}", printable_path(&sidecar)),
                ));
            }
        }
    }
    Ok(())
}

/// Size of a sidecar (`None` when it does not exist); any other metadata failure fails closed.
fn sidecar_len(sidecar: &Path) -> Result<Option<u64>, DbError> {
    match std::fs::metadata(sidecar) {
        Ok(meta) => Ok(Some(meta.len())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(DbError::new(
            "db_io_error",
            format!("zynk: cannot inspect {}: {err}", printable_path(sidecar)),
        )),
    }
}

/// `(dev, inode)` of whatever `path` currently refers to — the identity the pinned connection is
/// compared against while a by-name sidecar open is still ahead. Unsupported (always `None`, so the
/// check is skipped) where `std` exposes no stable file identity.
#[cfg(unix)]
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// The single pinned connection: no journal-mode change at connect, checkpoint-on-close disabled
/// until the open succeeds. Returns the file identity captured right after the open.
async fn connect_pinned(
    path: &Path,
    create: bool,
) -> Result<(SqliteConnection, Option<(u64, u64)>), DbError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
        .busy_timeout(Duration::from_millis(2000));
    let mut conn = SqliteConnection::connect_with(&options).await?;
    let identity = file_identity(path);
    let configured = async {
        set_checkpoint_on_close(&mut conn, false).await?;
        set_persist_wal(&mut conn).await
    }
    .await;
    if let Err(err) = configured {
        let _ = conn.close().await;
        return Err(err);
    }
    Ok((conn, identity))
}

/// `SQLITE_FCNTL_PERSIST_WAL`: after its close-time checkpoint SQLite normally removes `<db>-wal`
/// BY NAME — the one sidecar operation the pinned connection could not bind. With a persistent WAL
/// the file is simply left in place (SQLite truncates it only under `journal_size_limit`; the next
/// writer resets it through its own descriptor), so a file moved to that name in the meantime is never
/// deleted. ADR 0011 already allows a `-wal`/`-shm` to remain beside a zynk database.
async fn set_persist_wal(conn: &mut SqliteConnection) -> Result<(), DbError> {
    let mut handle = conn.lock_handle().await?;
    let mut persist: c_int = 1;
    // SAFETY: `handle` keeps the `sqlite3*` alive and exclusively locked for the call; "main" is a
    // NUL-terminated database name and the argument is a valid `int` the call reads and writes.
    let rc = unsafe {
        libsqlite3_sys::sqlite3_file_control(
            handle.as_raw_handle().as_ptr(),
            c"main".as_ptr(),
            libsqlite3_sys::SQLITE_FCNTL_PERSIST_WAL,
            (&mut persist as *mut c_int).cast::<c_void>(),
        )
    };
    if rc != libsqlite3_sys::SQLITE_OK {
        return Err(DbError::new(
            "db_config_failed",
            format!("sqlite3_file_control(PERSIST_WAL) failed: rc={rc}"),
        ));
    }
    Ok(())
}

async fn refuse<T>(conn: SqliteConnection, err: DbError) -> Result<T, DbError> {
    let _ = conn.close().await;
    Err(err)
}

async fn journal_mode(conn: &mut SqliteConnection) -> Result<String, DbError> {
    Ok(sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
        .fetch_one(&mut *conn)
        .await?
        .to_ascii_lowercase())
}

/// `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`: closing the last read-write connection to a WAL database
/// normally checkpoints the log into the main file — a data-byte mutation the fail-closed path must
/// never perform on a refused database.
async fn set_checkpoint_on_close(
    conn: &mut SqliteConnection,
    enabled: bool,
) -> Result<(), DbError> {
    let mut handle = conn.lock_handle().await?;
    let wanted = c_int::from(!enabled);
    let mut applied: c_int = -1;
    // SAFETY: `handle` keeps the `sqlite3*` alive and exclusively locked for the duration of the call;
    // the option takes an `int` and an out-pointer to an `int`, both provided as valid values.
    let rc = unsafe {
        libsqlite3_sys::sqlite3_db_config(
            handle.as_raw_handle().as_ptr(),
            libsqlite3_sys::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            wanted,
            &mut applied as *mut c_int,
        )
    };
    if rc != libsqlite3_sys::SQLITE_OK || applied != wanted {
        return Err(DbError::new(
            "db_config_failed",
            format!(
                "sqlite3_db_config(NO_CKPT_ON_CLOSE={wanted}) failed: rc={rc}, applied={applied}"
            ),
        ));
    }
    Ok(())
}

/// Sidecars SQLite would consume or discard while creating a database at `path`.
const ORPHAN_SIDECAR_SUFFIXES: &[&str] = &["-wal", "-journal"];

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// ADR 0011: a nonempty `-wal`/`-journal` beside an absent or zero-byte main file is existing data —
/// initialization would let SQLite discard or replay it, so fail closed before any connection.
fn refuse_orphan_sidecars(path: &Path) -> Result<(), DbError> {
    refuse_sidecar_links(path)?;
    if main_file_present(path)? {
        return Ok(());
    }
    for suffix in ORPHAN_SIDECAR_SUFFIXES {
        let sidecar = sidecar_path(path, suffix);
        let len = sidecar_len(&sidecar)?.unwrap_or(0);
        if len > 0 {
            return Err(DbError::new(
                "db_orphan_sidecar",
                format!(
                    "zynk: refusing to initialize a database at {}: the file is absent or empty but \
                     {} ({len} bytes) holds data that initialization would discard. Restore the \
                     matching database file beside it, or move the sidecar aside, then retry \
                     (`zynk db status` to inspect).",
                    printable_path(path),
                    printable_path(&sidecar)
                ),
            ));
        }
    }
    Ok(())
}

/// Conservative rollback-journal policy (Codex Gate-2 round 8): a nonempty `<db>-journal` whose first
/// byte is non-zero, beside a non-empty main file, is REFUSED before any connection exists. SQLite's
/// own hot-journal test (`hasHotJournal`) additionally consults lock state — a journal whose writer
/// still holds RESERVED is not hot — but zynk cannot observe that without a pager, and a read-write
/// pager plays a hot journal back on its FIRST shared lock (rewriting the main file and deleting the
/// journal) before any classification could run; a read-only pager cannot read such a database at all
/// (`SQLITE_READONLY_ROLLBACK`). So zynk refuses every journal that LOOKS hot: an inactive PERSIST-mode
/// journal (zeroed header) and a TRUNCATE-mode journal (empty file) pass; an unreadable journal fails
/// closed. zynk's own databases are WAL-mode and never carry a rollback journal, except for the
/// journal-mode switch of a brand-new file — which runs under the init lock, so the opener re-checks
/// after acquiring that lock before giving up (see `open_migrated_at_with_hook`).
fn refuse_hot_journal(path: &Path) -> Result<(), DbError> {
    if !main_file_present(path)? {
        return Ok(()); // absent/zero-byte main + nonempty journal is `refuse_orphan_sidecars`'s case
    }
    let journal = sidecar_path(path, "-journal");
    let Some(len) = sidecar_len(&journal)? else {
        return Ok(());
    };
    if len == 0 {
        return Ok(());
    }
    let hot = std::fs::File::open(&journal)
        .and_then(|mut file| {
            use std::io::Read;
            let mut first = [0u8; 1];
            file.read_exact(&mut first).map(|()| first[0] != 0)
        })
        .unwrap_or(true);
    if !hot {
        return Ok(());
    }
    Err(DbError::new(
        "db_hot_journal",
        format!(
            "zynk: refusing to open the database at {}: its rollback journal {} ({len} bytes) looks \
             hot (non-zero header) — a writer may have crashed before committing, or the journal is \
             unreadable — and opening the file read-write could roll it back (rewriting the main \
             file and deleting the journal). zynk will not do that to a database it has not \
             recognized as its own. If this is another application's database, let that \
             application recover it (back up both files first); if zynk was just creating this \
             database (no data yet), remove both files and retry. `zynk db status` reports the same \
             refusal.",
            printable_path(path),
            printable_path(&journal)
        ),
    ))
}

fn newer_lineage_error(path: &Path, versions: &[i64]) -> DbError {
    let listed: Vec<String> = versions.iter().map(i64::to_string).collect();
    DbError::new(
        "db_newer_lineage",
        format!(
            "zynk: the database at {} was migrated by a NEWER zynk (migration versions {} are \
             unknown to this build); refusing to open it. Upgrade zynk, or restore a database that \
             matches this build.",
            printable_path(path),
            listed.join(", ")
        ),
    )
}

fn target_changed_error(path: &Path) -> DbError {
    DbError::new(
        "db_target_changed",
        format!(
            "zynk: the file at {} was replaced while the database was being initialized; nothing \
             was written to the new file. Retry once the path is stable (`zynk db status` to inspect).",
            printable_path(path)
        ),
    )
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
                                printable_path(&lock_path)
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
    // The DISPLAY list: the same complete object set the decision uses (`schema_objects`), reduced to
    // tables and without FTS shadow/suffixed names; every name is escaped for terminals and logs.
    // Classification never depends on this list.
    const HIDDEN_SUFFIXES: &[&[u8]] = &[
        b"_fts",
        b"_data",
        b"_idx",
        b"_content",
        b"_docsize",
        b"_config",
    ];
    let mut names: Vec<String> = schema_objects(conn)
        .await?
        .into_iter()
        .filter(|object| object.kind == "table")
        .filter(|object| {
            !HIDDEN_SUFFIXES
                .iter()
                .any(|suffix| object.name.ends_with(suffix))
                && !object
                    .name
                    .windows(b"_fts_".len())
                    .any(|window| window == b"_fts_")
        })
        .map(|object| printable_name(&object.name))
        .collect();
    names.sort();
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
) -> Result<(DbClassification, MigrationState), DbError> {
    classify_open_conn_with_hook(conn, &mut || {}).await
}

/// The production wrapper with an interleaving hook that runs between the schema-object read and the
/// ledger read — a no-op in production; tests use it to commit a concurrent initializer at exactly
/// that point and prove the wrapper's read transaction pins ONE snapshot.
async fn classify_open_conn_with_hook(
    conn: &mut SqliteConnection,
    after_schema_read: &mut dyn FnMut(),
) -> Result<(DbClassification, MigrationState), DbError> {
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
) -> Result<(DbClassification, MigrationState), DbError> {
    // Every schema object counts — tables, views, indexes and triggers — with names read as bytes so
    // a name that is not valid UTF-8 still counts (it is never dropped). `user_table_names` is a
    // DISPLAY view that hides FTS-shadow/suffixed names; it never drives this decision.
    let objects = schema_objects(conn).await?;
    after_schema_read();
    if objects.is_empty() {
        return Ok((DbClassification::Empty, MigrationState::Pending));
    }
    let ledger_present = objects
        .iter()
        .any(|object| object.kind == "table" && object.name == b"_sqlx_migrations");
    if ledger_present && !ledger_structure_is_canonical(conn).await? {
        // A table that merely BORROWS the ledger's name is foreign schema: neither the empty-ledger
        // initialization exception nor native recognition applies, and the writable open (which
        // rewrites the header for WAL) must never happen (Gate-2 round 5).
        let mut tables = user_table_names(conn).await?;
        if tables.is_empty() {
            tables = objects.iter().map(SchemaObject::display).collect();
        }
        return Ok((
            DbClassification::Foreign { tables },
            MigrationState::Current,
        ));
    }
    if objects.len() == 1 && ledger_present {
        // sqlx creates its `_sqlx_migrations` ledger BEFORE the first migration's transaction commits.
        // A DB whose ONLY object is that ledger with NO recorded migrations is a native init in
        // progress (another zynk process) or an aborted one: nothing to protect → `Empty`. A ledger
        // that already RECORDS migrations but nothing else is unknown lineage → Foreign.
        if ledger_rows(conn).await?.is_empty() {
            return Ok((DbClassification::Empty, MigrationState::Pending));
        }
        return Ok((
            DbClassification::Foreign {
                tables: vec!["_sqlx_migrations".to_string()],
            },
            MigrationState::Current,
        ));
    }
    if ledger_present {
        if let Some(state) = native_lineage(conn, &objects).await? {
            return Ok((DbClassification::Native, state));
        }
    }
    let mut tables = user_table_names(conn).await?;
    if tables.is_empty() {
        tables = objects.iter().map(SchemaObject::display).collect();
    }
    Ok((
        DbClassification::Foreign { tables },
        MigrationState::Current,
    ))
}

/// Positive native recognition = migration PROVENANCE, never table names. Returns
/// `Some(state)` when the ledger is ours, `None` otherwise.
///
/// The rule, in order:
///
/// 1. every recorded row must have `success` equal to the literal integer `1` (what sqlx writes) — a
///    failed/dirty row or any other value is an unknown state, not ours;
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
) -> Result<Option<MigrationState>, DbError> {
    let rows = ledger_rows(conn).await?;
    if rows.is_empty() || rows.iter().any(|(_, _, success)| *success != 1) {
        return Ok(None);
    }
    let ours: Vec<_> = MIGRATOR.iter().collect();
    let newest_ours = ours.iter().map(|m| m.version).max().unwrap_or(0);
    let mut known: Vec<i64> = Vec::new();
    let mut newer: Vec<i64> = Vec::new();
    for (version, checksum, _) in &rows {
        match ours.iter().find(|m| m.version == *version) {
            Some(m) if m.checksum.as_ref() == checksum.as_slice() => known.push(*version),
            Some(_) => return Ok(None), // known version, foreign checksum
            None if *version > newest_ours => newer.push(*version), // newer zynk
            None => return Ok(None),    // unknown older version interleaved
        }
    }
    known.sort_unstable();
    let expected_prefix: Vec<i64> = ours.iter().take(known.len()).map(|m| m.version).collect();
    if known.is_empty() || known != expected_prefix {
        return Ok(None);
    }
    // A newer zynk records EVERY built-in migration before adding its own: a partial known prefix
    // plus unknown newer rows is not serially producible (Gate-3 round 2) — fail closed.
    if !newer.is_empty() && known.len() < ours.len() {
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
    // be serialized exactly like first-time initialization. Newer = ours, but migrated by a newer
    // zynk: this build must not open it (the migrator would reject the unknown versions).
    if !newer.is_empty() {
        newer.sort_unstable();
        return Ok(Some(MigrationState::Newer(newer)));
    }
    Ok(Some(if known.len() < ours.len() {
        MigrationState::Pending
    } else {
        MigrationState::Current
    }))
}

/// The canonical sqlx SQLite migration ledger (sqlx-sqlite 0.8.6 `migrate.rs`): `(name, declared type,
/// NOT NULL, default expression, primary key)`. The empty-ledger initialization exception and native
/// recognition apply ONLY to a table with exactly this structure.
const CANONICAL_LEDGER_COLUMNS: &[(&str, &str, bool, Option<&str>, bool)] = &[
    ("version", "BIGINT", false, None, true),
    ("description", "TEXT", true, None, false),
    (
        "installed_on",
        "TIMESTAMP",
        true,
        Some("CURRENT_TIMESTAMP"),
        false,
    ),
    ("success", "BOOLEAN", true, None, false),
    ("checksum", "BLOB", true, None, false),
    ("execution_time", "BIGINT", true, None, false),
];

/// The exact column definitions of the supported sqlx ledger, in declaration order, as they appear in
/// `sqlite_master.sql` after whitespace normalization. This is a deliberately NARROW supported-DDL
/// policy (Gate-2 round 6), not a SQL parser: anything the pinned sqlx initializer cannot produce is
/// foreign — including CHECK / FOREIGN KEY constraints and generated columns, which
/// `PRAGMA table_info` cannot see.
const CANONICAL_LEDGER_DDL_COLUMNS: &[&str] = &[
    "version BIGINT PRIMARY KEY",
    "description TEXT NOT NULL",
    "installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP",
    "success BOOLEAN NOT NULL",
    "checksum BLOB NOT NULL",
    "execution_time BIGINT NOT NULL",
];

/// Validates `_sqlx_migrations` on the caller's snapshot, two ways: (1) `PRAGMA table_xinfo`
/// (which, unlike `table_info`, lists hidden/generated columns) must report exactly the canonical
/// columns and nothing hidden; (2) the stored `CREATE TABLE` text must normalize to exactly the
/// canonical column list — no table constraints, no generated columns, no trailing table options.
async fn ledger_structure_is_canonical(conn: &mut SqliteConnection) -> Result<bool, DbError> {
    let rows = sqlx::query("PRAGMA table_xinfo(_sqlx_migrations)")
        .fetch_all(&mut *conn)
        .await?;
    if rows.len() != CANONICAL_LEDGER_COLUMNS.len() {
        return Ok(false);
    }
    for row in rows {
        let name = row.try_get::<String, _>("name")?;
        let declared = row.try_get::<String, _>("type")?;
        let not_null = row.try_get::<i64, _>("notnull")? != 0;
        let default = row.try_get::<Option<String>, _>("dflt_value")?;
        let primary_key = row.try_get::<i64, _>("pk")? != 0;
        let hidden = row.try_get::<i64, _>("hidden")?;
        let Some((_, expected_type, expected_not_null, expected_default, expected_pk)) =
            CANONICAL_LEDGER_COLUMNS
                .iter()
                .find(|(expected_name, ..)| *expected_name == name)
        else {
            return Ok(false);
        };
        let default_matches = match (default.as_deref(), expected_default) {
            (None, None) => true,
            (Some(actual), Some(expected)) => actual.trim().eq_ignore_ascii_case(expected),
            _ => false,
        };
        if hidden != 0
            || !declared.eq_ignore_ascii_case(expected_type)
            || not_null != *expected_not_null
            || !default_matches
            || primary_key != *expected_pk
        {
            return Ok(false);
        }
    }
    let ddl: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    Ok(ddl.as_deref().is_some_and(ledger_ddl_is_canonical))
}

/// Narrow supported-DDL check on the stored `CREATE TABLE` text: whitespace-normalized head must be
/// `CREATE TABLE [IF NOT EXISTS] _sqlx_migrations`, the parenthesized body must contain NO nested
/// parentheses (which rules out CHECK / FOREIGN KEY / GENERATED ALWAYS AS), split at commas into
/// exactly the canonical definitions in order, and nothing may follow the closing parenthesis.
fn ledger_ddl_is_canonical(sql: &str) -> bool {
    let collapsed = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(open) = collapsed.find('(') else {
        return false;
    };
    let Some(close) = collapsed.rfind(')') else {
        return false;
    };
    if close < open || !collapsed[close + 1..].trim().is_empty() {
        return false;
    }
    let head = collapsed[..open].trim().to_ascii_lowercase();
    if head != "create table _sqlx_migrations"
        && head != "create table if not exists _sqlx_migrations"
    {
        return false;
    }
    let body = &collapsed[open + 1..close];
    if body.contains('(') || body.contains(')') {
        return false;
    }
    let definitions: Vec<String> = body
        .split(',')
        .map(|definition| definition.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    definitions.len() == CANONICAL_LEDGER_DDL_COLUMNS.len()
        && definitions
            .iter()
            .zip(CANONICAL_LEDGER_DDL_COLUMNS)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
}

/// One `sqlite_master` row: `kind` is table/index/view/trigger; `name` is the raw byte string.
struct SchemaObject {
    kind: String,
    name: Vec<u8>,
    tbl_name: Vec<u8>,
    /// The recorded DDL as bytes: a non-UTF-8 object NAME makes its DDL non-UTF-8 too, and such an
    /// object must still count (fail closed), never error out of the decision.
    sql: Option<Vec<u8>>,
}

impl SchemaObject {
    fn display(&self) -> String {
        format!("{}:{}", self.kind, printable_name(&self.name))
    }

    /// An entry SQLite itself maintains, recognized by EXACT kind/name/shape — never by its
    /// `sqlite_` prefix alone, which a raw catalog edit can forge onto a readable user table
    /// (Gate-3 round 2): the implicit UNIQUE/PRIMARY KEY autoindex of a table that is present, and
    /// the AUTOINCREMENT / ANALYZE bookkeeping tables with their fixed DDL. Catalog text is compared
    /// as UTF-8 bytes: a database with another text encoding (UTF-16) cannot be zynk's, so its
    /// bookkeeping entries count as foreign schema — conservative by design, never a write hole.
    fn is_sqlite_owned(&self, tables: &[&[u8]]) -> bool {
        match self.kind.as_str() {
            "index" => {
                self.sql.is_none()
                    && tables.contains(&self.tbl_name.as_slice())
                    && autoindex_name_matches(&self.name, &self.tbl_name)
            }
            "table" => {
                self.tbl_name == self.name
                    && SQLITE_OWNED_TABLES.iter().any(|(name, ddl)| {
                        self.name == name.as_bytes() && self.sql.as_deref() == Some(ddl.as_bytes())
                    })
            }
            _ => false,
        }
    }
}

/// SQLite's own bookkeeping tables and the exact DDL it records for them (`build.c`, `analyze.c`).
const SQLITE_OWNED_TABLES: &[(&str, &str)] = &[
    ("sqlite_sequence", "CREATE TABLE sqlite_sequence(name,seq)"),
    ("sqlite_stat1", "CREATE TABLE sqlite_stat1(tbl,idx,stat)"),
    (
        "sqlite_stat4",
        "CREATE TABLE sqlite_stat4(tbl,idx,neq,nlt,ndlt,sample)",
    ),
];

/// `sqlite_autoindex_<table>_<N>` for exactly `table`, with a nonempty decimal `N`.
fn autoindex_name_matches(name: &[u8], table: &[u8]) -> bool {
    const PREFIX: &[u8] = b"sqlite_autoindex_";
    let Some(rest) = name.strip_prefix(PREFIX) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(table) else {
        return false;
    };
    let Some(digits) = rest.strip_prefix(b"_") else {
        return false;
    };
    !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
}

/// A path for terminals and logs: control characters escaped like `printable_name`.
pub fn printable_path(path: &Path) -> String {
    printable_name(path.display().to_string().as_bytes())
}

/// Characters that must never reach a terminal or log raw: the C0/C1 controls (`char::is_control`)
/// plus the Unicode format, bidi and invisible characters and the line/paragraph separators, which
/// can reorder, hide or split displayed text (Gate-3 round 3: a U+202E table name printed raw).
/// The list is the Unicode `Cf` category plus `Zl`/`Zp`, spelled out because `std` has no category
/// query; escaped with `escape_default` (`\u{202e}`).
pub fn is_terminal_hostile(c: char) -> bool {
    c.is_control()
        || matches!(
            u32::from(c),
            0x00AD
                | 0x0600..=0x0605
                | 0x061C
                | 0x06DD
                | 0x070F
                | 0x0890..=0x0891
                | 0x08E2
                | 0x180E
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x2064
                | 0x2066..=0x206F
                | 0xFEFF
                | 0xFFF9..=0xFFFB
                | 0x110BD
                | 0x110CD
                | 0x13430..=0x1343F
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0001
                | 0xE0020..=0xE007F
        )
}

/// A schema name for terminals and logs: valid UTF-8 with every terminal-hostile character (LF,
/// ESC, C1, Unicode format/bidi controls) escaped so a hostile name cannot inject or reorder
/// terminal or log text; other bytes as hex.
fn printable_name(name: &[u8]) -> String {
    match std::str::from_utf8(name) {
        Ok(name) => name
            .chars()
            .map(|c| {
                if is_terminal_hostile(c) {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect(),
        Err(_) => format!("<non-utf8 0x{}>", hex(name)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every schema object of any type, losslessly (a decode failure is an error, never a silently
/// dropped row), minus the entries SQLite itself maintains — recognized by exact shape, see
/// `SchemaObject::is_sqlite_owned`.
async fn schema_objects(conn: &mut SqliteConnection) -> Result<Vec<SchemaObject>, DbError> {
    let rows = sqlx::query(
        "SELECT type, CAST(name AS BLOB) AS name, CAST(tbl_name AS BLOB) AS tbl_name, \
         CAST(sql AS BLOB) AS sql FROM sqlite_master ORDER BY type, name",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut objects = Vec::with_capacity(rows.len());
    for row in rows {
        objects.push(SchemaObject {
            kind: row.try_get::<String, _>("type")?,
            name: row.try_get::<Vec<u8>, _>("name")?,
            tbl_name: row.try_get::<Vec<u8>, _>("tbl_name")?,
            sql: row.try_get::<Option<Vec<u8>>, _>("sql")?,
        });
    }
    let tables: Vec<&[u8]> = objects
        .iter()
        .filter(|object| object.kind == "table")
        .map(|object| object.name.as_slice())
        .collect();
    let owned: Vec<bool> = objects
        .iter()
        .map(|object| object.is_sqlite_owned(&tables))
        .collect();
    Ok(objects
        .into_iter()
        .zip(owned)
        .filter(|(_, owned)| !owned)
        .map(|(object, _)| object)
        .collect())
}

/// `(version, checksum, success)` of every migration the ledger records; `success` is read as the
/// stored integer so the rule can require the literal `1` rather than any truthy value.
async fn ledger_rows(conn: &mut SqliteConnection) -> Result<Vec<(i64, Vec<u8>, i64)>, DbError> {
    let rows =
        sqlx::query("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push((
            row.try_get::<i64, _>("version")?,
            row.try_get::<Vec<u8>, _>("checksum")?,
            row.try_get::<i64, _>("success")?,
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

/// `classify_db_at` plus the migration state (`db status` must never call a database this build
/// cannot open "ready").
pub async fn classify_db_at_with_state(
    path: &Path,
) -> Result<(DbClassification, MigrationState), DbError> {
    inspect_db_at(path).await
}

/// `classify_db_at` plus whether the built-in migrator still has work to do on it (always `true`
/// for Absent/Empty; meaningful for Native). Read-only like `classify_db_at` (ADR 0011: existing data
/// bytes are never modified).
async fn inspect_db_at(path: &Path) -> Result<(DbClassification, MigrationState), DbError> {
    let resolved = sqlite_effective_path(path)?;
    let path = resolved.as_path();
    refuse_orphan_sidecars(path)?;
    refuse_hot_journal(path)?;
    if !main_file_present(path)? {
        return Ok((DbClassification::Absent, MigrationState::Pending));
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
            printable_path(path),
            printable_path(path)
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

/// A message without any delivery event younger than this is treated as IN FLIGHT, never as an
/// orphan: a sender persists its message before its first transport event, and with concurrent
/// named-session servers sharing the global database a starting server must not fail a live peer's
/// fresh send (Gate-3 round 3). A crashed sender's message becomes recoverable once it is this old.
pub const ORPHAN_GRACE: Duration = Duration::from_secs(300);

pub async fn recover_orphan_messages(conn: &mut SqliteConnection) -> Result<(), DbError> {
    recover_orphan_messages_older_than(conn, ORPHAN_GRACE).await
}

/// Record `failed`/`system.recovery` for an event-less message EXACTLY ONCE across every server
/// that recovers it (Gate-3 round 3: concurrent named-server cold starts each recorded a failure).
/// The claim — bumping `delivery_seq` — is conditioned on the message STILL having no delivery
/// event, inside one immediate transaction, so a peer that already failed it (or a sender that
/// meanwhile recorded its first event) leaves zero rows and this call records nothing. Returns
/// whether this call recorded the failure.
pub(crate) async fn fail_orphan_message(
    conn: &mut SqliteConnection,
    message_id: &str,
) -> Result<bool, DbError> {
    use sqlx::Executor as _;
    conn.execute("BEGIN IMMEDIATE").await?;
    let result = fail_orphan_in_transaction(conn, message_id).await;
    match result {
        Ok(true) => match conn.execute("COMMIT").await {
            Ok(_) => Ok(true),
            Err(err) => {
                let _ = conn.execute("ROLLBACK").await;
                Err(err.into())
            }
        },
        Ok(false) => {
            let _ = conn.execute("ROLLBACK").await;
            Ok(false)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK").await;
            Err(err)
        }
    }
}

async fn fail_orphan_in_transaction(
    conn: &mut SqliteConnection,
    message_id: &str,
) -> Result<bool, DbError> {
    let claimed = sqlx::query(
        "UPDATE messages SET delivery_seq = delivery_seq + 1 WHERE id = ? AND NOT EXISTS \
         (SELECT 1 FROM delivery_events WHERE delivery_events.message_id = messages.id) \
         RETURNING delivery_seq",
    )
    .bind(message_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = claimed else {
        return Ok(false);
    };
    let seq = row.try_get::<i64, _>("delivery_seq")?;
    sqlx::query(
        "INSERT INTO delivery_events (id, message_id, event_type, proof_source, seq, timestamp, payload_json) VALUES (?, ?, 'failed', 'system.recovery', ?, ?, ?)",
    )
    .bind(crate::zynk::message::new_prefixed_id("evt"))
    .bind(message_id)
    .bind(seq)
    .bind(crate::zynk::message::now_rfc3339())
    .bind(r#"{"recovery":"orphaned_message_without_event"}"#)
    .execute(&mut *conn)
    .await?;
    Ok(true)
}

/// Fail every message with no delivery event whose `created_at` is older than `grace`.
pub async fn recover_orphan_messages_older_than(
    conn: &mut SqliteConnection,
    grace: Duration,
) -> Result<(), DbError> {
    let cutoff = crate::zynk::message::rfc3339_utc(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            - grace.as_secs() as i64,
    );
    let orphan_ids: Vec<String> = sqlx::query(
        "SELECT id FROM messages WHERE created_at < ? AND NOT EXISTS \
         (SELECT 1 FROM delivery_events WHERE delivery_events.message_id = messages.id)",
    )
    .bind(&cutoff)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|row| row.try_get::<String, _>("id").ok())
    .collect();

    for message_id in orphan_ids {
        fail_orphan_message(conn, &message_id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteJournalMode;

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
        init_lock_contention_is_bounded_body();
    }

    /// Windows CI runs only `windows_`-prefixed tests: the portable init-lock deadline contract
    /// must be exercised there too (Gate-3 round 4, INSPECTOR-BA765-002).
    #[cfg(windows)]
    #[test]
    fn windows_init_lock_contention_is_bounded() {
        init_lock_contention_is_bounded_body();
    }

    fn init_lock_contention_is_bounded_body() {
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
        let path = plant_real_sqlx_ledger(tag);
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
            conn.execute("PRAGMA journal_mode = WAL").await?;
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
        inspect: impl FnOnce(
            &mut SqliteConnection,
            &mut dyn FnMut(),
        ) -> (DbClassification, MigrationState),
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

    /// Gate-2 round 5 (Codex): the empty-ledger initialization exception must only apply to the
    /// CANONICAL sqlx ledger structure — a table that merely borrows the name is foreign schema.
    #[test]
    fn ledger_named_table_with_three_columns_is_foreign() {
        let path = tmp_db("g5-ledger-three-cols");
        plant_foreign_db(
            &path,
            "CREATE TABLE _sqlx_migrations (version, checksum, success)",
        );
        assert_fails_closed_unchanged("three-column-ledger", &path);
    }

    #[test]
    fn ledger_named_table_with_all_columns_but_wrong_structure_is_foreign() {
        // Same six names, but no primary key, no NOT NULL, no installed_on default.
        let path = tmp_db("g5-ledger-loose-cols");
        plant_foreign_db(
            &path,
            "CREATE TABLE _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time)",
        );
        assert_fails_closed_unchanged("loose-column-ledger", &path);
    }

    #[test]
    fn canonical_empty_ledger_still_initializes() {
        let path = tmp_db("g5-ledger-canonical");
        plant_foreign_db(&path, SQLX_LEDGER_DDL);
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Empty
        );
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
    }

    #[test]
    fn ledger_row_with_a_non_one_success_value_is_foreign() {
        // The stated rule is `success = 1` (what sqlx writes), not merely "truthy".
        let path = tmp_db("g5-success-two");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        set_ledger_rows(
            &path,
            "UPDATE _sqlx_migrations SET success = 2 WHERE version = 1",
        );
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    /// Gate-2 round 6 (Codex): `PRAGMA table_info` is lossy — generated/hidden columns are omitted
    /// and CHECK / FOREIGN KEY constraints are invisible — so a canonical-looking six-column ledger
    /// with one of these extras is NOT the sqlx ledger and must fail closed before writable connect.
    #[test]
    fn ledger_with_a_generated_or_constrained_extra_is_foreign() {
        let canonical_columns = "version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
             installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, \
             checksum BLOB NOT NULL, execution_time BIGINT NOT NULL";
        for (tag, extra) in [
            (
                "generated-virtual",
                ", foreign_marker TEXT GENERATED ALWAYS AS ('foreign schema') VIRTUAL",
            ),
            (
                "generated-stored",
                ", foreign_marker TEXT GENERATED ALWAYS AS ('foreign schema') STORED",
            ),
            ("check-constraint", ", CHECK(version > 1000)"),
            (
                "foreign-key",
                ", FOREIGN KEY(version) REFERENCES external_schema(id)",
            ),
        ] {
            let path = tmp_db(&format!("g6-ledger-{tag}"));
            plant_foreign_db(
                &path,
                &format!("CREATE TABLE _sqlx_migrations ({canonical_columns}{extra})"),
            );
            assert_fails_closed_unchanged(tag, &path);
        }
    }

    /// Positive compatibility with the REAL sqlx initializer, not only a handwritten copy of its DDL.
    fn plant_real_sqlx_ledger(tag: &str) -> std::path::PathBuf {
        use sqlx::migrate::Migrate;
        let path = tmp_db(tag);
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await?;
            conn.ensure_migrations_table()
                .await
                .map_err(|err| DbError::new("migrate", err.to_string()))?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        path
    }

    #[test]
    fn real_sqlx_empty_ledger_is_empty_and_initializes() {
        let path = plant_real_sqlx_ledger("g6-real-sqlx-empty");
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Empty
        );
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        assert_eq!(
            block_on(classify_db_at(&path)).unwrap(),
            DbClassification::Native
        );
    }

    // ---- Gate-3 round 2 (arbiter/sentinel) regressions ----

    fn sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
        path.with_file_name(format!(
            "{}{suffix}",
            path.file_name().unwrap().to_str().unwrap()
        ))
    }

    /// Main-file bytes plus the `-wal` bytes when a `-wal` exists: the ADR 0011 data-byte boundary.
    fn data_bytes(path: &std::path::Path) -> (Vec<u8>, Option<Vec<u8>>) {
        (
            std::fs::read(path).unwrap(),
            std::fs::read(sidecar(path, "-wal")).ok(),
        )
    }

    /// A foreign WAL-mode database at `path` whose committed rows are still in a nonempty `-wal`
    /// (no `-shm`): the shape of a WAL database copied or restored without a checkpoint.
    fn plant_foreign_wal_db(path: &std::path::Path, ddl: &str) {
        let src = tmp_db("wal-src");
        let holder = block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&src)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await?;
            conn.execute(ddl).await?;
            Ok::<SqliteConnection, DbError>(conn)
        })
        .unwrap();
        std::fs::copy(&src, path).unwrap();
        std::fs::copy(sidecar(&src, "-wal"), sidecar(path, "-wal")).unwrap();
        drop(holder);
        assert!(std::fs::metadata(sidecar(path, "-wal")).unwrap().len() > 0);
    }

    /// Atomic replacement of `to` by `from` (plus `-wal` when `from` has one): what `mv` does.
    fn rename_over(from: &std::path::Path, to: &std::path::Path) {
        std::fs::rename(from, to).unwrap();
        if sidecar(from, "-wal").exists() {
            std::fs::rename(sidecar(from, "-wal"), sidecar(to, "-wal")).unwrap();
        }
    }

    fn hold_init_lock(path: &std::path::Path) -> std::fs::File {
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(init_lock_path(path))
            .unwrap();
        holder.lock().unwrap();
        holder
    }

    #[test]
    fn refusing_a_foreign_wal_copy_leaves_its_data_bytes_untouched() {
        // Opener-level twin of the read-only sidecar tests: the opener inspects on a READ-WRITE
        // connection (identity pinning), and closing a read-write connection normally checkpoints the
        // WAL into the main file — for a refused foreign database that would rewrite its data bytes.
        let path = tmp_db("g3r2-foreign-wal-copy");
        plant_foreign_wal_db(
            &path,
            "CREATE TABLE projects (id TEXT PRIMARY KEY); INSERT INTO projects VALUES ('p')",
        );
        let before = data_bytes(&path);
        let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict", "{}", err.message);
        assert_eq!(data_bytes(&path), before, "foreign main/-wal bytes changed");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_sidecar_entry_is_refused_before_initialization() {
        // Gate-3 round 4 (SENT-R4-CUTOVER-001): a dangling `-wal` link beside an absent database
        // read as "no sidecar" (metadata follows the link), initialization went ahead, and SQLite
        // then failed to open the sidecar through the link. A sidecar entry that is not a plain
        // file fails closed before anything is created.
        // A dedicated directory: `tmp_db` names a file directly under the temp root, and the cleanup
        // below must never touch anything but this test's own tree.
        let dir = std::env::temp_dir().join(format!(
            "zynk-dangling-sidecar-{}-{}",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zynk.db");
        let link = sidecar(&path, "-wal");
        let escaped = dir.join("external").join("escaped-wal");
        std::os::unix::fs::symlink(&escaped, &link).unwrap();
        let err = block_on(open_migrated_at(&path)).unwrap_err();
        assert_eq!(err.code, "db_sidecar_link", "{}", err.message);
        assert!(
            !path.exists(),
            "no database may be created beside a refused sidecar"
        );
        assert!(!escaped.exists(), "the link target must never be touched");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "the entry is left in place"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn held_init_lock_creates_no_database_file() {
        // Sentinel: first-time initialization is serialized by the init lock, so nothing may be
        // created before the lock is held — a foreign holder plus an absent DB times out and the
        // database file still does not exist.
        let path = tmp_db("g3r2-lock-no-create");
        let holder = hold_init_lock(&path);
        let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
        assert_eq!(err.code, "db_init_lock_timeout", "{}", err.message);
        assert!(
            !path.exists(),
            "a database file was created before the init lock was held"
        );
        holder.unlock().unwrap();
    }

    /// Arbiter: classification authority for one target must not be reusable for a swapped target.
    /// `zynk.db` is a symlink to a fully-current native `safe.db`; exactly after the verdict the link
    /// is flipped to a foreign WAL database. The opener must keep working on the file it inspected
    /// and never touch the foreign main file or its pre-existing `-wal`.
    #[cfg(unix)]
    #[test]
    fn opener_pins_the_inspected_file_across_a_symlink_flip() {
        let safe = tmp_db("g3r2-toctou-safe");
        let foreign = tmp_db("g3r2-toctou-foreign");
        let link = tmp_db("g3r2-toctou-link");
        let _ = std::fs::remove_file(&link);
        block_on(open_migrated_at_without_recovery(&safe)).unwrap();
        plant_foreign_wal_db(
            &foreign,
            "CREATE TABLE secrets (v TEXT); INSERT INTO secrets VALUES ('FOREIGN-SYMLINK-SECRET')",
        );
        std::os::unix::fs::symlink(&safe, &link).unwrap();
        let foreign_before = data_bytes(&foreign);
        let (flip_foreign, flip_link) = (foreign.clone(), link.clone());
        let mut flip = move || {
            // Atomic replacement: build the new link beside the old one, then rename over it.
            let staged = flip_link.with_extension("db.flip");
            let _ = std::fs::remove_file(&staged);
            std::os::unix::fs::symlink(&flip_foreign, &staged).unwrap();
            std::fs::rename(&staged, &flip_link).unwrap();
        };
        let result = block_on(open_migrated_at_with_hook(&link, &mut flip));
        assert_eq!(
            data_bytes(&foreign),
            foreign_before,
            "the foreign database was reached through the flipped link"
        );
        result.expect("the open must complete on the file that was inspected (safe.db)");
        match block_on(classify_db_at(&foreign)).unwrap() {
            DbClassification::Foreign { tables } => assert_eq!(tables, vec!["secrets"]),
            other => panic!("foreign.db must still be foreign, got {other:?}"),
        }
    }

    /// Same rule for a REGULAR file: `zynk.db` is a partially migrated native DB (real pending
    /// writes). Exactly after the verdict a foreign WAL database is renamed over `zynk.db` and its
    /// `-wal` over `zynk.db-wal`. The writes must land in the inspected file — whose data and WAL
    /// are bound to the connection — never in the foreign pair now carrying those names.
    #[cfg(unix)]
    #[test]
    fn opener_pins_the_inspected_file_across_an_atomic_rename() {
        let db = plant_partial_native_db("g3r2-rename-db");
        let foreign = tmp_db("g3r2-rename-foreign");
        plant_foreign_wal_db(
            &foreign,
            "CREATE TABLE secrets (v TEXT); INSERT INTO secrets VALUES ('FOREIGN-RENAME-SECRET')",
        );
        let foreign_before = data_bytes(&foreign);
        let (from, to) = (foreign.clone(), db.clone());
        let mut swap = move || rename_over(&from, &to);
        let mut conn = block_on(open_migrated_at_with_hook(&db, &mut swap))
            .expect("the open must complete on the inspected file");
        assert_eq!(
            data_bytes(&db),
            foreign_before,
            "the foreign pair renamed into place was modified"
        );
        assert_eq!(
            block_on(classify_open_conn(&mut conn)).unwrap(),
            (DbClassification::Native, MigrationState::Current),
            "the pinned connection must see its own completed migration"
        );
        match block_on(classify_db_at(&db)).unwrap() {
            DbClassification::Foreign { tables } => assert_eq!(tables, vec!["secrets"]),
            other => panic!("the file at the name must be the foreign one, got {other:?}"),
        }
    }

    /// First-time initialization has no WAL bound at verdict time (SQLite creates it only when the
    /// journal mode is switched): if the name is re-pointed at another file in between, the opener
    /// must fail closed rather than create sidecars beside a file it never inspected.
    #[cfg(unix)]
    #[test]
    fn fresh_init_fails_closed_when_a_foreign_db_is_renamed_into_place_after_inspection() {
        let db = tmp_db("g3r2-fresh-rename-db");
        let foreign = tmp_db("g3r2-fresh-rename-foreign");
        plant_foreign_db(
            &foreign,
            "CREATE TABLE secrets (v TEXT); INSERT INTO secrets VALUES ('FOREIGN-FRESH-SECRET')",
        );
        let foreign_before = data_bytes(&foreign);
        let (from, to) = (foreign.clone(), db.clone());
        let mut swap = move || rename_over(&from, &to);
        let err = block_on(open_migrated_at_with_hook(&db, &mut swap)).unwrap_err();
        assert_eq!(err.code, "db_target_changed", "{}", err.message);
        assert_eq!(
            data_bytes(&db),
            foreign_before,
            "the foreign file was modified"
        );
        match block_on(classify_db_at(&db)).unwrap() {
            DbClassification::Foreign { tables } => assert_eq!(tables, vec!["secrets"]),
            other => panic!("expected the foreign DB at the name, got {other:?}"),
        }
    }

    /// A native DB externally converted to rollback journaling has no WAL bound at verdict time
    /// either (the WAL is opened by name when `apply_pragmas` switches it back): a foreign WAL pair
    /// renamed into place after the verdict must be refused with its bytes untouched.
    #[cfg(unix)]
    #[test]
    fn rollback_mode_native_open_fails_closed_when_a_foreign_wal_db_is_renamed_into_place() {
        let db = plant_partial_native_db("g3r2-rollback-rename-db");
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&db)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute("PRAGMA journal_mode = DELETE").await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        let foreign = tmp_db("g3r2-rollback-rename-foreign");
        plant_foreign_wal_db(
            &foreign,
            "CREATE TABLE secrets (v TEXT); INSERT INTO secrets VALUES ('FOREIGN-ROLLBACK-SECRET')",
        );
        let foreign_before = data_bytes(&foreign);
        let (from, to) = (foreign.clone(), db.clone());
        let mut swap = move || rename_over(&from, &to);
        let err = block_on(open_migrated_at_with_hook(&db, &mut swap)).unwrap_err();
        assert_eq!(err.code, "db_target_changed", "{}", err.message);
        assert_eq!(
            data_bytes(&db),
            foreign_before,
            "the foreign pair renamed into place was modified"
        );
    }

    #[test]
    fn closing_a_successful_open_never_deletes_the_wal_by_name() {
        // SQLite's close path checkpoints and then removes `<db>-wal` BY NAME; a different file moved
        // to that name in the meantime would be deleted. The pinned connection keeps its WAL
        // persistent (truncated through its own descriptor), so the name is never touched.
        let path = tmp_db("g3r2-persist-wal");
        let conn = block_on(open_migrated_at_without_recovery(&path)).unwrap();
        let decoy = sidecar(&path, "-wal");
        let staged = path.with_extension("decoy");
        std::fs::write(&staged, b"DECOY-WAL-BYTES").unwrap();
        std::fs::rename(&staged, &decoy).unwrap();
        block_on(async { conn.close().await.map_err(DbError::from) }).unwrap();
        assert_eq!(
            std::fs::read(&decoy).unwrap(),
            b"DECOY-WAL-BYTES",
            "closing deleted or rewrote the file at the -wal name"
        );
    }

    /// Child-process half of `foreign_db_with_a_hot_journal_is_refused_untouched`: a rollback-mode
    /// writer that spills past its page cache inside an open transaction, then dies without
    /// committing — the journal it leaves behind is HOT (real crash shape, no forged bytes).
    #[test]
    #[ignore = "helper: run only by its parent test"]
    fn hot_journal_crashing_writer() {
        let Ok(path) = std::env::var("ZYNK_TEST_HOT_JOURNAL_DB") else {
            return;
        };
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await?;
            conn.execute(
                "PRAGMA journal_mode = DELETE; PRAGMA cache_size = 8; \
                 CREATE TABLE customer_data (id INTEGER PRIMARY KEY, payload BLOB); \
                 INSERT INTO customer_data VALUES (0, zeroblob(1024))",
            )
            .await?;
            conn.execute("BEGIN IMMEDIATE").await?;
            for id in 1..400 {
                sqlx::query("INSERT INTO customer_data VALUES (?, randomblob(1024))")
                    .bind(id)
                    .execute(&mut conn)
                    .await?;
            }
            // Never dropped: a drop would let the worker thread close (and roll back) the
            // connection before the process dies.
            std::mem::forget(conn);
            Ok::<(), DbError>(())
        })
        .unwrap();
        // No close, no rollback: the process dies with the transaction open.
        std::process::exit(0);
    }

    #[test]
    fn foreign_db_with_a_hot_journal_is_refused_untouched() {
        // Codex Gate-2 round 8 (P1): a crashed foreign writer leaves a HOT rollback journal; any
        // read-write pager plays it back on its first shared lock, rewriting the main file and
        // deleting the journal — before any verdict. The guard must refuse before a connection exists
        // and leave main + journal byte-identical (a read-only inspection could not read it either).
        let path = tmp_db("g2r8-hot-journal");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "zynk::db::tests::hot_journal_crashing_writer",
                "--ignored",
                "--nocapture",
            ])
            .env("ZYNK_TEST_HOT_JOURNAL_DB", &path)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "crashing writer helper failed: {status}");
        let journal = sidecar(&path, "-journal");
        let journal_bytes = std::fs::read(&journal).unwrap();
        assert!(
            journal_bytes.len() > 512 && journal_bytes[0] != 0,
            "fixture must leave a hot journal ({} bytes)",
            journal_bytes.len()
        );
        let before = (std::fs::read(&path).unwrap(), journal_bytes);
        let err = block_on(classify_db_at(&path)).unwrap_err();
        assert_eq!(err.code, "db_hot_journal", "{}", err.message);
        let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
        assert_eq!(err.code, "db_hot_journal", "{}", err.message);
        assert_eq!(
            (
                std::fs::read(&path).unwrap(),
                std::fs::read(&journal).unwrap()
            ),
            before,
            "main/-journal bytes changed"
        );
    }

    #[test]
    fn non_hot_rollback_journals_classify_normally() {
        // Positive controls for the hot-journal rule: a PERSIST-mode journal (zeroed header) and a
        // TRUNCATE-mode journal (empty file) are not hot — the database classifies as usual and both
        // files stay identical.
        for (mode, tag) in [
            ("PERSIST", "g2r8-persist-journal"),
            ("TRUNCATE", "g2r8-truncate-journal"),
        ] {
            let path = tmp_db(tag);
            plant_foreign_db(
                &path,
                &format!(
                    "PRAGMA journal_mode = {mode}; CREATE TABLE projects (id TEXT PRIMARY KEY); \
                     INSERT INTO projects VALUES ('p')"
                ),
            );
            let journal = sidecar(&path, "-journal");
            assert!(
                journal.exists(),
                "{mode}: the fixture must leave a -journal behind"
            );
            if mode == "PERSIST" {
                assert_eq!(
                    std::fs::read(&journal).unwrap()[0],
                    0,
                    "PERSIST leaves a zeroed header"
                );
            }
            let before = (
                std::fs::read(&path).unwrap(),
                std::fs::read(&journal).unwrap(),
            );
            match block_on(classify_db_at(&path)).unwrap() {
                DbClassification::Foreign { tables } => {
                    assert_eq!(tables, vec!["projects"], "{mode}")
                }
                other => panic!("{mode}: expected Foreign, got {other:?}"),
            }
            let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
            assert_eq!(err.code, "db_foreign_conflict", "{mode}: {}", err.message);
            assert_eq!(
                (
                    std::fs::read(&path).unwrap(),
                    std::fs::read(&journal).unwrap()
                ),
                before,
                "{mode}: main/-journal bytes changed"
            );
        }
    }

    /// Codex Gate-2 round 9 (P1): the guards, the init lock and the connection must agree on the
    /// name SQLite actually opens. A STABLE `zynk.db -> foreign.db` link with the target's own hot
    /// journal used to pass the guards (they looked at `zynk.db-journal`) while SQLite replayed
    /// `foreign.db-journal`.
    #[cfg(unix)]
    #[test]
    fn stable_symlink_to_a_foreign_db_with_a_hot_journal_is_refused_untouched() {
        let foreign = tmp_db("g2r9-link-hot-foreign");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "zynk::db::tests::hot_journal_crashing_writer",
                "--ignored",
                "--nocapture",
            ])
            .env("ZYNK_TEST_HOT_JOURNAL_DB", &foreign)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "crashing writer helper failed: {status}");
        let journal = sidecar(&foreign, "-journal");
        assert!(
            std::fs::read(&journal).unwrap()[0] != 0,
            "fixture must leave a hot journal"
        );
        let before = (
            std::fs::read(&foreign).unwrap(),
            std::fs::read(&journal).unwrap(),
        );
        let link = tmp_db("g2r9-link-hot-link");
        std::os::unix::fs::symlink(&foreign, &link).unwrap();
        let err = block_on(classify_db_at(&link)).unwrap_err();
        assert_eq!(err.code, "db_hot_journal", "{}", err.message);
        let err = block_on(open_migrated_at_without_recovery(&link)).unwrap_err();
        assert_eq!(err.code, "db_hot_journal", "{}", err.message);
        assert_eq!(
            (
                std::fs::read(&foreign).unwrap(),
                std::fs::read(&journal).unwrap()
            ),
            before,
            "the link target's main/-journal bytes changed"
        );
    }

    /// Relative and chained links resolve exactly like SQLite (`readlink` joined with the link's
    /// directory, repeated): the target's hot journal is refused through both.
    #[cfg(unix)]
    #[test]
    fn relative_and_chained_symlinks_resolve_to_the_target_hot_journal() {
        let dir = std::env::temp_dir().join(format!(
            "zynk-g2r9-chain-{}-{}",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let foreign = dir.join("foreign.db");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "zynk::db::tests::hot_journal_crashing_writer",
                "--ignored",
                "--nocapture",
            ])
            .env("ZYNK_TEST_HOT_JOURNAL_DB", &foreign)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "crashing writer helper failed: {status}");
        let journal = sidecar(&foreign, "-journal");
        let before = (
            std::fs::read(&foreign).unwrap(),
            std::fs::read(&journal).unwrap(),
        );
        // relative link in the same directory, then a link to that link
        std::os::unix::fs::symlink("foreign.db", dir.join("relative.db")).unwrap();
        std::os::unix::fs::symlink("relative.db", dir.join("chained.db")).unwrap();
        for name in ["relative.db", "chained.db"] {
            let link = dir.join(name);
            assert_eq!(sqlite_effective_path(&link).unwrap(), foreign, "{name}");
            let err = block_on(classify_db_at(&link)).unwrap_err();
            assert_eq!(err.code, "db_hot_journal", "{name}: {}", err.message);
            let err = block_on(open_migrated_at_without_recovery(&link)).unwrap_err();
            assert_eq!(err.code, "db_hot_journal", "{name}: {}", err.message);
        }
        assert_eq!(
            (
                std::fs::read(&foreign).unwrap(),
                std::fs::read(&journal).unwrap()
            ),
            before,
            "the target's main/-journal bytes changed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn stable_symlink_to_an_absent_target_with_an_orphan_wal_is_refused() {
        // Orphan sidecars live beside the TARGET: `zynk.db -> missing.db` with a nonempty
        // `missing.db-wal` must be refused and nothing created at either name.
        let target = tmp_db("g2r9-link-orphan-target");
        let src = tmp_db("g2r9-link-orphan-src");
        plant_foreign_wal_db(
            &src,
            "CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('wal-only')",
        );
        std::fs::copy(sidecar(&src, "-wal"), sidecar(&target, "-wal")).unwrap();
        let before = std::fs::read(sidecar(&target, "-wal")).unwrap();
        let link = tmp_db("g2r9-link-orphan-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = block_on(classify_db_at(&link)).unwrap_err();
        assert_eq!(err.code, "db_orphan_sidecar", "{}", err.message);
        let err = block_on(open_migrated_at_without_recovery(&link)).unwrap_err();
        assert_eq!(err.code, "db_orphan_sidecar", "{}", err.message);
        assert!(!target.exists(), "the target was created");
        assert_eq!(std::fs::read(sidecar(&target, "-wal")).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn init_lock_at_the_link_target_blocks_a_symlinked_open() {
        // The init lock is keyed by the resolved name too: a holder at `<target>.init-lock` blocks an
        // open through `zynk.db -> target` (timeout, nothing created), as it would a direct open.
        let target = tmp_db("g2r9-link-lock-target");
        let link = tmp_db("g2r9-link-lock-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let holder = hold_init_lock(&target);
        let err = block_on(open_migrated_at_without_recovery(&link)).unwrap_err();
        assert_eq!(err.code, "db_init_lock_timeout", "{}", err.message);
        assert!(!target.exists(), "the target was created under a held lock");
        holder.unlock().unwrap();
        // Released: the same symlinked open initializes the target.
        block_on(open_migrated_at_without_recovery(&link)).unwrap();
        assert_eq!(
            block_on(classify_db_at(&target)).unwrap(),
            DbClassification::Native
        );
    }

    #[test]
    fn raw_renamed_sqlite_prefix_table_is_foreign() {
        // Arbiter: `sqlite_*` is a reserved prefix no SQL statement can create, but a raw catalog edit
        // can forge it; such a table must count as foreign schema, never vanish from the decision.
        let path = plant_real_sqlx_ledger("g3r2-prefix");
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute(
                "CREATE TABLE xsqlite_foo (secret TEXT); \
                 INSERT INTO xsqlite_foo VALUES ('prefix-sentinel')",
            )
            .await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        // Length-preserving raw rename of every catalog occurrence (name, tbl_name, sql).
        let original = std::fs::read(&path).unwrap();
        let mut forged = Vec::with_capacity(original.len());
        let mut hits = 0;
        let mut i = 0;
        while i < original.len() {
            if original[i..].starts_with(b"xsqlite_foo") {
                forged.extend_from_slice(b"sqlite_foox");
                i += b"xsqlite_foo".len();
                hits += 1;
            } else {
                forged.push(original[i]);
                i += 1;
            }
        }
        assert!(
            hits >= 3,
            "expected name, tbl_name and sql occurrences, got {hits}"
        );
        std::fs::write(&path, &forged).unwrap();
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { tables } => {
                assert!(
                    tables.iter().any(|t| t.contains("sqlite_foox")),
                    "the forged table must be named in the diagnostic: {tables:?}"
                );
            }
            other => panic!("expected Foreign, got {other:?}"),
        }
        let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict", "{}", err.message);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            forged,
            "forged DB bytes changed"
        );
    }

    #[test]
    fn sqlite_owned_catalog_tables_do_not_hide_or_taint_a_database() {
        // Positive controls for the exact-shape catalog filter: ANALYZE's `sqlite_stat*` tables on a
        // native DB keep it native and current; an AUTOINCREMENT-created `sqlite_sequence` beside a
        // canonical empty ledger keeps it Empty (initializes).
        let native = tmp_db("g3r2-analyzed-native");
        block_on(open_migrated_at_without_recovery(&native)).unwrap();
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&native)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute("ANALYZE").await?;
            let stats: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM sqlite_master WHERE name GLOB 'sqlite_stat*'",
            )
            .fetch_one(&mut conn)
            .await?;
            assert!(stats >= 1, "ANALYZE must leave a sqlite_stat table behind");
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        assert_eq!(
            block_on(classify_db_at_with_state(&native)).unwrap(),
            (DbClassification::Native, MigrationState::Current)
        );
        block_on(open_migrated_at_without_recovery(&native)).unwrap();

        let empty = plant_real_sqlx_ledger("g3r2-sequence-empty");
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&empty)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT); \
                 INSERT INTO t DEFAULT VALUES; DROP TABLE t",
            )
            .await?;
            let sequence: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM sqlite_master WHERE name = 'sqlite_sequence'",
            )
            .fetch_one(&mut conn)
            .await?;
            assert_eq!(sequence, 1, "sqlite_sequence must persist after DROP TABLE");
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        assert_eq!(
            block_on(classify_db_at(&empty)).unwrap(),
            DbClassification::Empty
        );
        block_on(open_migrated_at_without_recovery(&empty)).unwrap();
        assert_eq!(
            block_on(classify_db_at(&empty)).unwrap(),
            DbClassification::Native
        );
    }

    #[test]
    fn absent_db_with_a_nonempty_sidecar_fails_closed() {
        // Arbiter: a nonempty `-wal` (or `-journal`) beside an absent/zero-byte main file is existing
        // data (ADR 0011) — SQLite discards a stale log on the first read of a zero-page database, so
        // the guard must run before any connection is opened.
        for suffix in ["-wal", "-journal"] {
            let path = tmp_db(&format!("g3r2-orphan{suffix}"));
            let src = tmp_db("g3r2-orphan-src");
            plant_foreign_wal_db(
                &src,
                "CREATE TABLE foreign_records (v TEXT); \
                 INSERT INTO foreign_records VALUES ('sentinel-wal-only')",
            );
            std::fs::copy(sidecar(&src, "-wal"), sidecar(&path, suffix)).unwrap();
            let before = std::fs::read(sidecar(&path, suffix)).unwrap();
            let err = block_on(classify_db_at(&path)).unwrap_err();
            assert_eq!(err.code, "db_orphan_sidecar", "{suffix}: {}", err.message);
            let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
            assert_eq!(err.code, "db_orphan_sidecar", "{suffix}: {}", err.message);
            assert!(!path.exists(), "{suffix}: the main file was created");
            assert_eq!(
                std::fs::read(sidecar(&path, suffix)).unwrap(),
                before,
                "{suffix}: sidecar bytes changed"
            );
            // A zero-byte main file counts as absent too.
            std::fs::write(&path, b"").unwrap();
            let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
            assert_eq!(err.code, "db_orphan_sidecar", "{suffix}: {}", err.message);
            assert_eq!(std::fs::read(sidecar(&path, suffix)).unwrap(), before);
        }
    }

    #[test]
    fn partial_lineage_with_an_unknown_newer_row_is_foreign() {
        // Arbiter: `{1 correct, 10003 unknown}` is not serially producible by any zynk — fail closed
        // before any writable pragma (rollback journaling makes a header write visible in the SHA).
        let path = plant_partial_native_db("g3r2-partial-newer");
        set_ledger_rows(
            &path,
            "PRAGMA journal_mode = DELETE; \
             INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES (10003, 'future', 1, x'00', 0)",
        );
        let before = std::fs::read(&path).unwrap();
        match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { .. } => {}
            other => panic!("expected Foreign, got {other:?}"),
        }
        let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
        assert_eq!(err.code, "db_foreign_conflict", "{}", err.message);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "main DB bytes changed"
        );
    }

    #[test]
    fn newer_lineage_is_native_but_never_ready() {
        // Sentinel/arbiter: every built-in migration recorded plus a successful unknown newer row is
        // ours (a newer zynk migrated it) — but this build cannot open it, so it is reported as NEWER,
        // never "ready", and the opener refuses before any writable pragma.
        let path = tmp_db("g3r2-newer-lineage");
        block_on(open_migrated_at_without_recovery(&path)).unwrap();
        set_ledger_rows(
            &path,
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES (9999, 'future', 1, x'00', 0)",
        );
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            block_on(classify_db_at_with_state(&path)).unwrap(),
            (DbClassification::Native, MigrationState::Newer(vec![9999]))
        );
        let err = block_on(open_migrated_at_without_recovery(&path)).unwrap_err();
        assert_eq!(err.code, "db_newer_lineage", "{}", err.message);
        assert!(err.message.contains("9999"), "{}", err.message);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "main DB bytes changed"
        );
    }

    #[test]
    fn terminal_hostile_characters_cover_format_and_bidi_controls() {
        // Gate-3 round 3 (SENT-R3-DB-001): Cc is not enough — Unicode format/bidi/invisible
        // characters and the line/paragraph separators can reorder or hide terminal text.
        for c in [
            '\u{1b}',
            '\n',
            '\u{7f}',
            '\u{85}',
            '\u{202e}',
            '\u{200b}',
            '\u{2066}',
            '\u{feff}',
            '\u{2028}',
            '\u{00ad}',
            '\u{e0041}',
        ] {
            assert!(is_terminal_hostile(c), "{c:?} must be escaped");
        }
        for c in ['a', 'é', '中', '🦀', ' ', '\u{00a0}'] {
            assert!(!is_terminal_hostile(c), "{c:?} must print as is");
        }
        assert_eq!(printable_name("a\u{202e}b".as_bytes()), "a\\u{202e}b");
    }

    #[test]
    fn control_characters_in_object_names_are_escaped_in_diagnostics() {
        // Sentinel: a foreign table name carrying LF/ESC must not reach a terminal or log raw.
        let path = tmp_db("g3r2-control-name");
        plant_foreign_db(&path, "CREATE TABLE \"evil\n\u{1b}[31mred\u{1b}[0m\" (x)");
        let tables = match block_on(classify_db_at(&path)).unwrap() {
            DbClassification::Foreign { tables } => tables,
            other => panic!("expected Foreign, got {other:?}"),
        };
        assert_eq!(tables, vec!["evil\\n\\u{1b}[31mred\\u{1b}[0m"]);
        let message = foreign_db_error(&path, &tables).message;
        assert!(
            !message.chars().any(char::is_control),
            "control characters leaked: {message:?}"
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
