//! zynk fork: native DB cutover CLI (`zynk db status|adopt|backup|import`).
//!
//! ADR 0008 (safety-critical). The wrapper-era (`zynk` v1.5.1) and any unknown
//! database is treated as FOREIGN by `db::classify_db_at`. Native zynk will
//! NEVER auto-migrate or overwrite foreign data. These commands are the EXPLICIT,
//! operator-driven cutover surface:
//!
//! - `status` — classify the DB at the resolved native path and report it.
//! - `adopt` / `backup` / `import` — NON-DESTRUCTIVELY relocate a foreign/legacy
//!   DB out of the native path (into the directory `<path>.wrapper-backup-<N>/`) so zynk can then
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

/// Compute the first free `<path>.wrapper-backup-<N>` slot (deterministic). A slot is a DIRECTORY
/// that holds the whole bundle under the members' own names (`<slot>/zynk.db`, `<slot>/zynk.db-wal`
/// …), so SQLite can open the backup in place; it is free only when NO entry of any kind exists at
/// that name (a dangling symlink included). Reserving the slot is one atomic `mkdir`, which claims
/// every member name at once — flat per-file renames could never reserve four names together
/// (Gate-3 round 4, SENT-R4-CUTOVER-002). An inspection error is not "free": it fails closed.
/// Production goes through [`relocate_bundle_with`]; this wrapper serves the tests.
#[cfg(test)]
fn next_backup_path(path: &Path) -> Result<PathBuf, String> {
    next_backup_path_with(path, &BundleInspect::real())
}

fn next_backup_path_with(path: &Path, inspect: &BundleInspect<'_>) -> Result<PathBuf, String> {
    let base = path.as_os_str().to_owned();
    for n in 0..MAX_BACKUP_SLOTS {
        let mut candidate = base.clone();
        candidate.push(format!(".{BACKUP_SUFFIX}-{n}"));
        let candidate = PathBuf::from(candidate);
        if !inspect.present(&candidate)? {
            return Ok(candidate);
        }
    }
    // Pathological fallback (10k existing backups): append a process-unique id.
    let mut candidate = base;
    candidate.push(format!(
        ".{BACKUP_SUFFIX}-{}",
        crate::zynk::message::new_prefixed_id("n")
    ));
    Ok(PathBuf::from(candidate))
}

/// The two inspections a relocation decides on, injectable so tests can model I/O failures the
/// build machine cannot produce (Codex Gate-2 R20: an EIO on a member's metadata must never read
/// as "absent", and a slot that cannot be listed completely must never read as "clean").
struct BundleInspect<'a> {
    /// Does a directory entry of any kind exist at the path? `Err` for anything but NotFound.
    entry: &'a dyn Fn(&Path) -> std::io::Result<bool>,
    /// Every entry of the directory; `Err` when the listing or any entry cannot be read.
    list: &'a dyn Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
}

impl BundleInspect<'static> {
    fn real() -> Self {
        Self {
            entry: &entry_present,
            list: &list_dir,
        }
    }
}

impl BundleInspect<'_> {
    fn present(&self, path: &Path) -> Result<bool, String> {
        (self.entry)(path).map_err(|err| {
            format!(
                "zynk: cannot inspect {}: {err}; nothing was relocated",
                printable_path(path)
            )
        })
    }
}

/// Only NotFound means absent; a dangling symlink is present; any other error is the caller's to
/// fail closed on (`exists` follows links and swallows errors — never on a decision path).
fn entry_present(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// The identity of a directory entry — the entry itself, links included: device + inode on Unix,
/// volume serial + file index on Windows. `None` when it cannot be read; the custody check treats
/// that as "not the member that left the source" (fail closed), never as a pass.
#[cfg(unix)]
type FileIdentity = (u64, u64);
#[cfg(windows)]
type FileIdentity = (u32, u64);
#[cfg(not(any(unix, windows)))]
type FileIdentity = ();

#[cfg(unix)]
fn entry_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn entry_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT,
    };
    // Open the entry itself (a reparse point is not followed) with a metadata-only access mask:
    // querying file information needs no data-read permission, so an entry whose bytes this
    // process may not read is still identifiable.
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    // SAFETY: `info` is a properly sized, writable out-parameter and the handle is valid for the
    // duration of the call (the `File` outlives it).
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
    (ok != 0).then(|| {
        (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn entry_identity(_path: &Path) -> Option<FileIdentity> {
    None
}

fn list_dir(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        entries.push(entry?.path());
    }
    Ok(entries)
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

/// Test-only convenience: does a directory entry of any kind exist at `path`? Production decision
/// paths use [`entry_present`], which fails closed on inspection errors.
#[cfg(test)]
fn entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Move `from` to `to` in ONE atomic step that never replaces an existing entry at `to` (a
/// dangling symlink included): Linux `renameat2(RENAME_NOREPLACE)`, macOS `renamex_np(RENAME_EXCL)`,
/// Windows `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`. A competitor that appears between the
/// preflight and the move fails the move with `AlreadyExists` (the bundle rolls back), never an
/// overwrite. Where no such primitive exists the member is refused with an actionable error: a
/// two-step move (link or copy, then unlink of the source NAME) cannot be made safe against a
/// writer that atomically replaces the source or the target between the steps — it deletes the
/// newcomer or loses the original — so it is not offered at all (Gate-3 round 3). What is at the
/// source name at the instant of the move is what moves; a writer replacing the source afterwards
/// keeps its file at the source, and nothing else is ever deleted.
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    move_no_replace_with(from, to, &atomic_rename_noreplace)
}

fn move_no_replace_with(
    from: &Path,
    to: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(err),
        Err(err) if primitive_unavailable(&err) => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "no atomic no-replace move is available on this filesystem ({err}); move the \
                 bundle aside manually"
            ),
        )),
        Err(err) => Err(err),
    }
}

