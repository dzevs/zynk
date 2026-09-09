use crate::api::schema::{
    EmptyParams, LayoutSetSplitRatioParams, Method, PaneFocusDirectionParams, PaneRenameParams,
    PaneResizeParams, PaneSplitParams, PaneSwapParams, PaneTarget, PaneZoomParams, TabCreateParams,
    TabMoveParams, TabRenameParams, TabTarget, WorkspaceCreateParams, WorkspaceMoveParams,
    WorkspaceRenameParams, WorkspaceTarget, WorktreeCreateParams, WorktreeOpenParams,
    WorktreeRemoveParams,
};

use super::App;

impl App {
    pub(crate) fn dispatch_runtime_mutation(&mut self, id: &'static str, method: Method) -> String {
        self.dispatch_api_request(id, method)
    }

    pub(crate) fn dispatch_deferred_runtime_mutation(
        &mut self,
        id: &'static str,
        method: Method,
    ) -> Option<String> {
        self.dispatch_deferred_api_request(id, method)
    }

    pub(crate) fn runtime_workspace_focus(
        &mut self,
        id: &'static str,
        workspace_id: String,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceFocus(WorkspaceTarget { workspace_id }))
    }

    pub(crate) fn runtime_workspace_create(
        &mut self,
        id: &'static str,
        params: WorkspaceCreateParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceCreate(params))
    }

    pub(crate) fn runtime_workspace_rename(
        &mut self,
        id: &'static str,
        params: WorkspaceRenameParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceRename(params))
    }

    pub(crate) fn runtime_workspace_move(
        &mut self,
        id: &'static str,
        params: WorkspaceMoveParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceMove(params))
    }

    pub(crate) fn runtime_workspace_close(
        &mut self,
        id: &'static str,
        workspace_id: String,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceClose(WorkspaceTarget { workspace_id }))
    }

    pub(crate) fn runtime_tab_create(
        &mut self,
        id: &'static str,
        params: TabCreateParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::TabCreate(params))
    }

    pub(crate) fn runtime_tab_focus(&mut self, id: &'static str, tab_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::TabFocus(TabTarget { tab_id }))
    }

    pub(crate) fn runtime_tab_rename(
        &mut self,
        id: &'static str,
        params: TabRenameParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::TabRename(params))
    }

    pub(crate) fn runtime_tab_move(&mut self, id: &'static str, params: TabMoveParams) -> String {
        self.dispatch_runtime_mutation(id, Method::TabMove(params))
    }

    pub(crate) fn runtime_tab_close(&mut self, id: &'static str, tab_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::TabClose(TabTarget { tab_id }))
    }

    pub(crate) fn runtime_server_reload_config(&mut self, id: &'static str) -> String {
        self.dispatch_runtime_mutation(id, Method::ServerReloadConfig(EmptyParams::default()))
    }

    pub(crate) fn runtime_pane_focus(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneFocus(PaneTarget { pane_id }))
    }

    pub(crate) fn runtime_pane_close(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneClose(PaneTarget { pane_id }))
    }

    pub(crate) fn runtime_pane_rename(
        &mut self,
        id: &'static str,
        params: PaneRenameParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneRename(params))
    }

    pub(crate) fn runtime_pane_focus_direction(
        &mut self,
        id: &'static str,
        params: PaneFocusDirectionParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneFocusDirection(params))
    }

    pub(crate) fn runtime_pane_resize(
        &mut self,
        id: &'static str,
        params: PaneResizeParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneResize(params))
    }

    pub(crate) fn runtime_pane_swap(&mut self, id: &'static str, params: PaneSwapParams) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneSwap(params))
    }

    pub(crate) fn runtime_pane_split(
        &mut self,
        id: &'static str,
        params: PaneSplitParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneSplit(params))
    }

    pub(crate) fn runtime_pane_zoom(&mut self, id: &'static str, params: PaneZoomParams) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneZoom(params))
    }

    pub(crate) fn runtime_layout_set_split_ratio(
        &mut self,
        id: &'static str,
        params: LayoutSetSplitRatioParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::LayoutSetSplitRatio(params))
    }

    pub(crate) fn runtime_worktree_create_deferred(
        &mut self,
        id: &'static str,
        params: WorktreeCreateParams,
    ) -> Option<String> {
        self.dispatch_deferred_runtime_mutation(id, Method::WorktreeCreate(params))
    }

    pub(crate) fn runtime_worktree_open(
        &mut self,
        id: &'static str,
        params: WorktreeOpenParams,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorktreeOpen(params))
    }

    pub(crate) fn runtime_worktree_remove_deferred(
        &mut self,
        id: &'static str,
        params: WorktreeRemoveParams,
    ) -> Option<String> {
        self.dispatch_deferred_runtime_mutation(id, Method::WorktreeRemove(params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::Workspace;

    /// Pure-data App: `AppState::test_new()` (no PTYs, no channels) dropped onto an
    /// App built without a session, so the runtime-mutation dispatch below is
    /// exercised against state alone.
    fn app_with_pure_state() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state = AppState::test_new();
        app
    }

    #[test]
    fn runtime_workspace_mutations_drive_pure_state_without_a_pty() {
        let mut app = app_with_pure_state();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        assert_eq!(app.terminal_runtimes.len(), 0);

        let second = app.public_workspace_id(1);
        let focus_response = app.runtime_workspace_focus("tui.workspace.focus", second.clone());
        assert!(
            serde_json::from_str::<crate::api::schema::SuccessResponse>(&focus_response).is_ok(),
            "focus dispatch should succeed: {focus_response}"
        );
        assert_eq!(app.state.active, Some(1));

        let close_response = app.runtime_workspace_close("tui.workspace.close", second);
        assert!(
            serde_json::from_str::<crate::api::schema::SuccessResponse>(&close_response).is_ok(),
            "close dispatch should succeed: {close_response}"
        );
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
        assert_eq!(app.terminal_runtimes.len(), 0);
    }

    /// The typed pane-layout adapters must reach API dispatch straight from
    /// typed params. Everything here runs on pure `AppState`, so a PTY spawn
    /// would show up as a non-empty `terminal_runtimes`.
    #[test]
    fn runtime_pane_layout_adapters_drive_pure_state_without_a_pty() {
        let mut app = app_with_pure_state();
        let mut workspace = Workspace::test_new("layout");
        let second = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(second);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        assert_eq!(app.terminal_runtimes.len(), 0);

        let split_response = app.runtime_pane_split(
            "tui.pane.split",
            PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some("no-such-pane".into()),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: true,
            },
        );
        let split_error =
            serde_json::from_str::<crate::api::schema::ErrorResponse>(&split_response)
                .expect("split dispatch should answer with a structured error");
        assert_eq!(split_error.error.code, "pane_not_found");
        assert_eq!(app.terminal_runtimes.len(), 0);

        let zoom_response = app.runtime_pane_zoom(
            "tui.pane.zoom",
            PaneZoomParams {
                pane_id: None,
                mode: crate::api::schema::PaneZoomMode::Toggle,
            },
        );
        assert!(
            serde_json::from_str::<crate::api::schema::SuccessResponse>(&zoom_response).is_ok(),
            "zoom dispatch should succeed: {zoom_response}"
        );
        assert!(app.state.workspaces[0].tabs[0].zoomed);

        let pane_id = app.public_pane_id(0, second).expect("public pane id");
        let close_response = app.runtime_pane_close("tui.pane.close", pane_id);
        assert!(
            serde_json::from_str::<crate::api::schema::SuccessResponse>(&close_response).is_ok(),
            "close dispatch should succeed: {close_response}"
        );
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert_eq!(app.terminal_runtimes.len(), 0);
    }

    /// The worktree adapters carry typed params into the immediate and the
    /// deferred dispatch paths; an unknown workspace is answered structurally
    /// and never reaches git or a PTY.
    #[test]
    fn runtime_worktree_adapters_dispatch_typed_params_without_a_pty() {
        let mut app = app_with_pure_state();
        app.state.workspaces = vec![Workspace::test_new("a")];
        app.state.active = Some(0);
        app.state.selected = 0;

        let open_response = app.runtime_worktree_open(
            "tui.worktree.open",
            WorktreeOpenParams {
                workspace_id: Some("no-such-workspace".into()),
                cwd: None,
                path: Some("/tmp/no-such-worktree".into()),
                branch: None,
                focus: true,
                label: None,
            },
        );
        let open_error = serde_json::from_str::<crate::api::schema::ErrorResponse>(&open_response)
            .expect("worktree open should answer with a structured error");
        assert_eq!(open_error.error.code, "workspace_not_found");

        let remove_response = app
            .runtime_worktree_remove_deferred(
                "tui.worktree.remove",
                WorktreeRemoveParams {
                    workspace_id: "no-such-workspace".into(),
                    force: false,
                },
            )
            .expect("deferred worktree remove should answer immediately");
        let remove_error =
            serde_json::from_str::<crate::api::schema::ErrorResponse>(&remove_response)
                .expect("worktree remove should answer with a structured error");
        assert_eq!(remove_error.error.code, "workspace_not_found");
        assert_eq!(app.terminal_runtimes.len(), 0);
    }
}
