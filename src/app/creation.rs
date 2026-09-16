// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::path::PathBuf;

#[cfg(test)]
use tracing::error;

use super::{
    api_helpers::{pane_agent_status, tab_attention_priority},
    App, Mode,
};
use crate::{config::NewTerminalCwdConfig, workspace::Workspace};

fn usable_directory(path: PathBuf) -> Option<PathBuf> {
    (path.is_absolute() && path.is_dir()).then_some(path)
}

pub(crate) fn resolve_new_terminal_cwd(
    policy: &NewTerminalCwdConfig,
    follow_cwd: Option<PathBuf>,
) -> PathBuf {
    match policy {
        NewTerminalCwdConfig::Follow => follow_cwd
            .and_then(usable_directory)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .and_then(usable_directory)
            })
            .or_else(|| std::env::current_dir().ok().and_then(usable_directory))
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Home => std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Current => {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        }
        NewTerminalCwdConfig::Path(path) => crate::worktree::expand_tilde_path(path),
    }
}

impl App {
    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        self.state
            .workspaces
            .get(ws_idx)?
            .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
    }

    pub(super) fn follow_cwd_for_pane_in_workspace(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<PathBuf> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        ws.tabs.get(tab_idx)?.follow_cwd_for_pane(
            pane_id,
            &self.state.terminals,
            &self.terminal_runtimes,
        )
    }

    pub(super) fn focused_pane_cwd_in_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.follow_cwd_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub(super) fn workspace_creation_cwd(&self, ws_idx: usize) -> Option<PathBuf> {
        self.focused_pane_cwd_in_workspace(ws_idx)
            .or_else(|| self.seed_cwd_from_workspace(ws_idx))
    }

    pub(super) fn resolve_new_terminal_cwd(&self, follow_cwd: Option<PathBuf>) -> PathBuf {
        resolve_new_terminal_cwd(&self.state.new_terminal_cwd, follow_cwd)
    }

    pub(super) fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.mode == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    pub(super) fn begin_tui_workspace_create(&mut self, request_id: &'static str) {
        if self.state.prompt_new_workspace_name {
            let source_ws_idx = self.workspace_creation_source();
            let source_workspace_id = source_ws_idx
                .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                .map(|ws| ws.id.clone());
            let follow_cwd = source_ws_idx.and_then(|ws_idx| self.workspace_creation_cwd(ws_idx));
            let cwd = self.resolve_new_terminal_cwd(follow_cwd);
            let intent = if self.state.new_terminal_cwd == NewTerminalCwdConfig::Follow {
                crate::app::state::PendingWorkspaceCreateCwd::Follow {
                    source_workspace_id,
                    suggested_cwd: cwd,
                }
            } else {
                crate::app::state::PendingWorkspaceCreateCwd::Resolved(cwd)
            };
            super::input::open_new_workspace_dialog(&mut self.state, intent);
            return;
        }

        self.runtime_workspace_create(
            request_id,
            crate::api::schema::WorkspaceCreateParams {
                cwd: None,
                focus: true,
                label: None,
            },
        );
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    /// Create a workspace with a real PTY (needs event_tx).
    #[cfg(test)]
    pub(crate) fn create_workspace(&mut self) {
        let follow_cwd = self
            .workspace_creation_source()
            .and_then(|ws_idx| self.workspace_creation_cwd(ws_idx));
        let initial_cwd = self.resolve_new_terminal_cwd(follow_cwd);
        if let Err(e) = self.create_workspace_with_events(initial_cwd, true) {
            error!(err = %e, "failed to create workspace");
            self.state.mode = Mode::Navigate;
        }
    }

    /// Create a workspace and emit the workspace/tab/pane plugin lifecycle events.
    /// The UI-driven create flows must mirror the socket API flows, which already
    /// emit these events (port upstream d74ba8c).
    #[cfg(test)]
    pub(crate) fn create_workspace_with_events(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<()> {
        let ws_idx = self.create_workspace_with_options(initial_cwd, focus)?;
        self.emit_workspace_open_events(ws_idx);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn create_tab(&mut self) {
        let custom_name = self.state.requested_new_tab_name.take();
        let active_before = self.state.active;
        let follow_cwd = self
            .state
            .active
            .and_then(|ws_idx| self.workspace_creation_cwd(ws_idx));
        let initial_cwd = self.resolve_new_terminal_cwd(follow_cwd);
        match self.create_tab_with_options(initial_cwd, true) {
            Ok(created_idx) => {
                // With no active workspace, `create_tab_with_options` falls back to
                // creating a fresh workspace; its returned index is then a workspace
                // index and the new tab is the workspace's first tab (index 0).
                let created_workspace = active_before.is_none();
                let ws_idx = if created_workspace {
                    Some(created_idx)
                } else {
                    self.state.active
                };
                let tab_idx = if created_workspace { 0 } else { created_idx };
                if let Some(name) = custom_name {
                    if let Some(ws) =
                        ws_idx.and_then(|ws_idx| self.state.workspaces.get_mut(ws_idx))
                    {
                        if let Some(tab) = ws.tabs.get_mut(tab_idx) {
                            tab.set_custom_name(name);
                        }
                        self.schedule_session_save();
                    }
                }
                if let Some(ws_idx) = ws_idx {
                    if created_workspace {
                        self.emit_workspace_open_events(ws_idx);
                    } else {
                        self.emit_tab_created_events(ws_idx, tab_idx);
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "failed to create tab");
            }
        }
    }

    #[cfg(test)]
    pub(super) fn create_tab_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let Some(ws_idx) = self.state.active else {
            return self.create_workspace_with_options(initial_cwd, focus);
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let ws = &mut self.state.workspaces[ws_idx];
        let (idx, terminal, runtime) = ws.create_tab(
            rows,
            cols,
            initial_cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
        )?;
        let root_pane = ws.tabs[idx].root_pane;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.remove_alias_shadowed_by_new_pane(root_pane);
        if focus {
            self.state.switch_workspace_tab(ws_idx, idx);
            self.state.mode = Mode::Terminal;
        }
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self
            .public_tab_id(ws_idx, idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&workspace_id, idx + 1));
        let root_pane = self.state.workspaces[ws_idx].tabs[idx].root_pane.raw();
        crate::logging::tab_created(&workspace_id, &tab_id, root_pane);
        self.schedule_session_save();
        Ok(idx)
    }

    pub(crate) fn create_workspace_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let (ws, terminal, runtime) = Workspace::new(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        self.state
            .remove_alias_shadowed_by_new_pane(self.state.workspaces[idx].tabs[0].root_pane);
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].tabs[0].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .tabs
                .iter()
                .flat_map(|tab| tab.layout.pane_ids().into_iter())
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids().into_iter())
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    pub(super) fn tab_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let (agg_state, seen) = tab
            .panes
            .values()
            .filter_map(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| (terminal.state, pane.seen))
            })
            .max_by_key(|(state, seen)| tab_attention_priority(*state, *seen))
            .unwrap_or((crate::detect::AgentState::Unknown, true));
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            number: tab_idx + 1,
            label: ws.tab_display_name(tab_idx)?,
            focused: self.state.active == Some(ws_idx) && ws.active_tab == tab_idx,
            pane_count: tab.panes.len(),
            agent_status: pane_agent_status(agg_state, seen),
        })
    }

    pub(super) fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    pub(super) fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info(ws_idx, tab_idx)?,
            root_pane: self.root_pane_info(ws_idx, tab_idx)?,
        })
    }

    pub(super) fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        self.pane_info(ws_idx, tab.root_pane)
    }

    pub(super) fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let scroll = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .and_then(|runtime| runtime.scroll_metrics())
            .map(|metrics| crate::api::schema::PaneScrollInfo {
                offset_from_bottom: metrics.offset_from_bottom as u64,
                max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                viewport_rows: metrics.viewport_rows as u64,
            });
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        let presentation = terminal.effective_presentation();
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            terminal_id: terminal.id.to_string(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            cwd: ws.tabs[tab_idx]
                .cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            foreground_cwd: ws.tabs[tab_idx]
                .foreground_cwd_for_pane(pane_id, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            label: terminal.manual_label.clone(),
            agent: terminal.effective_agent_label().map(str::to_string),
            title: presentation.title,
            terminal_title: terminal.terminal_title().map(str::to_string),
            terminal_title_stripped: terminal.terminal_title_stripped(),
            display_agent: presentation.display_agent,
            agent_status: pane_agent_status(terminal.state, pane.seen),

            state_labels: presentation.state_labels,
            tokens: terminal.metadata_tokens.values(),
            agent_session: terminal_agent_session_info(terminal),
            scroll,
            revision: terminal.revision,
        })
    }

    /// Resolve the AUTHORITATIVE (hook-derived, never detection) receiver identity
    /// for a public pane id, for M3a receipt validation. Returns `None` when the
    /// pane does not resolve OR no hook reported an identity for it — a detection-only
    /// label is NOT receipt-capable, so the caller treats `None` as
    /// `receiver_identity_unverified`.
    ///
    /// Two hook shapes carry identity. A full-lifecycle integration carries it on
    /// `hook_authority` (identity + lifecycle). A session-identity-only integration
    /// carries it on `hook_identity`: its lifecycle stays screen-detected, but its
    /// label and session came from its own hook, so they anchor receipts just the same.
    pub(crate) fn authoritative_receiver_identity(
        &self,
        public_pane_id: &str,
    ) -> Option<crate::zynk::receipt::AuthoritativeReceiver> {
        let (ws_idx, pane_id) = self.parse_pane_id(public_pane_id)?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        // Hook-reported identity ONLY — never `effective_agent_label()`'s detection
        // fallback — and only while it is CONFIRMED: an identity accepted inside the
        // window between an exit's capture and its handling is held provisional until
        // the detector sees the process again, and a provisional identity anchors no
        // receipt (`TerminalState::confirmed_hook_owner`).
        if self
            .terminal_runtimes
            .get(&pane.attached_terminal_id)
            .is_some_and(|runtime| !runtime.pending_process_exits().is_empty())
        {
            return None;
        }
        let (_, agent_label) = terminal.confirmed_hook_owner()?;
        // Owner coherence: a persisted session is part of this receiver's identity only when it
        // was reported for the SAME agent the hook identity names; a session another owner
        // persisted on this terminal must not anchor a receipt (Codex Gate-2 R13).
        let agent_session = terminal_agent_session_info(terminal)
            .filter(|info| info.agent == agent_label)
            .and_then(|info| serde_json::to_value(info).ok());
        Some(crate::zynk::receipt::AuthoritativeReceiver {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            terminal_id: terminal.id.to_string(),
            agent_label: agent_label.to_string(),
            agent_session,
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::terminal::TerminalRuntime, String)> {
        let runtime =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state(&self.state.terminals);
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name_from(&self.state.terminals, &self.terminal_runtimes),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.tabs.len(),
            active_tab_id: self.public_tab_id(index, ws.active_tab).unwrap_or_else(|| {
                crate::workspace::public_tab_id_for_number(&ws.id, ws.active_tab + 1)
            }),
            agent_status: pane_agent_status(agg_state, seen),
            tokens: ws.metadata_tokens.values(),
            worktree: ws
                .worktree_space()
                .map(|space| crate::api::schema::WorkspaceWorktreeInfo {
                    repo_key: space.key.clone(),
                    repo_name: space.label.clone(),
                    repo_root: space.repo_root.display().to_string(),
                    checkout_path: space.checkout_path.display().to_string(),
                    is_linked_worktree: space.is_linked_worktree,
                }),
        }
    }
}

