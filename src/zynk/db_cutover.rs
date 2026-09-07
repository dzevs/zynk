//! zynk fork: native DB cutover CLI (`zynk db status|adopt|backup|import`).
//!
//! ADR 0008 (safety-critical). The wrapper-era (`zynk` v1.5.1) and any unknown
//! database is treated as FOREIGN by `db::classify_db_at`. Native zynk will
//! NEVER auto-migrate or overwrite foreign data. These commands are the EXPLICIT,
//! operator-driven cutover surface:
//!
//! - `status` — classify the DB at the resolved native path and report it.
//! - `adopt` / `backup` / `import` — NON-DESTRUCTIVELY relocate a foreign/legacy
//!   DB out of the native path (to `<path>.wrapper-backup-<N>`) so zynk can then
//!   create a fresh native DB. Nothing here is automatic, silent, or
//!   destructive: the original bytes are MOVED (renamed/copied), never deleted
//!   in place, and the chosen backup target never clobbers an existing file
//!   (the counter `N` increments until a free slot is found — a DETERMINISTIC
//!   suffix, never a wall-clock timestamp).
//!
//! Full content import of a foreign schema into native tables is intentionally
//! OUT OF SCOPE for M6 (see plan): `import` performs the same safe relocation as
//! `adopt` and tells the operator native will start clean.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::zynk::db::{
    block_on, classify_db_at, classify_db_at_with_state, printable_path, DbClassification,
    MigrationState,
};
use crate::zynk::db_path;

/// Backup-target suffix base. The final name is `<db>.wrapper-backup-<N>` where
/// `N` is the smallest non-negative integer with no existing file — DETERMINISTIC
/// (no `Date.now`/timestamp), so tests are reproducible.
const BACKUP_SUFFIX: &str = "wrapper-backup";
const MAX_BACKUP_SLOTS: u32 = 10_000;

/// The SQLite file bundle: the main file plus the sidecars that must travel with it. A rollback
/// `-journal` (hot, or PERSIST-mode inactive) and a `-wal` hold data; `-shm` is coordination state
/// but must not be stranded either (Gate-3 round 3: a left-behind journal made the native path
/// unusable — the orphan-sidecar guard correctly refuses to initialize beside it).
const BUNDLE_SUFFIXES: &[&str] = &["-journal", "-wal", "-shm"];

/// Outcome of a non-destructive relocate (for tests + reporting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocateOutcome {
    pub moved_from: PathBuf,
    pub moved_to: PathBuf,
    /// The bundle members that moved, as suffixes (`""` = the main file).
    pub members: Vec<&'static str>,
}

/// Compute the first free `<path>.wrapper-backup-<N>` target (deterministic). A slot is free only
/// when the base AND every bundle member target (`-journal`/`-wal`/`-shm`) are absent — an
/// existing `<slot>-wal` must never be overwritten by the source WAL (Gate-3 round 3).
pub fn next_backup_path(path: &Path) -> PathBuf {
    let base = path.as_os_str().to_owned();
    for n in 0..MAX_BACKUP_SLOTS {
        let mut candidate = base.clone();
        candidate.push(format!(".{BACKUP_SUFFIX}-{n}"));
        let candidate = PathBuf::from(candidate);
        let free = !entry_exists(&candidate)
            && BUNDLE_SUFFIXES
                .iter()
                .all(|suffix| !entry_exists(&sidecar(&candidate, suffix)));
        if free {
            return candidate;
        }
    }
    // Pathological fallback (10k existing backups): append a process-unique id.
    let mut candidate = base;
    candidate.push(format!(
        ".{BACKUP_SUFFIX}-{}",
        crate::zynk::message::new_prefixed_id("n")
    ));
    PathBuf::from(candidate)
}

/// Move the SQLite bundle at `path` (main file + every existing `-journal`/`-wal`/`-shm`) aside to
/// the next free backup slot, NON-DESTRUCTIVELY and all-or-nothing: every target is checked before
/// the first rename, sidecars move first and the main file last, and any failure moves the members
/// already relocated back and reports an error — never a "success" with a stranded member, never an
/// overwritten backup.
pub fn relocate_aside(path: &Path) -> Result<RelocateOutcome, String> {
    // Operate on the SQLite-effective path — the final symlink chain resolved, exactly as
    // inspection classifies it — so what the guards refuse (e.g. a nonempty `-wal` beside a link's
    // absent target) is what relocation moves; the link itself stays in place.
    let effective = crate::zynk::db::sqlite_effective_path(path)
        .map_err(|e| format!("zynk: cannot resolve {}: {e}", printable_path(path)))?;
    relocate_bundle(&effective, &mut |from, to| {
        move_no_replace(from, to).map_err(|err| err.to_string())
    })
}

