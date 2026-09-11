// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use bytes::Bytes;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, PaneClearAgentAuthorityParams, PaneCurrentParams,
    PaneDirection, PaneEdgesParams, PaneEdgesResult, PaneFocusDirectionParams,
    PaneFocusDirectionReason, PaneFocusDirectionResult, PaneInfo, PaneLayoutPane, PaneLayoutParams,
    PaneLayoutRect, PaneLayoutSnapshot, PaneLayoutSplit, PaneListParams, PaneMoveDestination,
    PaneMoveParams, PaneMoveReason, PaneMoveResult, PaneNeighborParams, PaneNeighborResult,
    PaneProcessInfo, PaneProcessInfoParams, PaneProcessInfoProcess, PaneReadParams, PaneReadResult,
    PaneReleaseAgentParams, PaneRenameParams, PaneReportAgentParams, PaneReportAgentSessionParams,
    PaneReportMetadataParams, PaneResizeParams, PaneResizeReason, PaneResizeResult,
    PaneSendInputParams, PaneSendKeysParams, PaneSendTextParams, PaneSplitParams, PaneSwapParams,
    PaneSwapReason, PaneSwapResult, PaneTarget, PaneZoomMode, PaneZoomParams, PaneZoomReason,
    PaneZoomResult, ReadFormat, ReadSource, ResponseResult,
};
use crate::app::actions::{PaneZoomCommand, PaneZoomNoopReason};
use crate::app::{App, Mode};
use crate::layout::{find_in_direction, NavDirection, PaneId};

