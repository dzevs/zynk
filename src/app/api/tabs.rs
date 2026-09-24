// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabCreateParams, TabListParams,
    TabMoveParams, TabRenameParams, TabTarget,
};
use crate::app::{App, Mode};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_tab_list(&mut self, id: String, params: TabListParams) -> String {
        let tabs = if let Some(workspace_id) = params.workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            let Some(_) = self.state.workspaces.get(ws_idx) else {
                return workspace_not_found(id, &workspace_id);
            };
            self.tab_list_info(ws_idx)
        } else {
            let mut tabs = Vec::new();
            for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                for tab_idx in 0..ws.tabs.len() {
                    if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                        tabs.push(tab);
                    }
                }
            }
            tabs
        };

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_get(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_create(&mut self, id: String, params: TabCreateParams) -> String {
        let TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
        } = params;
        let ws_idx = if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
            self.resolve_new_terminal_cwd(self.focused_pane_cwd_in_workspace(ws_idx))
        });
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("workspace disappeared"))
            .and_then(|ws| {
                ws.create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                )
            });
        match result {
            Ok((tab_idx, terminal, runtime)) => {
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[tab_idx].root_pane,
                );
                if let Some(label) = label {
                    let workspace_id = self.state.workspaces[ws_idx].id.clone();
                    let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
                        crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
                    });
                    if let Some(tab) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.tabs.get_mut(tab_idx))
                    {
                        tab.set_custom_name(label);
                        crate::logging::tab_renamed(&workspace_id, &tab_id);
                    }
                }
                if focus {
                    self.state.switch_workspace_tab(ws_idx, tab_idx);
                    self.state.mode = Mode::Terminal;
                }
                self.schedule_session_save();
                self.emit_tab_created_events(ws_idx, tab_idx);
                encode_success(
                    id,
                    self.tab_created_result(ws_idx, tab_idx)
                        .expect("new tab should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "tab_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_tab_focus(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_rename(&mut self, id: String, params: TabRenameParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
            crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
        });
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.set_custom_name(params.label.clone());
        crate::logging::tab_renamed(&workspace_id, &tab_id);
        if self.state.active == Some(ws_idx) {
            // Refresh cached hit areas so the new label width takes effect immediately.
            self.state.refresh_tab_bar_view();
        }
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                workspace_id: self.public_workspace_id(ws_idx),
                label: params.label,
            },
        });
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_move(&mut self, id: String, params: TabMoveParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &params.tab_id);
        };
        if params.insert_index > ws.tabs.len() {
            return encode_error(
                id,
                "tab_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let tab_id = self
            .public_tab_id(ws_idx, tab_idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&ws.id, tab_idx + 1));
        let workspace_id = self.public_workspace_id(ws_idx);
        let insert_index = params.insert_index;
        let moved = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|ws| ws.move_tab(tab_idx, insert_index));
        let tabs = self.tab_list_info(ws_idx);
        if moved {
            self.schedule_session_save();
            if self.state.active == Some(ws_idx) {
                self.state.tab_scroll_follow_active = true;
                self.state.refresh_tab_bar_view();
            }
            self.emit_event(EventEnvelope {
                event: EventKind::TabMoved,
                data: EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index,
                    tabs: tabs.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_close(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        let closes_workspace = ws.tabs.len() <= 1;
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let pane_ids = self.state.pane_ids_for_tab(ws_idx, tab_idx);

        if closes_workspace {
            if self.state.confirm_implicit_worktree_group_close(ws_idx) {
                return encode_error(
                    id,
                    "confirmation_required",
                    "closing this tab would close a worktree group",
                );
            }
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::TabClosed,
                data: EventData::TabClosed {
                    tab_id,
                    workspace_id: workspace_id.clone(),
                },
            });
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed { workspace_id },
            });
            return encode_success(id, ResponseResult::Ok {});
        }

        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        if !ws.close_tab(tab_idx) {
            return encode_error(
                id,
                "tab_close_failed",
                format!("tab {} could not be closed", target.tab_id),
            );
        }
        self.state.remove_plugin_pane_records(pane_ids);
        self.state.remove_unattached_terminal_ids(terminal_ids);
        self.shutdown_detached_terminal_runtimes();
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id,
                workspace_id,
            },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    fn tab_list_info(&self, ws_idx: usize) -> Vec<crate::api::schema::TabInfo> {
        self.state
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                (0..ws.tabs.len())
                    .filter_map(|idx| self.tab_info(ws_idx, idx))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

fn tab_not_found(id: String, tab_id: &str) -> String {
    encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::schema::SuccessResponse, config::Config, workspace::Workspace};

    #[test]
    fn api_tab_close_last_tab_closes_workspace_and_emits_both_events() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let workspace_id = app.public_workspace_id(0);

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: tab_id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.active.is_none());
        let events = event_hub.events_after(0);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            [EventKind::TabClosed, EventKind::WorkspaceClosed]
        );
        assert!(matches!(
            &events[0].1.data,
            EventData::TabClosed {
                tab_id: closed_tab_id,
                workspace_id: closed_workspace_id,
            } if closed_tab_id == &tab_id && closed_workspace_id == &workspace_id
        ));
        assert!(matches!(
            &events[1].1.data,
            EventData::WorkspaceClosed { workspace_id: closed_workspace_id }
                if closed_workspace_id == &workspace_id
        ));
    }

    #[test]
    fn api_tab_move_reorders_tabs_in_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("two"));
        workspace.test_add_tab(Some("three"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_root = app.state.workspaces[0].tabs[0].root_pane;
        let moved_id = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_tab_move(
            "req".into(),
            TabMoveParams {
                tab_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabList { tabs } = success.result else {
            panic!("expected tab list");
        };
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(tabs[2].tab_id, app.public_tab_id(0, 2).unwrap());
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index: 3,
                    tabs,
                } if tab_id == &moved_id
                    && workspace_id == &app.public_workspace_id(0)
                    && tabs[2].tab_id == moved_id
            )
        }));
    }
    fn m824_app_with_tabs() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.tabs[0].set_custom_name("a".into());
        workspace.test_add_tab(Some("b"));
        workspace.test_add_tab(Some("c"));
        app.state.workspaces = vec![Workspace::test_new("background"), workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.tab_scroll_follow_active = false;
        app.state.view.tab_bar_rect = ratatui::layout::Rect::new(0, 0, 80, 1);
        app.state.refresh_tab_bar_view();
        app
    }

    #[test]
    fn api_tab_rename_reflows_active_tab_bar() {
        let mut observed_widths = Vec::new();
        for tab_idx in [0, 2] {
            let mut app = m824_app_with_tabs();
            assert_ne!(tab_idx, 1, "workspace and tab indices must differ");
            assert_eq!(app.state.workspaces[1].active_tab, 0);
            let tab_id = app.public_tab_id(1, tab_idx).unwrap();
            let workspace_id = app.public_workspace_id(1);
            let focused_pane = app.state.workspaces[1].focused_pane_id();
            let width_before = app.state.view.tab_hit_areas[tab_idx].width;
            assert!(width_before > 0);
            assert!(app.event_hub.events_after(0).is_empty());
            let label = "a much longer custom tab label";

            let response = app.handle_api_request(crate::api::schema::Request {
                id: "rename".into(),
                method: crate::api::schema::Method::TabRename(TabRenameParams {
                    tab_id: tab_id.clone(),
                    label: label.into(),
                }),
            });

            let width_after = app.state.view.tab_hit_areas[tab_idx].width;
            observed_widths.push((tab_idx, width_before, width_after));
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::TabInfo { tab } = success.result else {
                panic!("expected renamed tab info");
            };
            assert_eq!(success.id, "rename");
            assert_eq!(tab.tab_id, tab_id);
            assert_eq!(tab.workspace_id, workspace_id);
            assert_eq!(tab.label, label);
            assert_eq!(tab.focused, tab_idx == 0);
            assert_eq!(app.state.active, Some(1));
            assert_eq!(app.state.selected, 1);
            assert_eq!(app.state.workspaces[1].active_tab, 0);
            assert_eq!(app.state.workspaces[1].focused_pane_id(), focused_pane);
            assert!(!app.state.tab_scroll_follow_active);
            assert_eq!(app.state.tab_scroll, 0);
            assert_eq!(
                app.event_hub.events_after(0),
                vec![(
                    1,
                    EventEnvelope {
                        event: EventKind::TabRenamed,
                        data: EventData::TabRenamed {
                            tab_id,
                            workspace_id,
                            label: label.into(),
                        },
                    },
                )]
            );
        }
        assert_eq!(observed_widths.len(), 2);
        assert!(
            observed_widths
                .iter()
                .all(|(_, before, after)| after > before),
            "tab bar must reflow immediately for active and inactive tabs: {observed_widths:?}"
        );
    }

    #[test]
    fn m824_background_tab_rename_preserves_active_cache() {
        let mut app = m824_app_with_tabs();
        app.state.view.tab_hit_areas = vec![ratatui::layout::Rect::new(11, 0, 7, 1); 3];
        app.state.view.tab_scroll_left_hit_area = ratatui::layout::Rect::new(2, 0, 1, 1);
        app.state.view.tab_scroll_right_hit_area = ratatui::layout::Rect::new(70, 0, 1, 1);
        app.state.view.new_tab_hit_area = ratatui::layout::Rect::new(75, 0, 2, 1);
        let cache = (
            app.state.view.tab_hit_areas.clone(),
            app.state.view.tab_scroll_left_hit_area,
            app.state.view.tab_scroll_right_hit_area,
            app.state.view.new_tab_hit_area,
        );
        let focused_pane = app.state.workspaces[1].focused_pane_id();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let workspace_id = app.public_workspace_id(0);
        let label = "background tab renamed";

        let response = app.handle_tab_rename(
            "background".into(),
            TabRenameParams {
                tab_id: tab_id.clone(),
                label: label.into(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabInfo { tab } = success.result else {
            panic!("expected background tab info");
        };
        assert_eq!(success.id, "background");
        assert_eq!(tab.tab_id, tab_id);
        assert_eq!(tab.label, label);
        assert!(!tab.focused);
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some(label)
        );
        assert_eq!(
            (
                app.state.view.tab_hit_areas.clone(),
                app.state.view.tab_scroll_left_hit_area,
                app.state.view.tab_scroll_right_hit_area,
                app.state.view.new_tab_hit_area,
            ),
            cache,
            "background rename must not recompute the active cache"
        );
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces[1].active_tab, 0);
        assert_eq!(app.state.workspaces[1].focused_pane_id(), focused_pane);
        assert!(!app.state.tab_scroll_follow_active);
        assert_eq!(app.state.tab_scroll, 0);
        assert_eq!(
            app.event_hub.events_after(0),
            vec![(
                1,
                EventEnvelope {
                    event: EventKind::TabRenamed,
                    data: EventData::TabRenamed {
                        tab_id,
                        workspace_id,
                        label: label.into(),
                    },
                },
            )]
        );
    }

    #[test]
    fn m824_invalid_tab_rename_preserves_cache_name_and_focus() {
        let mut app = m824_app_with_tabs();
        let cache = (
            app.state.view.tab_hit_areas.clone(),
            app.state.view.tab_scroll_left_hit_area,
            app.state.view.tab_scroll_right_hit_area,
            app.state.view.new_tab_hit_area,
        );
        let focused_pane = app.state.workspaces[1].focused_pane_id();
        let names: Vec<_> = app.state.workspaces[1]
            .tabs
            .iter()
            .map(|tab| tab.custom_name.clone())
            .collect();

        let response = app.handle_tab_rename(
            "invalid".into(),
            TabRenameParams {
                tab_id: "missing-tab".into(),
                label: "must not rename anything".into(),
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.id, "invalid");
        assert_eq!(error.error.code, "tab_not_found");
        assert_eq!(
            app.state.workspaces[1]
                .tabs
                .iter()
                .map(|tab| tab.custom_name.clone())
                .collect::<Vec<_>>(),
            names
        );
        assert_eq!(
            (
                app.state.view.tab_hit_areas.clone(),
                app.state.view.tab_scroll_left_hit_area,
                app.state.view.tab_scroll_right_hit_area,
                app.state.view.new_tab_hit_area,
            ),
            cache
        );
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces[1].active_tab, 0);
        assert_eq!(app.state.workspaces[1].focused_pane_id(), focused_pane);
        assert!(!app.state.tab_scroll_follow_active);
        assert_eq!(app.state.tab_scroll, 0);
        assert!(app.event_hub.events_after(0).is_empty());
    }

    #[test]
    fn api_tab_close_clears_copy_mode_for_removed_panes() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        let mut workspace = Workspace::test_new("tabs");
        let closing_tab = workspace.test_add_tab(Some("closing"));
        let closing_pane = workspace.tabs[closing_tab].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: closing_pane,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let closing_tab_id = app.public_tab_id(0, closing_tab).unwrap();

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: closing_tab_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(app.state.copy_mode.is_none());
        app.state.assert_invariants_for_test();
    }
}
