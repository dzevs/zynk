//! ADR 0014 — the pane-tree binding for identity reports and receipts.
//!
//! The API socket is `0o600` and owned by the user, so the server already knew
//! every caller was the same UID. What it did not know is WHICH process called,
//! and that is what let a passive process in one pane report an agent identity
//! for another pane and then receipt its messages.
//!
//! The principal is the target pane's process tree: a caller is accepted only
//! when the peer PID the kernel reported for its connection is the pane's PTY
//! child, or a descendant of it. Every other shape — no peer credentials, a
//! foreign UID, a pane with no live process, a caller elsewhere in the process
//! table, a hook that reparented itself to init — is refused fail-closed.

use crate::api::schema::Method;
use crate::api::ApiCaller;
use crate::app::App;

/// The F4 error code every refusal reports.
pub(crate) const CALLER_OUTSIDE_PANE: &str = "caller_outside_pane";

/// Why a caller was refused. Each becomes the same F4 code with a message that
/// names the peer PID and the pane, so a hook author can tell the shapes apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallerRejection {
    /// The connection reported no peer credentials, or the request never came
    /// over the API socket at all.
    UnknownPeer,
    /// The connected process runs as another user.
    ForeignUid { peer_pid: u32, uid: u32 },
    /// The pane exists but has no PTY child to be inside.
    PaneHasNoProcess { peer_pid: u32 },
    /// A real process, outside this pane's tree.
    OutsidePane { peer_pid: u32 },
}

impl CallerRejection {
    pub(crate) fn message(self, pane_id: &str) -> String {
        match self {
            Self::UnknownPeer => format!(
                "caller could not be identified: the connection reported no peer credentials, \
                 so it cannot be placed inside pane {pane_id} (ADR 0014)"
            ),
            Self::ForeignUid { peer_pid, uid } => format!(
                "caller pid {peer_pid} runs as uid {uid}, not as this server's user, \
                 so it cannot report for pane {pane_id} (ADR 0014)"
            ),
            Self::PaneHasNoProcess { peer_pid } => format!(
                "pane {pane_id} has no running process, so caller pid {peer_pid} \
                 cannot be inside it (ADR 0014)"
            ),
            Self::OutsidePane { peer_pid } => format!(
                "caller pid {peer_pid} is not inside pane {pane_id}'s process tree; \
                 identity reports and receipts are accepted only from the target pane \
                 (ADR 0014)"
            ),
        }
    }
}

/// The pane a method's caller must be inside (ADR 0014 Decision 1), or `None`
/// for every method that is not pane-bound.
///
/// `pane.release_agent`, `pane.clear_agent_authority` and `pane.report_metadata`
/// are deliberately absent: the first two only RETIRE an identity, so refusing
/// them from outside would keep a dead session alive — the wrong direction to
/// fail — and operator tooling releases panes from outside them by design;
/// `pane.report_metadata` writes presentation only and never touches
/// `hook_authority`, `hook_identity` or the persisted session.
pub(crate) fn pane_bound_target(method: &Method) -> Option<(&'static str, &str)> {
    match method {
        Method::PaneReportAgent(params) => Some(("pane.report_agent", params.pane_id.as_str())),
        Method::PaneReportAgentSession(params) => {
            Some(("pane.report_agent_session", params.pane_id.as_str()))
        }
        Method::ZynkMessageReceived(params) => {
            Some(("zynk.message_received", params.pane_id.as_str()))
        }
        _ => None,
    }
}

