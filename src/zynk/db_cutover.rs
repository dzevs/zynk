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

use crate::zynk::db::{block_on, classify_db_at, DbClassification};
use crate::zynk::db_path;

/// Backup-target suffix base. The final name is `<db>.wrapper-backup-<N>` where
/// `N` is the smallest non-negative integer with no existing file — DETERMINISTIC
/// (no `Date.now`/timestamp), so tests are reproducible.
const BACKUP_SUFFIX: &str = "wrapper-backup";
const MAX_BACKUP_SLOTS: u32 = 10_000;

/// Outcome of a non-destructive relocate (for tests + reporting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocateOutcome {
    pub moved_from: PathBuf,
    pub moved_to: PathBuf,
}

/// Compute the first free `<path>.wrapper-backup-<N>` target (deterministic).
pub fn next_backup_path(path: &Path) -> PathBuf {
    let base = path.as_os_str().to_owned();
    for n in 0..MAX_BACKUP_SLOTS {
        let mut candidate = base.clone();
        candidate.push(format!(".{BACKUP_SUFFIX}-{n}"));
        let candidate = PathBuf::from(candidate);
        if !candidate.exists() {
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

/// Move the file at `path` (plus any `-wal`/`-shm` sidecars) aside to the next
/// free backup target, NON-DESTRUCTIVELY. Returns the relocation outcome. Never
/// deletes data in place; never overwrites an existing backup.
pub fn relocate_aside(path: &Path) -> Result<RelocateOutcome, String> {
    if !path.exists() {
        return Err(format!(
            "zynk: nothing to relocate — no database at {}",
            path.display()
        ));
    }
    let target = next_backup_path(path);
    rename_or_copy(path, &target)?;
    // Relocate WAL/SHM sidecars too, so the moved DB stays consistent and the
    // native path is fully clear.
    for ext in ["-wal", "-shm"] {
        let side = sidecar(path, ext);
        if side.exists() {
            let side_target = sidecar(&target, ext);
            // Best-effort: a failed sidecar move must not strand the main file.
            let _ = rename_or_copy(&side, &side_target);
        }
    }
    Ok(RelocateOutcome {
        moved_from: path.to_path_buf(),
        moved_to: target,
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
            .map_err(|e| format!("zynk: cannot create {}: {e}", parent.display()))?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(from, to).map_err(|e| {
                format!(
                    "zynk: cannot copy {} -> {}: {e}",
                    from.display(),
                    to.display()
                )
            })?;
            std::fs::remove_file(from).map_err(|e| {
                format!("zynk: copied but could not remove {}: {e}", from.display())
            })?;
            Ok(())
        }
    }
}

fn classify(path: &Path) -> Result<DbClassification, String> {
    block_on(classify_db_at(path))
        .map_err(|e| format!("zynk: cannot classify {}: {e}", path.display()))
}

fn describe(class: &DbClassification) -> String {
    match class {
        DbClassification::Absent => {
            "absent (no database yet — zynk will create a native one)".into()
        }
        DbClassification::Empty => {
            "empty (no tables — zynk will initialize the native schema)".into()
        }
        DbClassification::Native => "native (recognized zynk schema — ready)".into(),
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
    match classify(path) {
        Ok(class) => {
            out.line(&format!("zynk db path: {}", path.display()));
            out.line(&format!("status:       {}", describe(&class)));
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
    // First, what's at the native path?
    let class = match classify(path) {
        Ok(c) => c,
        Err(message) => {
            err.line(&message);
            return 1;
        }
    };
    match class {
        DbClassification::Native => {
            err.line(&format!(
                "zynk: the database at {} is already native — nothing to {verb}.",
                path.display()
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
                    path.display()
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
            out.line(&format!(
                "zynk db {verb}: moved {} -> {}",
                outcome.moved_from.display(),
                outcome.moved_to.display()
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