/// The platform primitive exists but this filesystem or kernel does not provide it (as opposed
/// to an ordinary permission or I/O failure, which keeps its own cause).
fn primitive_unavailable(err: &std::io::Error) -> bool {
    if matches!(
        err.kind(),
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        // ENOTSUP and EOPNOTSUPP share a value on Linux and differ on macOS: compare, don't match.
        err.raw_os_error().is_some_and(|code| {
            [libc::ENOSYS, libc::ENOTSUP, libc::EOPNOTSUPP, libc::EXDEV].contains(&code)
        })
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE: the move would need a copy.
        err.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn atomic_rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    let (from_c, to_c) = (c_path(from)?, c_path(to)?);
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the call; the kernel
    // copies them. `AT_FDCWD` resolves relative paths against the working directory, as
    // `std::fs::rename` does; `RENAME_NOREPLACE` makes the kernel fail with EEXIST instead of
    // replacing whatever entry exists at `to`.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn atomic_rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    let (from_c, to_c) = (c_path(from)?, c_path(to)?);
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the call; the kernel
    // copies them. `RENAME_EXCL` fails with EEXIST instead of replacing an existing `to`.
    let rc = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn c_path(path: &Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains a NUL byte")
    })
}

#[cfg(windows)]
fn atomic_rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (from_w, to_w) = (wide(from), wide(to));
    // SAFETY: both buffers are valid NUL-terminated UTF-16 strings that outlive the call. Flags 0:
    // without `MOVEFILE_REPLACE_EXISTING` an existing target fails with ERROR_ALREADY_EXISTS, and
    // without `MOVEFILE_COPY_ALLOWED` the move is a single directory operation on one volume.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), 0)
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn atomic_rename_noreplace(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no atomic no-replace rename on this platform",
    ))
}

fn relocate_bundle(
    path: &Path,
    mover: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
) -> Result<RelocateOutcome, String> {
    relocate_bundle_with(path, mover, &BundleInspect::real())
}

