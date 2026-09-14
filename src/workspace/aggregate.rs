// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::collections::HashMap;

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::terminal::{TerminalId, TerminalState};

use super::{Tab, Workspace};

/// Detail info for a single pane, used by the agent detail panel.
pub struct PaneDetail {
    pub pane_id: PaneId,
    pub tab_idx: usize,
    pub tab_label: String,
    pub label: String,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: String,
    pub agent: Option<Agent>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub custom_status: Option<String>,
    pub state_labels: HashMap<String, String>,
    pub tokens: HashMap<String, String>,
}

impl Tab {
    fn pane_details(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        tab_idx: usize,
        tab_label: &str,
    ) -> Vec<PaneDetail> {
        self.layout
            .pane_ids()
            .iter()
            .filter_map(|id| {
                let pane = self.panes.get(id)?;
                let terminal = terminals.get(&pane.attached_terminal_id)?;
                let fallback_agent_label = terminal
                    .agent_name
                    .as_deref()
                    .or_else(|| terminal.effective_agent_label())?
                    .to_string();
                let agent_label = terminal
                    .effective_display_agent()
                    .unwrap_or_else(|| fallback_agent_label.clone());
                let presentation = terminal.effective_presentation();
                Some(PaneDetail {
                    pane_id: *id,
                    tab_idx,
                    tab_label: tab_label.to_string(),
                    label: agent_label.clone(),
                    pane_label: terminal
                        .effective_title()
                        .or_else(|| terminal.manual_label.clone()),
                    terminal_title: terminal.terminal_title().map(str::to_owned),
                    terminal_title_stripped: terminal.terminal_title_stripped(),
                    agent_label,
                    agent: terminal.effective_known_agent(),
                    state: terminal.state,
                    seen: pane.seen,
                    last_agent_state_change_seq: terminal.last_agent_state_change_seq,
                    custom_status: presentation.custom_status,
                    state_labels: presentation.state_labels,
                    tokens: terminal.metadata_tokens.values(),
                })
            })
            .collect()
    }
}

fn pane_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Working, _) => 3,
        (AgentState::Idle, false) => 2, // done / unseen
        (AgentState::Idle, true) => 1,  // idle / seen
        (AgentState::Unknown, _) => 0,
    }
}

impl Workspace {
    pub fn aggregate_state(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> (AgentState, bool) {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .filter_map(|pane| {
                terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| (terminal.state, pane.seen))
            })
            .max_by_key(|(state, seen)| pane_attention_priority(*state, *seen))
            .unwrap_or((AgentState::Unknown, true))
    }

