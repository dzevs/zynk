// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::{
    io::{self, Write},
    os::fd::RawFd,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Once, OnceLock},
};

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, Signal,
};

const PROCESS_DETECTION_ENV_VAR: &str = "ZYNK_PROCESS_DETECTION";
const CHILD_GROUPS_SCAN_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessDetectionMode {
    Native,
    ChildGroups,
}

fn parse_process_detection_mode(value: Option<&str>) -> Result<ProcessDetectionMode, &str> {
    match value {
        None | Some("") | Some("native") => Ok(ProcessDetectionMode::Native),
        Some("child-groups") => Ok(ProcessDetectionMode::ChildGroups),
        Some(value) => Err(value),
    }
}

fn process_detection_mode_from_value(value: Option<&str>) -> ProcessDetectionMode {
    parse_process_detection_mode(value).unwrap_or_else(|value| {
        tracing::warn!(
            variable = PROCESS_DETECTION_ENV_VAR,
            %value,
            "unknown process detection mode; using native detection"
        );
        ProcessDetectionMode::Native
    })
}

fn process_detection_mode() -> ProcessDetectionMode {
    static MODE: OnceLock<ProcessDetectionMode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let value = std::env::var(PROCESS_DETECTION_ENV_VAR).ok();
        process_detection_mode_from_value(value.as_deref())
    })
}

pub fn raise_server_nofile_limit() {}

/// The parent PID of `pid` and the start time the kernel stamped on it, from a
/// SINGLE read of `/proc/<pid>/stat` (ADR 0014).
///
/// Both facts come from one read on purpose: the ancestry walk compares the pid
/// it is standing on against a principal's start time, and two reads could
/// straddle the moment that pid changed hands.
///
/// `None` when the process is gone, when `/proc` cannot be read, or when the
/// parent is not a real process (a `ppid` of 0 is PID 1, which is inside no
/// pane) — every one of which is a refusal at the call site.
pub fn process_parent_and_start_time(pid: u32) -> Option<(u32, u64)> {
    // /proc/<pid>/stat: "pid (comm) state ppid pgrp ...". The (comm) field can
    // contain spaces and parens, so the numeric fields are read after the LAST
    // ')': state(0) ppid(1) ... starttime(19), i.e. stat fields 3 and 22.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ppid: i32 = fields.get(1)?.parse().ok()?;
    let start_time: u64 = fields.get(19)?.parse().ok()?;
    (ppid > 0).then_some((ppid as u32, start_time))
}

/// The start time `pid` was stamped with, in clock ticks since boot.
///
/// This is the half of a process's identity a pid alone does not carry: pids are
/// reused, so a pid that outlives its process can be handed to an unrelated one,
/// and only the start time tells the two apart (ADR 0014,
/// ARCH-E8-ADR14-PID-REUSE-001). `None` when the process is gone or `/proc`
/// cannot be read.
pub fn process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    // starttime is stat field 22, the 20th after `(comm)`. Zero is not a usable
    // identity here (nothing a pane starts is stamped at boot tick 0), and the
    // pane-tree check reads it as "no start time captured" and fails closed.
    let start_time: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    (start_time > 0).then_some(start_time)
}

/// The effective UID this server runs as. A peer on another UID is refused by
/// the pane-bound methods before any ancestry is walked (ADR 0014).
pub fn current_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, touches no memory and cannot fail.
    unsafe { libc::geteuid() }
}

/// Collect the foreground terminal job for a given child PID.
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    foreground_job_with(
        foreground_process_group_id(child_pid),
        process_detection_mode,
        || child_groups_foreground_process_group(child_pid),
        foreground_job_for_group,
    )
}

fn foreground_job_with(
    observed_group: Option<u32>,
    mode: impl FnOnce() -> ProcessDetectionMode,
    child_group: impl FnOnce() -> Option<u32>,
    job_for_group: impl FnOnce(u32) -> Option<ForegroundJob>,
) -> Option<ForegroundJob> {
    if let Some(group) = observed_group {
        return job_for_group(group);
    }
    if mode() != ProcessDetectionMode::ChildGroups {
        return None;
    }
    job_for_group(child_group()?)
}

