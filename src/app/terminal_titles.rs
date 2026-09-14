use super::App;

impl App {
    pub(crate) fn sync_terminal_titles(&mut self) -> bool {
        let mut observations = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let terminal_id = &pane.attached_terminal_id;
                    let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
                        continue;
                    };
                    observations.push((
                        ws_idx,
                        *pane_id,
                        terminal_id.clone(),
                        runtime.terminal_title(),
                    ));
                }
            }
        }

        let mut raw_changed = false;
        let mut publish = Vec::new();
        for (ws_idx, pane_id, terminal_id, title) in observations {
            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            let change = terminal.set_terminal_title(title);
            raw_changed |= change.raw_changed;
            if change.stripped_changed {
                publish.push((ws_idx, pane_id));
            }
        }

        for (ws_idx, pane_id) in publish {
            self.emit_pane_updated(ws_idx, pane_id);
        }

        raw_changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{AgentStatus, EventData, EventEnvelope, EventKind};
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    #[tokio::test]
    async fn m828c_sync_scales_over_one_and_fifteen_attached_panes() {
        const WARMUP: usize = 16;
        const SAMPLES: usize = 256;
        let mut summaries = Vec::new();
        for count in [1_usize, 15] {
            for changing in [false, true] {
                let hub = crate::api::EventHub::default();
                let mut app = App::new(
                    &Config::default(),
                    true,
                    None,
                    tokio::sync::mpsc::unbounded_channel().1,
                    hub.clone(),
                );
                let mut workspace = Workspace::test_new("title-scale");
                let mut panes = vec![workspace.tabs[0].root_pane];
                for _ in 1..count {
                    panes.push(workspace.test_split(ratatui::layout::Direction::Horizontal));
                }
                app.state.workspaces = vec![workspace];
                app.state.active = Some(0);
                app.state.ensure_test_terminals();
                assert_eq!(panes.len(), count);
                let title = |pane: usize, iteration: usize| {
                    let prefix = format!("pane-{pane:02} step-{iteration:03} ");
                    let suffix = "\u{1f642}".repeat(256 - prefix.chars().count());
                    prefix + &suffix
                };
                let mut fixtures = Vec::new();
                for (index, pane) in panes.iter().enumerate() {
                    let id = app.state.workspaces[0].terminal_id(*pane).cloned().unwrap();
                    let raw = title(index, 0);
                    assert_eq!(raw.chars().count(), 256);
                    let runtime =
                        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
                    runtime.test_process_pty_bytes(format!("\x1b]2;{raw}\x07").as_bytes());
                    assert_eq!(runtime.terminal_title().as_deref(), Some(raw.as_str()));
                    app.terminal_runtimes.insert(id.clone(), runtime);
                    fixtures.push((*pane, id, raw));
                }
                let cursor = hub.current_sequence();
                assert!(app.sync_terminal_titles());
                assert_eq!(hub.events_after(cursor).len(), count);
                for (pane, _, raw) in &fixtures {
                    let info = app.pane_info(0, *pane).unwrap();
                    assert_eq!(info.terminal_title.as_deref(), Some(raw.as_str()));
                    assert_eq!(info.terminal_title_stripped.as_deref(), Some(raw.as_str()));
                    assert_eq!(info.revision, 1);
                }
                let mut samples_ns = Vec::with_capacity(SAMPLES);
                for iteration in 0..WARMUP + SAMPLES {
                    if changing {
                        for (index, (_, id, raw)) in fixtures.iter_mut().enumerate() {
                            *raw = title(index, iteration + 1);
                            assert_eq!(raw.chars().count(), 256);
                            app.terminal_runtimes
                                .get(id)
                                .unwrap()
                                .test_process_pty_bytes(format!("\x1b]2;{raw}\x07").as_bytes());
                        }
                    }
                    let cursor = hub.current_sequence();
                    let started = std::time::Instant::now();
                    let changed = app.sync_terminal_titles();
                    let elapsed = started.elapsed();
                    assert_eq!(changed, changing);
                    let mut expected = Vec::new();
                    for (pane, _, raw) in &fixtures {
                        let info = app.pane_info(0, *pane).unwrap();
                        assert_eq!(info.terminal_title.as_deref(), Some(raw.as_str()));
                        assert_eq!(info.terminal_title_stripped.as_deref(), Some(raw.as_str()));
                        assert_eq!(
                            info.revision,
                            if changing { iteration as u64 + 2 } else { 1 }
                        );
                        expected.push(info);
                    }
                    expected.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
                    let mut actual: Vec<_> = hub
                        .events_after(cursor)
                        .into_iter()
                        .map(|(_, event)| {
                            assert_eq!(event.event, EventKind::PaneUpdated);
                            match event.data {
                                EventData::PaneUpdated { pane } => pane,
                                other => panic!("unexpected title event: {other:?}"),
                            }
                        })
                        .collect();
                    actual.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
                    if changing {
                        assert_eq!(actual, expected);
                    } else {
                        assert!(actual.is_empty());
                    }
                    if iteration >= WARMUP {
                        samples_ns.push(elapsed.as_nanos());
                    }
                }
                assert_eq!(samples_ns.len(), SAMPLES);
                let mut sorted = samples_ns.clone();
                sorted.sort_unstable();
                let median = sorted[SAMPLES / 2];
                let p95 = sorted[(SAMPLES * 95).div_ceil(100) - 1];
                let max = sorted[SAMPLES - 1];
                let phase = if changing { "changing" } else { "stable" };
                println!(
                    "M8_28C_SYNC {}",
                    serde_json::json!({
                        "panes": count, "phase": phase, "cols": 80, "rows": 24,
                        "title_scalars": 256, "warmup": WARMUP, "samples": SAMPLES,
                        "samples_ns": samples_ns, "median_ns": median, "p95_ns": p95, "max_ns": max,
                        "median_rule": "upper_middle", "p95_rule": "nearest_rank",
                        "interval": "sync_only", "threshold": null
                    })
                );
                summaries.push((count, changing, median));
            }
        }
        for changing in [false, true] {
            let one = summaries
                .iter()
                .find(|row| row.0 == 1 && row.1 == changing)
                .unwrap()
                .2;
            let fifteen = summaries
                .iter()
                .find(|row| row.0 == 15 && row.1 == changing)
                .unwrap()
                .2;
            println!(
                "M8_28C_SYNC_SCALING {}",
                serde_json::json!({
                    "phase": if changing { "changing" } else { "stable" },
                    "median_delta_ns": fifteen as i128 - one as i128,
                    "median_ratio": (one != 0).then(|| fifteen as f64 / one as f64),
                    "parent_helper_delta": null
                })
            );
        }
    }

    #[tokio::test]
    async fn m828c_sync_follows_all_attached_panes_and_skips_missing_state() {
        let hub = crate::api::EventHub::default();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            hub.clone(),
        );
        app.state.workspaces = vec![
            Workspace::test_new("active"),
            Workspace::test_new("background"),
        ];
        app.state.active = Some(0);
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let second = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let third = app.state.workspaces[1].tabs[0].root_pane;
        let fourth = app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        let extra_tab = app.state.workspaces[1].test_add_tab(Some("background tab"));
        let fifth = app.state.workspaces[1].tabs[extra_tab].root_pane;
        app.state.ensure_test_terminals();
        let mut targets = Vec::new();
        for (index, (ws, pane)) in [(0, first), (0, second), (1, third), (1, fourth), (1, fifth)]
            .into_iter()
            .enumerate()
        {
            let id = app.state.workspaces[ws].terminal_id(pane).cloned().unwrap();
            let terminal = app.state.terminals.get_mut(&id).unwrap();
            terminal.set_agent_metadata(crate::terminal::AgentMetadataReport {
                source: "legacy".into(),
                agent_label: None,
                applies_to_source: None,
                title: Some(format!("presentation-{index}")),
                display_agent: None,
                custom_status: None,
                state_labels: std::collections::HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_custom_status: false,
                clear_state_labels: false,
                ttl: None,
                seq: Some(1),
            });
            assert!(terminal.metadata_tokens.patch(
                std::collections::HashMap::from([("kept".into(), Some(format!("token-{index}")))]),
                None,
                std::time::Instant::now(),
            ));
            assert_eq!(terminal.revision, 0);
            let before = terminal.clone();
            let raw = format!("\u{25d0} attached-{index}");
            let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
            runtime.test_process_pty_bytes(format!("\x1b]2;{raw}\x07").as_bytes());
            assert_eq!(runtime.terminal_title().as_deref(), Some(raw.as_str()));
            app.terminal_runtimes.insert(id.clone(), runtime);
            targets.push((ws, pane, id, raw, before));
        }
        assert_eq!(
            targets
                .iter()
                .map(|row| &row.2)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
        let updates = |cursor| {
            let mut panes: Vec<_> = hub
                .events_after(cursor)
                .into_iter()
                .map(|(_, event)| {
                    assert_eq!(event.event, EventKind::PaneUpdated);
                    match event.data {
                        EventData::PaneUpdated { pane } => pane,
                        other => panic!("unexpected title event: {other:?}"),
                    }
                })
                .collect();
            panes.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
            panes
        };
        let cursor = hub.current_sequence();
        assert!(app.sync_terminal_titles());
        let mut expected = Vec::new();
        for (ws, pane, id, raw, before) in &targets {
            let info = app.pane_info(*ws, *pane).unwrap();
            assert_eq!(info.terminal_id, id.to_string());
            assert_eq!(info.terminal_title.as_deref(), Some(raw.as_str()));
            assert_eq!(
                info.terminal_title_stripped,
                crate::terminal::stripped_terminal_title(raw)
            );
            assert_eq!(info.revision, 1);
            let terminal = &app.state.terminals[id];
            assert_eq!(terminal.agent_metadata, before.agent_metadata);
            assert_eq!(terminal.metadata_tokens, before.metadata_tokens);
            expected.push(info);
        }
        expected.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
        assert_eq!(updates(cursor), expected);
        assert_eq!(app.state.active, Some(0));
        let old_public = app.public_pane_id(1, fourth).unwrap();
        let destination_tab = app.public_tab_id(0, 0).unwrap();
        let destination_pane = app.public_pane_id(0, first).unwrap();
        let request = serde_json::from_value(serde_json::json!({
            "id": "title-move", "method": "pane.move", "params": {
                "pane_id": old_public, "focus": false,
                "destination": {"type": "tab", "tab_id": destination_tab,
                    "target_pane_id": destination_pane, "split": "down"}
            }
        }))
        .unwrap();
        let moved: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request)).unwrap();
        assert_eq!(moved["result"]["type"], "pane_move", "{moved}");
        assert_eq!(moved["result"]["move_result"]["changed"], true);
        let moved_terminal = targets[3].2.clone();
        let moved_pane = *app.state.workspaces[0].tabs[0]
            .panes
            .iter()
            .find(|(_, pane)| pane.attached_terminal_id == moved_terminal)
            .unwrap()
            .0;
        let cursor = hub.current_sequence();
        app.terminal_runtimes
            .get(&moved_terminal)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]2;moved-title\x07");
        assert!(app.sync_terminal_titles());
        let info = app.pane_info(0, moved_pane).unwrap();
        assert_ne!(info.pane_id, old_public);
        assert_eq!(info.workspace_id, app.public_workspace_id(0));
        assert_eq!(info.tab_id, destination_tab);
        assert_eq!(info.terminal_id, moved_terminal.to_string());
        assert_eq!(info.terminal_title.as_deref(), Some("moved-title"));
        assert_eq!(info.revision, 2);
        assert_eq!(info.title.as_deref(), Some("presentation-3"));
        assert_eq!(info.tokens["kept"], "token-3");
        assert_eq!(updates(cursor), vec![info.clone()]);

        let detached = app.terminal_runtimes.remove(&moved_terminal).unwrap();
        detached.test_process_pty_bytes(b"\x1b]2;detached-new-title\x07");
        assert_eq!(
            detached.terminal_title().as_deref(),
            Some("detached-new-title")
        );
        let missing_state_id = &targets[4].2;
        let removed_state = app.state.terminals.remove(missing_state_id).unwrap();
        assert_eq!(removed_state.terminal_title(), Some(targets[4].3.as_str()));
        app.terminal_runtimes
            .get(missing_state_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]2;missing-state-new-title\x07");
        assert_eq!(
            app.terminal_runtimes
                .get(missing_state_id)
                .unwrap()
                .terminal_title()
                .as_deref(),
            Some("missing-state-new-title")
        );
        let cursor = hub.current_sequence();
        assert!(!app.sync_terminal_titles());
        assert!(updates(cursor).is_empty());
        let retained = app.pane_info(0, moved_pane).unwrap();
        assert_eq!(retained.terminal_title, info.terminal_title);
        assert_eq!(retained.revision, 2);
        assert_eq!(retained.title, info.title);
        assert_eq!(retained.tokens, info.tokens);
        assert!(!app.state.terminals.contains_key(missing_state_id));
    }

    #[tokio::test]
    async fn sync_keeps_latest_raw_title_and_emits_only_for_stripped_changes() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.state = AgentState::Working;
        let before = terminal.clone();
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        for (raw, stripped, revision, changed, emits) in [
            ("⠋ 修复🙂标题", Some("修复🙂标题"), 1, true, true),
            ("⠋ 修复🙂标题", Some("修复🙂标题"), 1, false, false),
            ("⠙ 修复🙂标题", Some("修复🙂标题"), 1, true, false),
            ("Done reviewing", Some("Done reviewing"), 2, true, true),
            ("", None, 3, true, true),
            ("", None, 3, false, false),
        ] {
            app.terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .test_process_pty_bytes(format!("\x1b]2;{raw}\x1b\\").as_bytes());
            let cursor = event_hub.current_sequence();
            assert_eq!(app.sync_terminal_titles(), changed, "{raw:?}");
            let pane = app.pane_info(0, pane_id).unwrap();
            assert_eq!(
                pane.terminal_title.as_deref(),
                (!raw.is_empty()).then_some(raw)
            );
            assert_eq!(pane.terminal_title_stripped.as_deref(), stripped);
            assert_eq!(pane.title, None);
            assert_eq!(pane.agent_status, AgentStatus::Working);
            assert_eq!(pane.revision, revision);
            let agent = app.collect_agent_infos().pop().unwrap();
            assert_eq!(agent.terminal_title, pane.terminal_title);
            assert_eq!(agent.terminal_title_stripped, pane.terminal_title_stripped);
            assert_eq!(agent.title, None);
            assert_eq!(agent.agent_status, AgentStatus::Working);
            assert_eq!(agent.revision, revision);
            let expected = if emits {
                vec![EventEnvelope {
                    event: EventKind::PaneUpdated,
                    data: EventData::PaneUpdated { pane },
                }]
            } else {
                Vec::new()
            };
            let suffix: Vec<_> = event_hub
                .events_after(cursor)
                .into_iter()
                .map(|(_, event)| event)
                .collect();
            assert_eq!(suffix, expected, "{raw:?}");
            let terminal = &app.state.terminals[&terminal_id];
            assert_eq!(terminal.agent_metadata, before.agent_metadata);
            assert_eq!(terminal.metadata_tokens, before.metadata_tokens);
            assert_eq!(terminal.state, before.state);
            assert_eq!(terminal.detected_agent, before.detected_agent);
            assert_eq!(terminal.hook_authority, before.hook_authority);
        }
    }
}
