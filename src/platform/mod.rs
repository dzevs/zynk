// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundJob {
    pub process_group_id: u32,
    pub processes: Vec<ForegroundProcess>,
}

fn normalized_shell_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .to_ascii_lowercase()
        .trim_end_matches(".exe")
        .to_owned()
}

pub(crate) fn is_pane_shell_process_name(name: &str) -> bool {
    matches!(
        normalized_shell_name(name).as_str(),
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "mksh" | "fish"
    )
}

pub(crate) fn interactive_shell_command(argv: &[String], shell_name: &str) -> Option<String> {
    if argv.first()?.is_empty()
        || argv.iter().any(|arg| arg.chars().any(char::is_control))
        || !is_pane_shell_process_name(shell_name)
    {
        return None;
    }
    let fish = normalized_shell_name(shell_name) == "fish";
    Some(
        argv.iter()
            .map(|arg| {
                if !arg.is_empty()
                    && arg.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
                    })
                {
                    return arg.clone();
                }
                let quoted = if fish {
                    arg.replace('\\', "\\\\").replace('\'', "\\'")
                } else {
                    arg.replace('\'', "'\\''")
                };
                format!("'{quoted}'")
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Credentials of the process on the other end of a Unix-socket connection
/// (ADR 0014). The kernel fills the pid and uid in at connect time, so a client
/// cannot forge them; they are never taken from a request field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    /// The start time of the process at `pid`, read the moment the connection
    /// was accepted. `None` when it could not be read, which means the peer was
    /// already gone — a refusal, never a pass.
    ///
    /// A pid on its own is not an identity: the kernel reuses pids, so between
    /// the accept and the check the pid could belong to someone else. The start
    /// time captured here is what makes the peer a principal
    /// (ARCH-E8-ADR14-PID-REUSE-001).
    pub start_time: Option<u64>,
}

/// A process identified the way ADR 0014 requires: a pid together with the start
/// time the kernel stamped on it.
///
/// A bare pid is not an identity. Pids are reused, so a pid whose process was
/// reaped can be handed to an unrelated process, and anything that trusted the
/// pid alone would hand that process the dead one's standing
/// (ARCH-E8-ADR14-PID-REUSE-001).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessPrincipal {
    pub pid: u32,
    pub start_time: u64,
}

/// One PID/start-time-pinned ancestry observation. Linux builds bracket the
/// argv read with two stat reads and return `None` if the process changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessInspection {
    pub(crate) principal: ProcessPrincipal,
    pub(crate) parent_pid: u32,
    pub(crate) argv: Vec<String>,
}

/// Where a caller sits relative to a pane's process tree (ADR 0014).
///
/// Every variant but `Inside` is a refusal; they are kept apart so the F4
/// message can say WHY, which is the difference between "you are in the wrong
/// pane" and "the pane you named is gone".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreePlacement {
    /// The caller is the pane's root process or a descendant of it, and both
    /// principals still hold the start times they were captured with.
    Inside,
    /// The pid the caller connected with is gone, or now belongs to a different
    /// process: nothing found at that pid can be attributed to the connection.
    CallerReplaced,
    /// The pane's root process is gone from `/proc`. No caller can be inside a
    /// tree whose root no longer exists.
    PaneRootGone,
    /// The pane's root pid is alive but holds a different process than the one
    /// the pane started: the pid was reused.
    PaneRootReplaced,
    /// A real, live process, outside this pane's tree.
    Outside,
}

/// The longest ancestry chain `process_is_descendant_of` walks before giving up.
/// A pane's own tree is a handful of hops (shell -> agent -> hook -> python), so
/// the bound only exists so a `/proc` cycle or a pathological tree cannot spin.
pub(crate) const MAX_ANCESTRY_HOPS: usize = 64;

