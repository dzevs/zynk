use crate::api::schema::{Method, PaneTarget, TabTarget, WorkspaceTarget};

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

    pub(crate) fn runtime_workspace_close(
        &mut self,
        id: &'static str,
        workspace_id: String,
    ) -> String {
        self.dispatch_runtime_mutation(id, Method::WorkspaceClose(WorkspaceTarget { workspace_id }))
    }

    pub(crate) fn runtime_tab_focus(&mut self, id: &'static str, tab_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::TabFocus(TabTarget { tab_id }))
    }

    pub(crate) fn runtime_tab_close(&mut self, id: &'static str, tab_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::TabClose(TabTarget { tab_id }))
    }

    pub(crate) fn runtime_pane_focus(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneFocus(PaneTarget { pane_id }))
    }

    pub(crate) fn runtime_pane_close(&mut self, id: &'static str, pane_id: String) -> String {
        self.dispatch_runtime_mutation(id, Method::PaneClose(PaneTarget { pane_id }))
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
}
