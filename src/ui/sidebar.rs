// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
mod tokens;

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
use tokens::{ResolvedToken, SpaceTokenContext};

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
    pub agent_kind_label: Option<String>,
    pub agent: Option<crate::detect::Agent>,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub tokens: std::collections::HashMap<String, String>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,

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
    agent_panel_header_label_rect(area, agent_panel_sort_label(sort))
}

fn agent_panel_header_label_rect(area: Rect, label: &str) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let width = display_width_u16(label).min(area.width);
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

fn active_agent_view_label(app: &AppState) -> Option<&str> {
    app.agent_view_override
        .as_ref()
        .map(|view| view.label.as_deref().unwrap_or("filtered"))
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn all_agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    collect_agent_panel_entries_with_runtimes(app, None)
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
    let mut entries = collect_agent_panel_entries_with_runtimes(app, terminal_runtimes);
    crate::app::agent_view::apply_agent_view(app, &mut entries);
    entries
}

fn collect_agent_panel_entries_with_runtimes(
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

    app.workspaces
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
                        agent_kind_label: detail.agent_kind_label,
                        agent: detail.agent,
                        pane_label: detail.pane_label,
                        terminal_title: detail.terminal_title,
                        terminal_title_stripped: detail.terminal_title_stripped,
                        tokens: detail.tokens,
                        state: detail.state,
                        seen: detail.seen,
                        last_agent_state_change_seq: detail.last_agent_state_change_seq,

                        state_labels: detail.state_labels,
                    }
                })
        })
        .collect()
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

fn workspace_row_height(app: &AppState, ws: &crate::workspace::Workspace, indented: bool) -> u16 {
    let (state, seen) = ws.aggregate_state(&app.terminals);
    let workspace_label = ws.display_name_from_terminals(&app.terminals);
    let label = if indented {
        grouped_child_display_label(
            &workspace_label,
            ws.branch().as_deref(),
            ws.custom_name.is_some(),
        )
    } else {
        workspace_label
    };
    let values = ws.metadata_tokens.values();
    tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: &label,
            branch: ws.branch().as_deref(),
            state_text: state_label(state, seen),
            ahead_behind: ws.git_ahead_behind(),
            tokens: &values,
            suppress_git_details: indented,
        },
    )
    .len()
    .max(1)
    .min(u16::MAX as usize) as u16
}

fn workspace_row_height_in_body(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    indented: bool,
    body_height: u16,
) -> u16 {
    workspace_row_height(app, ws, indented).min(body_height)
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

fn workspace_entry_gap(entries: &[WorkspaceListEntry], entry_idx: usize, row_gap: u16) -> u16 {
    if entry_idx.saturating_add(1) < entries.len()
        && !next_entry_is_indented_workspace(entries, entry_idx)
    {
        row_gap
    } else {
        0
    }
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    requested.min(workspace_list_bottom_start(app, ws_area))
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
    let body_width = area
        .width
        .saturating_sub(u16::from(has_scrollbar && area.width > 1));
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
        let row_height = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                workspace_row_height_in_body(app, ws, *indented, body.height)
            }
        };
        if row_height > body.height.saturating_sub(used_rows) {
            break;
        }
        let gap = workspace_entry_gap(&entries, entry_idx, app.sidebar_spaces.row_gap);
        used_rows = used_rows
            .saturating_add(row_height)
            .saturating_add(gap)
            .min(body.height);
        visible += 1;
    }
    visible
}

