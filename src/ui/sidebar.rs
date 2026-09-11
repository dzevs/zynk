// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{state_label, state_label_color};
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::{AgentPanelSort, Palette};
use crate::app::{AppState, Mode};
use crate::config::StatusIndicatorStyle;
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    /// Tab name — the grouped agents panel uses this as the group header label. Always populated
    /// (unlike `primary_tab_label`, which the flat/mobile path sets for multiple or renamed tabs).
    pub tab_label: String,
    pub agent_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub custom_status: Option<String>,
    pub state_labels: std::collections::HashMap<String, String>,
}

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let (ws_h, detail_h) = sidebar_section_heights(content.height, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(content.height, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

fn agent_panel_sort_label(sort: AgentPanelSort) -> &'static str {
    match sort {
        AgentPanelSort::Spaces => "grouped",
        AgentPanelSort::Priority => "priority",
    }
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, sort: AgentPanelSort) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let label = agent_panel_sort_label(sort);
    let width = display_width_u16(label);
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    let mut entries: Vec<_> = app
        .workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| {
            let multi_tab = ws.tabs.len() > 1;
            let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(move |detail| {
                    let show_tab = multi_tab
                        || ws
                            .tabs
                            .get(detail.tab_idx)
                            .is_some_and(|tab| !tab.is_auto_named());
                    AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        tab_label: detail.tab_label.clone(),
                        primary_tab_label: show_tab.then_some(detail.tab_label),
                        agent_label: Some(detail.agent_label),
                        state: detail.state,
                        seen: detail.seen,
                        last_agent_state_change_seq: detail.last_agent_state_change_seq,
                        custom_status: detail.custom_status,
                        state_labels: detail.state_labels,
                    }
                })
        })
        .collect();

    if matches!(app.agent_panel_sort, AgentPanelSort::Priority) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(workspace_attention_priority(entry.state, entry.seen)),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }

    entries
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn workspace_row_height(ws: &crate::workspace::Workspace) -> u16 {
    if ws.branch().is_some() {
        2
    } else {
        1
    }
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Working, _) => 3,
        (AgentState::Idle, false) => 2, // done / unseen
        (AgentState::Idle, true) => 1,  // idle / seen
        (AgentState::Unknown, _) => 0,
    }
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    app.workspaces
        .iter()
        .filter(|ws| ws.worktree_space().is_some_and(|space| space.key == key))
        .map(|ws| ws.aggregate_state(&app.terminals))
        .max_by_key(|(state, seen)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    if space.is_linked_worktree {
        return None;
    }
    let member_count = app
        .workspaces
        .iter()
        .filter(|ws| {
            ws.worktree_space()
                .is_some_and(|member| member.key == space.key)
        })
        .count();
    (member_count >= 2).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

pub(crate) fn grouped_child_display_label(
    label: &str,
    branch: Option<&str>,
    has_custom_name: bool,
) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace { ws_idx: usize, indented: bool },
}

pub(crate) fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    let entry_count = workspace_list_entries(app).len();
    if entry_count == 0 {
        0
    } else {
        requested.min(entry_count.saturating_sub(1))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, false)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true)
}

fn workspace_list_entries_inner(app: &AppState, force_expanded: bool) -> Vec<WorkspaceListEntry> {
    let mut members_by_key = std::collections::HashMap::<String, Vec<usize>>::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        if let Some(space) = ws.worktree_space() {
            members_by_key
                .entry(space.key.clone())
                .or_default()
                .push(ws_idx);
        }
    }
    let grouped_keys = members_by_key
        .iter()
        .filter(|(_, members)| {
            members.len() >= 2
                && members.iter().any(|idx| {
                    app.workspaces
                        .get(*idx)
                        .and_then(|ws| ws.worktree_space())
                        .is_some_and(|space| !space.is_linked_worktree)
                })
        })
        .map(|(key, _)| key.clone())
        .collect::<std::collections::HashSet<_>>();

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx.and_then(|idx| {
        app.workspaces
            .get(idx)
            .and_then(|ws| ws.worktree_space())
            .map(|space| space.key.clone())
    });

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut entries = Vec::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let Some(space) = ws
            .worktree_space()
            .filter(|space| grouped_keys.contains(&space.key))
        else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };

        if !emitted_groups.insert(space.key.clone()) {
            continue;
        }

        let Some(members) = members_by_key.get(&space.key) else {
            continue;
        };
        let Some(parent_idx) = members.iter().copied().find(|idx| {
            app.workspaces
                .get(*idx)
                .and_then(|member| member.worktree_space())
                .is_some_and(|member_space| !member_space.is_linked_worktree)
        }) else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        let collapsed = !force_expanded && app.collapsed_space_keys.contains(&space.key);
        entries.push(WorkspaceListEntry::Workspace {
            ws_idx: parent_idx,
            indented: false,
        });

        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| *idx != parent_idx)
                .filter(|_| active_group.as_deref() == Some(space.key.as_str()))
            {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: active_idx,
                    indented: true,
                });
            }
        } else {
            for member_idx in members {
                if *member_idx == parent_idx {
                    continue;
                }
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: *member_idx,
                    indented: true,
                });
            }
        }
    }
    entries
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let needed = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    1
                } else {
                    workspace_row_height(ws)
                };
                let gap = u16::from(!next_entry_is_indented_workspace(&entries, entry_idx));
                row_height.saturating_add(gap)
            }
        };
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        visible += 1;
    }
    visible
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let entries = workspace_list_entries(app);
    let total_rows = entries.len();
    let scroll = app.workspace_scroll.min(total_rows.saturating_sub(1));
    let viewport_rows = workspace_list_visible_count(app, area, scroll);
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

/// Number of `Child` rows the grouped placement renders starting at `scroll` — the real,
/// header-aware capacity (NOT a flat per-entry count). Uses only the pure placement primitive.
fn agent_children_placed_from(entries: &[AgentPanelEntry], body: Rect, scroll: usize) -> usize {
    agent_visible_rows_for_entries(entries, body, scroll)
        .iter()
        .filter(|r| matches!(r, AgentVisibleRow::Child { .. }))
        .count()
}

/// The maximum `agent_panel_scroll` (entries skipped from the top) that still renders the LAST entry
/// as a child: the smallest `s` with `s + children_placed_from(s) >= total` (monotone). A linear scan
/// over raw scroll offsets that calls ONLY the pure primitive — no metrics, no clamping, no recursion.
fn max_agent_panel_scroll(entries: &[AgentPanelEntry], body: Rect) -> usize {
    let total = entries.len();
    if total == 0 {
        return 0;
    }
    for scroll in 0..total {
        if scroll + agent_children_placed_from(entries, body, scroll) >= total {
            return scroll;
        }
    }
    total.saturating_sub(1)
}

/// Scroll metrics for the grouped agents panel, header-aware via the shared placement primitive.
/// Capacity is measured on the NO-scrollbar body: the scrollbar trims width only, never row capacity,
/// so this is stable and there is no body↔metrics cycle. Clamps only a LOCAL `scroll`; MUST NOT call
/// the public `agent_visible_rows` (keeps the DAG acyclic).
pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let entries = agent_panel_entries(app);
    let total = entries.len();
    let body = agent_panel_body_rect(area, false);
    let max_offset_from_bottom = max_agent_panel_scroll(&entries, body);
    let viewport_rows = total.saturating_sub(max_offset_from_bottom);
    let scroll = app.agent_panel_scroll.min(max_offset_from_bottom);

    crate::pane::ScrollMetrics {
        offset_from_bottom: max_offset_from_bottom.saturating_sub(scroll),
        max_offset_from_bottom,
        viewport_rows,
    }
}