fn relocate_bundle_with(
    path: &Path,
    mover: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
    inspect: &BundleInspect<'_>,
) -> Result<RelocateOutcome, String> {
    let slot = next_backup_path_with(path, inspect)?;
    let Some(file_name) = path.file_name() else {
        return Err(format!(
            "zynk: cannot relocate {}: not a file path",
            printable_path(path)
        ));
    };
    let member_name = |suffix: &str| {
        let mut name = file_name.to_os_string();
        name.push(suffix);
        name
    };
    // Plan: every bundle ENTRY beside the path (a dangling sidecar link is an entry SQLite would
    // open by name, so it moves too — `exists` follows links and skipped it, Gate-3 round 4);
    // sidecars first, the main file last. A main file is not required: orphan sidecars beside an
    // absent path are relocated too; only an entirely empty bundle is an error.
    let mut plan: Vec<(PathBuf, PathBuf, &'static str)> = Vec::new();
    for suffix in BUNDLE_SUFFIXES {
        let member = sidecar(path, suffix);
        if inspect.present(&member)? {
            plan.push((member, slot.join(member_name(suffix)), suffix));
        }
    }
    if inspect.present(path)? {
        plan.push((path.to_path_buf(), slot.join(member_name("")), ""));
    }
    if plan.is_empty() {
        return Err(format!(
            "zynk: nothing to relocate — no database at {}",
            printable_path(path)
        ));
    }
    // Reserve the slot: one atomic mkdir claims every member name; an entry that appeared since
    // the scan (a competitor, a dangling link) makes it fail — refuse, never replace.
    if let Err(err) = create_private_dir(&slot) {
        return Err(format!(
            "zynk: refusing to use backup slot {}: {err}; nothing was relocated",
            printable_path(&slot)
        ));
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut identities: Vec<FileIdentity> = Vec::new();
    let mut failure = None;
    for (from, to, _) in &plan {
        // The source entry's identity, captured before its move: custody is verified against it
        // after the moves (a competitor renamed over the moved member inside the slot would
        // otherwise pass a name-only check, Gate-3 round 5). No identity, no move: fail closed.
        let Some(identity) = entry_identity(from) else {
            failure = Some(format!(
                "could not read the identity of {} before moving it",
                printable_path(from)
            ));
            break;
        };
        if let Err(err) = mover(from, to) {
            failure = Some(format!(
                "could not move {} -> {}: {err}",
                printable_path(from),
                printable_path(to)
            ));
            break;
        }
        moved.push((from.clone(), to.clone()));
        identities.push(identity);
    }
    // Custody FIRST, for every member that moved, before any other verification and before any
    // rollback: a member replaced or removed inside the slot after its move is not ours. It is
    // never claimed and never moved back over the source name (its original bytes were displaced
    // by the other writer's own action; zynk deleted nothing) — whichever check fails first
    // (Gate-3 round 5, ARB-DA4-CUSTODY-ROLLBACK-001).
    let displaced: Vec<PathBuf> = moved
        .iter()
        .zip(identities.iter())
        .filter(|((_, to), before)| entry_identity(to).as_ref() != Some(*before))
        .map(|((_, to), _)| to.clone())
        .collect();
    // The slot must hold exactly the members that were moved: an entry someone else created
    // inside it means the backup is not this bundle — refuse and roll back. A slot that cannot be
    // listed completely is not known to be clean: that is a failure too, never "clean".
    if failure.is_none() {
        failure = match unexpected_slot_entry(&slot, &moved, inspect) {
            Ok(None) => None,
            Ok(Some(unexpected)) => Some(format!(
                "an entry that is not a bundle member appeared in the backup slot: {}",
                printable_path(&unexpected)
            )),
            Err(err) => Some(format!(
                "the backup slot {} could not be verified after the move ({err})",
                printable_path(&slot)
            )),
        };
    }
    // The source must hold no bundle member any more: a writer that created a sidecar beside the
    // main after the plan was built (and before the main moved) would otherwise split the bundle
    // — the main in the slot, a data-bearing WAL at the source — behind a "complete" backup
    // (Gate-3 round 5, WARDEN-09F-CUSTODY-001). The rescan fails closed like the plan did.
    if failure.is_none() {
        for suffix in BUNDLE_SUFFIXES.iter().chain(std::iter::once(&"")) {
            let member = sidecar(path, suffix);
            match inspect.present(&member) {
                Ok(false) => {}
                Ok(true) => {
                    failure = Some(format!(
                        "a bundle member is present at the source after the move ({}); the bundle \
                         is not complete in the backup",
                        printable_path(&member)
                    ));
                    break;
                }
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }
    }
    if !displaced.is_empty() {
        let custody = format!(
            "a member of the backup slot is missing, was replaced by another writer, or could not be \
             identified after it was moved ({}); its original bytes are not known to be in the backup \
             (a replaced member's bytes were displaced by that writer; an unreadable identity is an \
             I/O condition, not proof of a writer)",
            displaced
                .iter()
                .map(|p| printable_path(p))
                .collect::<Vec<_>>()
                .join(", ")
        );
        failure = Some(match failure {
            Some(other) => format!("{other}; {custody}"),
            None => custody,
        });
    }
    let Some(failure) = failure else {
        return Ok(RelocateOutcome {
            moved_from: path.to_path_buf(),
            moved_to: slot,
            members: plan.iter().map(|(_, _, suffix)| *suffix).collect(),
        });
    };
    let mut message = format!("zynk: {failure}; nothing was relocated");
    let mut not_restored = Vec::new();
    for (from, to) in moved.iter().rev() {
        if displaced.contains(to) {
            not_restored.push(format!(
                "{} (missing or replaced by another writer; not moved back)",
                printable_path(from)
            ));
            continue;
        }
        if let Err(restore_err) = mover(to, from) {
            not_restored.push(format!("{} ({restore_err})", printable_path(from)));
        }
    }
    if not_restored.is_empty() {
        let _ = std::fs::remove_dir(&slot);
    } else {
        message.push_str(&format!(
            " — WARNING: these members could not be moved back and now live under {}: {}",
            printable_path(&slot),
            not_restored.join(", ")
        ));
    }
    Err(message)
}

/// Create the backup slot directory atomically (fails when ANY entry exists at that name). On Unix
/// it is created owner-only (mode 0700); elsewhere `DirBuilder` inherits the parent directory's
/// permissions — the SQLite home is expected to be private to the operator, and the post-move
/// member check is the guard against an entry someone else adds (Gate-3 round 5).
fn create_private_dir(slot: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    builder.create(slot)
}

/// An entry in the slot that is not one of the members just moved there; `Err` when the slot
/// cannot be listed completely (the caller treats that as a failure, never as clean).
fn unexpected_slot_entry(
    slot: &Path,
    moved: &[(PathBuf, PathBuf)],
    inspect: &BundleInspect<'_>,
) -> std::io::Result<Option<PathBuf>> {
    Ok((inspect.list)(slot)?
        .into_iter()
        .find(|path| !moved.iter().any(|(_, to)| to == path)))
}

fn sidecar(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
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
     backup   alias of adopt: relocate the existing bundle into <path>.wrapper-backup-N/\n\
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
    // The ambient legacy database (`$HOME/.zynk/zynk-v2/zynk.db`) is a candidate for `adopt`
    // ONLY when the native path is the default one: a path selected by config, ZYNK_SQLITE_HOME
    // or ZYNK_HOME bounds every mutation to that path (Gate-3 round 4, SENT-R4-CUTOVER-003).
    let resolution = db_path::resolve_db_path();
    let probe_legacy = matches!(resolution.source, db_path::DbPathSource::DefaultHome);
    run_db_command_at_code_with(
        args,
        &resolution.db_path,
        probe_legacy,
        &mut StdOut,
        &mut StdErr,
    )
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
    // An explicit path never probes the ambient legacy database.
    run_db_command_at_code_with(args, path, false, out, err)
}

fn run_db_command_at_code_with(
    args: &[String],
    path: &Path,
    probe_legacy: bool,
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
                DbLeafArgs::Run => cmd_relocate(verb, path, probe_legacy, out, err),
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

fn cmd_relocate(
    verb: &str,
    path: &Path,
    probe_legacy: bool,
    out: &mut dyn Sink,
    err: &mut dyn Sink,
) -> i32 {
    let class = match classify_raw(path) {
        Ok(class) => class,
        // Existing data the guards refuse to open — a rollback journal that looks hot, or orphan
        // sidecars beside an absent main file — is exactly what relocation is for: the complete
        // bundle moves aside intact (a crashed foreign writer can still recover it at the backup
        // location) and the native path becomes usable.
        Err(refusal)
            if matches!(
                refusal.code,
                "db_hot_journal" | "db_orphan_sidecar" | "db_sidecar_link"
            ) =>
        {
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
            if probe_legacy && legacy != path && legacy.exists() {
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
        let first = next_backup_path(&db).unwrap();
        assert_eq!(first, db.with_file_name("zynk.db.wrapper-backup-0"));
        // Once slot 0 exists, the next call picks slot 1.
        std::fs::write(&first, b"y").unwrap();
        assert_eq!(
            next_backup_path(&db).unwrap(),
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
        let backup = db
            .with_file_name("zynk.db.wrapper-backup-0")
            .join("zynk.db");
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
        adopt_moves_the_complete_bundle_including_an_inactive_persist_journal_body();
    }

    /// Windows CI runs only `windows_`-prefixed tests: the real `MoveFileExW` path must be
    /// exercised there, not merely compiled (Gate-3 round 4, WARDEN-R4-WIN-001).
    #[cfg(windows)]
    #[test]
    fn windows_adopt_moves_the_complete_bundle_including_an_inactive_persist_journal() {
        adopt_moves_the_complete_bundle_including_an_inactive_persist_journal_body();
    }

    fn adopt_moves_the_complete_bundle_including_an_inactive_persist_journal_body() {
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
        let backup = db
            .with_file_name("zynk.db.wrapper-backup-0")
            .join("zynk.db");
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
        let backup = db
            .with_file_name("zynk.db.wrapper-backup-0")
            .join("zynk.db");
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
        let sentinel = db.with_file_name("zynk.db.wrapper-backup-0");
        std::fs::write(&sentinel, b"SENTINEL: an earlier backup").unwrap();
        let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();

        let (code, out, err) = run(&["adopt"], &db);
        assert_success(code, &out, &err);
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"SENTINEL: an earlier backup"
        );
        let slot1 = db
            .with_file_name("zynk.db.wrapper-backup-1")
            .join("zynk.db");
        assert!(
            slot1.exists(),
            "slot 0 is occupied: slot 1 must be used\n{out}"
        );
        assert_eq!(
            std::fs::read(bundle_member(&slot1, "-wal")).unwrap(),
            source_wal
        );
        assert!(
            std::fs::metadata(&sentinel).unwrap().is_file(),
            "the occupant at the slot name is untouched"
        );
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
        let sentinel = db.with_file_name("zynk.db.wrapper-backup-0");
        std::os::unix::fs::symlink("nowhere-at-all", &sentinel).unwrap();
        let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();

        let (code, out, err) = run(&["adopt"], &db);
        assert_success(code, &out, &err);
        let meta = std::fs::symlink_metadata(&sentinel).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "the dangling slot symlink was replaced\n{out}"
        );
        let slot1 = db
            .with_file_name("zynk.db.wrapper-backup-1")
            .join("zynk.db");
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
        relocation_refuses_a_target_that_appears_after_preflight_body();
    }

    /// Windows CI runs only `windows_`-prefixed tests: the real `MoveFileExW` path must be
    /// exercised there, not merely compiled (Gate-3 round 4, WARDEN-R4-WIN-001).
    #[cfg(windows)]
    #[test]
    fn windows_relocation_refuses_a_target_that_appears_after_preflight() {
        relocation_refuses_a_target_that_appears_after_preflight_body();
    }

    fn relocation_refuses_a_target_that_appears_after_preflight_body() {
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
        let slot0 = target
            .with_file_name("target.db.wrapper-backup-0")
            .join("target.db");
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

    fn unsupported(_from: &Path, _to: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "not on this filesystem",
        ))
    }

    #[test]
    fn a_missing_atomic_move_is_an_actionable_refusal_not_a_fallback() {
        a_missing_atomic_move_is_an_actionable_refusal_not_a_fallback_body();
    }

    /// Windows CI runs only `windows_`-prefixed tests: the real `MoveFileExW` path must be
    /// exercised there, not merely compiled (Gate-3 round 4, WARDEN-R4-WIN-001).
    #[cfg(windows)]
    #[test]
    fn windows_a_missing_atomic_move_is_an_actionable_refusal_not_a_fallback() {
        a_missing_atomic_move_is_an_actionable_refusal_not_a_fallback_body();
    }

    fn a_missing_atomic_move_is_an_actionable_refusal_not_a_fallback_body() {
        // Gate-3 round 3 (arbiter G3-PRE-B135-002, Codex R13): a two-step link/copy + unlink of
        // the source name deleted a file another writer placed there in between. Without the
        // platform's atomic no-replace rename the member is refused; nothing is moved or created.
        let dir = tmp_home("move-no-atomic");
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("src");
        let to = dir.join("dst");
        std::fs::write(&from, b"SOURCE").unwrap();
        let err = move_no_replace_with(&from, &to, &unsupported).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("manually"), "{err}");
        assert_eq!(std::fs::read(&from).unwrap(), b"SOURCE");
        assert!(!entry_exists(&to));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_ordinary_move_failure_keeps_its_cause() {
        let dir = tmp_home("move-denied");
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("src");
        let to = dir.join("dst");
        std::fs::write(&from, b"SOURCE").unwrap();
        let denied = |_from: &Path, _to: &Path| -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "rename denied",
            ))
        };
        let err = move_no_replace_with(&from, &to, &denied).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!err.to_string().contains("manually"), "{err}");
        assert_eq!(std::fs::read(&from).unwrap(), b"SOURCE");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_source_replaced_before_the_atomic_move_moves_whole_and_nothing_else_is_deleted() {
        a_source_replaced_before_the_atomic_move_moves_whole_and_nothing_else_is_deleted_body();
    }

    /// Windows CI runs only `windows_`-prefixed tests: the real `MoveFileExW` path must be
    /// exercised there, not merely compiled (Gate-3 round 4, WARDEN-R4-WIN-001).
    #[cfg(windows)]
    #[test]
    fn windows_a_source_replaced_before_the_atomic_move_moves_whole_and_nothing_else_is_deleted() {
        a_source_replaced_before_the_atomic_move_moves_whole_and_nothing_else_is_deleted_body();
    }

    #[cfg(any(any(target_os = "linux", target_os = "macos"), windows))]
    fn a_source_replaced_before_the_atomic_move_moves_whole_and_nothing_else_is_deleted_body() {
        // The boundary the atomic move guarantees: whatever sits at the source name at the instant
        // of the move is moved intact; a writer that replaced the source just before discarded its
        // own predecessor, and zynk deletes nothing beyond that single rename.
        let dir = tmp_home("move-source-swap");
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("src");
        let to = dir.join("dst");
        let replacement = dir.join("replacement");
        std::fs::write(&from, b"ORIGINAL").unwrap();
        std::fs::write(&replacement, b"REPLACEMENT").unwrap();
        let swapping = |from: &Path, to: &Path| {
            std::fs::rename(&replacement, from).unwrap();
            atomic_rename_noreplace(from, to)
        };
        move_no_replace_with(&from, &to, &swapping).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"REPLACEMENT");
        assert!(!entry_exists(&from) && !entry_exists(&replacement));
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "nothing else touched"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn adopt_moves_a_dangling_source_sidecar_link_and_native_startup_follows() {
        // Gate-3 round 4 (SENT-R4-CUTOVER-001): a dangling sidecar link beside the database was
        // skipped by the source plan (`exists` follows links), left at the native path, and the next
        // native start failed to open the database through it. The link entry itself must move.
        for suffix in BUNDLE_SUFFIXES {
            let dir = tmp_home(&format!("adopt-dangling-source{suffix}"));
            std::fs::create_dir_all(&dir).unwrap();
            let db = dir.join("zynk.db");
            std::fs::write(&db, b"not a database, but present").unwrap();
            let link = bundle_member(&db, suffix);
            let escaped = dir.join("external").join("escaped-sidecar");
            std::os::unix::fs::symlink(&escaped, &link).unwrap();

            let (code, out, err) = run(&["adopt"], &db);
            assert_success(code, &out, &err);
            assert!(
                !entry_exists(&link),
                "the dangling {suffix} link stayed at the native path\n{out}"
            );
            assert!(!entry_exists(&db));
            assert!(!escaped.exists(), "the link target must never be touched");
            block_on(async {
                let conn = crate::zynk::db::open_migrated_at(&db).await?;
                drop(conn);
                Ok::<(), crate::zynk::db::DbError>(())
            })
            .unwrap_or_else(|e| panic!("native startup after adopt failed for {suffix}: {e}"));
            assert!(
                !escaped.exists(),
                "startup must not create the old link target"
            );
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn an_explicit_db_path_never_relocates_the_ambient_legacy_database() {
        // Gate-3 round 4 (SENT-R4-CUTOVER-003): with the native path absent, adopt fell back to the
        // ambient `$HOME/.zynk/zynk-v2/zynk.db` even when ZYNK_SQLITE_HOME selected another
        // directory — a mutation outside the path the operator chose.
        let home = tmp_home("adopt-legacy-home");
        let selected = tmp_home("adopt-legacy-selected");
        std::fs::create_dir_all(&selected).unwrap();
        let legacy = db_path::legacy_native_db_path_with_home(&home);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"LEGACY DATABASE BYTES").unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var(crate::zynk::db_path::ZYNK_SQLITE_HOME_ENV, &selected);
        std::env::remove_var(crate::zynk::db_path::ZYNK_HOME_ENV);
        let code = run_db_command_code(&["adopt".to_string()]);
        assert_eq!(code, 0, "an absent selected path is a no-op");
        assert!(
            entry_exists(&legacy),
            "the ambient legacy database was relocated"
        );
        assert_eq!(std::fs::read(&legacy).unwrap(), b"LEGACY DATABASE BYTES");
        assert!(!entry_exists(
            &legacy.with_file_name("zynk.db.wrapper-backup-0")
        ));
        assert!(!entry_exists(&selected.join("zynk.db")));
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(selected).ok();
    }

    #[test]
    fn a_member_planted_in_the_reserved_slot_is_refused_and_rolled_back() {
        // Gate-3 round 4 (SENT-R4-CUTOVER-002): with a main-only source, a competitor created the
        // unplanned `<backup>-wal` beside the moved main and adopt reported a complete backup that
        // was a mixed bundle. The slot is a reserved directory now; an entry that is not a moved
        // member is detected after the moves, the bundle rolls back and the command fails.
        let dir = tmp_home("adopt-planted-member");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&db, b"MAIN ONLY").unwrap();
        let slot = next_backup_path(&db).unwrap();
        let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
            // The competitor lands inside the reserved slot right before the main move.
            std::fs::write(
                slot.join("zynk.db-wal"),
                b"FOREIGN WAL CREATED AFTER PREFLIGHT",
            )
            .unwrap();
            move_no_replace(from, to).map_err(|err| err.to_string())
        };
        let err = relocate_bundle(&db, &mut mover).unwrap_err();
        assert!(
            err.contains("not a bundle member") && err.contains("nothing was relocated"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(&db).unwrap(),
            b"MAIN ONLY",
            "the source is back"
        );
        assert!(
            !entry_exists(&slot.join("zynk.db")),
            "no main left in the slot"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_source_name_recreated_during_rollback_is_reported_not_replaced() {
        // Gate-3 round 4 (ARCH-CUTOVER-ATOMICITY-001): after a sidecar moved, a writer recreated
        // its original name; a later failure rolls back, the restore refuses to replace that file,
        // and the error names the member that now lives under the slot.
        let dir = tmp_home("adopt-rollback-collision");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let wal = bundle_member(&db, "-wal");
        let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
            if from == db {
                // Sidecars already moved; a writer recreates the WAL name, then the main fails.
                std::fs::write(&wal, b"WRITER RECREATED THE WAL NAME").unwrap();
                return Err("injected: main file cannot move".to_string());
            }
            move_no_replace(from, to).map_err(|err| err.to_string())
        };
        let err = relocate_bundle(&db, &mut mover).unwrap_err();
        assert!(
            err.contains("WARNING")
                && err.contains("could not be moved back")
                && err.contains("-wal"),
            "the blocked restore must be reported: {err}"
        );
        assert_eq!(
            std::fs::read(&wal).unwrap(),
            b"WRITER RECREATED THE WAL NAME",
            "the writer's file was replaced"
        );
        assert!(db.exists(), "the main file never moved");
        let slot = db.with_file_name("zynk.db.wrapper-backup-0");
        assert!(
            entry_exists(&slot.join("zynk.db-wal")),
            "the original WAL stays under the slot"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    fn io_err(kind: std::io::ErrorKind, what: &str) -> std::io::Error {
        std::io::Error::new(kind, what.to_string())
    }

    #[test]
    fn a_member_whose_metadata_cannot_be_read_refuses_the_relocation() {
        // Codex Gate-2 R20 on a3a71bc: EIO on the source `-wal` metadata read as "absent", so adopt
        // moved main + shm, left the data-bearing WAL behind and exited 0. Only NotFound is absent.
        let dir = tmp_home("adopt-metadata-eio");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let wal = bundle_member(&db, "-wal");
        let failing_entry = |path: &Path| -> std::io::Result<bool> {
            if path == wal {
                Err(io_err(std::io::ErrorKind::Other, "input/output error"))
            } else {
                entry_present(path)
            }
        };
        let inspect = BundleInspect {
            entry: &failing_entry,
            list: &list_dir,
        };
        let mut mover =
            |from: &Path, to: &Path| move_no_replace(from, to).map_err(|e| e.to_string());
        let err = relocate_bundle_with(&db, &mut mover, &inspect).unwrap_err();
        assert!(
            err.contains("cannot inspect") && err.contains("input/output error"),
            "{err}"
        );
        assert!(db.exists() && wal.exists(), "nothing may move");
        assert!(!entry_exists(
            &db.with_file_name("zynk.db.wrapper-backup-0")
        ));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_slot_that_cannot_be_listed_is_not_clean() {
        // Codex Gate-2 R20 on a3a71bc: an unexpected WAL in the reserved slot plus opendir EACCES
        // (or readdir EIO) was swallowed by `.ok()`/`flatten`, and adopt exited 0 with a mixed
        // backup. A listing that fails is a verification failure: rollback + error.
        for (label, failure) in [
            ("opendir", std::io::ErrorKind::PermissionDenied),
            ("readdir", std::io::ErrorKind::Other),
        ] {
            let dir = tmp_home(&format!("adopt-slot-list-{label}"));
            let db = dir.join("zynk.db");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&db, b"MAIN ONLY").unwrap();
            let slot = next_backup_path(&db).unwrap();
            let failing_list = |_path: &Path| -> std::io::Result<Vec<PathBuf>> {
                Err(io_err(failure, "cannot list the slot"))
            };
            let inspect = BundleInspect {
                entry: &entry_present,
                list: &failing_list,
            };
            let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
                std::fs::write(slot.join("zynk.db-wal"), b"FOREIGN WAL").unwrap();
                move_no_replace(from, to).map_err(|e| e.to_string())
            };
            let err = relocate_bundle_with(&db, &mut mover, &inspect).unwrap_err();
            assert!(
                err.contains("could not be verified") && err.contains("nothing was relocated"),
                "{label}: {err}"
            );
            assert_eq!(
                std::fs::read(&db).unwrap(),
                b"MAIN ONLY",
                "{label}: the source is back"
            );
            assert!(
                !entry_exists(&slot.join("zynk.db")),
                "{label}: no main left in the slot"
            );
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_member_replaced_inside_the_slot_after_its_move_is_not_claimed() {
        // Gate-3 round 5 (custody): after the real move, a competitor atomically renamed over the
        // moved member inside the slot passed a name-only check and adopt claimed success with the
        // competitor's bytes as the backup. Identity is verified per member: the relocation fails,
        // the intact members move back, the displaced member is named and not moved back.
        let dir = tmp_home("adopt-postmove-replace");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();
        let competitor = dir.join("competitor.db");
        std::fs::write(&competitor, b"COMPETITOR RENAMED OVER THE MOVED MAIN").unwrap();
        let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
            move_no_replace(from, to).map_err(|e| e.to_string())?;
            if from == db {
                std::fs::rename(&competitor, to).unwrap();
            }
            Ok(())
        };
        let err = relocate_bundle(&db, &mut mover).unwrap_err();
        assert!(
            err.contains("was replaced by another writer") && err.contains("not moved back"),
            "{err}"
        );
        let slot = db.with_file_name("zynk.db.wrapper-backup-0");
        assert_eq!(
            std::fs::read(slot.join("zynk.db")).unwrap(),
            b"COMPETITOR RENAMED OVER THE MOVED MAIN",
            "the competitor's file is left where it put it"
        );
        assert!(
            !db.exists(),
            "the displaced original is not resurrected from foreign bytes"
        );
        assert_eq!(
            std::fs::read(bundle_member(&db, "-wal")).unwrap(),
            source_wal,
            "the intact WAL moved back"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_sidecar_created_at_the_source_after_the_scan_fails_the_relocation() {
        // Gate-3 round 5 (WARDEN-09F-CUSTODY-001): a writer created a nonempty `-wal` at the source
        // after the plan was built and before the main moved; the main moved, the new WAL stayed
        // at the source, slot-only verification passed and adopt claimed a complete backup. The
        // source names are rescanned after the moves: any member left there fails the relocation.
        let dir = tmp_home("adopt-late-source-wal");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&db, b"MAIN ONLY AT SCAN TIME").unwrap();
        let wal = bundle_member(&db, "-wal");
        let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
            if from == db {
                // The writer lands a WAL beside the main right before the main moves.
                std::fs::write(&wal, b"WAL CREATED AFTER THE PLAN").unwrap();
            }
            move_no_replace(from, to).map_err(|e| e.to_string())
        };
        let err = relocate_bundle(&db, &mut mover).unwrap_err();
        assert!(
            err.contains("at the source after") && err.contains("nothing was relocated"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(&db).unwrap(),
            b"MAIN ONLY AT SCAN TIME",
            "the main moved back"
        );
        assert_eq!(std::fs::read(&wal).unwrap(), b"WAL CREATED AFTER THE PLAN");
        let slot = db.with_file_name("zynk.db.wrapper-backup-0");
        assert!(
            !entry_exists(&slot.join("zynk.db")),
            "no main left in the slot"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_member_deleted_from_the_slot_after_its_move_is_not_claimed() {
        // Gate-3 round 5 (SENT-R5-CUTOVER-001): verification is exact, not only "no extra": a moved
        // member that is gone by the time the slot is verified fails the relocation, is named, and
        // the intact members move back.
        let dir = tmp_home("adopt-postmove-delete");
        let db = dir.join("zynk.db");
        std::fs::create_dir_all(&dir).unwrap();
        plant_foreign_wal_pair(&db);
        let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();
        let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
            move_no_replace(from, to).map_err(|e| e.to_string())?;
            if from == db {
                std::fs::remove_file(to).unwrap();
            }
            Ok(())
        };
        let err = relocate_bundle(&db, &mut mover).unwrap_err();
        assert!(
            err.contains("was replaced by another writer") && err.contains("not moved back"),
            "{err}"
        );
        assert!(!db.exists(), "the deleted member cannot be resurrected");
        assert_eq!(
            std::fs::read(bundle_member(&db, "-wal")).unwrap(),
            source_wal,
            "the intact WAL moved back"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_replaced_member_is_never_rolled_back_when_another_check_fails_first() {
        // Gate-3 round 5 (ARB-DA4-CUSTODY-ROLLBACK-001): the displacement check ran only when no
        // earlier verification had failed, so a competitor's replacement could be moved back over
        // the source name during rollback. Two compound races: replacement + an unexpected slot
        // entry, and replacement + a sidecar created at the source after the plan.
        for variant in ["unexpected-slot-entry", "late-source-sidecar"] {
            let dir = tmp_home(&format!("adopt-compound-{variant}"));
            let db = dir.join("zynk.db");
            std::fs::create_dir_all(&dir).unwrap();
            plant_foreign_wal_pair(&db);
            let source_wal = std::fs::read(bundle_member(&db, "-wal")).unwrap();
            let competitor = dir.join("competitor.db");
            std::fs::write(&competitor, b"COMPETITOR").unwrap();
            let slot = next_backup_path(&db).unwrap();
            let mut mover = |from: &Path, to: &Path| -> Result<(), String> {
                move_no_replace(from, to).map_err(|e| e.to_string())?;
                if from == db {
                    std::fs::rename(&competitor, to).unwrap();
                    if variant == "unexpected-slot-entry" {
                        std::fs::write(slot.join("unexpected"), b"x").unwrap();
                    } else {
                        std::fs::write(bundle_member(&db, "-wal"), b"LATE WAL").unwrap();
                    }
                }
                Ok(())
            };
            let err = relocate_bundle(&db, &mut mover).unwrap_err();
            assert!(
                err.contains("was replaced by another writer") && err.contains("not moved back"),
                "{variant}: the displaced member must be named: {err}"
            );
            assert!(
                !db.exists(),
                "{variant}: the competitor must not land at the source name"
            );
            assert_eq!(
                std::fs::read(slot.join("zynk.db")).unwrap(),
                b"COMPETITOR",
                "{variant}: the competitor stays where it put itself"
            );
            if variant == "unexpected-slot-entry" {
                assert_eq!(
                    std::fs::read(bundle_member(&db, "-wal")).unwrap(),
                    source_wal
                );
            }
            std::fs::remove_dir_all(dir).ok();
        }
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
            move_no_replace(from, to).map_err(|err| err.to_string())
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
