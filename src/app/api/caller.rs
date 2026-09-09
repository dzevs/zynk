//! ADR 0014 — the pane-tree binding for identity reports and receipts.
//!
//! The API socket is `0o600` and owned by the user, so the server already knew
//! every caller was the same UID. What it did not know is WHICH process called,
//! and that is what let a passive process in one pane report an agent identity
//! for another pane and then receipt its messages.
//!
//! The principal is the target pane's process tree: a caller is accepted only
//! when the peer the kernel reported for its connection is the pane's PTY child,
//! or a descendant of it. Every other shape — no peer credentials, a foreign
//! UID, a pane with no live process, a caller elsewhere in the process table, a
//! hook that reparented itself to init — is refused fail-closed.
//!
//! Both ends of that comparison are `(pid, start time)` pairs, never bare pids.
//! Pids are reused: a pane root that has been reaped leaves its number behind for
//! the kernel to hand to an unrelated process, and a check that matched the
//! number alone would give that process the dead pane's whole subtree of trust
//! (ARCH-E8-ADR14-PID-REUSE-001). The start times pin both endpoints, and a pane
//! root that is gone, replaced, or was never identified fails closed.

use crate::api::schema::Method;
use crate::api::ApiCaller;
use crate::app::App;
use crate::platform::{PeerCredentials, ProcessPrincipal, TreePlacement};

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
    /// The peer's start time could not be read when the connection was accepted,
    /// so the connection was never bound to a process — only to a pid.
    CallerUnidentified { peer_pid: u32 },
    /// The pid the peer connected with is gone, or now holds a different
    /// process: the connection can no longer be attributed to anyone.
    CallerReplaced { peer_pid: u32 },
    /// The pane exists but has no PTY child to be inside.
    PaneHasNoProcess { peer_pid: u32 },
    /// The pane's PTY child pid was published without a start time, so the pane
    /// root is a bare pid and cannot be a principal.
    PaneRootUnidentified { peer_pid: u32, root_pid: u32 },
    /// The pane's root process has been reaped. Nothing can be inside a tree
    /// whose root no longer exists.
    PaneRootGone { peer_pid: u32, root_pid: u32 },
    /// The pane's root pid is alive but holds a different process than the one
    /// the pane started — the pid was reused
    /// (ARCH-E8-ADR14-PID-REUSE-001).
    PaneRootReplaced { peer_pid: u32, root_pid: u32 },
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
            Self::CallerUnidentified { peer_pid } => format!(
                "caller pid {peer_pid} had already exited when its connection was accepted, \
                 so no process was bound to it and it cannot be placed inside pane {pane_id} \
                 (ADR 0014)"
            ),
            Self::CallerReplaced { peer_pid } => format!(
                "caller pid {peer_pid} no longer holds the process that opened this connection, \
                 so it cannot be placed inside pane {pane_id}: a pid is reused, and only the \
                 process that connected may report (ADR 0014)"
            ),
            Self::PaneHasNoProcess { peer_pid } => format!(
                "pane {pane_id} has no running process, so caller pid {peer_pid} \
                 cannot be inside it (ADR 0014)"
            ),
            Self::PaneRootUnidentified { peer_pid, root_pid } => format!(
                "pane {pane_id}'s process {root_pid} has no recorded start time, so it is a \
                 bare pid rather than an identity and caller pid {peer_pid} cannot be placed \
                 inside it (ADR 0014)"
            ),
            Self::PaneRootGone { peer_pid, root_pid } => format!(
                "pane {pane_id}'s process {root_pid} has exited, so caller pid {peer_pid} \
                 cannot be inside its process tree (ADR 0014)"
            ),
            Self::PaneRootReplaced { peer_pid, root_pid } => format!(
                "pane {pane_id}'s process {root_pid} has exited and that pid now belongs to \
                 another process, so caller pid {peer_pid} cannot be inside the pane's tree; \
                 a reused pid does not inherit the pane it once rooted (ADR 0014)"
            ),
            Self::OutsidePane { peer_pid } => format!(
                "caller pid {peer_pid} is not inside pane {pane_id}'s process tree; \
                 identity reports and receipts are accepted only from the target pane \
                 (ADR 0014)"
            ),
        }
    }
}