fn foreground_job_for_group(tpgid: u32) -> Option<ForegroundJob> {
    let mut processes = Vec::new();

    for entry in std::fs::read_dir("/proc").ok()? {
        // Skip a transient bad entry (a pid dir vanishing mid-scan, or a non-UTF8 name) instead of
        // aborting the whole scan — one error must not collapse foreground detection to None.
        let Ok(entry) = entry else { continue };
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }

        let pid: u32 = match pid_str.parse() {
            Ok(pid) => pid,
            Err(_) => continue,
        };

        let Some((pgrp, name)) = process_pgrp_and_comm(pid) else {
            continue;
        };
        if pgrp as u32 != tpgid {
            continue;
        }

        let argv = process_argv(pid);
        processes.push(ForegroundProcess {
            pid,
            name,
            argv0: None,
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        });
    }

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id: tpgid,
        processes,
    })
}

/// Best effort only: without the native terminal signal, a background job can
/// be mistaken for the foreground. No recursive process-tree scan is used.
fn child_groups_foreground_process_group(child_pid: u32) -> Option<u32> {
    let shell_group_id = process_pgrp_and_comm(child_pid)
        .map(|(pgrp, _)| pgrp)
        .filter(|pgrp| *pgrp > 0)? as u32;
    let result = child_groups_foreground_process_group_with(
        child_pid,
        shell_group_id,
        process_task_ids,
        process_task_children,
        |pid| process_pgrp_and_comm(pid).map(|(pgrp, _)| pgrp),
    );
    match result {
        Ok(group) => group,
        Err(error) => {
            // A missing /proc children reader must not look like an empty job.
            // Keep retries enabled, but do not log on every detector tick.
            static WARNED: Once = Once::new();
            WARNED.call_once(|| {
                tracing::warn!(child_pid, %error, "child-group reader failed; no fallback group for this probe (further warnings suppressed)");
            });
            None
        }
    }
}

fn child_groups_foreground_process_group_with(
    child_pid: u32,
    shell_group_id: u32,
    mut task_ids: impl FnMut(u32) -> io::Result<Vec<u32>>,
    mut task_children: impl FnMut(u32, u32) -> io::Result<Vec<u32>>,
    mut process_group_id: impl FnMut(u32) -> Option<i32>,
) -> io::Result<Option<u32>> {
    let mut newest = None;
    let mut scanned = 0usize;
    for tid in task_ids(child_pid)? {
        for child in task_children(child_pid, tid)? {
            if scanned >= CHILD_GROUPS_SCAN_LIMIT {
                return Ok(None);
            }
            scanned += 1;
            let Some(pgrp) = process_group_id(child) else {
                continue;
            };
            if pgrp <= 0 || pgrp as u32 == shell_group_id {
                continue;
            }
            let pgrp = pgrp as u32;
            newest = Some(newest.map_or(pgrp, |current: u32| current.max(pgrp)));
        }
    }
    Ok(newest.or(Some(shell_group_id)))
}

fn process_task_ids(pid: u32) -> io::Result<Vec<u32>> {
    let mut tids = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/task"))? {
        if let Some(tid) = numeric_file_name(&entry?) {
            tids.push(tid);
        }
    }
    Ok(tids)
}

fn process_task_children(pid: u32, tid: u32) -> io::Result<Vec<u32>> {
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/children"))?;
    Ok(children
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect())
}

