use ratatui::layout::Rect;

use crate::app::state::{AppState, ViewLayout};

use super::ScrollbarClickTarget;

impl AppState {
    pub(super) fn workspace_list_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        crate::ui::workspace_list_rect(sidebar, self.sidebar_section_split)
    }

    pub(super) fn agent_panel_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        let (_, detail_area) =
            crate::ui::expanded_sidebar_sections(sidebar, self.sidebar_section_split);
        detail_area
    }

    pub(super) fn workspace_list_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn workspace_list_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_workspace_list_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn scroll_workspace_list(&mut self, delta: i16) {
        if delta.is_negative() {
            self.workspace_scroll = self
                .workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
            );
            return;
        }

        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = self
            .workspace_scroll
            .saturating_add(delta as usize)
            .min(metrics.max_offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn agent_panel_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        let track = crate::ui::agent_panel_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn agent_panel_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        let track = crate::ui::agent_panel_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_agent_panel_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        self.agent_panel_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
    }

    pub(super) fn scroll_agent_panel(&mut self, delta: i16) {
        let area = self.agent_panel_rect();
        let max_scroll = crate::ui::agent_panel_scroll_metrics(self, area).max_offset_from_bottom;
        if delta.is_negative() {
            self.agent_panel_scroll = self
                .agent_panel_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.agent_panel_scroll = self
                .agent_panel_scroll
                .saturating_add(delta as usize)
                .min(max_scroll);
        }
    }

    pub(crate) fn sidebar_footer_rect(&self) -> Rect {
        let ws_area = self.workspace_list_rect();
        if ws_area == Rect::default() {
            return Rect::default();
        }
        let y = ws_area.y + ws_area.height.saturating_sub(1);
        Rect::new(ws_area.x, y, ws_area.width, 1)
    }

    pub(crate) fn sidebar_new_button_rect(&self) -> Rect {
        let footer = self.sidebar_footer_rect();
        let width = 5u16.min(footer.width.max(1));
        Rect::new(footer.x, footer.y, width, footer.height)
    }

    pub(crate) fn global_launcher_rect(&self) -> Rect {
        if self.view.layout == ViewLayout::Mobile {
            return self.view.mobile_menu_hit_area;
        }

        let footer = self.sidebar_footer_rect();
        let width = if self.global_menu_attention_badge_visible() {
            8
        } else {
            6
        }
        .min(footer.width.max(1));
        let x = footer.x + footer.width.saturating_sub(width);
        Rect::new(x, footer.y, width, footer.height)
    }

    pub(crate) fn global_menu_labels(&self) -> Vec<&'static str> {
        let mut labels = vec!["settings", "keybinds", "reload config"];
        if self.update_available.is_some() {
            labels.push("update ready");
        } else if self.latest_release_notes_available {
            labels.push("what's new");
        }
        labels.push("detach");
        labels
    }

    pub(crate) fn global_menu_rect(&self) -> Rect {
        let screen = self.screen_rect();
        let launcher = self.global_launcher_rect();
        let labels = self.global_menu_labels();
        let content_width = labels
            .iter()
            .map(|label| {
                let badge_width = if self.global_menu_item_has_badge(label) {
                    2
                } else {
                    0
                };
                label.chars().count() as u16 + badge_width
            })
            .max()
            .unwrap_or(8)
            .saturating_add(2);
        let menu_w = content_width.saturating_add(2).min(screen.width.max(1));
        let menu_h = (labels.len() as u16 + 2).min(screen.height.max(1));
        let max_x = screen.x + screen.width.saturating_sub(menu_w);
        let desired_x = launcher.x + launcher.width.saturating_sub(menu_w);
        let x = desired_x.min(max_x);
        let y = launcher.y.saturating_sub(menu_h);
        Rect::new(x, y, menu_w, menu_h)
    }

    pub(super) fn on_sidebar_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed || self.agent_view_override.is_some() {
            return false;
        }
        let sidebar = self.view.sidebar_rect;
        let toggle = crate::ui::expanded_sidebar_toggle_rect(sidebar);
        let on_toggle = toggle.width > 0
            && col >= toggle.x
            && col < toggle.x + toggle.width
            && row >= toggle.y
            && row < toggle.y + toggle.height;
        sidebar.width > 0
            && !on_toggle
            && col == sidebar.x + sidebar.width.saturating_sub(1)
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
    }

    pub(super) fn on_sidebar_toggle(&self, col: u16, row: u16) -> bool {
        let rect = if self.sidebar_collapsed {
            crate::ui::collapsed_sidebar_toggle_rect(self.view.sidebar_rect)
        } else {
            crate::ui::expanded_sidebar_toggle_rect(self.view.sidebar_rect)
        };
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_manual_sidebar_width(&mut self, divider_col: u16) {
        let sidebar = self.view.sidebar_rect;
        let width = divider_col.saturating_sub(sidebar.x).saturating_add(1);
        self.sidebar_width = width.clamp(self.sidebar_min_width, self.sidebar_max_width);
        self.sidebar_width_source = crate::app::state::SidebarWidthSource::Manual;
        self.mark_session_dirty();
    }

    pub(super) fn on_sidebar_section_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let rect = crate::ui::sidebar_section_divider_rect(
            self.view.sidebar_rect,
            self.sidebar_section_split,
        );
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_sidebar_section_split(&mut self, row: u16) {
        let sidebar = self.view.sidebar_rect;
        let content_height = sidebar.height;
        if content_height < 6 {
            return;
        }
        let relative_y = row.saturating_sub(sidebar.y);
        let ratio = (relative_y as f32) / (content_height as f32);
        self.sidebar_section_split = ratio.clamp(0.1, 0.9);
        self.mark_session_dirty();
    }

    pub(super) fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let footer = self.sidebar_footer_rect();
        if footer == Rect::default() {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };

        cards.iter().find_map(|card| {
            (row >= card.rect.y && row < card.rect.y + card.rect.height).then_some(card.ws_idx)
        })
    }

    pub(super) fn collapsed_workspace_at_row(&self, row: u16) -> Option<usize> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (ws_area, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() || row < ws_area.y || row >= ws_area.y + ws_area.height {
            return None;
        }

        let idx = (row - ws_area.y) as usize;
        (idx < self.workspaces.len()).then_some(idx)
    }

    pub(super) fn collapsed_agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (_, _, detail_area) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        let detail_content_area = Rect::new(
            detail_area.x,
            detail_area.y,
            detail_area.width,
            detail_area.height.saturating_sub(1),
        );
        if detail_content_area == Rect::default()
            || row < detail_content_area.y
            || row >= detail_content_area.y + detail_content_area.height
        {
            return None;
        }

        let detail_idx = (row - detail_content_area.y) as usize;
        let details = crate::ui::agent_panel_entries(self);
        let detail = details.get(detail_idx)?;
        Some((detail.ws_idx, detail.tab_idx, detail.pane_id))
    }

    pub(super) fn workspace_drop_index_at_row(&self, row: u16) -> Option<usize> {
        let area = self.workspace_list_rect();
        let footer = self.sidebar_footer_rect();
        if area == Rect::default() || row < area.y || row >= footer.y {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };
        if cards.is_empty() {
            return Some(0);
        }

        let mut insert_indices = Vec::with_capacity(cards.len() + 1);
        for (idx, card) in cards.iter().enumerate() {
            let card_group = self
                .workspaces
                .get(card.ws_idx)
                .and_then(|ws| ws.worktree_space())
                .map(|space| space.key.as_str());
            let previous_group = idx.checked_sub(1).and_then(|prev_idx| {
                self.workspaces
                    .get(cards[prev_idx].ws_idx)
                    .and_then(|ws| ws.worktree_space())
                    .map(|space| space.key.as_str())
            });
            let inside_group_gap = card_group.is_some() && card_group == previous_group;
            if !inside_group_gap {
                insert_indices.push(card.ws_idx);
            }
        }
        insert_indices.push(cards.last().map(|card| card.ws_idx + 1).unwrap_or(0));

        let mut best: Option<(usize, u16)> = None;
        for insert_idx in insert_indices {
            let Some(slot_row) = crate::ui::workspace_drop_indicator_row(&cards, area, insert_idx)
            else {
                continue;
            };
            let distance = row.abs_diff(slot_row);
            match best {
                Some((best_idx, best_distance))
                    if distance > best_distance
                        || (distance == best_distance && insert_idx < best_idx) => {}
                _ => best = Some((insert_idx, distance)),
            }
        }

        best.map(|(insert_idx, _)| insert_idx)
    }

    pub(super) fn on_agent_panel_sort_toggle(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }

        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            self.view.sidebar_rect,
            self.sidebar_section_split,
        );
        let rect = crate::ui::agent_panel_toggle_rect(detail_area, self.agent_panel_sort);
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        if self.sidebar_collapsed {
            return None;
        }

        let detail_area = self.agent_panel_rect();
        let entries = crate::ui::agent_panel_entries(self);
        // Resolve through the shared grouped visible-row model: only a `Child` row maps to a pane; a
        // group header or an inter-group spacer resolves to nothing. Same model the render uses, so
        // clicks and what is drawn can never drift.
        crate::ui::agent_visible_rows(self, detail_area)
            .into_iter()
            .find_map(|visible| match visible {
                crate::ui::AgentVisibleRow::Child {
                    entry_idx,
                    y,
                    height,
                    ..
                } if row >= y && row < y.saturating_add(height) => entries
                    .get(entry_idx)
                    .map(|e| (e.ws_idx, e.tab_idx, e.pane_id)),
                _ => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, capture_snapshot, mouse, unique_temp_path};
    use crate::{
        app::state::{AgentPanelSort, DragTarget, Mode},
        config::SidebarCollapsedModeConfig,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    #[test]
    fn m828d2_every_agent_content_line_is_a_hit_but_headers_and_gaps_are_not() {
        let mut app = app_for_mouse_test();
        let source = "onboarding = false\n[ui.sidebar.agents]\nrow_gap = 2\nrows = [[\"agent\"], [\"$one\"], [\"$two\"]]\n";
        assert!(source.parse::<toml::Value>().is_ok());
        let config: crate::config::Config = toml::from_str(source).unwrap();
        app.apply_live_config(&config, &[], &[], false);
        let mut alpha = Workspace::test_new("alpha");
        alpha.test_split(ratatui::layout::Direction::Horizontal);
        alpha.test_add_tab(Some("logs"));
        app.state.workspaces = vec![alpha, Workspace::test_new("beta")];
        app.state.ensure_test_terminals();
        let ids: Vec<_> = app
            .state
            .workspaces
            .iter()
            .flat_map(|ws| {
                ws.tabs.iter().flat_map(|tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .map(|pane| tab.panes[&pane].attached_terminal_id.clone())
                })
            })
            .collect();
        assert_eq!(ids.len(), 4);
        for (index, (id, agent)) in ids
            .iter()
            .zip([Agent::Claude, Agent::Pi, Agent::Codex, Agent::Claude])
            .enumerate()
        {
            let terminal = app.state.terminals.get_mut(id).unwrap();
            terminal.detected_agent = Some(agent);
            terminal.state = AgentState::Working;
            let mut patch = std::collections::HashMap::new();
            if index != 1 {
                patch.insert("one".into(), Some(format!("ONE-{index}")));
            }
            if index == 0 || index == 3 {
                patch.insert("two".into(), Some(format!("TWO-{index}")));
            }
            if !patch.is_empty() {
                assert!(terminal
                    .metadata_tokens
                    .patch(patch, None, std::time::Instant::now()));
            }
            assert_eq!(terminal.effective_known_agent(), Some(agent));
            assert_eq!(terminal.metadata_tokens.values().len(), [2, 0, 1, 2][index]);
            if index != 1 {
                assert_eq!(
                    terminal.metadata_tokens.values()["one"],
                    format!("ONE-{index}")
                );
            }
            if index == 0 || index == 3 {
                assert_eq!(
                    terminal.metadata_tokens.values()["two"],
                    format!("TWO-{index}")
                );
            }
        }
        app.state.agent_panel_sort = AgentPanelSort::Spaces;
        app.state.active = Some(0);
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 70));
        let area = app.state.agent_panel_rect();
        let entries = crate::ui::agent_panel_entries(&app.state);
        let targets: Vec<_> = entries
            .iter()
            .map(|entry| (entry.ws_idx, entry.tab_idx, entry.pane_id))
            .collect();
        assert_eq!(
            targets
                .iter()
                .map(|(ws, tab, _)| (*ws, *tab))
                .collect::<Vec<_>>(),
            vec![(0, 0), (0, 0), (0, 1), (1, 0)]
        );
        let rows = crate::ui::agent_visible_rows(&app.state, area);
        assert!(
            matches!(rows.first(), Some(crate::ui::AgentVisibleRow::GroupHeader {
            entry_idx: 0, y,
        }) if *y == area.y + 3)
        );
        let starts = [area.y + 4, area.y + 9, area.y + 13, area.y + 18];
        let heights = [3, 1, 2, 3];
        let children: Vec<_> = crate::ui::agent_visible_rows(&app.state, area)
            .iter()
            .filter_map(|row| match row {
                crate::ui::AgentVisibleRow::Child { entry_idx, y, .. } => Some((*entry_idx, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(
            children,
            starts.iter().copied().enumerate().collect::<Vec<_>>(),
            "heterogeneous content precedes hit tests"
        );
        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(106, 70)).unwrap();
        screen
            .draw(|frame| crate::ui::render(&app.state, frame))
            .unwrap();
        let row_text = |y| {
            (area.x..area.right())
                .map(|x| screen.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        };
        for (index, name) in ["claude", "pi", "codex", "claude"].iter().enumerate() {
            assert!(row_text(starts[index]).contains(name));
            if heights[index] >= 2 {
                assert!(row_text(starts[index] + 1).contains(&format!("ONE-{index}")));
            }
            if heights[index] == 3 {
                assert!(row_text(starts[index] + 2).contains(&format!("TWO-{index}")));
            }
        }
        for (index, &(ws, tab, pane)) in targets.iter().enumerate() {
            for y in starts[index]..starts[index] + heights[index] {
                let other = targets[(index + 1) % targets.len()];
                app.state.focus_pane_in_workspace(ws, pane);
                assert!(app.state.focus_pane_in_workspace(other.0, other.2));
                app.state.agent_panel_scroll = 0;
                assert_eq!(app.state.active, Some(other.0));
                assert_ne!(app.state.workspaces[other.0].focused_pane_id(), Some(pane));
                assert_eq!(app.state.agent_detail_target_at(y), Some((ws, tab, pane)));
                app.handle_mouse(mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x + 2,
                    y,
                ));
                app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), area.x + 2, y));
                assert_eq!(app.state.active, Some(ws));
                assert_eq!(app.state.workspaces[ws].active_tab, tab);
                assert_eq!(app.state.workspaces[ws].tabs[tab].layout.focused(), pane);
            }
        }
        for y in starts[0] - 1..starts[3] + heights[3] {
            if starts
                .iter()
                .zip(heights)
                .any(|(&start, height)| (start..start + height).contains(&y))
            {
                continue;
            }
            app.state
                .focus_pane_in_workspace(targets[0].0, targets[0].2);
            assert!(app
                .state
                .focus_pane_in_workspace(targets[1].0, targets[1].2));
            assert!(app
                .state
                .focus_pane_in_workspace(targets[0].0, targets[0].2));
            app.state.agent_panel_scroll = 0;
            assert_eq!(
                app.state.agent_detail_target_at(starts[0]),
                Some(targets[0])
            );
            assert_eq!(
                app.state.agent_detail_target_at(y),
                None,
                "populated header/gap {y}"
            );
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 2,
                y,
            ));
            app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), area.x + 2, y));
            assert_eq!(app.state.active, Some(targets[0].0));
            assert_eq!(app.state.workspaces[targets[0].0].active_tab, targets[0].1);
            assert_eq!(
                app.state.workspaces[targets[0].0].focused_pane_id(),
                Some(targets[0].2)
            );
        }
    }

    #[test]
    fn m828d2_variable_height_wheel_and_drag_use_shared_metrics() {
        let fixture = || {
            let mut app = app_for_mouse_test();
            let source = "onboarding = false\n[ui.sidebar.spaces]\nrow_gap = 1\nrows = [[\"workspace\"], [\"$more\"]]\n[ui.sidebar.agents]\nrow_gap = 1\nrows = [[\"agent\"], [\"$more\"]]\n";
            assert!(source.parse::<toml::Value>().is_ok());
            let config: crate::config::Config = toml::from_str(source).unwrap();
            app.apply_live_config(&config, &[], &[], false);
            app.state.workspaces = (0..8)
                .map(|i| Workspace::test_new(&format!("space-{i}")))
                .collect();
            for (index, workspace) in app.state.workspaces.iter_mut().enumerate() {
                workspace.cached_git_branch = None;
                if index % 2 == 0 {
                    assert!(workspace.metadata_tokens.patch(
                        std::collections::HashMap::from([(
                            "more".into(),
                            Some(format!("SPACE-{index}"))
                        )]),
                        None,
                        std::time::Instant::now()
                    ));
                    assert_eq!(
                        workspace.metadata_tokens.values()["more"],
                        format!("SPACE-{index}")
                    );
                } else {
                    assert!(workspace.metadata_tokens.values().is_empty());
                }
            }
            app.state.ensure_test_terminals();
            for (index, workspace) in app.state.workspaces.iter().enumerate() {
                let tab = &workspace.tabs[0];
                let id = &tab.panes[&tab.root_pane].attached_terminal_id;
                let terminal = app.state.terminals.get_mut(id).unwrap();
                terminal.detected_agent = Some(Agent::Claude);
                terminal.state = AgentState::Working;
                if index % 2 == 0 {
                    assert!(terminal.metadata_tokens.patch(
                        std::collections::HashMap::from([(
                            "more".into(),
                            Some(format!("AGENT-{index}"))
                        )]),
                        None,
                        std::time::Instant::now()
                    ));
                    assert_eq!(
                        terminal.metadata_tokens.values()["more"],
                        format!("AGENT-{index}")
                    );
                } else {
                    assert!(terminal.metadata_tokens.values().is_empty());
                }
                assert_eq!(terminal.effective_known_agent(), Some(Agent::Claude));
            }
            app.state.active = Some(0);
            app.state.mode = Mode::Terminal;
            app.state.agent_panel_sort = AgentPanelSort::Spaces;
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));
            assert_eq!(crate::ui::agent_panel_entries(&app.state).len(), 8);
            app
        };
        let mut app = fixture();
        let spaces = app.state.workspace_list_rect();
        let agents = app.state.agent_panel_rect();
        assert_eq!(
            (spaces.y, spaces.height, agents.y, agents.height),
            (0, 12, 12, 12)
        );
        assert_eq!(
            app.state
                .view
                .workspace_card_areas
                .iter()
                .map(|card| (card.ws_idx, card.rect))
                .collect::<Vec<_>>(),
            vec![
                (0, Rect::new(0, 2, 24, 2)),
                (1, Rect::new(0, 5, 24, 1)),
                (2, Rect::new(0, 7, 24, 2)),
                (3, Rect::new(0, 10, 24, 1))
            ]
        );
        let children = |state: &crate::app::state::AppState, area| {
            crate::ui::agent_visible_rows(state, area)
                .iter()
                .filter_map(|row| match row {
                    crate::ui::AgentVisibleRow::Child { entry_idx, y, .. } => {
                        Some((*entry_idx, *y))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(children(&app.state, agents), vec![(0, 16), (1, 20)]);
        assert_eq!(
            crate::ui::workspace_list_scroll_metrics(&app.state, spaces).max_offset_from_bottom,
            4
        );
        assert_eq!(
            crate::ui::agent_panel_scroll_metrics(&app.state, agents).max_offset_from_bottom,
            5
        );
        for is_agent in [false, true] {
            let area = if is_agent { agents } else { spaces };
            app.handle_mouse(mouse(MouseEventKind::ScrollDown, area.x + 1, area.y + 4));
            assert_eq!(
                if is_agent {
                    app.state.agent_panel_scroll
                } else {
                    app.state.workspace_scroll
                },
                1
            );
            app.handle_mouse(mouse(MouseEventKind::ScrollUp, area.x + 1, area.y + 4));
            assert_eq!(
                if is_agent {
                    app.state.agent_panel_scroll
                } else {
                    app.state.workspace_scroll
                },
                0
            );
            let track = if is_agent {
                crate::ui::agent_panel_scrollbar_rect(&app.state, area)
            } else {
                crate::ui::workspace_list_scrollbar_rect(&app.state, area)
            }
            .unwrap();
            let target = if is_agent {
                app.state.agent_panel_scrollbar_target_at(track.x, track.y)
            } else {
                app.state
                    .workspace_list_scrollbar_target_at(track.x, track.y)
            };
            assert!(matches!(
                target,
                Some(super::ScrollbarClickTarget::Thumb { grab_row_offset: 0 })
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                track.x,
                track.y,
            ));
            if is_agent {
                assert!(matches!(
                    app.state.drag.as_ref().map(|drag| &drag.target),
                    Some(DragTarget::AgentPanelScrollbar { .. })
                ));
            } else {
                assert!(matches!(
                    app.state.drag.as_ref().map(|drag| &drag.target),
                    Some(DragTarget::WorkspaceListScrollbar { .. })
                ));
            }
            app.handle_mouse(mouse(
                MouseEventKind::Drag(MouseButton::Left),
                track.x,
                track.bottom() - 1,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                track.x,
                track.bottom() - 1,
            ));
            assert_eq!(
                if is_agent {
                    app.state.agent_panel_scroll
                } else {
                    app.state.workspace_scroll
                },
                if is_agent { 5 } else { 4 }
            );
            if is_agent {
                let last = children(&app.state, area)
                    .into_iter()
                    .find(|(index, _)| *index == 7)
                    .unwrap();
                let entry = &crate::ui::agent_panel_entries(&app.state)[7];
                assert_eq!(
                    app.state.agent_detail_target_at(last.1),
                    Some((entry.ws_idx, entry.tab_idx, entry.pane_id))
                );
                assert_eq!(app.state.agent_detail_target_at(area.y + 2), None);
            } else {
                crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));
                let last = app.state.view.workspace_card_areas.last().unwrap();
                assert_eq!((last.ws_idx, last.rect.y, last.rect.height), (7, 10, 1));
                assert_eq!(app.state.workspace_at_row(last.rect.y), Some(7));
                assert_eq!(app.state.workspace_at_row(area.y + 1), None);
            }
        }
        app.state.workspace_scroll = usize::MAX;
        app.state.agent_panel_scroll = usize::MAX;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));
        assert_eq!(
            (app.state.workspace_scroll, app.state.agent_panel_scroll),
            (4, 5)
        );
        assert_eq!(
            app.state.view.workspace_card_areas.last().unwrap().ws_idx,
            7
        );
        assert!(children(&app.state, agents)
            .iter()
            .any(|(index, _)| *index == 7));

        for width in [0u16, 1, 2] {
            for sort in [AgentPanelSort::Spaces, AgentPanelSort::Priority] {
                let mut app = fixture();
                assert!(!app.state.view.workspace_card_areas.is_empty());
                assert!(!children(&app.state, app.state.agent_panel_rect()).is_empty());
                app.state.sidebar_min_width = 1;
                app.state.sidebar_width = width + 1;
                app.state.agent_panel_sort = sort;
                crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));
                let full = app.state.view.sidebar_rect;
                assert_eq!(full.width, width + 1);
                let spaces = app.state.workspace_list_rect();
                let agents = app.state.agent_panel_rect();
                assert_eq!((spaces.width, agents.width), (width, width));
                let space_track = crate::ui::workspace_list_scrollbar_rect(&app.state, spaces);
                let agent_track = crate::ui::agent_panel_scrollbar_rect(&app.state, agents);
                let toggle = crate::ui::agent_panel_toggle_rect(agents, sort);
                if width == 0 {
                    assert!(space_track.is_none() && agent_track.is_none());
                    assert!(app.state.view.workspace_card_areas.is_empty());
                    assert!(children(&app.state, agents).is_empty());
                    assert_eq!(app.state.workspace_at_row(2), None);
                    assert_eq!(app.state.agent_detail_target_at(16), None);
                    assert_eq!(toggle, Rect::default());
                    assert!(!app.state.on_agent_panel_sort_toggle(0, 13));
                } else {
                    assert_eq!(
                        crate::ui::workspace_list_scroll_metrics(&app.state, spaces)
                            .max_offset_from_bottom,
                        4
                    );
                    assert_eq!(
                        crate::ui::agent_panel_scroll_metrics(&app.state, agents)
                            .max_offset_from_bottom,
                        5
                    );
                    assert_eq!(space_track.is_some(), width == 2);
                    assert_eq!(agent_track.is_some(), width == 2);
                    assert!(app
                        .state
                        .view
                        .workspace_card_areas
                        .iter()
                        .all(|card| card.rect.width == 1));
                    assert_eq!(app.state.workspace_at_row(2), Some(0));
                    assert_eq!(children(&app.state, agents), vec![(0, 16), (1, 20)]);
                    let entries = crate::ui::agent_panel_entries(&app.state);
                    assert_eq!(
                        app.state.agent_detail_target_at(17),
                        Some((entries[0].ws_idx, entries[0].tab_idx, entries[0].pane_id))
                    );
                    assert!(app
                        .state
                        .focus_pane_in_workspace(entries[1].ws_idx, entries[1].pane_id));
                    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 17));
                    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 0, 17));
                    assert_eq!(app.state.active, Some(entries[0].ws_idx));
                    assert_eq!(
                        app.state.workspaces[entries[0].ws_idx].focused_pane_id(),
                        Some(entries[0].pane_id)
                    );
                    if let Some(track) = space_track {
                        assert_eq!((track.x, track.width), (1, 1));
                    }
                    if let Some(track) = agent_track {
                        assert_eq!((track.x, track.width), (1, 1));
                    }
                    assert_eq!(toggle, Rect::new(agents.x, agents.y + 1, width, 1));
                    assert!(app.state.on_agent_panel_sort_toggle(toggle.x, toggle.y));
                    assert!(!app
                        .state
                        .on_agent_panel_sort_toggle(toggle.right(), toggle.y));
                    app.state.agent_panel_scroll = 1;
                    app.handle_mouse(mouse(
                        MouseEventKind::Down(MouseButton::Left),
                        toggle.x,
                        toggle.y,
                    ));
                    app.handle_mouse(mouse(
                        MouseEventKind::Up(MouseButton::Left),
                        toggle.x,
                        toggle.y,
                    ));
                    assert_ne!(app.state.agent_panel_sort, sort);
                    assert_eq!(app.state.agent_panel_scroll, 0);
                    app.state.agent_panel_sort = sort;
                    for col in [toggle.right(), full.right()] {
                        app.handle_mouse(mouse(
                            MouseEventKind::Down(MouseButton::Left),
                            col,
                            toggle.y,
                        ));
                        app.handle_mouse(mouse(
                            MouseEventKind::Up(MouseButton::Left),
                            col,
                            toggle.y,
                        ));
                        assert_eq!(app.state.agent_panel_sort, sort, "outside toggle dispatch");
                        app.state.drag = None;
                    }
                    assert!(app.state.focus_agent_entry(7));
                    assert!(children(&app.state, agents)
                        .iter()
                        .any(|(index, _)| *index == 7));
                    app.state.agent_panel_scroll = 0;
                }
                app.state.view.pane_infos.clear();
                app.state.view.split_borders.clear();
                app.state.view.tab_bar_rect = Rect::default();
                app.state.view.terminal_area = Rect::default();
                let mut screen =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(106, 24)).unwrap();
                screen
                    .draw(|frame| {
                        for cell in &mut frame.buffer_mut().content {
                            cell.set_symbol("#");
                        }
                        crate::ui::render(&app.state, frame);
                    })
                    .unwrap();
                for y in 0..24 {
                    for x in full.right()..106 {
                        assert_eq!(
                            screen.backend().buffer()[(x, y)].symbol(),
                            "#",
                            "outside full sidebar {width}/{sort:?}/{x}/{y}"
                        );
                    }
                }
                if width > 0 {
                    assert_eq!(
                        screen.backend().buffer()[(0, toggle.y)].symbol(),
                        if sort == AgentPanelSort::Spaces {
                            "g"
                        } else {
                            "p"
                        }
                    );
                    assert_eq!(screen.backend().buffer()[(0, 16)].symbol(), "└");
                    assert_ne!(screen.backend().buffer()[(0, 2)].symbol(), "#");
                }
            }
        }

        let source = "onboarding = false\n[ui.sidebar.agents]\nrow_gap = 0\nrows = [[\"agent\"], [\"$more\"]]\n";
        assert!(source.parse::<toml::Value>().is_ok());
        let config: crate::config::Config = toml::from_str(source).unwrap();
        let mut app = app_for_mouse_test();
        app.apply_live_config(&config, &[], &[], false);
        let mut workspace = Workspace::test_new("pair");
        workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        for terminal in app.state.terminals.values_mut() {
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = AgentState::Working;
        }
        app.state.active = Some(0);
        app.state.sidebar_min_width = 1;
        app.state.sidebar_width = 3;
        let panes = app.state.workspaces[0].tabs[0].layout.pane_ids();
        assert_eq!(panes.len(), 2);
        let second_id = app.state.workspaces[0].tabs[0].panes[&panes[1]]
            .attached_terminal_id
            .clone();
        for (step, more) in [false, true, false].into_iter().enumerate() {
            let terminal = app.state.terminals.get_mut(&second_id).unwrap();
            if more {
                assert!(terminal.metadata_tokens.patch(
                    std::collections::HashMap::from([("more".into(), Some("SECOND-LINE".into()))]),
                    None,
                    std::time::Instant::now()
                ));
                assert_eq!(terminal.metadata_tokens.values()["more"], "SECOND-LINE");
            } else if step == 2 {
                assert_eq!(terminal.metadata_tokens.values()["more"], "SECOND-LINE");
                assert!(terminal.metadata_tokens.patch(
                    std::collections::HashMap::from([("more".into(), None)]),
                    None,
                    std::time::Instant::now()
                ));
                assert!(terminal.metadata_tokens.values().is_empty());
                assert_eq!(app.state.agent_panel_scroll, 1);
            } else {
                assert!(terminal.metadata_tokens.values().is_empty());
            }
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 12));
            let area = app.state.agent_panel_rect();
            assert_eq!(area, Rect::new(0, 6, 2, 6));
            let track = crate::ui::agent_panel_scrollbar_rect(&app.state, area);
            assert_eq!(track.is_some(), more);
            assert_eq!(
                area.width - u16::from(track.is_some()),
                if more { 1 } else { 2 }
            );
            assert_eq!(
                crate::ui::agent_panel_scroll_metrics(&app.state, area).max_offset_from_bottom,
                usize::from(more)
            );
            assert_eq!(app.state.agent_panel_scroll, 0);
            assert_eq!(
                children(&app.state, area),
                if more {
                    vec![(0, 10)]
                } else {
                    vec![(0, 10), (1, 11)]
                }
            );
            assert_eq!(app.state.agent_detail_target_at(10), Some((0, 0, panes[0])));
            if more {
                assert!(app.state.focus_agent_entry(1));
                assert_eq!(app.state.agent_panel_scroll, 1);
                assert_eq!(children(&app.state, area), vec![(1, 10)]);
                assert_eq!(app.state.agent_detail_target_at(10), Some((0, 0, panes[1])));
                assert_eq!(app.state.agent_detail_target_at(11), Some((0, 0, panes[1])));
            } else {
                assert_eq!(app.state.agent_detail_target_at(11), Some((0, 0, panes[1])));
            }
        }

        app.state.sidebar_width = 2;
        assert!(app.state.focus_pane_in_workspace(0, panes[0]));
        assert!(app.state.focus_pane_in_workspace(0, panes[1]));
        app.state.agent_panel_scroll = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 10));
        let full = app.state.view.sidebar_rect;
        let area = app.state.agent_panel_rect();
        assert_eq!(full, Rect::new(0, 0, 2, 10));
        assert_eq!(area, Rect::new(0, 5, 1, 5));
        assert_eq!(children(&app.state, area), vec![(0, 9)]);
        let collapse = crate::ui::expanded_sidebar_toggle_rect(full);
        assert_eq!(collapse, Rect::new(1, 9, 1, 1));
        assert!(!app.state.on_sidebar_toggle(0, 9));
        assert!(app.state.on_sidebar_toggle(1, 9));
        assert!(!app.state.on_sidebar_divider(1, 9));
        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(106, 10)).unwrap();
        screen
            .draw(|frame| crate::ui::render(&app.state, frame))
            .unwrap();
        assert_eq!(screen.backend().buffer()[(0, 9)].symbol(), "├");
        assert_eq!(screen.backend().buffer()[(1, 9)].symbol(), "«");
        assert_eq!(app.state.agent_detail_target_at(9), Some((0, 0, panes[0])));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 9));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 0, 9));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(panes[0]));
        assert!(!app.state.sidebar_collapsed);
        assert_eq!(app.state.sidebar_width, 2);
        assert!(app.state.focus_pane_in_workspace(0, panes[1]));
        app.state.agent_panel_scroll = 0;
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 9));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 1, 9));
        assert!(app.state.sidebar_collapsed);
        assert_eq!(app.state.sidebar_width, 2);
        assert!(app.state.drag.is_none());
        for width in [0, 1, 3, 26] {
            assert_eq!(
                crate::ui::expanded_sidebar_toggle_rect(Rect::new(0, 0, width, 10)),
                if width <= 1 {
                    Rect::default()
                } else {
                    Rect::new(width - 2, 9, 1, 1)
                }
            );
        }
    }

    #[test]
    fn m828d2_gap_one_drop_premises_and_variable_rows_are_distinct() {
        {
            let mut app = app_for_mouse_test();
            app.state.sidebar_spaces.row_gap = 1;
            app.state.workspaces = vec![
                Workspace::test_new("a"),
                Workspace::test_new("b"),
                Workspace::test_new("c"),
            ];
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
            let cards = &app.state.view.workspace_card_areas;
            assert_eq!(
                cards
                    .iter()
                    .map(|card| (card.ws_idx, card.rect))
                    .collect::<Vec<_>>(),
                vec![(0, Rect::new(0, 2, 24, 2)), (1, Rect::new(0, 5, 24, 2))]
            );
            assert_eq!(app.state.sidebar_footer_rect(), Rect::new(0, 9, 25, 1));
            let slot = crate::ui::workspace_drop_indicator_row(
                cards,
                app.state.workspace_list_rect(),
                cards.len(),
            )
            .unwrap();
            assert_eq!(slot, 7);
            assert_eq!(slot, cards.last().unwrap().rect.bottom());
            assert!(slot < app.state.sidebar_footer_rect().y - 1);
        }
        {
            let mut app = app_for_mouse_test();
            app.state.sidebar_spaces.row_gap = 1;
            let repos = [temp_git_repo("main"), temp_git_repo("main")];
            app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
            for (workspace, repo) in app.state.workspaces.iter_mut().zip(&repos) {
                workspace.identity_cwd = repo.clone();
                workspace.refresh_git_ahead_behind();
            }
            app.state.ensure_test_terminals();
            for (workspace, repo) in app.state.workspaces.iter().zip(&repos) {
                let tab = &workspace.tabs[0];
                let id = &tab.panes[&tab.root_pane].attached_terminal_id;
                app.state.terminals.get_mut(id).unwrap().cwd = repo.clone();
            }
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
            assert_eq!(
                app.state
                    .view
                    .workspace_card_areas
                    .iter()
                    .map(|card| (card.ws_idx, card.rect))
                    .collect::<Vec<_>>(),
                vec![(0, Rect::new(0, 2, 25, 2)), (1, Rect::new(0, 5, 25, 2))]
            );
            for workspace in &app.state.workspaces {
                assert_eq!(workspace.branch().as_deref(), Some("main"));
            }
            for row in [0, 1, 2] {
                assert_eq!(app.state.workspace_drop_index_at_row(row), Some(0));
            }
            assert_eq!(app.state.workspace_drop_index_at_row(3), Some(1));
            for repo in repos {
                fs::remove_dir_all(repo).unwrap();
            }
        }
        {
            let mut app = app_for_mouse_test();
            app.state.sidebar_spaces.row_gap = 1;
            app.state.workspaces = vec![
                Workspace::test_new("a"),
                Workspace::test_new("b"),
                Workspace::test_new("c"),
            ];
            let active_id = app.state.workspaces[1].id.clone();
            let selected_id = app.state.workspaces[2].id.clone();
            app.state.active = Some(1);
            app.state.selected = 2;
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
            assert_eq!(
                app.state
                    .view
                    .workspace_card_areas
                    .iter()
                    .map(|card| (card.ws_idx, card.rect.y, card.rect.height))
                    .collect::<Vec<_>>(),
                vec![(0, 2, 2), (1, 5, 2)]
            );
            assert_eq!(
                crate::ui::workspace_drop_indicator_row(
                    &app.state.view.workspace_card_areas,
                    app.state.workspace_list_rect(),
                    0
                ),
                Some(1)
            );
            app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 5));
            app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 2, 1));
            assert!(matches!(
                app.state.drag.as_ref().map(|drag| &drag.target),
                Some(DragTarget::WorkspaceReorder {
                    source_ws_idx: 1,
                    insert_idx: Some(0),
                    ..
                })
            ));
            app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, 1));
            assert_eq!(
                app.state
                    .workspaces
                    .iter()
                    .map(|ws| ws.display_name())
                    .collect::<Vec<_>>(),
                vec!["b", "a", "c"]
            );
            assert_eq!((app.state.active, app.state.selected), (Some(0), 2));
            assert_eq!(app.state.workspaces[0].id, active_id);
            assert_eq!(app.state.workspaces[2].id, selected_id);
            assert_eq!(
                capture_snapshot(&app.state)
                    .workspaces
                    .iter()
                    .map(|ws| ws.custom_name.as_deref().unwrap())
                    .collect::<Vec<_>>(),
                vec!["b", "a", "c"]
            );
        }
        {
            let mut app = app_for_mouse_test();
            let source = "[ui.sidebar.spaces]\nrow_gap = 1\nrows = [[\"workspace\"], [\"$one\"], [\"$two\"]]\n";
            assert!(source.parse::<toml::Value>().is_ok());
            let config: crate::config::Config = toml::from_str(source).unwrap();
            app.apply_live_config(&config, &[], &[], false);
            app.state.workspaces = vec![
                Workspace::test_new("a"),
                Workspace::test_new("b"),
                Workspace::test_new("c"),
            ];
            for (index, workspace) in app.state.workspaces.iter_mut().enumerate() {
                workspace.cached_git_branch = None;
                let mut patch = std::collections::HashMap::new();
                if index != 1 {
                    patch.insert("one".into(), Some(format!("ONE-{index}")));
                }
                if index == 0 {
                    patch.insert("two".into(), Some("TWO-0".into()));
                }
                if !patch.is_empty() {
                    assert!(workspace.metadata_tokens.patch(
                        patch,
                        None,
                        std::time::Instant::now()
                    ));
                }
                assert_eq!(workspace.metadata_tokens.values().len(), [2, 0, 1][index]);
                if index != 1 {
                    assert_eq!(
                        workspace.metadata_tokens.values()["one"],
                        format!("ONE-{index}")
                    );
                }
            }
            app.state.active = Some(0);
            let first_id = app.state.workspaces[0].id.clone();
            let last_id = app.state.workspaces[2].id.clone();
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 30));
            let cards = &app.state.view.workspace_card_areas;
            assert_eq!(
                cards
                    .iter()
                    .map(|card| (card.ws_idx, card.rect))
                    .collect::<Vec<_>>(),
                vec![
                    (0, Rect::new(0, 2, 25, 3)),
                    (1, Rect::new(0, 6, 25, 1)),
                    (2, Rect::new(0, 8, 25, 2))
                ]
            );
            for (index, row) in [1, 5, 7, 10].into_iter().enumerate() {
                assert_eq!(
                    crate::ui::workspace_drop_indicator_row(
                        cards,
                        app.state.workspace_list_rect(),
                        index
                    ),
                    Some(row)
                );
                assert_eq!(app.state.workspace_drop_index_at_row(row), Some(index));
            }
            for (index, range) in [(0, 2..5), (1, 6..7), (2, 8..10)] {
                for row in range {
                    assert_eq!(app.state.workspace_at_row(row), Some(index));
                }
            }
            assert_eq!(app.state.workspace_at_row(5), None);
            assert_eq!(app.state.workspace_at_row(7), None);
            app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 8));
            app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 2, 1));
            assert!(matches!(
                app.state.drag.as_ref().map(|drag| &drag.target),
                Some(DragTarget::WorkspaceReorder {
                    source_ws_idx: 2,
                    insert_idx: Some(0),
                    ..
                })
            ));
            app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, 1));
            assert_eq!(
                app.state
                    .workspaces
                    .iter()
                    .map(|ws| ws.display_name())
                    .collect::<Vec<_>>(),
                vec!["c", "a", "b"]
            );
            assert_eq!(app.state.workspaces[0].id, last_id);
            assert_eq!(app.state.workspaces[1].id, first_id);
        }
        {
            let mut app = app_for_mouse_test();
            let source = "[ui.sidebar.spaces]\nrow_gap = 0\nrows = [[\"workspace\"], [\"$one\"]]\n";
            assert!(source.parse::<toml::Value>().is_ok());
            let config: crate::config::Config = toml::from_str(source).unwrap();
            app.apply_live_config(&config, &[], &[], false);
            let mut workspace = Workspace::test_new("solo");
            workspace.cached_git_branch = None;
            assert!(workspace.metadata_tokens.patch(
                std::collections::HashMap::from([("one".into(), Some("PACKED-END".into()))]),
                None,
                std::time::Instant::now()
            ));
            assert_eq!(workspace.metadata_tokens.values()["one"], "PACKED-END");
            app.state.workspaces = vec![workspace];
            app.state.active = Some(0);
            app.state.mouse_capture = true;
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 12));
            assert_eq!(
                app.state.view.workspace_card_areas[0].rect,
                Rect::new(0, 2, 25, 2)
            );
            assert_eq!(app.state.sidebar_footer_rect().y, 5);
            assert_eq!(
                crate::ui::workspace_drop_indicator_row(
                    &app.state.view.workspace_card_areas,
                    app.state.workspace_list_rect(),
                    1
                ),
                Some(4)
            );
            let mut screen =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(106, 12)).unwrap();
            screen
                .draw(|frame| crate::ui::render(&app.state, frame))
                .unwrap();
            assert!((0..25)
                .map(|x| screen.backend().buffer()[(x, 3)].symbol())
                .collect::<String>()
                .contains("PACKED-END"));
            app.state.drag = Some(crate::app::state::DragState {
                target: DragTarget::WorkspaceReorder {
                    source_id: 0,
                    source_ws_idx: 0,
                    insert_idx: Some(1),
                },
            });
            screen
                .draw(|frame| crate::ui::render(&app.state, frame))
                .unwrap();
            assert_eq!(screen.backend().buffer()[(0, 4)].symbol(), "─");
            assert_eq!(
                screen.backend().buffer()[(0, 4)].style().fg,
                Some(app.state.palette.accent)
            );
        }
    }

    #[test]
    fn m828d1_headers_and_configured_gaps_are_not_click_targets() {
        let mut app = app_for_mouse_test();
        let source = "[ui.sidebar.agents]\nrow_gap = 2\n";
        assert!(source.parse::<toml::Value>().is_ok());
        let config: crate::config::Config = toml::from_str(source).unwrap();
        app.apply_live_config(&config, &[], &[], false);
        let mut first = Workspace::test_new("alpha");
        first.test_split(ratatui::layout::Direction::Horizontal);
        first.test_add_tab(Some("logs"));
        app.state.workspaces = vec![first, Workspace::test_new("beta")];
        app.state.ensure_test_terminals();
        for terminal in app.state.terminals.values_mut() {
            terminal.detected_agent = Some(Agent::Claude);
        }
        app.state.agent_panel_sort = AgentPanelSort::Spaces;
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 70));
        let area = app.state.agent_panel_rect();
        let entries = crate::ui::agent_panel_entries(&app.state);
        assert_eq!(entries.len(), 4);
        let targets: Vec<_> = entries
            .iter()
            .map(|entry| (entry.ws_idx, entry.tab_idx, entry.pane_id))
            .collect();
        assert_eq!((targets[0].0, targets[0].1), (0, 0));
        assert_eq!((targets[1].0, targets[1].1), (0, 0));
        assert_eq!((targets[2].0, targets[2].1), (0, 1));
        assert_eq!((targets[3].0, targets[3].1), (1, 0));
        let rows = crate::ui::agent_visible_rows(&app.state, area);
        let children: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                crate::ui::AgentVisibleRow::Child { entry_idx, y, .. } => Some((*entry_idx, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(children.len(), 4);
        let first_y = children[0].1;
        assert_eq!(app.state.agent_detail_target_at(first_y), Some(targets[0]));
        assert_eq!(
            children[1].1,
            first_y + 3,
            "configured gap precedes second child"
        );
        assert_eq!(children[2].1, first_y + 7);
        assert_eq!(children[3].1, first_y + 11);
        for y in first_y - 1..=children[3].1 {
            if children.iter().any(|(_, child_y)| *child_y == y) {
                continue;
            }
            assert_eq!(app.state.agent_detail_target_at(first_y), Some(targets[0]));
            assert_eq!(
                app.state.agent_detail_target_at(y),
                None,
                "header/gap at {y}"
            );
            let active = app.state.active;
            let focused = app.state.workspaces[active.unwrap()].focused_pane_id();
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 2,
                y,
            ));
            app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), area.x + 2, y));
            assert_eq!(app.state.active, active);
            assert_eq!(
                app.state.workspaces[active.unwrap()].focused_pane_id(),
                focused
            );
        }
        for (entry_idx, y) in children {
            let (ws_idx, tab_idx, pane_id) = targets[entry_idx];
            assert_eq!(
                app.state.agent_detail_target_at(y),
                Some(targets[entry_idx])
            );
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 2,
                y,
            ));
            app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), area.x + 2, y));
            assert_eq!(app.state.active, Some(ws_idx));
            assert_eq!(app.state.workspaces[ws_idx].active_tab, tab_idx);
            assert_eq!(
                app.state.workspaces[ws_idx].tabs[tab_idx].layout.focused(),
                pane_id
            );
        }
    }

    #[test]
    fn m828d1_packed_drag_boundaries_keep_group_identity() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.state.workspaces {
            workspace.cached_git_branch = None;
        }
        app.state.ensure_test_terminals();
        let active_id = app.state.workspaces[1].id.clone();
        let selected_id = app.state.workspaces[3].id.clone();
        let group_ids = [
            app.state.workspaces[0].id.clone(),
            app.state.workspaces[2].id.clone(),
        ];
        app.state.active = Some(1);
        app.state.selected = 3;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));
        let cards = app.state.view.workspace_card_areas.clone();
        assert_eq!(
            cards.iter().map(|card| card.ws_idx).collect::<Vec<_>>(),
            vec![0, 2, 1, 3]
        );
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height);
        assert_eq!(
            cards[2].rect.y,
            cards[1].rect.y + cards[1].rect.height,
            "standalone follows packed group"
        );
        assert_eq!(cards[3].rect.y, cards[2].rect.y + cards[2].rect.height);
        let area = app.state.workspace_list_rect();
        let target_y = crate::ui::workspace_drop_indicator_row(&cards, area, 0).unwrap();
        assert_eq!(app.state.workspace_drop_index_at_row(target_y), Some(0));
        for y in cards[0].rect.y..cards[3].rect.bottom() {
            assert!(app.state.workspace_drop_index_at_row(y).is_some());
            assert_ne!(
                app.state.workspace_drop_index_at_row(y),
                Some(2),
                "no interior-group insertion at {y}"
            );
        }
        let source_y = cards[2].rect.y;
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, source_y));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 2, target_y));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 1,
                insert_idx: Some(0),
                ..
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_y));
        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|ws| ws.display_name())
                .collect::<Vec<_>>(),
            vec!["normal", "main", "issue", "notes"]
        );
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.selected, 3);
        assert_eq!(app.state.workspaces[0].id, active_id);
        assert_eq!(app.state.workspaces[3].id, selected_id);
        assert_eq!(
            [
                app.state.workspaces[1].id.clone(),
                app.state.workspaces[2].id.clone()
            ],
            group_ids
        );
        for idx in [1, 2] {
            assert_eq!(
                app.state.workspaces[idx]
                    .worktree_space
                    .as_ref()
                    .unwrap()
                    .key,
                "repo-key"
            );
        }
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(
            snapshot
                .workspaces
                .iter()
                .map(|ws| ws.custom_name.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["normal", "main", "issue", "notes"]
        );
    }

    #[test]
    fn m828d1_gap_wheel_and_scrollbar_share_the_visible_model() {
        for gap in [0, 2, u16::MAX] {
            let mut app = app_for_mouse_test();
            app.state.workspaces = (0..12)
                .map(|index| Workspace::test_new(&format!("agent-{index}")))
                .collect();
            app.state.ensure_test_terminals();
            for terminal in app.state.terminals.values_mut() {
                terminal.detected_agent = Some(Agent::Claude);
            }
            app.state.agent_panel_sort = AgentPanelSort::Spaces;
            app.state.sidebar_agents.row_gap = gap;
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.mode = Mode::Terminal;
            let area = app.state.agent_panel_rect();
            let entries = crate::ui::agent_panel_entries(&app.state);
            assert_eq!(entries.len(), 12);
            let first = entries[0].pane_id;
            let last = entries[11].pane_id;
            let metrics = crate::ui::agent_panel_scroll_metrics(&app.state, area);
            assert!(metrics.max_offset_from_bottom > 0);
            let track = crate::ui::agent_panel_scrollbar_rect(&app.state, area).unwrap();
            assert!(track.height >= 2);
            assert_eq!(app.state.agent_panel_scroll, 0);
            assert!(crate::ui::agent_visible_rows(&app.state, area)
                .iter()
                .any(|row| matches!(row, crate::ui::AgentVisibleRow::Child { entry_idx: 0, .. })));

            app.handle_mouse(mouse(MouseEventKind::ScrollDown, area.x + 1, track.y));
            assert_eq!(app.state.agent_panel_scroll, 1);
            let wheel_rows = crate::ui::agent_visible_rows(&app.state, area);
            assert!(wheel_rows
                .iter()
                .any(|row| matches!(row, crate::ui::AgentVisibleRow::Child { entry_idx: 1, .. })));
            app.state
                .set_agent_panel_offset_from_bottom(metrics.max_offset_from_bottom - 1);
            assert_eq!(crate::ui::agent_visible_rows(&app.state, area), wheel_rows);
            app.handle_mouse(mouse(MouseEventKind::ScrollUp, area.x + 1, track.y));
            assert_eq!(app.state.agent_panel_scroll, 0);

            for offset in [metrics.max_offset_from_bottom, 0] {
                app.state.set_agent_panel_offset_from_bottom(offset);
                let rows = crate::ui::agent_visible_rows(&app.state, area);
                let target = if offset == 0 { last } else { first };
                assert!(rows.iter().any(|row| matches!(row,
                    crate::ui::AgentVisibleRow::Child { entry_idx, .. }
                        if entries[*entry_idx].pane_id == target)));
                let mut positives = 0;
                let mut negatives = 0;
                for y in track.y..track.y + track.height {
                    let expected = rows.iter().find_map(|row| match row {
                        crate::ui::AgentVisibleRow::Child {
                            entry_idx,
                            y: child_y,
                            ..
                        } if *child_y == y => {
                            let entry = &entries[*entry_idx];
                            Some((entry.ws_idx, entry.tab_idx, entry.pane_id))
                        }
                        _ => None,
                    });
                    assert_eq!(
                        app.state.agent_detail_target_at(y),
                        expected,
                        "gap={gap} offset={offset} row={y}"
                    );
                    if expected.is_some() {
                        positives += 1;
                    } else {
                        negatives += 1;
                    }
                }
                assert!(
                    positives > 0 && negatives > 0,
                    "populated child and header/gap mirrors per page"
                );
            }

            app.state
                .set_agent_panel_offset_from_bottom(metrics.max_offset_from_bottom);
            assert!(matches!(
                app.state.agent_panel_scrollbar_target_at(track.x, track.y),
                Some(super::ScrollbarClickTarget::Thumb { grab_row_offset: 0 })
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                track.x,
                track.y,
            ));
            assert!(matches!(
                app.state.drag.as_ref().map(|drag| &drag.target),
                Some(DragTarget::AgentPanelScrollbar { grab_row_offset: 0 })
            ));
            let bottom = track.y + track.height - 1;
            app.handle_mouse(mouse(
                MouseEventKind::Drag(MouseButton::Left),
                track.x,
                bottom,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                track.x,
                bottom,
            ));
            assert!(app.state.drag.is_none());
            assert_eq!(app.state.agent_panel_scroll, metrics.max_offset_from_bottom);
            let rows = crate::ui::agent_visible_rows(&app.state, area);
            let last_y = rows
                .iter()
                .find_map(|row| match row {
                    crate::ui::AgentVisibleRow::Child {
                        entry_idx: 11, y, ..
                    } => Some(*y),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                app.state.agent_detail_target_at(last_y),
                Some((11, 0, last))
            );
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 2,
                last_y,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                area.x + 2,
                last_y,
            ));
            assert_eq!(app.state.active, Some(11));
            assert_eq!(app.state.workspaces[11].focused_pane_id(), Some(last));
            app.state.assert_invariants_for_test();
        }
    }

    /// The terminal row of `pane`'s `Child` row in the grouped agents panel, resolved through the
    /// same shared visible-row model the renderer + hit-test use.
    fn agent_child_row_y(state: &crate::app::state::AppState, pane: crate::layout::PaneId) -> u16 {
        let detail_area = state.agent_panel_rect();
        let entries = crate::ui::agent_panel_entries(state);
        let idx = entries
            .iter()
            .position(|e| e.pane_id == pane)
            .expect("pane has an agent entry");
        crate::ui::agent_visible_rows(state, detail_area)
            .into_iter()
            .find_map(|r| match r {
                crate::ui::AgentVisibleRow::Child { entry_idx, y, .. } if entry_idx == idx => {
                    Some(y)
                }
                _ => None,
            })
            .expect("pane's child row is visible")
    }

    #[test]
    fn clicking_launcher_opens_global_menu() {
        let mut app = app_for_mouse_test();
        let rect = app.state.global_launcher_rect();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + rect.width.saturating_sub(1),
            rect.y,
        ));

        assert_eq!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn hovering_global_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.global_menu.highlighted, 1);
    }

    #[test]
    fn clicking_keybinds_menu_item_opens_help() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn clicking_settings_menu_item_opens_settings() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::Settings);
    }

    #[test]
    fn clicking_reload_config_menu_item_requests_reload() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 3,
        ));

        assert!(app.state.request_reload_config);
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn update_pending_menu_surfaces_update_ready_entry() {
        let mut app = app_for_mouse_test();
        app.state.update_available = Some("0.3.2".into());
        app.state.latest_release_notes_available = true;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "update ready",
                "detach"
            ]
        );
        assert!(!app.state.should_quit);
    }

    #[test]
    fn persistence_mode_menu_surfaces_detach_action() {
        let mut app = app_for_mouse_test();
        app.state.detach_exits = false;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec!["settings", "keybinds", "reload config", "detach"]
        );

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 4,
        ));

        assert!(app.state.detach_requested);
        assert!(!app.state.should_quit);
        assert_ne!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn whats_new_remains_in_menu_for_latest_installed_release_notes() {
        let mut app = app_for_mouse_test();
        app.state.latest_release_notes_available = true;

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "what's new",
                "detach"
            ]
        );
    }

    #[test]
    fn clicking_agent_detail_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("main".into());
        let first_pane = ws.tabs[0].root_pane;
        let first_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[first_tab].root_pane;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[first_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        // Click the grouped child row for `second_pane` (the "logs" tab), found via the shared model.
        let target_y = agent_child_row_y(&app.state, second_pane);
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, target_y));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.workspaces[0].active_tab, first_tab);
        assert_eq!(
            snapshot.workspaces[0].tabs[first_tab].focused,
            Some(second_pane.raw())
        );
    }

    #[test]
    fn clicking_agent_panel_toggle_switches_sort() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.agent_panel_scroll = 3;

        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            app.state.view.sidebar_rect,
            app.state.sidebar_section_split,
        );
        let toggle = crate::ui::agent_panel_toggle_rect(detail_area, app.state.agent_panel_sort);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert_eq!(app.state.agent_panel_sort, AgentPanelSort::Priority);
        assert_eq!(app.state.agent_panel_scroll, 0);
    }

    #[test]
    fn clicking_all_workspaces_agent_row_switches_to_correct_workspace() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;

        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        // Click `second_pane`'s grouped child row, resolved through the shared model.
        let target_y = agent_child_row_y(&app.state, second_pane);
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, target_y));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces[1].active_tab, 0);
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            second_pane
        );
    }

    #[test]
    fn scrolling_agent_panel_with_wheel_updates_agent_panel_scroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;

        let mut tabs = Vec::new();
        for (tab_name, agent) in [
            ("logs", Agent::Claude),
            ("review", Agent::Codex),
            ("ops", Agent::Gemini),
        ] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        for (tab_idx, pane_id, agent) in tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let detail_area = app.state.agent_panel_rect();
        assert!(crate::ui::should_show_scrollbar(
            crate::ui::agent_panel_scroll_metrics(&app.state, detail_area)
        ));

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            detail_area.x + 1,
            detail_area.y + 4,
        ));

        assert_eq!(app.state.agent_panel_scroll, 1);
        assert_eq!(app.state.selected, 0);
    }

    #[test]
    fn clicking_scrolled_agent_detail_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        let mut extra_tabs = Vec::new();
        for (tab_name, agent) in [("review", Agent::Codex), ("ops", Agent::Gemini)] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            extra_tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        for (tab_idx, pane_id, agent) in extra_tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.agent_panel_scroll = 1;

        // Click `second_pane`'s child row (resolved through the model under the active scroll), not a
        // hard-coded row — with grouping the scrolled top row is the tab header, not an agent.
        let target_y = agent_child_row_y(&app.state, second_pane);
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, target_y));

        assert_eq!(app.state.workspaces[0].active_tab, second_tab);
        assert_eq!(
            app.state.workspaces[0].tabs[second_tab].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn agent_detail_target_at_matches_visible_row_model() {
        // Render + hit-test consume the same model: every Child row maps to its pane; every header
        // (and the inter-group spacers) maps to no pane.
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let p1 = ws.tabs[0].root_pane;
        let t2 = ws.test_add_tab(Some("logs"));
        let p2 = ws.tabs[t2].root_pane;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        for (pane, tab) in [(p1, 0usize), (p2, t2)] {
            let tid = app.state.workspaces[0].tabs[tab].panes[&pane]
                .attached_terminal_id
                .clone();
            app.state.terminals.get_mut(&tid).unwrap().detected_agent = Some(Agent::Claude);
        }
        app.state.active = Some(0);
        app.state.mode = Mode::Terminal;

        let detail_area = app.state.agent_panel_rect();
        let entries = crate::ui::agent_panel_entries(&app.state);
        let rows = crate::ui::agent_visible_rows(&app.state, detail_area);
        let mut child_count = 0;
        for r in &rows {
            match r {
                crate::ui::AgentVisibleRow::Child { entry_idx, y, .. } => {
                    child_count += 1;
                    let e = &entries[*entry_idx];
                    assert_eq!(
                        app.state.agent_detail_target_at(*y),
                        Some((e.ws_idx, e.tab_idx, e.pane_id)),
                        "child row {y} must map to its pane"
                    );
                }
                crate::ui::AgentVisibleRow::GroupHeader { y, .. } => {
                    assert_eq!(
                        app.state.agent_detail_target_at(*y),
                        None,
                        "header row {y} must map to no pane"
                    );
                }
            }
        }
        assert!(
            child_count >= 2,
            "expected >=2 visible children, got {child_count}"
        );
    }

    #[test]
    fn clicking_collapsed_agent_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let (_, _, detail_area) =
            crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            detail_area.x,
            detail_area.y + 1,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn clicking_collapsed_priority_agent_row_switches_to_matching_workspace() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.sidebar_collapsed = true;
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        for (ws_idx, pane_id, state) in [
            (0, first_pane, AgentState::Working),
            (1, second_pane, AgentState::Blocked),
        ] {
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        }

        let (_, _, detail_area) =
            crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            detail_area.x,
            detail_area.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            second_pane
        );
    }

    #[test]
    fn clicking_collapsed_sidebar_toggle_expands_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let toggle = crate::ui::collapsed_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(!app.state.sidebar_collapsed);
    }

    #[test]
    fn hidden_collapsed_sidebar_has_no_mouse_expand_hotspot() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
        app.state.view.sidebar_rect = Rect::new(0, 0, 0, 20);
        app.state.view.terminal_area = Rect::new(0, 0, 80, 20);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 19));

        assert!(app.state.sidebar_collapsed);
    }

    #[test]
    fn clicking_expanded_sidebar_toggle_collapses_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = false;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 20);

        let toggle = crate::ui::expanded_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(app.state.sidebar_collapsed);
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn clicking_workspace_switches_on_mouse_up() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let target_row = app.state.view.workspace_card_areas[1].rect.y;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            target_row,
        ));
        assert_eq!(app.state.active, Some(0));
        assert!(!app.state.workspace_presses.is_empty());

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert!(app.state.workspace_presses.is_empty());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.active, Some(1));
        assert_eq!(snapshot.selected, 1);
    }

    #[test]
    fn clicking_worktree_parent_row_focuses_workspace_without_toggling() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/zynk", "/repo/zynk-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "zynk".into(),
                    repo_root: "/repo/zynk".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0].rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn clicking_worktree_parent_chevron_toggles_group_only() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/zynk", "/repo/zynk-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "zynk".into(),
                    repo_root: "/repo/zynk".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0].rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x,
            parent.y,
        ));

        assert_eq!(app.state.active, None);
        assert!(app.state.workspace_presses.is_empty());
        assert!(app.state.collapsed_space_keys.contains("repo-key"));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x,
            parent.y,
        ));

        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn wheel_workspace_selection_follows_grouped_visual_order_without_scrollbar() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("main"),
            Workspace::test_new("normal"),
            Workspace::test_new("issue"),
        ];
        for (idx, checkout_path) in [(0, "/repo/zynk"), (2, "/repo/zynk-issue")] {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "zynk".into(),
                    repo_root: "/repo/zynk".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx != 0,
                });
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 30));
        let list = app.state.workspace_list_rect();
        assert!(!crate::ui::should_show_scrollbar(
            crate::ui::workspace_list_scroll_metrics(&app.state, list)
        ));

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, list.x + 1, list.y + 1));

        assert_eq!(app.state.selected, 2);
    }

    #[test]
    fn dragging_workspace_reorders_without_changing_identity() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        let active_id = app.state.workspaces[1].id.clone();
        let selected_id = app.state.workspaces[2].id.clone();
        app.state.active = Some(1);
        app.state.selected = 2;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let source_row = app.state.view.workspace_card_areas[1].rect.y;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            0,
        )
        .unwrap();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            source_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 1,
                insert_idx: Some(0),
                ..
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        let names: Vec<_> = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.selected, 2);
        assert_eq!(app.state.workspaces[0].id, active_id);
        assert_eq!(app.state.workspaces[2].id, selected_id);
        let snapshot = capture_snapshot(&app.state);
        let captured_names: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.custom_name.clone().unwrap())
            .collect();
        assert_eq!(captured_names, vec!["b", "a", "c"]);
    }

    #[test]
    fn clicking_tab_scroll_button_reveals_hidden_tabs_without_renaming() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));
        ws.test_add_tab(Some("review"));
        ws.test_add_tab(Some("ops"));
        ws.test_add_tab(Some("notes"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));

        let right = app.state.view.tab_scroll_right_hit_area;
        assert!(right.width > 0);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right.x + 1,
            right.y,
        ));

        assert_eq!(app.state.tab_scroll, 1);
        assert!(!app.state.tab_scroll_follow_active);
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(app.state.view.tab_hit_areas[0].width, 0);
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.state.workspaces[0].tabs[1].custom_name.as_deref(),
            Some("logs")
        );
    }

    #[test]
    fn clicking_last_visible_tab_at_right_edge_does_not_overscroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.tab_scroll = usize::MAX;
        app.state.tab_scroll_follow_active = false;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));

        let last_idx = app.state.workspaces[0].tabs.len() - 1;
        let target = app.state.view.tab_hit_areas[last_idx];
        let clamped_scroll = app.state.tab_scroll;
        assert!(target.width > 0, "last tab should already be visible");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, last_idx);
        assert_eq!(app.state.tab_scroll, clamped_scroll);
        assert!(app.state.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn dragging_tab_reorders_auto_and_custom_names_without_materializing_numbers() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("foo"));
        ws.test_add_tab(None);
        let moved_root = ws.tabs[0].root_pane;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let source = app.state.view.tab_hit_areas[0];
        let last = app.state.view.tab_hit_areas[2];
        let drop_col = last.x + last.width;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source.x + 1,
            source.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drop_col,
            source.y,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::TabReorder {
                ws_idx: 0,
                source_tab_idx: 0,
                insert_idx: Some(3),
                ..
            })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drop_col,
            source.y,
        ));

        let labels: Vec<_> = app.state.workspaces[0]
            .tabs
            .iter()
            .enumerate()
            .map(|(tab_idx, _)| app.state.workspaces[0].tab_display_name(tab_idx).unwrap())
            .collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("foo")
        );
        assert!(app.state.workspaces[0].tabs[1].custom_name.is_none());
        assert!(app.state.workspaces[0].tabs[2].custom_name.is_none());
        assert_eq!(app.state.workspaces[0].tabs[0].number, 2);
        assert_eq!(app.state.workspaces[0].tabs[1].number, 3);
        assert_eq!(app.state.workspaces[0].tabs[2].number, 1);
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let repo = unique_temp_path("sidebar-drop-slot-repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .unwrap();
        repo
    }

    fn workspace_with_space(name: &str, key: &str) -> Workspace {
        let mut ws = Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "zynk".into(),
            repo_root: "/repo/zynk".into(),
            checkout_path: format!("/repo/{name}").into(),
            is_linked_worktree: name != "main",
        });
        ws
    }

    #[test]
    fn top_drop_slot_is_distinct_from_gap_below_first_workspace() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_spaces.row_gap = 1;
        let first_repo = temp_git_repo("main");
        let second_repo = temp_git_repo("main");

        let mut first = Workspace::test_new("a");
        let first_root = first.tabs[0].root_pane;
        first.identity_cwd = first_repo.clone();
        first.refresh_git_ahead_behind();

        let mut second = Workspace::test_new("b");
        let second_root = second.tabs[0].root_pane;
        second.identity_cwd = second_repo.clone();
        second.refresh_git_ahead_behind();

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_root]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&first_terminal_id).unwrap().cwd = first_repo.clone();
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_root]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .cwd = second_repo.clone();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        assert_eq!(app.state.workspace_drop_index_at_row(0), Some(0));
        assert_eq!(app.state.workspace_drop_index_at_row(1), Some(0));
        assert_eq!(app.state.workspace_drop_index_at_row(2), Some(0));
        assert_eq!(app.state.workspace_drop_index_at_row(3), Some(1));

        let _ = fs::remove_dir_all(first_repo);
        let _ = fs::remove_dir_all(second_repo);
    }

    #[test]
    fn bottom_drop_slot_stays_below_last_workspace_not_footer() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_spaces.row_gap = 1;
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let cards = &app.state.view.workspace_card_areas;
        let bottom_slot = crate::ui::workspace_drop_indicator_row(
            cards,
            app.state.workspace_list_rect(),
            cards.len(),
        )
        .unwrap();

        let last = cards.last().unwrap().rect;
        assert_eq!(bottom_slot, last.y + last.height);
        assert!(bottom_slot < app.state.sidebar_footer_rect().y.saturating_sub(1));
    }

    #[test]
    fn grouped_sidebar_drop_slots_do_not_land_inside_compact_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(1);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let cards = &app.state.view.workspace_card_areas;
        let order = cards.iter().map(|card| card.ws_idx).collect::<Vec<_>>();
        assert_eq!(order, vec![0, 2, 1]);
        let issue = cards.iter().find(|card| card.ws_idx == 2).unwrap();
        let normal = cards.iter().find(|card| card.ws_idx == 1).unwrap();

        assert_eq!(app.state.workspace_drop_index_at_row(issue.rect.y), Some(1));
        assert_eq!(
            crate::ui::workspace_drop_indicator_row(cards, app.state.workspace_list_rect(), 2),
            Some(normal.rect.y + normal.rect.height)
        );
    }

    #[test]
    fn dragging_worktree_space_member_does_not_reorder_workspaces() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let source = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 2)
            .unwrap()
            .rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            0,
        )
        .unwrap();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, source.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(app.state.drag.is_none());
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        let names = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["main", "normal", "issue"]);
    }

    #[test]
    fn dragging_sidebar_divider_sets_manual_width() {
        let mut app = app_for_mouse_test();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30, 5));

        assert_eq!(app.state.sidebar_width, 31);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(31));
    }

    #[test]
    fn dragging_sidebar_bottom_divider_still_sets_manual_width() {
        let mut app = app_for_mouse_test();
        let divider_col = app.state.view.sidebar_rect.x + app.state.view.sidebar_rect.width - 1;
        let bottom_row = app.state.view.sidebar_rect.y + app.state.view.sidebar_rect.height - 1;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            divider_col,
            bottom_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            divider_col + 5,
            bottom_row,
        ));

        assert_eq!(app.state.sidebar_width, 31);
    }

    #[test]
    fn dragging_past_max_clamps_to_configured_max() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_max_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5));

        assert_eq!(app.state.sidebar_width, 30);
    }

    #[test]
    fn dragging_below_min_clamps_to_configured_min() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_min_width = 22;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 5));

        assert_eq!(app.state.sidebar_width, 22);
    }

    #[test]
    fn dragging_sidebar_section_divider_sets_split_ratio() {
        let mut app = app_for_mouse_test();
        let divider = crate::ui::sidebar_section_divider_rect(
            app.state.view.sidebar_rect,
            app.state.sidebar_section_split,
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            divider.x + 1,
            divider.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            divider.x + 1,
            divider.y + 4,
        ));

        assert!(app.state.sidebar_section_split > 0.5);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(
            snapshot.sidebar_section_split,
            Some(app.state.sidebar_section_split)
        );
    }

    #[test]
    fn double_clicking_sidebar_divider_resets_default_width() {
        let mut app = app_for_mouse_test();
        app.state.default_sidebar_width = 26;
        app.state.sidebar_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));

        assert_eq!(app.state.sidebar_width, 26);
        assert!(app.state.drag.is_none());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(26));
    }
}