use super::super::api_helpers::{
    detect_state_from_api, encode_api_keys, encode_api_text, normalize_custom_status,
    normalize_reported_agent_label,
};
use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_pane_split(&mut self, id: String, params: PaneSplitParams) -> String {
        let target = if let Some(target_pane_id) = params.target_pane_id.as_deref() {
            self.parse_pane_id(target_pane_id)
        } else if let Some(workspace_id) = params.workspace_id.as_deref() {
            self.parse_workspace_id(workspace_id).and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        } else {
            self.state.active.and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        };
        let Some((ws_idx, target_pane_id)) = target else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let split_cwd = params.cwd.map(std::path::PathBuf::from).or_else(|| {
            let follow_cwd = self.state.workspaces.get(ws_idx).and_then(|ws| {
                let tab_idx = ws.find_tab_index_for_pane(target_pane_id)?;
                ws.tabs.get(tab_idx)?.cwd_for_pane(
                    target_pane_id,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            });
            Some(self.resolve_new_terminal_cwd(follow_cwd))
        });
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let previous_focus = self.state.current_pane_focus_target();
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let direction = match params.direction {
            crate::api::schema::SplitDirection::Right => ratatui::layout::Direction::Horizontal,
            crate::api::schema::SplitDirection::Down => ratatui::layout::Direction::Vertical,
        };
        let shell_config = crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode);
        let split_result = match params.ratio {
            Some(ratio) => ws.split_pane_with_ratio(
                target_pane_id,
                direction,
                ratio,
                rows,
                cols,
                split_cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                params.focus,
            ),
            None => ws.split_pane(
                target_pane_id,
                direction,
                rows,
                cols,
                split_cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                params.focus,
            ),
        };
        let (target_tab_idx, new_pane) = match split_result {
            Some(Ok(result)) => result,
            Some(Err(err)) => return encode_error(id, "pane_split_failed", err.to_string()),
            None => return encode_error(id, "pane_not_found", "pane not found"),
        };
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, target_tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, new_pane.pane_id);
            self.state.settle_terminal_mode_after_focus();
        }
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        self.schedule_session_save();
        let pane = self.pane_info(ws_idx, new_pane.pane_id).unwrap();
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: pane.clone() },
        });

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_list(&mut self, id: String, params: PaneListParams) -> String {
        match self.collect_panes_for_workspace(params.workspace_id.as_deref()) {
            Ok(panes) => encode_success(id, ResponseResult::PaneList { panes }),
            Err((code, message)) => encode_error(id, &code, message),
        }
    }

    pub(super) fn handle_pane_current(&mut self, id: String, params: PaneCurrentParams) -> String {
        let target = match params.caller_pane_id.as_deref() {
            Some(caller_pane_id) => self.parse_pane_id(caller_pane_id),
            None => self.resolve_optional_pane(None),
        };
        let Some((ws_idx, pane_id)) = target else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };

        encode_success(id, ResponseResult::PaneCurrent { pane })
    }

    pub(super) fn handle_pane_get(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_focus(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(_tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };

        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.state.settle_terminal_mode_after_focus();

        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_layout(&mut self, id: String, params: PaneLayoutParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(id, ResponseResult::PaneLayout { layout })
    }

    pub(super) fn handle_pane_process_info(
        &mut self,
        id: String,
        params: PaneProcessInfoParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some((runtime, _workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let shell_pid = runtime.child_pid();
        let foreground_job = shell_pid.and_then(crate::detect::foreground_job);
        let foreground_process_group_id = foreground_job.as_ref().map(|job| job.process_group_id);
        let foreground_processes = foreground_job
            .map(|job| {
                job.processes
                    .into_iter()
                    .map(|process| PaneProcessInfoProcess {
                        pid: process.pid,
                        name: process.name,
                        argv0: process.argv0,
                        argv: process.argv,
                        cmdline: process.cmdline,
                        cwd: crate::platform::process_cwd(process.pid)
                            .map(|cwd| cwd.display().to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default();

        encode_success(
            id,
            ResponseResult::PaneProcessInfo {
                process_info: PaneProcessInfo {
                    pane_id: public_pane_id,
                    shell_pid,
                    foreground_process_group_id,
                    tty: None,
                    foreground_processes,
                },
            },
        )
    }

    pub(super) fn handle_pane_neighbor(
        &mut self,
        id: String,
        params: PaneNeighborParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(source_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let neighbor_pane_id = self
            .directional_pane_target(ws_idx, tab_idx, pane_id, params.direction)
            .and_then(|pane_id| self.public_pane_id(ws_idx, pane_id));
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneNeighbor {
                neighbor: PaneNeighborResult {
                    pane_id: source_public_id,
                    direction: params.direction,
                    neighbor_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_edges(&mut self, id: String, params: PaneEdgesParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(tab) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let area = self.state.view.terminal_area;
        let Some(info) = tab
            .layout
            .panes(area)
            .into_iter()
            .find(|info| info.id == pane_id)
        else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneEdges {
                edges: PaneEdgesResult {
                    pane_id: pane_public_id,
                    left: info.rect.x <= area.x,
                    right: info.rect.x + info.rect.width >= area.x + area.width,
                    up: info.rect.y <= area.y,
                    down: info.rect.y + info.rect.height >= area.y + area.height,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_focus_direction(
        &mut self,
        id: String,
        params: PaneFocusDirectionParams,
    ) -> String {
        let Some((ws_idx, source_pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref())
        else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(source_pane_id)
        else {
            return pane_not_found(
                id,
                &self
                    .public_pane_id(ws_idx, source_pane_id)
                    .unwrap_or_default(),
            );
        };
        let Some(source_public_id) = self.public_pane_id(ws_idx, source_pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let target =
            self.directional_pane_target(ws_idx, tab_idx, source_pane_id, params.direction);
        let reason = target
            .is_none()
            .then_some(PaneFocusDirectionReason::NoNeighbor);

        if let Some(target_pane_id) = target {
            self.state.focus_pane_in_workspace(ws_idx, target_pane_id);
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.state.settle_terminal_mode_after_focus();
        }
        let focused_pane_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .map(|tab| tab.layout.focused())
            .and_then(|pane_id| self.public_pane_id(ws_idx, pane_id));
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneFocusDirection {
                focus: PaneFocusDirectionResult {
                    changed: target.is_some(),
                    reason,
                    source_pane_id: source_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_resize(&mut self, id: String, params: PaneResizeParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };

        let amount = params
            .amount
            .filter(|amount| amount.is_finite())
            .unwrap_or(0.05)
            .abs()
            .min(0.5);
        let direction: NavDirection = params.direction.into();
        let area = self.state.view.terminal_area;
        let changed = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
            .is_some_and(|tab| tab.layout.resize_pane(pane_id, direction, amount, area));
        if changed {
            self.schedule_session_save();
        }

        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();

        encode_success(
            id,
            ResponseResult::PaneResize {
                resize: PaneResizeResult {
                    changed,
                    reason: (!changed).then_some(PaneResizeReason::Unchanged),
                    pane_id: pane_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_swap(&mut self, id: String, params: PaneSwapParams) -> String {
        let directional = params.direction.is_some();
        let explicit = params.source_pane_id.is_some() || params.target_pane_id.is_some();
        if directional == explicit {
            return encode_error(
                id,
                "invalid_pane_swap",
                "provide either direction with optional pane_id, or source_pane_id and target_pane_id",
            );
        }

        let (ws_idx, tab_idx, source_pane_id, target_pane_id, reason) = if let Some(direction) =
            params.direction
        {
            let Some((ws_idx, source_pane_id)) =
                self.resolve_swap_source(params.pane_id.as_deref())
            else {
                return encode_error(id, "pane_not_found", "source pane not found");
            };
            let Some(tab_idx) =
                self.state.workspaces[ws_idx].find_tab_index_for_pane(source_pane_id)
            else {
                return pane_not_found(
                    id,
                    &self
                        .public_pane_id(ws_idx, source_pane_id)
                        .unwrap_or_default(),
                );
            };
            let target = self.directional_pane_target(ws_idx, tab_idx, source_pane_id, direction);
            match target {
                Some(target_pane_id) => {
                    (ws_idx, tab_idx, source_pane_id, Some(target_pane_id), None)
                }
                None => (
                    ws_idx,
                    tab_idx,
                    source_pane_id,
                    None,
                    Some(PaneSwapReason::NoNeighbor),
                ),
            }
        } else {
            let Some(source_raw) = params.source_pane_id.as_deref() else {
                return encode_error(id, "invalid_pane_swap", "missing source_pane_id");
            };
            let Some(target_raw) = params.target_pane_id.as_deref() else {
                return encode_error(id, "invalid_pane_swap", "missing target_pane_id");
            };
            let source = self
                .parse_pane_id(source_raw)
                .and_then(|(ws_idx, pane_id)| {
                    let tab_idx = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
                    Some((ws_idx, tab_idx, pane_id))
                });
            let target = self
                .parse_pane_id(target_raw)
                .and_then(|(ws_idx, pane_id)| {
                    let tab_idx = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
                    Some((ws_idx, tab_idx, pane_id))
                });
            let response_context = source
                .map(|(ws_idx, tab_idx, _)| (ws_idx, tab_idx))
                .or_else(|| target.map(|(ws_idx, tab_idx, _)| (ws_idx, tab_idx)))
                .or_else(|| {
                    let ws_idx = self.state.active?;
                    let tab_idx = self.state.workspaces.get(ws_idx)?.active_tab_index();
                    Some((ws_idx, tab_idx))
                });
            let Some((ws_idx, tab_idx)) = response_context else {
                return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
            };
            let source_pane_id = source
                .map(|(_, _, pane_id)| pane_id)
                .or_else(|| {
                    self.state
                        .workspaces
                        .get(ws_idx)?
                        .tabs
                        .get(tab_idx)
                        .map(|tab| tab.layout.focused())
                })
                .unwrap_or(PaneId::from_raw(0));
            let target_pane_id = target.map(|(_, _, pane_id)| pane_id);
            let reason = match (source, target) {
                (None, _) | (_, None) => Some(PaneSwapReason::NotFound),
                (Some((_, _, source)), Some((_, _, target))) if source == target => {
                    Some(PaneSwapReason::SamePane)
                }
                (Some((source_ws, source_tab, _)), Some((target_ws, target_tab, _)))
                    if source_ws != target_ws || source_tab != target_tab =>
                {
                    Some(PaneSwapReason::CrossTab)
                }
                _ => None,
            };
            (ws_idx, tab_idx, source_pane_id, target_pane_id, reason)
        };

        let mut changed = false;
        if reason.is_none() {
            if let Some(target_pane_id) = target_pane_id {
                let previous_focus = self.state.current_pane_focus_target();
                if let Some(tab) = self
                    .state
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|ws| ws.tabs.get_mut(tab_idx))
                {
                    changed = tab.layout.swap_panes(source_pane_id, target_pane_id);
                    tab.layout.focus_pane(source_pane_id);
                    if changed {
                        self.state.switch_workspace_tab(ws_idx, tab_idx);
                        self.state
                            .record_pane_focus_change(previous_focus, ws_idx, source_pane_id);
                        self.state.mark_session_dirty();
                        self.schedule_session_save();
                    }
                }
            }
        }

        let source_public_id = match params.source_pane_id {
            Some(raw) => self
                .parse_pane_id(&raw)
                .and_then(|(idx, pane_id)| {
                    self.state
                        .workspaces
                        .get(idx)?
                        .find_tab_index_for_pane(pane_id)?;
                    self.public_pane_id(idx, pane_id)
                })
                .unwrap_or(raw),
            None => self
                .public_pane_id(ws_idx, source_pane_id)
                .unwrap_or_default(),
        };
        let target_public_id = match params.target_pane_id {
            Some(raw) => self
                .parse_pane_id(&raw)
                .and_then(|(idx, pane_id)| {
                    self.state
                        .workspaces
                        .get(idx)?
                        .find_tab_index_for_pane(pane_id)?;
                    self.public_pane_id(idx, pane_id)
                })
                .or(Some(raw)),
            None => target_pane_id.and_then(|pane_id| self.public_pane_id(ws_idx, pane_id)),
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();

        encode_success(
            id,
            ResponseResult::PaneSwap {
                swap: PaneSwapResult {
                    changed,
                    reason,
                    source_pane_id: source_public_id,
                    target_pane_id: target_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_move(&mut self, id: String, params: PaneMoveParams) -> String {
        let PaneMoveParams {
            pane_id,
            destination,
            focus,
        } = params;
        let Some((source_ws_idx, source_pane_id)) = self.parse_pane_id(&pane_id) else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let Some(source_tab_idx) =
            self.state.workspaces[source_ws_idx].find_tab_index_for_pane(source_pane_id)
        else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let previous_pane_id = self
            .public_pane_id(source_ws_idx, source_pane_id)
            .unwrap_or_else(|| pane_id.clone());
        let previous_workspace_id = self.public_workspace_id(source_ws_idx);
        let Some(previous_tab_id) = self.public_tab_id(source_ws_idx, source_tab_idx) else {
            return encode_error(id, "tab_not_found", "source tab not found");
        };
        let Some(source_terminal_id) = self
            .state
            .workspaces
            .get(source_ws_idx)
            .and_then(|ws| ws.tabs.get(source_tab_idx))
            .and_then(|tab| tab.terminal_id(source_pane_id))
            .cloned()
        else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let recovery_context = PaneMoveRecoveryContext {
            source_ws_idx,
            previous_workspace_id: previous_workspace_id.clone(),
            previous_workspace_label: self.state.workspaces[source_ws_idx].custom_name.clone(),
            previous_tab_label: self.state.workspaces[source_ws_idx].tabs[source_tab_idx]
                .custom_name
                .clone(),
            previous_worktree_space: self.state.workspaces[source_ws_idx].worktree_space.clone(),
            identity_cwd: self.state.workspaces[source_ws_idx].identity_cwd.clone(),
        };

        if self.state.workspaces[source_ws_idx].tabs[source_tab_idx].zoomed {
            let Some(layout) = self.pane_layout_snapshot(source_ws_idx, source_tab_idx) else {
                return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
            };
            let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                return encode_error(id, "pane_not_found", "source pane not found");
            };
            return encode_unchanged_pane_move(
                id,
                PaneMoveReason::ZoomedTab,
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane,
                Some(layout.clone()),
                layout,
            );
        }

        let resolved = match destination {
            PaneMoveDestination::Tab {
                tab_id,
                target_pane_id,
                split,
                ratio,
            } => {
                let Some((target_ws_idx, target_tab_idx)) = self.parse_tab_id(&tab_id) else {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
                };
                if source_ws_idx == target_ws_idx && source_tab_idx == target_tab_idx {
                    let Some(layout) = self.pane_layout_snapshot(source_ws_idx, source_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                        return encode_error(id, "pane_not_found", "source pane not found");
                    };
                    return encode_unchanged_pane_move(
                        id,
                        PaneMoveReason::SameTab,
                        previous_pane_id,
                        previous_workspace_id,
                        previous_tab_id,
                        pane,
                        Some(layout.clone()),
                        layout,
                    );
                }
                if self.state.workspaces[target_ws_idx].tabs[target_tab_idx].zoomed {
                    let Some(source_layout) =
                        self.pane_layout_snapshot(source_ws_idx, source_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(target_layout) =
                        self.pane_layout_snapshot(target_ws_idx, target_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                        return encode_error(id, "pane_not_found", "source pane not found");
                    };
                    return encode_unchanged_pane_move(
                        id,
                        PaneMoveReason::ZoomedTab,
                        previous_pane_id,
                        previous_workspace_id,
                        previous_tab_id,
                        pane,
                        Some(source_layout),
                        target_layout,
                    );
                }
                let target_pane_id = match target_pane_id {
                    Some(raw) => {
                        let Some((pane_ws_idx, pane_id)) = self.parse_pane_id(&raw) else {
                            return encode_error(
                                id,
                                "target_pane_not_found",
                                format!("target pane {raw} not found"),
                            );
                        };
                        let pane_tab_idx =
                            self.state.workspaces[pane_ws_idx].find_tab_index_for_pane(pane_id);
                        if pane_ws_idx != target_ws_idx || pane_tab_idx != Some(target_tab_idx) {
                            return encode_error(
                                id,
                                "target_pane_not_found",
                                format!("target pane {raw} is not in tab {tab_id}"),
                            );
                        }
                        pane_id
                    }
                    None => self.state.workspaces[target_ws_idx].tabs[target_tab_idx]
                        .layout
                        .focused(),
                };
                let Some(target_tab_id) = self.public_tab_id(target_ws_idx, target_tab_idx) else {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
                };
                ResolvedPaneMoveDestination::ExistingTab {
                    tab_id: target_tab_id,
                    target_pane_id,
                    split,
                    ratio: ratio.unwrap_or(0.5),
                    cross_workspace: source_ws_idx != target_ws_idx,
                }
            }
            PaneMoveDestination::NewTab {
                workspace_id,
                label,
            } => {
                let target_workspace_id = if let Some(workspace_id) = workspace_id {
                    let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                        return encode_error(
                            id,
                            "workspace_not_found",
                            format!("workspace {workspace_id} not found"),
                        );
                    };
                    self.public_workspace_id(ws_idx)
                } else {
                    previous_workspace_id.clone()
                };
                ResolvedPaneMoveDestination::NewTab {
                    workspace_id: target_workspace_id,
                    label,
                }
            }
            PaneMoveDestination::NewWorkspace { label, tab_label } => {
                ResolvedPaneMoveDestination::NewWorkspace { label, tab_label }
            }
        };

        let mut previous_focus = self.state.current_pane_focus_target();
        let taken = match self
            .state
            .workspaces
            .get_mut(source_ws_idx)
            .and_then(|ws| ws.take_pane_for_move(source_pane_id))
        {
            Some(taken) => taken,
            None => return encode_error(id, "pane_move_failed", "source pane could not be moved"),
        };
        let source_removed_tab_id = taken.removed_tab_idx.map(|_| previous_tab_id.clone());
        let source_workspace_empty = taken.workspace_empty;
        let moved = taken.moved;
        let cross_workspace = match &resolved {
            ResolvedPaneMoveDestination::ExistingTab {
                cross_workspace, ..
            } => *cross_workspace,
            ResolvedPaneMoveDestination::NewTab { workspace_id, .. } => {
                workspace_id != &previous_workspace_id
            }
            ResolvedPaneMoveDestination::NewWorkspace { .. } => true,
        };
        if cross_workspace {
            if let Some(ws) = self.state.workspaces.get_mut(source_ws_idx) {
                ws.unregister_moved_pane(source_pane_id);
            }
            self.state
                .public_pane_id_aliases
                .insert(previous_pane_id.clone(), source_pane_id);
        }

        let mut closed_workspace_id = None;
        if source_workspace_empty && cross_workspace {
            self.state.workspaces.remove(source_ws_idx);
            closed_workspace_id = Some(previous_workspace_id.clone());
            if self.state.workspaces.is_empty() {
                self.state.active = None;
                self.state.selected = 0;
            } else {
                if let Some(active) = self.state.active {
                    if active == source_ws_idx {
                        self.state.active =
                            Some(source_ws_idx.min(self.state.workspaces.len() - 1));
                    } else if active > source_ws_idx {
                        self.state.active = Some(active - 1);
                    }
                }
                if self.state.selected == source_ws_idx {
                    self.state.selected = source_ws_idx.min(self.state.workspaces.len() - 1);
                } else if self.state.selected > source_ws_idx {
                    self.state.selected -= 1;
                }
            }
        }

        let mut created_workspace = false;
        let mut created_tab = false;
        let (target_ws_idx, target_tab_idx, moved_pane_id) = match resolved {
            ResolvedPaneMoveDestination::ExistingTab {
                tab_id,
                target_pane_id,
                split,
                ratio,
                cross_workspace: _,
            } => {
                let Some((target_ws_idx, target_tab_idx)) = self.parse_tab_id(&tab_id) else {
                    self.recover_failed_pane_move(recovery_context, moved);
                    return encode_error(id, "pane_move_failed", "target tab disappeared");
                };
                let direction = split_direction_to_layout(split);
                let moved_pane_id = match self.state.workspaces[target_ws_idx]
                    .insert_moved_pane_into_tab(
                        target_tab_idx,
                        target_pane_id,
                        moved,
                        direction,
                        ratio,
                        focus,
                    ) {
                    Ok(pane_id) => pane_id,
                    Err(moved) => {
                        self.recover_failed_pane_move(recovery_context, moved);
                        return encode_error(
                            id,
                            "pane_move_failed",
                            "target pane could not be split",
                        );
                    }
                };
                (target_ws_idx, target_tab_idx, moved_pane_id)
            }
            ResolvedPaneMoveDestination::NewTab {
                workspace_id,
                label,
            } => {
                let Some(target_ws_idx) = self.parse_workspace_id(&workspace_id) else {
                    self.recover_failed_pane_move(recovery_context, moved);
                    return encode_error(id, "pane_move_failed", "target workspace disappeared");
                };
                let moved_pane_id = moved.pane_id;
                let target_tab_idx = self.state.workspaces[target_ws_idx]
                    .create_tab_from_existing_pane(
                        moved,
                        label,
                        self.event_tx.clone(),
                        self.render_notify.clone(),
                        self.render_dirty.clone(),
                    );
                created_tab = true;
                (target_ws_idx, target_tab_idx, moved_pane_id)
            }
            ResolvedPaneMoveDestination::NewWorkspace { label, tab_label } => {
                let identity_cwd = self
                    .state
                    .terminals
                    .get(&source_terminal_id)
                    .map(|terminal| terminal.cwd.clone())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
                let moved_pane_id = moved.pane_id;
                let workspace = crate::workspace::Workspace::from_existing_pane(
                    label,
                    tab_label,
                    identity_cwd,
                    moved,
                    self.event_tx.clone(),
                    self.render_notify.clone(),
                    self.render_dirty.clone(),
                );
                self.state.workspaces.push(workspace);
                let target_ws_idx = self.state.workspaces.len() - 1;
                created_workspace = true;
                created_tab = true;
                (target_ws_idx, 0, moved_pane_id)
            }
        };

        if cross_workspace {
            let destination_workspace_id = self.state.workspaces[target_ws_idx].id.clone();
            self.reconcile_cross_workspace_pane_move(
                source_ws_idx,
                closed_workspace_id.is_some(),
                moved_pane_id,
                &destination_workspace_id,
                &mut previous_focus,
            );
        }
        let preserve_relocation_modal = cross_workspace
            && match self.state.mode {
                Mode::RenameWorkspace => self.state.pending_workspace_create_cwd.is_some(),
                Mode::RenamePane => self.state.rename_pane_target == Some(moved_pane_id),
                Mode::NewLinkedWorktree => self.state.worktree_create.is_some(),
                Mode::OpenExistingWorktree => self.state.worktree_open.is_some(),
                Mode::ConfirmRemoveWorktree => self.state.worktree_remove.is_some(),
                _ => false,
            };

        if focus || self.state.active.is_none() {
            let preserved_focus_history =
                cross_workspace.then(|| self.state.previous_pane_focus.clone());
            self.state
                .switch_workspace_tab(target_ws_idx, target_tab_idx);
            if let Some(previous_pane_focus) = preserved_focus_history {
                self.state.previous_pane_focus = previous_pane_focus;
            }
            self.state
                .record_pane_focus_change(previous_focus, target_ws_idx, moved_pane_id);
            if !preserve_relocation_modal {
                self.state.settle_terminal_mode_after_focus();
            }
        }
        let created_workspace = created_workspace.then(|| self.workspace_info(target_ws_idx));
        let created_tab = if created_tab {
            self.tab_info(target_ws_idx, target_tab_idx)
        } else {
            None
        };

        self.state.remove_alias_shadowed_by_new_pane(moved_pane_id);
        self.state.mark_session_dirty();
        self.schedule_session_save();
        let Some(pane) = self.pane_info(target_ws_idx, moved_pane_id) else {
            return encode_error(id, "pane_move_failed", "moved pane is unavailable");
        };
        let source_layout = if closed_workspace_id.is_none() {
            self.parse_tab_id(&previous_tab_id)
                .and_then(|(ws_idx, tab_idx)| self.pane_layout_snapshot(ws_idx, tab_idx))
        } else {
            None
        };
        let Some(target_layout) = self.pane_layout_snapshot(target_ws_idx, target_tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = target_layout.focused_pane_id.clone();
        let move_result = PaneMoveResult {
            changed: true,
            reason: None,
            previous_pane_id: previous_pane_id.clone(),
            previous_workspace_id: previous_workspace_id.clone(),
            previous_tab_id: previous_tab_id.clone(),
            pane: Box::new(pane.clone()),
            source_layout: source_layout.map(Box::new),
            target_layout: Box::new(target_layout),
            created_workspace: created_workspace.clone(),
            created_tab: created_tab.clone(),
            closed_workspace_id: closed_workspace_id.clone(),
            closed_tab_id: source_removed_tab_id.clone(),
            focused_pane_id,
        };
        if let Some(closed_tab_id) = &source_removed_tab_id {
            self.emit_event(EventEnvelope {
                event: EventKind::TabClosed,
                data: EventData::TabClosed {
                    tab_id: closed_tab_id.clone(),
                    workspace_id: previous_workspace_id.clone(),
                },
            });
        }
        if let Some(closed_workspace_id) = &closed_workspace_id {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id: closed_workspace_id.clone(),
                },
            });
        }
        if let Some(workspace) = &created_workspace {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceCreated,
                data: EventData::WorkspaceCreated {
                    workspace: workspace.clone(),
                },
            });
        }
        if let Some(tab) = &created_tab {
            self.emit_event(EventEnvelope {
                event: EventKind::TabCreated,
                data: EventData::TabCreated { tab: tab.clone() },
            });
        }
        self.emit_event(EventEnvelope {
            event: EventKind::PaneMoved,
            data: EventData::PaneMoved {
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane: Box::new(pane),
                created_workspace,
                created_tab,
                closed_workspace_id,
                closed_tab_id: source_removed_tab_id,
            },
        });

        encode_success(id, ResponseResult::PaneMove { move_result })
    }

    fn reconcile_cross_workspace_pane_move(
        &mut self,
        source_ws_idx: usize,
        source_workspace_removed: bool,
        moved_pane_id: PaneId,
        destination_workspace_id: &str,
        previous_focus: &mut Option<crate::app::state::PaneFocusTarget>,
    ) {
        let rebind_moved_pane = |target: &mut crate::app::state::PaneFocusTarget| {
            if target.pane_id == moved_pane_id {
                target.workspace_id = destination_workspace_id.to_string();
            }
        };
        if let Some(target) = previous_focus {
            rebind_moved_pane(target);
        }
        if let Some(target) = &mut self.state.previous_pane_focus {
            rebind_moved_pane(target);
        }
        if let Some(target) = self
            .state
            .toast
            .as_mut()
            .and_then(|toast| toast.target.as_mut())
            .filter(|target| target.pane_id == moved_pane_id)
        {
            target.workspace_id = destination_workspace_id.to_string();
        }
        if let Some(pending) = self
            .state
            .pending_agent_notifications
            .get_mut(&moved_pane_id)
        {
            pending.workspace_id = destination_workspace_id.to_string();
        }

        self.last_focus = self.last_focus.and_then(|(_, pane_id)| {
            self.state
                .workspaces
                .iter()
                .position(|ws| ws.find_tab_index_for_pane(pane_id).is_some())
                .map(|ws_idx| (ws_idx, pane_id))
        });

        if self
            .state
            .copy_mode
            .as_ref()
            .is_some_and(|copy| copy.pane_id == moved_pane_id)
        {
            self.state.copy_mode = None;
            if self.state.mode == Mode::Copy {
                self.state.mode = Mode::Terminal;
            }
        }
        if self
            .state
            .selection
            .as_ref()
            .is_some_and(|selection| selection.pane_id == moved_pane_id)
        {
            self.state.clear_selection();
            self.selection_autoscroll_deadline = None;
        }

        self.state.workspace_presses.clear();
        self.state.tab_presses.clear();
        self.state.drag = None;
        self.state.context_menu = None;

        let cancel_rename_workspace = self.state.mode == Mode::RenameWorkspace
            && self.state.pending_workspace_create_cwd.is_none();
        let cancel_rename_tab = self.state.mode == Mode::RenameTab;
        let moved_rename_pane_target = self.state.rename_pane_target == Some(moved_pane_id);
        let preserve_rename_pane = self.state.mode == Mode::RenamePane && moved_rename_pane_target;
        if moved_rename_pane_target && !preserve_rename_pane {
            self.state.rename_pane_target = None;
            self.state.name_input.clear();
            self.state.name_input_replace_on_type = false;
        }
        let cancel_index_bound_mode = cancel_rename_workspace
            || cancel_rename_tab
            || matches!(
                self.state.mode,
                Mode::Resize | Mode::ConfirmClose | Mode::ContextMenu
            );
        if cancel_index_bound_mode {
            if cancel_rename_tab {
                self.state.creating_new_tab = false;
                self.state.requested_new_tab_name = None;
            }
            self.state.name_input.clear();
            self.state.name_input_replace_on_type = false;
            self.state.mode = if self.state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }

        if source_workspace_removed {
            rebase_removed_workspace_index(
                &mut self.state.request_new_linked_worktree,
                source_ws_idx,
            );
            rebase_removed_workspace_index(
                &mut self.state.request_open_existing_worktree,
                source_ws_idx,
            );
            rebase_removed_workspace_index(
                &mut self.state.request_remove_linked_worktree,
                source_ws_idx,
            );
            if let Some(open) = &mut self.state.worktree_open {
                for entry in &mut open.entries {
                    rebase_removed_workspace_index(&mut entry.already_open_ws_idx, source_ws_idx);
                }
            }
        }

        let workspace_exists = |workspace_id: &str| {
            self.state
                .workspaces
                .iter()
                .any(|workspace| workspace.id == workspace_id)
        };
        let drop_create = self
            .state
            .worktree_create
            .as_ref()
            .is_some_and(|state| !workspace_exists(&state.source_workspace_id));
        let drop_open = self
            .state
            .worktree_open
            .as_ref()
            .is_some_and(|state| !workspace_exists(&state.source_workspace_id));
        let drop_remove = self
            .state
            .worktree_remove
            .as_ref()
            .is_some_and(|state| !workspace_exists(&state.workspace_id));
        if drop_create {
            self.state.worktree_create = None;
            self.state.request_submit_worktree_create = false;
            if self.state.mode == Mode::NewLinkedWorktree {
                self.state.mode = Mode::Terminal;
            }
        }
        if drop_open {
            self.state.worktree_open = None;
            self.state.request_submit_worktree_open = false;
            if self.state.mode == Mode::OpenExistingWorktree {
                self.state.mode = Mode::Terminal;
            }
        }
        if drop_remove {
            self.state.worktree_remove = None;
            self.state.request_submit_worktree_remove = false;
            if self.state.mode == Mode::ConfirmRemoveWorktree {
                self.state.mode = Mode::Terminal;
            }
        }

        self.state.navigator.selected = 0;
        self.state.navigator.scroll = 0;
        self.state.view.workspace_card_areas.clear();
        self.state.view.tab_hit_areas.clear();
        self.state.view.tab_scroll_left_hit_area = ratatui::layout::Rect::default();
        self.state.view.tab_scroll_right_hit_area = ratatui::layout::Rect::default();
        self.state.view.new_tab_hit_area = ratatui::layout::Rect::default();
        self.state.view.pane_infos.clear();
        self.state.view.split_borders.clear();
        self.state.view.toast_hit_area = ratatui::layout::Rect::default();
        self.last_pane_click = None;
    }

    fn recover_failed_pane_move(
        &mut self,
        context: PaneMoveRecoveryContext,
        moved: crate::workspace::MovedPane,
    ) {
        if let Some(ws_idx) = self.parse_workspace_id(&context.previous_workspace_id) {
            self.state.workspaces[ws_idx].create_tab_from_existing_pane(
                moved,
                context.previous_tab_label,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
        } else {
            let mut workspace = crate::workspace::Workspace::from_existing_pane(
                context.previous_workspace_label,
                context.previous_tab_label,
                context.identity_cwd,
                moved,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
            workspace.id = context.previous_workspace_id;
            workspace.worktree_space = context.previous_worktree_space;
            let insert_idx = context.source_ws_idx.min(self.state.workspaces.len());
            if let Some(active) = self.state.active {
                if active >= insert_idx {
                    self.state.active = Some(active + 1);
                }
            }
            if self.state.selected >= insert_idx && !self.state.workspaces.is_empty() {
                self.state.selected += 1;
            }
            self.state.workspaces.insert(insert_idx, workspace);
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
    }

    pub(super) fn handle_pane_zoom(&mut self, id: String, params: PaneZoomParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let command = match params.mode {
            PaneZoomMode::Toggle => PaneZoomCommand::Toggle,
            PaneZoomMode::On => PaneZoomCommand::On,
            PaneZoomMode::Off => PaneZoomCommand::Off,
        };
        let Some(outcome) = self.state.apply_pane_zoom(ws_idx, pane_id, command) else {
            return pane_not_found(id, &pane_public_id);
        };
        if outcome.changed || outcome.focus_changed {
            self.schedule_session_save();
        }
        self.state.settle_terminal_mode_after_focus();
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();

        encode_success(
            id,
            ResponseResult::PaneZoom {
                zoom: PaneZoomResult {
                    changed: outcome.changed || outcome.focus_changed,
                    zoom_changed: outcome.changed,
                    focus_changed: outcome.focus_changed,
                    reason: outcome.reason.map(|reason| match reason {
                        PaneZoomNoopReason::SinglePane => PaneZoomReason::SinglePane,
                        PaneZoomNoopReason::AlreadyZoomed => PaneZoomReason::AlreadyZoomed,
                        PaneZoomNoopReason::AlreadyUnzoomed => PaneZoomReason::AlreadyUnzoomed,
                    }),
                    pane_id: pane_public_id,
                    focused_pane_id,
                    zoomed: outcome.zoomed,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_rename(&mut self, id: String, params: PaneRenameParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        match params.label.map(|label| label.trim().to_string()) {
            Some(label) if !label.is_empty() => terminal.set_manual_label(label),
            _ => terminal.clear_manual_label(),
        }
        self.state.mark_session_dirty();
        let pane = self.pane_info(ws_idx, pane_id).unwrap();

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_read(&mut self, id: String, params: PaneReadParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
        else {
            return pane_not_found(id, &params.pane_id);
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
                    pane_id: public_pane_id,
                    workspace_id,
                    tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                    source: params.source,
                    format: params.format,
                    text,
                    revision: 0,
                    truncated: false,
                },
            },
        )
    }

    pub(super) fn handle_pane_report_agent(
        &mut self,
        id: String,
        params: PaneReportAgentParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            session_ref: crate::agent_resume::session_ref_from_report(
                &params.source,
                &agent_label,
                params.agent_session_id,
                params.agent_session_path,
            ),
            source: params.source,
            agent_label,
            state: detect_state_from_api(params.state),
            message: params.message,
            custom_status: normalize_custom_status(params.custom_status),
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_agent_session(
        &mut self,
        id: String,
        params: PaneReportAgentSessionParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::AgentSessionReported {
            pane_id,
            session_ref: crate::agent_resume::session_ref_from_report(
                &params.source,
                &agent_label,
                params.agent_session_id,
                params.agent_session_path,
            ),
            source: params.source,
            agent_label,
            seq: params.seq,
            session_start_source: crate::agent_resume::normalize_session_start_source(
                params.session_start_source,
            ),
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_metadata(
        &mut self,
        id: String,
        params: PaneReportMetadataParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let agent_label = match params.agent.as_deref() {
            Some(agent) => match normalize_reported_agent_label(agent) {
                Some(agent_label) => Some(agent_label),
                None => return invalid_agent(id),
            },
            None => None,
        };
        let Some(source) = normalize_optional_text(Some(params.source)) else {
            return encode_error(id, "invalid_metadata_request", "missing metadata source");
        };
        let raw_title_set = params.title.is_some();
        let raw_display_agent_set = params.display_agent.is_some();
        let raw_custom_status_set = params.custom_status.is_some();
        let raw_state_labels_set = !params.state_labels.is_empty();
        let ttl = params.ttl_ms.map(std::time::Duration::from_millis);
        let title = normalize_presentation_text(params.title);
        let display_agent = normalize_presentation_text(params.display_agent);
        let custom_status = normalize_custom_status(params.custom_status);
        let applies_to_source = match params.applies_to_source {
            Some(applies_to_source) => {
                let Some(applies_to_source) = normalize_optional_text(Some(applies_to_source))
                else {
                    return encode_error(
                        id,
                        "invalid_metadata_request",
                        "missing metadata authority source",
                    );
                };
                Some(applies_to_source)
            }
            None => None,
        };
        let state_labels = match normalize_state_labels(params.state_labels) {
            Ok(labels) => labels,
            Err(status) => {
                return encode_error(
                    id,
                    "invalid_state_label",
                    format!("unknown state label: {status}"),
                );
            }
        };
        if raw_title_set && params.clear_title
            || raw_display_agent_set && params.clear_display_agent
            || raw_custom_status_set && params.clear_custom_status
            || raw_state_labels_set && params.clear_state_labels
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "cannot set and clear the same metadata field",
            );
        }
        if title.is_none()
            && display_agent.is_none()
            && custom_status.is_none()
            && state_labels.is_empty()
            && !params.clear_title
            && !params.clear_display_agent
            && !params.clear_custom_status
            && !params.clear_state_labels
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "missing metadata field to set or clear",
            );
        }
        self.handle_internal_event(crate::events::AppEvent::HookMetadataReported {
            pane_id,
            source,
            agent_label,
            applies_to_source,
            title,
            display_agent,
            custom_status,
            state_labels,
            clear_title: params.clear_title,
            clear_display_agent: params.clear_display_agent,
            clear_custom_status: params.clear_custom_status,
            clear_state_labels: params.clear_state_labels,
            seq: params.seq,
            ttl,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_clear_agent_authority(
        &mut self,
        id: String,
        params: PaneClearAgentAuthorityParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAuthorityCleared {
            pane_id,
            source: params.source,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_release_agent(
        &mut self,
        id: String,
        params: PaneReleaseAgentParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAgentReleased {
            pane_id,
            source: params.source,
            known_agent: crate::detect::parse_agent_label(&agent_label),
            agent_label,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_text(
        &mut self,
        id: String,
        params: PaneSendTextParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
            return encode_error(id, "pane_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_input(
        &mut self,
        id: String,
        params: PaneSendInputParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let encoded_keys = match encode_api_keys(runtime, &params.keys) {
            Ok(encoded_keys) => encoded_keys,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        if !params.text.is_empty() {
            let text_bytes = encode_api_text(runtime, &params.text);
            if let Err(err) = runtime.try_send_bytes(Bytes::from(text_bytes)) {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
        }
        for bytes in encoded_keys {
            if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_close(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        if self.state.close_pane_would_close_workspace(ws_idx, pane_id)
            && self.state.confirm_implicit_worktree_group_close(ws_idx)
        {
            return encode_error(
                id,
                "confirmation_required",
                "closing this pane would close a worktree group",
            );
        }
        let terminal_id = self.state.terminal_id_for_pane(ws_idx, pane_id);
        let should_close_workspace = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return pane_not_found(id, &target.pane_id);
            };
            ws.close_pane(pane_id)
        };
        self.state.remove_plugin_pane_records([pane_id]);
        if should_close_workspace {
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: public_pane_id,
                    workspace_id: workspace_id.clone(),
                },
            });
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed { workspace_id },
            });
        } else {
            self.state.remove_unattached_terminal_ids(terminal_id);
            self.shutdown_detached_terminal_runtimes();
            self.schedule_session_save();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: public_pane_id,
                    workspace_id,
                },
            });
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_keys(
        &mut self,
        id: String,
        params: PaneSendKeysParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let encoded_keys = match encode_api_keys(runtime, &params.keys) {
            Ok(encoded_keys) => encoded_keys,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        for bytes in encoded_keys {
            if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn rebase_removed_workspace_index(index: &mut Option<usize>, removed: usize) {
    *index = match *index {
        Some(current) if current == removed => None,
        Some(current) if current > removed => Some(current - 1),
        current => current,
    };
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn normalize_presentation_text(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    let normalized: String = trimmed
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    (!normalized.trim().is_empty()).then(|| normalized.trim().to_string())
}

fn normalize_state_labels(
    labels: std::collections::HashMap<String, String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    labels
        .into_iter()
        .map(|(status, label)| {
            let status = status.trim().to_ascii_lowercase();
            if !matches!(
                status.as_str(),
                "idle" | "working" | "blocked" | "done" | "unknown"
            ) {
                return Err(status);
            }
            Ok(normalize_presentation_text(Some(label)).map(|label| (status, label)))
        })
        .filter_map(Result::transpose)
        .collect()
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

impl App {
    fn resolve_optional_pane(&self, pane_id: Option<&str>) -> Option<(usize, PaneId)> {
        match pane_id {
            Some(pane_id) => self.parse_pane_id(pane_id),
            None => {
                let ws_idx = self.state.active?;
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            }
        }
    }

    fn resolve_swap_source(&self, pane_id: Option<&str>) -> Option<(usize, PaneId)> {
        self.resolve_optional_pane(pane_id)
    }

    fn directional_pane_target(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        source_pane_id: PaneId,
        direction: PaneDirection,
    ) -> Option<PaneId> {
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        let panes = tab.layout.panes(self.state.view.terminal_area);
        let source = panes.iter().find(|pane| pane.id == source_pane_id)?;
        find_in_direction(source, direction.into(), &panes)
    }

    fn pane_layout_snapshot(&self, ws_idx: usize, tab_idx: usize) -> Option<PaneLayoutSnapshot> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let area = self.state.view.terminal_area;
        let focused_pane_id = self.public_pane_id(ws_idx, tab.layout.focused())?;
        let panes = crate::ui::apply_pane_chrome(
            tab.layout.panes(area),
            self.state.pane_borders,
            self.state.pane_gaps,
            self.state.pane_outer_borders,
        )
        .into_iter()
        .filter_map(|pane| {
            Some(PaneLayoutPane {
                pane_id: self.public_pane_id(ws_idx, pane.id)?,
                focused: pane.is_focused,
                rect: pane.rect.into(),
            })
        })
        .collect();
        let splits = tab
            .layout
            .splits(area)
            .into_iter()
            .enumerate()
            .map(|(idx, split)| PaneLayoutSplit {
                id: split_path_id(idx, &split.path),
                direction: match split.direction {
                    ratatui::layout::Direction::Horizontal => {
                        crate::api::schema::SplitDirection::Right
                    }
                    ratatui::layout::Direction::Vertical => {
                        crate::api::schema::SplitDirection::Down
                    }
                },
                ratio: split.ratio,
                rect: split.area.into(),
            })
            .collect();

        Some(PaneLayoutSnapshot {
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            zoomed: tab.zoomed,
            area: area.into(),
            focused_pane_id,
            panes,
            splits,
        })
    }
}

impl From<PaneDirection> for NavDirection {
    fn from(direction: PaneDirection) -> Self {
        match direction {
            PaneDirection::Left => NavDirection::Left,
            PaneDirection::Right => NavDirection::Right,
            PaneDirection::Up => NavDirection::Up,
            PaneDirection::Down => NavDirection::Down,
        }
    }
}

enum ResolvedPaneMoveDestination {
    ExistingTab {
        tab_id: String,
        target_pane_id: PaneId,
        split: crate::api::schema::SplitDirection,
        ratio: f32,
        cross_workspace: bool,
    },
    NewTab {
        workspace_id: String,
        label: Option<String>,
    },
    NewWorkspace {
        label: Option<String>,
        tab_label: Option<String>,
    },
}

struct PaneMoveRecoveryContext {
    source_ws_idx: usize,
    previous_workspace_id: String,
    previous_workspace_label: Option<String>,
    previous_tab_label: Option<String>,
    previous_worktree_space: Option<crate::workspace::WorktreeSpaceMembership>,
    identity_cwd: std::path::PathBuf,
}

fn encode_unchanged_pane_move(
    id: String,
    reason: PaneMoveReason,
    previous_pane_id: String,
    previous_workspace_id: String,
    previous_tab_id: String,
    pane: PaneInfo,
    source_layout: Option<PaneLayoutSnapshot>,
    target_layout: PaneLayoutSnapshot,
) -> String {
    let focused_pane_id = target_layout.focused_pane_id.clone();
    encode_success(
        id,
        ResponseResult::PaneMove {
            move_result: PaneMoveResult {
                changed: false,
                reason: Some(reason),
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane: Box::new(pane),
                source_layout: source_layout.map(Box::new),
                target_layout: Box::new(target_layout),
                created_workspace: None,
                created_tab: None,
                closed_workspace_id: None,
                closed_tab_id: None,
                focused_pane_id,
            },
        },
    )
}

fn split_direction_to_layout(
    direction: crate::api::schema::SplitDirection,
) -> ratatui::layout::Direction {
    match direction {
        crate::api::schema::SplitDirection::Right => ratatui::layout::Direction::Horizontal,
        crate::api::schema::SplitDirection::Down => ratatui::layout::Direction::Vertical,
    }
}

impl From<ratatui::layout::Rect> for PaneLayoutRect {
    fn from(rect: ratatui::layout::Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

fn split_path_id(idx: usize, path: &[bool]) -> String {
    if path.is_empty() {
        return format!("split_{idx}_root");
    }
    let path = path
        .iter()
        .map(|right| if *right { "1" } else { "0" })
        .collect::<Vec<_>>()
        .join("");
    format!("split_{idx}_{path}")
}

fn invalid_agent(id: String) -> String {
    encode_error(id, "invalid_agent", "agent label must not be empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{ErrorResponse, SplitDirection, SuccessResponse},
        config::Config,
        workspace::Workspace,
    };

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

    fn app_with_send_key_runtime(
        capacity: usize,
    ) -> (App, String, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, capacity);
        app.state.workspaces[0].insert_test_runtime(pane_id, runtime);
        (app, public_pane_id, rx)
    }

    #[tokio::test]
    async fn api_pane_send_keys_encodes_shift_tab_as_backtab() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["shift+tab".into()],
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"\x1b[Z"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_accepts_control_navigation_chords() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(4);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec![
                    "ctrl+h".into(),
                    "ctrl+j".into(),
                    "ctrl+k".into(),
                    "ctrl+l".into(),
                ],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x08]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0a]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0b]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0c]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_preserves_legacy_control_c_aliases() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(3);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["C-c".into(), "c-c".into(), "ctrl+c".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_accepts_literal_plus() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["+".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"+"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_keys_accept_key_combo_chords() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: String::new(),
                keys: vec!["ctrl+j".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0a]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_rejects_invalid_keys_before_writing() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(2);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["ctrl+h".into(), "not-a-key".into()],
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert_eq!(error.error.message, "unsupported key not-a-key");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_rejects_prefix_bindings_before_writing_text_or_keys() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(4);
        let raw_key = " prefix+h ".to_string();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: "hello".into(),
                keys: vec!["ctrl+h".into(), raw_key.clone()],
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert_eq!(error.error.message, format!("unsupported key {raw_key}"));
        assert!(rx.try_recv().is_err());
    }

    fn seed_terminal_states(app: &mut App) {
        for ws in &app.state.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    app.state
                        .terminals
                        .entry(pane.attached_terminal_id.clone())
                        .or_insert_with(|| {
                            crate::terminal::TerminalState::new(
                                pane.attached_terminal_id.clone(),
                                std::path::PathBuf::from("/zynk-test"),
                            )
                        });
                }
            }
        }
    }

    fn app_with_cross_workspace_move_source(
        source_has_sibling: bool,
    ) -> (App, PaneId, Option<PaneId>, PaneId) {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let sibling = source_has_sibling
            .then(|| app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal));
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        app.state
            .workspaces
            .push(Workspace::test_new("destination"));
        let target = app.state.workspaces[1].tabs[0].root_pane;
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        seed_terminal_states(&mut app);
        (app, source, sibling, target)
    }

    fn move_between_workspaces(
        app: &mut App,
        source_ws_idx: usize,
        source: PaneId,
        target_ws_idx: usize,
        target: PaneId,
        focus: bool,
    ) -> PaneMoveResult {
        let response = app.handle_pane_move(
            "move".into(),
            PaneMoveParams {
                pane_id: app.public_pane_id(source_ws_idx, source).unwrap(),
                destination: PaneMoveDestination::Tab {
                    tab_id: app.public_tab_id(target_ws_idx, 0).unwrap(),
                    target_pane_id: Some(app.public_pane_id(target_ws_idx, target).unwrap()),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response)
            .unwrap_or_else(|error| panic!("pane move failed: {error}; response: {response}"));
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        move_result
    }

    fn pane_mouse(
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
    ) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }

    fn install_global_reporting_runtime(
        app: &mut App,
        ws_idx: usize,
        pane_id: PaneId,
        mode: &[u8],
        capacity: usize,
    ) -> tokio::sync::mpsc::Receiver<bytes::Bytes> {
        let terminal_id = app.state.workspaces[ws_idx]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let (runtime, receiver) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, mode, capacity,
            );
        app.terminal_runtimes.insert(terminal_id, runtime);
        receiver
    }

    fn route_mouse(
        app: &mut App,
        capture: bool,
        source_id: crate::app::InputSourceId,
        mouse: crossterm::event::MouseEvent,
    ) {
        if capture {
            app.handle_mouse_from_input_source(source_id, mouse);
        } else {
            app.route_client_events_from(
                source_id,
                vec![crate::raw_input::RawInputEvent::Mouse(mouse)],
                false,
            );
        }
    }

    #[test]
    fn cross_workspace_move_rebinds_focus_history_and_notification_targets() {
        let (mut app, source, sibling, target) = app_with_cross_workspace_move_source(true);
        let sibling = sibling.unwrap();
        let source_workspace_id = app.state.workspaces[0].id.clone();
        let destination_workspace_id = app.state.workspaces[1].id.clone();
        app.state.previous_pane_focus = Some(crate::app::state::PaneFocusTarget {
            workspace_id: source_workspace_id.clone(),
            pane_id: source,
        });
        app.last_focus = Some((0, source));
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "done".into(),
            context: "source".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id: source_workspace_id.clone(),
                pane_id: source,
            }),
        });
        let terminal_id = app.state.workspaces[0].terminal_id(source).unwrap().clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority(
                "zynk:codex".into(),
                "codex".into(),
                crate::detect::AgentState::Idle,
                None,
                Some(1),
            );
        let deadline = std::time::Instant::now();
        app.state.pending_agent_notifications.insert(
            source,
            crate::app::state::PendingAgentNotification {
                pane_id: source,
                workspace_id: source_workspace_id,
                agent_label: "codex".into(),
                known_agent: Some(crate::detect::Agent::Codex),
                kind: crate::app::state::ToastKind::Finished,
                state: crate::detect::AgentState::Idle,
                deadline,
            },
        );

        move_between_workspaces(&mut app, 0, source, 1, target, false);

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(sibling));
        assert_eq!(
            app.state
                .previous_pane_focus
                .as_ref()
                .map(|focus| (focus.workspace_id.as_str(), focus.pane_id,)),
            Some((destination_workspace_id.as_str(), source))
        );
        assert_eq!(app.last_focus, Some((1, source)));
        assert_eq!(
            app.state
                .toast
                .as_ref()
                .and_then(|toast| toast.target.as_ref())
                .map(|target| (target.workspace_id.as_str(), target.pane_id)),
            Some((destination_workspace_id.as_str(), source))
        );
        assert_eq!(
            app.state
                .pending_agent_notifications
                .get(&source)
                .map(|pending| pending.workspace_id.as_str()),
            Some(destination_workspace_id.as_str())
        );
        let deliveries = app.state.drain_due_agent_notifications(deadline);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].workspace_id, destination_workspace_id);
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "done".into(),
            context: "destination".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id: deliveries[0].workspace_id.clone(),
                pane_id: source,
            }),
        });
        app.focus_toast_target_via_api();
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(source));
        assert!(app.state.toast.is_none());
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn cross_workspace_move_cancels_pane_and_structure_bound_transient_state() {
        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(true);
        let info = app.state.workspaces[0].tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(20, 2, 80, 20))
            .into_iter()
            .find(|info| info.id == source)
            .unwrap();
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        app.state.selection = Some(crate::selection::Selection::range(source, 0, 0, 2, None));
        app.state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: info.inner_rect.x,
            last_mouse_screen_row: info.inner_rect.y,
            inner_rect: info.inner_rect,
        });
        app.selection_autoscroll_deadline = Some(std::time::Instant::now());
        app.state.context_menu = Some(crate::app::state::ContextMenuState {
            kind: crate::app::state::ContextMenuKind::Pane {
                ws_idx: 0,
                tab_idx: 0,
                pane_id: source,
                source_pane_id: None,
                has_manual_label: false,
            },
            x: 2,
            y: 2,
            list: crate::app::state::MenuListState::new(0),
        });
        app.state.drag = Some(crate::app::state::DragState {
            target: crate::app::state::DragTarget::PaneSplit {
                path: Vec::new(),
                direction: ratatui::layout::Direction::Horizontal,
                area: ratatui::layout::Rect::new(20, 2, 80, 20),
                grab_offset: 0,
            },
        });
        app.state.workspace_presses.insert(
            7,
            crate::app::state::WorkspacePressState {
                ws_idx: 0,
                start_col: 1,
                start_row: 1,
            },
        );
        app.state.tab_presses.insert(
            7,
            crate::app::state::TabPressState {
                ws_idx: 0,
                tab_idx: 0,
                start_col: 2,
                start_row: 2,
            },
        );
        app.state.rename_pane_target = Some(source);
        app.state.name_input = "stale".into();
        app.state.mode = Mode::Copy;
        app.state
            .view
            .workspace_card_areas
            .push(crate::app::state::WorkspaceCardArea {
                ws_idx: 0,
                rect: ratatui::layout::Rect::new(0, 0, 10, 2),
                indented: false,
            });
        app.state
            .view
            .tab_hit_areas
            .push(ratatui::layout::Rect::new(20, 0, 10, 1));
        app.state.view.pane_infos.push(info);
        app.state
            .view
            .split_borders
            .push(crate::layout::SplitBorder {
                pos: 40,
                direction: ratatui::layout::Direction::Horizontal,
                ratio: 0.5,
                area: ratatui::layout::Rect::new(20, 2, 80, 20),
                path: Vec::new(),
            });
        app.state.view.toast_hit_area = ratatui::layout::Rect::new(1, 1, 2, 2);
        app.last_pane_click = Some(crate::app::PaneClickState {
            pane_id: source,
            viewport_row: 0,
            col: 0,
            at: std::time::Instant::now(),
        });

        move_between_workspaces(&mut app, 0, source, 1, target, false);

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_none());
        assert!(app.state.selection.is_none());
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
        assert!(app.state.context_menu.is_none());
        assert!(app.state.drag.is_none());
        assert!(app.state.workspace_presses.is_empty());
        assert!(app.state.tab_presses.is_empty());
        assert!(app.state.rename_pane_target.is_none());
        assert!(app.state.name_input.is_empty());
        assert!(app.state.view.workspace_card_areas.is_empty());
        assert!(app.state.view.tab_hit_areas.is_empty());
        assert!(app.state.view.pane_infos.is_empty());
        assert!(app.state.view.split_borders.is_empty());
        assert_eq!(
            app.state.view.toast_hit_area,
            ratatui::layout::Rect::default()
        );
        assert!(app.last_pane_click.is_none());
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn cross_workspace_move_rebases_removed_workspace_indices_and_preserves_create_intent() {
        let mut app = app_with_linked_worktree();
        app.state
            .workspaces
            .insert(0, Workspace::test_new("before"));
        app.state
            .workspaces
            .push(Workspace::test_new("destination"));
        app.state.active = Some(2);
        app.state.selected = 2;
        seed_terminal_states(&mut app);
        let source = app.state.workspaces[1].tabs[0].root_pane;
        let source_workspace_id = app.state.workspaces[1].id.clone();
        let target = app.state.workspaces[2].tabs[0].root_pane;
        let destination_workspace_id = app.state.workspaces[2].id.clone();
        app.state.request_new_linked_worktree = Some(1);
        app.state.request_open_existing_worktree = Some(2);
        app.state.request_remove_linked_worktree = Some(0);
        app.state.worktree_open = Some(crate::app::state::WorktreeOpenState {
            source_workspace_id: destination_workspace_id.clone(),
            source_existing_membership: None,
            source_checkout_path: "/repo/destination".into(),
            source_repo_root: "/repo".into(),
            repo_key: "repo".into(),
            repo_name: "repo".into(),
            entries: vec![
                crate::app::state::WorktreeOpenEntry {
                    path: "/repo/source".into(),
                    branch: Some("source".into()),
                    is_linked_worktree: true,
                    already_open_ws_idx: Some(1),
                },
                crate::app::state::WorktreeOpenEntry {
                    path: "/repo/destination".into(),
                    branch: Some("destination".into()),
                    is_linked_worktree: true,
                    already_open_ws_idx: Some(2),
                },
            ],
            selected: 1,
            query: String::new(),
            search_focused: false,
            error: None,
        });
        app.state.pending_workspace_create_cwd =
            Some(crate::app::state::PendingWorkspaceCreateCwd::Follow {
                source_workspace_id: Some(source_workspace_id.clone()),
                suggested_cwd: "/repo/source".into(),
            });
        app.state.name_input = "new workspace".into();
        app.state.mode = Mode::RenameWorkspace;
        app.last_focus = Some((2, target));
        app.state.navigator.query = "destination".into();
        app.state.navigator.selected = 9;
        app.state.navigator.scroll = 7;
        app.state
            .navigator
            .expanded_workspaces
            .insert(destination_workspace_id.clone());

        move_between_workspaces(&mut app, 1, source, 2, target, false);

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.request_new_linked_worktree, None);
        assert_eq!(app.state.request_open_existing_worktree, Some(1));
        assert_eq!(app.state.request_remove_linked_worktree, Some(0));
        assert_eq!(app.last_focus, Some((1, target)));
        let entries = &app.state.worktree_open.as_ref().unwrap().entries;
        assert_eq!(entries[0].already_open_ws_idx, None);
        assert_eq!(entries[1].already_open_ws_idx, Some(1));
        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert_eq!(app.state.name_input, "new workspace");
        assert!(matches!(
            app.state.pending_workspace_create_cwd,
            Some(crate::app::state::PendingWorkspaceCreateCwd::Follow {
                source_workspace_id: Some(ref id),
                ..
            }) if id == &source_workspace_id
        ));
        assert_eq!(app.state.navigator.selected, 0);
        assert_eq!(app.state.navigator.scroll, 0);
        assert_eq!(app.state.navigator.query, "destination");
        assert!(app
            .state
            .navigator
            .expanded_workspaces
            .contains(&destination_workspace_id));
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn focused_cross_workspace_move_preserves_workspace_create_prompt() {
        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(true);
        app.state.pending_workspace_create_cwd = Some(
            crate::app::state::PendingWorkspaceCreateCwd::Resolved("/repo/new".into()),
        );
        app.state.name_input = "new workspace".into();
        app.state.name_input_replace_on_type = true;
        app.state.mode = Mode::RenameWorkspace;

        move_between_workspaces(&mut app, 0, source, 1, target, true);

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert_eq!(app.state.name_input, "new workspace");
        assert!(app.state.name_input_replace_on_type);
        assert!(matches!(
            app.state.pending_workspace_create_cwd,
            Some(crate::app::state::PendingWorkspaceCreateCwd::Resolved(ref cwd))
                if cwd == std::path::Path::new("/repo/new")
        ));
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn cross_workspace_move_preserves_accepted_rename_pane_intent() {
        for focus in [false, true] {
            let (mut app, source, sibling, target) = app_with_cross_workspace_move_source(true);
            let sibling = sibling.unwrap();
            let source_terminal_id = app.state.workspaces[0].terminal_id(source).unwrap().clone();
            let sibling_terminal_id = app.state.workspaces[0]
                .terminal_id(sibling)
                .unwrap()
                .clone();
            let target_terminal_id = app.state.workspaces[1].terminal_id(target).unwrap().clone();
            app.state.mode = Mode::RenamePane;
            app.state.rename_pane_target = Some(source);
            app.state.name_input = "relocated pane".into();
            app.state.name_input_replace_on_type = false;

            move_between_workspaces(&mut app, 0, source, 1, target, focus);

            assert_eq!(app.state.mode, Mode::RenamePane, "focus={focus}");
            assert_eq!(app.state.rename_pane_target, Some(source), "focus={focus}");
            assert_eq!(app.state.name_input, "relocated pane", "focus={focus}");
            assert!(!app.state.name_input_replace_on_type, "focus={focus}");

            app.handle_rename_key_via_api(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ));

            assert_eq!(
                app.state.terminals[&source_terminal_id]
                    .manual_label
                    .as_deref(),
                Some("relocated pane"),
                "focus={focus}: moved pane"
            );
            assert_eq!(
                app.state.terminals[&sibling_terminal_id].manual_label, None,
                "focus={focus}: source sibling"
            );
            assert_eq!(
                app.state.terminals[&target_terminal_id].manual_label, None,
                "focus={focus}: destination pane"
            );
            assert_eq!(app.state.mode, Mode::Terminal, "focus={focus}");
            assert!(app.state.rename_pane_target.is_none(), "focus={focus}");
            assert!(app.state.name_input.is_empty(), "focus={focus}");
            assert!(!app.state.name_input_replace_on_type, "focus={focus}");
            app.state.assert_invariants_for_test();
        }
    }

    #[test]
    fn cross_workspace_move_cancels_index_bound_modal_targets() {
        for mode in [
            Mode::RenameWorkspace,
            Mode::RenameTab,
            Mode::Resize,
            Mode::ConfirmClose,
            Mode::ContextMenu,
        ] {
            let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(false);
            app.state.mode = mode;
            app.state.name_input = "stale target".into();
            match mode {
                Mode::RenameTab => {
                    app.state.creating_new_tab = true;
                    app.state.requested_new_tab_name = Some("stale tab".into());
                }
                Mode::ContextMenu => {
                    app.state.context_menu = Some(crate::app::state::ContextMenuState {
                        kind: crate::app::state::ContextMenuKind::Workspace { ws_idx: 0 },
                        x: 0,
                        y: 0,
                        list: crate::app::state::MenuListState::new(0),
                    });
                }
                _ => {}
            }

            move_between_workspaces(&mut app, 0, source, 1, target, false);

            assert_eq!(app.state.mode, Mode::Terminal, "{mode:?}");
            assert!(app.state.name_input.is_empty(), "{mode:?}");
            assert!(!app.state.creating_new_tab, "{mode:?}");
            assert!(app.state.requested_new_tab_name.is_none(), "{mode:?}");
            assert!(app.state.rename_pane_target.is_none(), "{mode:?}");
            assert!(app.state.context_menu.is_none(), "{mode:?}");
            app.state.assert_invariants_for_test();
        }
    }

    #[test]
    fn cross_workspace_move_cancels_worktree_modal_with_removed_stable_workspace() {
        for mode in [
            Mode::NewLinkedWorktree,
            Mode::OpenExistingWorktree,
            Mode::ConfirmRemoveWorktree,
        ] {
            let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(false);
            let source_workspace_id = app.state.workspaces[0].id.clone();
            app.state.mode = mode;
            match mode {
                Mode::NewLinkedWorktree => {
                    app.state.worktree_create = Some(crate::app::state::WorktreeCreateState {
                        source_workspace_id,
                        source_checkout_path: "/repo/source".into(),
                        source_existing_membership: None,
                        source_repo_root: "/repo".into(),
                        repo_key: "repo".into(),
                        repo_name: "repo".into(),
                        branch: "new".into(),
                        checkout_path: "/repo/new".into(),
                        error: None,
                        creating: false,
                    });
                    app.state.request_submit_worktree_create = true;
                }
                Mode::OpenExistingWorktree => {
                    app.state.worktree_open = Some(crate::app::state::WorktreeOpenState {
                        source_workspace_id,
                        source_existing_membership: None,
                        source_checkout_path: "/repo/source".into(),
                        source_repo_root: "/repo".into(),
                        repo_key: "repo".into(),
                        repo_name: "repo".into(),
                        entries: Vec::new(),
                        selected: 0,
                        query: String::new(),
                        search_focused: false,
                        error: None,
                    });
                    app.state.request_submit_worktree_open = true;
                }
                Mode::ConfirmRemoveWorktree => {
                    app.state.worktree_remove = Some(crate::app::state::WorktreeRemoveState {
                        workspace_id: source_workspace_id,
                        repo_root: "/repo".into(),
                        path: "/repo/source".into(),
                        error: None,
                        removing: false,
                        force_confirmation: false,
                    });
                    app.state.request_submit_worktree_remove = true;
                }
                _ => unreachable!(),
            }

            move_between_workspaces(&mut app, 0, source, 1, target, false);

            assert_eq!(app.state.mode, Mode::Terminal, "{mode:?}");
            assert!(app.state.worktree_create.is_none(), "{mode:?}");
            assert!(app.state.worktree_open.is_none(), "{mode:?}");
            assert!(app.state.worktree_remove.is_none(), "{mode:?}");
            assert!(!app.state.request_submit_worktree_create, "{mode:?}");
            assert!(!app.state.request_submit_worktree_open, "{mode:?}");
            assert!(!app.state.request_submit_worktree_remove, "{mode:?}");
            app.state.assert_invariants_for_test();
        }
    }

    #[tokio::test]
    async fn cross_workspace_move_preserves_owned_mouse_gesture_for_capture_and_no_capture() {
        use crossterm::event::{MouseButton, MouseEventKind};

        for capture in [true, false] {
            let (mut app, source, sibling, target) = app_with_cross_workspace_move_source(true);
            let sibling = sibling.unwrap();
            app.state.mouse_capture = capture;
            let mut source_rx =
                install_global_reporting_runtime(&mut app, 0, source, b"\x1b[?1002h\x1b[?1006h", 8);
            let mut sibling_rx = install_global_reporting_runtime(
                &mut app,
                0,
                sibling,
                b"\x1b[?1002h\x1b[?1006h",
                8,
            );
            crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
            let source_info = app.state.pane_info_by_id(source).unwrap().clone();
            let column = source_info.inner_rect.x + 2;
            let row = source_info.inner_rect.y + 3;

            route_mouse(
                &mut app,
                capture,
                7,
                pane_mouse(MouseEventKind::Down(MouseButton::Left), column, row),
            );
            assert_eq!(
                source_rx.try_recv().unwrap(),
                bytes::Bytes::from_static(b"\x1b[<0;3;4M"),
                "capture={capture} down"
            );

            move_between_workspaces(&mut app, 0, source, 1, target, false);
            crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
            let source_runtime = app
                .state
                .runtime_for_pane(&app.terminal_runtimes, source)
                .unwrap();
            source_runtime.resize(2, 4, 0, 0);

            for kind in [
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                route_mouse(
                    &mut app,
                    capture,
                    9,
                    pane_mouse(kind, column + 90, row + 90),
                );
            }
            assert!(
                sibling_rx.try_recv().is_err(),
                "capture={capture}: foreign source received an unsolicited gesture"
            );

            route_mouse(
                &mut app,
                capture,
                7,
                pane_mouse(
                    MouseEventKind::Drag(MouseButton::Left),
                    column + 90,
                    row + 90,
                ),
            );
            route_mouse(
                &mut app,
                capture,
                7,
                pane_mouse(MouseEventKind::Up(MouseButton::Left), column + 90, row + 90),
            );
            assert_eq!(
                source_rx.try_recv().unwrap(),
                bytes::Bytes::from_static(b"\x1b[<32;4;2M"),
                "capture={capture} clamped drag"
            );
            assert_eq!(
                source_rx.try_recv().unwrap(),
                bytes::Bytes::from_static(b"\x1b[<0;4;2m"),
                "capture={capture} clamped release"
            );
            assert!(source_rx.try_recv().is_err(), "capture={capture}");
            assert!(sibling_rx.try_recv().is_err(), "capture={capture}");
        }
    }

    #[tokio::test]
    async fn rejected_terminal_mouse_down_does_not_survive_cross_workspace_relocation() {
        use crossterm::event::{MouseButton, MouseEventKind};

        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(true);
        let mut source_rx =
            install_global_reporting_runtime(&mut app, 0, source, b"\x1b[?1002h\x1b[?1006h", 1);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
        let info = app.state.pane_info_by_id(source).unwrap().clone();
        let column = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 3;
        app.state
            .runtime_for_pane(&app.terminal_runtimes, source)
            .unwrap()
            .try_send_bytes(bytes::Bytes::from_static(b"filler"))
            .expect("fill bounded input channel");
        app.state.selection = Some(crate::selection::Selection::range(source, 0, 0, 1, None));

        app.handle_mouse_from_input_source(
            7,
            pane_mouse(MouseEventKind::Down(MouseButton::Left), column, row),
        );

        assert!(
            app.state.selection.is_none(),
            "a rejected reported Down must remain consumed by terminal routing"
        );
        assert!(
            !app.state
                .terminal_mouse_gestures
                .contains_key(&(7, MouseButton::Left)),
            "a rejected Down must not establish durable gesture ownership"
        );

        move_between_workspaces(&mut app, 0, source, 1, target, false);
        assert_eq!(
            source_rx.try_recv().expect("queued filler"),
            bytes::Bytes::from_static(b"filler")
        );
        for kind in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.handle_mouse_from_input_source(7, pane_mouse(kind, column, row));
        }

        assert!(source_rx.try_recv().is_err());
        assert!(!app
            .state
            .terminal_mouse_gestures
            .contains_key(&(7, MouseButton::Left)));
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn closed_terminal_mouse_down_does_not_create_owned_gesture() {
        use crossterm::event::{MouseButton, MouseEventKind};

        let (mut app, source, _sibling, _target) = app_with_cross_workspace_move_source(false);
        let source_rx =
            install_global_reporting_runtime(&mut app, 0, source, b"\x1b[?1002h\x1b[?1006h", 1);
        drop(source_rx);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
        let info = app.state.pane_info_by_id(source).unwrap().clone();
        let column = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 3;
        app.state.selection = Some(crate::selection::Selection::range(source, 0, 0, 1, None));

        app.handle_mouse_from_input_source(
            7,
            pane_mouse(MouseEventKind::Down(MouseButton::Left), column, row),
        );

        assert!(app.state.selection.is_none());
        assert!(!app
            .state
            .terminal_mouse_gestures
            .contains_key(&(7, MouseButton::Left)));
        for kind in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.handle_mouse_from_input_source(7, pane_mouse(kind, column, row));
        }
        assert!(!app
            .state
            .terminal_mouse_gestures
            .contains_key(&(7, MouseButton::Left)));
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn rejected_right_passthrough_is_consumed_without_durable_ownership() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let (mut app, source, _sibling, _target) = app_with_cross_workspace_move_source(false);
        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);
        let mut source_rx =
            install_global_reporting_runtime(&mut app, 0, source, b"\x1b[?1002h\x1b[?1006h", 1);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
        let info = app.state.pane_info_by_id(source).unwrap().clone();
        app.state
            .runtime_for_pane(&app.terminal_runtimes, source)
            .unwrap()
            .try_send_bytes(bytes::Bytes::from_static(b"filler"))
            .expect("fill bounded input channel");

        app.handle_mouse_from_input_source(
            7,
            MouseEvent {
                modifiers: KeyModifiers::CONTROL,
                ..pane_mouse(
                    MouseEventKind::Down(MouseButton::Right),
                    info.inner_rect.x + 2,
                    info.inner_rect.y + 3,
                )
            },
        );

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.context_menu.is_none());
        assert!(app.state.right_click_passthrough.is_none());
        assert!(!app
            .state
            .terminal_mouse_gestures
            .contains_key(&(7, MouseButton::Right)));
        assert_eq!(
            source_rx.try_recv().expect("queued filler"),
            bytes::Bytes::from_static(b"filler")
        );
        app.handle_mouse_from_input_source(
            7,
            pane_mouse(
                MouseEventKind::Up(MouseButton::Right),
                info.inner_rect.x + 2,
                info.inner_rect.y + 3,
            ),
        );
        assert!(source_rx.try_recv().is_err());
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn cross_workspace_move_keeps_right_passthrough_owned_by_its_source() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(true);
        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);
        let mut source_rx =
            install_global_reporting_runtime(&mut app, 0, source, b"\x1b[?1002h\x1b[?1006h", 8);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
        let source_info = app.state.pane_info_by_id(source).unwrap().clone();
        let column = source_info.inner_rect.x + 2;
        let row = source_info.inner_rect.y + 3;
        let with_control = |kind| MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..pane_mouse(kind, column, row)
        };

        app.handle_mouse_from_input_source(
            7,
            with_control(MouseEventKind::Down(MouseButton::Right)),
        );
        assert_eq!(
            source_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[<2;3;4M")
        );
        move_between_workspaces(&mut app, 0, source, 1, target, false);

        app.handle_mouse_from_input_source(9, with_control(MouseEventKind::Up(MouseButton::Right)));
        assert!(app.state.right_click_passthrough.is_some());
        assert!(source_rx.try_recv().is_err());

        app.handle_mouse_from_input_source(7, with_control(MouseEventKind::Up(MouseButton::Right)));
        assert!(app.state.right_click_passthrough.is_none());
        assert_eq!(
            source_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[<2;3;4m")
        );
        assert!(source_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn terminal_mouse_gesture_clears_on_source_teardown_and_pane_removal() {
        use crossterm::event::{MouseButton, MouseEventKind};

        let (mut app, source, sibling, _target) = app_with_cross_workspace_move_source(true);
        let sibling = sibling.unwrap();
        let mut source_rx =
            install_global_reporting_runtime(&mut app, 0, source, b"\x1b[?1002h\x1b[?1006h", 8);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
        let info = app.state.pane_info_by_id(source).unwrap().clone();
        let down = pane_mouse(
            MouseEventKind::Down(MouseButton::Left),
            info.inner_rect.x + 1,
            info.inner_rect.y + 1,
        );
        app.handle_mouse_from_input_source(7, down);
        assert!(app
            .state
            .terminal_mouse_gestures
            .contains_key(&(7, MouseButton::Left)));
        app.clear_input_source(7);
        assert!(!app
            .state
            .terminal_mouse_gestures
            .contains_key(&(7, MouseButton::Left)));
        app.handle_mouse_from_input_source(
            7,
            pane_mouse(
                MouseEventKind::Up(MouseButton::Left),
                info.inner_rect.x + 1,
                info.inner_rect.y + 1,
            ),
        );
        assert_eq!(
            source_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[<0;2;2M")
        );
        assert!(source_rx.try_recv().is_err());

        app.handle_mouse_from_input_source(8, down);
        assert!(app
            .state
            .terminal_mouse_gestures
            .contains_key(&(8, MouseButton::Left)));
        app.state.confirm_close = false;
        let response = app.handle_pane_close(
            "close".into(),
            PaneTarget {
                pane_id: app.public_pane_id(0, source).unwrap(),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(sibling));
        assert!(!app
            .state
            .terminal_mouse_gestures
            .contains_key(&(8, MouseButton::Left)));
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn moved_selection_cannot_consume_copy_shortcuts_or_copy_on_select_release() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::SUPER] {
            let (mut app, source, sibling, target) = app_with_cross_workspace_move_source(true);
            let sibling = sibling.unwrap();
            let mut sibling_rx = install_global_reporting_runtime(&mut app, 0, sibling, b"", 4);
            let mut selection = crate::selection::Selection::range(source, 0, 0, 2, None);
            selection.finish();
            app.state.selection = Some(selection);
            app.state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
                direction: crate::app::state::SelectionAutoscrollDirection::Down,
                last_mouse_screen_col: 1,
                last_mouse_screen_row: 1,
                inner_rect: ratatui::layout::Rect::new(0, 0, 10, 4),
            });
            app.selection_autoscroll_deadline = Some(std::time::Instant::now());
            app.state.copy_on_select = false;

            move_between_workspaces(&mut app, 0, source, 1, target, false);
            let key = crate::input::TerminalKey::from(KeyEvent::new(KeyCode::Char('c'), modifiers));
            app.handle_key(key).await;

            assert!(app.state.selection.is_none(), "{modifiers:?}");
            assert!(app.state.selection_autoscroll.is_none(), "{modifiers:?}");
            assert!(app.selection_autoscroll_deadline.is_none(), "{modifiers:?}");
            assert!(app.state.request_clipboard_write.is_none(), "{modifiers:?}");
            assert!(
                sibling_rx.try_recv().is_ok(),
                "{modifiers:?} was consumed by the moved retained selection"
            );
        }

        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(true);
        let mut selection = crate::selection::Selection::range(source, 0, 0, 2, None);
        selection.finish();
        app.state.selection = Some(selection);
        app.state.copy_on_select = true;
        move_between_workspaces(&mut app, 0, source, 1, target, false);
        app.handle_mouse(pane_mouse(MouseEventKind::Up(MouseButton::Left), 1, 1));
        assert!(app.state.selection.is_none());
        assert!(app.state.request_clipboard_write.is_none());
    }

    #[test]
    fn cross_workspace_move_cancels_each_shifted_chrome_gesture_class() {
        for case in 0..4 {
            let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(false);
            match case {
                0 => {
                    app.state.workspace_presses.insert(
                        7,
                        crate::app::state::WorkspacePressState {
                            ws_idx: 0,
                            start_col: 1,
                            start_row: 1,
                        },
                    );
                }
                1 => {
                    app.state.tab_presses.insert(
                        7,
                        crate::app::state::TabPressState {
                            ws_idx: 0,
                            tab_idx: 0,
                            start_col: 1,
                            start_row: 1,
                        },
                    );
                }
                2 => {
                    app.state.drag = Some(crate::app::state::DragState {
                        target: crate::app::state::DragTarget::WorkspaceReorder {
                            source_id: 7,
                            source_ws_idx: 0,
                            insert_idx: Some(1),
                        },
                    });
                }
                3 => {
                    app.state.drag = Some(crate::app::state::DragState {
                        target: crate::app::state::DragTarget::TabReorder {
                            source_id: 7,
                            ws_idx: 0,
                            source_tab_idx: 0,
                            insert_idx: Some(0),
                        },
                    });
                }
                _ => unreachable!(),
            }

            move_between_workspaces(&mut app, 0, source, 1, target, false);

            assert!(app.state.workspace_presses.is_empty(), "case={case}");
            assert!(app.state.tab_presses.is_empty(), "case={case}");
            assert!(app.state.drag.is_none(), "case={case}");
            app.state.assert_invariants_for_test();
        }
    }

    #[test]
    fn cross_workspace_move_cancels_each_shifted_context_menu_class() {
        use crate::app::state::{ContextMenuKind, ContextMenuState, MenuListState};

        for case in 0..3 {
            let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(false);
            let kind = match case {
                0 => ContextMenuKind::Workspace { ws_idx: 0 },
                1 => ContextMenuKind::GitWorkspace {
                    ws_idx: 0,
                    is_linked_worktree: false,
                    has_worktree_children: false,
                    collapsed: false,
                },
                2 => ContextMenuKind::Tab {
                    ws_idx: 0,
                    tab_idx: 0,
                },
                _ => unreachable!(),
            };
            app.state.context_menu = Some(ContextMenuState {
                kind,
                x: 1,
                y: 1,
                list: MenuListState::new(0),
            });
            app.state.mode = Mode::ContextMenu;

            move_between_workspaces(&mut app, 0, source, 1, target, false);

            assert_eq!(app.state.mode, Mode::Terminal, "case={case}");
            assert!(app.state.context_menu.is_none(), "case={case}");
            app.state.assert_invariants_for_test();
        }
    }

    #[test]
    fn cross_workspace_move_preserves_live_stable_worktree_modal() {
        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(true);
        let destination_id = app.state.workspaces[1].id.clone();
        app.state.worktree_open = Some(crate::app::state::WorktreeOpenState {
            source_workspace_id: destination_id.clone(),
            source_existing_membership: None,
            source_checkout_path: "/repo/destination".into(),
            source_repo_root: "/repo".into(),
            repo_key: "repo".into(),
            repo_name: "repo".into(),
            entries: Vec::new(),
            selected: 0,
            query: "keep".into(),
            search_focused: true,
            error: None,
        });
        app.state.request_submit_worktree_open = true;
        app.state.mode = Mode::OpenExistingWorktree;

        move_between_workspaces(&mut app, 0, source, 1, target, false);

        let open = app.state.worktree_open.as_ref().unwrap();
        assert_eq!(open.source_workspace_id, destination_id);
        assert_eq!(open.query, "keep");
        assert!(open.search_focused);
        assert!(app.state.request_submit_worktree_open);
        assert_eq!(app.state.mode, Mode::OpenExistingWorktree);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn focused_cross_workspace_move_preserves_live_stable_worktree_modal() {
        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(true);
        let destination_id = app.state.workspaces[1].id.clone();
        app.state.worktree_open = Some(crate::app::state::WorktreeOpenState {
            source_workspace_id: destination_id,
            source_existing_membership: None,
            source_checkout_path: "/repo/destination".into(),
            source_repo_root: "/repo".into(),
            repo_key: "repo".into(),
            repo_name: "repo".into(),
            entries: Vec::new(),
            selected: 0,
            query: "keep".into(),
            search_focused: true,
            error: None,
        });
        app.state.request_submit_worktree_open = true;
        app.state.mode = Mode::OpenExistingWorktree;

        move_between_workspaces(&mut app, 0, source, 1, target, true);

        assert_eq!(app.state.mode, Mode::OpenExistingWorktree);
        assert_eq!(app.state.worktree_open.as_ref().unwrap().query, "keep");
        assert!(app.state.request_submit_worktree_open);
        app.state.assert_invariants_for_test();
    }

    fn install_focus_runtime(
        app: &mut App,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> tokio::sync::mpsc::Receiver<bytes::Bytes> {
        install_global_reporting_runtime(app, ws_idx, pane_id, b"\x1b[?1004h", 4)
    }

    #[tokio::test]
    async fn cross_workspace_move_focus_events_follow_stable_pane_identity() {
        for (focus, source_was_current) in [(false, true), (true, true), (true, false)] {
            let (mut app, source, sibling, target) = app_with_cross_workspace_move_source(true);
            let sibling = sibling.unwrap();
            if source_was_current {
                app.state.workspaces[0].tabs[0].layout.focus_pane(source);
            } else {
                app.state.workspaces[0].tabs[0].layout.focus_pane(sibling);
            }
            let mut source_rx = install_focus_runtime(&mut app, 0, source);
            let mut sibling_rx = install_focus_runtime(&mut app, 0, sibling);
            app.last_focus = Some((0, if source_was_current { source } else { sibling }));

            move_between_workspaces(&mut app, 0, source, 1, target, focus);
            app.sync_focus_events();

            match (focus, source_was_current) {
                (false, true) => {
                    assert_eq!(
                        source_rx.try_recv().unwrap(),
                        bytes::Bytes::from_static(b"\x1b[O")
                    );
                    assert_eq!(
                        sibling_rx.try_recv().unwrap(),
                        bytes::Bytes::from_static(b"\x1b[I")
                    );
                }
                (true, true) => {
                    assert!(source_rx.try_recv().is_err());
                    assert!(sibling_rx.try_recv().is_err());
                    assert!(
                        app.state.previous_pane_focus.is_none(),
                        "moving the current pane must not create fake focus history"
                    );
                }
                (true, false) => {
                    assert_eq!(
                        sibling_rx.try_recv().unwrap(),
                        bytes::Bytes::from_static(b"\x1b[O")
                    );
                    assert_eq!(
                        source_rx.try_recv().unwrap(),
                        bytes::Bytes::from_static(b"\x1b[I")
                    );
                }
                _ => unreachable!(),
            }
            assert!(source_rx.try_recv().is_err());
            assert!(sibling_rx.try_recv().is_err());
        }

        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(false);
        let mut source_rx = install_focus_runtime(&mut app, 0, source);
        let mut target_rx = install_focus_runtime(&mut app, 1, target);
        app.last_focus = Some((0, source));

        move_between_workspaces(&mut app, 0, source, 1, target, false);
        assert_eq!(app.last_focus, Some((0, source)));
        app.sync_focus_events();

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.last_focus, Some((0, target)));
        assert_eq!(
            source_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[O")
        );
        assert_eq!(
            target_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[I")
        );
        assert!(source_rx.try_recv().is_err());
        assert!(target_rx.try_recv().is_err());
    }

    #[test]
    fn api_pane_close_closes_linked_worktree_workspace_only() {
        let mut app = app_with_linked_worktree();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_pane_close(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(app.state.request_remove_linked_worktree, None);
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn api_pane_close_clears_copy_mode_for_removed_pane() {
        let mut app = app_with_linked_worktree();
        let closing = app.state.workspaces[0].tabs[0].root_pane;
        let survivor = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        seed_terminal_states(&mut app);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].tabs[0].layout.focus_pane(closing);
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: closing,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let closing_public = app.public_pane_id(0, closing).unwrap();

        let response = app.handle_pane_close(
            "req".into(),
            PaneTarget {
                pane_id: closing_public,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(app.state.copy_mode.is_none());
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(survivor));
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn api_pane_swap_explicit_source_and_target_preserves_focus_and_returns_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(swap.changed);
        assert_eq!(swap.reason, None);
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.focused_pane_id, swap.source_pane_id);
        assert_eq!(swap.layout.focused_pane_id, swap.source_pane_id);
        assert_eq!(swap.layout.panes.len(), 2);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
    }

    #[test]
    fn api_pane_swap_unfocused_source_updates_last_pane_history() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let focused = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let target = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].tabs[0].layout.focus_pane(focused);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public),
                target_pane_id: Some(target_public),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(swap.changed);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));

        app.state.last_pane();

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(focused));
    }

    #[test]
    fn api_pane_swap_direction_no_neighbor_returns_unchanged_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                pane_id: Some(source_public.clone()),
                direction: Some(PaneDirection::Left),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NoNeighbor));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, None);
        assert_eq!(swap.layout.panes.len(), 1);
    }

    #[test]
    fn api_pane_swap_explicit_missing_target_returns_not_found_noop() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some("missing-pane".into()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NotFound));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some("missing-pane".into()));
        assert_eq!(swap.layout.panes.len(), 1);
    }

    #[test]
    fn api_pane_swap_explicit_missing_source_returns_not_found_noop() {
        let mut app = app_with_linked_worktree();
        let target = app.state.workspaces[0].tabs[0].root_pane;
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some("missing-pane".into()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NotFound));
        assert_eq!(swap.source_pane_id, "missing-pane");
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.layout.panes.len(), 1);
    }

    #[test]
    fn api_pane_swap_explicit_cross_workspace_preserves_target_id() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::CrossTab));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.layout.workspace_id, app.public_workspace_id(0));
    }

    #[test]
    fn api_pane_move_to_existing_tab_preserves_internal_pane_and_terminal() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_tab_public = app.public_tab_id(0, 0).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public.clone(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: Some(0.25),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.reason, None);
        assert_eq!(move_result.previous_pane_id, source_public);
        assert_eq!(move_result.previous_tab_id, source_tab_public);
        assert_eq!(move_result.pane.pane_id, move_result.previous_pane_id);
        assert_eq!(move_result.pane.tab_id, target_tab_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(move_result.closed_tab_id, Some(source_tab_public));
        assert_eq!(move_result.closed_workspace_id, None);
        assert_eq!(move_result.target_layout.panes.len(), 2);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), source);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
    }

    #[test]
    fn api_pane_move_focuses_copy_mode_pane_back_into_copy_mode() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        seed_terminal_states(&mut app);
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(app.state.copy_mode.expect("copy mode").pane_id, source);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), source);
    }

    #[tokio::test]
    async fn key_release_follows_pane_moved_across_workspaces() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[>3u",
                2,
            );
        app.terminal_runtimes.insert(source_terminal_id, runtime);
        app.state.workspaces.push(Workspace::test_new("other"));
        seed_terminal_states(&mut app);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        let source_public = app.public_pane_id(0, source).unwrap();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let target_pane_id = app.public_pane_id(1, target).unwrap();

        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('j'),
                    crossterm::event::KeyModifiers::CONTROL,
                ),
            )],
            false,
        );
        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id,
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response)
            .unwrap_or_else(|error| panic!("pane move failed: {error}; response: {response}"));
        assert!(matches!(success.result, ResponseResult::PaneMove { .. }));
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('j'),
                    crossterm::event::KeyModifiers::CONTROL,
                )
                .with_kind(crossterm::event::KeyEventKind::Release),
            )],
            false,
        );

        assert_eq!(
            rx.try_recv().expect("forwarded press"),
            bytes::Bytes::from_static(b"\x1b[106;5:1u")
        );
        assert_eq!(
            rx.try_recv().expect("forwarded release after pane move"),
            bytes::Bytes::from_static(b"\x1b[106;5:3u")
        );
        assert!(app.input_leases.is_empty());
    }

    #[test]
    fn api_pane_move_to_existing_tab_across_workspace_reassigns_public_pane_id() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let previous_pane_id = app.public_pane_id(0, source).unwrap();
        let previous_workspace_id = app.public_workspace_id(0);
        let target_workspace_id = app.public_workspace_id(1);
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let target_pane_id = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: previous_pane_id.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id.clone(),
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.previous_pane_id, previous_pane_id);
        assert_eq!(move_result.previous_workspace_id, previous_workspace_id);
        assert_eq!(move_result.closed_workspace_id, Some(previous_workspace_id));
        assert_ne!(move_result.pane.pane_id, move_result.previous_pane_id);
        assert!(move_result
            .pane
            .pane_id
            .starts_with(&format!("{target_workspace_id}:p")));
        assert_eq!(move_result.pane.workspace_id, target_workspace_id);
        assert_eq!(move_result.pane.tab_id, target_tab_id);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert_eq!(app.parse_pane_id(&previous_pane_id), Some((0, source)));
    }

    #[test]
    fn api_pane_move_legacy_target_tab_id_survives_source_workspace_removal() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: "t_2_1".into(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(move_result.closed_workspace_id, Some("w1".into()));
        assert_eq!(move_result.pane.workspace_id, "w2");
        assert_eq!(move_result.pane.tab_id, "w2:t1");
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
    }

    #[test]
    fn api_pane_move_to_new_tab_creates_tab_without_spawning_terminal() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(
            move_result
                .created_tab
                .as_ref()
                .map(|tab| tab.label.as_str()),
            Some("moved")
        );
        assert_eq!(
            move_result.created_tab.as_ref().map(|tab| tab.focused),
            Some(true)
        );
        assert_eq!(move_result.closed_tab_id, None);
        assert_eq!(move_result.pane.pane_id, source_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.workspaces[0].tabs[0].terminal_id(right).is_some());
        assert_eq!(
            app.state.workspaces[0].tabs[1].terminal_id(source),
            Some(&source_terminal)
        );
        let envelopes = app.event_hub.events_after(0);
        let events: Vec<_> = envelopes
            .iter()
            .map(|(_, envelope)| envelope.event)
            .collect();
        assert_eq!(events, vec![EventKind::TabCreated, EventKind::PaneMoved]);
        match &envelopes[0].1.data {
            EventData::TabCreated { tab } => assert!(tab.focused),
            other => panic!("expected tab created event, got {other:?}"),
        }
    }

    #[test]
    fn api_pane_move_only_pane_to_new_tab_uses_app_render_handles() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_notify,
            &app.render_notify
        ));
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_dirty,
            &app.render_dirty
        ));
    }

    #[test]
    fn api_pane_move_to_new_workspace_closes_empty_source_workspace() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_workspace = app.public_workspace_id(0);

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::NewWorkspace {
                    label: Some("promoted".into()),
                    tab_label: Some("main".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.closed_workspace_id, Some(source_workspace));
        assert_eq!(
            move_result
                .created_workspace
                .as_ref()
                .map(|ws| ws.label.as_str()),
            Some("promoted")
        );
        assert_eq!(
            move_result.created_workspace.as_ref().map(|ws| ws.focused),
            Some(true)
        );
        assert_eq!(
            move_result
                .created_tab
                .as_ref()
                .map(|tab| tab.label.as_str()),
            Some("main")
        );
        assert_eq!(
            move_result.created_tab.as_ref().map(|tab| tab.focused),
            Some(true)
        );
        assert_ne!(move_result.pane.pane_id, source_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_notify,
            &app.render_notify
        ));
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_dirty,
            &app.render_dirty
        ));
        let envelopes = app.event_hub.events_after(0);
        let events: Vec<_> = envelopes
            .iter()
            .map(|(_, envelope)| envelope.event)
            .collect();
        assert_eq!(
            events,
            vec![
                EventKind::TabClosed,
                EventKind::WorkspaceClosed,
                EventKind::WorkspaceCreated,
                EventKind::TabCreated,
                EventKind::PaneMoved,
            ]
        );
        match &envelopes[2].1.data {
            EventData::WorkspaceCreated { workspace } => assert!(workspace.focused),
            other => panic!("expected workspace created event, got {other:?}"),
        }
        match &envelopes[3].1.data {
            EventData::TabCreated { tab } => assert!(tab.focused),
            other => panic!("expected tab created event, got {other:?}"),
        }
    }

    #[test]
    fn api_pane_move_same_tab_returns_same_tab_noop() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_tab = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: source_tab,
                    target_pane_id: None,
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(!move_result.changed);
        assert_eq!(move_result.reason, Some(PaneMoveReason::SameTab));
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn api_pane_move_rejects_target_pane_outside_target_tab() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let other_tab = app.state.workspaces[0].test_add_tab(Some("other"));
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let wrong_target = app
            .public_pane_id(0, app.state.workspaces[0].tabs[other_tab].root_pane)
            .unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(wrong_target),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "target_pane_not_found");
        assert_eq!(app.state.workspaces[0].tabs.len(), 3);
    }

    #[test]
    fn api_pane_move_existing_tab_no_focus_preserves_previous_target_focus() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let previously_focused = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.workspaces[0].active_tab = target_tab;
        let previous_origin =
            app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let explicit_target =
            app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[target_tab]
            .layout
            .focus_pane(previous_origin);
        app.state.workspaces[0].tabs[target_tab]
            .layout
            .focus_pane(previously_focused);
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let explicit_target_public = app.public_pane_id(0, explicit_target).unwrap();
        let previously_focused_public = app.public_pane_id(0, previously_focused).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(explicit_target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.focused_pane_id, previously_focused_public);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            previously_focused
        );
        assert!(!app.state.workspaces[0].close_pane(previously_focused));
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            previous_origin
        );
    }

    #[test]
    fn api_pane_move_recovery_restores_removed_source_workspace() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let context = PaneMoveRecoveryContext {
            source_ws_idx: 0,
            previous_workspace_id: app.public_workspace_id(0),
            previous_workspace_label: app.state.workspaces[0].custom_name.clone(),
            previous_tab_label: app.state.workspaces[0].tabs[0].custom_name.clone(),
            previous_worktree_space: app.state.workspaces[0].worktree_space.clone(),
            identity_cwd: app.state.workspaces[0].identity_cwd.clone(),
        };
        let taken = app.state.workspaces[0]
            .take_pane_for_move(source)
            .expect("source pane should be movable");
        app.state.workspaces.remove(0);
        app.state.active = None;
        app.state.selected = 0;

        app.recover_failed_pane_move(context, taken.moved);

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].id, "w1");
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert_eq!(app.parse_pane_id("w1:p1"), Some((0, source)));
    }

    #[test]
    fn failed_cross_workspace_insertion_preserves_transient_state() {
        use crossterm::event::{KeyModifiers, MouseButton};

        let (mut app, source, _sibling, target) = app_with_cross_workspace_move_source(false);
        let source_workspace_id = app.state.workspaces[0].id.clone();
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(1, 0).unwrap();
        let target_public = app.public_pane_id(1, target).unwrap();
        let source_info = app.state.workspaces[0].tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(20, 2, 80, 20))[0]
            .clone();

        // Make the destination reject the moved pane after source extraction. The
        // duplicate lives only in this deliberately malformed test layout.
        assert!(app.state.workspaces[1].tabs[0].layout.insert_pane_near(
            target,
            source,
            ratatui::layout::Direction::Horizontal,
            0.5,
            false,
        ));

        app.state.previous_pane_focus = Some(crate::app::state::PaneFocusTarget {
            workspace_id: source_workspace_id.clone(),
            pane_id: source,
        });
        app.last_focus = Some((0, source));
        app.state.request_open_existing_worktree = Some(1);
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 3,
            cursor_col: 4,
            entry_offset_from_bottom: 5,
            selection: None,
            search: Default::default(),
        });
        app.state.selection = Some(crate::selection::Selection::range(source, 1, 2, 5, None));
        app.state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 7,
            last_mouse_screen_row: 8,
            inner_rect: source_info.inner_rect,
        });
        let selection_deadline = std::time::Instant::now();
        app.selection_autoscroll_deadline = Some(selection_deadline);
        app.state.context_menu = Some(crate::app::state::ContextMenuState {
            kind: crate::app::state::ContextMenuKind::Workspace { ws_idx: 0 },
            x: 9,
            y: 10,
            list: crate::app::state::MenuListState::new(0),
        });
        app.state.workspace_presses.insert(
            7,
            crate::app::state::WorkspacePressState {
                ws_idx: 0,
                start_col: 11,
                start_row: 12,
            },
        );
        app.state.drag = Some(crate::app::state::DragState {
            target: crate::app::state::DragTarget::PaneSplit {
                path: vec![true],
                direction: ratatui::layout::Direction::Horizontal,
                area: ratatui::layout::Rect::new(1, 2, 3, 4),
                grab_offset: 2,
            },
        });
        app.state.terminal_mouse_gestures.insert(
            (7, MouseButton::Left),
            crate::app::state::TerminalMouseGesture {
                pane_info: source_info.clone(),
                modifiers_to_strip: KeyModifiers::ALT,
            },
        );
        app.state.terminal_mouse_gestures.insert(
            (7, MouseButton::Right),
            crate::app::state::TerminalMouseGesture {
                pane_info: source_info.clone(),
                modifiers_to_strip: KeyModifiers::CONTROL,
            },
        );
        app.state.right_click_passthrough = Some(crate::app::state::RightClickPassthroughGesture {
            source_id: 7,
            pane_info: source_info,
        });
        app.state.mode = Mode::Copy;
        app.state.navigator.query = "keep query".into();
        app.state.navigator.selected = 4;
        app.state.navigator.scroll = 3;
        app.state
            .view
            .tab_hit_areas
            .push(ratatui::layout::Rect::new(5, 6, 7, 8));
        let pane_click_at = std::time::Instant::now();
        app.last_pane_click = Some(crate::app::PaneClickState {
            pane_id: source,
            viewport_row: 13,
            col: 14,
            at: pane_click_at,
        });

        let response = app.handle_pane_move(
            "move".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: false,
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "pane_move_failed");
        assert_eq!(app.state.workspaces[0].id, source_workspace_id);
        assert_eq!(app.parse_pane_id("w1:p1"), Some((0, source)));
        assert_eq!(
            app.state.previous_pane_focus,
            Some(crate::app::state::PaneFocusTarget {
                workspace_id: source_workspace_id,
                pane_id: source,
            })
        );
        assert_eq!(app.last_focus, Some((0, source)));
        assert_eq!(app.state.request_open_existing_worktree, Some(1));
        assert_eq!(app.state.mode, Mode::Copy);
        let copy = app.state.copy_mode.as_ref().unwrap();
        assert_eq!(
            (copy.pane_id, copy.cursor_row, copy.cursor_col),
            (source, 3, 4)
        );
        assert_eq!(app.state.selection.as_ref().unwrap().pane_id, source);
        let autoscroll = app.state.selection_autoscroll.as_ref().unwrap();
        assert_eq!(
            (
                autoscroll.last_mouse_screen_col,
                autoscroll.last_mouse_screen_row
            ),
            (7, 8)
        );
        assert_eq!(app.selection_autoscroll_deadline, Some(selection_deadline));
        assert!(matches!(
            app.state.context_menu.as_ref().map(|menu| &menu.kind),
            Some(crate::app::state::ContextMenuKind::Workspace { ws_idx: 0 })
        ));
        assert_eq!(app.state.workspace_presses.get(&7).unwrap().ws_idx, 0);
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(crate::app::state::DragTarget::PaneSplit { path, .. }) if path == &[true]
        ));
        let gesture = app
            .state
            .terminal_mouse_gestures
            .get(&(7, MouseButton::Left))
            .unwrap();
        assert_eq!(gesture.pane_info.id, source);
        assert_eq!(gesture.modifiers_to_strip, KeyModifiers::ALT);
        let gesture = app
            .state
            .terminal_mouse_gestures
            .get(&(7, MouseButton::Right))
            .unwrap();
        assert_eq!(gesture.pane_info.id, source);
        assert_eq!(gesture.modifiers_to_strip, KeyModifiers::CONTROL);
        let right = app.state.right_click_passthrough.as_ref().unwrap();
        assert_eq!((right.source_id, right.pane_info.id), (7, source));
        assert_eq!(app.state.navigator.query, "keep query");
        assert_eq!(
            (app.state.navigator.selected, app.state.navigator.scroll),
            (4, 3)
        );
        assert_eq!(
            app.state.view.tab_hit_areas,
            vec![ratatui::layout::Rect::new(5, 6, 7, 8)]
        );
        let click = app.last_pane_click.unwrap();
        assert_eq!(
            (click.pane_id, click.viewport_row, click.col),
            (source, 13, 14)
        );
        assert_eq!(click.at, pane_click_at);
    }

    #[test]
    fn api_pane_move_to_zoomed_target_returns_target_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.workspaces[0].tabs[target_tab].zoomed = true;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public.clone(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(!move_result.changed);
        assert_eq!(move_result.reason, Some(PaneMoveReason::ZoomedTab));
        assert_eq!(move_result.target_layout.tab_id, target_tab_public);
        assert_eq!(
            move_result
                .source_layout
                .as_ref()
                .map(|layout| layout.tab_id.as_str()),
            app.public_tab_id(0, 0).as_deref()
        );
    }

    #[test]
    fn api_pane_zoom_current_toggles_zoom() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let _right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom("req".into(), PaneZoomParams::default());

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, None);
        assert_eq!(zoom.pane_id, root_public);
        assert_eq!(zoom.focused_pane_id, zoom.pane_id);
        assert!(zoom.zoomed);
        assert!(zoom.layout.zoomed);

        let response = app.handle_pane_zoom("req".into(), PaneZoomParams::default());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(!zoom.zoomed);
        assert!(!zoom.layout.zoomed);
    }

    #[test]
    fn api_pane_zoom_explicit_background_pane_updates_focus_history() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let _other = app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].tabs[0].layout.focus_pane(first);
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(target_public.clone()),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(zoom.focus_changed);
        assert_eq!(zoom.pane_id, target_public);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target));
        assert!(app.state.workspaces[1].tabs[0].zoomed);

        app.state.last_pane();

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(first));
    }

    #[test]
    fn api_pane_zoom_focuses_copy_mode_pane_back_into_copy_mode() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let _other = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let _target_other =
            app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[1].tabs[0].layout.focus_pane(target);
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(source_public),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.focus_changed);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target));
    }

    #[test]
    fn api_pane_zoom_single_pane_returns_noop() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::Toggle,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::SinglePane));
        assert_eq!(zoom.pane_id, root_public);
        assert!(!zoom.zoomed);
        assert!(!app.state.workspaces[0].tabs[0].zoomed);
    }

    #[test]
    fn api_pane_zoom_on_and_off_are_idempotent() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let _right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::On,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::On,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyZoomed));
        assert!(zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public),
                mode: PaneZoomMode::Off,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(!zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: None,
                mode: PaneZoomMode::Off,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyUnzoomed));
        assert!(!zoom.zoomed);
    }

    #[test]
    fn api_pane_zoom_idempotent_mode_reports_focus_change() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        app.state.workspaces[0].tabs[0].zoomed = true;
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(right_public),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyZoomed));
        assert!(zoom.zoomed);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn api_pane_zoom_params_serialize_modes() {
        let request = crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneZoom(PaneZoomParams {
                pane_id: Some("issue-1".into()),
                mode: PaneZoomMode::On,
            }),
        };

        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"method\":\"pane.zoom\""));
        assert!(encoded.contains("\"mode\":\"on\""));

        let decoded: crate::api::schema::Request = serde_json::from_str(&encoded).unwrap();
        let crate::api::schema::Method::PaneZoom(params) = decoded.method else {
            panic!("expected pane zoom request");
        };
        assert_eq!(params.pane_id, Some("issue-1".into()));
        assert_eq!(params.mode, PaneZoomMode::On);
    }

    #[test]
    fn api_pane_layout_returns_public_ids_rects_and_splits() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_layout(
            "req".into(),
            crate::api::schema::PaneLayoutParams {
                pane_id: Some(root_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneLayout { layout } = success.result else {
            panic!("expected pane layout response");
        };
        assert_eq!(layout.focused_pane_id, root_public);
        assert!(layout.panes.iter().any(|pane| pane.pane_id == root_public));
        assert!(layout.panes.iter().any(|pane| pane.pane_id == right_public));
        assert_eq!(layout.splits.len(), 1);
        assert_eq!(
            layout.splits[0].direction,
            crate::api::schema::SplitDirection::Right
        );
    }

    #[test]
    fn api_pane_neighbor_returns_directional_neighbor_public_id() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_neighbor(
            "req".into(),
            crate::api::schema::PaneNeighborParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneNeighbor { neighbor } = success.result else {
            panic!("expected pane neighbor response");
        };
        assert_eq!(neighbor.pane_id, root_public);
        assert_eq!(neighbor.direction, PaneDirection::Right);
        assert_eq!(neighbor.neighbor_pane_id, Some(right_public));
    }

    #[test]
    fn api_pane_edges_reports_physical_layout_edges() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_edges(
            "req".into(),
            crate::api::schema::PaneEdgesParams {
                pane_id: Some(right_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneEdges { edges } = success.result else {
            panic!("expected pane edges response");
        };
        assert_eq!(edges.pane_id, right_public);
        assert!(!edges.left);
        assert!(edges.right);
        assert!(edges.up);
        assert!(edges.down);
    }

    #[test]
    fn api_pane_resize_changes_target_ratio_without_changing_focus() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(right);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_resize(
            "req".into(),
            crate::api::schema::PaneResizeParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
                amount: Some(0.1),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneResize { resize } = success.result else {
            panic!("expected pane resize response");
        };
        assert!(resize.changed);
        assert_eq!(resize.reason, None);
        assert_eq!(resize.pane_id, root_public);
        assert_eq!(resize.focused_pane_id, right_public);
        assert_eq!(resize.layout.focused_pane_id, right_public);
        assert!((resize.layout.splits[0].ratio - 0.6).abs() < f32::EPSILON);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn api_pane_focus_direction_focuses_neighbor() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_focus_direction(
            "req".into(),
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneFocusDirection { focus } = success.result else {
            panic!("expected pane focus direction response");
        };
        assert!(focus.changed);
        assert_eq!(focus.reason, None);
        assert_eq!(focus.source_pane_id, root_public);
        assert_eq!(focus.focused_pane_id, Some(right_public.clone()));
        assert_eq!(focus.layout.focused_pane_id, right_public);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn api_pane_focus_focuses_direct_target_across_tabs_and_workspaces() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let target_tab_idx = app.state.workspaces[1].test_add_tab(Some("target"));
        app.state.workspaces[1].switch_tab(target_tab_idx);
        let target_pane = app.state.workspaces[1].tabs[target_tab_idx].root_pane;
        app.state.ensure_test_terminals();
        let target_public = app.public_pane_id(1, target_pane).unwrap();
        app.state.switch_workspace(0);
        assert_eq!(app.state.active, Some(0));

        let response = app.handle_pane_focus(
            "req".into(),
            crate::api::schema::PaneTarget {
                pane_id: target_public.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info response");
        };
        assert_eq!(pane.pane_id, target_public);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab, target_tab_idx);
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn api_pane_focus_rejects_invalid_pane_id() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_pane_focus(
            "req".into(),
            crate::api::schema::PaneTarget {
                pane_id: "pane_missing".into(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "pane_not_found");
    }

    #[test]
    fn api_pane_focus_direction_no_neighbor_is_noop() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_focus_direction(
            "req".into(),
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Left,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneFocusDirection { focus } = success.result else {
            panic!("expected pane focus direction response");
        };
        assert!(!focus.changed);
        assert_eq!(focus.reason, Some(PaneFocusDirectionReason::NoNeighbor));
        assert_eq!(focus.source_pane_id, root_public.clone());
        assert_eq!(focus.focused_pane_id, Some(root_public));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
    }
}