fn numeric_file_name(entry: &std::fs::DirEntry) -> Option<u32> {
    let name = entry.file_name();
    let name = name.to_str()?;
    if !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    name.parse().ok()
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let (pgrp, name) = process_pgrp_and_comm(process_group_id)?;
    if pgrp as u32 != process_group_id {
        return None;
    }

    let argv = process_argv(process_group_id);
    Some(ForegroundJob {
        process_group_id,
        processes: vec![ForegroundProcess {
            pid: process_group_id,
            name,
            argv0: None,
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        }],
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    // /proc/<pid>/stat format: "pid (comm) state ppid pgrp session tty_nr tpgid ..."
    // The (comm) field can contain spaces and parens, so we find the last ')' first.
    let stat = std::fs::read_to_string(format!("/proc/{child_pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After (comm): state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    let tpgid: i32 = fields.get(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid as u32)
}

pub fn foreground_process_group_id_for_tty_fd(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

fn process_pgrp_and_comm(pid: u32) -> Option<(i32, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let comm = stat.get(1 + stat.find('(')?..close)?.to_string();
    let rest = stat.get(close + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let pgrp: i32 = fields.get(2)?.parse().ok()?;
    Some((pgrp, comm))
}

fn process_argv(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let parts: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// Get the current working directory of a process.
/// Uses /proc/<pid>/cwd symlink.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Read a zynk agent identity hint from a process environment.
pub fn process_agent_hint(pid: u32) -> Option<crate::detect::Agent> {
    if pid == 0 {
        return None;
    }
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    parse_agent_env_hint(&environ)
}

fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"ZYNK_AGENT=") else {
            continue;
        };
        let value = std::str::from_utf8(value).ok()?;
        return crate::detect::parse_agent_label(value);
    }
    None
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    let Some(session_id) = process_session_id(child_pid) else {
        return Vec::new();
    };

    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }

        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if process_session_id(pid) == Some(session_id) {
            pids.push(pid);
        }
    }
    pids
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    let sig = match signal {
        Signal::Hangup => libc::SIGHUP,
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };

    for &pid in pids {
        if pid == 0 {
            continue;
        }
        unsafe {
            libc::kill(pid as i32, sig);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    for command in clipboard_commands() {
        if run_clipboard_command(&command, bytes) {
            return true;
        }
    }
    false
}

pub fn read_clipboard_text() -> Option<String> {
    for command in read_clipboard_text_commands() {
        if let Some(text) = read_clipboard_text_with_command(&command) {
            return Some(text);
        }
    }
    None
}

pub fn open_url(url: &str) -> std::io::Result<Option<std::process::Child>> {
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Some)
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    for (mime, extension) in [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/jpg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("image/bmp", "bmp"),
    ] {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Some(image) =
                read_validated_clipboard_image("wl-paste", &["--type", mime], extension)
            {
                return Some(image);
            }
        }

        if std::env::var_os("DISPLAY").is_some() {
            if let Some(image) = read_validated_clipboard_image(
                "xclip",
                &["-selection", "clipboard", "-t", mime, "-o"],
                extension,
            ) {
                return Some(image);
            }
        }
    }

    None
}

fn read_validated_clipboard_image(
    program: &str,
    args: &[&str],
    extension: &'static str,
) -> Option<ClipboardImage> {
    let bytes = read_clipboard_image_with_command(program, args)?;
    if !bytes_match_image_signature(extension, &bytes) {
        return None;
    }
    Some(ClipboardImage { bytes, extension })
}

fn bytes_match_image_signature(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP",
        "bmp" => {
            if bytes.len() < 26 || !bytes.starts_with(b"BM") {
                return false;
            }
            let offset = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
            (26..=bytes.len()).contains(&offset)
        }
        _ => false,
    }
}

/// Show a native desktop notification through libnotify's command-line helper.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Ok(false);
    }

    let mut cmd = command("notify-send");
    cmd.arg("--").arg(title);
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        cmd.arg(body);
    }
    run_notification_command(cmd)
}

fn run_notification_command(mut command: Command) -> std::io::Result<bool> {
    let status = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    Ok(status.success())
}

fn read_clipboard_image_with_command(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command.args(args);
    read_clipboard_image_with_spawned_command(command)
}

fn read_clipboard_image_with_spawned_command(command: Command) -> Option<Vec<u8>> {
    read_clipboard_image_with_spawned_command_max(
        command,
        crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD,
    )
}

fn read_clipboard_image_with_spawned_command_max(
    mut command: Command,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;

    let read = match read_limited_reader(stdout, max_bytes) {
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };

    if read == LimitedRead::Oversized {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }

    match read {
        LimitedRead::Complete(bytes) => Some(bytes),
        LimitedRead::Empty | LimitedRead::Oversized => None,
    }
}

fn clipboard_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-copy",
            args: &["--type", "text/plain;charset=utf-8"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-in"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--input"],
        });
    }

    commands
}

fn read_clipboard_text_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain;charset=utf-8"],
        });
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-out"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--output"],
        });
    }

    commands
}

fn read_clipboard_text_with_command(command: &ClipboardCommand) -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;

    let mut child = Command::new(command.program)
        .args(command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    let read = match read_limited_reader(stdout, MAX_CLIPBOARD_TEXT_BYTES) {
        Ok(LimitedRead::Oversized) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };

    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }

    match read {
        LimitedRead::Complete(bytes) => String::from_utf8(bytes).ok(),
        LimitedRead::Empty => None,
        LimitedRead::Oversized => unreachable!("oversized clipboard text is handled before wait"),
    }
}