pub(crate) fn terminal_agent_session_info(
    terminal: &crate::terminal::TerminalState,
) -> Option<crate::api::schema::AgentSessionInfo> {
    if let Some(authority) = terminal.hook_authority.as_ref() {
        if let Some(session_ref) = authority.session_ref.as_ref() {
            return Some(crate::api::schema::AgentSessionInfo {
                source: authority.source.clone(),
                agent: authority.agent_label.clone(),
                kind: session_ref.kind,
                value: session_ref.value.clone(),
            });
        }
    }

    terminal
        .persisted_agent_session
        .as_ref()
        .map(|session| crate::api::schema::AgentSessionInfo {
            source: session.source.clone(),
            agent: session.agent.clone(),
            kind: session.session_ref.kind,
            value: session.session_ref.value.clone(),
        })
}

#[cfg(test)]
pub(super) mod tests {
    use super::App;
    use crate::api::schema as api;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;
    use std::time::{Duration, Instant};

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    pub(in crate::app) struct CwdFixture {
        pub(in crate::app) app: App,
        pub(in crate::app) root: std::path::PathBuf,
    }

    #[derive(Clone, Copy, Debug)]
    enum CachedCwdRoute {
        Tab,
        Layout,
        Split,
    }

    const CACHED_CWD_ROUTES: [CachedCwdRoute; 3] = [
        CachedCwdRoute::Tab,
        CachedCwdRoute::Layout,
        CachedCwdRoute::Split,
    ];