impl App {
    /// `Ok(())` when `caller` is a process inside `pane_id`'s tree (ADR 0014).
    ///
    /// A pane id that does not resolve returns `Ok(())` on purpose: the handler
    /// answers it with its own `pane_not_found` / `receiver_identity_unverified`,
    /// which is both more accurate and no weaker — nothing is granted either way.
    pub(crate) fn caller_is_inside_pane(
        &self,
        pane_id: &str,
        caller: ApiCaller,
    ) -> Result<(), CallerRejection> {
        let Some((ws_idx, resolved)) = self.parse_pane_id(pane_id) else {
            return Ok(());
        };

        #[cfg(debug_assertions)]
        if caller.trusted_as_pane_child {
            return Ok(());
        }

        let Some(peer) = caller.peer else {
            return Err(CallerRejection::UnknownPeer);
        };
        if peer.uid != crate::platform::current_uid() {
            return Err(CallerRejection::ForeignUid {
                peer_pid: peer.pid,
                uid: peer.uid,
            });
        }

        let child_pid = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, resolved)
            .and_then(|runtime| runtime.child_pid());
        let Some(child_pid) = child_pid else {
            return Err(CallerRejection::PaneHasNoProcess { peer_pid: peer.pid });
        };

        if crate::platform::process_is_descendant_of(
            peer.pid,
            child_pid,
            crate::platform::parent_pid,
        ) {
            Ok(())
        } else {
            Err(CallerRejection::OutsidePane { peer_pid: peer.pid })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::process_is_descendant_of;
    use std::collections::HashMap;

    /// A synthetic process table: child -> parent.
    fn table(pairs: &[(u32, u32)]) -> impl Fn(u32) -> Option<u32> + '_ {
        let map: HashMap<u32, u32> = pairs.iter().copied().collect();
        move |pid| map.get(&pid).copied()
    }

    #[test]
    fn a_process_is_inside_the_pane_it_descends_from() {
        // shell(100) -> agent(200) -> hook(300) -> python(400): the whole chain
        // an integration actually spawns is inside the pane.
        let procs = table(&[(400, 300), (300, 200), (200, 100), (100, 10), (10, 1)]);
        assert!(
            process_is_descendant_of(100, 100, &procs),
            "the shell itself"
        );
        assert!(process_is_descendant_of(200, 100, &procs), "the agent");
        assert!(
            process_is_descendant_of(400, 100, &procs),
            "the hook's python"
        );
    }

    #[test]
    fn a_process_in_another_pane_is_outside() {
        // Two panes under one server(10); neither pane's tree reaches the other.
        let procs = table(&[(201, 101), (202, 102), (101, 10), (102, 10), (10, 1)]);
        assert!(!process_is_descendant_of(202, 101, &procs));
        assert!(!process_is_descendant_of(201, 102, &procs));
        assert!(
            !process_is_descendant_of(10, 101, &procs),
            "the server itself"
        );
    }

    #[test]
    fn a_reparented_process_is_outside() {
        // A double-forked hook whose parent became PID 1 has left its pane and
        // can no longer be placed: the walk hits init and refuses.
        let procs = table(&[(500, 1), (100, 10), (10, 1)]);
        assert!(!process_is_descendant_of(500, 100, &procs));
    }

    #[test]
    fn an_unreadable_or_cyclic_ancestry_refuses() {
        // A process whose parent cannot be read (it exited mid-walk) refuses,
        // and a self-parent cannot loop the walk.
        let unreadable = table(&[(300, 200)]);
        assert!(!process_is_descendant_of(300, 100, &unreadable));
        let cyclic = table(&[(300, 300)]);
        assert!(!process_is_descendant_of(300, 100, &cyclic));
    }

    #[test]
    fn a_deeper_chain_than_the_hop_bound_refuses() {
        // The bound is a refusal, not a truncation: a chain longer than
        // MAX_ANCESTRY_HOPS fails closed rather than being accepted early.
        let hops = crate::platform::MAX_ANCESTRY_HOPS as u32;
        let chain: Vec<(u32, u32)> = (1..=hops + 4).map(|pid| (pid + 1, pid)).collect();
        let procs = table(&chain);
        assert!(
            process_is_descendant_of(10, 1, &procs),
            "a short chain resolves"
        );
        assert!(!process_is_descendant_of(hops + 5, 1, &procs));
    }

    #[test]
    fn pid_zero_is_never_inside_anything() {
        let procs = table(&[(100, 10)]);
        assert!(!process_is_descendant_of(0, 100, &procs));
        assert!(!process_is_descendant_of(100, 0, &procs));
    }

