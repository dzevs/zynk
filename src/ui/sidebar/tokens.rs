use crate::config::{
    AgentSidebarToken, AgentsSidebarConfig, SpaceSidebarToken, SpacesSidebarConfig,
};

use super::AgentPanelEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedToken {
    StateIcon,
    StateText(String),
    Workspace(String),
    Tab(String),
    Pane(String),
    Agent(String),
    TerminalTitle(String),
    Branch(String),
    GitStatus { ahead: usize, behind: usize },
    Custom(String),
}

pub(super) fn agent_rows(
    config: &AgentsSidebarConfig,
    entry: &AgentPanelEntry,
    state_text: &str,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows_for_agent(entry.agent)
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|token| match token {
                    AgentSidebarToken::StateIcon => Some(ResolvedToken::StateIcon),
                    AgentSidebarToken::StateText => {
                        Some(ResolvedToken::StateText(state_text.to_string()))
                    }
                    AgentSidebarToken::Workspace => {
                        Some(ResolvedToken::Workspace(entry.primary_label.clone()))
                    }
                    AgentSidebarToken::Tab => {
                        entry.primary_tab_label.clone().map(ResolvedToken::Tab)
                    }
                    AgentSidebarToken::Pane => entry.pane_label.clone().map(ResolvedToken::Pane),
                    AgentSidebarToken::Agent => entry.agent_label.clone().map(ResolvedToken::Agent),
                    AgentSidebarToken::TerminalTitle => entry
                        .terminal_title
                        .clone()
                        .map(ResolvedToken::TerminalTitle),
                    AgentSidebarToken::TerminalTitleStripped => entry
                        .terminal_title_stripped
                        .clone()
                        .map(ResolvedToken::TerminalTitle),
                    AgentSidebarToken::Custom(name) => {
                        entry.tokens.get(name).cloned().map(ResolvedToken::Custom)
                    }
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(super) struct SpaceTokenContext<'a> {
    pub workspace: &'a str,
    pub branch: Option<&'a str>,
    pub state_text: &'a str,
    pub ahead_behind: Option<(usize, usize)>,
    pub tokens: &'a std::collections::HashMap<String, String>,
    pub suppress_git_details: bool,
}

pub(super) fn space_rows(
    config: &SpacesSidebarConfig,
    context: SpaceTokenContext<'_>,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|token| match token {
                    SpaceSidebarToken::StateIcon => Some(ResolvedToken::StateIcon),
                    SpaceSidebarToken::StateText => {
                        Some(ResolvedToken::StateText(context.state_text.to_string()))
                    }
                    SpaceSidebarToken::Workspace => {
                        Some(ResolvedToken::Workspace(context.workspace.to_string()))
                    }
                    SpaceSidebarToken::Branch if !context.suppress_git_details => context
                        .branch
                        .map(|branch| ResolvedToken::Branch(branch.to_string())),
                    SpaceSidebarToken::Branch => None,
                    SpaceSidebarToken::GitStatus if !context.suppress_git_details => context
                        .ahead_behind
                        .filter(|(ahead, behind)| *ahead > 0 || *behind > 0)
                        .map(|(ahead, behind)| ResolvedToken::GitStatus { ahead, behind }),
                    SpaceSidebarToken::GitStatus => None,
                    SpaceSidebarToken::Custom(name) => {
                        context.tokens.get(name).cloned().map(ResolvedToken::Custom)
                    }
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(super) fn separator(previous: &ResolvedToken, current: &ResolvedToken) -> &'static str {
    if matches!(previous, ResolvedToken::StateIcon)
        || matches!(current, ResolvedToken::GitStatus { .. })
    {
        " "
    } else {
        " \u{b7} "
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use std::collections::{BTreeMap, HashMap};

    fn entry() -> AgentPanelEntry {
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            primary_label: "workspace-value".into(),
            primary_tab_label: Some("tab-value".into()),
            tab_label: "group-header-value".into(),
            agent_label: Some("renamed-claude".into()),
            agent: Some(Agent::Claude),
            pane_label: Some("pane-value".into()),
            terminal_title: Some("\u{280b} raw-title".into()),
            terminal_title_stripped: Some("raw-title".into()),
            tokens: HashMap::from([
                ("wanted".into(), "custom-wanted".into()),
                ("decoy".into(), "custom-decoy".into()),
                ("terminal_title".into(), "custom-title".into()),
            ]),
            state: AgentState::Working,
            seen: true,
            last_agent_state_change_seq: None,

            state_labels: HashMap::new(),
        }
    }

    #[test]
    fn m828d2_agent_rows_resolve_builtin_custom_and_missing_values() {
        use AgentSidebarToken as A;
        use ResolvedToken as R;
        let populated = entry();
        assert_eq!(populated.tokens["wanted"], "custom-wanted");
        assert_eq!(populated.tokens["decoy"], "custom-decoy");
        assert_eq!(populated.tokens["terminal_title"], "custom-title");
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                A::StateIcon,
                A::StateText,
                A::Workspace,
                A::Tab,
                A::Pane,
                A::Agent,
                A::TerminalTitle,
                A::TerminalTitleStripped,
                A::Custom("wanted".into()),
                A::Custom("terminal_title".into()),
            ]],
            ..Default::default()
        };
        assert_eq!(
            agent_rows(&config, &populated, "status-value"),
            vec![vec![
                R::StateIcon,
                R::StateText("status-value".into()),
                R::Workspace("workspace-value".into()),
                R::Tab("tab-value".into()),
                R::Pane("pane-value".into()),
                R::Agent("renamed-claude".into()),
                R::TerminalTitle("\u{280b} raw-title".into()),
                R::TerminalTitle("raw-title".into()),
                R::Custom("custom-wanted".into()),
                R::Custom("custom-title".into()),
            ]]
        );
        for (token, expected) in [
            (A::Tab, R::Tab("tab-value".into())),
            (A::Pane, R::Pane("pane-value".into())),
            (A::Agent, R::Agent("renamed-claude".into())),
            (
                A::TerminalTitle,
                R::TerminalTitle("\u{280b} raw-title".into()),
            ),
            (
                A::TerminalTitleStripped,
                R::TerminalTitle("raw-title".into()),
            ),
            (
                A::Custom("wanted".into()),
                R::Custom("custom-wanted".into()),
            ),
        ] {
            let mut value = entry();
            let config = AgentsSidebarConfig {
                rows: vec![
                    vec![A::StateIcon, token.clone(), A::Workspace],
                    vec![token.clone()],
                ],
                ..Default::default()
            };
            assert_eq!(
                agent_rows(&config, &value, "working"),
                vec![
                    vec![
                        R::StateIcon,
                        expected.clone(),
                        R::Workspace("workspace-value".into())
                    ],
                    vec![expected],
                ]
            );
            let empty = match &token {
                A::Tab => {
                    value.primary_tab_label = Some(String::new());
                    R::Tab(String::new())
                }
                A::Pane => {
                    value.pane_label = Some(String::new());
                    R::Pane(String::new())
                }
                A::Agent => {
                    value.agent_label = Some(String::new());
                    R::Agent(String::new())
                }
                A::TerminalTitle => {
                    value.terminal_title = Some(String::new());
                    R::TerminalTitle(String::new())
                }
                A::TerminalTitleStripped => {
                    value.terminal_title_stripped = Some(String::new());
                    R::TerminalTitle(String::new())
                }
                A::Custom(key) => {
                    value.tokens.insert(key.clone(), String::new());
                    R::Custom(String::new())
                }
                _ => unreachable!(),
            };
            assert_eq!(
                agent_rows(&config, &value, "working"),
                vec![
                    vec![
                        R::StateIcon,
                        empty.clone(),
                        R::Workspace("workspace-value".into())
                    ],
                    vec![empty],
                ]
            );
            match &token {
                A::Tab => value.primary_tab_label = None,
                A::Pane => value.pane_label = None,
                A::Agent => value.agent_label = None,
                A::TerminalTitle => value.terminal_title = None,
                A::TerminalTitleStripped => value.terminal_title_stripped = None,
                A::Custom(key) => {
                    value.tokens.remove(key);
                }
                _ => unreachable!(),
            }
            let rows = agent_rows(&config, &value, "working");
            assert_eq!(
                rows,
                vec![vec![R::StateIcon, R::Workspace("workspace-value".into())]]
            );
            assert_eq!(separator(&rows[0][0], &rows[0][1]), " ");
            assert_eq!(value.tokens["decoy"], "custom-decoy");
        }
        let empty = AgentsSidebarConfig {
            rows: vec![vec![], vec![A::Custom("absent".into())]],
            ..Default::default()
        };
        assert!(agent_rows(&empty, &populated, "working").is_empty());
    }

    #[test]
    fn m828d2_agent_overrides_use_enum_not_display_rename() {
        use AgentSidebarToken as A;
        use ResolvedToken as R;
        let mut value = entry();
        value.tokens.insert("global".into(), "GLOBAL".into());
        value.tokens.insert("special".into(), "OVERRIDE".into());
        let mut config = AgentsSidebarConfig {
            rows: vec![vec![A::Custom("global".into())]],
            rows_by_agent: BTreeMap::from([(
                "claude".into(),
                vec![vec![A::Agent, A::Custom("special".into())]],
            )]),
            ..Default::default()
        };
        assert_eq!(value.agent, Some(Agent::Claude));
        assert_eq!(value.agent_label.as_deref(), Some("renamed-claude"));
        assert_eq!(value.tokens["global"], "GLOBAL");
        assert_eq!(value.tokens["special"], "OVERRIDE");
        assert_eq!(
            agent_rows(&config, &value, "working"),
            vec![vec![
                R::Agent("renamed-claude".into()),
                R::Custom("OVERRIDE".into())
            ]]
        );
        value.agent = None;
        value.agent_label = Some("claude".into());
        assert_eq!(
            agent_rows(&config, &value, "working"),
            vec![vec![R::Custom("GLOBAL".into())]]
        );
        value.agent = Some(Agent::Pi);
        assert_eq!(
            agent_rows(&config, &value, "working"),
            vec![vec![R::Custom("GLOBAL".into())]]
        );
        value.agent = Some(Agent::Claude);
        assert_eq!(
            agent_rows(&config, &value, "working"),
            vec![vec![
                R::Agent("claude".into()),
                R::Custom("OVERRIDE".into())
            ]]
        );
        config.rows_by_agent.insert("claude".into(), vec![]);
        assert!(agent_rows(&config, &value, "working").is_empty());
        config.rows_by_agent.insert("claude".into(), vec![vec![]]);
        assert!(agent_rows(&config, &value, "working").is_empty());
        value.agent = None;
        assert_eq!(
            agent_rows(&config, &value, "working"),
            vec![vec![R::Custom("GLOBAL".into())]]
        );
    }

    #[test]
    fn m828d2_space_rows_suppress_only_grouped_builtin_git_values() {
        use ResolvedToken as R;
        use SpaceSidebarToken as S;
        let tokens = HashMap::from([
            ("wanted".into(), "custom-space".into()),
            ("decoy".into(), "decoy-space".into()),
            ("branch".into(), "custom-branch".into()),
            ("git_status".into(), "custom-counters".into()),
        ]);
        assert_eq!(tokens["wanted"], "custom-space");
        assert_eq!(tokens["decoy"], "decoy-space");
        let config = SpacesSidebarConfig {
            rows: vec![
                vec![
                    S::StateIcon,
                    S::StateText,
                    S::Workspace,
                    S::Branch,
                    S::GitStatus,
                    S::Custom("wanted".into()),
                    S::Custom("branch".into()),
                    S::Custom("git_status".into()),
                    S::Custom("absent".into()),
                ],
                vec![S::Custom("absent".into())],
            ],
            ..Default::default()
        };
        let resolve = |branch, ahead_behind, suppress_git_details| {
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace: "workspace-label",
                    branch,
                    state_text: "state-label",
                    ahead_behind,
                    tokens: &tokens,
                    suppress_git_details,
                },
            )
        };
        let visible = vec![
            R::StateIcon,
            R::StateText("state-label".into()),
            R::Workspace("workspace-label".into()),
            R::Branch("feature-branch".into()),
            R::GitStatus {
                ahead: 2,
                behind: 3,
            },
            R::Custom("custom-space".into()),
            R::Custom("custom-branch".into()),
            R::Custom("custom-counters".into()),
        ];
        assert_eq!(
            resolve(Some("feature-branch"), Some((2, 3)), false),
            vec![visible.clone()]
        );
        let nongit = vec![
            R::StateIcon,
            R::StateText("state-label".into()),
            R::Workspace("workspace-label".into()),
            R::Custom("custom-space".into()),
            R::Custom("custom-branch".into()),
            R::Custom("custom-counters".into()),
        ];
        assert_eq!(
            resolve(Some("feature-branch"), Some((2, 3)), true),
            vec![nongit.clone()]
        );
        for (branch, counters) in [
            (None, Some((2, 3))),
            (Some("feature-branch"), None),
            (Some("feature-branch"), Some((0, 0))),
        ] {
            assert_eq!(
                resolve(Some("feature-branch"), Some((2, 3)), false),
                vec![visible.clone()]
            );
            let mut expected = visible.clone();
            if branch.is_none() {
                expected.retain(|token| !matches!(token, R::Branch(_)));
            }
            if counters.is_none() || counters == Some((0, 0)) {
                expected.retain(|token| !matches!(token, R::GitStatus { .. }));
            }
            assert_eq!(resolve(branch, counters, false), vec![expected]);
        }
        for (ahead, behind) in [(0, 3), (2, 0)] {
            let mut expected = visible.clone();
            expected[4] = R::GitStatus { ahead, behind };
            assert_eq!(
                resolve(Some("feature-branch"), Some((ahead, behind)), false),
                vec![expected]
            );
        }
        let rows = resolve(None, None, false);
        assert_eq!(rows, vec![nongit]);
        assert_eq!(
            rows[0]
                .windows(2)
                .map(|pair| separator(&pair[0], &pair[1]))
                .collect::<Vec<_>>(),
            vec![" ", " \u{b7} ", " \u{b7} ", " \u{b7} ", " \u{b7} "]
        );
        let custom_only = SpacesSidebarConfig {
            rows: vec![vec![S::Custom("wanted".into())]],
            ..Default::default()
        };
        assert_eq!(
            space_rows(
                &custom_only,
                SpaceTokenContext {
                    workspace: "workspace-label",
                    branch: None,
                    state_text: "state-label",
                    ahead_behind: None,
                    tokens: &tokens,
                    suppress_git_details: true,
                }
            ),
            vec![vec![R::Custom("custom-space".into())]]
        );
    }
}