fn workspace_list_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = workspace_list_body_rect(area, false);
    let entries = workspace_list_entries(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (entry_idx, entry) in entries.iter().enumerate().rev() {
        let WorkspaceListEntry::Workspace { ws_idx, indented } = entry;
        let Some(workspace) = app.workspaces.get(*ws_idx) else {
            continue;
        };
        let height = workspace_row_height_in_body(app, workspace, *indented, body.height);
        let gap = workspace_entry_gap(&entries, entry_idx, app.sidebar_spaces.row_gap);
        let remaining = body.height.saturating_sub(used_rows);
        if height > remaining || gap > remaining.saturating_sub(height) {
            break;
        }
        used_rows = used_rows.saturating_add(height).saturating_add(gap);
        start = entry_idx;
    }
    start.min(entries.len().saturating_sub(1))
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let max_offset_from_bottom = workspace_list_bottom_start(app, area);
    let scroll = app.workspace_scroll.min(max_offset_from_bottom);
    let viewport_rows = workspace_list_visible_count(app, area, scroll);
    let offset_from_bottom = max_offset_from_bottom.saturating_sub(scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && area.width > 1 && body.width > 0 && body.height > 0)
        .then_some(Rect::new(
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
    let body_height = area.y.saturating_add(area.height).saturating_sub(body_y);
    let body_width = area
        .width
        .saturating_sub(u16::from(has_scrollbar && area.width > 1));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn agent_resolved_rows(app: &AppState, entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let label = entry
        .state_labels
        .get(agent_panel_status_key(entry.state, entry.seen))
        .map(String::as_str)
        .unwrap_or_else(|| state_label(entry.state, entry.seen));
    tokens::agent_rows(&app.sidebar_agents, entry, label)
}

fn agent_row_heights(app: &AppState, entries: &[AgentPanelEntry]) -> Vec<u16> {
    entries
        .iter()
        .map(|entry| {
            agent_resolved_rows(app, entry)
                .len()
                .max(1)
                .min(u16::MAX as usize) as u16
        })
        .collect()
}

/// Number of `Child` rows the grouped placement renders starting at `scroll` — the real,
/// header-aware capacity (NOT a flat per-entry count). Uses only the pure placement primitive.
fn agent_children_placed_from(
    entries: &[AgentPanelEntry],
    body: Rect,
    scroll: usize,
    row_gap: u16,
    heights: &[u16],
) -> usize {
    agent_visible_rows_for_entries(entries, body, scroll, row_gap, heights)
        .iter()
        .filter(|r| matches!(r, AgentVisibleRow::Child { .. }))
        .count()
}

/// The maximum `agent_panel_scroll` (entries skipped from the top) that still renders the LAST entry
/// as a child: the smallest `s` with `s + children_placed_from(s) >= total` (monotone). A linear scan
/// over raw scroll offsets that calls ONLY the pure primitive — no metrics, no clamping, no recursion.
fn max_agent_panel_scroll(
    entries: &[AgentPanelEntry],
    body: Rect,
    row_gap: u16,
    heights: &[u16],
) -> usize {
    let total = entries.len();
    if total == 0 {
        return 0;
    }
    for scroll in 0..total {
        if scroll + agent_children_placed_from(entries, body, scroll, row_gap, heights) >= total {
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
    let heights = agent_row_heights(app, &entries);
    let max_offset_from_bottom =
        max_agent_panel_scroll(&entries, body, app.sidebar_agents.row_gap, &heights);
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
    let heights = agent_row_heights(app, &entries);
    agent_visible_rows_for_entries(&entries, body, scroll, app.sidebar_agents.row_gap, &heights)
}

pub(crate) fn agent_panel_scroll_for_target(app: &AppState, area: Rect, target: usize) -> usize {
    let entries = agent_panel_entries(app);
    let gap = app.sidebar_agents.row_gap;
    let heights = agent_row_heights(app, &entries);
    let max_scroll =
        max_agent_panel_scroll(&entries, agent_panel_body_rect(area, false), gap, &heights);
    let scroll = app.agent_panel_scroll.min(max_scroll);
    if target >= entries.len() {
        return scroll;
    }

    let body = agent_panel_body_rect(area, max_scroll > 0);
    (scroll.min(target)..=target.min(max_scroll))
        .find(|offset| {
            agent_visible_rows_for_entries(&entries, body, *offset, gap, &heights)
                .iter()
                .any(|row| matches!(row, AgentVisibleRow::Child { entry_idx, .. } if *entry_idx == target))
        })
        .unwrap_or(scroll)
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (area.width > 1 && should_show_scrollbar(metrics) && body.width > 0 && body.height > 0)
        .then_some(Rect::new(
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

    let scroll = app.workspace_scroll.min(metrics.max_offset_from_bottom);
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
                let row_height = workspace_row_height_in_body(app, ws, *indented, body.height);
                let gap = workspace_entry_gap(&entries, entry_idx, app.sidebar_spaces.row_gap);
                if row_height > body_bottom.saturating_sub(row_y) {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                });
                row_y = row_y
                    .saturating_add(row_height)
                    .saturating_add(gap)
                    .min(body_bottom);
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
        let divider_color = if app.agent_view_override.is_some() {
            p.accent
        } else {
            p.surface_dim
        };
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(divider_color));
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
        height: u16,
        last: bool,
    },
}

/// Tab group key for the agents panel: agents are grouped by their `(workspace, tab)`.
fn agent_group_key(e: &AgentPanelEntry) -> (usize, usize) {
    (e.ws_idx, e.tab_idx)
}

/// PURE placement of the grouped agents rows: takes NO `AppState`, computes NO metrics, does NO
/// clamping. Given the entry slice, the body rect, and a scroll offset (entries skipped from the
/// top), it lays out the parallel heights from `body.y`, with `row_gap` before every entry except
/// the first. Heights clip to body-minus-header regardless of the scroll offset.
/// A tab-group change emits a `GroupHeader` before the `Child`; the same group emits just the
/// `Child`. It stops before any row would leave the body and never emits a dangling header whose
/// child would not fit. This is the one primitive `agent_children_placed_from` /
/// `max_agent_panel_scroll` / the public `agent_visible_rows` all build on (a strict, non-recursive DAG).
fn agent_visible_rows_for_entries(
    entries: &[AgentPanelEntry],
    body: Rect,
    scroll: usize,
    row_gap: u16,
    heights: &[u16],
) -> Vec<AgentVisibleRow> {
    debug_assert_eq!(entries.len(), heights.len());
    let mut rows = Vec::new();
    if body.width == 0 || body.height < 2 {
        return rows;
    }
    let body_bottom = body.y.saturating_add(body.height);
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
        let spacer = if placed_any { row_gap } else { 0 };
        let header = u16::from(group_changed);
        let height = heights[idx].max(1).min(body.height.saturating_sub(1));
        // Admit the complete child before publishing either it or its group header.
        let child_y = row_y.saturating_add(spacer).saturating_add(header);
        if child_y >= body_bottom || height > body_bottom.saturating_sub(child_y) {
            break;
        }
        row_y = row_y.saturating_add(spacer);
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
            height,
            last,
        });
        row_y = row_y.saturating_add(height);
        prev_group = Some(group);
        placed_any = true;
    }
    rows
}

#[derive(Clone, Copy)]
struct TokenStyles {
    state_text: Style,
    workspace: Style,
    secondary: Style,
    custom: Style,
}

fn resolved_token_spans(
    resolved: &[ResolvedToken],
    state_icon: (&str, Style),
    styles: TokenStyles,
    p: &Palette,
    max_width: usize,
    right_align_state: bool,
) -> Vec<Span<'static>> {
    if right_align_state {
        if let Some(ResolvedToken::StateText(text)) = resolved.last() {
            let text = truncate_end(text, max_width);
            let width = display_width(&text);
            let prefix = &resolved[..resolved.len() - 1];
            let mut spans = resolved_token_spans(
                prefix,
                state_icon,
                styles,
                p,
                max_width.saturating_sub(width + usize::from(!prefix.is_empty())),
                false,
            );
            let used = spans
                .iter()
                .map(|span| display_width(&span.content))
                .sum::<usize>();
            let padding = max_width
                .saturating_sub(used + width)
                .max(usize::from(used > 0));
            if padding > 0 {
                spans.push(Span::raw(" ".repeat(padding)));
            }
            spans.push(Span::styled(text, styles.state_text));
            return spans;
        }
    }
    let fixed_widths = resolved
        .iter()
        .map(|token| match token {
            ResolvedToken::StateIcon => display_width(state_icon.0),
            ResolvedToken::Branch(_) => display_width("\u{2387} "),
            ResolvedToken::GitStatus { ahead, behind } => {
                usize::from(*ahead > 0) * display_width(&format!("\u{2191}{ahead}"))
                    + usize::from(*behind > 0) * display_width(&format!("\u{2193}{behind}"))
                    + usize::from(*ahead > 0 && *behind > 0)
            }
            _ => 0,
        })
        .collect::<Vec<_>>();
    let flexible_widths = resolved
        .iter()
        .map(|token| match token {
            ResolvedToken::StateText(text)
            | ResolvedToken::Workspace(text)
            | ResolvedToken::Tab(text)
            | ResolvedToken::Pane(text)
            | ResolvedToken::Agent(text)
            | ResolvedToken::TerminalTitle(text)
            | ResolvedToken::Branch(text)
            | ResolvedToken::Custom(text) => display_width(text),
            _ => 0,
        })
        .collect::<Vec<_>>();
    let minimum_width = |active: &[bool]| {
        let indices = active
            .iter()
            .enumerate()
            .filter_map(|(index, active)| active.then_some(index))
            .collect::<Vec<_>>();
        let content = indices
            .iter()
            .map(|index| fixed_widths[*index] + usize::from(flexible_widths[*index] > 0))
            .sum::<usize>();
        let separators = indices
            .windows(2)
            .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
            .sum::<usize>();
        content + separators
    };
    let mut active = vec![true; resolved.len()];
    if minimum_width(&active) > max_width {
        for (index, width) in flexible_widths.iter().enumerate() {
            if *width > 0 {
                active[index] = false;
            }
        }
        for index in (0..resolved.len()).rev() {
            if flexible_widths[index] == 0 {
                continue;
            }
            active[index] = true;
            if minimum_width(&active) > max_width {
                active[index] = false;
            }
        }
    }
    let visible_indices = active
        .iter()
        .enumerate()
        .filter_map(|(index, active)| active.then_some(index))
        .collect::<Vec<_>>();
    let separator_width = visible_indices
        .windows(2)
        .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
        .sum::<usize>();
    let fixed_width = visible_indices
        .iter()
        .map(|index| fixed_widths[*index])
        .sum::<usize>();
    let mut budgets = flexible_widths
        .iter()
        .enumerate()
        .map(|(index, width)| usize::from(active[index] && *width > 0))
        .collect::<Vec<_>>();
    let minimum = budgets.iter().sum::<usize>();
    let mut remaining = max_width
        .saturating_sub(separator_width + fixed_width)
        .saturating_sub(minimum);
    while remaining > 0 {
        let mut grew = false;
        for (budget, width) in budgets.iter_mut().zip(&flexible_widths) {
            if *budget > 0 && *budget < *width {
                *budget += 1;
                remaining -= 1;
                grew = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut spans = Vec::new();
    for (position, index) in visible_indices.iter().copied().enumerate() {
        let token = &resolved[index];
        if position > 0 {
            let separator = tokens::separator(&resolved[visible_indices[position - 1]], token);
            let style = if separator == " " {
                Style::default()
            } else {
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(separator, style));
        }
        match token {
            ResolvedToken::StateIcon => {
                spans.push(Span::styled(state_icon.0.to_string(), state_icon.1))
            }
            ResolvedToken::StateText(text) => spans.push(Span::styled(
                truncate_end(text, budgets[index]),
                styles.state_text,
            )),
            ResolvedToken::Workspace(text) => spans.push(Span::styled(
                truncate_end(text, budgets[index]),
                styles.workspace,
            )),
            ResolvedToken::Tab(text) | ResolvedToken::Pane(text) | ResolvedToken::Agent(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    styles.secondary,
                ))
            }
            ResolvedToken::Branch(text) => {
                spans.push(Span::styled("\u{2387} ", styles.secondary));
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    styles.secondary,
                ));
            }
            ResolvedToken::GitStatus { ahead, behind } => {
                if *ahead > 0 {
                    spans.push(Span::styled(
                        format!("\u{2191}{ahead}"),
                        Style::default().fg(p.green),
                    ));
                }
                if *ahead > 0 && *behind > 0 {
                    spans.push(Span::raw(" "));
                }
                if *behind > 0 {
                    spans.push(Span::styled(
                        format!("\u{2193}{behind}"),
                        Style::default().fg(p.red),
                    ));
                }
            }
            ResolvedToken::TerminalTitle(text) | ResolvedToken::Custom(text) => spans.push(
                Span::styled(truncate_end(text, budgets[index]), styles.custom),
            ),
        }
    }
    spans
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

        let row_style = if selected {
            Style::default().bg(workspace_selection_background(p, is_active))
        } else if is_dragged {
            Style::default().bg(p.surface1)
        } else if is_active {
            Style::default().bg(p.active_row_bg)
        } else {
            Style::default()
        };
        if highlighted {
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(row_style);
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let display_label = if card.indented {
            grouped_child_display_label(&label, ws.branch().as_deref(), ws.custom_name.is_some())
        } else {
            label
        };
        let parent_group = (!card.indented)
            .then(|| workspace_parent_group_state(app, i))
            .flatten();
        let (display_state, display_seen) = parent_group
            .as_ref()
            .filter(|(_, collapsed)| *collapsed)
            .map(|(key, _)| space_aggregate_state(app, key))
            .unwrap_or((agg_state, agg_seen));
        let state_icon =
            workspace_state_icon(display_state, display_seen, app.status_indicators, p);
        let branch_style = Style::default().fg(if selected || is_active {
            p.mauve
        } else {
            p.overlay0
        });
        let styles = TokenStyles {
            state_text: Style::default()
                .fg(state_label_color(display_state, display_seen, p))
                .add_modifier(Modifier::DIM),
            workspace: name_style,
            secondary: branch_style,
            custom: branch_style,
        };
        let values = ws.metadata_tokens.values();
        let rows = tokens::space_rows(
            &app.sidebar_spaces,
            SpaceTokenContext {
                workspace: &display_label,
                branch: ws.branch().as_deref(),
                state_text: state_label(display_state, display_seen),
                ahead_behind: ws.git_ahead_behind(),
                tokens: &values,
                suppress_git_details: card.indented,
            },
        );
        for (row_index, resolved) in rows.iter().take(row_height as usize).enumerate() {
            let y = row_y.saturating_add(row_index as u16);
            if y >= list_bottom {
                break;
            }
            let mut spans = Vec::new();
            if row_index == 0 {
                if card.indented {
                    spans.push(Span::raw("   "));
                } else if let Some((_, collapsed)) = parent_group.as_ref() {
                    spans.push(Span::styled(
                        if *collapsed { "▸" } else { "▾" },
                        Style::default().fg(p.accent),
                    ));
                    spans.push(Span::raw(" "));
                } else {
                    spans.push(Span::raw(" "));
                }
            } else if card.indented {
                spans.push(Span::raw("     "));
            } else if matches!(resolved.first(), Some(ResolvedToken::Branch(_))) {
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::raw("   "));
            }
            let prefix_width = spans
                .iter()
                .map(|span| display_width(&span.content))
                .sum::<usize>();
            spans.extend(resolved_token_spans(
                resolved,
                state_icon,
                styles,
                p,
                (card.rect.width as usize).saturating_sub(prefix_width),
                false,
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(row_style),
                Rect::new(card.rect.x, y, card.rect.width, 1),
            );
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
    let control_label = active_agent_view_label(app)
        .unwrap_or_else(|| agent_panel_sort_label(app.agent_panel_sort));
    let toggle_rect = agent_panel_header_label_rect(area, control_label);
    if toggle_rect != Rect::default() {
        let color = if app.agent_view_override.is_some() {
            p.accent
        } else {
            p.overlay0
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                control_label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
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
    if details.is_empty() && app.agent_view_override.is_some() {
        frame.render_widget(
            Paragraph::new(" no matching agents")
                .style(Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)),
            Rect::new(body.x, body.y, body.width, 1),
        );
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
            AgentVisibleRow::Child {
                entry_idx,
                y,
                height,
                last,
            } => {
                let Some(detail) = details.get(entry_idx) else {
                    continue;
                };
                let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
                let (icon, icon_style) =
                    sidebar_agent_icon(detail.state, detail.seen, app.status_indicators, p);
                let label_color = state_label_color(detail.state, detail.seen, p);
                let connector = if last { "└─" } else { "├─" };

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

                let styles = TokenStyles {
                    state_text: status_style,
                    workspace: name_style,
                    secondary: name_style,
                    custom: name_style,
                };
                let resolved = agent_resolved_rows(app, detail);
                for line in 0..height {
                    let row_y = y.saturating_add(line);
                    let mut spans = if line == 0 {
                        vec![
                            Span::styled(connector, Style::default().fg(p.overlay0)),
                            Span::raw(" "),
                        ]
                    } else {
                        vec![Span::styled(
                            if last { "   " } else { "│  " },
                            Style::default().fg(p.overlay0),
                        )]
                    };
                    if let Some(tokens) = resolved.get(line as usize) {
                        spans.extend(resolved_token_spans(
                            tokens,
                            (icon, icon_style),
                            styles,
                            p,
                            body_width.saturating_sub(3),
                            true,
                        ));
                    }
                    if is_active {
                        frame
                            .buffer_mut()
                            .set_style(Rect::new(body.x, row_y, body.width, 1), row_style);
                    }
                    frame.render_widget(
                        Paragraph::new(Line::from(spans)).style(row_style),
                        Rect::new(body.x, row_y, body.width, 1),
                    );
                }
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
        area.x
            + if area.width == 2 {
                1
            } else {
                area.width.saturating_sub(2)
            },
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

    fn m828e_presentation_owner() -> crate::app::App {
        let config: crate::config::Config =
            toml::from_str("onboarding = false\n[ui.sidebar.agents]\nrows = [[\"$task\"]]\n")
                .unwrap();
        let mut app = crate::app::App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("retirement")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane)
            .unwrap()
            .seen = true;
        app.next_resize_poll = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        let ws = &app.state.workspaces[0];
        let target = crate::workspace::public_pane_id_for_number(
            &ws.id,
            ws.public_pane_number(pane).unwrap(),
        );
        let request = serde_json::from_value(serde_json::json!({"id":"retirement", "method":"pane.report_metadata", "params":{
            "pane_id":target, "source":"user:retirement", "title":"TITLE", "display_agent":"DISPLAY", "state_labels":{"idle":"STATE-LABEL"}, "custom_status":"OLD-STATUS", "tokens":{"task":"TOKEN-ONLY"}
        }})).unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request)).unwrap();
        assert_eq!(
            response,
            serde_json::json!({"id":"retirement", "result":{"type":"ok"}})
        );
        app
    }

    #[test]
    fn m828e_desktop_token_replacement_is_explicit_and_repeatable() {
        let mut owner = m828e_presentation_owner();
        let app = &mut owner.state;
        let area = ratatui::layout::Rect::new(0, 0, 100, 36);
        crate::ui::compute_view(app, area);
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Desktop);
        let entries = agent_panel_entries(app);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].tokens.get("task").map(String::as_str),
            Some("TOKEN-ONLY")
        );
        assert_eq!(entries[0].agent_label.as_deref(), Some("DISPLAY"));
        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 36)).unwrap();
        screen.draw(|frame| crate::ui::render(app, frame)).unwrap();
        let text: String = screen
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("TOKEN-ONLY"), "{text}");
        assert!(!text.contains("OLD-STATUS"), "{text}");
        assert!(
            !text.contains("STATE-LABEL"),
            "explicit custom-only row: {text}"
        );
        let baseline = screen.backend().buffer().clone();
        screen.draw(|frame| crate::ui::render(app, frame)).unwrap();
        assert_eq!(screen.backend().buffer(), &baseline);
    }

    #[test]
    fn m828d2_token_render_cost_is_observed_on_bounded_geometry() {
        const AREA: Rect = Rect::new(0, 0, 120, 48);
        const WARMUPS: usize = 16;
        const SAMPLES: usize = 64;
        let mut observations = Vec::new();
        for count in [1, 15] {
            for row_count in [1u16, 4, 16] {
                for gap in [0, 3] {
                    let config = crate::config::Config {
                        onboarding: Some(false),
                        ..crate::config::Config::default()
                    };
                    let mut owner = crate::app::App::new(
                        &config,
                        true,
                        None,
                        tokio::sync::mpsc::unbounded_channel().1,
                        crate::api::EventHub::default(),
                    );
                    let app = &mut owner.state;
                    app.workspaces = (0..count)
                        .map(|index| {
                            let mut ws = Workspace::test_new(&format!("bench-{index:02}"));
                            ws.tabs[0].set_custom_name(format!("TAB-{index:02}"));
                            ws.cached_git_branch = None;
                            ws
                        })
                        .collect();
                    app.ensure_test_terminals();
                    for (index, ws) in app.workspaces.iter_mut().enumerate() {
                        let values = (0..row_count)
                            .map(|row| (format!("r{row}"), Some(format!("S{index:02}R{row:02}"))))
                            .collect();
                        assert!(ws
                            .metadata_tokens
                            .patch(values, None, std::time::Instant::now()));
                        let pane = ws.tabs[0].root_pane;
                        let id = ws.terminal_id(pane).unwrap();
                        let terminal = app.terminals.get_mut(id).unwrap();
                        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
                        let values = (0..row_count)
                            .map(|row| (format!("r{row}"), Some(format!("A{index:02}R{row:02}"))))
                            .collect();
                        assert!(terminal.metadata_tokens.patch(
                            values,
                            None,
                            std::time::Instant::now()
                        ));
                        for row in 0..row_count {
                            assert_eq!(
                                ws.metadata_tokens.values()[&format!("r{row}")],
                                format!("S{index:02}R{row:02}")
                            );
                            assert_eq!(
                                terminal.metadata_tokens.values()[&format!("r{row}")],
                                format!("A{index:02}R{row:02}")
                            );
                        }
                    }
                    app.sidebar_spaces.rows = (0..row_count)
                        .map(|row| {
                            vec![
                                crate::config::SpaceSidebarToken::Workspace,
                                crate::config::SpaceSidebarToken::Custom(format!("r{row}")),
                            ]
                        })
                        .collect();
                    app.sidebar_agents.rows = (0..row_count)
                        .map(|row| {
                            vec![
                                crate::config::AgentSidebarToken::Agent,
                                crate::config::AgentSidebarToken::Custom(format!("r{row}")),
                            ]
                        })
                        .collect();
                    app.sidebar_agents.rows_by_agent.clear();
                    app.sidebar_spaces.row_gap = gap;
                    app.sidebar_agents.row_gap = gap;
                    app.sidebar_width = 40;
                    app.sidebar_max_width = 40;
                    app.agent_panel_sort = AgentPanelSort::Spaces;
                    app.active = Some(0);
                    app.selected = 0;
                    app.mode = Mode::Terminal;
                    app.update_available = None;
                    assert_eq!(app.terminals.len(), count);
                    let entries = agent_panel_entries(app);
                    assert_eq!(entries.len(), count);
                    assert_eq!(agent_row_heights(app, &entries), vec![row_count; count]);
                    for ws in &app.workspaces {
                        assert_eq!(workspace_row_height(app, ws, false), row_count);
                    }
                    crate::ui::compute_view(app, AREA);
                    assert_eq!(app.view.layout, crate::app::state::ViewLayout::Desktop);
                    assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 40, 48));
                    let (_, agents) =
                        expanded_sidebar_sections(app.view.sidebar_rect, app.sidebar_section_split);
                    let cards = app.view.workspace_card_areas.clone();
                    let rows = agent_visible_rows(app, agents);
                    assert_eq!(
                        (cards[0].ws_idx, cards[0].rect.y, cards[0].rect.height),
                        (0, 2, row_count)
                    );
                    assert!(rows.iter().any(|row| matches!(row, AgentVisibleRow::Child {
                        entry_idx: 0, y: 28, height, ..
                    } if *height == row_count)));
                    let mut screen =
                        Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
                    for _ in 0..WARMUPS {
                        screen.draw(|frame| crate::ui::render(app, frame)).unwrap();
                    }
                    let baseline = screen.backend().buffer().clone();
                    let text = |y| {
                        (0..39)
                            .map(|x| baseline[(x, y)].symbol())
                            .collect::<String>()
                    };
                    for row in 0..row_count {
                        assert!(text(2 + row).contains(&format!("S00R{row:02}")));
                        assert!(text(28 + row).contains(&format!("A00R{row:02}")));
                    }
                    let mut samples = Vec::with_capacity(SAMPLES);
                    for _ in 0..SAMPLES {
                        let mut elapsed = None;
                        screen
                            .draw(|frame| {
                                let start = std::time::Instant::now();
                                crate::ui::render(app, frame);
                                elapsed = Some(start.elapsed().as_nanos());
                            })
                            .unwrap();
                        samples.push(elapsed.unwrap());
                        assert_eq!(screen.backend().buffer(), &baseline);
                        assert_eq!(app.view.workspace_card_areas, cards);
                        assert_eq!(agent_visible_rows(app, agents), rows);
                    }
                    assert_eq!(samples.len(), SAMPLES);
                    let mut sorted = samples.clone();
                    sorted.sort_unstable();
                    observations.push(serde_json::json!({
                        "panes": count, "rows": row_count, "tokens_per_row": 2, "gap": gap,
                        "width": AREA.width, "height": AREA.height, "sidebar_width": 40,
                        "warmups": WARMUPS, "sample_count": SAMPLES, "samples_ns": samples,
                        "median_ns": sorted[SAMPLES / 2], "p95_ns": sorted[(SAMPLES * 95).div_ceil(100) - 1],
                        "max_ns": sorted[SAMPLES - 1], "debug_assertions": cfg!(debug_assertions),
                        "visible_cards": cards.len(),
                        "visible_children": rows.iter().filter(|r| matches!(r, AgentVisibleRow::Child { .. })).count(),
                        "interval": "public_render_only_inside_draw_closure",
                        "route": "AppState with one attached terminal state per workspace; TestBackend; no live PTY",
                        "threshold": null, "parent_delta": null
                    }));

                    app.sidebar_min_width = 1;
                    app.sidebar_width = 2;
                    crate::ui::compute_view(app, AREA);
                    assert_eq!(app.view.sidebar_rect.width, 2);
                    assert_eq!(app.view.workspace_card_areas[0].rect.width, 1);
                    screen.draw(|frame| crate::ui::render(app, frame)).unwrap();
                    assert_eq!(screen.backend().buffer()[(0, 28)].symbol(), "\u{2514}");
                    app.sidebar_collapsed = true;
                    app.sidebar_collapsed_mode = crate::config::SidebarCollapsedModeConfig::Hidden;
                    crate::ui::compute_view(app, AREA);
                    assert_eq!(app.view.sidebar_rect.width, 0);
                    assert_eq!(app.view.terminal_area.width, 120);
                    assert!(app.view.workspace_card_areas.is_empty());
                    assert_eq!(
                        app.workspaces[0].tab_display_name(0).as_deref(),
                        Some("TAB-00")
                    );
                    screen.draw(|frame| crate::ui::render(app, frame)).unwrap();
                    let text: String = screen
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|c| c.symbol())
                        .collect();
                    assert!(text.contains("TAB-00"));
                    assert!(!text.contains("S00R00") && !text.contains("A00R00"));
                    app.sidebar_collapsed = false;
                    app.mode = Mode::Navigate;
                    let mobile_area = Rect::new(0, 0, 44, 40);
                    crate::ui::compute_view(app, mobile_area);
                    assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);
                    let mut mobile = Terminal::new(TestBackend::new(44, 40)).unwrap();
                    mobile.draw(|frame| crate::ui::render(app, frame)).unwrap();
                    let text: String = mobile
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|c| c.symbol())
                        .collect();
                    assert!(text.contains("bench-00") && text.to_lowercase().contains("claude"));
                    assert!(!text.contains("S00R00") && !text.contains("A00R00"));
                }
            }
        }
        assert_eq!(observations.len(), 12);
        for observation in observations {
            println!("M8_28D2_RENDER {observation}");
        }
    }

    #[test]
    fn m828d2_token_occurrences_preserve_fork_glyphs_and_backgrounds() {
        use crate::config::{AgentSidebarToken as A, SpaceSidebarToken as S};
        let cases = [
            (
                AgentState::Unknown,
                true,
                None,
                "idle",
                "\u{25cc}",
                "\u{b7}",
            ),
            (
                AgentState::Working,
                true,
                None,
                "working",
                "\u{25cf}",
                "\u{25d0}",
            ),
            (
                AgentState::Idle,
                false,
                None,
                "done",
                "\u{25cb}",
                "\u{2713}",
            ),
            (AgentState::Idle, true, None, "idle", "\u{25cb}", "\u{25cb}"),
            (
                AgentState::Blocked,
                true,
                None,
                "blocked",
                "\u{25c9}",
                "\u{d7}",
            ),
            (
                AgentState::Working,
                true,
                Some("waiting"),
                "working",
                "\u{25cf}",
                "\u{25d0}",
            ),
            (
                AgentState::Idle,
                true,
                Some("stopped"),
                "idle",
                "\u{25cb}",
                "\u{25cb}",
            ),
        ];
        let mut combinations = 0;
        for (state, seen, custom_state, space_status, dots, symbols) in cases {
            for indicator in [StatusIndicatorStyle::Dots, StatusIndicatorStyle::Symbols] {
                for context in [
                    "inactive",
                    "active",
                    "selected",
                    "drag",
                    "selected-drag",
                    "selected-reset",
                ] {
                    let mut app = AppState::test_new();
                    app.palette.sidebar_bg = Color::Rgb(11, 22, 33);
                    app.palette.active_row_bg = Color::Rgb(44, 55, 66);
                    app.palette.selection_bg = if context == "selected-reset" {
                        Color::Reset
                    } else {
                        Color::Rgb(77, 88, 99)
                    };
                    app.palette.surface1 = Color::Rgb(100, 110, 120);
                    app.palette.surface_dim = Color::Rgb(9, 8, 7);
                    app.status_indicators = indicator;
                    let selected = context.starts_with("selected");
                    let active =
                        context == "active" || context == "selected" || context == "selected-reset";
                    let dragged = context == "drag" || context == "selected-drag";
                    app.active = active.then_some(0);
                    app.selected = 0;
                    app.mode = if selected {
                        Mode::Navigate
                    } else {
                        Mode::Terminal
                    };
                    app.mouse_capture = false;
                    app.workspaces = vec![Workspace::test_new("WSPACE")];
                    app.workspaces[0].tabs[0].set_custom_name("GROUP".into());
                    app.workspaces[0].cached_git_branch = Some("topic".into());
                    app.workspaces[0].cached_git_ahead_behind = Some((2, 1));
                    assert!(app.workspaces[0].metadata_tokens.patch(
                        std::collections::HashMap::from([(
                            "tag".into(),
                            Some("SPACE-CUSTOM".into())
                        )]),
                        None,
                        std::time::Instant::now()
                    ));
                    app.ensure_test_terminals();
                    let pane = app.workspaces[0].tabs[0].root_pane;
                    app.workspaces[0].tabs[0].panes.get_mut(&pane).unwrap().seen = seen;
                    let id = app.workspaces[0].tabs[0].panes[&pane]
                        .attached_terminal_id
                        .clone();
                    let terminal = app.terminals.get_mut(&id).unwrap();
                    terminal.set_detected_state(Some(Agent::Claude), state);
                    if let Some(label) = custom_state {
                        terminal.set_agent_metadata(crate::terminal::AgentMetadataReport {
                            source: "d2-palette".into(),
                            agent_label: None,
                            applies_to_source: None,
                            title: None,
                            display_agent: None,

                            state_labels: std::collections::HashMap::from([(
                                space_status.into(),
                                label.into(),
                            )]),
                            clear_title: false,
                            clear_display_agent: false,

                            clear_state_labels: false,
                            ttl: None,
                            seq: Some(1),
                        });
                    }
                    assert!(terminal.metadata_tokens.patch(
                        std::collections::HashMap::from([(
                            "tag".into(),
                            Some("AGENT-CUSTOM".into())
                        )]),
                        None,
                        std::time::Instant::now()
                    ));
                    assert_eq!(terminal.state, state);
                    assert_eq!(terminal.effective_known_agent(), Some(Agent::Claude));
                    app.sidebar_spaces.rows = vec![
                        vec![
                            S::StateIcon,
                            S::Workspace,
                            S::StateText,
                            S::Custom("tag".into()),
                            S::StateIcon,
                            S::GitStatus,
                        ],
                        vec![
                            S::Branch,
                            S::Workspace,
                            S::Custom("tag".into()),
                            S::GitStatus,
                        ],
                    ];
                    app.sidebar_agents.rows = vec![
                        vec![
                            A::StateIcon,
                            A::Workspace,
                            A::Agent,
                            A::Custom("tag".into()),
                            A::StateIcon,
                            A::StateText,
                        ],
                        vec![
                            A::Workspace,
                            A::Agent,
                            A::Custom("tag".into()),
                            A::StateIcon,
                            A::StateText,
                        ],
                    ];
                    if dragged {
                        app.drag = Some(crate::app::state::DragState {
                            target: crate::app::state::DragTarget::WorkspaceReorder {
                                source_id: 0,
                                source_ws_idx: 0,
                                insert_idx: None,
                            },
                        });
                    }
                    let entries = agent_panel_entries(&app);
                    assert_eq!(entries.len(), 1);
                    assert_eq!((entries[0].state, entries[0].seen), (state, seen));
                    assert_eq!(entries[0].agent_label.as_deref(), Some("claude"));
                    assert_eq!(entries[0].tokens["tag"], "AGENT-CUSTOM");
                    let label = custom_state.unwrap_or(space_status);
                    assert!(
                        matches!(agent_resolved_rows(&app, &entries[0])[0].last(), Some(ResolvedToken::StateText(text)) if text == label)
                    );
                    let full = Rect::new(0, 0, 80, 30);
                    app.view.sidebar_rect = full;
                    app.view.workspace_card_areas = compute_workspace_card_areas(&app, full);
                    assert_eq!(
                        app.view.workspace_card_areas[0].rect,
                        Rect::new(0, 2, 79, 2)
                    );
                    let agents = expanded_sidebar_sections(full, app.sidebar_section_split).1;
                    let child = agent_visible_rows(&app, agents)
                        .into_iter()
                        .find_map(|r| match r {
                            AgentVisibleRow::Child { y, height, .. } => Some((y, height)),
                            _ => None,
                        })
                        .unwrap();
                    assert_eq!(child, (19, 2));
                    let mut screen = Terminal::new(TestBackend::new(80, 30)).unwrap();
                    screen
                        .draw(|frame| {
                            render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, full)
                        })
                        .unwrap();
                    let buffer = screen.backend().buffer();
                    let row = |y| (0..79).map(|x| buffer[(x, y)].symbol()).collect::<String>();
                    let first = row(2);
                    assert!(
                        first.contains("WSPACE")
                            && first.contains("SPACE-CUSTOM")
                            && first.contains("\u{2191}2 \u{2193}1")
                    );
                    let glyph = if indicator == StatusIndicatorStyle::Symbols {
                        symbols
                    } else {
                        dots
                    };
                    assert_eq!(
                        buffer[(1, 2)].symbol(),
                        glyph,
                        "space glyph {context}/{state:?}/{indicator:?}"
                    );
                    assert_eq!(first.trim_end(), format!(" {glyph} WSPACE \u{b7} {space_status} \u{b7} SPACE-CUSTOM \u{b7} {glyph} \u{2191}2 \u{2193}1"));
                    assert_eq!(
                        row(3).trim_end(),
                        " \u{2387} topic \u{b7} WSPACE \u{b7} SPACE-CUSTOM \u{2191}2 \u{2193}1"
                    );
                    let space_bg = if selected {
                        if active && context == "selected-reset" {
                            app.palette.active_row_bg
                        } else {
                            app.palette.selection_bg
                        }
                    } else if dragged {
                        app.palette.surface1
                    } else if active {
                        app.palette.active_row_bg
                    } else {
                        app.palette.sidebar_bg
                    };
                    for y in [2, 3] {
                        for x in 0..79 {
                            assert_eq!(
                                buffer[(x, y)].bg,
                                space_bg,
                                "space occurrence/background {context}/{x}/{y}"
                            );
                        }
                        let text = row(y);
                        let (at, _) = text.match_indices("WSPACE").next().unwrap();
                        let x = display_width(&text[..at]) as u16;
                        let style = buffer[(x, y)].style();
                        assert_eq!(
                            style.fg,
                            Some(if selected || active || dragged {
                                app.palette.text
                            } else {
                                app.palette.subtext0
                            })
                        );
                        assert_eq!(
                            style.add_modifier.contains(Modifier::BOLD),
                            selected || active || dragged
                        );
                        assert!(!style.add_modifier.contains(Modifier::DIM));
                        for (at, _) in text.match_indices("\u{2191}2") {
                            assert_eq!(
                                buffer[(display_width(&text[..at]) as u16, y)].fg,
                                app.palette.green
                            );
                        }
                        for (at, _) in text.match_indices("\u{2193}1") {
                            assert_eq!(
                                buffer[(display_width(&text[..at]) as u16, y)].fg,
                                app.palette.red
                            );
                        }
                    }
                    let agent_bg = if active {
                        app.palette.active_row_bg
                    } else {
                        app.palette.sidebar_bg
                    };
                    for y in [19, 20] {
                        let text = row(y);
                        assert!(
                            text.contains("WSPACE")
                                && text.contains("claude")
                                && text.contains("AGENT-CUSTOM")
                        );
                        assert!(text.ends_with(label));
                        for x in 0..79 {
                            assert_eq!(
                                buffer[(x, y)].bg,
                                agent_bg,
                                "agent occurrence/background {context}/{x}/{y}"
                            );
                        }
                        for marker in ["WSPACE", "claude", "AGENT-CUSTOM"] {
                            let (at, _) = text.match_indices(marker).next().unwrap();
                            let style = buffer[(display_width(&text[..at]) as u16, y)].style();
                            assert_eq!(
                                style.fg,
                                Some(if active {
                                    app.palette.text
                                } else {
                                    app.palette.subtext0
                                })
                            );
                            assert_eq!(style.add_modifier.contains(Modifier::BOLD), active);
                            assert!(!style.add_modifier.contains(Modifier::DIM));
                        }
                    }
                    assert_eq!(buffer[(3, 19)].symbol(), glyph);
                    assert_eq!(
                        (0..3).map(|x| buffer[(x, 20)].symbol()).collect::<String>(),
                        "   "
                    );
                    let expected_color = match (state, seen) {
                        (AgentState::Blocked, _) => app.palette.red,
                        (AgentState::Working, _) => app.palette.yellow,
                        (AgentState::Idle, false) => app.palette.teal,
                        (AgentState::Idle, true) => app.palette.green,
                        (AgentState::Unknown, _) => app.palette.overlay0,
                    };
                    assert_eq!(buffer[(3, 19)].fg, expected_color);
                    assert_eq!(buffer[(1, 2)].fg, expected_color);
                    combinations += 1;
                }
            }
        }
        assert_eq!(combinations, 84);
    }

    #[test]
    fn m828d2_heterogeneous_grouped_placement_obeys_admission_and_gaps() {
        let entries: Vec<_> = [0, 0, 1, 2, 2]
            .into_iter()
            .enumerate()
            .map(|(index, group)| agent_entry(0, group, "claude", index as u32 + 1))
            .collect();
        let mut cases = 0;
        let mut populated = 0;
        for heights in [[1u16, 2, 4, 2, 1], [3, 1, 2, 4, 1]] {
            for gap in [0u16, 1, 3, u16::MAX] {
                for width in [0u16, 1, 32] {
                    for height in [0u16, 1, 2, 3, 5, 9, 15] {
                        for (x, y) in [(2, 3), (0, u16::MAX - 18)] {
                            for scroll in [0, 1, 2, 4, 5, usize::MAX] {
                                let body = Rect::new(x, y, width, height);
                                let mut expected = Vec::new();
                                let mut cursor = usize::from(body.y);
                                let end = cursor + usize::from(body.height);
                                let mut previous = None;
                                if body.width > 0 && body.height >= 2 {
                                    for (index, entry) in entries.iter().enumerate().skip(scroll) {
                                        let group = (entry.ws_idx, entry.tab_idx);
                                        let header = previous != Some(group);
                                        let prefix = if previous.is_some() {
                                            usize::from(gap)
                                        } else {
                                            0
                                        };
                                        let header_y = cursor + prefix;
                                        let child_y = header_y + usize::from(header);
                                        let child_height = usize::from(heights[index])
                                            .max(1)
                                            .min(usize::from(body.height) - 1);
                                        if child_y + child_height > end {
                                            break;
                                        }
                                        if header {
                                            expected.push(AgentVisibleRow::GroupHeader {
                                                entry_idx: index,
                                                y: header_y as u16,
                                            });
                                        }
                                        expected.push(AgentVisibleRow::Child {
                                            entry_idx: index,
                                            y: child_y as u16,
                                            height: child_height as u16,
                                            last: entries.get(index + 1).is_none_or(|next| {
                                                (next.ws_idx, next.tab_idx) != group
                                            }),
                                        });
                                        cursor = child_y + child_height;
                                        previous = Some(group);
                                    }
                                }
                                let rows = agent_visible_rows_for_entries(
                                    &entries, body, scroll, gap, &heights,
                                );
                                assert_eq!(
                                    rows, expected,
                                    "heights={heights:?} gap={gap} body={body:?} scroll={scroll}"
                                );
                                let mut previous_bottom = usize::from(body.y);
                                for (index, row) in rows.iter().enumerate() {
                                    match row {
                                        AgentVisibleRow::GroupHeader { entry_idx, y } => {
                                            assert!(usize::from(*y) >= previous_bottom);
                                            assert!(
                                                matches!(rows.get(index + 1), Some(AgentVisibleRow::Child { entry_idx: child, y: child_y, .. })
                                                if child == entry_idx && usize::from(*child_y) == usize::from(*y) + 1)
                                            );
                                            previous_bottom = usize::from(*y) + 1;
                                        }
                                        AgentVisibleRow::Child { y, height, .. } => {
                                            assert!(*height > 0);
                                            assert!(usize::from(*y) >= previous_bottom);
                                            previous_bottom =
                                                usize::from(*y) + usize::from(*height);
                                            assert!(previous_bottom <= end);
                                        }
                                    }
                                }
                                if let Some(AgentVisibleRow::GroupHeader { y, .. }) = rows.first() {
                                    assert_eq!(*y, body.y, "no leading gap");
                                    populated += 1;
                                }
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 2016);
        assert!(populated > 0);
        assert!(agent_visible_rows_for_entries(&[], Rect::new(0, 0, 1, 8), 0, 0, &[]).is_empty());

        use crate::config::AgentSidebarToken as A;
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("ordered");
        workspace.test_split(Direction::Horizontal);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.sidebar_agents.rows = vec![
            vec![A::Agent],
            vec![A::Custom("a".into())],
            vec![A::Custom("b".into())],
        ];
        let panes = app.workspaces[0].tabs[0].layout.pane_ids();
        assert_eq!(panes.len(), 2);
        for (index, pane) in panes.iter().enumerate() {
            let id = app.workspaces[0].tabs[0].panes[pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&id).unwrap();
            let state = if index == 0 {
                AgentState::Idle
            } else {
                AgentState::Working
            };
            terminal.set_detected_state(Some(Agent::Claude), state);
            if index == 1 {
                assert!(terminal.metadata_tokens.patch(
                    std::collections::HashMap::from([
                        ("a".into(), Some("A".into())),
                        ("b".into(), Some("B".into()))
                    ]),
                    None,
                    std::time::Instant::now()
                ));
                assert_eq!(terminal.metadata_tokens.values().len(), 2);
            } else {
                assert!(terminal.metadata_tokens.values().is_empty());
            }
            assert_eq!(terminal.state, state);
        }
        app.agent_panel_sort = AgentPanelSort::Spaces;
        let traversal = agent_panel_entries(&app);
        assert_eq!(
            traversal.iter().map(|e| e.pane_id).collect::<Vec<_>>(),
            panes
        );
        assert_eq!(agent_row_heights(&app, &traversal), vec![1, 3]);
        app.agent_panel_sort = AgentPanelSort::Priority;
        let priority = agent_panel_entries(&app);
        assert_eq!(
            priority.iter().map(|e| e.pane_id).collect::<Vec<_>>(),
            vec![panes[1], panes[0]]
        );
        assert_eq!(agent_row_heights(&app, &priority), vec![3, 1]);
        let children: Vec<_> = agent_visible_rows(&app, Rect::new(0, 0, 30, 16))
            .into_iter()
            .filter_map(|row| match row {
                AgentVisibleRow::Child {
                    entry_idx,
                    y,
                    height,
                    ..
                } => Some((priority[entry_idx].pane_id, y, height)),
                _ => None,
            })
            .collect();
        assert_eq!(children, vec![(panes[1], 4, 3), (panes[0], 7, 1)]);
    }

    #[test]
    fn m828d2_empty_and_oversized_agent_rows_keep_header_child_atomicity() {
        use crate::config::AgentSidebarToken as A;
        for kind in ["empty", "empty-override", "missing", "oversized"] {
            let mut app = AppState::test_new();
            let mut workspace = Workspace::test_new("atomic");
            workspace.test_split(Direction::Horizontal);
            app.workspaces = vec![workspace];
            app.ensure_test_terminals();
            app.sidebar_agents.row_gap = 0;
            app.sidebar_agents.rows = match kind {
                "empty" => vec![],
                "missing" => vec![vec![A::Custom("missing".into())]],
                _ => vec![vec![A::Agent]],
            };
            if kind == "empty-override" {
                app.sidebar_agents
                    .rows_by_agent
                    .insert("claude".into(), vec![]);
            }
            if kind == "oversized" {
                app.sidebar_agents
                    .rows_by_agent
                    .insert("claude".into(), vec![vec![A::Agent]; 16]);
            }
            let panes = app.workspaces[0].tabs[0].layout.pane_ids();
            assert_eq!(panes.len(), 2);
            for (index, pane) in panes.iter().enumerate() {
                let id = app.workspaces[0].tabs[0].panes[pane]
                    .attached_terminal_id
                    .clone();
                let terminal = app.terminals.get_mut(&id).unwrap();
                let agent = if kind == "oversized" && index == 0 {
                    Agent::Codex
                } else {
                    Agent::Claude
                };
                terminal.set_detected_state(Some(agent), AgentState::Working);
                assert_eq!(terminal.effective_known_agent(), Some(agent));
                if kind == "missing" {
                    assert!(terminal.metadata_tokens.patch(
                        std::collections::HashMap::from([(
                            "missing".into(),
                            Some("VISIBLE".into())
                        )]),
                        None,
                        std::time::Instant::now()
                    ));
                }
            }
            if kind == "missing" {
                for entry in agent_panel_entries(&app) {
                    assert_eq!(
                        agent_resolved_rows(&app, &entry),
                        vec![vec![ResolvedToken::Custom("VISIBLE".into())]]
                    );
                }
                for terminal in app.terminals.values_mut() {
                    assert!(terminal.metadata_tokens.patch(
                        std::collections::HashMap::from([("missing".into(), None)]),
                        None,
                        std::time::Instant::now()
                    ));
                    assert!(terminal.metadata_tokens.values().is_empty());
                }
            }
            let entries = agent_panel_entries(&app);
            let heights = agent_row_heights(&app, &entries);
            assert_eq!(
                heights,
                if kind == "oversized" {
                    vec![1, 16]
                } else {
                    vec![1, 1]
                }
            );
            if kind != "oversized" {
                assert!(entries
                    .iter()
                    .all(|entry| agent_resolved_rows(&app, entry).is_empty()));
            }
            let positive = agent_visible_rows(&app, Rect::new(0, 0, 30, 24));
            let positive_children: Vec<_> = positive
                .iter()
                .filter_map(|row| match row {
                    AgentVisibleRow::Child {
                        entry_idx, height, ..
                    } => Some((*entry_idx, *height)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                positive_children,
                vec![(0, 1), (1, if kind == "oversized" { 16 } else { 1 })]
            );
            for width in [0u16, 1, 30] {
                for body_height in [0u16, 1, 2, 3, 5, 18] {
                    let area = Rect::new(0, 0, width, body_height + 3);
                    let body = agent_panel_body_rect(area, false);
                    for scroll in [0usize, 1] {
                        let rows =
                            agent_visible_rows_for_entries(&entries, body, scroll, 0, &heights);
                        if width == 0 || body_height < 2 {
                            assert!(rows.is_empty());
                            continue;
                        }
                        assert!(
                            matches!(rows.first(), Some(AgentVisibleRow::GroupHeader { entry_idx, y: 3 }) if *entry_idx == scroll)
                        );
                        assert!(
                            matches!(rows.get(1), Some(AgentVisibleRow::Child { entry_idx, y: 4, height, .. })
                            if *entry_idx == scroll && *height == heights[scroll].min(body_height - 1))
                        );
                        for row in rows {
                            if let AgentVisibleRow::Child {
                                entry_idx,
                                y,
                                height,
                                ..
                            } = row
                            {
                                assert_eq!(height, heights[entry_idx].min(body_height - 1));
                                assert!(y + height <= body.bottom());
                            }
                        }
                    }
                    if width > 0 && body_height >= 2 {
                        app.agent_panel_scroll = usize::MAX;
                        let target_scroll = agent_panel_scroll_for_target(&app, area, 1);
                        app.agent_panel_scroll = target_scroll;
                        let rows = agent_visible_rows(&app, area);
                        assert!(rows.iter().any(|row| matches!(row, AgentVisibleRow::Child { entry_idx: 1, y, height, .. }
                            if *height == heights[1].min(body_height - 1) && *y + *height <= body.bottom())));
                    }
                }
            }
            if kind == "oversized" {
                let body = Rect::new(0, 3, 30, 5);
                assert_eq!(
                    agent_visible_rows_for_entries(&entries, body, 0, 0, &heights),
                    vec![
                        AgentVisibleRow::GroupHeader { entry_idx: 0, y: 3 },
                        AgentVisibleRow::Child {
                            entry_idx: 0,
                            y: 4,
                            height: 1,
                            last: false
                        },
                    ]
                );
                assert_eq!(
                    agent_visible_rows_for_entries(&entries, body, 1, 0, &heights),
                    vec![
                        AgentVisibleRow::GroupHeader { entry_idx: 1, y: 3 },
                        AgentVisibleRow::Child {
                            entry_idx: 1,
                            y: 4,
                            height: 4,
                            last: true
                        },
                    ]
                );
            }
        }
    }

    #[test]
    fn m828d2_d1_geometry_premises_are_rechecked_with_resolved_heights() {
        use crate::config::{AgentSidebarToken as A, SpaceSidebarToken as S};
        let mut app = AppState::test_new();
        app.sidebar_spaces.row_gap = 1;
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/zynk-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/zynk-two"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = Some("main".into());
        }
        let full = Rect::new(0, 0, 30, 30);
        let (cards, headers) = compute_workspace_list_areas(&app, full);
        assert!(headers.is_empty());
        assert_eq!(
            cards
                .iter()
                .map(|c| (c.ws_idx, c.indented, c.rect))
                .collect::<Vec<_>>(),
            vec![
                (0, false, Rect::new(0, 2, 29, 2)),
                (1, true, Rect::new(0, 4, 29, 1)),
                (2, true, Rect::new(0, 5, 29, 1)),
                (3, false, Rect::new(0, 7, 29, 2)),
            ]
        );
        let metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 8));
        assert_eq!(
            (metrics.viewport_rows, metrics.max_offset_from_bottom),
            (3, 1)
        );
        let entries = vec![
            agent_entry(0, 0, "claude", 1),
            agent_entry(0, 0, "codex", 2),
            agent_entry(0, 1, "pi", 3),
        ];
        let body = Rect::new(0, 0, 30, 12);
        assert_eq!(agent_row_heights(&app, &entries), vec![1, 1, 1]);
        assert_eq!(
            agent_visible_rows_for_entries(&entries, body, 0, 0, &[1, 1, 1]),
            vec![
                AgentVisibleRow::GroupHeader { entry_idx: 0, y: 0 },
                AgentVisibleRow::Child {
                    entry_idx: 0,
                    y: 1,
                    height: 1,
                    last: false
                },
                AgentVisibleRow::Child {
                    entry_idx: 1,
                    y: 2,
                    height: 1,
                    last: true
                },
                AgentVisibleRow::GroupHeader { entry_idx: 2, y: 3 },
                AgentVisibleRow::Child {
                    entry_idx: 2,
                    y: 4,
                    height: 1,
                    last: true
                },
            ]
        );
        let many: Vec<_> = (0..8)
            .map(|i| agent_entry(i, 0, "claude", i as u32 + 1))
            .collect();
        let body = Rect::new(0, 0, 30, 8);
        assert_eq!(agent_row_heights(&app, &many), vec![1; 8]);
        assert_eq!(max_agent_panel_scroll(&many, body, 0, &[1; 8]), 4);
        assert_eq!(agent_children_placed_from(&many, body, 4, 0, &[1; 8]), 4);
        assert_eq!(
            agent_visible_rows_for_entries(&many, body, 4, 0, &[1; 8])
                .iter()
                .filter_map(|r| match r {
                    AgentVisibleRow::Child {
                        entry_idx,
                        y,
                        height,
                        ..
                    } => Some((*entry_idx, *y, *height)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![(4, 1, 1), (5, 3, 1), (6, 5, 1), (7, 7, 1)]
        );

        for gap in [0u16, 1, 3, u16::MAX] {
            app.sidebar_spaces.row_gap = gap;
            app.sidebar_spaces.rows = vec![
                vec![S::Workspace],
                vec![S::Custom("a".into())],
                vec![S::Custom("b".into())],
            ];
            for (index, workspace) in app.workspaces.iter_mut().enumerate() {
                let values = if index == 0 {
                    vec![("a", "A"), ("b", "B")]
                } else if index == 1 {
                    vec![]
                } else {
                    vec![("a", "A")]
                };
                workspace.metadata_tokens.patch(
                    values
                        .into_iter()
                        .map(|(key, value)| (key.into(), Some(value.into())))
                        .collect(),
                    None,
                    std::time::Instant::now(),
                );
            }
            let cards = compute_workspace_card_areas(&app, full);
            let mut expected = vec![(0, 2, 3), (1, 5, 1), (2, 6, 2)];
            if gap < u16::MAX {
                expected.push((3, 8 + gap, 2));
            }
            assert_eq!(
                cards
                    .iter()
                    .map(|c| (c.ws_idx, c.rect.y, c.rect.height))
                    .collect::<Vec<_>>(),
                expected
            );
            app.sidebar_agents.rows = vec![
                vec![A::Agent],
                vec![A::Custom("a".into())],
                vec![A::Custom("b".into())],
            ];
            let mut entries = vec![
                agent_entry(0, 0, "claude", 1),
                agent_entry(0, 0, "codex", 2),
                agent_entry(0, 1, "pi", 3),
            ];
            entries[0].tokens.insert("a".into(), "A".into());
            entries[0].tokens.insert("b".into(), "B".into());
            entries[2].tokens.insert("a".into(), "A".into());
            let heights = agent_row_heights(&app, &entries);
            assert_eq!(heights, vec![3, 1, 2]);
            let rows =
                agent_visible_rows_for_entries(&entries, Rect::new(0, 0, 30, 24), 0, gap, &heights);
            let expected = if gap == u16::MAX {
                vec![(0, 1, 3)]
            } else {
                vec![(0, 1, 3), (1, 4 + gap, 1), (2, 6 + 2 * gap, 2)]
            };
            assert_eq!(
                rows.iter()
                    .filter_map(|r| match r {
                        AgentVisibleRow::Child {
                            entry_idx,
                            y,
                            height,
                            ..
                        } => Some((*entry_idx, *y, *height)),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn m828d2_token_separators_follow_surviving_occurrences() {
        use crate::config::SpaceSidebarToken as S;
        use ResolvedToken as R;
        let app = AppState::test_new();
        let styles = TokenStyles {
            state_text: Style::default(),
            workspace: Style::default(),
            secondary: Style::default(),
            custom: Style::default(),
        };
        let draw = |resolved: &[R], width: u16, align: bool| {
            let spans = resolved_token_spans(
                resolved,
                ("\u{25cf}", Style::default()),
                styles,
                &app.palette,
                width as usize,
                align,
            );
            let mut terminal = Terminal::new(TestBackend::new(width + 4, 3)).unwrap();
            terminal
                .draw(|frame| {
                    for cell in &mut frame.buffer_mut().content {
                        cell.set_symbol("#");
                    }
                    frame.render_widget(ratatui::widgets::Clear, Rect::new(2, 1, width, 1));
                    frame.render_widget(
                        Paragraph::new(Line::from(spans)),
                        Rect::new(2, 1, width, 1),
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..3 {
                for x in 0..width + 4 {
                    if y != 1 || x < 2 || x >= width + 2 {
                        assert_eq!(buffer[(x, y)].symbol(), "#");
                    }
                }
            }
            (2..width + 2)
                .map(|x| buffer[(x, 1)].symbol())
                .collect::<String>()
        };
        assert_eq!(
            draw(
                &[
                    R::StateIcon,
                    R::Workspace("W".into()),
                    R::Custom("C".into()),
                    R::GitStatus {
                        ahead: 2,
                        behind: 1
                    }
                ],
                40,
                false
            )
            .trim_end(),
            "\u{25cf} W \u{b7} C \u{2191}2 \u{2193}1"
        );
        let config = crate::config::SpacesSidebarConfig {
            rows: vec![vec![
                S::Custom("a".into()),
                S::Custom("b".into()),
                S::Custom("c".into()),
            ]],
            ..Default::default()
        };
        for (missing, expected) in [
            ("a", "B \u{b7} C"),
            ("b", "A \u{b7} C"),
            ("c", "A \u{b7} B"),
        ] {
            let mut values = std::collections::HashMap::from([
                ("a".into(), "A".into()),
                ("b".into(), "B".into()),
                ("c".into(), "C".into()),
            ]);
            let resolve = |values: &std::collections::HashMap<String, String>| {
                tokens::space_rows(
                    &config,
                    SpaceTokenContext {
                        workspace: "ws",
                        state_text: "idle",
                        branch: None,
                        ahead_behind: None,
                        tokens: values,
                        suppress_git_details: false,
                    },
                )
            };
            let positive = resolve(&values);
            assert_eq!(positive.len(), 1);
            assert_eq!(
                draw(&positive[0], 40, false).trim_end(),
                "A \u{b7} B \u{b7} C"
            );
            assert!(values.remove(missing).is_some());
            let rows = resolve(&values);
            assert_eq!(rows.len(), 1);
            assert_eq!(draw(&rows[0], 40, false).trim_end(), expected);
            values.clear();
            assert!(resolve(&values).is_empty());
            assert!(draw(&[], 40, false).trim().is_empty());
        }
        assert_eq!(
            draw(
                &[R::Workspace("W".into()), R::Workspace("W".into())],
                20,
                false
            )
            .trim_end(),
            "W \u{b7} W"
        );
        assert_eq!(
            draw(
                &[R::Workspace("left".into()), R::StateText("done".into())],
                20,
                true
            ),
            "left            done"
        );
        assert_eq!(
            draw(
                &[
                    R::Workspace("left".into()),
                    R::Custom("x".into()),
                    R::StateText("done".into())
                ],
                20,
                true
            ),
            "left \u{b7} x        done"
        );
        assert_eq!(
            draw(
                &[R::StateText("done".into()), R::Workspace("right".into())],
                20,
                true
            )
            .trim_end(),
            "done \u{b7} right"
        );
    }

    #[test]
    fn m828d2_narrow_unicode_rows_keep_late_tokens_and_clip_to_cells() {
        use ResolvedToken as R;
        let app = AppState::test_new();
        let styles = TokenStyles {
            state_text: Style::default(),
            workspace: Style::default(),
            secondary: Style::default(),
            custom: Style::default(),
        };
        let draw = |resolved: &[R], width: u16| {
            let spans = resolved_token_spans(
                resolved,
                ("\u{25cf}", Style::default()),
                styles,
                &app.palette,
                width as usize,
                false,
            );
            let text = spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            let mut terminal = Terminal::new(TestBackend::new(width + 4, 3)).unwrap();
            terminal
                .draw(|frame| {
                    for cell in &mut frame.buffer_mut().content {
                        cell.set_symbol("#");
                    }
                    frame.render_widget(
                        Paragraph::new(Line::from(spans)),
                        Rect::new(2, 1, width, 1),
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            for y in 0..3 {
                for x in 0..width + 4 {
                    if y != 1 || x < 2 || x >= width + 2 {
                        assert_eq!(buffer[(x, y)].symbol(), "#");
                    }
                }
            }
            (text, buffer)
        };
        let late = [R::Workspace("abcdefghijkl".into()), R::Tab("LATE".into())];
        assert_eq!(draw(&late, 40).0, "abcdefghijkl \u{b7} LATE");
        assert_eq!(draw(&late, 4).0, "LATE");
        let (narrow, cells) = draw(&late, 5);
        assert_eq!(narrow, "\u{2026} \u{b7} \u{2026}");
        assert_eq!(cells[(6, 1)].symbol(), "\u{2026}");
        for (value, columns, first) in [
            ("\u{754c}", 2, "\u{754c}"),
            ("\u{1f680}", 2, "\u{1f680}"),
            ("e\u{301}xy", 3, "e\u{301}"),
        ] {
            assert_eq!(display_width(value), columns);
            let row = [R::Workspace(value.into()), R::Tab("LATE".into())];
            let expected = format!("{value} \u{b7} LATE");
            assert_eq!(draw(&row, 40).0, expected);
            let (text, cells) = draw(&row, (columns + 7) as u16);
            assert_eq!(text, expected);
            assert_eq!(cells[(2, 1)].symbol(), first);
            assert_eq!(cells[((columns + 8) as u16, 1)].symbol(), "E");
        }
        let wide = [R::Workspace("\u{754c}\u{754c}Z".into())];
        assert_eq!(draw(&wide, 20).0, "\u{754c}\u{754c}Z");
        assert_eq!(draw(&wide, 3).0, "\u{754c}\u{2026}");
        let titles = [
            R::TerminalTitle("\u{280b} raw".into()),
            R::TerminalTitle("raw".into()),
        ];
        assert_eq!(draw(&titles, 40).0, "\u{280b} raw \u{b7} raw");
        assert_eq!(draw(&titles, 6).0, "\u{280b}\u{2026} \u{b7} \u{2026}");
        let git = [
            R::Branch("branch".into()),
            R::GitStatus {
                ahead: 12,
                behind: 3,
            },
        ];
        assert_eq!(draw(&git, 40).0, "\u{2387} branch \u{2191}12 \u{2193}3");
        let (fixed, cells) = draw(&git, 5);
        assert!(
            display_width(&fixed) > 5,
            "fixed payload is clipped by Paragraph, not span allocation"
        );
        assert_eq!(
            (2..7).map(|x| cells[(x, 1)].symbol()).collect::<String>(),
            "\u{2191}12 \u{2193}"
        );
        let (_, cells) = draw(&git, 0);
        assert!(cells.content.iter().all(|cell| cell.symbol() == "#"));
    }

    #[test]
    fn m828d2_empty_and_oversized_space_rows_have_bounded_selectable_height() {
        use crate::config::SpaceSidebarToken as S;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        for rows in [
            vec![],
            vec![vec![S::Custom("absent".into())]],
            vec![vec![S::Workspace]; 16],
        ] {
            let oversized = rows.len() == 16;
            let mut config = crate::config::Config {
                onboarding: Some(false),
                ..Default::default()
            };
            config.ui.sidebar.spaces.rows = rows.clone();
            config.ui.sidebar.spaces.row_gap = u16::MAX;
            let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut owner =
                crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
            owner.state.workspaces = vec![
                Workspace::test_new("VISIBLE"),
                Workspace::test_new("second"),
            ];
            for workspace in &mut owner.state.workspaces {
                workspace.cached_git_branch = None;
            }
            owner.state.ensure_test_terminals();
            owner.state.mode = Mode::Terminal;
            owner.state.active = Some(1);
            if !rows.is_empty() && !oversized {
                let workspace = &mut owner.state.workspaces[0];
                assert!(workspace.metadata_tokens.patch(
                    std::collections::HashMap::from([("absent".into(), Some("POSITIVE".into()))]),
                    None,
                    std::time::Instant::now()
                ));
                let values = workspace.metadata_tokens.values();
                let positive = tokens::space_rows(
                    &owner.state.sidebar_spaces,
                    SpaceTokenContext {
                        workspace: "VISIBLE",
                        state_text: "unknown",
                        branch: None,
                        ahead_behind: None,
                        tokens: &values,
                        suppress_git_details: false,
                    },
                );
                assert_eq!(
                    positive,
                    vec![vec![ResolvedToken::Custom("POSITIVE".into())]]
                );
                assert!(workspace.metadata_tokens.patch(
                    std::collections::HashMap::from([("absent".into(), None)]),
                    None,
                    std::time::Instant::now()
                ));
                assert!(workspace.metadata_tokens.values().is_empty());
            }
            let large = Rect::new(0, 0, 20, 44);
            let large_cards = compute_workspace_card_areas(&owner.state, large);
            assert_eq!(large_cards[0].rect.height, if oversized { 16 } else { 1 });
            let area = Rect::new(0, 0, 20, 12);
            let section = workspace_list_rect(area, owner.state.sidebar_section_split);
            let body = workspace_list_body_rect(section, true);
            assert_eq!(body, Rect::new(0, 2, 18, 3));
            assert_eq!(owner.state.workspace_scroll, 0);
            let cards = compute_workspace_card_areas(&owner.state, area);
            assert_eq!(cards.len(), 1);
            assert_eq!(cards[0].ws_idx, 0);
            assert_eq!(
                cards[0].rect,
                Rect::new(0, 2, 18, if oversized { 3 } else { 1 })
            );
            assert_eq!(workspace_list_visible_count(&owner.state, section, 0), 1);
            if oversized {
                assert_eq!(cards[0].rect.bottom(), body.bottom());
            }
            assert_eq!(owner.state.workspace_scroll, 0);
            owner.state.view.sidebar_rect = area;
            owner.state.view.workspace_card_areas = cards;
            let mut terminal = Terminal::new(TestBackend::new(24, 16)).unwrap();
            terminal
                .draw(|frame| {
                    for cell in &mut frame.buffer_mut().content {
                        cell.set_symbol("#");
                    }
                    frame.render_widget(ratatui::widgets::Clear, section);
                    render_workspace_list(
                        &owner.state,
                        &TerminalRuntimeRegistry::new(),
                        frame,
                        section,
                        false,
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let card = owner.state.view.workspace_card_areas[0].rect;
            for y in card.y..card.bottom() {
                let line = (card.x..card.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>();
                if oversized {
                    assert!(line.contains("VISIBLE"));
                } else {
                    assert!(line.trim().is_empty());
                }
                owner.state.active = Some(1);
                owner.route_client_events(
                    [
                        MouseEventKind::Down(MouseButton::Left),
                        MouseEventKind::Up(MouseButton::Left),
                    ]
                    .into_iter()
                    .map(|kind| {
                        crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                            kind,
                            column: 2,
                            row: y,
                            modifiers: KeyModifiers::NONE,
                        })
                    })
                    .collect(),
                    false,
                );
                assert_eq!(owner.state.active, Some(0));
            }
            for y in 0..16 {
                for x in 0..24 {
                    if x >= section.right() || y >= section.bottom() {
                        assert_eq!(buffer[(x, y)].symbol(), "#");
                    }
                }
            }
            owner.state.active = Some(1);
            owner.route_client_events(
                [
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Up(MouseButton::Left),
                ]
                .into_iter()
                .map(|kind| {
                    crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                        kind,
                        column: 2,
                        row: 1,
                        modifiers: KeyModifiers::NONE,
                    })
                })
                .collect(),
                false,
            );
            assert_eq!(
                owner.state.active,
                Some(1),
                "blank header is outside card hit area"
            );
        }
    }

    #[test]
    fn m828d2_configured_space_rows_compose_with_worktree_gaps() {
        for gap in [0, 3] {
            let source = format!(
                "[ui.sidebar.spaces]\nrow_gap = {gap}\nrows = [[\"workspace\"], [\"$one\"], [\"$two\"], [\"branch\", \"git_status\"]]\n"
            );
            assert_eq!(
                source.parse::<toml::Value>().unwrap()["ui"]["sidebar"]["spaces"]["rows"]
                    .as_array()
                    .unwrap()
                    .len(),
                4
            );
            let config: crate::config::Config = toml::from_str(&source).unwrap();
            let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut owner =
                crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
            let app = &mut owner.state;
            app.workspaces = vec![
                workspace_with_worktree_space("main", Some("d2-group"), "/repo/zynk"),
                workspace_with_worktree_space("one", Some("d2-group"), "/repo/zynk-one"),
                workspace_with_worktree_space("two", Some("d2-group"), "/repo/zynk-two"),
                Workspace::test_new("notes"),
            ];
            for (index, values) in [
                vec![("one", "ROOT-A"), ("two", "ROOT-B")],
                vec![("one", "CHILD-A")],
                vec![],
                vec![("one", "NOTES-A")],
            ]
            .into_iter()
            .enumerate()
            {
                let workspace = &mut app.workspaces[index];
                workspace.cached_git_branch = Some(format!("branch-{index}"));
                workspace.cached_git_ahead_behind = Some((2, 1));
                let expected: std::collections::HashMap<String, String> = values
                    .iter()
                    .map(|(key, value)| ((*key).into(), (*value).into()))
                    .collect();
                workspace.metadata_tokens.patch(
                    expected
                        .iter()
                        .map(|(key, value)| (key.clone(), Some(value.clone())))
                        .collect(),
                    None,
                    std::time::Instant::now(),
                );
                assert_eq!(workspace.metadata_tokens.values(), expected);
                assert_eq!(workspace.branch(), Some(format!("branch-{index}")));
                assert_eq!(workspace.git_ahead_behind(), Some((2, 1)));
            }
            app.active = Some(0);
            app.mode = Mode::Terminal;
            let area = Rect::new(0, 0, 48, 48);
            let body = workspace_list_body_rect(
                workspace_list_rect(area, app.sidebar_section_split),
                false,
            );
            let (cards, headers) = compute_workspace_list_areas(app, area);
            assert!(headers.is_empty());
            assert_eq!(
                cards
                    .iter()
                    .map(|card| (card.ws_idx, card.indented))
                    .collect::<Vec<_>>(),
                vec![(0, false), (1, true), (2, true), (3, false)]
            );
            for (index, y, height) in [(0, 2, 4), (1, 6, 2), (2, 8, 1), (3, 9 + gap, 3)] {
                assert_eq!(
                    cards[index].rect,
                    Rect::new(body.x, y, body.width, height),
                    "card {index}, gap {gap}"
                );
            }
            let lines = render_workspace_list_to_lines(app, area.width, area.height, false);
            for (y, marker) in [
                (2, "main"),
                (3, "ROOT-A"),
                (4, "ROOT-B"),
                (5, "branch-0"),
                (6, "one"),
                (7, "CHILD-A"),
                (8, "two"),
                (9 + gap, "notes"),
                (10 + gap, "NOTES-A"),
                (11 + gap, "branch-3"),
            ] {
                assert!(
                    lines[y as usize].contains(marker),
                    "row {y}: {:?}",
                    lines[y as usize]
                );
            }
            assert!(lines[5].contains("↑2") && lines[5].contains("↓1"));
            for card in &cards[1..3] {
                let text = lines[card.rect.y as usize..card.rect.bottom() as usize].join("\n");
                assert!(!text.contains("branch-") && !text.contains("↑2") && !text.contains("↓1"));
            }
            for y in 9..9 + gap {
                assert!(lines[y as usize].trim().is_empty());
            }
            assert_eq!(
                workspace_list_visible_count(
                    app,
                    workspace_list_rect(area, app.sidebar_section_split),
                    0
                ),
                4
            );
        }
    }

    #[test]
    fn m828d2_configured_agent_rows_share_grouped_geometry_and_render() {
        let source =
            "[ui.sidebar.agents]\nrow_gap = 2\nrows = [[\"agent\"], [\"pane\"], [\"$mark\"]]\n";
        assert_eq!(
            source.parse::<toml::Value>().unwrap()["ui"]["sidebar"]["agents"]["rows"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let config: crate::config::Config = toml::from_str(source).unwrap();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut owner =
            crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
        let app = &mut owner.state;
        let mut first = Workspace::test_new("alpha");
        first.test_split(Direction::Horizontal);
        app.workspaces = vec![first, Workspace::test_new("beta")];
        app.ensure_test_terminals();
        let ids: Vec<_> = app
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
        assert_eq!(ids.len(), 3);
        for (index, (id, agent)) in ids
            .iter()
            .zip([Agent::Claude, Agent::Pi, Agent::Codex])
            .enumerate()
        {
            let terminal = app.terminals.get_mut(id).unwrap();
            terminal.detected_agent = Some(agent);
            terminal.state = AgentState::Working;
            terminal.set_manual_label(format!("PANE-{index}"));
            assert!(terminal.metadata_tokens.patch(
                std::collections::HashMap::from([("mark".into(), Some(format!("MARK-{index}")))]),
                None,
                std::time::Instant::now()
            ));
            assert_eq!(
                terminal.metadata_tokens.values()["mark"],
                format!("MARK-{index}")
            );
            assert_eq!(terminal.effective_known_agent(), Some(agent));
        }
        let entries = agent_panel_entries(app);
        assert_eq!(entries.len(), 3);
        assert_eq!(agent_group_key(&entries[0]), agent_group_key(&entries[1]));
        assert_ne!(agent_group_key(&entries[1]), agent_group_key(&entries[2]));
        let area = Rect::new(0, 0, 48, 24);
        let rows = agent_visible_rows(app, area);
        let children: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                AgentVisibleRow::Child {
                    entry_idx, y, last, ..
                } => Some((*entry_idx, *y, *last)),
                _ => None,
            })
            .collect();
        assert_eq!(children, vec![(0, 4, false), (1, 9, true), (2, 15, true)]);
        let headers: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                AgentVisibleRow::GroupHeader { entry_idx, y } => Some((*entry_idx, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(headers, vec![(0, 3), (2, 14)]);
        let lines = render_agent_detail_to_lines(app, area.width, area.height);
        for (index, y, last) in children {
            assert!(lines[y as usize].contains(entries[index].agent_label.as_deref().unwrap()));
            assert!(lines[y as usize].contains(if last { "└─" } else { "├─" }));
            assert!(lines[y as usize + 1].contains(&format!("PANE-{index}")));
            assert!(lines[y as usize + 2].contains(&format!("MARK-{index}")));
            assert!(y + 3 <= area.bottom());
        }
        for (index, y) in headers {
            assert!(lines[y as usize].contains(&entries[index].tab_label));
        }
        for y in [7, 8, 12, 13] {
            assert!(lines[y].trim().is_empty());
        }
    }

    #[test]
    fn m828d2_renamed_agent_override_changes_only_its_resolved_rows() {
        let source = "[ui.sidebar.agents]\nrows = [[\"$global\"]]\n[ui.sidebar.agents.rows_by_agent]\nclaude = [[\"$special\"], [\"agent\"], [\"$third\"]]\n";
        assert_eq!(
            source.parse::<toml::Value>().unwrap()["ui"]["sidebar"]["agents"]["rows_by_agent"]
                ["claude"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let config: crate::config::Config = toml::from_str(source).unwrap();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut owner =
            crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
        let app = &mut owner.state;
        let mut workspace = Workspace::test_new("override-group");
        workspace.test_split(Direction::Horizontal);
        workspace.test_split(Direction::Vertical);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let tab = &app.workspaces[0].tabs[0];
        let ids: Vec<_> = tab
            .layout
            .pane_ids()
            .into_iter()
            .map(|pane| tab.panes[&pane].attached_terminal_id.clone())
            .collect();
        assert_eq!(ids.len(), 3);
        for (index, ((id, agent), display)) in ids
            .iter()
            .zip([Some(Agent::Claude), Some(Agent::Pi), None])
            .zip(["renamed-agent", "pi-display", "claude"])
            .enumerate()
        {
            let terminal = app.terminals.get_mut(id).unwrap();
            terminal.detected_agent = agent;
            terminal.set_agent_name(display.into());
            terminal.state = AgentState::Working;
            let values = std::collections::HashMap::from([
                ("global".into(), Some(format!("GLOBAL-{index}"))),
                ("special".into(), Some(format!("OVERRIDE-{index}"))),
                ("third".into(), Some(format!("THIRD-{index}"))),
            ]);
            assert!(terminal
                .metadata_tokens
                .patch(values, None, std::time::Instant::now()));
            assert_eq!(
                terminal.metadata_tokens.values()["special"],
                format!("OVERRIDE-{index}")
            );
            assert_eq!(terminal.effective_known_agent(), agent);
            assert_eq!(terminal.agent_name.as_deref(), Some(display));
        }
        let entries = agent_panel_entries(app);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.agent_label.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("renamed-agent"), Some("pi-display"), Some("claude")]
        );
        let lines = render_agent_detail_to_lines(app, 48, 18);
        assert!(lines[4].contains("OVERRIDE-0"), "{:?}", lines[4]);
        assert!(lines[5].contains("renamed-agent"));
        assert!(lines[6].contains("THIRD-0"));
        assert!(lines[7].contains("GLOBAL-1"));
        assert!(lines[8].contains("GLOBAL-2"));
        assert!(!lines.join("\n").contains("OVERRIDE-1"));
        assert!(!lines.join("\n").contains("OVERRIDE-2"));
        let children: Vec<_> = agent_visible_rows(app, Rect::new(0, 0, 48, 18))
            .into_iter()
            .filter_map(|row| match row {
                AgentVisibleRow::Child { entry_idx, y, .. } => Some((entry_idx, y)),
                _ => None,
            })
            .collect();
        assert_eq!(children, vec![(0, 4), (1, 7), (2, 8)]);
    }

    #[test]
    fn m828d2_variable_space_heights_clamp_to_the_last_full_page() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("d2-scroll"), "/repo/zynk"),
            workspace_with_worktree_space("one", Some("d2-scroll"), "/repo/zynk-one"),
            workspace_with_worktree_space("two", Some("d2-scroll"), "/repo/zynk-two"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = None;
        }
        let large = Rect::new(0, 0, 30, 30);
        assert_eq!(workspace_list_entries(&app).len(), 3);
        assert_eq!(compute_workspace_card_areas(&app, large).len(), 3);
        assert_eq!(app.workspace_scroll, 0);
        assert_eq!(
            normalized_workspace_scroll(&app, large, 2),
            0,
            "all entries already fit"
        );
        assert_eq!(
            app.workspace_scroll, 0,
            "pure normalization does not mutate state"
        );

        let source =
            "[ui.sidebar.spaces]\nrow_gap = 2\nrows = [[\"workspace\"], [\"$one\"], [\"$two\"]]\n";
        assert!(source.parse::<toml::Value>().is_ok());
        let config: crate::config::Config = toml::from_str(source).unwrap();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut owner =
            crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
        owner.state.workspaces = app.workspaces;
        let app = &mut owner.state;
        app.workspaces.push(Workspace::test_new("notes"));
        for (index, keys) in [vec!["one", "two"], vec![], vec!["two"], vec![]]
            .into_iter()
            .enumerate()
        {
            app.workspaces[index].cached_git_branch = None;
            let values: std::collections::HashMap<String, Option<String>> = keys
                .iter()
                .map(|key| ((*key).into(), Some(format!("{index}-{key}"))))
                .collect();
            app.workspaces[index]
                .metadata_tokens
                .patch(values, None, std::time::Instant::now());
            assert_eq!(
                app.workspaces[index].metadata_tokens.values().len(),
                keys.len()
            );
        }
        let area = Rect::new(0, 0, 30, 16);
        let section = workspace_list_rect(area, app.sidebar_section_split);
        let body = workspace_list_body_rect(section, true);
        assert_eq!(body.height, 5);
        for (requested, expected_scroll, expected) in [
            (0, 0, vec![(0, 2, 3), (1, 5, 1)]),
            (1, 1, vec![(1, 2, 1), (2, 3, 2)]),
            (2, 2, vec![(2, 2, 2), (3, 6, 1)]),
            (usize::MAX, 2, vec![(2, 2, 2), (3, 6, 1)]),
        ] {
            assert_eq!(
                normalized_workspace_scroll(app, area, requested),
                expected_scroll
            );
            app.workspace_scroll = expected_scroll;
            let cards = compute_workspace_card_areas(app, area);
            assert_eq!(
                cards
                    .iter()
                    .map(|card| (card.ws_idx, card.rect.y, card.rect.height))
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(cards
                .iter()
                .all(|card| card.rect.width == body.width && card.rect.bottom() <= body.bottom()));
            let metrics = workspace_list_scroll_metrics(app, section);
            assert_eq!(metrics.max_offset_from_bottom, 2);
            assert_eq!(metrics.viewport_rows, cards.len());
            assert_eq!(app.workspace_scroll, expected_scroll);
        }
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("d2-scroll".into());
        for (active, indices) in [(Some(3), vec![0, 3]), (Some(2), vec![0, 2, 3])] {
            app.active = active;
            app.workspace_scroll = 0;
            assert_eq!(
                compute_workspace_card_areas(app, large)
                    .iter()
                    .map(|card| card.ws_idx)
                    .collect::<Vec<_>>(),
                indices
            );
            assert_eq!(normalized_workspace_scroll(app, large, usize::MAX), 0);
        }
    }

    #[test]
    fn m828d2_fork_default_rows_keep_group_status_and_branch_presentation() {
        let config = crate::config::Config::default();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut owner =
            crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
        let app = &mut owner.state;
        let mut workspace = Workspace::test_new("alpha");
        workspace.test_split(Direction::Horizontal);
        workspace.tabs[0].set_custom_name("alpha".into());
        assert_eq!(workspace.tab_display_name(0).as_deref(), Some("alpha"));
        workspace.cached_git_branch = Some("d2-main".into());
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let ids: Vec<_> = app.workspaces[0].tabs[0]
            .layout
            .pane_ids()
            .into_iter()
            .map(|id| {
                app.workspaces[0].tabs[0].panes[&id]
                    .attached_terminal_id
                    .clone()
            })
            .collect();
        for (id, agent, state) in [
            (&ids[0], Agent::Claude, AgentState::Blocked),
            (&ids[1], Agent::Pi, AgentState::Working),
        ] {
            let terminal = app.terminals.get_mut(id).unwrap();
            terminal.detected_agent = Some(agent);
            terminal.state = state;
            assert_eq!(terminal.effective_known_agent(), Some(agent));
        }
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Navigate;
        app.status_indicators = StatusIndicatorStyle::Dots;
        let entries = agent_panel_entries(app);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].agent_label.as_deref(), Some("claude"));
        assert_eq!(entries[1].agent_label.as_deref(), Some("pi"));
        assert!(app.is_active_pane(entries[1].ws_idx, entries[1].tab_idx, entries[1].pane_id));
        let sidebar = Rect::new(0, 0, 48, 30);
        app.view.sidebar_rect = sidebar;
        app.view.workspace_card_areas = compute_workspace_card_areas(app, sidebar);
        assert_eq!(app.view.workspace_card_areas.len(), 1);
        assert_eq!(
            app.view.workspace_card_areas[0].rect,
            Rect::new(0, 2, 47, 2)
        );
        let runtimes = TerminalRuntimeRegistry::new();
        let mut spaces = Terminal::new(TestBackend::new(48, 30)).unwrap();
        spaces
            .draw(|frame| {
                render_workspace_list(
                    app,
                    &runtimes,
                    frame,
                    workspace_list_rect(sidebar, app.sidebar_section_split),
                    true,
                )
            })
            .unwrap();
        let buffer = spaces.backend().buffer();
        assert_eq!(buffer[(1, 2)].symbol(), "◉");
        assert_eq!(buffer[(3, 2)].symbol(), "a");
        assert_eq!(
            (3..8).map(|x| buffer[(x, 2)].symbol()).collect::<String>(),
            "alpha"
        );
        assert_eq!(buffer[(1, 3)].symbol(), "⎇");
        assert_eq!(
            (3..10).map(|x| buffer[(x, 3)].symbol()).collect::<String>(),
            "d2-main"
        );
        for y in [2, 3] {
            assert_eq!(buffer[(25, y)].style().bg, Some(app.palette.selection_bg));
        }

        let area = Rect::new(0, 0, 48, 14);
        let rows = agent_visible_rows(app, area);
        let coordinates: Vec<_> = rows
            .iter()
            .map(|row| match row {
                AgentVisibleRow::GroupHeader { entry_idx, y } => ("header", *entry_idx, *y),
                AgentVisibleRow::Child { entry_idx, y, .. } => ("child", *entry_idx, *y),
            })
            .collect();
        assert_eq!(
            coordinates,
            vec![("header", 0, 3), ("child", 0, 4), ("child", 1, 5)]
        );
        let mut agents = Terminal::new(TestBackend::new(48, 14)).unwrap();
        agents
            .draw(|frame| render_agent_detail(app, &runtimes, frame, area))
            .unwrap();
        let buffer = agents.backend().buffer();
        assert_eq!(buffer[(0, 3)].symbol(), "◉");
        assert_eq!(
            (2..7).map(|x| buffer[(x, 3)].symbol()).collect::<String>(),
            "alpha"
        );
        for (y, connector, icon, name, status) in [
            (4, "├", "◉", "claude", "blocked"),
            (5, "└", "●", "pi", "working"),
        ] {
            assert_eq!(buffer[(0, y)].symbol(), connector);
            assert_eq!(buffer[(1, y)].symbol(), "─");
            assert_eq!(buffer[(3, y)].symbol(), icon);
            assert_eq!(
                (5..5 + name.len() as u16)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>(),
                name
            );
            assert_eq!(
                (41..48)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>(),
                status
            );
        }
        assert_eq!(buffer[(25, 5)].style().bg, Some(app.palette.active_row_bg));
        assert_ne!(buffer[(25, 4)].style().bg, Some(app.palette.active_row_bg));
    }

    #[test]
    fn m828d2_configured_rows_leave_hidden_collapsed_and_mobile_paths_unchanged() {
        let mut outputs = Vec::new();
        for source in ["", "[ui.sidebar.agents]\nrow_gap = 3\nrows = [[\"$mark\"], [\"pane\"]]\n[ui.sidebar.agents.rows_by_agent]\nclaude = [[\"$mark\"], [\"agent\"], [\"workspace\"]]\n[ui.sidebar.spaces]\nrow_gap = 4\nrows = [[\"$mark\"], [\"workspace\"], [\"branch\"]]\n"] {
            assert!(source.parse::<toml::Value>().is_ok());
            let config: crate::config::Config = toml::from_str(source).unwrap();
            let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut owner = crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
            let app = &mut owner.state;
            app.workspaces = vec![Workspace::test_new("alpha"), Workspace::test_new("beta")];
            for workspace in &mut app.workspaces {
                let tab_name = format!("{}-tab", workspace.display_name());
                workspace.tabs[0].set_custom_name(tab_name.clone());
                assert_eq!(workspace.tab_display_name(0).as_deref(), Some(tab_name.as_str()));
                workspace.metadata_tokens.patch(std::collections::HashMap::from([("mark".into(), Some("SPACE-MARK".into()))]), None, std::time::Instant::now());
                assert_eq!(workspace.metadata_tokens.values()["mark"], "SPACE-MARK");
            }
            app.ensure_test_terminals();
            for terminal in app.terminals.values_mut() {
                terminal.detected_agent = Some(Agent::Claude);
                terminal.state = AgentState::Working;
                terminal.metadata_tokens.patch(std::collections::HashMap::from([("mark".into(), Some("AGENT-MARK".into()))]), None, std::time::Instant::now());
                assert_eq!(terminal.metadata_tokens.values()["mark"], "AGENT-MARK");
            }
            app.active = Some(0);
            app.selected = 0;
            app.mode = Mode::Terminal;
            app.update_available = None;
            assert_eq!(agent_panel_entries(app).len(), 2);
            app.sidebar_collapsed = true;
            app.sidebar_collapsed_mode = crate::config::SidebarCollapsedModeConfig::Hidden;
            let area = Rect::new(0, 0, 106, 24);
            crate::ui::compute_view(app, area);
            assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 0, 24));
            assert_eq!(app.view.terminal_area, Rect::new(0, 1, 106, 23));
            assert!(app.view.workspace_card_areas.is_empty());
            let mut hidden = Terminal::new(TestBackend::new(106, 24)).unwrap();
            hidden.draw(|frame| crate::ui::render(app, frame)).unwrap();
            let hidden_buffer = hidden.backend().buffer().clone();
            let hidden_text: String = hidden_buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(hidden_text.contains("alpha"));

            app.mode = Mode::Navigate;
            let area = Rect::new(0, 0, 4, 18);
            let mut collapsed = Terminal::new(TestBackend::new(4, 18)).unwrap();
            collapsed.draw(|frame| render_sidebar_collapsed(app, frame, area)).unwrap();
            let (spaces, _, agents) = collapsed_sidebar_sections(area);
            let buffer = collapsed.backend().buffer();
            for section in [spaces, agents] {
                assert_eq!(buffer[(section.x, section.y)].symbol(), "1");
                assert_eq!(buffer[(section.x, section.y + 1)].symbol(), "2");
                assert_eq!(buffer[(section.x + 2, section.y)].symbol(), "●");
            }
            let collapsed_buffer = buffer.clone();

            app.sidebar_collapsed = false;
            let area = Rect::new(0, 0, 44, 40);
            crate::ui::compute_view(app, area);
            assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);
            let mut mobile = Terminal::new(TestBackend::new(44, 40)).unwrap();
            mobile.draw(|frame| crate::ui::render(app, frame)).unwrap();
            let mobile_buffer = mobile.backend().buffer().clone();
            let text: String = mobile_buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("alpha") && text.contains("beta"));
            assert!(text.to_lowercase().contains("claude"));
            assert!(text.find("alpha").unwrap() < text.find("beta").unwrap());
            outputs.push((hidden_buffer, collapsed_buffer, mobile_buffer));
        }
        assert_eq!(outputs.len(), 2);
        assert_eq!(
            outputs[0], outputs[1],
            "complete symbols and styles at the three bounded routes"
        );
    }

    #[test]
    fn m828d1_a_fitting_space_does_not_owe_a_trailing_gap() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("alpha"), Workspace::test_new("beta")];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = None;
        }
        let one_row = Rect::new(0, 0, 30, 8);
        let section = workspace_list_rect(one_row, app.sidebar_section_split);
        let body = workspace_list_body_rect(section, false);
        assert_eq!(body.height, 1);
        for gap in [u16::MAX, 2, 0] {
            app.sidebar_spaces.row_gap = gap;
            let cards = compute_workspace_card_areas(&app, one_row);
            assert_eq!(cards.len(), 1, "content fits even with gap {gap}");
            assert_eq!(cards[0].ws_idx, 0);
            assert_eq!(cards[0].rect.y, body.y);
            assert_eq!(cards[0].rect.height, 1);
            assert_eq!(cards[0].rect.bottom(), body.bottom());
            let metrics = workspace_list_scroll_metrics(&app, section);
            assert_eq!(metrics.viewport_rows, cards.len(), "gap {gap}");
            assert_eq!(metrics.max_offset_from_bottom, 1);
        }

        let two_rows = Rect::new(0, 0, 30, 10);
        let section = workspace_list_rect(two_rows, app.sidebar_section_split);
        assert_eq!(workspace_list_body_rect(section, false).height, 2);
        app.sidebar_spaces.row_gap = 0;
        let cards = compute_workspace_card_areas(&app, two_rows);
        assert_eq!(cards.len(), 2);
        assert_eq!(
            cards.iter().map(|card| card.ws_idx).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(cards[1].rect.y, cards[0].rect.bottom());
        assert_eq!(
            workspace_list_scroll_metrics(&app, section).viewport_rows,
            2
        );

        app.sidebar_spaces.row_gap = u16::MAX;
        app.workspaces[0].cached_git_branch = Some("main".into());
        let cards = compute_workspace_card_areas(&app, two_rows);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].rect.height, 2);
        assert_eq!(
            cards[0].rect.bottom(),
            workspace_list_body_rect(section, false).bottom()
        );
        assert_eq!(
            workspace_list_scroll_metrics(&app, section).viewport_rows,
            1
        );
        app.workspace_scroll = 1;
        let last = compute_workspace_card_areas(&app, one_row);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].ws_idx, 1);
        assert_eq!(
            workspace_list_scroll_metrics(
                &app,
                workspace_list_rect(one_row, app.sidebar_section_split)
            )
            .viewport_rows,
            1
        );
    }

    #[test]
    fn m828d1_space_gaps_preserve_parent_child_packing() {
        for gap in [2, 0] {
            let source = format!("[ui.sidebar.spaces]\nrow_gap = {gap}\n");
            assert!(source.parse::<toml::Value>().is_ok());
            let config: crate::config::Config = toml::from_str(&source).unwrap();
            let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut owner =
                crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
            let app = &mut owner.state;
            app.workspaces = vec![
                workspace_with_worktree_space("main", Some("repo-key"), "/repo/zynk"),
                workspace_with_worktree_space("one", Some("repo-key"), "/repo/zynk-one"),
                workspace_with_worktree_space("two", Some("repo-key"), "/repo/zynk-two"),
                Workspace::test_new("notes"),
            ];
            for workspace in &mut app.workspaces {
                workspace.cached_git_branch = Some("main".into());
            }
            let area = Rect::new(0, 0, 30, 30);
            let ws_area = workspace_list_rect(area, app.sidebar_section_split);
            let body = workspace_list_body_rect(ws_area, false);
            let (cards, headers) = compute_workspace_list_areas(app, area);
            assert_eq!(cards.len(), 4);
            assert!(headers.is_empty());
            assert_eq!(
                cards
                    .iter()
                    .map(|card| (card.ws_idx, card.indented))
                    .collect::<Vec<_>>(),
                vec![(0, false), (1, true), (2, true), (3, false)]
            );
            assert_eq!(cards[0].rect, Rect::new(body.x, body.y, body.width, 2));
            assert_eq!(cards[1].rect, Rect::new(body.x, body.y + 2, body.width, 1));
            assert_eq!(cards[2].rect, Rect::new(body.x, body.y + 3, body.width, 1));
            assert_eq!(
                cards[3].rect,
                Rect::new(body.x, body.y + 4 + gap, body.width, 2),
                "configured gap follows the last grouped child"
            );
            assert_eq!(workspace_list_visible_count(app, ws_area, 0), cards.len());
        }
    }

    #[test]
    fn m828d1_default_gaps_pack_expanded_entries() {
        let config = crate::config::Config::default();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut owner =
            crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
        let app = &mut owner.state;
        app.workspaces = vec![Workspace::test_new("alpha"), Workspace::test_new("beta")];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = None;
        }
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.detected_agent = Some(Agent::Claude);
        }
        app.active = Some(0);
        let entries = agent_panel_entries(app);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].primary_label, "alpha");
        assert_eq!(entries[1].primary_label, "beta");
        let cards = compute_workspace_card_areas(app, Rect::new(0, 0, 30, 30));
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].rect.height, 1);
        assert_eq!(
            cards[1].rect.y,
            cards[0].rect.y + 1,
            "default spaces are adjacent"
        );
        let area = Rect::new(0, 0, 30, 14);
        let y = agent_panel_body_rect(area, false).y;
        assert_eq!(
            agent_visible_rows(app, area),
            vec![
                AgentVisibleRow::GroupHeader { entry_idx: 0, y },
                AgentVisibleRow::Child {
                    entry_idx: 0,
                    y: y + 1,
                    height: 1,
                    last: true
                },
                AgentVisibleRow::GroupHeader {
                    entry_idx: 1,
                    y: y + 2
                },
                AgentVisibleRow::Child {
                    entry_idx: 1,
                    y: y + 3,
                    height: 1,
                    last: true
                },
            ]
        );
        let lines = render_agent_detail_to_lines(app, area.width, area.height);
        for (idx, header_y, child_y) in [(0, y, y + 1), (1, y + 2, y + 3)] {
            assert!(lines[header_y as usize].contains(&entries[idx].tab_label));
            assert!(lines[child_y as usize].contains(entries[idx].agent_label.as_deref().unwrap()));
        }
    }

    #[test]
    fn m828d1_agent_gaps_match_grouped_rendered_rows() {
        let source = "[ui.sidebar.agents]\nrow_gap = 2\n";
        assert!(source.parse::<toml::Value>().is_ok());
        let config: crate::config::Config = toml::from_str(source).unwrap();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut owner =
            crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
        let app = &mut owner.state;
        let mut first = Workspace::test_new("alpha");
        first.test_split(Direction::Horizontal);
        app.workspaces = vec![first, Workspace::test_new("beta")];
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.detected_agent = Some(Agent::Claude);
        }
        let entries = agent_panel_entries(app);
        assert_eq!(entries.len(), 3);
        assert_eq!(agent_group_key(&entries[0]), agent_group_key(&entries[1]));
        assert_ne!(agent_group_key(&entries[1]), agent_group_key(&entries[2]));
        let area = Rect::new(0, 0, 32, 18);
        let y = agent_panel_body_rect(area, false).y;
        let rows = agent_visible_rows(app, area);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0], AgentVisibleRow::GroupHeader { entry_idx: 0, y });
        assert_eq!(
            rows[1],
            AgentVisibleRow::Child {
                entry_idx: 0,
                y: y + 1,
                height: 1,
                last: false
            }
        );
        assert_eq!(
            rows[2],
            AgentVisibleRow::Child {
                entry_idx: 1,
                y: y + 4,
                height: 1,
                last: true
            },
            "configured gap applies within the group"
        );
        assert_eq!(
            rows[3],
            AgentVisibleRow::GroupHeader {
                entry_idx: 2,
                y: y + 7
            }
        );
        assert_eq!(
            rows[4],
            AgentVisibleRow::Child {
                entry_idx: 2,
                y: y + 8,
                height: 1,
                last: true
            }
        );
        let lines = render_agent_detail_to_lines(app, area.width, area.height);
        for row in rows {
            match row {
                AgentVisibleRow::GroupHeader { entry_idx, y } => {
                    assert!(lines[y as usize].contains(&entries[entry_idx].tab_label));
                }
                AgentVisibleRow::Child { entry_idx, y, .. } => {
                    assert!(lines[y as usize]
                        .contains(entries[entry_idx].agent_label.as_deref().unwrap()));
                }
            }
        }
        for empty_y in [y + 2, y + 3, y + 5, y + 6] {
            assert!(lines[empty_y as usize].trim().is_empty());
        }
    }

    #[test]
    fn m828d1_gaps_leave_collapsed_and_mobile_presentations_unchanged() {
        let mut outputs = Vec::new();
        for gap in [0, 4] {
            let source = format!(
                "[ui.sidebar.agents]\nrow_gap = {gap}\n[ui.sidebar.spaces]\nrow_gap = {gap}\n"
            );
            assert!(source.parse::<toml::Value>().is_ok());
            let config: crate::config::Config = toml::from_str(&source).unwrap();
            let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut owner =
                crate::app::App::new(&config, true, None, rx, crate::api::EventHub::default());
            let app = &mut owner.state;
            app.workspaces = vec![Workspace::test_new("alpha"), Workspace::test_new("beta")];
            app.ensure_test_terminals();
            for terminal in app.terminals.values_mut() {
                terminal.detected_agent = Some(Agent::Claude);
                terminal.state = AgentState::Working;
            }
            app.active = Some(0);
            app.selected = 0;
            app.mode = Mode::Navigate;
            app.update_available = None;
            assert_eq!(agent_panel_entries(app).len(), 2);
            let area = Rect::new(0, 0, 4, 18);
            let mut collapsed = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            collapsed
                .draw(|frame| render_sidebar_collapsed(app, frame, area))
                .unwrap();
            let (spaces, _, agents) = collapsed_sidebar_sections(area);
            let buffer = collapsed.backend().buffer();
            for section in [spaces, agents] {
                assert_eq!(buffer[(section.x, section.y)].symbol(), "1");
                assert_eq!(buffer[(section.x, section.y + 1)].symbol(), "2");
                assert_ne!(buffer[(section.x + 2, section.y)].symbol(), " ");
            }
            let area = Rect::new(0, 0, 44, 40);
            crate::ui::compute_view(app, area);
            assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);
            let mut mobile = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            mobile.draw(|frame| crate::ui::render(app, frame)).unwrap();
            let text: String = mobile
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("alpha"));
            assert!(text.contains("beta"));
            assert!(text.to_lowercase().contains("claude"));
            assert!(text.find("alpha").unwrap() < text.find("beta").unwrap());
            outputs.push((buffer.clone(), mobile.backend().buffer().clone()));
        }
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0], outputs[1], "symbols and styles stay identical");
    }

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
        app.sidebar_spaces.row_gap = 1;
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
        let area = Rect::new(0, 0, 30, 8);
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

    #[test]
    fn m828d1_gap_placement_has_bounded_non_orphan_geometry() {
        let entries = vec![
            agent_entry(0, 0, "claude", 1),
            agent_entry(0, 0, "claude", 2),
            agent_entry(0, 1, "claude", 3),
            agent_entry(1, 0, "claude", 4),
            agent_entry(1, 0, "claude", 5),
        ];
        let mut populated_cases = 0;
        let mut cases = 0;
        for gap in [0, 1, 2, u16::MAX] {
            for width in [0, 1, 30] {
                for height in [0, 1, 2, 3, 8] {
                    for scroll in [0, 1, 2, 4, 5, usize::MAX] {
                        let body = Rect::new(2, 3, width, height);
                        let mut expected = Vec::new();
                        let mut cursor = usize::from(body.y);
                        let end = cursor + usize::from(height);
                        let mut previous = None;
                        if width > 0 {
                            for (idx, entry) in entries.iter().enumerate().skip(scroll) {
                                let group = (entry.ws_idx, entry.tab_idx);
                                let header = previous != Some(group);
                                if previous.is_some() {
                                    cursor += usize::from(gap);
                                }
                                let child_y = cursor + usize::from(header);
                                if child_y >= end {
                                    break;
                                }
                                if header {
                                    expected.push(AgentVisibleRow::GroupHeader {
                                        entry_idx: idx,
                                        y: cursor as u16,
                                    });
                                }
                                expected.push(AgentVisibleRow::Child {
                                    entry_idx: idx,
                                    y: child_y as u16,
                                    height: 1,
                                    last: entries
                                        .get(idx + 1)
                                        .is_none_or(|next| (next.ws_idx, next.tab_idx) != group),
                                });
                                cursor = child_y + 1;
                                previous = Some(group);
                            }
                        }
                        let rows = agent_visible_rows_for_entries(
                            &entries,
                            body,
                            scroll,
                            gap,
                            &vec![1; entries.len()],
                        );
                        assert_eq!(rows, expected, "gap={gap} body={body:?} scroll={scroll}");
                        for (position, row) in rows.iter().enumerate() {
                            let y = match row {
                                AgentVisibleRow::GroupHeader { entry_idx, y } => {
                                    assert!(matches!(rows.get(position + 1), Some(
                                        AgentVisibleRow::Child { entry_idx: child, y: child_y, .. }
                                    ) if child == entry_idx && *child_y == y + 1));
                                    *y
                                }
                                AgentVisibleRow::Child { y, .. } => *y,
                            };
                            assert!(y >= body.y && usize::from(y) < end);
                        }
                        if let Some(AgentVisibleRow::GroupHeader { y, .. }) = rows.first() {
                            assert_eq!(*y, body.y, "no leading gap");
                            populated_cases += 1;
                        }
                        cases += 1;
                    }
                }
            }
            assert!(
                agent_visible_rows_for_entries(&[], Rect::new(0, 0, 30, 8), 0, gap, &[]).is_empty()
            );
        }
        assert_eq!(cases, 360);
        assert!(
            populated_cases > 0,
            "the Cartesian table includes placed children"
        );
    }

    #[test]
    fn m828d1_gap_metrics_reach_the_last_child() {
        let mut app = AppState::test_new();
        app.workspaces = [1, 2, 1, 3]
            .into_iter()
            .enumerate()
            .map(|(index, count)| {
                let mut ws = Workspace::test_new(&format!("group-{index}"));
                for _ in 1..count {
                    ws.test_split(Direction::Horizontal);
                }
                ws
            })
            .collect();
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.detected_agent = Some(Agent::Claude);
        }
        app.agent_panel_sort = AgentPanelSort::Spaces;
        let entries = agent_panel_entries(&app);
        assert_eq!(entries.len(), 7);
        assert_eq!(
            entries.iter().map(|e| e.ws_idx).collect::<Vec<_>>(),
            vec![0, 1, 1, 2, 3, 3, 3]
        );
        for gap in [0, 2] {
            app.sidebar_agents.row_gap = gap;
            for height in [4, 8] {
                let area = Rect::new(0, 0, 30, height + AGENT_PANEL_HEADER_ROWS);
                let body = agent_panel_body_rect(area, false);
                let last = entries.len() - 1;
                let expected_max = (0..entries.len())
                    .find(|scroll| {
                        agent_visible_rows_for_entries(
                            &entries,
                            body,
                            *scroll,
                            gap,
                            &vec![1; entries.len()],
                        )
                        .iter()
                        .any(|row| {
                            matches!(row,
                            AgentVisibleRow::Child { entry_idx, .. } if *entry_idx == last)
                        })
                    })
                    .unwrap();
                assert!(expected_max > 0);
                app.agent_panel_scroll = usize::MAX;
                let metrics = agent_panel_scroll_metrics(&app, area);
                let rows = agent_visible_rows(&app, area);
                assert!(
                    rows.iter().any(|row| matches!(row,
                    AgentVisibleRow::Child { entry_idx, .. } if *entry_idx == last)),
                    "last child at stale-scroll-clamped page, gap={gap} height={height}: {rows:?}"
                );
                assert_eq!(metrics.max_offset_from_bottom, expected_max);
                assert_eq!(metrics.offset_from_bottom, 0);
                assert_eq!(metrics.viewport_rows, entries.len() - expected_max);
                assert_eq!(
                    rows,
                    agent_visible_rows_for_entries(
                        &entries,
                        agent_panel_body_rect(area, true),
                        expected_max,
                        gap,
                        &vec![1; entries.len()]
                    )
                );
                assert_eq!(
                    agent_children_placed_from(
                        &entries,
                        body,
                        expected_max,
                        gap,
                        &vec![1; entries.len()]
                    ),
                    metrics.viewport_rows
                );
                app.agent_panel_scroll = 0;
                assert_eq!(
                    agent_panel_scroll_metrics(&app, area).offset_from_bottom,
                    expected_max
                );
                app.agent_panel_scroll = expected_max;
                assert_eq!(agent_visible_rows(&app, area), rows);
            }
        }
    }

    #[test]
    fn m828d1_gap_positions_preserve_glyphs_and_active_backgrounds() {
        for custom in [false, true] {
            for indicator in [StatusIndicatorStyle::Dots, StatusIndicatorStyle::Symbols] {
                for gap in [0, 2] {
                    let mut app = AppState::test_new();
                    app.palette = Palette::rose_pine();
                    if custom {
                        app.palette.sidebar_bg = Color::Rgb(12, 34, 56);
                        app.palette.active_row_bg = Color::Rgb(65, 43, 21);
                        app.palette.yellow = Color::Rgb(190, 170, 20);
                        app.palette.red = Color::Rgb(180, 30, 40);
                        app.palette.green = Color::Rgb(20, 180, 80);
                        app.palette.teal = Color::Rgb(10, 160, 170);
                    } else {
                        assert_eq!(app.palette.surface_dim, Color::Rgb(38, 35, 58));
                    }
                    assert_ne!(app.palette.active_row_bg, app.palette.sidebar_bg);
                    app.status_indicators = indicator;
                    app.sidebar_agents.row_gap = gap;
                    app.sidebar_spaces.row_gap = gap;
                    app.workspaces = ["alpha", "beta"]
                        .into_iter()
                        .map(|name| {
                            let mut ws = Workspace::test_new(name);
                            ws.cached_git_branch = None;
                            ws.test_split(Direction::Horizontal);
                            ws
                        })
                        .collect();
                    app.ensure_test_terminals();
                    for terminal in app.terminals.values_mut() {
                        terminal.detected_agent = Some(Agent::Claude);
                    }
                    app.agent_panel_sort = AgentPanelSort::Spaces;
                    app.active = Some(0);
                    app.selected = 0;
                    app.mode = Mode::Terminal;
                    let entries = agent_panel_entries(&app);
                    assert_eq!(entries.len(), 4);
                    let states = [
                        (AgentState::Working, true),
                        (AgentState::Blocked, false),
                        (AgentState::Idle, true),
                        (AgentState::Idle, false),
                    ];
                    for (entry, (state, seen)) in entries.iter().zip(states) {
                        let pane = app.workspaces[entry.ws_idx].tabs[entry.tab_idx]
                            .panes
                            .get_mut(&entry.pane_id)
                            .unwrap();
                        pane.seen = seen;
                        app.terminals
                            .get_mut(&pane.attached_terminal_id)
                            .unwrap()
                            .state = state;
                    }
                    let active_pane = app.workspaces[0].focused_pane_id().unwrap();
                    let area = Rect::new(0, 0, 40, 46);
                    app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
                    assert_eq!(app.view.workspace_card_areas.len(), 2);
                    let (_, agents) = expanded_sidebar_sections(area, app.sidebar_section_split);
                    let rows = agent_visible_rows(&app, agents);
                    let children: Vec<_> = rows
                        .iter()
                        .filter_map(|row| match row {
                            AgentVisibleRow::Child {
                                entry_idx, y, last, ..
                            } => Some((*entry_idx, *y, *last)),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(children.len(), 4);
                    assert_eq!(children[1].1, children[0].1 + 1 + gap);
                    assert_eq!(children[2].1, children[1].1 + 2 + gap);
                    let mut terminal =
                        Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
                    terminal
                        .draw(|frame| {
                            render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area)
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let glyphs = if indicator == StatusIndicatorStyle::Dots {
                        ["●", "◉", "○", "○"]
                    } else {
                        ["◐", "×", "○", "✓"]
                    };
                    let colors = [
                        app.palette.yellow,
                        app.palette.red,
                        app.palette.green,
                        app.palette.teal,
                    ];
                    let labels = ["working", "blocked", "idle", "done"];
                    let body = agent_panel_body_rect(agents, false);
                    let mut highlighted = 0;
                    for (index, y, last) in children {
                        assert_eq!(buffer[(body.x, y)].symbol(), if last { "└" } else { "├" });
                        assert_eq!(buffer[(body.x + 1, y)].symbol(), "─");
                        assert_eq!(buffer[(body.x + 3, y)].symbol(), glyphs[index]);
                        assert_eq!(buffer[(body.x + 3, y)].fg, colors[index]);
                        let text: String = (body.x..body.x + body.width)
                            .map(|x| buffer[(x, y)].symbol())
                            .collect();
                        assert!(text.to_lowercase().contains("claude"));
                        assert!(text.contains(labels[index]));
                        let active = entries[index].pane_id == active_pane;
                        highlighted += usize::from(active);
                        for x in body.x..body.x + body.width {
                            assert_eq!(
                                buffer[(x, y)].bg,
                                if active {
                                    app.palette.active_row_bg
                                } else {
                                    app.palette.sidebar_bg
                                }
                            );
                        }
                    }
                    assert_eq!(highlighted, 1);
                    let mut blank_rows = 0;
                    for y in body.y
                        ..rows
                            .iter()
                            .map(|row| match row {
                                AgentVisibleRow::GroupHeader { y, .. }
                                | AgentVisibleRow::Child { y, .. } => *y,
                            })
                            .max()
                            .unwrap()
                    {
                        if rows.iter().any(|row| matches!(row,
                            AgentVisibleRow::GroupHeader { y: placed, .. } | AgentVisibleRow::Child { y: placed, .. } if *placed == y)) {
                            continue;
                        }
                        for x in body.x..body.x + body.width {
                            assert_eq!(buffer[(x, y)].symbol(), " ");
                            assert_eq!(buffer[(x, y)].bg, app.palette.sidebar_bg);
                        }
                        blank_rows += 1;
                    }
                    assert_eq!(blank_rows, 3 * usize::from(gap));
                    for card in &app.view.workspace_card_areas {
                        assert_eq!(
                            buffer[(card.rect.x, card.rect.y)].bg,
                            if card.ws_idx == 0 {
                                app.palette.active_row_bg
                            } else {
                                app.palette.sidebar_bg
                            }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn m828d1_gap_render_scales_on_fixed_geometry() {
        const AREA: Rect = Rect::new(0, 0, 80, 24);
        const WARMUPS: usize = 16;
        const SAMPLES: usize = 256;
        let mut observations = Vec::new();
        for count in [1, 15] {
            for gap in [0, 2] {
                let mut app = AppState::test_new();
                app.workspaces = (0..count)
                    .map(|index| {
                        let mut ws = Workspace::test_new(&format!("probe-{index}"));
                        ws.cached_git_branch = None;
                        ws
                    })
                    .collect();
                app.ensure_test_terminals();
                for terminal in app.terminals.values_mut() {
                    terminal.detected_agent = Some(Agent::Claude);
                    terminal.state = AgentState::Working;
                }
                app.active = Some(0);
                app.selected = 0;
                app.mode = Mode::Terminal;
                app.update_available = None;
                app.sidebar_agents.row_gap = gap;
                app.sidebar_spaces.row_gap = gap;
                app.agent_panel_sort = AgentPanelSort::Spaces;
                assert_eq!(agent_panel_entries(&app).len(), count);
                assert_eq!(app.terminals.len(), count);
                let mut terminal =
                    Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
                for _ in 0..WARMUPS {
                    crate::ui::compute_view(&mut app, AREA);
                    terminal
                        .draw(|frame| crate::ui::render(&app, frame))
                        .unwrap();
                }
                assert_eq!(app.view.layout, crate::app::state::ViewLayout::Desktop);
                let (_, agents) =
                    expanded_sidebar_sections(app.view.sidebar_rect, app.sidebar_section_split);
                let expected_rows = agent_visible_rows(&app, agents);
                let expected_cards: Vec<_> = app
                    .view
                    .workspace_card_areas
                    .iter()
                    .map(|card| (card.ws_idx, card.rect))
                    .collect();
                assert!(!expected_cards.is_empty());
                assert_eq!(expected_cards[0].0, 0);
                assert!(expected_rows
                    .iter()
                    .any(|row| matches!(row, AgentVisibleRow::Child { entry_idx: 0, .. })));
                let mut samples = Vec::with_capacity(SAMPLES);
                for _ in 0..SAMPLES {
                    let start = std::time::Instant::now();
                    crate::ui::compute_view(&mut app, AREA);
                    terminal
                        .draw(|frame| crate::ui::render(&app, frame))
                        .unwrap();
                    samples.push(start.elapsed().as_nanos());
                    assert_eq!(agent_visible_rows(&app, agents), expected_rows);
                    assert_eq!(
                        app.view
                            .workspace_card_areas
                            .iter()
                            .map(|card| (card.ws_idx, card.rect))
                            .collect::<Vec<_>>(),
                        expected_cards
                    );
                }
                assert_eq!(samples.len(), SAMPLES);
                let body = agent_panel_body_rect(
                    agents,
                    should_show_scrollbar(agent_panel_scroll_metrics(&app, agents)),
                );
                for row in &expected_rows {
                    let y = match row {
                        AgentVisibleRow::GroupHeader { y, .. }
                        | AgentVisibleRow::Child { y, .. } => *y,
                    };
                    assert!(y >= body.y && y < body.y + body.height);
                }
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(text.contains("probe-0"));
                assert!(text.to_lowercase().contains("claude"));
                observations.push(serde_json::json!({"panes": count, "row_gap": gap,
                    "width": AREA.width, "height": AREA.height, "warmups": WARMUPS,
                    "sample_count": SAMPLES, "samples_ns": samples,
                    "visible_cards": expected_cards.len(),
                    "visible_children": expected_rows.iter().filter(|row| matches!(row, AgentVisibleRow::Child { .. })).count(),
                    "route": "AppState populated with one pane per workspace; compute_view and public render into TestBackend; no live PTY"}));
            }
        }
        assert_eq!(observations.len(), 4);
        println!(
            "M828D1_GAP_RENDER {}",
            serde_json::to_string(&observations).unwrap()
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
            agent_kind_label: Some(agent.into()),
            state: AgentState::Idle,
            seen: true,
            last_agent_state_change_seq: None,
            agent: None,
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            tokens: std::collections::HashMap::new(),

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
        let rows = agent_visible_rows_for_entries(&entries, body, 0, 0, &vec![1; entries.len()]);

        let headers: Vec<_> = rows
            .iter()
            .filter(|r| matches!(r, AgentVisibleRow::GroupHeader { .. }))
            .collect();
        assert_eq!(headers.len(), 2, "two tab groups -> two headers: {rows:?}");

        let children: Vec<(usize, u16, bool)> = rows
            .iter()
            .filter_map(|r| match r {
                AgentVisibleRow::Child {
                    entry_idx, y, last, ..
                } => Some((*entry_idx, *y, *last)),
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
        let max = max_agent_panel_scroll(&entries, body, 0, &vec![1; entries.len()]);
        assert!(
            max > 0,
            "headers consume rows -> last child needs scrolling (max={max})"
        );
        let rows = agent_visible_rows_for_entries(&entries, body, max, 0, &vec![1; entries.len()]);
        let last = entries.len() - 1;
        assert!(
            rows.iter().any(
                |r| matches!(r, AgentVisibleRow::Child { entry_idx, .. } if *entry_idx == last)
            ),
            "last child reachable at max scroll: {rows:?}"
        );
        // metrics/rows consistency: children placed at the bottom == viewport (total - max).
        assert_eq!(
            agent_children_placed_from(&entries, body, max, 0, &vec![1; entries.len()]),
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
    fn m844_active_view_renders_label_and_empty_state() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        app.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "manual.view".to_string(),
            label: Some("blocked only".to_string()),
            filter: Some(crate::api::schema::AgentViewFilter::Eq {
                field: crate::api::schema::AgentViewField::Builtin(
                    crate::api::schema::AgentViewBuiltinField::Status,
                ),
                value: crate::api::schema::AgentViewValue::String("blocked".to_string()),
            }),
            sort: Vec::new(),
        });

        let joined = render_agent_detail_to_lines(&mut app, 30, 8).join("\n");
        assert!(joined.contains("blocked only"), "{joined}");
        assert!(joined.contains("no matching agents"), "{joined}");
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