/// Place `caller` relative to the tree rooted at `pane_root`, following
/// `ancestry_of` upward from the caller.
///
/// Both ends are PRINCIPALS, not pids (ADR 0014, ARCH-E8-ADR14-PID-REUSE-001).
/// The pane's root must still be the process the pane started, and the caller's
/// pid must still hold the process that connected; a pid whose start time no
/// longer matches has changed hands and is refused, as is a pane root that has
/// been reaped. Only then is the ancestry walked.
///
/// The walk stops at PID 1 (init), at an unreadable process, at a self-parent,
/// and after `MAX_ANCESTRY_HOPS`; every stop is a refusal, so it fails closed.
/// Each hop's parent link is maintained by the kernel — a process whose parent
/// exits is reparented, so a `ppid` never points at a reaped pid — which is what
/// makes the pinned endpoints enough. The lookup is injected rather than called
/// directly so the rule can be unit-tested against a synthetic process table.
pub(crate) fn place_process_in_tree(
    caller: ProcessPrincipal,
    pane_root: ProcessPrincipal,
    mut ancestry_of: impl FnMut(u32) -> Option<(u32, u64)>,
) -> TreePlacement {
    if caller.pid == 0 || pane_root.pid == 0 {
        return TreePlacement::Outside;
    }

    // The pane's root has to still BE the process the pane started. Without this
    // an unrelated process that inherited a reaped pane root's pid would carry
    // the pane's whole subtree of trust with it.
    match ancestry_of(pane_root.pid) {
        None => return TreePlacement::PaneRootGone,
        Some((_, start_time)) if start_time != pane_root.start_time => {
            return TreePlacement::PaneRootReplaced
        }
        Some(_) => {}
    }

    // And the pid the kernel reported at accept has to still hold that peer.
    match ancestry_of(caller.pid) {
        None => return TreePlacement::CallerReplaced,
        Some((_, start_time)) if start_time != caller.start_time => {
            return TreePlacement::CallerReplaced
        }
        Some(_) => {}
    }

    let mut current = caller.pid;
    for _ in 0..MAX_ANCESTRY_HOPS {
        if current == pane_root.pid {
            return TreePlacement::Inside;
        }
        // PID 1 has no parent inside any pane: a process reparented to init has
        // left the tree it was spawned in and can no longer be placed.
        if current <= 1 {
            return TreePlacement::Outside;
        }
        match ancestry_of(current) {
            Some((parent, _)) if parent != current => current = parent,
            _ => return TreePlacement::Outside,
        }
    }
    TreePlacement::Outside
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hangup,
    Terminate,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformCapabilities {
    pub(crate) live_handoff: bool,
    pub(crate) remote_attach: bool,
    pub(crate) direct_terminal_attach: bool,
    pub(crate) preserve_legacy_doubled_escape_input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteSshConfigPaths {
    pub(crate) user_config: Option<std::path::PathBuf>,
    pub(crate) system_config: Option<std::path::PathBuf>,
}

pub(crate) const fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        live_handoff: true,
        remote_attach: true,
        direct_terminal_attach: true,
        preserve_legacy_doubled_escape_input: false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LimitedRead {
    Empty,
    Complete(Vec<u8>),
    Oversized,
}

pub(crate) fn read_limited_reader(
    mut reader: impl std::io::Read,
    max_bytes: usize,
) -> std::io::Result<LimitedRead> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];

    while bytes.len() < max_bytes {
        let remaining = max_bytes - bytes.len();
        let read_len = remaining.min(buffer.len());
        let bytes_read = match reader.read(&mut buffer[..read_len]) {
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if bytes_read == 0 {
            return if bytes.is_empty() {
                Ok(LimitedRead::Empty)
            } else {
                Ok(LimitedRead::Complete(bytes))
            };
        }
        bytes.extend_from_slice(&buffer[..bytes_read]);
    }

    let mut sentinel = [0_u8; 1];
    loop {
        return match reader.read(&mut sentinel) {
            Ok(0) if bytes.is_empty() => Ok(LimitedRead::Empty),
            Ok(0) => Ok(LimitedRead::Complete(bytes)),
            Ok(_) => Ok(LimitedRead::Oversized),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => Err(err),
        };
    }
}

#[derive(Debug)]
pub(crate) enum BoundedChildWait {
    Exited(std::process::ExitStatus),
    TimedOut(std::process::ExitStatus),
    Cancelled(std::process::ExitStatus),
}

#[derive(Debug)]
pub(crate) struct BoundedChildOutput {
    pub(crate) stdout: LimitedRead,
    pub(crate) stderr: LimitedRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundedChildOutputError {
    Wait,
    Timeout,
    Output,
    Exit,
    Validation,
}

pub(crate) struct BoundedChildOutputOptions {
    pub deadline: std::time::Instant,
    pub cleanup_grace: std::time::Duration,
    pub stdout_max_bytes: usize,
    pub stderr_max_bytes: usize,
}

pub(crate) struct BoundedChildWaitOptions<'a> {
    pub timeout: Option<std::time::Duration>,
    pub terminate_process_group: bool,
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

pub(crate) fn wait_for_bounded_child(
    child: &mut std::process::Child,
    options: BoundedChildWaitOptions<'_>,
) -> std::io::Result<BoundedChildWait> {
    if options.timeout.is_none() && options.cancel.is_none() {
        return child.wait().map(BoundedChildWait::Exited);
    }

    let deadline = options
        .timeout
        .map(|timeout| std::time::Instant::now() + timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(BoundedChildWait::Exited(status)),
            Ok(None) => {}
            Err(wait_error) => {
                let _ = terminate_and_reap_child(child, options.terminate_process_group);
                return Err(wait_error);
            }
        }

        let cancelled = options
            .cancel
            .is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Acquire));
        let timed_out = deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if cancelled || timed_out {
            let status = terminate_and_reap_child(child, options.terminate_process_group)?;
            return Ok(if timed_out {
                BoundedChildWait::TimedOut(status)
            } else {
                BoundedChildWait::Cancelled(status)
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn terminate_and_reap_child(
    child: &mut std::process::Child,
    terminate_process_group: bool,
) -> std::io::Result<std::process::ExitStatus> {
    terminate_and_reap_child_until(child, terminate_process_group, None)
}

fn terminate_and_reap_child_until(
    child: &mut std::process::Child,
    terminate_process_group: bool,
    deadline: Option<std::time::Instant>,
) -> std::io::Result<std::process::ExitStatus> {
    let mut termination_error = terminate_process_group
        .then(|| kill_process_group(child.id()).err())
        .flatten();
    if let Err(err) = child.kill() {
        if err.kind() != std::io::ErrorKind::InvalidInput {
            termination_error.get_or_insert(err);
        }
    }
    let status = if let Some(deadline) = deadline {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "child did not exit within cleanup grace",
                        ));
                    }
                    continue;
                }
                Err(err) => return Err(err),
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "child did not exit within cleanup grace",
                ));
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(now)
                    .min(std::time::Duration::from_millis(2)),
            );
        }
    } else {
        loop {
            match child.wait() {
                Ok(status) => break status,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    };
    if let Some(err) = termination_error {
        return Err(err);
    }
    Ok(status)
}

mod linux;
pub use linux::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_resize_signal_is_recorded_once_per_delivery() {
        watch_terminal_resize_signal();
        assert!(!take_terminal_resize_signal());

        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());
    }

    #[test]
    fn read_limited_reader_returns_complete_data_under_limit() {
        let input = std::io::Cursor::new(b"image".to_vec());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_returns_empty_for_empty_input() {
        let input = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Empty
        );
    }

    #[test]
    fn read_limited_reader_accepts_data_exactly_at_limit() {
        let input = std::io::Cursor::new(b"four".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Complete(b"four".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_rejects_data_over_limit() {
        let input = std::io::Cursor::new(b"oversized".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Oversized
        );
    }

    #[test]
    fn read_limited_reader_retries_interrupted_reads() {
        struct InterruptedOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.inner.read(buffer)
            }
        }

        let input = InterruptedOnce {
            interrupted: false,
            inner: std::io::Cursor::new(b"image".to_vec()),
        };
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn bounded_child_cancellation_kills_and_reaps_the_process_group() {
        use std::os::unix::process::CommandExt as _;
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 5 & wait"]).process_group(0);
        let mut child = command.spawn().unwrap();
        let process_group_id = child.id();
        let cancel = AtomicBool::new(true);
        let started = std::time::Instant::now();
        let outcome = wait_for_bounded_child(
            &mut child,
            BoundedChildWaitOptions {
                timeout: Some(std::time::Duration::from_secs(5)),
                terminate_process_group: true,
                cancel: Some(&cancel),
            },
        )
        .unwrap();

        assert!(matches!(outcome, BoundedChildWait::Cancelled(_)));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(unsafe { libc::kill(-(process_group_id as i32), 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(cancel.load(Ordering::Acquire));
    }
}