    pub fn pane_details(&self, terminals: &HashMap<TerminalId, TerminalState>) -> Vec<PaneDetail> {
        let multi_tab = self.tabs.len() > 1;
        self.tabs
            .iter()
            .enumerate()
            .flat_map(|(tab_idx, tab)| {
                let tab_label = self
                    .tab_display_name(tab_idx)
                    .unwrap_or_else(|| (tab_idx + 1).to_string());
                tab.pane_details(terminals, tab_idx, &tab_label).into_iter()
            })
            .map(|mut detail| {
                if multi_tab {
                    detail.label = format!("{}·{}", detail.tab_label, detail.agent_label);
                }
                detail
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Direction;

    use super::*;
    use crate::detect::Agent;

    fn terminal_for_pane(ws: &Workspace, pane_id: PaneId) -> TerminalState {
        TerminalState::new(ws.terminal_id(pane_id).unwrap().clone(), "/tmp".into())
    }

    #[test]
    fn m828d2_pane_projection_keeps_distinct_label_title_and_token_sources() {
        use crate::terminal::AgentMetadataReport;

        for (known, effective, manual, expected_pane) in [
            (
                Some(Agent::Claude),
                Some("effective-title"),
                Some("manual-pane"),
                Some("effective-title"),
            ),
            (
                Some(Agent::Pi),
                None,
                Some("fallback-pane"),
                Some("fallback-pane"),
            ),
            (None, None, None, None),
        ] {
            let mut workspace = Workspace::test_new("workspace-source");
            workspace.tabs[0].custom_name = Some("explicit-tab".into());
            let pane_id = workspace.tabs[0].root_pane;
            workspace.tabs[0].panes.get_mut(&pane_id).unwrap().seen = false;
            let mut terminal = terminal_for_pane(&workspace, pane_id);
            terminal.set_detected_state(known, AgentState::Working);
            terminal.set_agent_name("fallback-agent".into());
            if let Some(manual) = manual {
                terminal.set_manual_label(manual.into());
            }
            terminal.set_agent_metadata(AgentMetadataReport {
                source: "projection-fixture".into(),
                agent_label: None,
                applies_to_source: None,
                title: effective.map(str::to_owned),
                display_agent: Some("renamed-display".into()),
                custom_status: Some("legacy-status".into()),
                state_labels: HashMap::from([("working".into(), "legacy-working".into())]),
                clear_title: false,
                clear_display_agent: false,
                clear_custom_status: false,
                clear_state_labels: false,
                ttl: None,
                seq: Some(1),
            });
            let change = terminal.set_terminal_title(Some("\u{280b} osc-observation".into()));
            assert!(change.raw_changed);
            assert!(change.stripped_changed);
            let tokens = HashMap::from([
                ("build".into(), "custom-build".into()),
                ("terminal_title".into(), "custom-title".into()),
            ]);
            assert!(terminal.metadata_tokens.patch(
                tokens
                    .iter()
                    .map(|(key, value): (&String, &String)| (key.clone(), Some(value.clone())))
                    .collect(),
                None,
                std::time::Instant::now(),
            ));
            assert_eq!(terminal.state, AgentState::Working);
            assert_eq!(terminal.effective_known_agent(), known);
            assert_eq!(
                terminal.effective_display_agent().as_deref(),
                Some("renamed-display")
            );
            assert_eq!(terminal.effective_title().as_deref(), effective);
            assert_eq!(terminal.manual_label.as_deref(), manual);
            assert_eq!(terminal.terminal_title(), Some("\u{280b} osc-observation"));
            assert_eq!(
                terminal.terminal_title_stripped().as_deref(),
                Some("osc-observation")
            );
            assert_eq!(terminal.metadata_tokens.values(), tokens);
            let presentation = terminal.effective_presentation();
            assert_eq!(presentation.custom_status.as_deref(), Some("legacy-status"));
            assert_eq!(
                presentation.state_labels,
                HashMap::from([("working".into(), "legacy-working".into())])
            );
            assert_eq!(
                workspace.tab_display_name(0).as_deref(),
                Some("explicit-tab")
            );
            let mut app = crate::app::AppState::test_new();
            app.terminals = HashMap::from([(terminal.id.clone(), terminal)]);
            app.workspaces = vec![workspace];
            let details = app.workspaces[0].pane_details(&app.terminals);
            assert_eq!(details.len(), 1);
            let detail = &details[0];
            assert_eq!(detail.pane_id, pane_id);
            assert_eq!(detail.tab_label, "explicit-tab");
            assert_eq!(detail.agent, known);
            assert_eq!(detail.agent_label, "renamed-display");
            assert_eq!(detail.pane_label.as_deref(), expected_pane);
            assert_eq!(
                detail.terminal_title.as_deref(),
                Some("\u{280b} osc-observation")
            );
            assert_eq!(
                detail.terminal_title_stripped.as_deref(),
                Some("osc-observation")
            );
            assert_eq!(detail.tokens, tokens);
            assert_eq!(detail.custom_status, presentation.custom_status);
            assert_eq!(detail.state_labels, presentation.state_labels);
            assert_eq!(detail.state, AgentState::Working);
            assert!(!detail.seen);

            let entries = crate::ui::agent_panel_entries(&app);
            assert_eq!(entries.len(), 1);
            let entry = &entries[0];
            assert_eq!(entry.pane_id, pane_id);
            assert_eq!(entry.primary_label, "workspace-source");
            assert_eq!(entry.primary_tab_label.as_deref(), Some("explicit-tab"));
            assert_eq!(entry.agent, known);
            assert_eq!(entry.agent_label.as_deref(), Some("renamed-display"));
            assert_eq!(entry.pane_label, detail.pane_label);
            assert_eq!(entry.terminal_title, detail.terminal_title);
            assert_eq!(
                entry.terminal_title_stripped,
                detail.terminal_title_stripped
            );
            assert_eq!(entry.tokens, tokens);
            assert_eq!(entry.custom_status, detail.custom_status);
            assert_eq!(entry.state_labels, detail.state_labels);
        }
    }

    #[test]
    fn aggregate_state_all_unknown() {
        let ws = Workspace::test_new("test");
        let mut terminals = HashMap::new();
        let root = ws.tabs[0].root_pane;
        let terminal = terminal_for_pane(&ws, root);
        terminals.insert(terminal.id.clone(), terminal);
        let (state, seen) = ws.aggregate_state(&terminals);
        assert_eq!(state, AgentState::Unknown);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_priority() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);

        let (state, seen) = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Working);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_working_beats_done_unseen() {
        // Follow-up semantic change: an active Working agent outranks a done/unseen one, so a
        // workspace mixing both rolls up to Working (Blocked still wins above; see below).
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);
        let root = ws.tabs[0].panes.get_mut(&root_id).unwrap();
        root.seen = false;

        let (state, seen) = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Working);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_blocked_beats_working() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Blocked;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);

