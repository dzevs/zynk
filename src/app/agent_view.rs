// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::cmp::Ordering;

use crate::api::schema::{
    AgentStatus, AgentViewBuiltinField, AgentViewBuiltinSortField, AgentViewContext,
    AgentViewField, AgentViewFilter, AgentViewSetParams, AgentViewSort, AgentViewSortField,
    AgentViewSortOrder, AgentViewValue,
};
use crate::ui::AgentPanelEntry;

use super::{AppState, Mode};

const MAX_FILTER_DEPTH: usize = 8;
const MAX_FILTER_NODES: usize = 64;
const MAX_FILTER_VALUES: usize = 32;
const MAX_SORT_FIELDS: usize = 8;
const MAX_SOURCE_CHARS: usize = 120;
const MAX_LABEL_CHARS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EvalValue {
    String(String),
    Bool(bool),
    Number(u64),
}

pub(crate) fn validate_agent_view(spec: &mut AgentViewSetParams) -> Result<(), String> {
    spec.source = normalize_source(&spec.source)?;
    spec.label = spec
        .label
        .take()
        .map(|label| normalize_label(&label))
        .transpose()?;

    let mut nodes = 0;
    if let Some(filter) = &spec.filter {
        validate_filter(filter, 1, &mut nodes)?;
    }
    if spec.sort.len() > MAX_SORT_FIELDS {
        return Err(format!(
            "agent view sort may contain at most {MAX_SORT_FIELDS} fields"
        ));
    }
    for sort in &spec.sort {
        validate_sort_field(&sort.field)?;
    }
    Ok(())
}

pub(crate) fn validate_agent_view_source(source: &str) -> Result<String, String> {
    normalize_source(source)
}

pub(crate) fn apply_agent_view(app: &AppState, entries: &mut Vec<AgentPanelEntry>) {
    if let Some(spec) = app.agent_view_override.as_ref() {
        if let Some(filter) = &spec.filter {
            entries.retain(|entry| matches_filter(app, entry, filter));
        }
        if !spec.sort.is_empty() {
            entries.sort_by(|left, right| compare_entries(app, left, right, &spec.sort));
            return;
        }
    }

    if matches!(
        app.agent_panel_sort,
        crate::app::state::AgentPanelSort::Priority
    ) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(agent_panel_attention_priority(entry.state, entry.seen)),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }
}

pub(crate) fn presented_workspace_idx(app: &AppState) -> Option<usize> {
    if app.mode == Mode::Navigate {
        app.workspaces.get(app.selected).map(|_| app.selected)
    } else {
        app.active
    }
}

fn normalize_source(source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty()
        || source.chars().count() > MAX_SOURCE_CHARS
        || !source
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '.' | '_' | '-'))
    {
        return Err(format!(
            "agent view source must be non-empty, at most {MAX_SOURCE_CHARS} characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen"
        ));
    }
    Ok(source.to_string())
}

fn normalize_label(label: &str) -> Result<String, String> {
    let label = label
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "agent view label must be non-empty and at most {MAX_LABEL_CHARS} characters"
        ));
    }
    Ok(label)
}

fn validate_filter(
    filter: &AgentViewFilter,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > MAX_FILTER_DEPTH {
        return Err(format!(
            "agent view filter may be nested at most {MAX_FILTER_DEPTH} levels"
        ));
    }
    *nodes += 1;
    if *nodes > MAX_FILTER_NODES {
        return Err(format!(
            "agent view filter may contain at most {MAX_FILTER_NODES} nodes"
        ));
    }

    match filter {
        AgentViewFilter::All { filters } | AgentViewFilter::Any { filters } => {
            if filters.is_empty() {
                return Err("agent view all/any filters must not be empty".to_string());
            }
            for filter in filters {
                validate_filter(filter, depth + 1, nodes)?;
            }
        }
        AgentViewFilter::Not { filter } => validate_filter(filter, depth + 1, nodes)?,
        AgentViewFilter::Eq { field, value } => validate_field_value(field, value)?,
        AgentViewFilter::In { field, values } => {
            if values.is_empty() || values.len() > MAX_FILTER_VALUES {
                return Err(format!(
                    "agent view in filters require 1 to {MAX_FILTER_VALUES} values"
                ));
            }
            for value in values {
                validate_field_value(field, value)?;
            }
        }
        AgentViewFilter::Exists { field } => validate_field(field)?,
    }
    Ok(())
}