/// Does a directory entry exist at `path` in ANY form (a dangling symlink included)? `Path::exists`
/// follows symlinks, so a dangling destination read as free while POSIX rename would replace it.
fn entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Move `from` to `to` WITHOUT replacing anything at `to`: a hard link fails with `AlreadyExists`
/// when the target name exists in any form, so a competitor that appears between the preflight
/// and the move turns into an error (and a rollback) rather than an overwrite; the source is
/// unlinked only once the link exists. Filesystems without hard links fall back to
/// `rename_or_copy` after an entry check — the residual race on such a filesystem is documented.
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(from, to) {
        Ok(()) => std::fs::remove_file(from),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(err),
        Err(err) => {
            if entry_exists(to) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} exists", to.display()),
                ));
            }
            rename_or_copy(from, to).map_err(|fallback| {
                std::io::Error::other(format!("{fallback} (hard link failed: {err})"))
            })
        }
    }
}

fn relocate_bundle(
    path: &Path,
    mover: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
) -> Result<RelocateOutcome, String> {
    // A main file is not required: orphan sidecars beside an absent path are relocated too (the
    // guard's stated remedy); only an entirely empty bundle is an error.
    let target = next_backup_path(path);
    // Plan: sidecars first, the main file last (a failure part-way strands nothing that SQLite
    // would need to interpret the main file at either location).
    let mut plan: Vec<(PathBuf, PathBuf, &'static str)> = Vec::new();
    for suffix in BUNDLE_SUFFIXES {
        let member = sidecar(path, suffix);
        if member.exists() {
            plan.push((member, sidecar(&target, suffix), suffix));
        }
    }
    if path.exists() {
        plan.push((path.to_path_buf(), target.clone(), ""));
    }
    if plan.is_empty() {
        return Err(format!(
            "zynk: nothing to relocate — no database at {}",
            printable_path(path)
        ));
    }
    // Preflight: no target member may exist (the slot reservation already guarantees it; a re-check
    // here costs nothing and turns a race into a refusal rather than an overwrite).
    for (_, to, _) in &plan {
        if entry_exists(to) {
            return Err(format!(
                "zynk: refusing to overwrite an existing backup member at {}; nothing was relocated",
                printable_path(to)
            ));
        }
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, to, _) in &plan {
        if let Err(err) = mover(from, to) {
            let mut message = format!(
                "zynk: could not move {} -> {}: {err}; nothing was relocated",
                printable_path(from),
                printable_path(to)
            );
            let mut not_restored = Vec::new();
            for (from, to) in moved.iter().rev() {
                if let Err(restore_err) = mover(to, from) {
                    not_restored.push(format!("{} ({restore_err})", printable_path(from)));
                }
            }
            if !not_restored.is_empty() {
                message.push_str(&format!(
                    " — WARNING: these members could not be moved back and now live under {}: {}",
                    printable_path(&target),
                    not_restored.join(", ")
                ));
            }
            return Err(message);
        }
        moved.push((from.clone(), to.clone()));
    }
    Ok(RelocateOutcome {
        moved_from: path.to_path_buf(),
        moved_to: target,
        members: plan.iter().map(|(_, _, suffix)| *suffix).collect(),
    })
}

fn sidecar(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

/// Rename, falling back to copy+remove across filesystems. The source is only
/// removed AFTER a successful copy, so a failure never loses data.
fn rename_or_copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("zynk: cannot create {}: {e}", printable_path(parent)))?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(from, to).map_err(|e| {
                format!(
                    "zynk: cannot copy {} -> {}: {e}",
                    printable_path(from),
                    printable_path(to)
                )
            })?;
            std::fs::remove_file(from).map_err(|e| {
                format!(
                    "zynk: copied but could not remove {}: {e}",
                    printable_path(from)
                )
            })?;
            Ok(())
        }
    }
}

fn classify_raw(path: &Path) -> Result<DbClassification, crate::zynk::db::DbError> {
    block_on(classify_db_at(path))
}

fn classify_with_state(path: &Path) -> Result<(DbClassification, MigrationState), String> {
    block_on(classify_db_at_with_state(path))
        .map_err(|e| format!("zynk: cannot classify {}: {e}", printable_path(path)))
}

/// The status line. "ready" is claimed ONLY for a native database this build opens as-is: a
/// pending upgrade is named, and a database migrated by a NEWER zynk — which this build refuses —
/// is never called ready (Gate-3 round 2).
fn describe(class: &DbClassification, state: &MigrationState) -> String {
    match class {
        DbClassification::Absent => {
            "absent (no database yet — zynk will create a native one)".into()
        }
        DbClassification::Empty => {
            "empty (no tables — zynk will initialize the native schema)".into()
        }
        DbClassification::Native => match state {
            MigrationState::Current => "native (recognized zynk schema — ready)".into(),
            MigrationState::Pending => {
                "native (recognized zynk schema — upgrade pending; zynk migrates it on open)".into()
            }
            MigrationState::Newer(versions) => {
                let listed: Vec<String> = versions.iter().map(i64::to_string).collect();
                format!(
                    "native but NEWER (migrated by a newer zynk — migration versions {} are \
                     unknown to this build; this zynk will NOT open it — upgrade zynk)",
                    listed.join(", ")
                )
            }
        },
        DbClassification::Foreign { tables } => {
            if tables.is_empty() {
                "FOREIGN (unrecognized schema — zynk will NOT touch it)".into()
            } else {
                format!(
                    "FOREIGN (non-native tables: {} — zynk will NOT touch it)",
                    tables.join(", ")
                )
            }
        }
    }
}