    impl CwdFixture {
        pub(in crate::app) fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "zynk-creation-cwd-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            for name in ["seed", "cached", "other"] {
                std::fs::create_dir_all(root.join(name)).unwrap();
            }
            let mut app = test_app();
            app.state.default_shell = "/usr/bin/true".into();
            app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
            app.state.new_terminal_cwd = crate::config::NewTerminalCwdConfig::Follow;
            let mut source = Workspace::test_new("cwd-source");
            source.identity_cwd = root.join("seed");
            source.test_add_tab(None);
            let mut other = Workspace::test_new("cwd-other");
            other.identity_cwd = root.join("other");
            app.state.workspaces = vec![source, other];
            app.state.ensure_test_terminals();
            for (ws, tab, name) in [(0, 0, "seed"), (0, 1, "cached"), (1, 0, "other")] {
                let tab = &app.state.workspaces[ws].tabs[tab];
                let terminal = tab.terminal_id(tab.root_pane).unwrap().clone();
                app.state.terminals.get_mut(&terminal).unwrap().cwd = root.join(name);
            }
            app.state.switch_workspace_tab(0, 1);
            app.state.mode = crate::app::Mode::Terminal;
            assert_eq!(app.terminal_runtimes.len(), 0);
            assert_ne!(root.join("cached"), std::env::current_dir().unwrap());
            assert_ne!(
                Some(root.join("cached").into_os_string()),
                std::env::var_os("HOME")
            );
            Self { app, root }
        }

        pub(in crate::app) async fn install_foreground(
            &mut self,
            ws: usize,
            tab_idx: usize,
        ) -> std::path::PathBuf {
            let shell_cwd = self.cwd(ws, tab_idx);
            let leader_cwd = self.root.join(format!("leader-{ws}-{tab_idx}"));
            let marker = self.root.join(format!("ready-{ws}-{tab_idx}"));
            std::fs::create_dir(&leader_cwd).unwrap();
            let tab = &self.app.state.workspaces[ws].tabs[tab_idx];
            let pane_id = tab.root_pane;
            let terminal_id = tab.terminal_id(pane_id).unwrap().clone();
            let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
                pane_id,
                24,
                80,
                shell_cwd.clone(),
                &[
                    "/bin/bash".into(),
                    "--noprofile".into(),
                    "--norc".into(),
                    "-i".into(),
                ],
                &crate::pane::PaneLaunchEnv::default(),
                crate::pane::AgentDetection::Enabled,
                0,
                self.app.state.host_terminal_theme,
                self.app.state.host_terminal_appearance,
                self.app.event_tx.clone(),
                self.app.render_notify.clone(),
                self.app.render_dirty.clone(),
            )
            .unwrap();
            // Own the real runtime before readiness or any assertion can fail.
            assert!(self
                .app
                .terminal_runtimes
                .insert(terminal_id.clone(), runtime)
                .is_none());
            let runtime = self.app.terminal_runtimes.get(&terminal_id).unwrap();
            let shell = runtime.child_pid().unwrap();
            runtime
                .send_bytes(bytes::Bytes::from(format!(
                    "/bin/sh -c 'cd \"{}\" && printf %s $$ > \"{}\" && exec sleep 30'\r",
                    leader_cwd.display(),
                    marker.display()
                )))
                .await
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let leader = loop {
                if let Ok(text) = std::fs::read_to_string(&marker) {
                    if let Ok(pid) = text.parse::<u32>() {
                        break pid;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "foreground setup timeout: shell={shell}, marker={marker:?}, output={:?}",
                    runtime.recent_text(10)
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert_ne!(shell, leader);
            assert_eq!(crate::platform::process_cwd(shell), Some(shell_cwd.clone()));
            assert_eq!(
                crate::platform::process_cwd(leader),
                Some(leader_cwd.clone())
            );
            assert_eq!(
                crate::platform::foreground_process_group_id(shell),
                Some(leader)
            );
            assert_eq!(unsafe { libc::getpgid(leader as i32) }, leader as i32);
            assert_eq!(runtime.cwd(), Some(shell_cwd));
            assert_eq!(runtime.foreground_cwd(), Some(leader_cwd.clone()));
            leader_cwd
        }

        fn cwd(&self, ws: usize, tab: usize) -> std::path::PathBuf {
            let tab = &self.app.state.workspaces[ws].tabs[tab];
            let terminal = tab.terminal_id(tab.root_pane).unwrap();
            self.app.state.terminals.get(terminal).unwrap().cwd.clone()
        }

        fn call(&mut self, method: api::Method) -> api::ResponseResult {
            let response = self.app.handle_api_request(api::Request {
                id: "test.creation.cwd".into(),
                method,
            });
            let response: api::SuccessResponse = serde_json::from_str(&response)
                .unwrap_or_else(|error| panic!("{error}: {response}"));
            response.result
        }

        fn create_target(
            &mut self,
            route: CachedCwdRoute,
            cwd: Option<String>,
        ) -> std::path::PathBuf {
            self.app.state.switch_workspace(1);
            let focus = self.app.state.current_pane_focus_target();
            let before: std::collections::HashSet<_> =
                self.app.state.terminals.keys().cloned().collect();
            let workspace_id = Some(self.app.public_workspace_id(0));
            let method = match route {
                CachedCwdRoute::Tab => api::Method::TabCreate(api::TabCreateParams {
                    workspace_id,
                    cwd,
                    focus: false,
                    label: None,
                }),
                CachedCwdRoute::Layout => api::Method::LayoutApply(api::LayoutApplyParams {
                    workspace_id,
                    tab_id: None,
                    tab_label: None,
                    focus: false,
                    root: api::LayoutNode::Pane {
                        pane: api::LayoutPane {
                            cwd,
                            ..Default::default()
                        },
                    },
                }),
                CachedCwdRoute::Split => api::Method::PaneSplit(api::PaneSplitParams {
                    workspace_id,
                    target_pane_id: None,
                    cwd,
                    focus: false,
                    direction: api::SplitDirection::Right,
                    ratio: None,
                }),
            };
            self.call(method);
            let created: Vec<_> = self
                .app
                .state
                .terminals
                .iter()
                .filter(|(id, _)| !before.contains(*id))
                .map(|(_, terminal)| terminal.cwd.clone())
                .collect();
            assert_eq!(created.len(), 1, "{route:?}");
            assert_eq!(
                self.app.state.current_pane_focus_target(),
                focus,
                "{route:?}"
            );
            created[0].clone()
        }
    }

    impl Drop for CwdFixture {
        fn drop(&mut self) {
            for (_, runtime) in self.app.terminal_runtimes.drain() {
                runtime.shutdown();
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn m825_live_foreground_drives_background_tab_layout_and_split() {
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for route in CACHED_CWD_ROUTES {
            let mut fixture = CwdFixture::new();
            let leader = fixture.install_foreground(0, 1).await;
            actual.push(fixture.create_target(route, None));
            expected.push(leader);
        }
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn m825_live_layout_replacement_uses_target_before_workspace_focus() {
        let mut fixture = CwdFixture::new();
        let target_cwd = fixture.install_foreground(0, 0).await;
        let focused_cwd = fixture.install_foreground(0, 1).await;
        assert_ne!(target_cwd, focused_cwd);
        fixture.app.state.switch_workspace(1);
        let focus = fixture.app.state.current_pane_focus_target();
        let result = fixture.call(api::Method::LayoutApply(api::LayoutApplyParams {
            workspace_id: None,
            tab_id: fixture.app.public_tab_id(0, 0),
            tab_label: None,
            focus: false,
            root: api::LayoutNode::Pane {
                pane: api::LayoutPane::default(),
            },
        }));
        let api::ResponseResult::LayoutApply { layout } = result else {
            panic!("not a layout response")
        };
        let (ws, tab) = fixture.app.parse_tab_id(&layout.tab_id).unwrap();
        assert_eq!(ws, 0);
        assert_eq!(fixture.app.state.workspaces[0].tabs.len(), 2);
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
        assert_eq!(fixture.cwd(ws, tab), target_cwd);
    }

    #[tokio::test]
    async fn m825_live_workspace_api_uses_focused_leader() {
        let mut fixture = CwdFixture::new();
        let leader = fixture.install_foreground(0, 1).await;
        let focus = fixture.app.state.current_pane_focus_target();
        fixture.call(api::Method::WorkspaceCreate(api::WorkspaceCreateParams {
            cwd: None,
            focus: false,
            label: Some("live".into()),
        }));
        assert_eq!(fixture.app.state.workspaces.len(), 3);
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
        assert_eq!(
            fixture.app.state.workspaces[2].custom_name.as_deref(),
            Some("live")
        );
        assert_eq!(fixture.app.state.workspaces[2].identity_cwd, leader);
        assert_eq!(fixture.cwd(2, 0), leader);
    }

    #[tokio::test]
    async fn m825_live_workspace_prompt_suggests_selected_sources_leader() {
        let mut fixture = CwdFixture::new();
        let leader = fixture.install_foreground(0, 1).await;
        let source_id = fixture.app.state.workspaces[0].id.clone();
        fixture.app.state.switch_workspace(1);
        fixture.app.state.mode = crate::app::Mode::Navigate;
        fixture.app.state.selected = 0;
        fixture.app.state.prompt_new_workspace_name = true;
        fixture.app.begin_tui_workspace_create("test.m825.prompt");
        let Some(crate::app::state::PendingWorkspaceCreateCwd::Follow {
            source_workspace_id,
            suggested_cwd,
        }) = fixture.app.state.pending_workspace_create_cwd.as_ref()
        else {
            panic!("missing Follow intent")
        };
        assert_eq!(source_workspace_id.as_deref(), Some(source_id.as_str()));
        assert_eq!(fixture.app.state.workspaces.len(), 2);
        assert_eq!(fixture.app.state.mode, crate::app::Mode::RenameWorkspace);
        assert!(fixture.app.state.name_input_replace_on_type);
        assert_eq!(suggested_cwd, &leader);
        assert_eq!(fixture.app.state.name_input, "leader-0-1");
    }

    #[tokio::test]
    async fn m825_live_workspace_confirmation_reresolves_original_sources_new_leader() {
        let mut fixture = CwdFixture::new();
        let original = fixture.install_foreground(0, 1).await;
        let source_id = fixture.app.state.workspaces[0].id.clone();
        fixture.app.state.prompt_new_workspace_name = true;
        fixture.app.begin_tui_workspace_create("test.m825.confirm");
        assert!(
            matches!(fixture.app.state.pending_workspace_create_cwd.as_ref(),
            Some(crate::app::state::PendingWorkspaceCreateCwd::Follow { source_workspace_id: Some(id), .. }) if id == &source_id)
        );
        let new_tab = fixture.app.state.workspaces[0].test_add_tab(None);
        fixture.app.state.ensure_test_terminals();
        let tab = &fixture.app.state.workspaces[0].tabs[new_tab];
        let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
        fixture
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .cwd = fixture.root.join("other");
        let new_leader = fixture.install_foreground(0, new_tab).await;
        assert_ne!(original, new_leader);
        fixture.app.state.switch_workspace_tab(0, new_tab);
        fixture.app.state.switch_workspace(1);
        fixture.app.state.name_input = "  chosen live label  ".into();
        fixture
            .app
            .handle_rename_key_via_api(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ));
        assert_eq!(fixture.app.state.workspaces.len(), 3);
        assert_eq!(
            fixture.app.state.workspaces[2].custom_name.as_deref(),
            Some("chosen live label")
        );
        assert!(fixture.app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(fixture.app.state.active, Some(2));
        assert_eq!(fixture.app.state.workspaces[2].identity_cwd, new_leader);
        assert_eq!(fixture.cwd(2, 0), new_leader);
    }

    #[tokio::test]
    async fn m825_live_workspace_prompt_off_dispatches_selected_sources_leader() {
        let mut fixture = CwdFixture::new();
        let leader = fixture.install_foreground(0, 1).await;
        fixture.app.state.switch_workspace(1);
        fixture.app.state.mode = crate::app::Mode::Navigate;
        fixture.app.state.selected = 0;
        fixture.app.state.prompt_new_workspace_name = false;
        fixture
            .app
            .begin_tui_workspace_create("test.m825.prompt-off");
        assert_eq!(fixture.app.state.workspaces.len(), 3);
        assert!(fixture.app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(fixture.app.state.active, Some(2));
        assert_eq!(fixture.app.state.mode, crate::app::Mode::Terminal);
        assert_eq!(fixture.cwd(2, 0), leader);
    }

    #[tokio::test]
    async fn m825_live_tui_split_submitter_follows_leader() {
        let mut fixture = CwdFixture::new();
        let leader = fixture.install_foreground(0, 1).await;
        fixture
            .app
            .split_focused_pane_via_api(api::SplitDirection::Right);
        let tab = &fixture.app.state.workspaces[0].tabs[1];
        assert_eq!(tab.panes.len(), 2);
        assert_ne!(tab.layout.focused(), tab.root_pane);
        let terminal = tab.terminal_id(tab.layout.focused()).unwrap();
        assert_eq!(
            fixture.app.state.terminals.get(terminal).unwrap().cwd,
            leader
        );
    }

    #[tokio::test]
    async fn m825_legacy_workspace_wrapper_follows_focused_leader() {
        let mut fixture = CwdFixture::new();
        let leader = fixture.install_foreground(0, 1).await;
        fixture.app.create_workspace();
        assert_eq!(fixture.app.state.workspaces.len(), 3);
        assert_eq!(fixture.app.state.active, Some(2));
        assert_eq!(fixture.cwd(2, 0), leader);
    }

    #[tokio::test]
    async fn m825_legacy_tab_wrapper_follows_focused_leader() {
        let mut fixture = CwdFixture::new();
        let leader = fixture.install_foreground(0, 1).await;
        fixture.app.state.requested_new_tab_name = Some("legacy live".into());
        fixture.app.create_tab();
        assert_eq!(fixture.app.state.workspaces[0].tabs.len(), 3);
        assert_eq!(fixture.app.state.workspaces[0].active_tab, 2);
        assert_eq!(fixture.cwd(0, 2), leader);
    }

    #[tokio::test]
    async fn m825_legacy_split_wrapper_follows_leader() {
        let mut fixture = CwdFixture::new();
        let leader = fixture.install_foreground(0, 1).await;
        fixture.app.state.split_pane(
            &mut fixture.app.terminal_runtimes,
            ratatui::layout::Direction::Horizontal,
        );
        let tab = &fixture.app.state.workspaces[0].tabs[1];
        assert_eq!(tab.panes.len(), 2);
        assert_ne!(tab.layout.focused(), tab.root_pane);
        let terminal = tab.terminal_id(tab.layout.focused()).unwrap();
        assert_eq!(
            fixture.app.state.terminals.get(terminal).unwrap().cwd,
            leader
        );
    }

    #[tokio::test]
    async fn m825_live_source_preserves_explicit_and_nonfollow_policies() {
        use crate::config::NewTerminalCwdConfig;
        for route in CACHED_CWD_ROUTES {
            for policy in [
                NewTerminalCwdConfig::Follow,
                NewTerminalCwdConfig::Home,
                NewTerminalCwdConfig::Current,
                NewTerminalCwdConfig::Path("/".into()),
            ] {
                let mut fixture = CwdFixture::new();
                let leader = fixture.install_foreground(0, 1).await;
                let explicit =
                    (policy == NewTerminalCwdConfig::Follow).then(|| fixture.root.join("other"));
                let expected = match &policy {
                    NewTerminalCwdConfig::Follow => explicit.clone().unwrap(),
                    NewTerminalCwdConfig::Home => std::env::var_os("HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::env::current_dir().unwrap()),
                    NewTerminalCwdConfig::Current => std::env::current_dir().unwrap(),
                    NewTerminalCwdConfig::Path(_) => std::path::PathBuf::from("/"),
                };
                assert_ne!(expected, leader);
                fixture.app.state.new_terminal_cwd = policy;
                let created =
                    fixture.create_target(route, explicit.map(|path| path.display().to_string()));
                assert_eq!(created, expected, "{route:?}");
            }
        }
    }

    #[tokio::test]
    async fn tab_create_follows_cached_focused_pane_cwd_without_runtime() {
        let mut fixture = CwdFixture::new();
        fixture.app.state.switch_workspace(1);
        let focus = fixture.app.state.current_pane_focus_target();
        let result = fixture.call(api::Method::TabCreate(api::TabCreateParams {
            workspace_id: Some(fixture.app.public_workspace_id(0)),
            cwd: None,
            focus: false,
            label: Some("cached".into()),
        }));
        assert!(matches!(result, api::ResponseResult::TabCreated { .. }));
        assert_eq!(fixture.app.state.workspaces[0].tabs.len(), 3);
        assert_eq!(fixture.cwd(0, 2), fixture.root.join("cached"));
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
    }

    #[tokio::test]
    async fn layout_apply_new_tab_follows_cached_focused_pane_cwd_without_runtime() {
        let mut fixture = CwdFixture::new();
        fixture.app.state.switch_workspace(1);
        let focus = fixture.app.state.current_pane_focus_target();
        let result = fixture.call(api::Method::LayoutApply(api::LayoutApplyParams {
            workspace_id: Some(fixture.app.public_workspace_id(0)),
            tab_id: None,
            tab_label: Some("cached".into()),
            focus: false,
            root: api::LayoutNode::Pane {
                pane: api::LayoutPane::default(),
            },
        }));
        assert!(matches!(result, api::ResponseResult::LayoutApply { .. }));
        assert_eq!(fixture.app.state.workspaces[0].tabs.len(), 3);
        assert_eq!(fixture.cwd(0, 2), fixture.root.join("cached"));
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
    }

    #[test]
    fn cached_cwd_helpers_resolve_owner_and_missing_targets() {
        let mut fixture = CwdFixture::new();
        fixture.app.state.switch_workspace(1);
        for (ws, tab, name) in [(0, 0, "seed"), (0, 1, "cached"), (1, 0, "other")] {
            let pane = fixture.app.state.workspaces[ws].tabs[tab].root_pane;
            assert_eq!(
                fixture.app.follow_cwd_for_pane_in_workspace(ws, pane),
                Some(fixture.root.join(name))
            );
            assert_eq!(
                fixture.app.follow_cwd_for_pane_in_workspace(1 - ws, pane),
                None
            );
        }
        assert_eq!(
            fixture.app.focused_pane_cwd_in_workspace(0),
            Some(fixture.root.join("cached"))
        );
        assert_eq!(
            fixture.app.focused_pane_cwd_in_workspace(1),
            Some(fixture.root.join("other"))
        );
        assert_eq!(fixture.app.focused_pane_cwd_in_workspace(2), None);
        let pane = fixture.app.state.workspaces[0].tabs[1].root_pane;
        assert_eq!(fixture.app.follow_cwd_for_pane_in_workspace(2, pane), None);
        assert_eq!(
            fixture
                .app
                .follow_cwd_for_pane_in_workspace(0, crate::layout::PaneId::alloc()),
            None
        );
        let terminal_id = fixture.app.state.workspaces[0].tabs[1]
            .terminal_id(pane)
            .unwrap()
            .clone();
        fixture.app.state.terminals.remove(&terminal_id);
        assert_eq!(fixture.app.follow_cwd_for_pane_in_workspace(0, pane), None);
        assert_eq!(fixture.app.focused_pane_cwd_in_workspace(0), None);
    }

    #[tokio::test]
    async fn cached_cwd_helpers_prefer_runtime_then_cached_state() {
        let mut fixture = CwdFixture::new();
        let pane = fixture.app.state.workspaces[0].tabs[1].root_pane;
        let terminal_id = fixture.app.state.workspaces[0].tabs[1]
            .terminal_id(pane)
            .unwrap()
            .clone();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        assert_eq!(runtime.cwd(), None);
        fixture
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        assert_eq!(
            fixture.app.focused_pane_cwd_in_workspace(0),
            Some(fixture.root.join("cached"))
        );
        let reported = fixture.root.join("other");
        fixture
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_publish_reported_cwd(reported.clone());
        assert_eq!(
            fixture.app.follow_cwd_for_pane_in_workspace(0, pane),
            Some(reported.clone())
        );
        assert_eq!(fixture.app.focused_pane_cwd_in_workspace(0), Some(reported));
        fixture
            .app
            .terminal_runtimes
            .remove(&terminal_id)
            .unwrap()
            .shutdown();
        assert_eq!(
            fixture.app.focused_pane_cwd_in_workspace(0),
            Some(fixture.root.join("cached"))
        );
    }

    #[tokio::test]
    async fn cached_cwd_pane_split_preserves_inactive_target_ownership() {
        let mut fixture = CwdFixture::new();
        fixture.app.state.switch_workspace(1);
        let focus = fixture.app.state.current_pane_focus_target();
        let pane = fixture.app.state.workspaces[0].tabs[0].root_pane;
        let result = fixture.call(api::Method::PaneSplit(api::PaneSplitParams {
            workspace_id: None,
            target_pane_id: fixture.app.public_pane_id(0, pane),
            direction: api::SplitDirection::Right,
            ratio: Some(0.4),
            cwd: None,
            focus: false,
        }));
        let api::ResponseResult::PaneInfo { pane: created } = result else {
            panic!("not a split response")
        };
        let (ws_idx, created_pane) = fixture.app.parse_pane_id(&created.pane_id).unwrap();
        assert_eq!(ws_idx, 0);
        assert_eq!(
            fixture.app.state.workspaces[0].find_tab_index_for_pane(created_pane),
            Some(0)
        );
        let terminal_id = fixture.app.state.workspaces[0].tabs[0]
            .terminal_id(created_pane)
            .unwrap();
        assert_eq!(
            fixture.app.state.terminals.get(terminal_id).unwrap().cwd,
            fixture.root.join("seed")
        );
        assert_eq!(fixture.app.state.workspaces[0].tabs[0].panes.len(), 2);
        assert_eq!(fixture.app.state.workspaces[0].tabs[1].panes.len(), 1);
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
    }

    #[tokio::test]
    async fn cached_cwd_layout_replacement_and_split_preserve_target_before_focus() {
        let mut fixture = CwdFixture::new();
        fixture.app.state.switch_workspace(1);
        let focus = fixture.app.state.current_pane_focus_target();
        let result = fixture.call(api::Method::LayoutApply(api::LayoutApplyParams {
            workspace_id: None,
            tab_id: fixture.app.public_tab_id(0, 0),
            tab_label: None,
            focus: false,
            root: api::LayoutNode::Split {
                direction: api::SplitDirection::Right,
                ratio: 0.4,
                first: Box::new(api::LayoutNode::Pane {
                    pane: api::LayoutPane::default(),
                }),
                second: Box::new(api::LayoutNode::Pane {
                    pane: api::LayoutPane::default(),
                }),
            },
        }));
        let api::ResponseResult::LayoutApply { layout } = result else {
            panic!("not a layout response")
        };
        let (ws_idx, tab_idx) = fixture.app.parse_tab_id(&layout.tab_id).unwrap();
        assert_eq!(ws_idx, 0);
        let tab = &fixture.app.state.workspaces[ws_idx].tabs[tab_idx];
        assert_eq!(fixture.app.state.workspaces[0].tabs.len(), 2);
        assert_eq!(tab.panes.len(), 2);
        for pane in tab.panes.keys() {
            let terminal_id = tab.terminal_id(*pane).unwrap();
            assert_eq!(
                fixture.app.state.terminals.get(terminal_id).unwrap().cwd,
                fixture.root.join("seed")
            );
        }
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
    }

    #[tokio::test]
    async fn cached_cwd_creation_respects_explicit_and_policy_precedence() {
        use crate::config::NewTerminalCwdConfig;
        for route in CACHED_CWD_ROUTES {
            for policy in [
                NewTerminalCwdConfig::Home,
                NewTerminalCwdConfig::Current,
                NewTerminalCwdConfig::Path("/".into()),
            ] {
                let mut fixture = CwdFixture::new();
                let expected = super::resolve_new_terminal_cwd(&policy, None);
                assert_ne!(expected, fixture.root.join("cached"));
                fixture.app.state.new_terminal_cwd = policy;
                assert_eq!(fixture.create_target(route, None), expected, "{route:?}");
            }
            let mut fixture = CwdFixture::new();
            fixture.app.state.new_terminal_cwd = NewTerminalCwdConfig::Home;
            let explicit = fixture.root.join("seed");
            assert_eq!(
                fixture.create_target(route, Some(explicit.display().to_string())),
                explicit,
                "{route:?}"
            );
        }
    }

    fn assert_invalid_cached_cwd_rejected(invalid: impl Fn(&CwdFixture) -> std::path::PathBuf) {
        for route in CACHED_CWD_ROUTES {
            let mut fixture = CwdFixture::new();
            let cwd = invalid(&fixture);
            let pane = fixture.app.state.workspaces[0].tabs[1].root_pane;
            let terminal_id = fixture.app.state.workspaces[0].tabs[1]
                .terminal_id(pane)
                .unwrap()
                .clone();
            fixture
                .app
                .state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .cwd = cwd.clone();
            assert_eq!(
                fixture.app.focused_pane_cwd_in_workspace(0),
                Some(cwd.clone())
            );
            let expected =
                super::resolve_new_terminal_cwd(&crate::config::NewTerminalCwdConfig::Follow, None);
            assert_ne!(expected, cwd);
            let actual = fixture.create_target(route, None);
            assert_eq!(actual, expected, "{route:?}");
            assert!(
                actual.is_absolute() && actual.is_dir(),
                "{route:?}: {actual:?}"
            );
        }
    }

    #[tokio::test]
    async fn cached_cwd_creation_rejects_relative_directory() {
        assert!(std::path::Path::new(".").is_dir());
        assert_invalid_cached_cwd_rejected(|_| ".".into());
    }

    #[tokio::test]
    async fn cached_cwd_creation_rejects_file() {
        assert_invalid_cached_cwd_rejected(|fixture| {
            let path = fixture.root.join("file");
            std::fs::write(&path, b"not a directory").unwrap();
            path
        });
    }

    #[tokio::test]
    async fn cached_cwd_creation_rejects_deleted_directory() {
        assert_invalid_cached_cwd_rejected(|fixture| {
            let path = fixture.root.join("deleted");
            std::fs::create_dir(&path).unwrap();
            std::fs::remove_dir(&path).unwrap();
            path
        });
    }

    #[tokio::test]
    async fn workspace_create_follows_focused_cwd_through_api_and_runtime() {
        for through_runtime in [false, true] {
            let mut fixture = CwdFixture::new();
            let focus = fixture.app.state.current_pane_focus_target();
            let params = api::WorkspaceCreateParams {
                cwd: None,
                focus: false,
                label: Some("focused".into()),
            };
            if through_runtime {
                fixture
                    .app
                    .runtime_workspace_create("test.workspace.focused", params);
            } else {
                assert!(matches!(
                    fixture.call(api::Method::WorkspaceCreate(params)),
                    api::ResponseResult::WorkspaceCreated { .. }
                ));
            }
            assert_eq!(fixture.app.state.workspaces.len(), 3);
            assert_eq!(
                fixture.app.state.workspaces[2].identity_cwd,
                fixture.root.join("cached"),
                "runtime={through_runtime}"
            );
            assert_eq!(fixture.cwd(2, 0), fixture.root.join("cached"));
            assert_eq!(
                fixture.app.state.workspaces[2].custom_name.as_deref(),
                Some("focused")
            );
            assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
        }
    }

    #[test]
    fn workspace_create_prompt_suggests_selected_workspaces_focused_cwd() {
        let mut fixture = CwdFixture::new();
        let source_id = fixture.app.state.workspaces[0].id.clone();
        fixture.app.state.switch_workspace(1);
        fixture.app.state.mode = crate::app::Mode::Navigate;
        fixture.app.state.selected = 0;
        fixture.app.state.prompt_new_workspace_name = true;
        fixture
            .app
            .begin_tui_workspace_create("test.workspace.prompt");
        let Some(crate::app::state::PendingWorkspaceCreateCwd::Follow {
            source_workspace_id,
            suggested_cwd,
        }) = fixture.app.state.pending_workspace_create_cwd.as_ref()
        else {
            panic!("missing Follow intent")
        };
        assert_eq!(source_workspace_id.as_deref(), Some(source_id.as_str()));
        assert_eq!(suggested_cwd, &fixture.root.join("cached"));
        assert_eq!(fixture.app.state.name_input, "cached");
        assert!(fixture.app.state.name_input_replace_on_type);
        assert_eq!(fixture.app.state.mode, crate::app::Mode::RenameWorkspace);
        assert_eq!(fixture.app.state.workspaces.len(), 2);
    }

    #[tokio::test]
    async fn workspace_create_confirmation_reresolves_original_sources_new_focus() {
        let mut fixture = CwdFixture::new();
        let source_id = fixture.app.state.workspaces[0].id.clone();
        fixture.app.state.prompt_new_workspace_name = true;
        fixture
            .app
            .begin_tui_workspace_create("test.workspace.confirm");
        assert!(
            matches!(fixture.app.state.pending_workspace_create_cwd.as_ref(),
            Some(crate::app::state::PendingWorkspaceCreateCwd::Follow { source_workspace_id: Some(id), .. }) if id == &source_id)
        );

        let new_cwd = fixture.root.join("new-focus");
        std::fs::create_dir(&new_cwd).unwrap();
        let new_tab = fixture.app.state.workspaces[0].test_add_tab(None);
        fixture.app.state.ensure_test_terminals();
        let tab = &fixture.app.state.workspaces[0].tabs[new_tab];
        let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
        fixture
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .cwd = new_cwd.clone();
        fixture.app.state.switch_workspace_tab(0, new_tab);
        fixture.app.state.switch_workspace(1);
        assert_eq!(fixture.app.state.mode, crate::app::Mode::RenameWorkspace);
        fixture.app.state.name_input = "  chosen label  ".into();
        fixture
            .app
            .handle_rename_key_via_api(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ));

        assert_eq!(fixture.app.state.workspaces.len(), 3);
        assert_eq!(fixture.app.state.workspaces[2].identity_cwd, new_cwd);
        assert_eq!(fixture.cwd(2, 0), new_cwd);
        assert_eq!(
            fixture.app.state.workspaces[2].custom_name.as_deref(),
            Some("chosen label")
        );
        assert!(fixture.app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(fixture.app.state.active, Some(2));
    }

    #[tokio::test]
    async fn workspace_create_prompt_off_follows_selected_workspace_focus() {
        let mut fixture = CwdFixture::new();
        fixture.app.state.switch_workspace(1);
        fixture.app.state.mode = crate::app::Mode::Navigate;
        fixture.app.state.selected = 0;
        fixture.app.state.prompt_new_workspace_name = false;
        fixture
            .app
            .begin_tui_workspace_create("test.workspace.prompt-off");
        assert_eq!(fixture.app.state.workspaces.len(), 3);
        assert_eq!(fixture.cwd(2, 0), fixture.root.join("cached"));
        assert!(fixture.app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(fixture.app.state.active, Some(2));
        assert_eq!(fixture.app.state.mode, crate::app::Mode::Terminal);
    }

    #[test]
    fn workspace_creation_cwd_falls_back_to_seed_without_redefining_identity() {
        let mut fixture = CwdFixture::new();
        assert_eq!(
            fixture.app.seed_cwd_from_workspace(0),
            Some(fixture.root.join("seed"))
        );
        assert_eq!(
            fixture.app.workspace_creation_cwd(0),
            Some(fixture.root.join("cached"))
        );
        let pane = fixture.app.state.workspaces[0].tabs[1].root_pane;
        let terminal_id = fixture.app.state.workspaces[0].tabs[1]
            .terminal_id(pane)
            .unwrap()
            .clone();
        fixture.app.state.terminals.remove(&terminal_id);
        assert_eq!(fixture.app.focused_pane_cwd_in_workspace(0), None);
        assert_eq!(
            fixture.app.workspace_creation_cwd(0),
            Some(fixture.root.join("seed"))
        );
        assert_eq!(
            fixture.app.seed_cwd_from_workspace(0),
            Some(fixture.root.join("seed"))
        );
        assert_eq!(fixture.app.workspace_creation_cwd(2), None);
    }

    #[tokio::test]
    async fn workspace_create_confirmation_tracks_source_id_after_reorder() {
        let mut fixture = CwdFixture::new();
        let source_id = fixture.app.state.workspaces[0].id.clone();
        fixture.app.state.prompt_new_workspace_name = true;
        fixture
            .app
            .begin_tui_workspace_create("test.workspace.reorder");
        fixture.call(api::Method::WorkspaceMove(api::WorkspaceMoveParams {
            workspace_id: fixture.app.public_workspace_id(0),
            insert_index: 2,
        }));
        assert_eq!(fixture.app.state.workspaces[1].id, source_id);
        fixture.app.state.switch_workspace(0);
        assert_eq!(fixture.app.state.mode, crate::app::Mode::RenameWorkspace);
        fixture.app.state.name_input = "reordered".into();
        fixture
            .app
            .handle_rename_key_via_api(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ));
        assert_eq!(fixture.app.state.workspaces.len(), 3);
        assert_eq!(fixture.cwd(2, 0), fixture.root.join("cached"));
        assert_eq!(
            fixture.app.state.workspaces[2].identity_cwd,
            fixture.root.join("cached")
        );
        assert_eq!(
            fixture.app.state.workspaces[2].custom_name.as_deref(),
            Some("reordered")
        );
    }

    #[tokio::test]
    async fn workspace_create_preserves_explicit_and_nonfollow_cwd_policies() {
        use crate::config::NewTerminalCwdConfig;
        for (policy, explicit) in [
            (NewTerminalCwdConfig::Home, false),
            (NewTerminalCwdConfig::Current, false),
            (NewTerminalCwdConfig::Path("/".into()), false),
            (NewTerminalCwdConfig::Follow, true),
            (NewTerminalCwdConfig::Home, true),
        ] {
            let mut fixture = CwdFixture::new();
            let cwd = explicit.then(|| fixture.root.join("other"));
            let expected = cwd
                .clone()
                .unwrap_or_else(|| super::resolve_new_terminal_cwd(&policy, None));
            fixture.app.state.new_terminal_cwd = policy;
            let focus = fixture.app.state.current_pane_focus_target();
            fixture.call(api::Method::WorkspaceCreate(api::WorkspaceCreateParams {
                cwd: cwd.map(|path| path.display().to_string()),
                focus: false,
                label: None,
            }));
            assert_eq!(fixture.app.state.workspaces.len(), 3);
            assert_eq!(fixture.app.state.workspaces[2].identity_cwd, expected);
            assert_eq!(fixture.cwd(2, 0), expected);
            assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
        }
    }

    #[tokio::test]
    async fn workspace_create_revalidates_invalid_cached_focus_at_creation() {
        for invalid in ["relative", "file", "deleted"] {
            let mut fixture = CwdFixture::new();
            let cwd = match invalid {
                "relative" => std::path::PathBuf::from("."),
                "file" => {
                    let path = fixture.root.join("file");
                    std::fs::write(&path, b"not a directory").unwrap();
                    path
                }
                "deleted" => {
                    let path = fixture.root.join("deleted");
                    std::fs::create_dir(&path).unwrap();
                    std::fs::remove_dir(&path).unwrap();
                    path
                }
                _ => unreachable!(),
            };
            let pane = fixture.app.state.workspaces[0].tabs[1].root_pane;
            let terminal_id = fixture.app.state.workspaces[0].tabs[1]
                .terminal_id(pane)
                .unwrap()
                .clone();
            fixture
                .app
                .state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .cwd = cwd.clone();
            assert_eq!(fixture.app.workspace_creation_cwd(0), Some(cwd.clone()));
            let expected =
                super::resolve_new_terminal_cwd(&crate::config::NewTerminalCwdConfig::Follow, None);
            assert_ne!(expected, cwd);
            fixture.call(api::Method::WorkspaceCreate(api::WorkspaceCreateParams {
                cwd: None,
                focus: false,
                label: None,
            }));
            assert_eq!(fixture.app.state.workspaces.len(), 3);
            assert_eq!(
                fixture.app.state.workspaces[2].identity_cwd, expected,
                "{invalid}"
            );
            assert_eq!(fixture.cwd(2, 0), expected, "{invalid}");
            assert!(expected.is_absolute() && expected.is_dir());
        }
    }

    #[test]
    fn authoritative_receiver_identity_is_none_while_the_hook_identity_is_provisional() {
        // Gate-3 B1 arbiter (msg_c76820d29bbb759b): the receipt gate is where a
        // provisional identity has to be invisible. The pane still shows its session —
        // the identity exists — but until the detector has seen the process after the
        // exit it captured, nothing may anchor a receipt on it, and the CLI answers
        // `receiver_identity_unverified` exactly as for a pane no hook ever named.
        let mut app = test_app();
        let workspace = Workspace::test_new("provisional-receiver");
        let pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let terminal_id = app.state.workspaces[0]
            .panes
            .get(&pane)
            .unwrap()
            .attached_terminal_id
            .clone();

        let exit_at = Instant::now();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
            std::thread::sleep(Duration::from_millis(20));
            terminal
                .set_agent_session_ref_for_session_start(
                    "zynk:hermes".into(),
                    "hermes".into(),
                    crate::agent_resume::AgentSessionRef::id("existing-session"),
                    Some(20),
                    Some("startup".into()),
                )
                .expect("initial session");
        }
        assert!(app.authoritative_receiver_identity(&pane_id).is_some());

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Hermes),
                AgentState::Idle,
                false,
                false,
                false,
                true,
                exit_at,
            );

        assert!(
            app.state.terminals[&terminal_id].hook_identity.is_some(),
            "the late exit retired an identity it was too old to retire"
        );
        assert!(
            app.authoritative_receiver_identity(&pane_id).is_none(),
            "a provisional identity was accepted as a receipt anchor"
        );

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Hermes),
                AgentState::Idle,
                false,
                false,
                false,
                false,
                exit_at + Duration::from_millis(5),
            );

        let receiver = app
            .authoritative_receiver_identity(&pane_id)
            .expect("a confirmed identity anchors a receipt again");
        assert_eq!(receiver.agent_label, "hermes");
    }

    #[tokio::test]
    async fn a_captured_exit_blocks_receipts_before_the_event_queue_accepts_it() {
        let mut app = test_app();
        let workspace = Workspace::test_new("pending-exit");
        let pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let public_id = app.public_pane_id(0, pane).unwrap();
        let terminal_id = app.state.workspaces[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminal_runtimes.insert(
            terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::id("old-session"),
                Some(10),
            )
            .expect("hook authority");
        assert!(app.authoritative_receiver_identity(&public_id).is_some());

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(crate::events::AppEvent::UpdateReady {
            version: "test".into(),
            install_command: String::new(),
        })
        .unwrap();
        let exit_at = Instant::now();
        {
            let publish = app
                .terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .test_publish_process_exit(tx, pane, Agent::Pi, exit_at);
            tokio::pin!(publish);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut publish)
                    .await
                    .is_err()
            );
            assert!(
                app.authoritative_receiver_identity(&public_id).is_none(),
                "an exit waiting for queue space must already fence receipt authority"
            );
            rx.recv().await.unwrap();
            publish.await;
            assert!(
                app.authoritative_receiver_identity(&public_id).is_none(),
                "queue acceptance is not application of the exit"
            );
        }
        // Exercise the receipt fence itself, independent of the debug-only caller seam.
        let response = app.handle_zynk_message_received(
            "pending-exit-receipt".into(),
            crate::api::schema::ZynkMessageReceivedParams {
                pane_id: public_id.clone(),
                message_id: "msg".into(),
                conversation_id: "conv".into(),
                conversation_seq: 1,
                runtime_session_id: "rt".into(),
                socket_namespace: "sock".into(),
                receiver_seq: None,
                timestamp: None,
                status: None,
                receiver_agent_session: None,
            },
        );
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["error"]["code"], "receiver_identity_unverified");
        app.handle_internal_event(rx.recv().await.unwrap());
        assert!(app.authoritative_receiver_identity(&public_id).is_none());
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id: pane,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: Instant::now(),
        });
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::id("new-session"),
                Some(11),
            )
            .expect("fresh owner");
        assert!(
            app.authoritative_receiver_identity(&public_id).is_some(),
            "applying an exit must retire its pending fence, not ban a later owner"
        );
    }
}