fn validate_field(field: &AgentViewField) -> Result<(), String> {
    if let AgentViewField::Token { token } = field {
        validate_token(token)?;
    }
    Ok(())
}

fn validate_field_value(field: &AgentViewField, value: &AgentViewValue) -> Result<(), String> {
    validate_field(field)?;
    match (field, value) {
        (
            AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
            AgentViewValue::Context {
                context: AgentViewContext::CurrentWorkspaceId,
            },
        )
        | (
            AgentViewField::Builtin(AgentViewBuiltinField::TabId),
            AgentViewValue::Context {
                context: AgentViewContext::CurrentTabId,
            },
        ) => Ok(()),
        (_, AgentViewValue::Context { .. }) => {
            Err("agent view context type does not match the selected field".to_string())
        }
        (AgentViewField::Builtin(AgentViewBuiltinField::Seen), AgentViewValue::Bool(_))
        | (
            AgentViewField::Builtin(AgentViewBuiltinField::StateChangeSeq),
            AgentViewValue::Number(_),
        ) => Ok(()),
        (
            AgentViewField::Builtin(
                AgentViewBuiltinField::Status
                | AgentViewBuiltinField::WorkspaceId
                | AgentViewBuiltinField::TabId
                | AgentViewBuiltinField::PaneId
                | AgentViewBuiltinField::Agent,
            )
            | AgentViewField::Token { .. },
            AgentViewValue::String(value),
        ) => {
            if matches!(
                field,
                AgentViewField::Builtin(AgentViewBuiltinField::Status)
            ) && !matches!(
                value.as_str(),
                "idle" | "working" | "blocked" | "done" | "unknown"
            ) {
                return Err(format!("unknown agent status `{value}`"));
            }
            Ok(())
        }
        _ => Err("agent view value type does not match the selected field".to_string()),
    }
}

fn validate_sort_field(field: &AgentViewSortField) -> Result<(), String> {
    if let AgentViewSortField::Token { token } = field {
        validate_token(token)?;
    }
    Ok(())
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.is_empty()
        || token.len() > 32
        || !token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(format!("invalid agent view token `{token}`"));
    }
    Ok(())
}

fn matches_filter(app: &AppState, entry: &AgentPanelEntry, filter: &AgentViewFilter) -> bool {
    match filter {
        AgentViewFilter::All { filters } => filters
            .iter()
            .all(|filter| matches_filter(app, entry, filter)),
        AgentViewFilter::Any { filters } => filters
            .iter()
            .any(|filter| matches_filter(app, entry, filter)),
        AgentViewFilter::Not { filter } => !matches_filter(app, entry, filter),
        AgentViewFilter::Eq { field, value } => {
            field_value(app, entry, field) == operand_value(app, value)
        }
        AgentViewFilter::In { field, values } => {
            let actual = field_value(app, entry, field);
            values
                .iter()
                .any(|value| actual == operand_value(app, value))
        }
        AgentViewFilter::Exists { field } => field_value(app, entry, field).is_some(),
    }
}