/// The grouped agents rows for the live panel. Clamps `app.agent_panel_scroll` to the metric maximum
/// and makes exactly ONE call to the pure placement primitive. Render + hit-test consume this; the
/// metrics never call it, so the DAG stays acyclic.
pub(crate) fn agent_visible_rows(app: &AppState, area: Rect) -> Vec<AgentVisibleRow> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    let entries = agent_panel_entries(app);
    let scroll = app.agent_panel_scroll.min(metrics.max_offset_from_bottom);
    agent_visible_rows_for_entries(&entries, body, scroll)
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (Vec<crate::app::state::WorkspaceCardArea>, Vec<()>) {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let headers = Vec::new();

    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    1
                } else {
                    workspace_row_height(ws)
                };
                let gap = u16::from(!next_entry_is_indented_workspace(&entries, entry_idx));
                if row_y.saturating_add(row_height).saturating_add(gap) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                });
                row_y = row_y.saturating_add(row_height + gap);
            }
        }
    }

    (cards, headers)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

fn workspace_selection_background(p: &Palette, is_active: bool) -> Color {
    if is_active && p.selection_bg == Color::Reset {
        p.active_row_bg
    } else {
        p.selection_bg
    }
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(p.sidebar_bg));
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        // Same workspace-scoped glyph as the expanded spaces list (Unknown -> ◌, not the global ·).
        let (icon, icon_style) =
            workspace_state_icon(agg_state, agg_seen, app.status_indicators, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let selection_bg = workspace_selection_background(p, is_active);
        let row_style = if is_selected {
            Style::default().bg(selection_bg)
        } else if is_active {
            Style::default().bg(p.active_row_bg)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(selection_bg)
        } else if is_active {
            Style::default().fg(p.text).bg(p.active_row_bg)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{:<2}", visible_idx + 1), num_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(p.surface_dim));
        }
    }

    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        for (detail_idx, detail) in agent_panel_entries(app).iter().enumerate() {
            let y = detail_content_area.y + detail_idx as u16;
            if y >= detail_content_area.y + detail_content_area.height {
                break;
            }
            let position = detail_idx + 1;
            let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
            let position_style = if is_active {
                Style::default().fg(p.text).bg(p.active_row_bg)
            } else {
                Style::default().fg(p.overlay0)
            };
            // Collapsed rail uses the expanded panel's sidebar-agent icon grammar.
            let (icon, icon_style) =
                sidebar_agent_icon(detail.state, detail.seen, app.status_indicators, p);
            if is_active {
                let buf = frame.buffer_mut();
                for x in detail_content_area.x..detail_content_area.x + detail_content_area.width {
                    buf[(x, y)].set_style(Style::default().bg(p.active_row_bg));
                }
            }
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{position:<2}"), position_style),
                    Span::styled(icon, icon_style),
                ])),
                Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
            );
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_indicator_row(
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    insert_idx: usize,
) -> Option<u16> {
    if area.height == 0 {
        return None;
    }
    let list_bottom = area.y + area.height.saturating_sub(1);

    let first = cards.first()?;
    if insert_idx == first.ws_idx {
        return first.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    if let Some(row) = cards
        .last()
        .filter(|card| insert_idx == card.ws_idx.saturating_add(1))
        .map(|card| card.rect.y.saturating_add(card.rect.height))
        .filter(|y| *y < list_bottom)
    {
        return Some(row);
    }

    if let Some(card) = cards.iter().find(|card| card.ws_idx == insert_idx) {
        return card.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    None
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(p.sidebar_bg));
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, detail_area) = expanded_sidebar_sections(area, app.sidebar_section_split);

    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    render_agent_detail(app, terminal_runtimes, frame, detail_area);
    render_sidebar_toggle(app, frame, area, false, p);
}

// Sidebar marks retain their surface-specific non-working glyphs.
fn sidebar_agent_icon(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    if indicator_style == StatusIndicatorStyle::Symbols {
        return super::status::state_icon(state, seen, indicator_style, p);
    }
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("○", Style::default().fg(p.teal)), // done / unseen
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)), // idle / seen
        (AgentState::Unknown, _) => ("◌", Style::default().fg(p.overlay0)),
    }
}

/// Group priority: blocked, working, done/unseen, idle, then unknown.
fn agents_group_aggregate(
    states: &[(AgentState, bool)],
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    if states.iter().any(|&(s, _)| s == AgentState::Blocked) {
        return sidebar_agent_icon(AgentState::Blocked, false, indicator_style, p);
    }
    if states.iter().any(|&(s, _)| s == AgentState::Working) {
        return sidebar_agent_icon(AgentState::Working, false, indicator_style, p);
    }
    if states
        .iter()
        .any(|&(s, seen)| s == AgentState::Idle && !seen)
    {
        return sidebar_agent_icon(AgentState::Idle, false, indicator_style, p);
    }
    if states
        .iter()
        .any(|&(s, seen)| s == AgentState::Idle && seen)
    {
        return sidebar_agent_icon(AgentState::Idle, true, indicator_style, p);
    }
    sidebar_agent_icon(AgentState::Unknown, false, indicator_style, p)
}

/// Spaces and agents use the same static state marks.
fn workspace_state_icon(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    sidebar_agent_icon(state, seen, indicator_style, p)
}

/// One placed row in the grouped agents panel — the single layout primitive that render, hit-test,
/// and scroll metrics all derive from. `GroupHeader.entry_idx` is the first child of the tab group
/// (its label + aggregate source); `Child.last` drives `└─` vs `├─`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentVisibleRow {
    GroupHeader {
        entry_idx: usize,
        y: u16,
    },
    Child {
        entry_idx: usize,
        y: u16,
        last: bool,
    },
}

/// Tab group key for the agents panel: agents are grouped by their `(workspace, tab)`.
fn agent_group_key(e: &AgentPanelEntry) -> (usize, usize) {
    (e.ws_idx, e.tab_idx)
}

