// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{
    AgentPromptParams, AgentRenameParams, AgentSendKeysParams, AgentSendParams, AgentStartParams,
    AgentTarget, PaneReadResult, ReadFormat, ReadSource, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

const AGENT_PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);

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

    pub(super) fn handle_agent_prompt(&mut self, id: String, params: AgentPromptParams) -> String {
        if params.text.is_empty() {
            return encode_error(id, "empty_agent_prompt", "agent prompt must not be empty");
        }
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return agent_not_found(id, &params.target);
        };
        if terminal.agent_name.as_deref() != Some(params.target.as_str()) {
            return agent_not_ready(id, &params.target);
        }
        if params
            .expected_terminal_id
            .as_deref()
            .is_some_and(|expected| expected != resolved.terminal_id)
        {
            return encode_error(
                id,
                "agent_target_changed",
                "agent target no longer has the expected terminal_id",
            );
        }
        let changed = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .is_some_and(|terminal| {
                terminal.reconcile_managed_agent_at(std::time::Instant::now(), None)
            });
        if changed {
            self.state.mark_session_dirty();
            self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
            self.schedule_session_save();
        }
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return agent_not_found(id, &params.target);
        };
        // Detection and foreground checks gate readiness, never receiver authority.
        let Some(expected_agent) = terminal.effective_known_agent().filter(|_| {
            terminal.agent_name.as_deref() == Some(params.target.as_str())
                && terminal.state != crate::detect::AgentState::Unknown
        }) else {
            return agent_not_ready(id, &params.target);
        };
        if terminal.managed_agent_launch_pending() {
            return agent_not_ready(id, &params.target);
        }
        if terminal.state == crate::detect::AgentState::Working {
            return encode_error(
                id,
                "agent_working",
                format!(
                    "agent {} is still working; wait before prompting",
                    params.target
                ),
            );
        }
        if terminal.state == crate::detect::AgentState::Blocked {
            return encode_error(
                id,
                "agent_blocked",
                format!(
                    "agent {} is blocked and needs user input before it can accept a prompt",
                    params.target
                ),
            );
        }
        if terminal.managed_agent_kind().is_some() && !terminal.managed_agent_interactive_ready() {
            return agent_not_ready(id, &params.target);
        }
        let baseline_state_change_seq = terminal.last_agent_state_change_seq.unwrap_or(0);
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !runtime.pending_process_exits().is_empty()
            || !runtime_hosts_agent(runtime, expected_agent)
        {
            return encode_error(
                id,
                "agent_not_ready",
                format!(
                    "agent {} is no longer the pane foreground process or has a pending exit",
                    params.target
                ),
            );
        }
        let (mut text, enter) =
            crate::app::api_helpers::encode_api_submission_parts(runtime, &params.text);
        if expected_agent == crate::detect::Agent::GithubCopilot {
            let focus = match crate::ghostty::encode_focus(crate::ghostty::FocusEvent::Gained) {
                Ok(focus) => focus,
                Err(err) => return encode_error(id, "agent_prompt_failed", err.to_string()),
            };
            let mut focused = Vec::with_capacity(focus.len() + text.len());
            focused.extend_from_slice(&focus);
            focused.append(&mut text);
            text = focused;
        }
        if let Err(err) = runtime.try_send_bytes_with_delayed_suffix(
            Bytes::from(text),
            Bytes::from(enter),
            AGENT_PROMPT_SUBMIT_DELAY,
        ) {
            return encode_error(id, "agent_prompt_failed", err.to_string());
        }
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        encode_success(
            id,
            ResponseResult::AgentPrompted {
                agent,
                baseline_state_change_seq,
            },
        )
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

    pub(super) fn handle_agent_send_keys(
        &mut self,
        id: String,
        params: AgentSendKeysParams,
    ) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.effective_known_agent())
        else {
            return agent_not_ready(id, &params.target);
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !runtime_hosts_agent(runtime, expected_agent) {
            return agent_not_ready(id, &params.target);
        }
        let encoded = match crate::app::api_helpers::encode_api_keys(runtime, &params.keys) {
            Ok(encoded) => encoded,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        let bytes = encoded.into_iter().flatten().collect::<Vec<_>>();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "agent_send_keys_failed", err.to_string());
        }
        encode_success(id, ResponseResult::Ok {})
    }
}

fn runtime_hosts_agent(
    runtime: &crate::terminal::TerminalRuntime,
    expected: crate::detect::Agent,
) -> bool {
    let Some(job) = runtime.child_pid().and_then(crate::detect::foreground_job) else {
        return false;
    };
    crate::detect::identify_agent_in_job(&job)
        .map(|(agent, _)| agent)
        .or_else(|| {
            job.processes
                .iter()
                .find_map(|process| crate::platform::process_agent_hint(process.pid))
        })
        == Some(expected)
}