fn usage() -> String {
    "usage: zynk db <status|adopt|backup|import>\n\
     \n\
     status   show the classification of the database at the resolved native path\n\
     adopt    move a foreign/legacy database aside (non-destructive) so zynk can\n\
              create a fresh native database at the native path\n\
     backup   alias of adopt: relocate the existing database to <path>.wrapper-backup-N\n\
     import   relocate the existing (foreign/legacy) database aside; native starts clean\n\
              (full foreign-content import is not supported in this release)"
        .into()
}

/// Process-level entry point: `zynk db …` as a standalone `main`-style return.
/// The live CLI dispatch uses the i32 `run_db_command_code` directly; this
/// `ExitCode` form is the documented standalone-`main` API, retained for parity.
#[allow(dead_code)]
pub fn run_db_command(args: &[String]) -> ExitCode {
    ExitCode::from(run_db_command_code(args) as u8)
}

/// CLI-dispatch entry point wired by `src/cli.rs::maybe_run`
/// (`"db" => return Ok(CommandOutcome::Handled(run_db_command_code(&args[2..])))`).
/// Returns the raw i32 exit code the positional dispatcher expects.
pub fn run_db_command_code(args: &[String]) -> i32 {
    let resolution = db_path::resolve_db_path();
    run_db_command_at_code(args, &resolution.db_path, &mut StdOut, &mut StdErr)
}

/// Sink abstraction so unit tests can capture output deterministically.
pub trait Sink {
    fn line(&mut self, s: &str);
}
struct StdOut;
impl Sink for StdOut {
    fn line(&mut self, s: &str) {
        println!("{s}");
    }
}
struct StdErr;
impl Sink for StdErr {
    fn line(&mut self, s: &str) {
        eprintln!("{s}");
    }
}

/// Path-injectable core (tested directly), returning an `ExitCode`. `path` is the
/// resolved native DB path. Thin `ExitCode` wrapper over `run_db_command_at_code`
/// so existing `ExitCode`-shaped unit tests stay valid (test-only consumer).
#[allow(dead_code)]
pub fn run_db_command_at(
    args: &[String],
    path: &Path,
    out: &mut dyn Sink,
    err: &mut dyn Sink,
) -> ExitCode {
    ExitCode::from(run_db_command_at_code(args, path, out, err) as u8)
}

/// Path-injectable core returning the raw i32 exit code (the shape the CLI
/// dispatcher consumes). `path` is the resolved native DB path.
pub fn run_db_command_at_code(
    args: &[String],
    path: &Path,
    out: &mut dyn Sink,
    err: &mut dyn Sink,
) -> i32 {
    let sub = args.first().map(|s| s.as_str());
    let rest = args.get(1..).unwrap_or(&[]);
    match sub {
        Some(verb @ ("status" | "adopt" | "backup" | "import")) => {
            // Safe-help gate: `db <leaf> --help` / `-h` must NEVER run or relocate.
            // Trailing args other than an exact help flag are rejected (exit 2) so a
            // mutating leaf never silently ignores them.
            match classify_db_leaf_args(rest) {
                DbLeafArgs::Run if verb == "status" => cmd_status(path, out, err),
                DbLeafArgs::Run => cmd_relocate(verb, path, out, err),
                DbLeafArgs::Help => {
                    out.line(&usage());
                    0
                }
                DbLeafArgs::BadArgs => {
                    err.line(&format!("zynk db {verb}: unexpected argument"));
                    err.line(&usage());
                    2
                }
            }
        }
        Some("help") | Some("--help") | Some("-h") => {
            out.line(&usage());
            0
        }
        Some(other) => {
            err.line(&format!("zynk db: unknown subcommand `{other}`"));
            err.line(&usage());
            2
        }
        None => {
            err.line(&usage());
            2
        }
    }
}

enum DbLeafArgs {
    Run,
    Help,
    BadArgs,
}

/// Classify the args trailing a `db` leaf. Only an empty tail runs the leaf; an
/// exact `--help`/`-h` requests help; anything else is rejected (so a mutating
/// leaf never ignores stray args). The bare word `help` is NOT a help flag here.
fn classify_db_leaf_args(rest: &[String]) -> DbLeafArgs {
    match rest {
        [] => DbLeafArgs::Run,
        [one] if matches!(one.as_str(), "--help" | "-h") => DbLeafArgs::Help,
        _ => DbLeafArgs::BadArgs,
    }
}