fn compare_entries(
    app: &AppState,
    left: &AgentPanelEntry,
    right: &AgentPanelEntry,
    sorts: &[AgentViewSort],
) -> Ordering {
    for sort in sorts {
        let left = sort_value(app, left, &sort.field);
        let right = sort_value(app, right, &sort.field);
        let ordering = compare_optional_values(left, right, sort.order);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn compare_optional_values(
    left: Option<EvalValue>,
    right: Option<EvalValue>,
    order: AgentViewSortOrder,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            let ordering = left.cmp(&right);
            if matches!(order, AgentViewSortOrder::Desc) {
                ordering.reverse()
            } else {
                ordering
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn field_value(
    app: &AppState,
    entry: &AgentPanelEntry,
    field: &AgentViewField,
) -> Option<EvalValue> {
    match field {
        AgentViewField::Builtin(field) => builtin_field_value(app, entry, *field),
        AgentViewField::Token { token } => entry.tokens.get(token).cloned().map(EvalValue::String),
    }
}

fn builtin_field_value(
    app: &AppState,
    entry: &AgentPanelEntry,
    field: AgentViewBuiltinField,
) -> Option<EvalValue> {
    match field {
        AgentViewBuiltinField::Status => {
            Some(EvalValue::String(status_name(entry.state, entry.seen)))
        }
        AgentViewBuiltinField::WorkspaceId => app
            .workspaces
            .get(entry.ws_idx)
            .map(|workspace| EvalValue::String(workspace.id.clone())),
        AgentViewBuiltinField::TabId => public_tab_id(app, entry).map(EvalValue::String),
        AgentViewBuiltinField::PaneId => public_pane_id(app, entry).map(EvalValue::String),
        AgentViewBuiltinField::Agent => entry.agent_kind_label.clone().map(EvalValue::String),
        AgentViewBuiltinField::Seen => Some(EvalValue::Bool(entry.seen)),
        AgentViewBuiltinField::StateChangeSeq => {
            entry.last_agent_state_change_seq.map(EvalValue::Number)
        }
    }
}

fn operand_value(app: &AppState, value: &AgentViewValue) -> Option<EvalValue> {
    match value {
        AgentViewValue::String(value) => Some(EvalValue::String(value.clone())),
        AgentViewValue::Bool(value) => Some(EvalValue::Bool(*value)),
        AgentViewValue::Number(value) => Some(EvalValue::Number(*value)),
        AgentViewValue::Context { context } => context_value(app, *context),
    }
}

fn context_value(app: &AppState, context: AgentViewContext) -> Option<EvalValue> {
    let ws_idx = presented_workspace_idx(app)?;
    let workspace = app.workspaces.get(ws_idx)?;
    match context {
        AgentViewContext::CurrentWorkspaceId => Some(EvalValue::String(workspace.id.clone())),
        AgentViewContext::CurrentTabId => {
            let tab_number = workspace.public_tab_number(workspace.active_tab)?;
            Some(EvalValue::String(
                crate::workspace::public_tab_id_for_number(&workspace.id, tab_number),
            ))
        }
    }
}

fn sort_value(
    app: &AppState,
    entry: &AgentPanelEntry,
    field: &AgentViewSortField,
) -> Option<EvalValue> {
    match field {
        AgentViewSortField::Token { token } => {
            entry.tokens.get(token).cloned().map(EvalValue::String)
        }
        AgentViewSortField::Builtin(field) => match field {
            AgentViewBuiltinSortField::WorkspaceOrder => {
                Some(EvalValue::Number(entry.ws_idx as u64))
            }
            AgentViewBuiltinSortField::TabOrder => app
                .workspaces
                .get(entry.ws_idx)
                .and_then(|workspace| workspace.public_tab_number(entry.tab_idx))
                .map(|number| EvalValue::Number(number as u64)),
            AgentViewBuiltinSortField::PaneOrder => app
                .workspaces
                .get(entry.ws_idx)
                .and_then(|workspace| workspace.public_pane_number(entry.pane_id))
                .map(|number| EvalValue::Number(number as u64)),
            AgentViewBuiltinSortField::Attention => Some(EvalValue::Number(u64::from(
                agent_panel_attention_priority(entry.state, entry.seen),
            ))),
            AgentViewBuiltinSortField::Status => {
                Some(EvalValue::String(status_name(entry.state, entry.seen)))
            }
            AgentViewBuiltinSortField::Agent => {
                entry.agent_kind_label.clone().map(EvalValue::String)
            }
            AgentViewBuiltinSortField::Seen => Some(EvalValue::Bool(entry.seen)),
            AgentViewBuiltinSortField::StateChangeSeq => {
                entry.last_agent_state_change_seq.map(EvalValue::Number)
            }
        },
    }
}

fn agent_panel_attention_priority(state: crate::detect::AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (crate::detect::AgentState::Blocked, _) => 4,
        (crate::detect::AgentState::Working, _) => 3,
        (crate::detect::AgentState::Idle, false) => 2,
        (crate::detect::AgentState::Idle, true) => 1,
        (crate::detect::AgentState::Unknown, _) => 0,
    }
}

fn status_name(state: crate::detect::AgentState, seen: bool) -> String {
    let status = match (state, seen) {
        (crate::detect::AgentState::Idle, false) => AgentStatus::Done,
        (crate::detect::AgentState::Idle, true) => AgentStatus::Idle,
        (crate::detect::AgentState::Working, _) => AgentStatus::Working,
        (crate::detect::AgentState::Blocked, _) => AgentStatus::Blocked,
        (crate::detect::AgentState::Unknown, _) => AgentStatus::Unknown,
    };
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
    .to_string()
}

fn public_tab_id(app: &AppState, entry: &AgentPanelEntry) -> Option<String> {
    let workspace = app.workspaces.get(entry.ws_idx)?;
    let number = workspace.public_tab_number(entry.tab_idx)?;
    Some(crate::workspace::public_tab_id_for_number(
        &workspace.id,
        number,
    ))
}

fn public_pane_id(app: &AppState, entry: &AgentPanelEntry) -> Option<String> {
    let workspace = app.workspaces.get(entry.ws_idx)?;
    let number = workspace.public_pane_number(entry.pane_id)?;
    Some(crate::workspace::public_pane_id_for_number(
        &workspace.id,
        number,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    fn state_with_agents() -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        for (ws_idx, agent_state) in [(0, AgentState::Idle), (1, AgentState::Working)] {
            let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Claude), agent_state);
        }
        state
    }

    #[test]
    fn m844_recursive_filter_context_and_stable_sort_define_projection_only() {
        let mut state = state_with_agents();
        state.agent_view_override = Some(AgentViewSetParams {
            source: "plugin:focus".to_string(),
            label: Some("current".to_string()),
            filter: Some(AgentViewFilter::All {
                filters: vec![
                    AgentViewFilter::Eq {
                        field: AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
                        value: AgentViewValue::Context {
                            context: AgentViewContext::CurrentWorkspaceId,
                        },
                    },
                    AgentViewFilter::Not {
                        filter: Box::new(AgentViewFilter::Eq {
                            field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                            value: AgentViewValue::String("blocked".to_string()),
                        }),
                    },
                ],
            }),
            sort: vec![AgentViewSort {
                field: AgentViewSortField::Builtin(AgentViewBuiltinSortField::Status),
                order: AgentViewSortOrder::Asc,
            }],
        });

        let entries = crate::ui::agent_panel_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ws_idx, 0);
        let terminal = state.terminals.values().next().unwrap();
        assert!(terminal.hook_authority.is_none());
        assert!(terminal.hook_identity.is_none());

        state.agent_view_override = Some(AgentViewSetParams {
            source: "plugin:stable".to_string(),
            label: None,
            filter: None,
            sort: vec![AgentViewSort {
                field: AgentViewSortField::Builtin(AgentViewBuiltinSortField::Agent),
                order: AgentViewSortOrder::Asc,
            }],
        });
        let entries = crate::ui::agent_panel_entries(&state);
        assert_eq!(
            entries.iter().map(|entry| entry.ws_idx).collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn m844_validation_bounds_recursive_filters_values_and_tokens() {
        let mut nested = AgentViewFilter::Exists {
            field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
        };
        for _ in 0..MAX_FILTER_DEPTH {
            nested = AgentViewFilter::Not {
                filter: Box::new(nested),
            };
        }
        let mut view = AgentViewSetParams {
            source: "plugin:focus".to_string(),
            label: None,
            filter: Some(nested),
            sort: Vec::new(),
        };
        assert!(validate_agent_view(&mut view)
            .unwrap_err()
            .contains("nested at most"));

        let mut view = AgentViewSetParams {
            source: " plugin:focus ".to_string(),
            label: Some(" queue ".to_string()),
            filter: Some(AgentViewFilter::In {
                field: AgentViewField::Token {
                    token: "queue".to_string(),
                },
                values: (0..=MAX_FILTER_VALUES)
                    .map(|index| AgentViewValue::String(index.to_string()))
                    .collect(),
            }),
            sort: Vec::new(),
        };
        assert!(validate_agent_view(&mut view)
            .unwrap_err()
            .contains("require 1 to"));

        view.filter = Some(AgentViewFilter::Exists {
            field: AgentViewField::Token {
                token: "invalid token".to_string(),
            },
        });
        assert!(validate_agent_view(&mut view)
            .unwrap_err()
            .contains("invalid agent view token"));
    }
}
