// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use bytes::Bytes;

use crate::api::schema::{
    AgentRenameParams, AgentSendParams, AgentStartParams, AgentTarget, PaneReadResult, ReadFormat,
    ReadSource, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.agent_info_for_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let (agent, argv) = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(super) fn handle_agent_read(
        &mut self,
        id: String,
        params: crate::api::schema::AgentReadParams,
    ) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
        let text = match params.format {
            ReadFormat::Text => match params.source {
                ReadSource::Visible => pane.visible_text(),
                ReadSource::Recent => pane.recent_text(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_text(requested_lines),
                ReadSource::Detection => pane.detection_text(),
            },
            ReadFormat::Ansi => match params.source {
                ReadSource::Visible => pane.visible_ansi(),
                ReadSource::Recent => pane.recent_ansi(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_ansi(requested_lines),
                ReadSource::Detection => pane.detection_text(),
            },
        };

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text,
                    revision: 0,
                    truncated: false,
                },
            },
        )
    }

    pub(super) fn handle_agent_explain(&mut self, id: String, target: AgentTarget) -> String {
        let resolved = match self.resolve_terminal_target(&target.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, _workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return agent_not_found(id, &target.target);
        };
        if terminal.full_lifecycle_hook_authority_active() {
            let explain = serde_json::json!({
                "agent": terminal.effective_agent_label().unwrap_or("unknown"),
                "state": crate::detect::manifest::agent_state_label(terminal.state),
                "manifest_source": null,
                "manifest_version": null,
                "cached_remote_version": null,
                "local_override_shadowing_remote": false,
                "remote_update_status": null,
                "remote_update_error": null,
                "matched_rule": null,
                "visible_idle": false,
                "visible_blocker": false,
                "visible_working": false,
                "screen_detection_skipped": true,
                "screen_detection_skip_reason": "full_lifecycle_hook_authority",
                "skip_state_update": false,
                "skipped_update_reason": null,
                "fallback_reason": null,
                "warning": null,
                "evaluated_rules": [],
            });
            return encode_success(id, ResponseResult::AgentExplain { explain });
        }
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return encode_error(
                id,
                "agent_explain_unavailable",
                format!(
                    "agent target {} does not have a detected agent label",
                    target.target
                ),
            );
        };

        let screen = pane.detection_text();
        let unwrapped_tail = pane.detection_unwrapped_text();
        let osc_title = pane.agent_osc_title();
        let osc_progress = pane.agent_osc_progress();
        let explain = crate::detect::manifest::explain_with_input(
            agent,
            crate::detect::manifest::DetectionInput {
                screen: &screen,
                unwrapped_tail: Some(&unwrapped_tail),
                osc_title: &osc_title,
                osc_progress: &osc_progress,
            },
        );
        let value = crate::detect::manifest::explain_to_json_value(&explain);

        encode_success(id, ResponseResult::AgentExplain { explain: value })
    }

    pub(super) fn handle_agent_send(&mut self, id: String, params: AgentSendParams) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
            return encode_error(id, "agent_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{AgentStatus, ErrorResponse, Method, PaneTarget, Request, SuccessResponse},
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn m823_app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.outer_terminal_focus = Some(false);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app
    }

    #[test]
    fn agent_focus_marks_already_focused_done_agent_seen() {
        let mut app = m823_app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public = app.public_pane_id(0, pane_id).unwrap();
        let before = app.agent_info_for_target("pi").unwrap();
        assert_eq!(before.agent_status, AgentStatus::Done);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));

        let response = app.handle_agent_focus(
            "req".into(),
            AgentTarget {
                target: "pi".into(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(success.id, "req");
        assert_eq!(agent.pane_id, public);
        assert_eq!(agent.agent_status, AgentStatus::Idle);
        let pane = &app.state.workspaces[0].tabs[0].panes[&pane_id];
        assert!(pane.seen);
        assert_eq!(
            app.state.terminals[&pane.attached_terminal_id].state,
            AgentState::Idle
        );
        assert_eq!(app.state.outer_terminal_focus, Some(false));
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));
        assert!(app.event_hub.events_after(0).is_empty());
    }

    fn m823_assert_background_focus_marks_only_target_tab(agent_focus: bool) {
        let mut app = m823_app_with_agent();
        app.state.workspaces.push(Workspace::test_new("target"));
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let sibling = app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        let other_tab = app.state.workspaces[1].test_add_tab(Some("unseen-other-tab"));
        app.state.ensure_test_terminals();
        let target_terminal = app.state.workspaces[1].tabs[0].panes[&target]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&target_terminal)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        for ws in &mut app.state.workspaces {
            for tab in &mut ws.tabs {
                for pane in tab.panes.values_mut() {
                    pane.seen = false;
                }
            }
        }
        let prior = app.state.workspaces[0].tabs[0].root_pane;
        let public = app.public_pane_id(1, target).unwrap();
        assert_eq!(app.state.active, Some(0));
        let method = if agent_focus {
            Method::AgentFocus(AgentTarget {
                target: public.clone(),
            })
        } else {
            Method::PaneFocus(PaneTarget {
                pane_id: public.clone(),
            })
        };

        let response = app.handle_api_request(Request {
            id: "background-focus".into(),
            method,
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let (response_pane, status) = match success.result {
            ResponseResult::AgentInfo { agent } if agent_focus => {
                (agent.pane_id, agent.agent_status)
            }
            ResponseResult::PaneInfo { pane } if !agent_focus => (pane.pane_id, pane.agent_status),
            _ => panic!("expected matching focus response"),
        };
        assert_eq!(success.id, "background-focus");
        assert_eq!(response_pane, public);
        assert_eq!(status, AgentStatus::Idle);
        assert!(
            !app.state.workspaces[0].tabs[0].panes[&prior].seen,
            "prior workspace must remain unseen"
        );
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target));
        assert!(app.state.workspaces[1].tabs[0].panes[&target].seen);
        assert!(app.state.workspaces[1].tabs[0].panes[&sibling].seen);
        assert!(app.state.workspaces[1].tabs[other_tab]
            .panes
            .values()
            .all(|pane| !pane.seen));
        assert_eq!(
            app.state.terminals[&target_terminal].state,
            AgentState::Idle
        );
        assert_eq!(app.state.outer_terminal_focus, Some(false));
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.event_hub.events_after(0).is_empty());
    }

    #[test]
    fn m823_agent_focus_marks_target_tab_without_marking_prior_workspace() {
        m823_assert_background_focus_marks_only_target_tab(true);
    }

    #[test]
    fn m823_pane_focus_marks_target_tab_without_marking_prior_workspace() {
        m823_assert_background_focus_marks_only_target_tab(false);
    }

    #[test]
    fn m823_invalid_focus_target_preserves_attention_and_focus() {
        for agent_focus in [true, false] {
            let mut app = m823_app_with_agent();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let method = if agent_focus {
                Method::AgentFocus(AgentTarget {
                    target: "missing-focus-target".into(),
                })
            } else {
                Method::PaneFocus(PaneTarget {
                    pane_id: "missing-focus-target".into(),
                })
            };
            let response = app.handle_api_request(Request {
                id: "invalid-focus".into(),
                method,
            });
            let error: ErrorResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(error.id, "invalid-focus");
            assert_eq!(
                error.error.code,
                if agent_focus {
                    "agent_not_found"
                } else {
                    "pane_not_found"
                }
            );
            assert_eq!(app.state.active, Some(0));
            assert_eq!(app.state.selected, 0);
            assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));
            assert!(!app.state.workspaces[0].tabs[0].panes[&pane_id].seen);
            assert_eq!(app.state.outer_terminal_focus, Some(false));
            assert!(app.event_hub.events_after(0).is_empty());
        }
    }
}