/// PURE placement of the grouped agents rows: takes NO `AppState`, computes NO metrics, does NO
/// clamping. Given the entry slice, the body rect, and a scroll offset (entries skipped from the
/// top), it lays out height-1 rows from `body.y`: on each tab-group change it emits a 1-row spacer
/// (skipped at the very top), then a `GroupHeader`, then the `Child`; the same group emits just the
/// `Child`. It stops before any row would leave the body and never emits a dangling header whose
/// child would not fit. This is the one primitive `agent_children_placed_from` /
/// `max_agent_panel_scroll` / the public `agent_visible_rows` all build on (a strict, non-recursive DAG).
fn agent_visible_rows_for_entries(
    entries: &[AgentPanelEntry],
    body: Rect,
    scroll: usize,
) -> Vec<AgentVisibleRow> {
    let mut rows = Vec::new();
    if body.width == 0 || body.height == 0 {
        return rows;
    }
    let body_bottom = body.y + body.height;
    let mut row_y = body.y;
    let mut prev_group: Option<(usize, usize)> = None;
    let mut placed_any = false;
    for (idx, e) in entries.iter().enumerate().skip(scroll) {
        let group = agent_group_key(e);
        let group_changed = prev_group != Some(group);
        let last = match entries.get(idx + 1) {
            Some(next) => agent_group_key(next) != group,
            None => true,
        };
        let spacer = u16::from(group_changed && placed_any);
        let header = u16::from(group_changed);
        // The child row lands at row_y + spacer + header; bail (without a dangling header) if it
        // would overflow the body.
        let child_y = row_y.saturating_add(spacer).saturating_add(header);
        if child_y >= body_bottom {
            break;
        }
        if spacer == 1 {
            row_y = row_y.saturating_add(1);
        }
        if header == 1 {
            rows.push(AgentVisibleRow::GroupHeader {
                entry_idx: idx,
                y: row_y,
            });
            row_y = row_y.saturating_add(1);
        }
        rows.push(AgentVisibleRow::Child {
            entry_idx: idx,
            y: row_y,
            last,
        });
        row_y = row_y.saturating_add(1);
        prev_group = Some(group);
        placed_any = true;
    }
    rows
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            insert_idx: Some(insert_idx),
            ..
        }) => workspace_drop_indicator_row(&app.view.workspace_card_areas, area, *insert_idx),
        _ => None,
    };

    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                workspace_selection_background(p, is_active)
            } else if is_dragged {
                p.surface1
            } else {
                p.active_row_bg
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let (icon, icon_style) =
            workspace_state_icon(agg_state, agg_seen, app.status_indicators, p);
        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let mut line1 = Vec::new();
        let mut show_workspace_icon = true;
        if card.indented {
            line1.push(Span::styled("   ", Style::default()));
        } else if let Some((key, collapsed)) = workspace_parent_group_state(app, i) {
            let icon = if collapsed { "▸" } else { "▾" };
            let (state_icon, state_style) = if collapsed {
                let (state, seen) = space_aggregate_state(app, &key);
                workspace_state_icon(state, seen, app.status_indicators, p)
            } else {
                (icon, Style::default().fg(p.accent))
            };
            line1.push(Span::styled(icon, Style::default().fg(p.accent)));
            if collapsed {
                line1.push(Span::styled(" ", Style::default()));
                line1.push(Span::styled(state_icon, state_style));
                show_workspace_icon = false;
            }
            line1.push(Span::styled(" ", Style::default()));
        } else {
            line1.push(Span::styled(" ", Style::default()));
        }
        if show_workspace_icon {
            line1.push(Span::styled(icon, icon_style));
            line1.push(Span::styled(" ", Style::default()));
        }
        if card.indented {
            let display_label = grouped_child_display_label(
                &label,
                ws.branch().as_deref(),
                ws.custom_name.is_some(),
            );
            line1.push(Span::styled(display_label, name_style));
        } else {
            line1.push(Span::styled(label, name_style));
        }

        frame.render_widget(
            Paragraph::new(Line::from(line1)),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );

        if row_height > 1 && row_y + 1 < list_bottom {
            if let Some(branch) = ws.branch() {
                let upstream_label = ws.git_ahead_behind().and_then(|(ahead, behind)| {
                    let mut parts = Vec::new();
                    if ahead > 0 {
                        parts.push((format!("↑{}", ahead), p.green));
                    }
                    if behind > 0 {
                        parts.push((format!("↓{}", behind), p.red));
                    }
                    (!parts.is_empty()).then_some(parts)
                });
                let reserved = upstream_label
                    .as_ref()
                    .map(|parts| {
                        parts.iter().map(|(label, _)| label.len()).sum::<usize>() + parts.len()
                    })
                    .unwrap_or(0);
                let max_branch_len = (card.rect.width as usize).saturating_sub(5 + reserved);
                // Truncate by DISPLAY WIDTH (`truncate_end`), not byte-slicing — a branch name can
                // be non-ASCII and a byte index could land inside a codepoint and panic (SB-001).
                let branch_display = truncate_end(&branch, max_branch_len);
                let branch_color = if selected || is_active {
                    p.mauve
                } else {
                    p.overlay0
                };
                // Branch icon (U+2387) folded into the existing indent width (3 normal / 5 indented)
                // so the branch text stays column-aligned under the name and `max_branch_len` is
                // unaffected. The icon sits directly under the workspace's state dot.
                let branch_lead = if card.indented { "   " } else { " " };
                let mut spans = vec![
                    Span::styled(branch_lead, Style::default()),
                    Span::styled("⎇", Style::default().fg(branch_color)),
                    Span::styled(" ", Style::default()),
                    Span::styled(branch_display, Style::default().fg(branch_color)),
                ];
                if let Some(parts) = upstream_label {
                    spans.push(Span::styled(" ", Style::default()));
                    for (idx, (label, color)) in parts.into_iter().enumerate() {
                        if idx > 0 {
                            spans.push(Span::styled(" ", Style::default()));
                        }
                        spans.push(Span::styled(label, Style::default().fg(color)));
                    }
                }
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect::new(card.rect.x, row_y + 1, card.rect.width, 1),
                );
            }
        }
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && list_bottom > area.y {
        // Subtle section divider: a dim rule on the row directly above the toolbar/footer, drawn only
        // when that row is empty (no workspace card occupies it). Zero rows reserved — when the list
        // fills the area the rule is simply omitted. Uses `overlay0` dimmed (matching the agents-section
        // separator), NOT `surface_dim`, which equals the background on some themes and renders invisible.
        // Decorative-loses-to-functional: skip the divider when the drag insertion indicator owns that
        // row, and stop the rule before the scrollbar column so neither is ever overwritten.
        let footer_y = app.sidebar_footer_rect().y;
        let divider_y = footer_y.saturating_sub(1);
        let row_has_card = cards
            .iter()
            .any(|c| divider_y >= c.rect.y && divider_y < c.rect.y + c.rect.height);
        let divider_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let divider_width = divider_right.saturating_sub(area.x);
        if divider_y > area.y
            && divider_y < footer_y
            && !row_has_card
            && insertion_row != Some(divider_y)
            && divider_width > 0
        {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "─".repeat(divider_width as usize),
                    Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
                )),
                Rect::new(area.x, divider_y, divider_width, 1),
            );
        }

        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled("+ new", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        let menu_line = if app.global_menu_attention_badge_visible() {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled("⋯", Style::default().fg(p.overlay0)),
            ])
        } else {
            Line::from(vec![Span::styled("⋯", Style::default().fg(p.overlay0))])
        };
        frame.render_widget(
            Paragraph::new(menu_line).alignment(Alignment::Right),
            menu_rect,
        );
    }
}