/// The whole ADR 0014 rule, with every `/proc` read injected.
///
/// `pane_root` is what the pane published: its PTY child pid, and the start time
/// captured when that pid was published — `None` for a pid published without
/// one, which is a refusal rather than a licence to match the pid alone.
fn place_caller_in_pane_tree(
    peer: PeerCredentials,
    server_uid: u32,
    pane_root: Option<(u32, Option<u64>)>,
    ancestry_of: impl Fn(u32) -> Option<(u32, u64)>,
) -> Result<(), CallerRejection> {
    let peer_pid = peer.pid;
    if peer.uid != server_uid {
        return Err(CallerRejection::ForeignUid {
            peer_pid,
            uid: peer.uid,
        });
    }
    let Some(peer_start_time) = peer.start_time else {
        return Err(CallerRejection::CallerUnidentified { peer_pid });
    };
    let Some((root_pid, root_start_time)) = pane_root else {
        return Err(CallerRejection::PaneHasNoProcess { peer_pid });
    };
    let Some(root_start_time) = root_start_time else {
        return Err(CallerRejection::PaneRootUnidentified { peer_pid, root_pid });
    };

    match crate::platform::place_process_in_tree(
        ProcessPrincipal {
            pid: peer_pid,
            start_time: peer_start_time,
        },
        ProcessPrincipal {
            pid: root_pid,
            start_time: root_start_time,
        },
        ancestry_of,
    ) {
        TreePlacement::Inside => Ok(()),
        TreePlacement::CallerReplaced => Err(CallerRejection::CallerReplaced { peer_pid }),
        TreePlacement::PaneRootGone => Err(CallerRejection::PaneRootGone { peer_pid, root_pid }),
        TreePlacement::PaneRootReplaced => {
            Err(CallerRejection::PaneRootReplaced { peer_pid, root_pid })
        }
        TreePlacement::Outside => Err(CallerRejection::OutsidePane { peer_pid }),
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

        // Load the pid first: the runtime publishes the start time before it, so
        // reading in this order always pairs a pid with its own start time.
        let pane_root = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, resolved)
            .and_then(|runtime| {
                runtime
                    .child_pid()
                    .map(|pid| (pid, runtime.child_start_time()))
            });

        place_caller_in_pane_tree(
            peer,
            crate::platform::current_uid(),
            pane_root,
            crate::platform::process_parent_and_start_time,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::place_process_in_tree;
    use std::collections::HashMap;

    /// A synthetic process table: pid -> (parent, start time).
    fn table(rows: &[(u32, u32, u64)]) -> impl Fn(u32) -> Option<(u32, u64)> + '_ {
        let map: HashMap<u32, (u32, u64)> = rows
            .iter()
            .map(|&(pid, parent, start_time)| (pid, (parent, start_time)))
            .collect();
        move |pid| map.get(&pid).copied()
    }

    fn start_time_in(rows: &[(u32, u32, u64)], pid: u32) -> u64 {
        rows.iter().find(|row| row.0 == pid).map_or(0, |row| row.2)
    }

    /// Place a caller using the start times the table itself reports for both
    /// ends — the honest case, in which nothing has changed hands.
    fn place(caller_pid: u32, root_pid: u32, rows: &[(u32, u32, u64)]) -> TreePlacement {
        place_process_in_tree(
            ProcessPrincipal {
                pid: caller_pid,
                start_time: start_time_in(rows, caller_pid),
            },
            ProcessPrincipal {
                pid: root_pid,
                start_time: start_time_in(rows, root_pid),
            },
            table(rows),
        )
    }

    fn peer(pid: u32) -> PeerCredentials {
        PeerCredentials {
            pid,
            uid: 1000,
            start_time: Some(900),
        }
    }

    #[test]
    fn a_process_is_inside_the_pane_it_descends_from() {
        // shell(100) -> agent(200) -> hook(300) -> python(400): the whole chain
        // an integration actually spawns is inside the pane.
        let procs = [
            (400, 300, 40),
            (300, 200, 30),
            (200, 100, 20),
            (100, 10, 10),
            (10, 1, 5),
        ];
        assert_eq!(place(100, 100, &procs), TreePlacement::Inside, "the shell");
        assert_eq!(place(200, 100, &procs), TreePlacement::Inside, "the agent");
        assert_eq!(
            place(400, 100, &procs),
            TreePlacement::Inside,
            "the hook's python"
        );
    }

    #[test]
    fn a_process_in_another_pane_is_outside() {
        // Two panes under one server(10); neither pane's tree reaches the other.
        let procs = [
            (201, 101, 21),
            (202, 102, 22),
            (101, 10, 11),
            (102, 10, 12),
            (10, 1, 5),
        ];
        assert_eq!(place(202, 101, &procs), TreePlacement::Outside);
        assert_eq!(place(201, 102, &procs), TreePlacement::Outside);
        assert_eq!(
            place(10, 101, &procs),
            TreePlacement::Outside,
            "the server itself"
        );
    }

    #[test]
    fn a_reparented_process_is_outside() {
        // A double-forked hook whose parent became PID 1 has left its pane and
        // can no longer be placed: the walk hits init and refuses.
        let procs = [(500, 1, 50), (100, 10, 10), (10, 1, 5)];
        assert_eq!(place(500, 100, &procs), TreePlacement::Outside);
    }

    #[test]
    fn an_unreadable_or_cyclic_ancestry_refuses() {
        // A process whose parent cannot be read (it exited mid-walk) refuses,
        // and a self-parent cannot loop the walk.
        let unreadable = [(300, 200, 30), (100, 10, 10), (10, 1, 5)];
        assert_eq!(place(300, 100, &unreadable), TreePlacement::Outside);
        let cyclic = [(300, 300, 30), (100, 10, 10)];
        assert_eq!(place(300, 100, &cyclic), TreePlacement::Outside);
    }

    #[test]
    fn a_deeper_chain_than_the_hop_bound_refuses() {
        // The bound is a refusal, not a truncation: a chain longer than
        // MAX_ANCESTRY_HOPS fails closed rather than being accepted early.
        let hops = crate::platform::MAX_ANCESTRY_HOPS as u32;
        let mut procs: Vec<(u32, u32, u64)> = vec![(1000, 10, 1000), (10, 1, 5)];
        procs.extend((1..=hops + 4).map(|n| (1000 + n, 999 + n, 1000 + u64::from(n))));
        assert_eq!(
            place(1003, 1000, &procs),
            TreePlacement::Inside,
            "a short chain resolves"
        );
        assert_eq!(place(1000 + hops + 4, 1000, &procs), TreePlacement::Outside);
    }

    #[test]
    fn pid_zero_is_never_inside_anything() {
        let procs = [(100, 10, 10), (10, 1, 5)];
        assert_eq!(place(0, 100, &procs), TreePlacement::Outside);
        assert_eq!(place(100, 0, &procs), TreePlacement::Outside);
    }

    #[test]
    fn a_reused_pane_root_pid_is_not_the_pane_it_once_rooted() {
        // ARCH-E8-ADR14-PID-REUSE-001, in the reviewer's own numbers. The pane's
        // root was pid 4242, started at tick 900. It exited and was reaped, and
        // the kernel handed 4242 to an unrelated process started at tick 5000,
        // which forked 4243. Ancestry alone reaches the pane root's PID, so a
        // pid-only principal called 4243 "inside the pane" and let it report an
        // identity for it and receipt its messages.
        let procs = [(4243, 4242, 5001), (4242, 1000, 5000), (1000, 1, 10)];
        assert_eq!(
            place_process_in_tree(
                ProcessPrincipal {
                    pid: 4243,
                    start_time: 5001,
                },
                ProcessPrincipal {
                    pid: 4242,
                    start_time: 900,
                },
                table(&procs),
            ),
            TreePlacement::PaneRootReplaced,
            "a reused pane-root pid must not be inheritable"
        );

        // The same shape with the start time the pane actually recorded is the
        // ordinary in-pane caller, and stays accepted.
        assert_eq!(place(4243, 4242, &procs), TreePlacement::Inside);
    }

    #[test]
    fn a_reaped_pane_root_fails_closed() {
        // Nothing can be inside a tree whose root no longer exists, and the
        // refusal does not depend on where the orphan's own ancestry now leads.
        let procs = [(4243, 1, 5001)];
        assert_eq!(
            place_process_in_tree(
                ProcessPrincipal {
                    pid: 4243,
                    start_time: 5001,
                },
                ProcessPrincipal {
                    pid: 4242,
                    start_time: 900,
                },
                table(&procs),
            ),
            TreePlacement::PaneRootGone
        );
    }

    #[test]
    fn a_caller_pid_that_changed_hands_is_refused() {
        // The other reuse window: between `connect()` and this check the peer
        // exited and its pid was handed to another process — which may well be
        // inside the pane. The connection was never that process's, so the pid
        // the kernel reported at accept no longer speaks for anyone.
        let procs = [(200, 100, 5000), (100, 10, 10), (10, 1, 5)];
        assert_eq!(
            place_process_in_tree(
                ProcessPrincipal {
                    pid: 200,
                    start_time: 20,
                },
                ProcessPrincipal {
                    pid: 100,
                    start_time: 10,
                },
                table(&procs),
            ),
            TreePlacement::CallerReplaced
        );
    }

    #[test]
    fn a_caller_that_has_exited_is_refused() {
        let procs = [(100, 10, 10), (10, 1, 5)];
        assert_eq!(
            place_process_in_tree(
                ProcessPrincipal {
                    pid: 200,
                    start_time: 20,
                },
                ProcessPrincipal {
                    pid: 100,
                    start_time: 10,
                },
                table(&procs),
            ),
            TreePlacement::CallerReplaced
        );
    }

    #[test]
    fn the_decision_names_every_fail_closed_shape() {
        // The App-level rule, with both `/proc` reads injected. Each refusal is
        // its own value so the F4 message can say which one happened.
        let procs = [(200, 100, 20), (100, 10, 10), (10, 1, 5)];
        let ancestry = table(&procs);
        let inside = PeerCredentials {
            pid: 200,
            uid: 1000,
            start_time: Some(20),
        };

        assert_eq!(
            place_caller_in_pane_tree(inside, 1000, Some((100, Some(10))), &ancestry),
            Ok(())
        );
        assert_eq!(
            place_caller_in_pane_tree(
                PeerCredentials {
                    uid: 1001,
                    ..inside
                },
                1000,
                Some((100, Some(10))),
                &ancestry,
            ),
            Err(CallerRejection::ForeignUid {
                peer_pid: 200,
                uid: 1001
            })
        );
        assert_eq!(
            place_caller_in_pane_tree(
                PeerCredentials {
                    start_time: None,
                    ..inside
                },
                1000,
                Some((100, Some(10))),
                &ancestry,
            ),
            Err(CallerRejection::CallerUnidentified { peer_pid: 200 }),
            "a peer whose start time was never captured is not a principal"
        );
        assert_eq!(
            place_caller_in_pane_tree(inside, 1000, None, &ancestry),
            Err(CallerRejection::PaneHasNoProcess { peer_pid: 200 })
        );
        assert_eq!(
            place_caller_in_pane_tree(inside, 1000, Some((100, None)), &ancestry),
            Err(CallerRejection::PaneRootUnidentified {
                peer_pid: 200,
                root_pid: 100
            }),
            "a pane root published without a start time must not match on the pid alone"
        );
        assert_eq!(
            place_caller_in_pane_tree(inside, 1000, Some((100, Some(999))), &ancestry),
            Err(CallerRejection::PaneRootReplaced {
                peer_pid: 200,
                root_pid: 100
            })
        );
        assert_eq!(
            place_caller_in_pane_tree(inside, 1000, Some((404, Some(40))), &ancestry),
            Err(CallerRejection::PaneRootGone {
                peer_pid: 200,
                root_pid: 404
            })
        );
        assert_eq!(
            place_caller_in_pane_tree(peer(10), 1000, Some((100, Some(10))), &ancestry),
            Err(CallerRejection::CallerReplaced { peer_pid: 10 }),
            "the helper's peer start time is not the server's, so pid 10 reads as replaced"
        );
        assert_eq!(
            place_caller_in_pane_tree(
                PeerCredentials {
                    pid: 10,
                    uid: 1000,
                    start_time: Some(5),
                },
                1000,
                Some((100, Some(10))),
                &ancestry,
            ),
            Err(CallerRejection::OutsidePane { peer_pid: 10 }),
            "the server itself is a live process outside every pane"
        );
    }

    #[test]
    fn every_refusal_names_the_pane_and_the_decision() {
        // The socket `ErrorBody` carries only a code and a message, so the reason
        // has to be legible in the prose a hook author reads.
        for (rejection, expected) in [
            (CallerRejection::UnknownPeer, "no peer credentials"),
            (
                CallerRejection::ForeignUid {
                    peer_pid: 7,
                    uid: 1001,
                },
                "uid 1001",
            ),
            (
                CallerRejection::CallerUnidentified { peer_pid: 7 },
                "already exited",
            ),
            (
                CallerRejection::CallerReplaced { peer_pid: 7 },
                "no longer holds the process",
            ),
            (
                CallerRejection::PaneHasNoProcess { peer_pid: 7 },
                "no running process",
            ),
            (
                CallerRejection::PaneRootUnidentified {
                    peer_pid: 7,
                    root_pid: 42,
                },
                "no recorded start time",
            ),
            (
                CallerRejection::PaneRootGone {
                    peer_pid: 7,
                    root_pid: 42,
                },
                "has exited",
            ),
            (
                CallerRejection::PaneRootReplaced {
                    peer_pid: 7,
                    root_pid: 42,
                },
                "now belongs to another process",
            ),
            (
                CallerRejection::OutsidePane { peer_pid: 7 },
                "not inside pane",
            ),
        ] {
            let message = rejection.message("w1-3");
            assert!(
                message.contains("w1-3") && message.contains("ADR 0014"),
                "{rejection:?} must name the pane and the decision: {message}"
            );
            assert!(
                message.contains(expected),
                "{rejection:?} must say why: {message}"
            );
        }
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
