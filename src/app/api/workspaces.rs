// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, WorkspaceCreateParams,
    WorkspaceMoveBlockParams, WorkspaceMoveParams, WorkspaceRenameParams,
    WorkspaceReportMetadataParams, WorkspaceTarget,
};
use crate::app::App;

use super::super::api_helpers::{normalize_metadata_source, normalize_metadata_ttl};
use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_workspace_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::WorkspaceList {
                workspaces: self.workspace_list_info(),
            },
        )
    }

    pub(super) fn handle_workspace_get(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        let Some(_) = self.state.workspaces.get(index) else {
            return workspace_not_found(id, &target.workspace_id);
        };

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_create(
        &mut self,
        id: String,
        params: WorkspaceCreateParams,
    ) -> String {
        let cwd = params.cwd.map(PathBuf::from).unwrap_or_else(|| {
            let follow_cwd = self
                .workspace_creation_source()
                .and_then(|ws_idx| self.workspace_creation_cwd(ws_idx));
            self.resolve_new_terminal_cwd(follow_cwd)
        });
        match self.create_workspace_with_options(cwd, params.focus) {
            Ok(index) => {
                if let Some(label) = params.label {
                    if let Some(workspace) = self.state.workspaces.get_mut(index) {
                        workspace.set_custom_name(label);
                        crate::logging::workspace_renamed(&workspace.id);
                    }
                }
                self.emit_workspace_open_events(index);
                encode_success(
                    id,
                    self.workspace_created_result(index)
                        .expect("new workspace should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "workspace_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_workspace_focus(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &target.workspace_id);
        }
        self.state.switch_workspace(index);

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_rename(
        &mut self,
        id: String,
        params: WorkspaceRenameParams,
    ) -> String {
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        let Some(ws) = self.state.workspaces.get_mut(index) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        ws.set_custom_name(params.label.clone());
        crate::logging::workspace_renamed(&ws.id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceRenamed,
            data: EventData::WorkspaceRenamed {
                workspace_id: self.public_workspace_id(index),
                label: params.label,
            },
        });

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_move(
        &mut self,
        id: String,
        params: WorkspaceMoveParams,
    ) -> String {
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &params.workspace_id);
        }
        if params.insert_index > self.state.workspaces.len() {
            return encode_error(
                id,
                "workspace_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let workspace_id = self.public_workspace_id(index);
        let insert_index = params.insert_index;
        let moved = self.state.move_workspace(index, insert_index);
        let workspaces = self.workspace_list_info();
        if moved {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceMoved,
                data: EventData::WorkspaceMoved {
                    workspace_id,
                    insert_index,
                    workspaces: workspaces.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::WorkspaceList { workspaces })
    }

    pub(super) fn handle_workspace_move_block(
        &mut self,
        id: String,
        params: WorkspaceMoveBlockParams,
    ) -> String {
        if params.workspace_ids.is_empty() {
            return encode_error(
                id,
                "workspace_move_block_failed",
                "workspace_ids must not be empty",
            );
        }

        let mut workspace_ids = Vec::with_capacity(params.workspace_ids.len());
        let mut seen_ids = std::collections::HashSet::new();
        for requested_id in &params.workspace_ids {
            let Some(index) = self.parse_workspace_id(requested_id) else {
                return workspace_not_found(id, requested_id);
            };
            let Some(workspace) = self.state.workspaces.get(index) else {
                return workspace_not_found(id, requested_id);
            };
            if !seen_ids.insert(workspace.id.clone()) {
                return encode_error(
                    id,
                    "workspace_move_block_failed",
                    format!("workspace {requested_id} appears more than once"),
                );
            }
            workspace_ids.push(workspace.id.clone());
        }

        let before_workspace_id = match params.before_workspace_id {
            Some(requested_id) => {
                let Some(index) = self.parse_workspace_id(&requested_id) else {
                    return workspace_not_found(id, &requested_id);
                };
                let Some(workspace) = self.state.workspaces.get(index) else {
                    return workspace_not_found(id, &requested_id);
                };
                if seen_ids.contains(&workspace.id) {
                    return encode_error(
                        id,
                        "workspace_move_block_failed",
                        "before_workspace_id must not be part of workspace_ids",
                    );
                }
                Some(workspace.id.clone())
            }
            None => None,
        };

        let moved = self
            .state
            .move_workspace_block(&workspace_ids, before_workspace_id.as_deref());
        let workspaces = self.workspace_list_info();
        if moved {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceReordered,
                data: EventData::WorkspaceReordered {
                    workspace_ids,
                    before_workspace_id,
                    workspaces: workspaces.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::WorkspaceList { workspaces })
    }

    pub(super) fn handle_workspace_report_metadata(
        &mut self,
        id: String,
        params: WorkspaceReportMetadataParams,
    ) -> String {
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        let source = match normalize_metadata_source(params.source) {
            Ok(source) => source,
            Err(message) => return encode_error(id, "invalid_metadata_source", message),
        };
        let ttl = match normalize_metadata_ttl(params.ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return encode_error(id, "invalid_metadata_ttl", message),
        };
        let tokens = match super::super::api_helpers::normalize_metadata_tokens(params.tokens) {
            Ok(tokens) => tokens,
            Err(message) => return encode_error(id, "invalid_metadata_token", message),
        };
        let Some(workspace) = self.state.workspaces.get_mut(index) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        if !crate::metadata_tokens::sequence_is_fresh(
            &workspace.metadata_token_sequences,
            &source,
            params.seq,
        ) {
            return encode_success(id, ResponseResult::Ok {});
        }
        if workspace.metadata_tokens.key_count_after_patch(&tokens)
            > super::super::api_helpers::MAX_METADATA_TOKEN_KEYS_PER_RESOURCE
        {
            return encode_error(
                id,
                "metadata_token_limit",
                format!(
                    "workspace metadata may contain at most {} tokens",
                    super::super::api_helpers::MAX_METADATA_TOKEN_KEYS_PER_RESOURCE
                ),
            );
        }
        match crate::metadata_tokens::accept_sequence(
            &mut workspace.metadata_token_sequences,
            &source,
            params.seq,
        ) {
            Ok(true) => {}
            Ok(false) => return encode_success(id, ResponseResult::Ok {}),
            Err(()) => {
                return encode_error(
                    id,
                    "metadata_sequence_source_limit",
                    format!(
                        "workspace metadata may track at most {} sequenced sources",
                        crate::metadata_tokens::MAX_SEQUENCE_SOURCES
                    ),
                );
            }
        }
        let changed = workspace
            .metadata_tokens
            .patch(tokens, ttl, std::time::Instant::now());
        if changed {
            self.sync_agent_metadata_deadline();
            self.emit_workspace_token_updated(index);
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_workspace_close(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &target.workspace_id);
        }
        let workspace_id = self.public_workspace_id(index);
        self.state.selected = index;
        self.state.close_selected_workspace();
        self.shutdown_detached_terminal_runtimes();
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceClosed,
            data: EventData::WorkspaceClosed { workspace_id },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    fn workspace_list_info(&self) -> Vec<crate::api::schema::WorkspaceInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .map(|(idx, _)| self.workspace_info(idx))
            .collect()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::schema::SuccessResponse, config::Config, workspace::Workspace};

    fn m828a_app() -> App {
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("active"), Workspace::test_new("target")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.next_resize_poll = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        app
    }

    fn m828a_dispatch(app: &mut App, method: &str, params: serde_json::Value) -> serde_json::Value {
        let request = serde_json::from_value::<crate::api::schema::Request>(serde_json::json!({
            "id": "m828a-request", "method": method, "params": params
        }));
        assert!(request.is_ok(), "{method} JSON refused: {request:?}");
        serde_json::from_str(&app.handle_api_request(request.unwrap())).unwrap()
    }

    fn m828a_params(app: &App, tokens: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"workspace_id": app.public_workspace_id(1), "source": "user:build", "tokens": tokens})
    }

    fn m828a_report(app: &mut App, params: serde_json::Value) -> serde_json::Value {
        m828a_dispatch(app, "workspace.report_metadata", params)
    }

    fn m828a_ok(response: serde_json::Value) {
        assert_eq!(
            response,
            serde_json::json!({"id": "m828a-request", "result": {"type": "ok"}})
        );
    }

    fn m828a_tokens(app: &App, index: usize) -> serde_json::Value {
        serde_json::to_value(app.workspace_info(index))
            .unwrap()
            .get("tokens")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}))
    }

    #[test]
    fn workspace_metadata_tokens_patch_clear_and_emit_snapshot() {
        let mut app = m828a_app();
        let start = app.event_hub.current_sequence();
        let mut expected = Vec::new();
        for patch in [
            serde_json::json!({"branch": "main"}),
            serde_json::json!({"build": "ok"}),
            serde_json::json!({"branch": null}),
        ] {
            let params = m828a_params(&app, patch);
            m828a_ok(m828a_report(&mut app, params));
            expected.push(serde_json::json!({
                "event": "workspace_metadata_updated", "data": {
                    "type": "workspace_metadata_updated", "workspace": app.workspace_info(1)
                }
            }));
        }
        assert_eq!(
            expected[0]["data"]["workspace"]["tokens"],
            serde_json::json!({"branch": "main"})
        );
        assert_eq!(
            expected[1]["data"]["workspace"]["tokens"],
            serde_json::json!({"branch": "main", "build": "ok"})
        );
        assert_eq!(m828a_tokens(&app, 1), serde_json::json!({"build": "ok"}));
        assert_eq!(m828a_tokens(&app, 0), serde_json::json!({}));
        assert_eq!(app.state.active, Some(0));
        let events = app
            .event_hub
            .events_after(start)
            .into_iter()
            .map(|(_, event)| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events, expected);
        let no_event = app.event_hub.current_sequence();
        let mut params = m828a_params(&app, serde_json::json!({"build": "ok"}));
        params["seq"] = serde_json::json!(5);
        m828a_ok(m828a_report(&mut app, params.clone()));
        params["tokens"] = serde_json::json!({"build": "stale"});
        m828a_ok(m828a_report(&mut app, params));
        assert_eq!(m828a_tokens(&app, 1), serde_json::json!({"build": "ok"}));
        assert!(app.event_hub.events_after(no_event).is_empty());
        let target = app.public_workspace_id(1);
        let get = m828a_dispatch(
            &mut app,
            "workspace.get",
            serde_json::json!({"workspace_id": target}),
        );
        assert_eq!(
            get["result"]["workspace"]["tokens"],
            serde_json::json!({"build": "ok"})
        );
        let list = m828a_dispatch(&mut app, "workspace.list", serde_json::json!({}));
        assert_eq!(
            list["result"]["workspaces"][1]["tokens"],
            get["result"]["workspace"]["tokens"]
        );
    }

    #[test]
    fn m828a_workspace_metadata_validation_is_atomic() {
        let mut app = m828a_app();
        let mut seed = m828a_params(&app, serde_json::json!({"kept": "original"}));
        seed["seq"] = serde_json::json!(1);
        m828a_ok(m828a_report(&mut app, seed));
        let seventeen = (0..17)
            .map(|i| (format!("k{i}"), serde_json::json!("x")))
            .collect::<serde_json::Map<_, _>>();
        let invalid = [
            ("source", serde_json::json!(" "), "invalid_metadata_source"),
            (
                "source",
                serde_json::json!("s".repeat(81)),
                "invalid_metadata_source",
            ),
            (
                "source",
                serde_json::json!("bad/source"),
                "invalid_metadata_source",
            ),
            (
                "source",
                serde_json::json!("u\u{e9}"),
                "invalid_metadata_source",
            ),
            ("ttl_ms", serde_json::json!(0), "invalid_metadata_ttl"),
            (
                "ttl_ms",
                serde_json::json!(86_400_001),
                "invalid_metadata_ttl",
            ),
            ("tokens", serde_json::json!({}), "invalid_metadata_token"),
            (
                "tokens",
                serde_json::json!(seventeen),
                "invalid_metadata_token",
            ),
            (
                "tokens",
                serde_json::json!({"": "x"}),
                "invalid_metadata_token",
            ),
            (
                "tokens",
                serde_json::json!({"a.b": "x"}),
                "invalid_metadata_token",
            ),
            (
                "tokens",
                serde_json::json!({"k".repeat(33): "x"}),
                "invalid_metadata_token",
            ),
            (
                "tokens",
                serde_json::json!({"\u{e9}": "x", "kept": "must not apply"}),
                "invalid_metadata_token",
            ),
        ];
        for (index, (field, value, code)) in invalid.into_iter().enumerate() {
            let before = m828a_tokens(&app, 1);
            let sequence = app.event_hub.current_sequence();
            let mut params = m828a_params(&app, serde_json::json!({"kept": "must not apply"}));
            params["seq"] = serde_json::json!(index as u64 + 2);
            params[field] = value;
            let response = m828a_report(&mut app, params);
            assert_eq!(response["error"]["code"], code, "row {index}: {response}");
            assert_eq!(m828a_tokens(&app, 1), before, "row {index}");
            assert!(
                app.event_hub.events_after(sequence).is_empty(),
                "row {index}"
            );
            let mut retry =
                m828a_params(&app, serde_json::json!({"kept": format!("retry-{index}")}));
            retry["seq"] = serde_json::json!(index as u64 + 2);
            m828a_ok(m828a_report(&mut app, retry));
            assert_eq!(m828a_tokens(&app, 1)["kept"], format!("retry-{index}"));
        }
        let mut stale_invalid = m828a_params(&app, serde_json::json!({}));
        stale_invalid["seq"] = serde_json::json!(0);
        assert_eq!(
            m828a_report(&mut app, stale_invalid)["error"]["code"],
            "invalid_metadata_token"
        );
        let sixteen = (0..16)
            .map(|i| (format!("legal-{i}"), serde_json::json!("x")))
            .collect::<serde_json::Map<_, _>>();
        let mut params = m828a_params(&app, serde_json::json!(sixteen));
        params["source"] = serde_json::json!("s".repeat(80));
        params["ttl_ms"] = serde_json::json!(86_400_000);
        m828a_ok(m828a_report(&mut app, params));
        let params = m828a_params(
            &app,
            serde_json::json!({
                "k".repeat(32): "value", "unicode": "\u{754c}".repeat(81),
                "sanitized": "  hi\u{0}\tthere  ", "cleared": "\u{0}\n "
            }),
        );
        m828a_ok(m828a_report(&mut app, params));
        let tokens = m828a_tokens(&app, 1);
        assert_eq!(tokens["k".repeat(32)], "value");
        assert_eq!(tokens["unicode"], "\u{754c}".repeat(80));
        assert_eq!(tokens["sanitized"], "hithere");
        assert!(tokens.get("cleared").is_none());
        let mut params = m828a_params(&app, serde_json::json!({"lower": "accepted"}));
        params["ttl_ms"] = serde_json::json!(1);
        m828a_ok(m828a_report(&mut app, params));
        assert_eq!(m828a_tokens(&app, 1)["lower"], "accepted");
    }

    #[test]
    fn m828a_workspace_token_resource_limit_counts_the_net_patch() {
        let mut app = m828a_app();
        for batch in 0..2 {
            let tokens = (batch * 16..(batch + 1) * 16)
                .map(|i| (format!("k{i:02}"), serde_json::json!("x")))
                .collect::<serde_json::Map<_, _>>();
            let mut params = m828a_params(&app, serde_json::json!(tokens));
            params["seq"] = serde_json::json!(batch + 1);
            m828a_ok(m828a_report(&mut app, params));
        }
        assert_eq!(m828a_tokens(&app, 1).as_object().unwrap().len(), 32);
        let before = m828a_tokens(&app, 1);
        let start = app.event_hub.current_sequence();
        let mut params = m828a_params(&app, serde_json::json!({"extra": "x"}));
        params["seq"] = serde_json::json!(3);
        assert_eq!(
            m828a_report(&mut app, params.clone())["error"]["code"],
            "metadata_token_limit"
        );
        assert_eq!(m828a_tokens(&app, 1), before);
        assert!(app.event_hub.events_after(start).is_empty());
        params["seq"] = serde_json::json!(2);
        m828a_ok(m828a_report(&mut app, params.clone()));
        assert_eq!(m828a_tokens(&app, 1), before);
        params["seq"] = serde_json::json!(3);
        params["tokens"] = serde_json::json!({"extra": "x", "k31": null});
        m828a_ok(m828a_report(&mut app, params));
        let values = m828a_tokens(&app, 1);
        assert_eq!(values.as_object().unwrap().len(), 32);
        assert_eq!(values["extra"], "x");
        assert!(values.get("k31").is_none());
    }

    #[test]
    fn m828a_workspace_sequences_are_per_source_and_survive_clear() {
        let mut app = m828a_app();
        let mut params = m828a_params(&app, serde_json::json!({"key": "first"}));
        params["seq"] = serde_json::json!(0);
        m828a_ok(m828a_report(&mut app, params.clone()));
        assert_eq!(m828a_tokens(&app, 1)["key"], "first");
        let start = app.event_hub.current_sequence();
        params["tokens"] = serde_json::json!({"key": "repeat"});
        m828a_ok(m828a_report(&mut app, params.clone()));
        assert_eq!(m828a_tokens(&app, 1)["key"], "first");
        assert!(app.event_hub.events_after(start).is_empty());
        params["seq"] = serde_json::json!(2);
        params["tokens"] = serde_json::json!({"key": "newer"});
        m828a_ok(m828a_report(&mut app, params.clone()));
        params["seq"] = serde_json::json!(1);
        params["tokens"] = serde_json::json!({"key": "older"});
        m828a_ok(m828a_report(&mut app, params.clone()));
        assert_eq!(m828a_tokens(&app, 1)["key"], "newer");
        params["source"] = serde_json::json!("user:other");
        params["seq"] = serde_json::json!(0);
        params["tokens"] = serde_json::json!({"key": "other"});
        m828a_ok(m828a_report(&mut app, params.clone()));
        assert_eq!(m828a_tokens(&app, 1)["key"], "other");
        params["source"] = serde_json::json!("user:build");
        params.as_object_mut().unwrap().remove("seq");
        params["tokens"] = serde_json::json!({"key": "unsequenced"});
        m828a_ok(m828a_report(&mut app, params.clone()));
        params["seq"] = serde_json::json!(1);
        params["tokens"] = serde_json::json!({"key": "stale after unsequenced"});
        m828a_ok(m828a_report(&mut app, params.clone()));
        assert_eq!(m828a_tokens(&app, 1)["key"], "unsequenced");
        for index in 0..30 {
            params["source"] = serde_json::json!(format!("producer:{index}"));
            params["seq"] = serde_json::json!(0);
            params["tokens"] = serde_json::json!({"absent": null});
            m828a_ok(m828a_report(&mut app, params.clone()));
        }
        params["source"] = serde_json::json!("overflow");
        params["tokens"] = serde_json::json!({"extra": "unsequenced"});
        params.as_object_mut().unwrap().remove("seq");
        m828a_ok(m828a_report(&mut app, params.clone()));
        params["seq"] = serde_json::json!(0);
        assert_eq!(
            m828a_report(&mut app, params.clone())["error"]["code"],
            "metadata_sequence_source_limit"
        );
        let mut clear = m828a_params(&app, serde_json::json!({"key": null, "extra": null}));
        clear["seq"] = serde_json::json!(3);
        m828a_ok(m828a_report(&mut app, clear));
        assert_eq!(m828a_tokens(&app, 1), serde_json::json!({}));
        assert_eq!(
            m828a_report(&mut app, params.clone())["error"]["code"],
            "metadata_sequence_source_limit"
        );
        let mut due = m828a_params(&app, serde_json::json!({"due": "x"}));
        due["seq"] = serde_json::json!(4);
        due["ttl_ms"] = serde_json::json!(50);
        m828a_ok(m828a_report(&mut app, due));
        let deadline = app.agent_metadata_deadline.expect("workspace deadline");
        app.handle_scheduled_tasks(deadline, false);
        assert_eq!(m828a_tokens(&app, 1), serde_json::json!({}));
        assert_eq!(
            m828a_report(&mut app, params)["error"]["code"],
            "metadata_sequence_source_limit"
        );
        let mut fresh = m828a_params(&app, serde_json::json!({"key": "still accepted"}));
        fresh["seq"] = serde_json::json!(5);
        m828a_ok(m828a_report(&mut app, fresh));
        assert_eq!(m828a_tokens(&app, 1)["key"], "still accepted");
    }

    #[test]
    fn m828a_workspace_report_is_presentation_only_and_follows_workspace_identity() {
        let mut app = m828a_app();
        let pane = app.state.workspaces[1].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[1]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority(
                "zynk:pi".into(),
                "pi".into(),
                crate::detect::AgentState::Working,
                None,
                None,
            );
        let authority = app.state.terminals[&terminal_id].hook_authority.clone();
        let identity = app.state.terminals[&terminal_id].hook_identity.clone();
        let persisted = app.state.terminals[&terminal_id]
            .persisted_agent_session
            .clone();
        let snapshot = |app: &App| {
            serde_json::to_value(crate::persist::capture(
                &app.state.workspaces,
                &app.state.terminals,
                &app.terminal_runtimes,
                app.state.active,
                app.state.selected,
                app.state.sidebar_width,
                app.state.sidebar_section_split,
                app.state.collapsed_space_keys.clone(),
            ))
            .unwrap()
        };
        let before = snapshot(&app);
        let workspace_id = app.public_workspace_id(1);
        let params = m828a_params(&app, serde_json::json!({"agent": "not-an-identity"}));
        m828a_ok(m828a_report(&mut app, params));
        assert_eq!(app.state.terminals[&terminal_id].hook_authority, authority);
        assert_eq!(app.state.terminals[&terminal_id].hook_identity, identity);
        assert_eq!(
            app.state.terminals[&terminal_id].persisted_agent_session,
            persisted
        );
        assert_eq!(snapshot(&app), before);
        assert_eq!(app.state.active, Some(0));
        m828a_dispatch(
            &mut app,
            "workspace.move",
            serde_json::json!({"workspace_id": workspace_id, "insert_index": 0}),
        );
        let get = m828a_dispatch(
            &mut app,
            "workspace.get",
            serde_json::json!({"workspace_id": workspace_id}),
        );
        assert_eq!(get["result"]["workspace"]["workspace_id"], workspace_id);
        assert_eq!(get["result"]["workspace"]["number"], 1);
        assert_eq!(
            get["result"]["workspace"]["tokens"],
            serde_json::json!({"agent": "not-an-identity"})
        );
        assert_eq!(m828a_tokens(&app, 1), serde_json::json!({}));
    }

    #[test]
    fn workspace_token_ttl_expires_through_runtime_and_emits_update() {
        use std::time::Duration;

        let mut app = m828a_app();
        let mut first = m828a_params(&app, serde_json::json!({"first": "due"}));
        first["ttl_ms"] = 1000.into();
        first["seq"] = 3.into();
        m828a_ok(m828a_report(&mut app, first));
        let first_deadline = app.state.workspaces[1]
            .metadata_tokens
            .next_expiry()
            .unwrap();
        assert_eq!(app.agent_metadata_deadline, Some(first_deadline));

        let mut second = m828a_params(&app, serde_json::json!({"second": "also due"}));
        second["workspace_id"] = app.public_workspace_id(0).into();
        second["ttl_ms"] = 2000.into();
        second["seq"] = 7.into();
        m828a_ok(m828a_report(&mut app, second));
        let second_deadline = app.state.workspaces[0]
            .metadata_tokens
            .next_expiry()
            .unwrap();
        assert!(second_deadline > first_deadline);
        assert_eq!(app.agent_metadata_deadline, Some(first_deadline));
        let start = app.event_hub.current_sequence();
        assert!(!app.expire_due_metadata(first_deadline - Duration::from_nanos(1)));
        assert_eq!(m828a_tokens(&app, 1), serde_json::json!({"first": "due"}));
        assert_eq!(
            m828a_tokens(&app, 0),
            serde_json::json!({"second": "also due"})
        );
        assert!(app.event_hub.events_after(start).is_empty());

        let now = second_deadline + Duration::from_nanos(1);
        app.expire_metadata_at(first_deadline, now);
        assert_eq!(m828a_tokens(&app, 0), serde_json::json!({}));
        assert_eq!(m828a_tokens(&app, 1), serde_json::json!({}));
        assert_eq!(app.agent_metadata_deadline, None);
        assert_eq!(
            app.state.workspaces[0].metadata_token_sequences["user:build"],
            7
        );
        assert_eq!(
            app.state.workspaces[1].metadata_token_sequences["user:build"],
            3
        );
        let events = app
            .event_hub
            .events_after(start)
            .into_iter()
            .map(|(_, event)| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>();
        let expected = (0..2)
            .map(|index| {
                serde_json::json!({
                    "event": "workspace_metadata_updated", "data": {
                        "type": "workspace_metadata_updated", "workspace": app.workspace_info(index)
                    }
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(events, expected);
        let after = app.event_hub.current_sequence();
        assert!(!app.expire_due_metadata(now));
        app.expire_metadata_at(first_deadline, now);
        assert!(app.event_hub.events_after(after).is_empty());
    }

    fn app_with_linked_worktree() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("issue")];
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "zynk".into(),
            repo_root: "/repo/zynk".into(),
            checkout_path: "/repo/zynk-issue".into(),
            is_linked_worktree: true,
        });
        app
    }

    #[test]
    fn api_workspace_close_closes_linked_worktree_workspace_only() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_workspace_close(
            "req".into(),
            WorkspaceTarget {
                workspace_id: app.state.workspaces[0].id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(app.state.request_remove_linked_worktree, None);
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn api_workspace_move_reorders_workspaces() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_id = app.public_workspace_id(0);

        let response = app.handle_workspace_move(
            "req".into(),
            WorkspaceMoveParams {
                workspace_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(workspaces[2].workspace_id, moved_id);
        assert_eq!(app.state.workspaces[2].display_name(), "one");
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceMoved {
                    workspace_id,
                    insert_index: 3,
                    workspaces,
                } if workspace_id == &moved_id
                    && workspaces[2].workspace_id == moved_id
            )
        }));
    }

    #[test]
    fn api_workspace_move_block_reorders_atomically() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![
            Workspace::test_new("child"),
            Workspace::test_new("normal"),
            Workspace::test_new("parent"),
            Workspace::test_new("tail"),
        ];
        let parent_id = app.public_workspace_id(2);
        let child_id = app.public_workspace_id(0);
        let tail_id = app.public_workspace_id(3);

        let response = app.handle_workspace_move_block(
            "req".into(),
            WorkspaceMoveBlockParams {
                workspace_ids: vec![parent_id.clone(), child_id.clone()],
                before_workspace_id: Some(tail_id.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "parent", "child", "tail"]
        );
        assert_eq!(workspaces[1].workspace_id, parent_id);
        assert_eq!(workspaces[2].workspace_id, child_id);
        let events = event_hub.events_after(0);
        assert!(matches!(
            &events[0].1.data,
            EventData::WorkspaceReordered {
                workspace_ids,
                before_workspace_id,
                workspaces,
            } if workspace_ids == &[parent_id.clone(), child_id]
                && before_workspace_id.as_deref() == Some(tail_id.as_str())
                && workspaces[1].workspace_id == parent_id
        ));
    }

    #[test]
    fn api_workspace_move_noop_does_not_emit_event() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        let moved_id = app.public_workspace_id(0);

        let response = app.handle_workspace_move(
            "req".into(),
            WorkspaceMoveParams {
                workspace_id: moved_id.clone(),
                insert_index: 1,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(workspaces[0].workspace_id, moved_id);
        assert!(event_hub.events_after(0).is_empty());
    }
}
