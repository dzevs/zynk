use crate::api::schema::{ResponseResult, SessionSnapshot};
use crate::app::App;

use super::responses::encode_success;

impl App {
    pub(super) fn handle_session_snapshot(&self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionSnapshot {
                snapshot: Box::new(self.session_snapshot()),
            },
        )
    }

    fn session_snapshot(&self) -> SessionSnapshot {
        let focused_workspace = self
            .state
            .active
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx).map(|ws| (ws_idx, ws)));
        let focused_workspace_id = focused_workspace.map(|(_, ws)| ws.id.clone());
        let focused_tab_id = focused_workspace
            .and_then(|(ws_idx, ws)| self.public_tab_id(ws_idx, ws.active_tab_index()));
        let focused_pane_id = focused_workspace
            .and_then(|(ws_idx, ws)| self.public_pane_id(ws_idx, ws.focused_pane_id()?));

        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut layouts = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            workspaces.push(self.workspace_info(ws_idx));
            for tab_idx in 0..ws.tabs.len() {
                if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                    tabs.push(tab);
                }
                if let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) {
                    layouts.push(layout);
                }
            }
        }

        SessionSnapshot {
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            workspaces,
            tabs,
            panes: self.collect_panes_for_workspace(None).unwrap_or_default(),
            layouts,
            agents: self.collect_agent_infos(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{Request, SuccessResponse};
    use crate::app::App;
    use crate::{config::Config, workspace::Workspace};
    use serde_json::{json, Value};

    fn test_app() -> App {
        App::new(
            &Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    fn snapshot(app: &mut App) -> Value {
        let value = json!({"id": "snapshot", "method": "session.snapshot", "params": {}});
        let request: Request = serde_json::from_value(value.clone())
            .expect("session.snapshot request must be supported");
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        let raw = app.handle_api_request_after_internal_events_drained(request);
        let response: SuccessResponse = serde_json::from_str(&raw).unwrap();
        let roundtrip = serde_json::to_value(response).unwrap();
        assert_eq!(roundtrip, serde_json::from_str::<Value>(&raw).unwrap());
        assert_eq!(roundtrip["id"], "snapshot");
        assert_eq!(roundtrip["result"]["type"], "session_snapshot");
        roundtrip["result"]["snapshot"].clone()
    }

    #[test]
    fn session_snapshot_empty_session_has_current_metadata_and_no_focus() {
        let mut app = test_app();
        let value = snapshot(&mut app);
        assert_eq!(value["version"], crate::build_info::version());
        assert_eq!(value["protocol"], crate::protocol::PROTOCOL_VERSION);
        for key in ["workspaces", "tabs", "panes", "layouts", "agents"] {
            assert_eq!(value[key], json!([]), "{key}");
        }
        for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
            assert!(value.get(key).is_none(), "{key}");
        }
    }

    #[test]
    fn session_snapshot_includes_inactive_tabs_and_current_focus() {
        let mut app = test_app();
        for name in ["first", "second"] {
            let mut workspace = Workspace::test_new(name);
            workspace.test_add_tab(Some("background"));
            app.state.workspaces.push(workspace);
        }
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.workspaces[1].active_tab = 1;
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 120, 40);
        let value = snapshot(&mut app);
        assert_eq!(value["workspaces"].as_array().unwrap().len(), 2);
        assert_eq!(value["tabs"].as_array().unwrap().len(), 4);
        assert_eq!(value["panes"].as_array().unwrap().len(), 4);
        assert_eq!(value["layouts"].as_array().unwrap().len(), 4);
        assert_eq!(value["focused_workspace_id"], app.public_workspace_id(1));
        assert_eq!(value["focused_tab_id"], app.public_tab_id(1, 1).unwrap());
        let pane = app.state.workspaces[1].focused_pane_id().unwrap();
        assert_eq!(
            value["focused_pane_id"],
            app.public_pane_id(1, pane).unwrap()
        );
        assert_eq!(
            value["panes"],
            serde_json::to_value(app.collect_panes_for_workspace(None).unwrap()).unwrap()
        );
        for (ws_idx, workspace) in app.state.workspaces.iter().enumerate() {
            assert_eq!(
                value["workspaces"][ws_idx],
                json!(app.workspace_info(ws_idx))
            );
            for tab_idx in 0..workspace.tabs.len() {
                assert_eq!(
                    value["tabs"][ws_idx * 2 + tab_idx],
                    json!(app.tab_info(ws_idx, tab_idx).unwrap())
                );
            }
        }
        for layout in value["layouts"].as_array().unwrap() {
            let request: Request = serde_json::from_value(json!({
                "id": "layout", "method": "pane.layout",
                "params": {"pane_id": layout["focused_pane_id"]}
            }))
            .unwrap();
            let response: Value = serde_json::from_str(&app.handle_api_request(request)).unwrap();
            assert_eq!(&response["result"]["layout"], layout);
        }
    }

    #[test]
    fn session_snapshot_missing_active_owner_does_not_invent_focus() {
        let mut app = test_app();
        app.state.workspaces.push(Workspace::test_new("unfocused"));
        app.state.ensure_test_terminals();
        for active in [None, Some(usize::MAX)] {
            app.state.active = active;
            let value = snapshot(&mut app);
            assert_eq!(value["workspaces"].as_array().unwrap().len(), 1);
            for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
                assert!(value.get(key).is_none(), "{key}");
            }
        }
    }

    #[tokio::test]
    async fn session_snapshot_preserves_identity_runtime_and_event_state() {
        let mut app = test_app();
        app.state.workspaces.push(Workspace::test_new("observed"));
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let pane_id = app.state.workspaces[0].focused_pane_id().unwrap();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        let identity = (
            terminal.revision,
            terminal.hook_authority.clone(),
            terminal.hook_identity.clone(),
            terminal.persisted_agent_session.clone(),
        );
        let (runtime, mut bytes) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        app.state.session_dirty = false;
        let agents = json!(app.collect_agent_infos());
        let sequence = app.event_hub.current_sequence();
        let value = snapshot(&mut app);
        assert_eq!(value["agents"], agents);
        assert_eq!(value["agents"].as_array().unwrap().len(), 1);
        assert_eq!(value["agents"][0]["agent"], "claude");
        assert!(value["agents"][0].get("agent_session").is_none());
        assert!(value["panes"][0].get("agent_session").is_none());
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            identity,
            (
                terminal.revision,
                terminal.hook_authority.clone(),
                terminal.hook_identity.clone(),
                terminal.persisted_agent_session.clone(),
            )
        );
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));
        assert!(!app.state.session_dirty);
        assert!(bytes.try_recv().is_err());
        assert_eq!(app.event_hub.current_sequence(), sequence);
        assert!(app.event_hub.events_after(sequence).is_empty());
        assert_eq!(snapshot(&mut app), value);
    }
}
