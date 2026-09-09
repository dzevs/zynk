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
    ancestry_of: impl Fn(u32) -> Option<(u32, u64)>,
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
}

pub(crate) const fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        live_handoff: true,
        remote_attach: true,
        direct_terminal_attach: true,
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

mod linux;
pub use linux::*;

#[cfg(test)]
mod tests {
    use super::*;

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
}