fn run_clipboard_command(command: &ClipboardCommand, bytes: &[u8]) -> bool {
    let mut child = match Command::new(command.program)
        .args(command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    if stdin.write_all(bytes).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    drop(stdin);

    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn process_session_id(pid: u32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn process_detection_mode_requires_explicit_child_groups_value() {
        for value in [None, Some(""), Some("native")] {
            assert_eq!(
                parse_process_detection_mode(value),
                Ok(ProcessDetectionMode::Native)
            );
        }
        assert_eq!(
            parse_process_detection_mode(Some("child-groups")),
            Ok(ProcessDetectionMode::ChildGroups)
        );
        for value in ["gvisor", "auto", "Child-Groups", "child-groups "] {
            assert_eq!(parse_process_detection_mode(Some(value)), Err(value));
        }
    }

    #[test]
    fn unknown_process_detection_mode_warns_and_uses_native() {
        #[derive(Clone)]
        struct LogBuffer(std::sync::Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for LogBuffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = LogBuffer(std::sync::Arc::new(Mutex::new(Vec::new())));
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .without_time()
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            assert_eq!(
                process_detection_mode_from_value(Some("gvisor")),
                ProcessDetectionMode::Native
            );
        });
        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("WARN"), "{log}");
        assert!(
            log.contains("unknown process detection mode; using native detection"),
            "{log}"
        );
        assert!(
            log.contains("ZYNK_PROCESS_DETECTION") && log.contains("gvisor"),
            "{log}"
        );
    }

    #[test]
    fn native_foreground_group_wins_even_when_its_job_lookup_fails() {
        for mode in [
            ProcessDetectionMode::Native,
            ProcessDetectionMode::ChildGroups,
        ] {
            for found in [false, true] {
                let job = foreground_job_with(
                    Some(42),
                    || mode,
                    || panic!("native lookup invoked fallback"),
                    |group| {
                        assert_eq!(group, 42);
                        found.then_some(ForegroundJob {
                            process_group_id: group,
                            processes: Vec::new(),
                        })
                    },
                );
                assert_eq!(job.map(|job| job.process_group_id), found.then_some(42));
            }
        }
    }

    #[test]
    fn missing_native_group_requires_opt_in_and_a_fallback_group() {
        assert!(foreground_job_with(
            None,
            || ProcessDetectionMode::Native,
            || panic!("default invoked fallback"),
            |_| panic!("default looked up a group"),
        )
        .is_none());
        for inferred in [None, Some(300)] {
            let job = foreground_job_with(
                None,
                || ProcessDetectionMode::ChildGroups,
                || inferred,
                |group| {
                    assert_eq!(Some(group), inferred);
                    Some(ForegroundJob {
                        process_group_id: group,
                        processes: Vec::new(),
                    })
                },
            );
            assert_eq!(job.map(|job| job.process_group_id), inferred);
        }
    }

    #[test]
    fn child_groups_foreground_group_picks_the_newest_job() {
        let group = child_groups_foreground_process_group_with(
            100,
            90,
            |pid| {
                assert_eq!(pid, 100);
                Ok(vec![100, 101])
            },
            |pid, tid| {
                assert_eq!(pid, 100, "must not recurse into children");
                Ok(if tid == 100 {
                    vec![200]
                } else {
                    vec![300, 250]
                })
            },
            |pid| Some(pid as i32),
        )
        .unwrap();
        assert_eq!(group, Some(300));
    }

    #[test]
    fn child_groups_foreground_group_returns_to_the_shell_group() {
        let group = child_groups_foreground_process_group_with(
            100,
            90,
            |_| Ok(vec![100]),
            |_, _| Ok(vec![150, 160]),
            |_| Some(90),
        )
        .unwrap();
        assert_eq!(group, Some(90));
    }

    #[test]
    fn child_groups_foreground_group_skips_the_shell_group_and_invalid_groups() {
        let groups = std::collections::HashMap::from([
            (150, 90),
            (160, 90),
            (200, -1),
            (250, 0),
            (300, 300),
        ]);
        let group = child_groups_foreground_process_group_with(
            100,
            90,
            |_| Ok(vec![100]),
            |_, _| Ok(vec![150, 160, 200, 250, 300, 400]),
            |pid| groups.get(&pid).copied(),
        )
        .unwrap();
        assert_eq!(group, Some(300));
    }

    #[test]
    fn child_groups_foreground_group_fails_closed_at_the_scan_limit() {
        for count in [64u32, 65, 74] {
            let mut inspected = 0usize;
            let group = child_groups_foreground_process_group_with(
                100,
                100,
                |_| Ok(vec![100, 101]),
                |_, tid| {
                    Ok(if tid == 100 {
                        (1..=32).collect()
                    } else {
                        (33..=count).collect()
                    })
                },
                |pid| {
                    inspected += 1;
                    Some(pid as i32)
                },
            )
            .unwrap();
            assert_eq!(inspected, 64, "cap must apply across all tasks");
            assert_eq!(group, if count == 64 { Some(64) } else { None });
        }
    }

    #[test]
    fn child_groups_readable_empty_is_not_reader_unavailable() {
        let empty = child_groups_foreground_process_group_with(
            100,
            90,
            |_| Ok(vec![100]),
            |_, _| Ok(vec![]),
            |_| panic!("empty children probed"),
        )
        .unwrap();
        assert_eq!(empty, Some(90));
        let unavailable_tasks = child_groups_foreground_process_group_with(
            100,
            90,
            |_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            |_, _| panic!("unreadable tasks enumerated"),
            |_| panic!("unreadable tasks probed"),
        )
        .unwrap_err();
        assert_eq!(
            unavailable_tasks.kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let unavailable_children = child_groups_foreground_process_group_with(
            100,
            90,
            |_| Ok(vec![100, 101]),
            |_, tid| {
                if tid == 100 {
                    Ok(vec![300])
                } else {
                    Err(std::io::Error::from(std::io::ErrorKind::NotFound))
                }
            },
            |pid| Some(pid as i32),
        )
        .unwrap_err();
        assert_eq!(
            unavailable_children.kind(),
            std::io::ErrorKind::NotFound,
            "partial result must not hide an unavailable reader"
        );
    }

    #[test]
    fn proc_task_readers_report_live_children_empty_and_missing_files() {
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let child = ChildGuard(
            Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let me = std::process::id();
        // SAFETY: gettid takes no arguments and always returns this thread's ID.
        let tid = unsafe { libc::gettid() } as u32;
        assert!(process_task_ids(me).unwrap().contains(&tid));
        assert!(process_task_children(me, tid)
            .unwrap()
            .contains(&child.0.id()));
        assert!(process_task_children(child.0.id(), child.0.id())
            .unwrap()
            .is_empty());
        assert!(process_task_ids(u32::MAX).is_err());
        assert!(process_task_children(me, u32::MAX).is_err());
    }

    #[test]
    fn a_live_process_reports_a_start_time_and_its_parent_from_one_read() {
        // ADR 0014's principal is (pid, start time), so both facts have to be
        // readable, and the pair has to come from the same `/proc` read.
        let me = std::process::id();
        let start_time = process_start_time(me).expect("this process has a start time");
        let (parent, paired_start_time) =
            process_parent_and_start_time(me).expect("this process has a parent");
        assert_eq!(
            paired_start_time, start_time,
            "the paired read must agree with the standalone one"
        );
        assert_ne!(parent, 0, "a real parent is a real pid");
        assert_eq!(
            process_start_time(me),
            Some(start_time),
            "a start time never changes while the process lives"
        );
    }

    #[test]
    fn a_pid_with_no_process_reports_nothing() {
        // The pid ceiling is well under u32::MAX, so nothing can hold this one.
        // Both accessors fail closed, which is what every caller relies on.
        assert_eq!(process_start_time(u32::MAX), None);
        assert_eq!(process_parent_and_start_time(u32::MAX), None);
    }

    #[test]
    fn a_child_starts_later_than_its_parent_and_is_told_apart_by_it() {
        // The whole point of the second half of the principal: two DIFFERENT
        // processes are distinguishable even though a pid alone would not say so
        // (ARCH-E8-ADR14-PID-REUSE-001).
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a child");
        let child_pid = child.id();
        let child_start = process_start_time(child_pid).expect("the child has a start time");
        let (parent, _) = process_parent_and_start_time(child_pid).expect("the child has a parent");
        assert_eq!(
            parent,
            std::process::id(),
            "this test process is the parent"
        );
        assert!(
            child_start >= process_start_time(std::process::id()).expect("own start time"),
            "a child cannot have started before its parent"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0ZYNK_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"ZYNK_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"ZYNK_AGENT=not-an-agent\0"), None);
    }

    #[test]
    fn clipboard_commands_prefer_wayland_when_available() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::remove_var("DISPLAY");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "wl-copy");
    }

    #[test]
    fn clipboard_commands_include_x11_fallbacks() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program, "xclip");
        assert_eq!(commands[1].program, "xsel");
    }

    #[test]
    fn read_clipboard_text_commands_include_session_backends() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
        }

        let commands = read_clipboard_text_commands();
        assert_eq!(commands[0].program, "wl-paste");
        assert_eq!(commands[1].program, "wl-paste");
        assert_eq!(commands[2].program, "xclip");
        assert_eq!(commands[3].program, "xsel");
    }

    #[test]
    fn read_clipboard_text_with_command_reads_utf8() {
        let command = ClipboardCommand {
            program: "printf",
            args: &["feature/linear-302"],
        };

        assert_eq!(
            read_clipboard_text_with_command(&command).as_deref(),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn read_clipboard_text_with_command_rejects_oversized_output() {
        let command = ClipboardCommand {
            program: "sh",
            args: &["-c", "yes x | head -c 1048578"],
        };

        assert_eq!(read_clipboard_text_with_command(&command), None);
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_reads_under_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf image");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 16),
            Some(b"image".to_vec())
        );
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_rejects_over_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf oversized");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 4),
            None
        );
    }

    #[test]
    fn read_clipboard_image_rejects_xclip_text_served_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!("zynk-fake-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&fake_xclip)
                .expect("fake xclip metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&fake_xclip, permissions)
                .expect("fake xclip should be executable");
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_clipboard_image_rejects_wayland_xclip_fallback_text_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("zynk-fake-wayland-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_wl_paste = temp_dir.join("wl-paste");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_wl_paste, "#!/bin/sh\nexit 1\n")
            .expect("fake wl-paste should be written");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        {
            use std::os::unix::fs::PermissionsExt;

            for command in [&fake_wl_paste, &fake_xclip] {
                let mut permissions = std::fs::metadata(command)
                    .expect("fake clipboard command metadata")
                    .permissions();
                permissions.set_mode(0o700);
                std::fs::set_permissions(command, permissions)
                    .expect("fake clipboard command should be executable");
            }
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_wl_paste);
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_validated_clipboard_image_accepts_real_png_payload() {
        assert_eq!(
            read_validated_clipboard_image(
                "sh",
                &["-c", "printf '\\211PNG\\r\\n\\032\\nrest-of-image'"],
                "png"
            ),
            Some(ClipboardImage {
                bytes: b"\x89PNG\r\n\x1a\nrest-of-image".to_vec(),
                extension: "png",
            })
        );
    }

    #[test]
    fn image_signatures_match_only_their_format() {
        assert!(bytes_match_image_signature("png", b"\x89PNG\r\n\x1a\n..."));
        assert!(bytes_match_image_signature(
            "jpg",
            &[0xFF, 0xD8, 0xFF, 0xE0]
        ));
        assert!(bytes_match_image_signature("gif", b"GIF87a..."));
        assert!(bytes_match_image_signature("gif", b"GIF89a..."));
        assert!(bytes_match_image_signature(
            "webp",
            b"RIFF\x10\x00\x00\x00WEBPVP8 "
        ));

        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10] = 26;
        assert!(bytes_match_image_signature("bmp", &bmp));

        assert!(!bytes_match_image_signature("png", b"# Tasks"));
        assert!(!bytes_match_image_signature("jpg", b"plain clipboard text"));
        assert!(!bytes_match_image_signature("gif", b""));
        assert!(!bytes_match_image_signature("webp", b"RIFF but not webp"));
        assert!(!bytes_match_image_signature("bmp", b"\x89PNG\r\n\x1a\n"));
        assert!(!bytes_match_image_signature(
            "bmp",
            b"BM text is not a bitmap"
        ));
        assert!(!bytes_match_image_signature("svg", b"<svg></svg>"));
    }

    #[test]
    fn desktop_notification_separates_option_like_titles() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }

        let path =
            std::env::temp_dir().join(format!("zynk-notify-send-args-{}", std::process::id()));
        let script = "printf '%s\\n' \"$@\" > \"$ZYNK_NOTIFY_ARGS\"";
        let shown = show_desktop_notification_with_command("-danger", Some("body"), |_| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg("notify-send")
                .env("ZYNK_NOTIFY_ARGS", &path);
            cmd
        })
        .expect("notification command should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(args, "--\n-danger\nbody\n");
    }
}