fn cmd_status(path: &Path, out: &mut dyn Sink, err: &mut dyn Sink) -> i32 {
    match classify_with_state(path) {
        Ok((class, state)) => {
            out.line(&format!(
                "zynk db path: {}",
                crate::zynk::db::printable_path(path)
            ));
            out.line(&format!("status:       {}", describe(&class, &state)));
            if let DbClassification::Foreign { .. } = class {
                out.line(
                    "action:       run `zynk db adopt` (or `zynk db backup`) to relocate it aside,",
                );
                out.line("              then zynk will create a fresh native database here.");
            }
            0
        }
        Err(message) => {
            err.line(&message);
            1
        }
    }
}

fn cmd_relocate(verb: &str, path: &Path, out: &mut dyn Sink, err: &mut dyn Sink) -> i32 {
    let class = match classify_raw(path) {
        Ok(class) => class,
        // Existing data the guards refuse to open — a rollback journal that looks hot, or orphan
        // sidecars beside an absent main file — is exactly what relocation is for: the complete
        // bundle moves aside intact (a crashed foreign writer can still recover it at the backup
        // location) and the native path becomes usable.
        Err(refusal) if matches!(refusal.code, "db_hot_journal" | "db_orphan_sidecar") => {
            out.line(&format!(
                "zynk: the path holds data zynk refuses to open ({}); relocating the complete \
                 SQLite bundle aside instead.",
                refusal.code
            ));
            return relocate_and_report(verb, path, out, err);
        }
        Err(e) => {
            err.line(&format!(
                "zynk: cannot classify {}: {e}",
                printable_path(path)
            ));
            return 1;
        }
    };
    match class {
        DbClassification::Native => {
            err.line(&format!(
                "zynk: the database at {} is already native — nothing to {verb}.",
                printable_path(path)
            ));
            3
        }
        DbClassification::Absent => {
            // Nothing at the native path. Offer to relocate the legacy
            // (`~/.zynk/zynk-v2/zynk.db`) DB if one exists, so `adopt` is useful
            // even when the wrapper used the old subdir.
            let legacy = db_path::legacy_native_db_path();
            if legacy != path && legacy.exists() {
                relocate_and_report(verb, &legacy, out, err)
            } else {
                out.line(&format!(
                    "zynk: no database at {} — nothing to {verb}; zynk will create a native one.",
                    printable_path(path)
                ));
                0
            }
        }
        DbClassification::Empty | DbClassification::Foreign { .. } => {
            relocate_and_report(verb, path, out, err)
        }
    }
}