    #[test]
    fn the_debug_peer_trust_seam_cannot_exist_in_a_release_build() {
        // ADR 0014: the seam that lets a connection be treated as the target pane's
        // own child is compiled ONLY under `#[cfg(debug_assertions)]`, so its env-var
        // name is not even a string in a release binary. This pins that gating in the
        // source; the release-binary string audit asserts the absence in the artifact.
        // Assembled at runtime so this file is not itself a source-tree mention of
        // the name it is counting.
        let seam = format!("ZYNK_TEST_{}", "TRUST_PEER_PID");
        let seam = seam.as_str();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        fn rust_sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("read src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    rust_sources(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    found.push(path);
                }
            }
        }

        let mut sources = Vec::new();
        rust_sources(&root, &mut sources);
        let mut mentions: Vec<String> = sources
            .iter()
            .filter(|path| {
                std::fs::read_to_string(path)
                    .expect("read source")
                    .contains(seam)
            })
            .map(|path| {
                path.strip_prefix(&root)
                    .expect("under src")
                    .display()
                    .to_string()
            })
            .collect();
        mentions.sort();
        assert_eq!(
            mentions,
            vec!["api/mod.rs".to_string()],
            "the seam's env-var name must exist in exactly one source file"
        );

        let api_mod = std::fs::read_to_string(root.join("api/mod.rs")).expect("read api/mod.rs");
        assert_eq!(
            api_mod.matches(seam).count(),
            1,
            "the seam name must appear once, as the constant's value"
        );
        for gated in [
            "#[cfg(debug_assertions)]\npub const TEST_TRUST_PEER_PID_ENV",
            "#[cfg(debug_assertions)]\nfn accept_trusts_pane_child()",
            "#[cfg(debug_assertions)]\n    pub trusted_as_pane_child: bool,",
            "#[cfg(debug_assertions)]\n            trusted_as_pane_child: accept_trusts_pane_child(),",
        ] {
            assert!(
                api_mod.contains(gated),
                "the seam is no longer compile-gated at: {gated:?}"
            );
        }
    }

    #[test]
    fn only_the_three_identity_methods_are_pane_bound() {
        use crate::api::schema::{
            PaneAgentState, PaneReportAgentParams, PaneReportAgentSessionParams, PaneTarget,
            ZynkMessageReceivedParams,
        };

        assert_eq!(
            pane_bound_target(&Method::PaneReportAgent(PaneReportAgentParams {
                pane_id: "w-1".into(),
                source: "zynk:pi".into(),
                agent: "pi".into(),
                state: PaneAgentState::Idle,
                message: None,
                custom_status: None,
                seq: None,
                agent_session_id: None,
                agent_session_path: None,
            })),
            Some(("pane.report_agent", "w-1"))
        );
        assert_eq!(
            pane_bound_target(&Method::PaneReportAgentSession(
                PaneReportAgentSessionParams {
                    pane_id: "w-2".into(),
                    source: "zynk:pi".into(),
                    agent: "pi".into(),
                    seq: None,
                    agent_session_id: None,
                    agent_session_path: None,
                    session_start_source: None,
                }
            )),
            Some(("pane.report_agent_session", "w-2"))
        );
        assert_eq!(
            pane_bound_target(&Method::ZynkMessageReceived(ZynkMessageReceivedParams {
                pane_id: "w-3".into(),
                message_id: "m".into(),
                conversation_id: "c".into(),
                conversation_seq: 1,
                runtime_session_id: "rt".into(),
                socket_namespace: "sock".into(),
                receiver_seq: None,
                timestamp: None,
                status: None,
                receiver_agent_session: None,
            })),
            Some(("zynk.message_received", "w-3"))
        );
        // Read paths and the retiring/presentation methods stay unbound.
        assert_eq!(
            pane_bound_target(&Method::PaneGet(PaneTarget {
                pane_id: "w-1".into()
            })),
            None
        );
    }
}