        let (state, _seen) = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Blocked);
    }

    #[test]
    fn pane_details_prefers_agent_name_over_detected_agent_label() {
        let ws = Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, root_pane);
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("planner".into());
        terminals.insert(terminal.id.clone(), terminal);

        let labels: Vec<_> = ws
            .pane_details(&terminals)
            .into_iter()
            .map(|detail| (detail.label, detail.agent_label, detail.agent))
            .collect();

        assert_eq!(
            labels,
            vec![("planner".into(), "planner".into(), Some(Agent::Pi))]
        );
    }

    #[test]
    fn pane_details_includes_tab_context_for_multi_tab_workspace() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].custom_name = Some("main".into());
        let root_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("review"));
        let review_pane = ws.tabs[second_tab].root_pane;
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_pane);
        root_terminal.set_hook_authority(
            "test".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut review_terminal = terminal_for_pane(&ws, review_pane);
        review_terminal.set_hook_authority(
            "test".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
        );
        terminals.insert(review_terminal.id.clone(), review_terminal);

        let labels: Vec<_> = ws
            .pane_details(&terminals)
            .into_iter()
            .map(|detail| (detail.label, detail.agent_label, detail.agent))
            .collect();

        assert_eq!(
            labels,
            vec![
                ("main·pi".into(), "pi".into(), Some(Agent::Pi)),
                ("review·claude".into(), "claude".into(), Some(Agent::Claude)),
            ]
        );
    }

    #[test]
    fn pane_details_use_tab_vector_index_not_stable_public_tab_number() {
        let mut ws = Workspace::test_new("test");
        let removed_tab = ws.test_add_tab(Some("removed"));
        let survivor_tab = ws.test_add_tab(Some("survivor"));
        let survivor_pane = ws.tabs[survivor_tab].root_pane;
        assert!(ws.close_tab(removed_tab));

        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, survivor_pane);
        terminal.detected_agent = Some(Agent::Codex);
        terminals.insert(terminal.id.clone(), terminal);

        let details = ws.pane_details(&terminals);
        let survivor = details
            .iter()
            .find(|detail| detail.pane_id == survivor_pane)
            .expect("surviving tab agent should be listed");

        assert_eq!(ws.tabs[1].number, 3);
        assert_eq!(survivor.tab_idx, 1);
    }
}