fn agent_not_ready(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_ready",
        format!("agent {target} is not an active named agent"),
    )
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
    fn m839_name_detected_fixture(fixture: &mut crate::app::creation::tests::CwdFixture) {
        let (_, terminal, _) = m839_fixture_target(&fixture.app);
        let state = fixture.app.state.terminals.get_mut(&terminal).unwrap();
        state.set_agent_name("worker".into());
        state.set_detected_state(Some(Agent::Codex), AgentState::Idle);
    }

    fn m839_prompt_params(
        terminal: Option<String>,
        text: &str,
    ) -> crate::api::schema::AgentPromptParams {
        crate::api::schema::AgentPromptParams {
            target: "worker".into(),
            text: text.into(),
            expected_terminal_id: terminal,
            wait: None,
        }
    }

    #[tokio::test]
    async fn m839b_prompt_and_send_input_share_one_real_queue_item_without_authority() {
        let mut fixture = m839_real_agent_fixture(false).await;
        m839_name_detected_fixture(&mut fixture);
        let (internal, terminal, pane) = m839_fixture_target(&fixture.app);
        let observed_at = std::time::Instant::now();
        for state in [AgentState::Working, AgentState::Idle] {
            fixture
                .app
                .handle_internal_event(crate::events::AppEvent::StateChanged {
                    pane_id: internal,
                    agent: Some(Agent::Codex),
                    state,
                    visible_blocker: false,
                    visible_working: false,
                    process_exited: false,
                    observed_at,
                });
        }
        let baseline = fixture.app.state.terminals[&terminal]
            .last_agent_state_change_seq
            .unwrap();
        assert!(baseline > 0);
        let other = fixture.app.state.workspaces[0].tabs[0].root_pane;
        fixture
            .app
            .handle_internal_event(crate::events::AppEvent::StateChanged {
                pane_id: other,
                agent: Some(Agent::Pi),
                state: AgentState::Working,
                visible_blocker: false,
                visible_working: true,
                process_exited: false,
                observed_at,
            });
        assert!(fixture.app.state.next_agent_state_change_seq > baseline);
        let text = crate::zynk::header::prepend_header("M839-HEADER", "body\nnext");
        for (flags, enter) in [
            (0, b"\r".as_slice()),
            (1, b"\r".as_slice()),
            (9, b"\x1b[13u".as_slice()),
            (11, b"\x1b[13u".as_slice()),
        ] {
            for bracketed in [false, true] {
                let modes = format!(
                    "\x1b[={flags}u\x1b[?2004{}",
                    if bracketed { 'h' } else { 'l' }
                );
                fixture
                    .app
                    .terminal_runtimes
                    .get(&terminal)
                    .unwrap()
                    .test_process_pty_bytes(modes.as_bytes());
                let mut expected = if bracketed {
                    format!("\x1b[200~{text}\x1b[201~").into_bytes()
                } else {
                    text.as_bytes().to_vec()
                };
                expected.extend_from_slice(enter);
                assert_eq!(
                    fixture.app.state.terminals[&terminal].last_agent_state_change_seq,
                    Some(baseline)
                );
                let (prompted, prompt_items) = crate::pane::test_observe_try_sends(|| {
                    m839_call(
                        &mut fixture.app,
                        Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), &text)),
                    )
                });
                assert_eq!(prompted["result"]["type"], "agent_prompted", "{prompted}");
                assert_eq!(prompted["result"]["baseline_state_change_seq"], baseline);
                assert_eq!(
                    prompted["result"]["agent"]["terminal_id"],
                    terminal.to_string()
                );
                assert_eq!(prompted["result"]["agent"]["agent"], "codex");
                assert!(prompted["result"]["agent"].get("agent_session").is_none());
                let (sent, send_items) = crate::pane::test_observe_try_sends(|| {
                    m839_call(
                        &mut fixture.app,
                        Method::PaneSendInput(crate::api::schema::PaneSendInputParams {
                            pane_id: pane.clone(),
                            text: text.clone(),
                            keys: vec!["Enter".into()],
                        }),
                    )
                });
                assert_eq!(sent["result"]["type"], "ok");
                assert_eq!(prompt_items.len(), 1);
                assert_eq!(send_items.len(), 1);
                assert_eq!(
                    prompt_items[0].outcome,
                    crate::pane::TestInputOutcome::Accepted
                );
                assert_eq!(prompt_items, send_items);
                assert_eq!(
                    prompt_items[0].bytes.as_ref(),
                    expected,
                    "flags={flags} bracketed={bracketed}"
                );
                assert_eq!(
                    fixture.app.state.terminals[&terminal].confirmed_hook_owner(),
                    None
                );
                assert_eq!(
                    fixture.app.state.terminals[&terminal].persisted_agent_session,
                    None
                );
                assert!(fixture.app.authoritative_receiver_identity(&pane).is_none());
            }
        }
        let receipt = fixture.app.handle_zynk_message_received(
            "m839-no-owner".into(),
            crate::api::schema::ZynkMessageReceivedParams {
                pane_id: pane,
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
        let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(receipt["error"]["code"], "receiver_identity_unverified");
    }

    #[tokio::test]
    async fn m839b_prompt_terminal_precondition_precedes_readiness_and_missing_pin_is_optional() {
        let mut fixture = m839_real_agent_fixture(false).await;
        m839_name_detected_fixture(&mut fixture);
        let (_, terminal, _) = m839_fixture_target(&fixture.app);
        fixture
            .app
            .state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);
        let (changed, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some("term_old".into()), "body")),
            )
        });
        assert_eq!(changed["error"]["code"], "agent_target_changed");
        assert!(attempts.is_empty());
        let (working, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });
        assert_eq!(working["error"]["code"], "agent_working");
        assert!(attempts.is_empty());
        fixture
            .app
            .state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);
        let (unbound, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(None, "body")),
            )
        });
        assert_eq!(unbound["result"]["type"], "agent_prompted");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, crate::pane::TestInputOutcome::Accepted);
        assert_eq!(attempts[0].bytes.as_ref(), b"body\r");
    }

    #[tokio::test]
    async fn m870_blocked_detection_refuses_prompt_without_minting_receiver_authority() {
        let mut fixture = m839_real_agent_fixture(false).await;
        m839_name_detected_fixture(&mut fixture);
        let (_, terminal, pane) = m839_fixture_target(&fixture.app);
        fixture
            .app
            .state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Codex),
                AgentState::Blocked,
                false,
                true,
                false,
                false,
                std::time::Instant::now(),
            );

        let (response, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });

        assert_eq!(response["error"]["code"], "agent_blocked");
        assert!(attempts.is_empty());
        let state = &fixture.app.state.terminals[&terminal];
        assert_eq!(state.confirmed_hook_owner(), None);
        assert_eq!(state.persisted_agent_session, None);
        assert!(fixture.app.authoritative_receiver_identity(&pane).is_none());
    }

    #[tokio::test]
    async fn m871_copilot_focus_and_prompt_share_one_delayed_submission() {
        let mut fixture = m839_real_program_fixture("copilot").await;
        let (_, terminal, pane) = m839_fixture_target(&fixture.app);
        let state = fixture.app.state.terminals.get_mut(&terminal).unwrap();
        state.set_agent_name("worker".into());
        state.set_detected_state(Some(Agent::GithubCopilot), AgentState::Idle);

        let (response, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });

        assert_eq!(response["result"]["type"], "agent_prompted", "{response}");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, crate::pane::TestInputOutcome::Accepted);
        assert_eq!(attempts[0].bytes.as_ref(), b"\x1b[Ibody\r");
        assert!(fixture.app.authoritative_receiver_identity(&pane).is_none());
    }

    #[tokio::test]
    async fn m840_agent_send_keys_validates_all_keys_before_one_enqueue() {
        let mut fixture = m839_real_agent_fixture(false).await;
        m839_name_detected_fixture(&mut fixture);

        let (rejected, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentSendKeys(AgentSendKeysParams {
                    target: "worker".into(),
                    keys: vec!["up".into(), "not-a-key".into()],
                }),
            )
        });
        assert_eq!(rejected["error"]["code"], "invalid_key");
        assert!(attempts.is_empty());

        let (sent, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentSendKeys(AgentSendKeysParams {
                    target: "worker".into(),
                    keys: vec!["up".into(), "enter".into()],
                }),
            )
        });
        assert_eq!(sent["result"]["type"], "ok");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, crate::pane::TestInputOutcome::Accepted);
        assert_eq!(attempts[0].bytes.as_ref(), b"\x1b[A\r");
    }

    #[tokio::test]
    async fn m839b_prompt_refuses_empty_unready_and_actual_foreground_mismatch() {
        let mut fixture = m839_real_agent_fixture(false).await;
        m839_name_detected_fixture(&mut fixture);
        let (_, terminal, _) = m839_fixture_target(&fixture.app);
        for state in [AgentState::Idle, AgentState::Working, AgentState::Unknown] {
            fixture
                .app
                .state
                .terminals
                .get_mut(&terminal)
                .unwrap()
                .set_detected_state(Some(Agent::Codex), state);
            let (empty, attempts) = crate::pane::test_observe_try_sends(|| {
                m839_call(
                    &mut fixture.app,
                    Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "")),
                )
            });
            assert_eq!(empty["error"]["code"], "empty_agent_prompt");
            assert!(attempts.is_empty());
        }
        let (unknown, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });
        assert_eq!(unknown["error"]["code"], "agent_not_ready");
        assert!(attempts.is_empty());

        let mut fixture = m839_real_agent_fixture(true).await;
        m839_name_detected_fixture(&mut fixture);
        let (_, terminal, _) = m839_fixture_target(&fixture.app);
        let (mismatch, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });
        assert_eq!(mismatch["error"]["code"], "agent_not_ready");
        assert!(mismatch["error"]["message"]
            .as_str()
            .unwrap()
            .contains("foreground"));
        assert!(attempts.is_empty());

        let mut fixture = m839_real_program_fixture("cat").await;
        m839_name_detected_fixture(&mut fixture);
        let (_, terminal, _) = m839_fixture_target(&fixture.app);
        let (non_agent, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });
        assert_eq!(non_agent["error"]["code"], "agent_not_ready");
        assert!(non_agent["error"]["message"]
            .as_str()
            .unwrap()
            .contains("foreground"));
        assert!(attempts.is_empty());

        let mut fixture = crate::app::creation::tests::CwdFixture::new();
        m839_name_detected_fixture(&mut fixture);
        let (_, terminal, _) = m839_fixture_target(&fixture.app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        fixture
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        let (missing, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });
        assert_eq!(missing["error"]["code"], "agent_not_ready");
        assert!(attempts.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn m839b_pending_exit_keeps_session_observation_but_refuses_prompt_and_receipt() {
        use std::time::Instant;
        let mut fixture = m839_real_agent_fixture(false).await;
        m839_name_detected_fixture(&mut fixture);
        let (pane_id, terminal, public) = m839_fixture_target(&fixture.app);
        fixture
            .app
            .state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_hook_authority_with_session_ref(
                "zynk:codex".into(),
                "codex".into(),
                AgentState::Idle,
                None,
                crate::agent_resume::AgentSessionRef::id("old-session"),
                Some(10),
            )
            .unwrap();
        assert!(fixture
            .app
            .authoritative_receiver_identity(&public)
            .is_some());
        let (before_exit, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(
                    Some(terminal.to_string()),
                    "before exit",
                )),
            )
        });
        assert_eq!(before_exit["result"]["type"], "agent_prompted");
        assert_eq!(
            before_exit["result"]["agent"]["agent_session"]["value"],
            "old-session"
        );
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, crate::pane::TestInputOutcome::Accepted);
        assert_eq!(attempts[0].bytes.as_ref(), b"before exit\r");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        fixture
            .app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .test_publish_process_exit(tx, pane_id, Agent::Codex, Instant::now())
            .await;
        assert_eq!(
            fixture
                .app
                .terminal_runtimes
                .get(&terminal)
                .unwrap()
                .pending_process_exits()
                .len(),
            1
        );
        let observed = m839_call(
            &mut fixture.app,
            Method::AgentGet(AgentTarget {
                target: "worker".into(),
            }),
        );
        assert_eq!(
            observed["result"]["agent"]["agent_session"]["value"],
            "old-session"
        );
        assert!(fixture
            .app
            .authoritative_receiver_identity(&public)
            .is_none());
        let (refused, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentPrompt(m839_prompt_params(Some(terminal.to_string()), "body")),
            )
        });
        assert_eq!(refused["error"]["code"], "agent_not_ready");
        assert!(attempts.is_empty());
        assert_eq!(
            fixture
                .app
                .terminal_runtimes
                .get(&terminal)
                .unwrap()
                .pending_process_exits()
                .len(),
            1
        );
        fixture.app.handle_internal_event(rx.recv().await.unwrap());
        assert!(fixture
            .app
            .authoritative_receiver_identity(&public)
            .is_none());
        assert_eq!(
            fixture.app.state.terminals[&terminal].confirmed_hook_owner(),
            None
        );
    }

    use std::time::{Duration, Instant};
    #[tokio::test]
    async fn m839a_app_applies_capture_order_without_reviving_hook_authority() {
        let mut fixture = crate::app::creation::tests::CwdFixture::new();
        let (pane, terminal, public_pane) = m839_fixture_target(&fixture.app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        fixture
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        let (events, mut pending) = tokio::sync::mpsc::channel(8);
        let old_capture = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let state = fixture.app.state.terminals.get_mut(&terminal).unwrap();
        state
            .set_hook_authority_at(
                "zynk:codex".into(),
                "codex".into(),
                AgentState::Idle,
                None,
                None,
                Some(1),
                old_capture,
            )
            .unwrap();
        state.set_manual_label("manual".into());
        fixture
            .app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .test_publish_process_exit(events.clone(), pane, Agent::Codex, old_capture)
            .await;
        let started_at = Instant::now();
        assert!(started_at > old_capture);
        fixture
            .app
            .state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .begin_managed_agent(
                "worker".into(),
                Agent::Codex,
                started_at,
                Duration::from_secs(3),
                Duration::from_secs(30),
            );
        assert!(fixture
            .app
            .authoritative_receiver_identity(&public_pane)
            .is_none());
        fixture
            .app
            .handle_internal_event(pending.try_recv().unwrap());
        let state = &fixture.app.state.terminals[&terminal];
        assert_eq!(state.managed_agent_kind(), Some(Agent::Codex));
        assert!(state.managed_agent_launch_pending());
        assert_eq!(state.agent_name.as_deref(), Some("worker"));
        assert!(state.hook_authority.is_none());
        assert!(state.hook_identity.is_none());
        assert!(fixture
            .app
            .authoritative_receiver_identity(&public_pane)
            .is_none());
        let new_capture = Instant::now();
        fixture
            .app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .test_publish_process_exit(events, pane, Agent::Codex, new_capture)
            .await;
        fixture
            .app
            .handle_internal_event(pending.try_recv().unwrap());
        let state = &fixture.app.state.terminals[&terminal];
        assert_eq!(state.managed_agent_kind(), None);
        assert!(!state.managed_agent_launch_pending());
        assert_eq!(state.agent_name, None);
        assert_eq!(state.manual_label.as_deref(), Some("manual"));
        assert!(fixture
            .app
            .authoritative_receiver_identity(&public_pane)
            .is_none());
    }

    #[tokio::test]
    async fn m839a_start_uses_existing_shell_path_and_one_real_queue_item() {
        use std::time::{Duration, Instant};
        let mut fixture = m839_real_agent_fixture(true).await;
        let (_, terminal, pane) = m839_fixture_target(&fixture.app);
        fixture.app.state.switch_workspace_tab(1, 0);
        let focus = fixture.app.state.current_pane_focus_target();
        let previous = fixture.app.state.previous_pane_focus.clone();
        let topology = m839_topology(&fixture.app);
        let terminals = fixture.app.state.terminals.len();
        let mut params = m839_start_params(pane.clone());
        params.args = vec![
            "two words".into(),
            "a'b".into(),
            "$HOME".into(),
            "one\\two".into(),
            String::new(),
        ];
        let (response, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(&mut fixture.app, Method::AgentStart(params))
        });
        assert_eq!(response["result"]["type"], "agent_started", "{response}");
        assert_eq!(
            response["result"]["agent"]["terminal_id"],
            terminal.to_string()
        );
        assert_eq!(response["result"]["agent"]["pane_id"], pane);
        assert_eq!(response["result"]["agent"]["launch_pending"], true);
        assert!(response["result"]["agent"]
            .get("interactive_ready")
            .is_none());
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, crate::pane::TestInputOutcome::Accepted);
        assert_eq!(
            attempts[0].bytes.as_ref(),
            b"codex 'two words' 'a'\\''b' '$HOME' 'one\\two' ''\r"
        );
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
        assert_eq!(fixture.app.state.previous_pane_focus, previous);
        assert_eq!(m839_topology(&fixture.app), topology);
        assert_eq!(fixture.app.state.terminals.len(), terminals);
        assert_eq!(
            fixture.app.state.terminals[&terminal].confirmed_hook_owner(),
            None
        );
        assert_eq!(
            fixture.app.state.terminals[&terminal].persisted_agent_session,
            None
        );
        let path = fixture.root.join("m839-argv");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text == "two words\na'b\n$HOME\none\\two\n\n" {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "pane-local PATH command did not report arguments: {:?}",
                std::fs::read_to_string(&path)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn m839a_start_validates_timeout_kind_and_argument_before_queueing() {
        let mut fixture = m839_real_agent_fixture(true).await;
        let (_, terminal, pane) = m839_fixture_target(&fixture.app);
        for (timeout, kind, args, code) in [
            (0, "codex", vec![], "invalid_agent_timeout"),
            (3000, "codex", vec![], "invalid_agent_timeout"),
            (300001, "codex", vec![], "invalid_agent_timeout"),
            (u64::MAX, "codex", vec![], "invalid_agent_timeout"),
            (30000, "omp", vec![], "unsupported_agent_kind"),
            (
                30000,
                "codex",
                vec!["line\nbreak".into()],
                "invalid_agent_argument",
            ),
            (
                30000,
                "codex",
                vec!["zero\0byte".into()],
                "invalid_agent_argument",
            ),
        ] {
            let mut params = m839_start_params(pane.clone());
            params.timeout_ms = Some(timeout);
            params.kind = kind.into();
            params.args = args;
            let (response, attempts) = crate::pane::test_observe_try_sends(|| {
                m839_call(&mut fixture.app, Method::AgentStart(params))
            });
            assert_eq!(
                response["error"]["code"], code,
                "timeout={timeout} kind={kind}: {response}"
            );
            assert!(attempts.is_empty());
            assert_eq!(fixture.app.state.terminals[&terminal].agent_name, None);
            assert_eq!(
                fixture.app.state.terminals[&terminal].managed_agent_kind(),
                None
            );
        }
    }

    #[tokio::test]
    async fn m839a_start_closed_admission_restores_each_previous_manual_label() {
        use std::time::Duration;
        for label in [None, Some("my original label")] {
            let mut fixture = m839_real_agent_fixture(true).await;
            let (_, terminal, pane) = m839_fixture_target(&fixture.app);
            if let Some(label) = label {
                fixture
                    .app
                    .state
                    .terminals
                    .get_mut(&terminal)
                    .unwrap()
                    .set_manual_label(label.into());
            }
            fixture
                .app
                .terminal_runtimes
                .get(&terminal)
                .unwrap()
                .pause_handoff_reader(Duration::from_secs(1))
                .unwrap();
            let (response, attempts) = crate::pane::test_observe_try_sends(|| {
                m839_call(
                    &mut fixture.app,
                    Method::AgentStart(m839_start_params(pane)),
                )
            });
            assert_eq!(
                response["error"]["code"], "agent_start_input_failed",
                "{response}"
            );
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].outcome, crate::pane::TestInputOutcome::Closed);
            assert_eq!(attempts[0].bytes.as_ref(), b"codex\r");
            let state = &fixture.app.state.terminals[&terminal];
            assert_eq!(state.manual_label.as_deref(), label);
            assert_eq!(state.agent_name, None);
            assert_eq!(state.managed_agent_kind(), None);
            assert_eq!(state.confirmed_hook_owner(), None);
        }
    }

    #[tokio::test]
    async fn m839a_start_refuses_missing_pid_and_real_non_shell_foreground() {
        let mut fixture = crate::app::creation::tests::CwdFixture::new();
        let (_, terminal, pane) = m839_fixture_target(&fixture.app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        assert_eq!(runtime.child_pid(), None);
        fixture
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        let (response, attempts) = crate::pane::test_observe_try_sends(|| {
            m839_call(
                &mut fixture.app,
                Method::AgentStart(m839_start_params(pane)),
            )
        });
        assert_eq!(response["error"]["code"], "agent_target_busy");
        assert_eq!(
            response["error"]["message"],
            "pane has no child pid; foreground shell unavailable"
        );
        assert!(attempts.is_empty());
        assert!(rx.try_recv().is_err());

        for program in ["codex", "cat"] {
            let mut fixture = m839_real_program_fixture(program).await;
            let (_, terminal, pane) = m839_fixture_target(&fixture.app);
            let (response, attempts) = crate::pane::test_observe_try_sends(|| {
                m839_call(
                    &mut fixture.app,
                    Method::AgentStart(m839_start_params(pane)),
                )
            });
            assert_eq!(response["error"]["code"], "agent_target_busy", "{response}");
            assert_eq!(
                response["error"]["message"],
                format!("foreground {program:?} is not a supported pane shell")
            );
            assert!(attempts.is_empty());
            assert_eq!(fixture.app.state.terminals[&terminal].agent_name, None);
        }
    }

    #[tokio::test]
    async fn m839a_pending_and_active_names_reserve_holder_and_pending_refuses_rename() {
        let mut fixture = m839_real_agent_fixture(true).await;
        m839_install_program(&mut fixture, 0, "shell").await;
        let (_, terminal, pane) = m839_fixture_target(&fixture.app);
        let (_, other_terminal, other_pane) = m839_fixture_tab_target(&fixture.app, 0);
        let started = m839_call(
            &mut fixture.app,
            Method::AgentStart(m839_start_params(pane.clone())),
        );
        assert_eq!(started["result"]["agent"]["launch_pending"], true);
        for active in [false, true] {
            if active {
                let state = fixture.app.state.terminals.get_mut(&terminal).unwrap();
                let settled_at = state.next_managed_agent_deadline().unwrap();
                state.set_detected_state_with_screen_signals_at(
                    Some(Agent::Codex),
                    AgentState::Idle,
                    false,
                    false,
                    false,
                    false,
                    settled_at,
                );
                state.reconcile_managed_agent_at(settled_at, None);
                assert!(!state.managed_agent_launch_pending());
                assert_eq!(state.managed_agent_kind(), Some(Agent::Codex));
                assert!(state.managed_agent_interactive_ready());
            }
            let (duplicate, attempts) = crate::pane::test_observe_try_sends(|| {
                m839_call(
                    &mut fixture.app,
                    Method::AgentStart(m839_start_params(other_pane.clone())),
                )
            });
            assert_eq!(
                duplicate["error"]["code"], "agent_name_taken",
                "active={active}: {duplicate}"
            );
            assert!(duplicate["error"]["message"]
                .as_str()
                .unwrap()
                .contains(&terminal.to_string()));
            assert!(attempts.is_empty());
            assert_eq!(
                fixture.app.state.terminals[&other_terminal].agent_name,
                None
            );
            if !active {
                let rename = m839_call(
                    &mut fixture.app,
                    Method::AgentRename(AgentRenameParams {
                        target: "worker".into(),
                        name: Some("replacement".into()),
                    }),
                );
                assert_eq!(rename["error"]["code"], "agent_launch_pending");
                assert_eq!(
                    fixture.app.state.terminals[&terminal].agent_name.as_deref(),
                    Some("worker")
                );
            }
        }
        let rename = m839_call(
            &mut fixture.app,
            Method::AgentRename(AgentRenameParams {
                target: "worker".into(),
                name: Some("replacement".into()),
            }),
        );
        assert_eq!(rename["result"]["agent"]["name"], "replacement");
        assert_eq!(
            rename["result"]["agent"]["terminal_id"],
            terminal.to_string()
        );
        assert_eq!(
            fixture.app.state.terminals[&terminal].confirmed_hook_owner(),
            None
        );
    }

    #[tokio::test]
    async fn m839a_start_accepts_both_timeout_bounds_and_default_expiry() {
        use std::time::Duration;
        for timeout in [Some(3001), Some(300000), None] {
            let mut fixture = m839_real_agent_fixture(true).await;
            let (_, terminal, pane) = m839_fixture_target(&fixture.app);
            let mut params = m839_start_params(pane);
            params.timeout_ms = timeout;
            let (response, attempts) = crate::pane::test_observe_try_sends(|| {
                m839_call(&mut fixture.app, Method::AgentStart(params))
            });
            assert_eq!(response["result"]["type"], "agent_started", "{response}");
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].outcome, crate::pane::TestInputOutcome::Accepted);
            let state = fixture.app.state.terminals.get_mut(&terminal).unwrap();
            let settle = state.next_managed_agent_deadline().unwrap();
            state.reconcile_managed_agent_at(settle, None);
            let deadline = settle + Duration::from_millis(timeout.unwrap_or(30000) - 3000);
            assert_eq!(state.next_managed_agent_deadline(), Some(deadline));
            state.reconcile_managed_agent_at(deadline - Duration::from_millis(1), None);
            assert!(state.managed_agent_launch_pending());
            state.reconcile_managed_agent_at(deadline, None);
            assert_eq!(state.agent_name, None);
            assert_eq!(state.managed_agent_kind(), None);
            assert_eq!(state.manual_label.as_deref(), Some("worker"));
            assert_eq!(state.next_managed_agent_deadline(), None);
        }
    }

    fn m839_fixture_target(
        app: &App,
    ) -> (crate::layout::PaneId, crate::terminal::TerminalId, String) {
        m839_fixture_tab_target(app, 1)
    }

    fn m839_fixture_tab_target(
        app: &App,
        tab_index: usize,
    ) -> (crate::layout::PaneId, crate::terminal::TerminalId, String) {
        let tab = &app.state.workspaces[0].tabs[tab_index];
        let pane = tab.root_pane;
        (
            pane,
            tab.terminal_id(pane).unwrap().clone(),
            app.public_pane_id(0, pane).unwrap(),
        )
    }

    fn m839_call(app: &mut App, method: Method) -> serde_json::Value {
        serde_json::from_str(&app.handle_api_request(Request {
            id: "m839-app".into(),
            method,
        }))
        .unwrap()
    }

    async fn m839_real_agent_fixture(shell: bool) -> crate::app::creation::tests::CwdFixture {
        m839_real_program_fixture(if shell { "shell" } else { "codex" }).await
    }

    async fn m839_real_program_fixture(program: &str) -> crate::app::creation::tests::CwdFixture {
        let mut fixture = crate::app::creation::tests::CwdFixture::new();
        m839_install_program(&mut fixture, 1, program).await;
        fixture
    }

    async fn m839_install_program(
        fixture: &mut crate::app::creation::tests::CwdFixture,
        tab_index: usize,
        program: &str,
    ) {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};
        let bin = fixture.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert!(matches!(program, "shell" | "codex" | "copilot" | "cat"));
        let shell = program == "shell";
        let executable = bin.join(if program == "shell" { "codex" } else { program });
        if shell {
            std::fs::write(
                &executable,
                b"#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/m839-argv\"\nexec /bin/cat\n",
            )
            .unwrap();
        } else {
            std::fs::copy("/bin/cat", &executable).unwrap();
        }
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (pane, terminal, _) = m839_fixture_tab_target(&fixture.app, tab_index);
        let cwd = fixture.app.state.terminals[&terminal].cwd.clone();
        let launch = crate::pane::PaneLaunchEnv::from_extra(vec![
            ("HOME".into(), fixture.root.display().to_string()),
            ("ENV".into(), "/dev/null".into()),
            ("BASH_ENV".into(), "/dev/null".into()),
            ("INPUTRC".into(), "/dev/null".into()),
            ("PROMPT_COMMAND".into(), String::new()),
            ("PS1".into(), String::new()),
            ("PS2".into(), String::new()),
            ("PATH".into(), bin.display().to_string()),
            ("ZYNK_AGENT".into(), String::new()),
        ])
        .without_pane_identity();
        let argv = if shell {
            vec![
                "/bin/bash".into(),
                "--noprofile".into(),
                "--norc".into(),
                "--noediting".into(),
                "-i".into(),
            ]
        } else {
            vec![executable.display().to_string()]
        };
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane,
            24,
            80,
            cwd,
            &argv,
            &launch,
            crate::pane::AgentDetection::Disabled,
            0,
            fixture.app.state.host_terminal_theme,
            fixture.app.state.host_terminal_appearance,
            fixture.app.event_tx.clone(),
            fixture.app.render_notify.clone(),
            fixture.app.render_dirty.clone(),
        )
        .unwrap();
        assert!(fixture
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime)
            .is_none());
        let runtime = fixture.app.terminal_runtimes.get(&terminal).unwrap();
        let pid = runtime.child_pid().unwrap();
        if shell {
            runtime
                .send_bytes(Bytes::from(format!(
                    "printf '%s' ready > \"$HOME/m839-shell-ready-{tab_index}\"\r"
                )))
                .await
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(job) = crate::detect::foreground_job(pid) {
                for process in &job.processes {
                    assert_eq!(crate::platform::process_agent_hint(process.pid), None);
                }
                let ready = if shell {
                    crate::platform::available_pane_shell(pid).is_ok()
                        && std::fs::read(fixture.root.join(format!("m839-shell-ready-{tab_index}")))
                            .ok()
                            .as_deref()
                            == Some(b"ready".as_slice())
                } else if matches!(program, "codex" | "copilot") {
                    crate::detect::identify_agent_in_job(&job).map(|(agent, _)| agent)
                        == Some(if program == "codex" {
                            Agent::Codex
                        } else {
                            Agent::GithubCopilot
                        })
                } else {
                    crate::detect::identify_agent_in_job(&job).is_none()
                        && job
                            .processes
                            .iter()
                            .any(|process| process.pid == pid && process.name == "cat")
                };
                if ready {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "m839 real foreground not ready: program={program} pid={pid}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!runtime.bracketed_paste_enabled());
        assert_eq!(
            runtime.keyboard_protocol(),
            crate::input::KeyboardProtocol::Legacy
        );
    }

    fn m839_topology(app: &App) -> serde_json::Value {
        fn node(value: &crate::layout::Node) -> serde_json::Value {
            match value {
                crate::layout::Node::Pane(pane) => serde_json::json!({"pane":pane.raw()}),
                crate::layout::Node::Split {
                    direction,
                    ratio,
                    first,
                    second,
                } => serde_json::json!({
                    "direction":format!("{direction:?}"), "ratio":ratio, "first":node(first), "second":node(second),
                }),
            }
        }
        serde_json::Value::Array(app.state.workspaces.iter().map(|ws| {
            serde_json::json!({"active_tab":ws.active_tab, "tabs":ws.tabs.iter().map(|tab| {
                let panes: std::collections::BTreeMap<_, _> = tab.panes.iter().map(|(pane, state)| (pane.raw().to_string(), state.attached_terminal_id.to_string())).collect();
                serde_json::json!({"number":tab.number, "root":tab.root_pane.raw(), "layout":node(tab.layout.root()), "focused":tab.layout.focused().raw(), "panes":panes})
            }).collect::<Vec<_>>()})
        }).collect())
    }

    fn m839_start_params(pane: String) -> crate::api::schema::AgentStartParams {
        crate::api::schema::AgentStartParams {
            name: "worker".into(),
            kind: "codex".into(),
            pane_id: pane,
            args: Vec::new(),
            timeout_ms: Some(30_000),
        }
    }

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