fn render_agent_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    // Section separator: `overlay0` dimmed rather than `surface_dim`, which equals the background on
    // some themes (e.g. tokyo-night `surface_dim == panel_bg`) and would render invisible.
    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(
            &sep_line,
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let toggle_rect = agent_panel_toggle_rect(area, app.agent_panel_sort);
    if toggle_rect != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                agent_panel_sort_label(app.agent_panel_sort),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }

    let details = agent_panel_entries_from(app, terminal_runtimes);
    let metrics = agent_panel_scroll_metrics(app, area);
    let scrollbar_rect = agent_panel_scrollbar_rect(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    if body == Rect::default() {
        return;
    }

    let body_width = body.width as usize;
    // Grouped tree: iterate the shared visible-row model (the SAME one hit-test + scroll metrics use).
    // A `GroupHeader` shows the aggregate icon + tab name; a `Child` shows the tree connector, the
    // state-grammar icon, the agent name, and a right-aligned state label.
    for row in agent_visible_rows(app, area) {
        match row {
            AgentVisibleRow::GroupHeader { entry_idx, y } => {
                let Some(detail) = details.get(entry_idx) else {
                    continue;
                };
                let key = (detail.ws_idx, detail.tab_idx);
                let states: Vec<(AgentState, bool)> = details
                    .iter()
                    .filter(|d| (d.ws_idx, d.tab_idx) == key)
                    .map(|d| (d.state, d.seen))
                    .collect();
                let (icon, icon_style) = agents_group_aggregate(&states, app.status_indicators, p);
                let tab = truncate_end(&detail.tab_label, body_width.saturating_sub(2));
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(icon, icon_style),
                        Span::styled(" ", Style::default()),
                        Span::styled(
                            tab,
                            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD),
                        ),
                    ])),
                    Rect::new(body.x, y, body.width, 1),
                );
            }
            AgentVisibleRow::Child { entry_idx, y, last } => {
                let Some(detail) = details.get(entry_idx) else {
                    continue;
                };
                let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
                let (icon, icon_style) =
                    sidebar_agent_icon(detail.state, detail.seen, app.status_indicators, p);
                let label_color = state_label_color(detail.state, detail.seen, p);
                let label = detail
                    .state_labels
                    .get(agent_panel_status_key(detail.state, detail.seen))
                    .map(String::as_str)
                    .unwrap_or_else(|| state_label(detail.state, detail.seen));
                let connector = if last { "└─" } else { "├─" };
                let name = detail.agent_label.as_deref().unwrap_or("agent");

                let row_style = if is_active {
                    Style::default().bg(p.active_row_bg)
                } else {
                    Style::default()
                };
                let name_style = if is_active {
                    Style::default().fg(p.text).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(p.subtext0)
                };
                let status_style = if is_active {
                    Style::default().fg(label_color)
                } else {
                    Style::default().fg(label_color).add_modifier(Modifier::DIM)
                };

                // `└─ ` + icon + ` ` = 5 cols of fixed prefix; the state label is right-aligned with
                // at least one separating space, and the agent name elides between them.
                let label_width = display_width(label);
                let name_budget = body_width.saturating_sub(5 + label_width + 1).max(1);
                let name = truncate_end(name, name_budget);
                let mut spans = vec![
                    Span::styled(connector, Style::default().fg(p.overlay0)),
                    Span::styled(" ", Style::default()),
                    Span::styled(icon, icon_style),
                    Span::styled(" ", Style::default()),
                    Span::styled(name, name_style),
                ];
                let used: usize = spans.iter().map(|s| display_width(&s.content)).sum();
                let pad = body_width
                    .saturating_sub(used)
                    .saturating_sub(label_width)
                    .max(1);
                spans.push(Span::styled(" ".repeat(pad), Style::default()));
                spans.push(Span::styled(label, status_style));
                frame.render_widget(
                    Paragraph::new(Line::from(spans)).style(row_style),
                    Rect::new(body.x, y, body.width, 1),
                );
            }
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x + area.width.saturating_sub(2),
        area.y + area.height.saturating_sub(1),
        1,
        1,
    )
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    let toggle_area = if collapsed {
        collapsed_sidebar_toggle_rect(area)
    } else {
        expanded_sidebar_toggle_rect(area)
    };
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, layout::PaneId, workspace::Workspace};
    use ratatui::{backend::TestBackend, layout::Direction, Terminal};

    #[test]
    fn expanded_and_collapsed_sidebars_use_custom_background() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        app.active = None;
        app.palette.sidebar_bg = ratatui::style::Color::Rgb(12, 34, 56);
        let area = Rect::new(0, 0, 26, 20);

        let mut expanded = Terminal::new(TestBackend::new(26, 20)).unwrap();
        expanded
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert!(expanded
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == app.palette.sidebar_bg));

        let mut collapsed = Terminal::new(TestBackend::new(26, 20)).unwrap();
        collapsed
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        assert!(collapsed
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == app.palette.sidebar_bg));
    }

    #[test]
    fn navigate_selection_keeps_its_existing_background_beside_active_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.selected = 1;
        app.mode = Mode::Navigate;
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let active_row = app.view.workspace_card_areas[0].rect.y;
        let selected_row = app.view.workspace_card_areas[1].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(
            buffer[(0, active_row)].bg,
            app.palette.active_row_bg,
            "active workspace should keep its dedicated background"
        );
        assert_eq!(
            buffer[(0, selected_row)].bg,
            app.palette.selection_bg,
            "navigate selection should use its dedicated cursor background"
        );
    }

    #[test]
    fn selected_active_workspace_resolves_expanded_background() {
        let mut app = crate::app::state::AppState::test_new();
        app.palette = crate::app::state::Palette::terminal();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Navigate;
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let active_row = app.view.workspace_card_areas[0].rect.y;
        let inactive_row = app.view.workspace_card_areas[1].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        assert_eq!(
            terminal.backend().buffer()[(0, active_row)].bg,
            app.palette.active_row_bg
        );

        app.selected = 1;
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, active_row)].bg,
            app.palette.active_row_bg
        );
        assert_eq!(
            terminal.backend().buffer()[(0, inactive_row)].bg,
            app.palette.selection_bg
        );

        app.palette = crate::app::state::Palette::catppuccin();
        app.selected = 0;
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, active_row)].bg,
            app.palette.selection_bg
        );
    }

    #[test]
    fn selected_active_workspace_resolves_collapsed_background() {
        let mut app = crate::app::state::AppState::test_new();
        app.palette = crate::app::state::Palette::terminal();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Navigate;
        let area = Rect::new(0, 0, 5, 8);
        let mut terminal = Terminal::new(TestBackend::new(5, 8)).unwrap();
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();

        let (workspace_area, _, _) = collapsed_sidebar_sections(area);
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y)].bg,
            app.palette.active_row_bg
        );

        app.selected = 1;
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y)].bg,
            app.palette.active_row_bg
        );
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y + 1)].bg,
            app.palette.selection_bg
        );

        app.palette = crate::app::state::Palette::catppuccin();
        app.selected = 0;
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(workspace_area.x, workspace_area.y)].bg,
            app.palette.selection_bg
        );
    }

    #[test]
    fn custom_sidebar_backgrounds_preserve_grouped_active_agent_rows() {
        let (mut app, _, active_pane) = collapsed_agent_app();
        app.active = Some(0);
        app.workspaces[0].tabs[0].layout.focus_pane(active_pane);
        app.palette.sidebar_bg = Color::Rgb(10, 20, 30);
        app.palette.active_row_bg = Color::Rgb(50, 60, 70);
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let (_, detail_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(detail_area, false);
        let entries = agent_panel_entries(&app);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let mut children = 0;
        let mut active = 0;
        for row in agent_visible_rows(&app, detail_area) {
            let (y, background) = match row {
                AgentVisibleRow::GroupHeader { y, .. } => (y, app.palette.sidebar_bg),
                AgentVisibleRow::Child { entry_idx, y, .. } => {
                    children += 1;
                    let entry = &entries[entry_idx];
                    if app.is_active_pane(entry.ws_idx, entry.tab_idx, entry.pane_id) {
                        active += 1;
                        (y, app.palette.active_row_bg)
                    } else {
                        (y, app.palette.sidebar_bg)
                    }
                }
            };
            for x in body.x..body.right() {
                assert_eq!(terminal.backend().buffer()[(x, y)].bg, background);
            }
        }
        assert_eq!((children, active), (3, 1));
    }

    #[test]
    fn collapsed_sidebar_uses_all_workspaces_agent_panel_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        let urgent_pane = app.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        for (ws_idx, pane, state) in [
            (0, app.workspaces[0].tabs[0].root_pane, AgentState::Working),
            (1, app.workspaces[1].tabs[0].root_pane, AgentState::Working),
            (1, urgent_pane, AgentState::Blocked),
        ] {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        }

        assert_eq!(app.workspaces[1].public_pane_number(urgent_pane), Some(2));
        assert_eq!(agent_panel_entries(&app)[0].pane_id, urgent_pane);
        let area = Rect::new(0, 0, 4, 16);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, detail_area.y)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 1)].symbol(), "2");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 2)].symbol(), "3");
        assert_eq!(buffer[(detail_area.x + 2, detail_area.y)].symbol(), "◉");
        assert_eq!(
            buffer[(detail_area.x + 2, detail_area.y)].style().fg,
            Some(app.palette.red)
        );
    }

    #[test]
    fn collapsed_sidebar_numbers_grouped_agents_by_list_position() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();
        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }
        let area = Rect::new(0, 0, 4, 12);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, detail_area.y)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 1)].symbol(), "2");
    }

    /// Two agent panes in one workspace plus a second workspace, so the
    /// assertions can tell pane-level highlighting apart from workspace-level.
    fn collapsed_agent_app() -> (crate::app::state::AppState, PaneId, PaneId) {
        let mut app = crate::app::state::AppState::test_new();
        let mut first = Workspace::test_new("one");
        let second_pane = first.test_split(Direction::Horizontal);
        let first_pane = first.tabs[0].root_pane;
        app.workspaces = vec![first, Workspace::test_new("two")];
        app.ensure_test_terminals();

        let terminal_ids: Vec<_> = app
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect();
        for terminal_id in terminal_ids {
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        (app, first_pane, second_pane)
    }

    #[test]
    fn distinct_indicators_reach_group_headers_children_and_collapsed_spaces() {
        let (mut app, first, second) = collapsed_agent_app();
        app.status_indicators = StatusIndicatorStyle::Symbols;
        let third = app.workspaces[1].tabs[0].root_pane;
        for (ws_idx, pane_id, state) in [
            (0, first, AgentState::Blocked),
            (0, second, AgentState::Idle),
            (1, third, AgentState::Working),
        ] {
            let pane = app.workspaces[ws_idx].tabs[0]
                .panes
                .get_mut(&pane_id)
                .unwrap();
            pane.seen = false;
            app.terminals
                .get_mut(&pane.attached_terminal_id)
                .unwrap()
                .state = state;
        }
        let expected = |pane| {
            if pane == first {
                "×"
            } else if pane == second {
                "✓"
            } else {
                "◐"
            }
        };
        let runtimes = TerminalRuntimeRegistry::new();
        let area = Rect::new(0, 0, 38, 22);
        let entries = agent_panel_entries(&app);
        let body = agent_panel_body_rect(area, false);
        let mut terminal = Terminal::new(TestBackend::new(38, 22)).unwrap();
        terminal
            .draw(|frame| render_agent_detail(&app, &runtimes, frame, area))
            .unwrap();
        let mut counts = (0, 0);
        for row in agent_visible_rows(&app, area) {
            let (x, y, symbol) = match row {
                AgentVisibleRow::GroupHeader { entry_idx, y } => {
                    counts.0 += 1;
                    (
                        body.x,
                        y,
                        if entries[entry_idx].ws_idx == 0 {
                            "×"
                        } else {
                            "◐"
                        },
                    )
                }
                AgentVisibleRow::Child { entry_idx, y, .. } => {
                    counts.1 += 1;
                    (body.x + 3, y, expected(entries[entry_idx].pane_id))
                }
            };
            assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), symbol);
        }
        assert_eq!(counts, (2, 3));

        let area = Rect::new(0, 0, 4, 18);
        let (spaces, _, agents) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(4, 18)).unwrap();
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(spaces.x + 2, spaces.y)].symbol(), "×");
        assert_eq!(buffer[(spaces.x + 2, spaces.y + 1)].symbol(), "◐");
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(
                buffer[(agents.x + 2, agents.y + index as u16)].symbol(),
                expected(entry.pane_id)
            );
        }
    }

    fn collapsed_agent_row_styles(
        app: &crate::app::state::AppState,
        area: Rect,
        detail_area: Rect,
        rows: u16,
    ) -> Vec<Vec<ratatui::style::Style>> {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar_collapsed(app, frame, area))
            .expect("collapsed sidebar should render");
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|row| {
                (detail_area.x..detail_area.x + detail_area.width)
                    .map(|x| buffer[(x, detail_area.y + row)].style())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn collapsed_sidebar_highlights_only_the_focused_agent_pane() {
        let (mut app, first_pane, second_pane) = collapsed_agent_app();
        app.active = Some(0);
        app.workspaces[0].tabs[0].layout.focus_pane(second_pane);
        assert!(app.is_active_pane(0, 0, second_pane));
        assert!(!app.is_active_pane(0, 0, first_pane));

        let area = Rect::new(0, 0, 4, 14);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let rows = collapsed_agent_row_styles(&app, area, detail_area, 3);

        let highlighted: Vec<_> = rows
            .iter()
            .filter(|cells| {
                cells
                    .iter()
                    .all(|style| style.bg == Some(app.palette.active_row_bg))
            })
            .collect();
        assert_eq!(
            highlighted.len(),
            1,
            "only the focused agent pane should be highlighted, across the whole row"
        );
        assert_eq!(highlighted[0][0].fg, Some(app.palette.text));

        let muted = rows
            .iter()
            .filter(|cells| cells[0].fg == Some(app.palette.overlay0))
            .count();
        assert_eq!(
            muted, 2,
            "the sibling pane in the active workspace and the other workspace stay muted"
        );
    }

    #[test]
    fn collapsed_sidebar_does_not_highlight_agents_without_active_workspace() {
        let (mut app, _, _) = collapsed_agent_app();
        app.active = None;

        let area = Rect::new(0, 0, 4, 14);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let rows = collapsed_agent_row_styles(&app, area, detail_area, 3);

        for cells in rows {
            assert_eq!(cells[0].fg, Some(app.palette.overlay0));
            for style in cells {
                assert_ne!(style.bg, Some(app.palette.active_row_bg));
            }
        }
    }

    #[test]
    fn collapsed_sidebar_keeps_workspace_status_visible_for_two_digit_positions() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = (1..=10)
            .map(|idx| Workspace::test_new(&format!("workspace-{idx}")))
            .collect();
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 4, 25);
        let (workspace_area, _, _) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let tenth_row = workspace_area.y + 9;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(workspace_area.x, workspace_area.y)].symbol(), "1");
        assert_eq!(
            buffer[(workspace_area.x + 1, workspace_area.y)].symbol(),
            " "
        );
        assert_eq!(
            buffer[(workspace_area.x + 2, workspace_area.y)].symbol(),
            "◌"
        );
        assert_eq!(buffer[(workspace_area.x, tenth_row)].symbol(), "1");
        assert_eq!(buffer[(workspace_area.x + 1, tenth_row)].symbol(), "0");
        assert_eq!(buffer[(workspace_area.x + 2, tenth_row)].symbol(), "◌");
    }

    #[test]
    fn workspace_list_keeps_parent_and_children_packed_with_fixed_gap() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/zynk-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/zynk-two"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = Some("main".into());
        }

        let (cards, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert_eq!(cards.len(), 4);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height);
        assert_eq!(cards[2].rect.y, cards[1].rect.y + cards[1].rect.height);
        assert_eq!(cards[3].rect.y, cards[2].rect.y + cards[2].rect.height + 1);

        let metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 8));
        assert_eq!(metrics.viewport_rows, 3);
        assert_eq!(metrics.max_offset_from_bottom, 1);
    }

    #[test]
    fn collapsed_sidebar_keeps_status_visible_for_two_digit_positions() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = (1..=10)
            .map(|idx| Workspace::test_new(&format!("workspace-{idx}")))
            .collect();
        app.ensure_test_terminals();
        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = AgentState::Idle;
        }
        let area = Rect::new(0, 0, 4, 25);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");
        let tenth_row = detail_area.y + 9;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, tenth_row)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x + 1, tenth_row)].symbol(), "0");
        assert_eq!(buffer[(detail_area.x + 2, tenth_row)].symbol(), "○");
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, area, false, &app.palette))
            .expect("sidebar toggle should render");

        let toggle = expanded_sidebar_toggle_rect(area);
        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x + area.width - 2);
        assert_eq!(toggle.y, area.y + area.height - 1);
    }

    #[test]
    fn all_workspaces_agent_panel_entries_use_workspace_and_optional_tab_labels() {
        let mut app = crate::app::state::AppState::test_new();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_tab = second.test_add_tab(Some("logs"));
        let second_pane = second.tabs[second_tab].root_pane;

        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.workspaces[1].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "one");
        assert!(entries[0].primary_tab_label.is_none());
        assert_eq!(entries[0].agent_label.as_deref(), Some("pi"));
        assert_eq!(entries[1].primary_label, "two");
        assert_eq!(entries[1].primary_tab_label.as_deref(), Some("logs"));
        assert_eq!(entries[1].agent_label.as_deref(), Some("claude"));
    }

    #[test]
    fn agent_panel_tab_label_visibility_tracks_tab_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let single_auto = Workspace::test_new("auto");
        let mut single_custom = Workspace::test_new("custom");
        single_custom.tabs[0].set_custom_name("focus".into());
        let mut multi = Workspace::test_new("multi");
        multi.test_add_tab(Some("logs"));
        app.workspaces = vec![single_auto, single_custom, multi];
        app.ensure_test_terminals();
        for (ws_idx, tab_idx, agent) in [
            (0, 0, Agent::Pi),
            (1, 0, Agent::Claude),
            (2, 0, Agent::Codex),
            (2, 1, Agent::Pi),
        ] {
            let pane_id = app.workspaces[ws_idx].tabs[tab_idx].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(agent);
        }
        let entries = agent_panel_entries(&app);
        let labels: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.primary_label.as_str(),
                    entry.primary_tab_label.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("auto", None),
                ("custom", Some("focus")),
                ("multi", Some("1")),
                ("multi", Some("logs")),
            ]
        );
    }

    #[test]
    fn priority_agent_panel_sort_uses_attention_then_space_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
            Workspace::test_new("four"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, state| {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, AgentState::Working);
        set_state(&mut app, 1, AgentState::Idle);
        set_state(&mut app, 2, AgentState::Working);
        set_state(&mut app, 3, AgentState::Blocked);

        let done_pane = app.workspaces[1].tabs[0].root_pane;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&done_pane)
            .unwrap()
            .seen = false;

        let labels: Vec<String> = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect();

        // zynk's `workspace_attention_priority` ranks Working ABOVE done/unseen (the fork keeps a
        // live working pane louder than a finished one — see `workspace_attention_priority_working
        // _beats_done` + the `workspace_info` API regression). So priority order here is
        // blocked > working > done > idle: four (blocked), then the two working spaces in space
        // order (one, three), then two (done). Upstream's ordering put done above working.
        assert_eq!(labels, ["four", "one", "three", "two"]);
    }

    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "zynk-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("zynk");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "zynk");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5);

        assert_eq!(divider, Rect::default());
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("zynk-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "zynk".into(),
                repo_root: std::path::PathBuf::from("/repo/zynk"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            repo_name: "zynk".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
        ];

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height);
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/zynk-review"),
        ];

        let entries = workspace_list_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_offset_can_start_inside_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/zynk-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/zynk-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = Some("main".into());
        }
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/zynk-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    /// Render `render_workspace_list` into a fresh `TestBackend` and return its rows as strings.
    /// Mirrors the real render path: `sidebar_rect` is set, the workspace-list sub-rect is derived
    /// from it (so footer/toolbar rects land inside the buffer), and `workspace_card_areas` is
    /// computed for that sub-rect just like `render_sidebar` does.
    fn render_workspace_list_to_lines(
        app: &mut AppState,
        width: u16,
        height: u16,
        is_navigating: bool,
    ) -> Vec<String> {
        app.view.sidebar_rect = Rect::new(0, 0, width, height);
        // `compute_workspace_card_areas` derives the ws sub-rect from the FULL sidebar rect itself,
        // so feed it the full rect; `render_workspace_list` takes the already-derived ws sub-rect.
        app.view.workspace_card_areas = compute_workspace_card_areas(app, app.view.sidebar_rect);
        let ws_area = expanded_sidebar_sections(app.view.sidebar_rect, app.sidebar_section_split).0;
        let runtimes = TerminalRuntimeRegistry::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_workspace_list(app, &runtimes, frame, ws_area, is_navigating))
            .expect("workspace list should render");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn branch_row_shows_branch_icon_before_branch_text() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("main".to_string()); // pub(crate); backs Workspace::branch()
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;

        let lines = render_workspace_list_to_lines(&mut app, 26, 12, false);
        let joined = lines.join("\n");
        assert!(
            joined.contains('⎇'),
            "branch line must show the ⎇ icon, got:\n{joined}"
        );
        let branch_row = lines
            .iter()
            .find(|l| l.contains("main"))
            .expect("a branch row");
        let icon_col = branch_row.find('⎇').expect("icon on branch row");
        let text_col = branch_row.find("main").expect("branch text on branch row");
        assert!(
            icon_col < text_col,
            "icon must come before the branch text: {branch_row:?}"
        );
    }

    #[test]
    fn workspace_unknown_state_uses_hollow_dotted_glyph() {
        // `spaces` shows `◌` for a no-agent / Unknown workspace (idle stays `○`). Scoped to the
        // workspace list — agent-panel icons are unaffected.
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("solo");
        ws.cached_git_branch = None;
        app.workspaces = vec![ws];
        app.ensure_test_terminals(); // a bare terminal with no detected agent -> Unknown
        app.active = Some(0);

        let lines = render_workspace_list_to_lines(&mut app, 26, 20, false);
        let row = lines
            .iter()
            .find(|l| l.contains("solo"))
            .expect("a workspace row");
        assert!(
            row.contains('◌'),
            "Unknown workspace must use the ◌ glyph, got {row:?}",
        );
        assert!(
            !row.contains('·'),
            "the old · glyph must be gone, got {row:?}",
        );
    }

    #[test]
    fn short_list_draws_subtle_divider_above_toolbar() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("solo")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mouse_capture = true; // the toolbar (and thus its boundary) only renders in mouse mode

        let lines = render_workspace_list_to_lines(&mut app, 26, 20, false);
        let footer_y = app.sidebar_footer_rect().y;
        assert!(footer_y >= 1, "need a row above the footer");
        let divider_row = &lines[(footer_y - 1) as usize];
        assert!(
            divider_row.contains('─'),
            "expected a subtle divider above the toolbar, got {divider_row:?}"
        );
    }

    #[test]
    fn toolbar_shows_plus_new_and_ellipsis_no_box() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("solo")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mouse_capture = true;

        let lines = render_workspace_list_to_lines(&mut app, 26, 20, false);
        let joined = lines.join("\n");
        assert!(
            joined.contains("+ new"),
            "toolbar must read '+ new', got:\n{joined}"
        );
        assert!(
            joined.contains('⋯'),
            "toolbar must show the ⋯ menu glyph, got:\n{joined}"
        );
        assert!(
            !joined.contains("menu"),
            "old 'menu' text must be gone, got:\n{joined}"
        );
    }

    #[test]
    fn toolbar_divider_yields_to_drag_insertion_indicator() {
        // The decorative divider must never overwrite the functional drag insertion indicator. We
        // engineer a layout where the drop indicator (drop AFTER the last card) lands exactly on the
        // divider row (the empty row above the toolbar), then assert the accent indicator survives.
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("solo");
        ws.cached_git_branch = Some("feature".to_string()); // 2-row card so its bottom hits the divider row
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mouse_capture = true;
        app.drag = Some(crate::app::state::DragState {
            target: crate::app::state::DragTarget::WorkspaceReorder {
                source_id: 0,
                source_ws_idx: 0,
                insert_idx: Some(1),
            },
        });
        app.view.sidebar_rect = Rect::new(0, 0, 26, 12);

        let area = expanded_sidebar_sections(app.view.sidebar_rect, app.sidebar_section_split).0;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, app.view.sidebar_rect);

        // Precondition: the drag drop indicator coincides with the divider row (footer - 1).
        let footer_y = app.sidebar_footer_rect().y;
        let divider_y = footer_y - 1;
        assert_eq!(
            workspace_drop_indicator_row(&app.view.workspace_card_areas, area, 1),
            Some(divider_y),
            "test setup: the drop indicator must land on the divider row",
        );

        let runtimes = TerminalRuntimeRegistry::new();
        let mut terminal = Terminal::new(TestBackend::new(26, 12)).unwrap();
        terminal
            .draw(|frame| render_workspace_list(&app, &runtimes, frame, area, false))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(
            buffer[(area.x, divider_y)].style().fg,
            Some(app.palette.accent),
            "functional drag indicator (accent) must not be overwritten by the dim divider",
        );
    }

    #[test]
    fn divider_is_visible_against_background() {
        // Guards the `surface_dim == panel_bg` trap: on tokyo-night those are identical, so a
        // surface_dim divider would render invisible. The divider must use a colour distinct from the
        // background (overlay0 dimmed).
        let mut app = AppState::test_new();
        app.palette = crate::app::state::Palette::tokyo_night();
        assert_eq!(
            app.palette.surface_dim, app.palette.panel_bg,
            "precondition: this theme has surface_dim == background",
        );
        let mut ws = Workspace::test_new("solo");
        ws.cached_git_branch = None;
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mouse_capture = true;
        app.view.sidebar_rect = Rect::new(0, 0, 26, 20);

        let area = expanded_sidebar_sections(app.view.sidebar_rect, app.sidebar_section_split).0;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, app.view.sidebar_rect);
        let divider_y = app.sidebar_footer_rect().y - 1;

        let runtimes = TerminalRuntimeRegistry::new();
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_workspace_list(&app, &runtimes, frame, area, false))
            .unwrap();
        let cell = terminal.backend().buffer()[(area.x, divider_y)].clone();

        assert_eq!(
            cell.symbol(),
            "─",
            "divider should render above the toolbar"
        );
        assert_eq!(
            cell.style().fg,
            Some(app.palette.overlay0),
            "divider uses overlay0 so it is visible",
        );
        assert_ne!(
            cell.style().fg,
            Some(app.palette.panel_bg),
            "divider must never be the background colour",
        );
    }

    #[test]
    fn working_marks_match_across_sidebar_and_mobile() {
        let p = crate::app::state::Palette::tokyo_night();
        let expected = ("●", Style::default().fg(p.yellow));
        assert_eq!(
            sidebar_agent_icon(AgentState::Working, false, StatusIndicatorStyle::Dots, &p),
            expected
        );
        assert_eq!(
            crate::ui::status::agent_icon(
                AgentState::Working,
                false,
                StatusIndicatorStyle::Dots,
                &p
            ),
            expected
        );
        assert_eq!(
            crate::ui::status::state_icon(
                AgentState::Working,
                false,
                StatusIndicatorStyle::Dots,
                &p
            ),
            expected
        );
    }

    #[test]
    fn sidebar_agent_icon_grammar() {
        let p = crate::app::state::Palette::tokyo_night();
        for seen in [false, true] {
            let (g, s) =
                sidebar_agent_icon(AgentState::Working, seen, StatusIndicatorStyle::Dots, &p);
            assert_eq!(g, "●");
            assert_eq!(s.fg, Some(p.yellow));
        }
        assert_eq!(
            sidebar_agent_icon(AgentState::Idle, true, StatusIndicatorStyle::Dots, &p),
            ("○", Style::default().fg(p.green))
        );
        assert_eq!(
            sidebar_agent_icon(AgentState::Idle, false, StatusIndicatorStyle::Dots, &p),
            ("○", Style::default().fg(p.teal))
        );
        assert_eq!(
            sidebar_agent_icon(AgentState::Blocked, false, StatusIndicatorStyle::Dots, &p),
            ("◉", Style::default().fg(p.red))
        );
        assert_eq!(
            sidebar_agent_icon(AgentState::Unknown, false, StatusIndicatorStyle::Dots, &p).0,
            "◌"
        );
    }

    #[test]
    fn agents_group_aggregate_priority() {
        let p = crate::app::state::Palette::tokyo_night();
        use AgentState::*;
        // blocked wins outright
        assert_eq!(
            agents_group_aggregate(
                &[(Idle, true), (Blocked, false)],
                StatusIndicatorStyle::Dots,
                &p
            )
            .0,
            "◉"
        );
        // else working wins with a static mark
        assert_eq!(
            agents_group_aggregate(
                &[(Idle, true), (Working, false)],
                StatusIndicatorStyle::Dots,
                &p
            )
            .0,
            "●"
        );
        // else done/unseen (teal)
        assert_eq!(
            agents_group_aggregate(
                &[(Idle, true), (Idle, false)],
                StatusIndicatorStyle::Dots,
                &p
            ),
            ("○", Style::default().fg(p.teal))
        );
        // else idle (green)
        assert_eq!(
            agents_group_aggregate(&[(Idle, true)], StatusIndicatorStyle::Dots, &p),
            ("○", Style::default().fg(p.green))
        );
        // else unknown / empty
        assert_eq!(
            agents_group_aggregate(&[(Unknown, false)], StatusIndicatorStyle::Dots, &p).0,
            "◌"
        );
        assert_eq!(
            agents_group_aggregate(&[], StatusIndicatorStyle::Dots, &p).0,
            "◌"
        );
    }

    #[test]
    fn workspace_attention_priority_working_beats_done() {
        use AgentState::*;
        // Spaces aggregate ordering: working outranks done/unseen; blocked still outranks working.
        assert!(
            workspace_attention_priority(Working, true) > workspace_attention_priority(Idle, false),
            "working beats done/unseen"
        );
        assert!(
            workspace_attention_priority(Blocked, false)
                > workspace_attention_priority(Working, true),
            "blocked beats working"
        );
        assert!(
            workspace_attention_priority(Idle, false) > workspace_attention_priority(Idle, true),
            "done/unseen beats idle/seen"
        );
        assert!(
            workspace_attention_priority(Idle, true) > workspace_attention_priority(Unknown, false),
            "idle beats unknown"
        );
    }

    #[test]
    fn space_aggregate_state_working_beats_done() {
        // A space with one done/unseen workspace and one working workspace rolls up to Working.
        let mut app = AppState::test_new();
        let space = |key: &str| crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: key.into(),
            repo_root: "/tmp/repo".into(),
            checkout_path: "/tmp/repo/x".into(),
            is_linked_worktree: false,
        };
        let mut ws_done = Workspace::test_new("done-ws");
        ws_done.worktree_space = Some(space("repo"));
        let mut ws_working = Workspace::test_new("working-ws");
        ws_working.worktree_space = Some(space("repo"));
        app.workspaces = vec![ws_done, ws_working];
        app.ensure_test_terminals();

        // workspace 0 = done/unseen, workspace 1 = working
        for (idx, state, seen) in [
            (0usize, AgentState::Idle, false),
            (1usize, AgentState::Working, true),
        ] {
            let pane = app.workspaces[idx].tabs[0].root_pane;
            let tid = app.workspaces[idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&tid).unwrap().state = state;
            app.workspaces[idx].tabs[0]
                .panes
                .get_mut(&pane)
                .unwrap()
                .seen = seen;
        }

        let (state, _seen) = space_aggregate_state(&app, "repo");
        assert_eq!(state, AgentState::Working);
    }

    #[test]
    fn workspace_state_icon_grammar() {
        // Spaces retain the sidebar-specific non-working marks.
        let p = crate::app::state::Palette::tokyo_night();
        for seen in [false, true] {
            let (g, s) =
                workspace_state_icon(AgentState::Working, seen, StatusIndicatorStyle::Dots, &p);
            assert_eq!(g, "●");
            assert_eq!(s.fg, Some(p.yellow));
        }
        assert_eq!(
            workspace_state_icon(AgentState::Blocked, false, StatusIndicatorStyle::Dots, &p),
            ("◉", Style::default().fg(p.red))
        );
        assert_eq!(
            workspace_state_icon(AgentState::Idle, false, StatusIndicatorStyle::Dots, &p),
            ("○", Style::default().fg(p.teal))
        );
        assert_eq!(
            workspace_state_icon(AgentState::Idle, true, StatusIndicatorStyle::Dots, &p),
            ("○", Style::default().fg(p.green))
        );
        assert_eq!(
            workspace_state_icon(AgentState::Unknown, false, StatusIndicatorStyle::Dots, &p).0,
            "◌"
        );
    }

    #[test]
    fn global_default_state_icon_unchanged() {
        // Non-goal guard: the spaces grammar change must NOT alter the default `state_icon`
        // (other surfaces depend on it) — filled dots, its own mapping, no animation.
        let p = crate::app::state::Palette::tokyo_night();
        assert_eq!(
            crate::ui::status::state_icon(
                AgentState::Working,
                true,
                StatusIndicatorStyle::Dots,
                &p
            ),
            ("●", Style::default().fg(p.yellow))
        );
        assert_eq!(
            crate::ui::status::state_icon(AgentState::Idle, false, StatusIndicatorStyle::Dots, &p),
            ("●", Style::default().fg(p.teal))
        );
        assert_eq!(
            crate::ui::status::state_icon(
                AgentState::Unknown,
                false,
                StatusIndicatorStyle::Dots,
                &p
            ),
            ("·", Style::default().fg(p.overlay0))
        );
    }

    fn agent_entry(ws_idx: usize, tab_idx: usize, agent: &str, pane: u32) -> AgentPanelEntry {
        AgentPanelEntry {
            ws_idx,
            tab_idx,
            pane_id: crate::layout::PaneId::from_raw(pane),
            primary_label: "ws".into(),
            primary_tab_label: None,
            tab_label: format!("tab {}", tab_idx + 1),
            agent_label: Some(agent.into()),
            state: AgentState::Idle,
            seen: true,
            last_agent_state_change_seq: None,
            custom_status: None,
            state_labels: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn agent_visible_rows_for_entries_groups_with_tree() {
        // tab 1: claude, codex; tab 2: pi.
        let entries = vec![
            agent_entry(0, 0, "claude", 1),
            agent_entry(0, 0, "codex", 2),
            agent_entry(0, 1, "pi", 3),
        ];
        let body = Rect::new(0, 0, 30, 12);
        let rows = agent_visible_rows_for_entries(&entries, body, 0);

        let headers: Vec<_> = rows
            .iter()
            .filter(|r| matches!(r, AgentVisibleRow::GroupHeader { .. }))
            .collect();
        assert_eq!(headers.len(), 2, "two tab groups -> two headers: {rows:?}");

        let children: Vec<(usize, u16, bool)> = rows
            .iter()
            .filter_map(|r| match r {
                AgentVisibleRow::Child { entry_idx, y, last } => Some((*entry_idx, *y, *last)),
                _ => None,
            })
            .collect();
        assert_eq!(
            children.iter().map(|c| c.0).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "children in order"
        );
        assert_eq!(
            children.iter().map(|c| c.2).collect::<Vec<_>>(),
            vec![false, true, true],
            "last flags: claude=false, codex=true, pi=true"
        );
        let ys: Vec<u16> = children.iter().map(|c| c.1).collect();
        assert!(
            ys.windows(2).all(|w| w[0] < w[1]),
            "child y ascending: {ys:?}"
        );

        for r in &rows {
            if let AgentVisibleRow::GroupHeader { entry_idx, y } = r {
                let child_y = children.iter().find(|c| c.0 == *entry_idx).unwrap().1;
                assert!(
                    *y < child_y,
                    "header y {y} must precede its first child y {child_y}"
                );
            }
        }
    }

    #[test]
    fn max_agent_panel_scroll_reaches_last_child_when_headers_consume_rows() {
        // 8 single-child tab groups (distinct ws_idx) — headers consume rows in a short body, so the
        // last child is only reachable by scrolling. A header-blind metric would strand it.
        let entries: Vec<_> = (0..8)
            .map(|i| agent_entry(i, 0, "claude", i as u32 + 1))
            .collect();
        let body = Rect::new(0, 0, 30, 8);
        let max = max_agent_panel_scroll(&entries, body);
        assert!(
            max > 0,
            "headers consume rows -> last child needs scrolling (max={max})"
        );
        let rows = agent_visible_rows_for_entries(&entries, body, max);
        let last = entries.len() - 1;
        assert!(
            rows.iter().any(
                |r| matches!(r, AgentVisibleRow::Child { entry_idx, .. } if *entry_idx == last)
            ),
            "last child reachable at max scroll: {rows:?}"
        );
        // metrics/rows consistency: children placed at the bottom == viewport (total - max).
        assert_eq!(
            agent_children_placed_from(&entries, body, max),
            entries.len() - max
        );
    }

    #[test]
    fn agent_visible_rows_clamps_stale_scroll_to_last_page() {
        let mut app = AppState::test_new();
        app.workspaces = (0..8)
            .map(|i| Workspace::test_new(&format!("w{i}")))
            .collect();
        app.ensure_test_terminals();
        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let tid = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&tid).unwrap().detected_agent = Some(Agent::Claude);
        }
        app.active = Some(0);

        let total = agent_panel_entries(&app).len();
        assert_eq!(total, 8, "8 single-pane workspaces -> 8 agent entries");
        let area = Rect::new(0, 0, 30, 8 + AGENT_PANEL_HEADER_ROWS);

        // A stale, over-large scroll must NOT hide the last child — the clamp lives in the wrapper,
        // not in a recursive metrics call.
        app.agent_panel_scroll = 9999;
        let rows = agent_visible_rows(&app, area);
        let last = total - 1;
        assert!(
            rows.iter().any(
                |r| matches!(r, AgentVisibleRow::Child { entry_idx, .. } if *entry_idx == last)
            ),
            "stale scroll still renders the last reachable page: {rows:?}"
        );
    }

    fn render_agent_detail_to_lines(app: &mut AppState, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let runtimes = TerminalRuntimeRegistry::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_agent_detail(app, &runtimes, frame, area))
            .expect("agent detail should render");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn render_agent_detail_groups_panes_under_tab_header_with_tree() {
        // One workspace, one tab, two agent panes -> one tab group, two children
        // (├─ then └─).
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("zynk");
        let p1 = ws.tabs[0].root_pane;
        let p2 = ws.test_split(ratatui::layout::Direction::Horizontal);
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        for pane in [p1, p2] {
            let tid = app.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals
                .get_mut(&tid)
                .unwrap()
                .set_detected_state(Some(Agent::Claude), AgentState::Working);
        }

        let lines = render_agent_detail_to_lines(&mut app, 30, 16);
        let joined = lines.join("\n");
        assert!(
            lines.iter().any(|l| l.contains("├─")),
            "first child uses ├─:\n{joined}"
        );
        assert!(
            lines.iter().any(|l| l.contains("└─")),
            "last child uses └─:\n{joined}"
        );
        assert!(
            joined.contains("●"),
            "a static working mark renders:\n{joined}"
        );
        // the state label is present.
        assert!(joined.contains("working"), "state label present:\n{joined}");
    }

    #[test]
    fn collapsed_workspace_rail_uses_hollow_dotted_for_unknown() {
        // The collapsed workspace rail must share the spaces Unknown glyph `◌`, not the global `·`.
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("solo");
        ws.cached_git_branch = None;
        app.workspaces = vec![ws];
        app.ensure_test_terminals(); // bare terminal, no detected agent -> Unknown aggregate
        app.active = Some(0);

        let area = Rect::new(0, 0, 4, 16);
        let mut terminal =
            Terminal::new(TestBackend::new(4, 16)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");
        let buffer = terminal.backend().buffer().clone();
        // workspace 0's rail row is the first row (y=0): num + space + state dot.
        let row0: String = (0..4)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(
            row0.contains('◌'),
            "collapsed workspace rail must show ◌ for Unknown, got {row0:?}"
        );
        assert!(
            !row0.contains('·'),
            "the old · glyph must be gone from the collapsed rail, got {row0:?}"
        );
    }

    #[test]
    fn branch_row_truncates_non_ascii_branch_without_panic() {
        // SB-001 regression: a multibyte branch name in a narrow card must truncate by CHARACTER
        // (safe), not byte-slice mid-codepoint and panic.
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("fitur/日本語-emoji-🌙-very-long".to_string());
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;

        // Narrow width forces the truncation path (small max_branch_len).
        let lines = render_workspace_list_to_lines(&mut app, 14, 12, false);
        let joined = lines.join("\n");
        // Reaching here without a panic is the core assertion; truncation produced an ellipsis.
        assert!(
            joined.contains('…'),
            "narrow non-ASCII branch must truncate with …, got:\n{joined}"
        );
    }
}