fn relocate_and_report(verb: &str, src: &Path, out: &mut dyn Sink, err: &mut dyn Sink) -> i32 {
    match relocate_aside(src) {
        Ok(outcome) => {
            let members: Vec<&str> = outcome
                .members
                .iter()
                .map(|suffix| if suffix.is_empty() { "main" } else { *suffix })
                .collect();
            out.line(&format!(
                "zynk db {verb}: moved {} -> {} ({})",
                printable_path(&outcome.moved_from),
                printable_path(&outcome.moved_to),
                members.join(", ")
            ));
            out.line("zynk will create a fresh native database at the native path on next use.");
            if verb == "import" {
                out.line(
                    "note: foreign-content import is not supported in this release; native starts clean.",
                );
            }
            0
        }
        Err(message) => {
            err.line(&message);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{Connection, Executor, SqliteConnection};

    struct Capture(Vec<String>);
    impl Sink for Capture {
        fn line(&mut self, s: &str) {
            self.0.push(s.to_string());
        }
    }
    fn joined(c: &Capture) -> String {
        c.0.join("\n")
    }

    fn tmp_home(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "zynk-cutover-{tag}-{}-{}",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("h")
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn plant_foreign(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true),
            )
            .await?;
            conn.execute("CREATE TABLE projects (id TEXT PRIMARY KEY)")
                .await?;
            conn.close().await?;
            Ok::<(), crate::zynk::db::DbError>(())
        })
        .unwrap();
    }

    fn plant_native(path: &Path) {
        block_on(crate::zynk::db::open_migrated_at_without_recovery(path)).unwrap();
    }

    #[test]
    fn next_backup_path_is_deterministic_and_increments() {
        let dir = tmp_home("backup-name");
        let db = dir.join("zynk.db");
        std::fs::write(&db, b"x").unwrap();
        let first = next_backup_path(&db);
        assert_eq!(first, db.with_file_name("zynk.db.wrapper-backup-0"));
        // Once slot 0 exists, the next call picks slot 1.
        std::fs::write(&first, b"y").unwrap();
        assert_eq!(
            next_backup_path(&db),
            db.with_file_name("zynk.db.wrapper-backup-1")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn status_reports_foreign_and_does_not_mutate() {
        let dir = tmp_home("status-foreign");
        let db = dir.join("zynk.db");
        plant_foreign(&db);
        let before = std::fs::read(&db).unwrap();

        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let code = run_db_command_at(&["status".to_string()], &db, &mut out, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        let text = joined(&out);
        assert!(text.contains("FOREIGN"), "{text}");
        assert!(text.contains("projects"), "{text}");
        assert!(text.contains("zynk db adopt"), "{text}");

        assert_eq!(
            std::fs::read(&db).unwrap(),
            before,
            "status must not mutate"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn status_never_calls_a_newer_lineage_ready() {
        // Gate-3 round 2: a database migrated by a NEWER zynk is ours, but this build refuses to
        // open it — `db status` must say so instead of "ready".
        let dir = tmp_home("status-newer");
        let db = dir.join("zynk.db");
        block_on(crate::zynk::db::open_migrated_at_without_recovery(&db)).unwrap();
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&db)
                    .create_if_missing(false),
            )
            .await?;
            conn.execute(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (9999, 'future', 1, x'00', 0)",
            )
            .await?;
            conn.close().await?;
            Ok::<(), crate::zynk::db::DbError>(())
        })
        .unwrap();

        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let code = run_db_command_at(&["status".to_string()], &db, &mut out, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        let text = joined(&out);
        assert!(text.contains("NEWER") && text.contains("9999"), "{text}");
        assert!(!text.contains("ready"), "{text}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adopt_relocates_foreign_non_destructively() {
        let dir = tmp_home("adopt-foreign");
        let db = dir.join("zynk.db");
        plant_foreign(&db);
        let original = std::fs::read(&db).unwrap();

        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let code = run_db_command_at(&["adopt".to_string()], &db, &mut out, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));

        // Native path is now clear...
        assert!(
            !db.exists(),
            "native path must be cleared: {}",
            joined(&out)
        );
        // ...and the data was moved aside intact (same bytes), not deleted.
        let backup = db.with_file_name("zynk.db.wrapper-backup-0");
        assert!(backup.exists(), "backup must exist: {}", joined(&out));
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            original,
            "relocated bytes must be identical"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn backup_alias_relocates_too() {
        let dir = tmp_home("backup-alias");
        let db = dir.join("zynk.db");
        plant_foreign(&db);
        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let code = run_db_command_at(&["backup".to_string()], &db, &mut out, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        assert!(db.with_file_name("zynk.db.wrapper-backup-0").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    fn bundle_member(path: &Path, suffix: &str) -> PathBuf {
        let mut s = path.as_os_str().to_owned();
        s.push(suffix);
        PathBuf::from(s)
    }

    /// A foreign WAL-mode database whose committed rows are still in a nonempty `-wal` (main +
    /// `-wal` copied out from under a live holder), no `-shm`.
    fn plant_foreign_wal_pair(path: &Path) {
        let src = path.with_file_name("wal-source.db");
        let holder = block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&src)
                    .create_if_missing(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
            )
            .await?;
            conn.execute(
                "CREATE TABLE projects (id TEXT PRIMARY KEY); INSERT INTO projects VALUES ('p')",
            )
            .await?;
            Ok::<SqliteConnection, crate::zynk::db::DbError>(conn)
        })
        .unwrap();
        std::fs::copy(&src, path).unwrap();
        std::fs::copy(bundle_member(&src, "-wal"), bundle_member(path, "-wal")).unwrap();
        drop(holder);
        assert!(
            std::fs::metadata(bundle_member(path, "-wal"))
                .unwrap()
                .len()
                > 0
        );
    }

    fn run(args: &[&str], db: &Path) -> (ExitCode, String, String) {
        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let code = run_db_command_at(&args, db, &mut out, &mut err);
        (code, joined(&out), joined(&err))
    }

    fn assert_success(code: ExitCode, out: &str, err: &str) {
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::SUCCESS),
            "out:\n{out}\nerr:\n{err}"
        );
    }

    #[test]
    fn adopt_moves_the_complete_bundle_including_an_inactive_persist_journal() {
        // Gate-3 round 3 (AUD-310-CUTOVER-001): a PERSIST-mode foreign database keeps a nonempty
        // (zeroed-header) `-journal`. Relocation must move the COMPLETE SQLite bundle — otherwise the
        // orphan-sidecar guard correctly refuses to initialize the native path afterwards, and the
        // command has promised a clean native start it cannot deliver.
        let dir = tmp_home("adopt-persist-bundle");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&db)
                    .create_if_missing(true),
            )
            .await?;
            conn.execute(
                "PRAGMA journal_mode = PERSIST; CREATE TABLE projects (id TEXT PRIMARY KEY); \
                 INSERT INTO projects VALUES ('p')",
            )
            .await?;
            conn.close().await?;
            Ok::<(), crate::zynk::db::DbError>(())
        })
        .unwrap();
        let journal = bundle_member(&db, "-journal");
        assert!(
            std::fs::metadata(&journal).unwrap().len() > 0,
            "fixture leaves an inactive journal"
        );
        let before = (
            std::fs::read(&db).unwrap(),
            std::fs::read(&journal).unwrap(),
        );

        let (code, out, err) = run(&["adopt"], &db);
        assert_success(code, &out, &err);
        for suffix in ["", "-journal", "-wal", "-shm"] {
            assert!(
                !bundle_member(&db, suffix).exists(),
                "source member left behind: {suffix:?}"
            );
        }
        let backup = db.with_file_name("zynk.db.wrapper-backup-0");
        assert_eq!(
            (
                std::fs::read(&backup).unwrap(),
                std::fs::read(bundle_member(&backup, "-journal")).unwrap()
            ),
            before,
            "the relocated bundle must be byte-identical"
        );
        // The native path is genuinely clear: status says absent, and a native init succeeds.
        let (code, out, err) = run(&["status"], &db);
        assert_success(code, &out, &err);
        assert!(out.contains("absent"), "{out}");
        plant_native(&db);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adopt_moves_a_hot_journal_bundle_and_native_init_follows() {
        // A crashed foreign writer left a HOT journal (real crashed-writer fixture). The bundle
        // moves as a whole (main + journal, byte-identical) and the native path initializes.
        let dir = tmp_home("adopt-hot-bundle");
        let db = dir.join("zynk.db");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "zynk::db::tests::hot_journal_crashing_writer",
                "--ignored",
                "--nocapture",
            ])
            .env("ZYNK_TEST_HOT_JOURNAL_DB", &db)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "crashing writer helper failed: {status}");
        let journal = bundle_member(&db, "-journal");
        assert!(
            std::fs::read(&journal).unwrap()[0] != 0,
            "fixture must leave a hot journal"
        );
        let before = (
            std::fs::read(&db).unwrap(),
            std::fs::read(&journal).unwrap(),
        );

        let (code, out, err) = run(&["adopt"], &db);
        assert_success(code, &out, &err);
        assert!(
            !db.exists() && !journal.exists(),
            "the bundle must leave the native path"
        );
        let backup = db.with_file_name("zynk.db.wrapper-backup-0");
        assert_eq!(
            (
                std::fs::read(&backup).unwrap(),
                std::fs::read(bundle_member(&backup, "-journal")).unwrap()
            ),
            before
        );
        plant_native(&db);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn backup_slot_reserves_every_bundle_member() {
        // Gate-3 round 3 (AUD-310-CUTOVER-002): a slot is free only when the base AND every bundle
        // target are absent; an existing `<slot>-wal` must never be overwritten by the source WAL.
        let dir = tmp_home("adopt-slot-collision");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let sentinel = db.with_file_name("zynk.db.wrapper-backup-0-wal");
        std::fs::write(&sentinel, b"SENTINEL: an earlier backup's WAL").unwrap();
        let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();

        let (code, out, err) = run(&["adopt"], &db);
        assert_success(code, &out, &err);
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"SENTINEL: an earlier backup's WAL"
        );
        let slot1 = db.with_file_name("zynk.db.wrapper-backup-1");
        assert!(
            slot1.exists(),
            "slot 0 was occupied by a sidecar: slot 1 must be used\n{out}"
        );
        assert_eq!(
            std::fs::read(bundle_member(&slot1, "-wal")).unwrap(),
            source_wal
        );
        assert!(!db.with_file_name("zynk.db.wrapper-backup-0").exists());
        assert!(!db.exists() && !bundle_member(&db, "-wal").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn backup_slot_treats_a_dangling_symlink_as_occupied() {
        // Gate-3 round 3 pre-read (arbiter r79-dangle, Codex): `Path::exists()` follows symlinks, so a
        // dangling `<slot>-wal` symlink read as a free slot and POSIX rename replaced it.
        let dir = tmp_home("adopt-dangling-slot");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let sentinel = db.with_file_name("zynk.db.wrapper-backup-0-wal");
        std::os::unix::fs::symlink("nowhere-at-all", &sentinel).unwrap();
        let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();

        let (code, out, err) = run(&["adopt"], &db);
        assert_success(code, &out, &err);
        let meta = std::fs::symlink_metadata(&sentinel).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "the dangling slot symlink was replaced\n{out}"
        );
        let slot1 = db.with_file_name("zynk.db.wrapper-backup-1");
        assert_eq!(
            std::fs::read(bundle_member(&slot1, "-wal")).unwrap(),
            source_wal,
            "slot 0 is occupied by the symlink: slot 1 must be used\n{out}"
        );
        assert!(!db.exists() && !bundle_member(&db, "-wal").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn relocation_refuses_a_target_that_appears_after_preflight() {
        // Gate-3 round 3 pre-read (arbiter, Codex r15_rename_collision): a competing file created
        // between the preflight and the move must not be replaced — the move is no-replace, the
        // failure rolls back, and the source bundle stays where it was.
        let dir = tmp_home("adopt-post-preflight-race");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let source_db = std::fs::read(&db).unwrap();
        let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();
        let mut planted: Option<PathBuf> = None;
        let mut mover = |from: &Path, to: &Path| {
            if planted.is_none() {
                std::fs::write(to, b"PLANTED between preflight and move").unwrap();
                planted = Some(to.to_path_buf());
            }
            move_no_replace(from, to).map_err(|err| err.to_string())
        };
        let err = relocate_bundle(&db, &mut mover).unwrap_err();
        assert!(
            err.contains("nothing was relocated"),
            "must refuse and roll back: {err}"
        );
        let planted = planted.expect("the mover ran");
        assert_eq!(
            std::fs::read(&planted).unwrap(),
            b"PLANTED between preflight and move",
            "the competing file was replaced"
        );
        assert_eq!(std::fs::read(&db).unwrap(), source_db);
        assert_eq!(
            std::fs::read(bundle_member(&db, "-wal")).unwrap(),
            source_wal
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn adopt_through_a_final_symlink_relocates_the_target_bundle() {
        // Gate-3 round 3 pre-read (Codex finding 2; arbiter r79-symlinkorphan): inspection resolves
        // the final symlink (`sqlite_effective_path`), so `zynk.db -> target.db` with a nonempty
        // `target.db-wal` is refused as `db_orphan_sidecar`; relocation must act on the SAME
        // effective path — the target's bundle moves, the link stays, and status clears.
        let dir = tmp_home("adopt-through-symlink");
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("zynk.db");
        let target = dir.join("target.db");
        std::os::unix::fs::symlink("target.db", &link).unwrap();
        let orphan_wal = bundle_member(&target, "-wal");
        std::fs::write(&orphan_wal, vec![0x5au8; 4096]).unwrap();

        let (code, out, err) = run(&["adopt"], &link);
        assert_success(code, &out, &err);
        assert!(
            !orphan_wal.exists(),
            "the target's orphan WAL stayed put\n{out}"
        );
        let slot0 = target.with_file_name("target.db.wrapper-backup-0");
        assert_eq!(
            std::fs::read(bundle_member(&slot0, "-wal")).unwrap(),
            vec![0x5au8; 4096]
        );
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let (_, status_out, status_err) = run(&["status"], &link);
        assert!(
            !status_out.contains("db_orphan_sidecar") && !status_err.contains("db_orphan_sidecar"),
            "status still refuses after adopt:\n{status_out}\n{status_err}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn relocation_rolls_back_when_a_member_cannot_move() {
        // All-or-nothing: an injected failure on the last member (the main file) restores the
        // sidecars already moved and reports an error — no stranded members, no false success.
        let dir = tmp_home("adopt-rollback");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let before = (
            std::fs::read(&db).unwrap(),
            std::fs::read(bundle_member(&db, "-wal")).unwrap(),
        );
        let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
            if from == db {
                return Err("injected: main file cannot move".to_string());
            }
            rename_or_copy(from, to)
        };
        let err = relocate_bundle(&db, &mut mover).unwrap_err();
        assert!(
            err.contains("injected") && err.contains("nothing was relocated"),
            "{err}"
        );
        assert_eq!(
            (
                std::fs::read(&db).unwrap(),
                std::fs::read(bundle_member(&db, "-wal")).unwrap()
            ),
            before,
            "source bundle must be intact after rollback"
        );
        let slot0 = db.with_file_name("zynk.db.wrapper-backup-0");
        for suffix in ["", "-journal", "-wal", "-shm"] {
            assert!(
                !bundle_member(&slot0, suffix).exists(),
                "target member left behind: {suffix:?}"
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cutover_output_never_emits_raw_control_or_format_characters() {
        // Gate-3 round 3 (SENT-R3-DB-001): every db_cutover renderer — adopt/backup outcomes, the
        // no-database line, the classify wrapper — must escape terminal-hostile characters in
        // PATHS as well as names, including Unicode format/bidi controls (U+202E).
        let dir = tmp_home("esc\u{1b}[31m");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&db)
                    .create_if_missing(true),
            )
            .await?;
            conn.execute("CREATE TABLE \"proj\u{202e}ects\" (id TEXT PRIMARY KEY)")
                .await?;
            conn.close().await?;
            Ok::<(), crate::zynk::db::DbError>(())
        })
        .unwrap();
        let (code, out, err) = run(&["status"], &db);
        assert_success(code, &out, &err);
        assert!(
            !out.contains('\u{1b}') && !out.contains('\u{202e}'),
            "raw hostile char: {out:?}"
        );
        assert!(
            out.contains("\\u{1b}") && out.contains("\\u{202e}"),
            "{out}"
        );

        let (code, out, err) = run(&["adopt"], &db);
        assert_success(code, &out, &err);
        assert!(
            !out.contains('\u{1b}') && !err.contains('\u{1b}'),
            "adopt leaked ESC: {out:?} {err:?}"
        );
        assert!(out.contains("\\u{1b}"), "{out}");

        // Failure path through the classify wrapper: an orphan WAL beside the now-absent path.
        std::fs::write(bundle_member(&db, "-wal"), b"not empty").unwrap();
        let (code, out, err) = run(&["status"], &db);
        assert_ne!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::SUCCESS),
            "{out}"
        );
        assert!(
            !err.contains('\u{1b}'),
            "status failure leaked ESC: {err:?}"
        );
        assert!(
            err.contains("\\u{1b}") && err.contains("db_orphan_sidecar"),
            "{err}"
        );

        let (code, out, err) = run(&["adopt"], &db);
        // `adopt` is the remedy the guard names: the orphan sidecar moves aside (escaped output),
        // and the path is then genuinely clear.
        assert_success(code, &out, &err);
        assert!(
            !err.contains('\u{1b}') && !out.contains('\u{1b}'),
            "{out:?} {err:?}"
        );
        assert!(
            out.contains("db_orphan_sidecar") && out.contains("-wal"),
            "{out}"
        );
        assert!(
            !bundle_member(&db, "-wal").exists(),
            "orphan WAL must have moved aside"
        );
        let (code, out, err) = run(&["status"], &db);
        assert_success(code, &out, &err);
        assert!(out.contains("absent"), "{out}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adopt_refuses_native_db() {
        let dir = tmp_home("adopt-native");
        let db = dir.join("zynk.db");
        plant_native(&db);
        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let code = run_db_command_at(&["adopt".to_string()], &db, &mut out, &mut err);
        // Non-zero exit; native DB is NOT relocated (refused, not backed up).
        // (We don't assert raw byte-equality here: the DB is WAL-mode, so a
        // read-only classify can passively touch the file/sidecars; the
        // byte-immutability guarantee that matters is for FOREIGN DBs and is
        // asserted in db.rs. Here the contract is: refuse + no backup file.)
        assert_ne!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        assert!(joined(&err).contains("already native"), "{}", joined(&err));
        assert!(db.exists(), "native DB must remain in place");
        assert!(
            !db.with_file_name("zynk.db.wrapper-backup-0").exists(),
            "native DB must NOT be relocated"
        );
        // Still classifies native afterward.
        assert_eq!(
            block_on(classify_db_at(&db)).unwrap(),
            DbClassification::Native
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adopt_absent_is_noop_success() {
        let dir = tmp_home("adopt-absent");
        let db = dir.join("zynk.db");
        // No file planted; legacy path also absent (isolated temp home).
        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let code = run_db_command_at(&["adopt".to_string()], &db, &mut out, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        assert!(
            joined(&out).contains("nothing to adopt"),
            "{}",
            joined(&out)
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_subcommand_errors() {
        let mut out = Capture(vec![]);
        let mut err = Capture(vec![]);
        let code = run_db_command_at(
            &["wat".to_string()],
            Path::new("/tmp/none/zynk.db"),
            &mut out,
            &mut err,
        );
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(2)));
        assert!(joined(&err).contains("unknown subcommand"));
    }

    #[test]
    fn db_leaf_help_flag_exits_zero_and_never_mutates() {
        for leaf in ["status", "adopt", "backup", "import"] {
            for flag in ["--help", "-h"] {
                let dir = tmp_home(&format!("db-help-{leaf}-{}", flag.trim_start_matches('-')));
                let db = dir.join("zynk.db");
                plant_foreign(&db);
                let before = std::fs::read(&db).unwrap();

                let mut out = Capture(vec![]);
                let mut err = Capture(vec![]);
                let code = run_db_command_at_code(
                    &[leaf.to_string(), flag.to_string()],
                    &db,
                    &mut out,
                    &mut err,
                );

                assert_eq!(code, 0, "db {leaf} {flag} must exit 0");
                assert!(
                    joined(&out).contains("usage: zynk db"),
                    "db {leaf} {flag} must print db usage: {}",
                    joined(&out)
                );
                // The safe-help gate must NOT run/relocate: bytes intact, no backup created.
                assert_eq!(
                    std::fs::read(&db).unwrap(),
                    before,
                    "db {leaf} {flag} must not mutate the database"
                );
                assert!(
                    !db.with_file_name("zynk.db.wrapper-backup-0").exists(),
                    "db {leaf} {flag} must not relocate the database"
                );
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }

    #[test]
    fn db_leaf_unexpected_trailing_arg_exits_two_without_mutation() {
        for leaf in ["status", "adopt", "backup", "import"] {
            let dir = tmp_home(&format!("db-extra-{leaf}"));
            let db = dir.join("zynk.db");
            plant_foreign(&db);
            let before = std::fs::read(&db).unwrap();

            let mut out = Capture(vec![]);
            let mut err = Capture(vec![]);
            let code = run_db_command_at_code(
                &[leaf.to_string(), "extra".to_string()],
                &db,
                &mut out,
                &mut err,
            );

            assert_eq!(code, 2, "db {leaf} extra must exit 2");
            assert_eq!(
                std::fs::read(&db).unwrap(),
                before,
                "db {leaf} extra must not mutate the database"
            );
            assert!(
                !db.with_file_name("zynk.db.wrapper-backup-0").exists(),
                "db {leaf} extra must not relocate the database"
            );
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
