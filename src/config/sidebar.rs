use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::detect::Agent;

const MAX_SIDEBAR_ROWS: usize = 16;
const MAX_SIDEBAR_TOKENS_PER_ROW: usize = 16;

fn deserialize_sidebar_rows<'de, D, T>(deserializer: D) -> Result<Vec<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let rows = Vec::<Vec<T>>::deserialize(deserializer)?;
    validate_sidebar_rows(&rows).map_err(serde::de::Error::custom)?;
    Ok(rows)
}

fn validate_sidebar_rows<T>(rows: &[Vec<T>]) -> Result<(), String> {
    if rows.len() > MAX_SIDEBAR_ROWS {
        return Err(format!(
            "sidebar layouts may contain at most {MAX_SIDEBAR_ROWS} rows"
        ));
    }
    if rows
        .iter()
        .any(|row| row.len() > MAX_SIDEBAR_TOKENS_PER_ROW)
    {
        return Err(format!(
            "sidebar rows may contain at most {MAX_SIDEBAR_TOKENS_PER_ROW} tokens"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSidebarToken {
    StateIcon,
    StateText,
    Workspace,
    Tab,
    Pane,
    Agent,
    TerminalTitle,
    TerminalTitleStripped,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceSidebarToken {
    StateIcon,
    StateText,
    Workspace,
    Branch,
    GitStatus,
    Custom(String),
}

fn parse_sidebar_token<'de, D, T>(deserializer: D, builtins: &[(&str, T)]) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Clone + From<String>,
{
    let value = String::deserialize(deserializer)?;
    if let Some((_, token)) = builtins.iter().find(|(name, _)| *name == value) {
        return Ok(token.clone());
    }
    let Some(name) = value.strip_prefix('$') else {
        return Err(serde::de::Error::custom(format!(
            "unknown sidebar token `{value}`; custom tokens must start with `$`"
        )));
    };
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(serde::de::Error::custom(format!(
            "invalid custom sidebar token `{value}`"
        )));
    }
    Ok(T::from(name.to_string()))
}

impl Serialize for AgentSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::StateIcon => serializer.serialize_str("state_icon"),
            Self::StateText => serializer.serialize_str("state_text"),
            Self::Workspace => serializer.serialize_str("workspace"),
            Self::Tab => serializer.serialize_str("tab"),
            Self::Pane => serializer.serialize_str("pane"),
            Self::Agent => serializer.serialize_str("agent"),
            Self::TerminalTitle => serializer.serialize_str("terminal_title"),
            Self::TerminalTitleStripped => serializer.serialize_str("terminal_title_stripped"),
            Self::Custom(name) => serializer.serialize_str(&format!("${name}")),
        }
    }
}

impl From<String> for AgentSidebarToken {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

impl<'de> Deserialize<'de> for AgentSidebarToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        parse_sidebar_token(
            deserializer,
            &[
                ("state_icon", Self::StateIcon),
                ("state_text", Self::StateText),
                ("workspace", Self::Workspace),
                ("tab", Self::Tab),
                ("pane", Self::Pane),
                ("agent", Self::Agent),
                ("terminal_title", Self::TerminalTitle),
                ("terminal_title_stripped", Self::TerminalTitleStripped),
            ],
        )
    }
}

impl Serialize for SpaceSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::StateIcon => serializer.serialize_str("state_icon"),
            Self::StateText => serializer.serialize_str("state_text"),
            Self::Workspace => serializer.serialize_str("workspace"),
            Self::Branch => serializer.serialize_str("branch"),
            Self::GitStatus => serializer.serialize_str("git_status"),
            Self::Custom(name) => serializer.serialize_str(&format!("${name}")),
        }
    }
}

impl From<String> for SpaceSidebarToken {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

impl<'de> Deserialize<'de> for SpaceSidebarToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        parse_sidebar_token(
            deserializer,
            &[
                ("state_icon", Self::StateIcon),
                ("state_text", Self::StateText),
                ("workspace", Self::Workspace),
                ("branch", Self::Branch),
                ("git_status", Self::GitStatus),
            ],
        )
    }
}

type AgentSidebarRows = Vec<Vec<AgentSidebarToken>>;
type SpaceSidebarRows = Vec<Vec<SpaceSidebarToken>>;

fn deserialize_rows_by_agent<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, AgentSidebarRows>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let rows_by_agent = BTreeMap::<String, AgentSidebarRows>::deserialize(deserializer)?;
    for (id, rows) in &rows_by_agent {
        if crate::detect::parse_canonical_agent_label(id).is_none() {
            return Err(serde::de::Error::custom(format!(
                "unknown canonical agent id `{id}` in sidebar rows_by_agent"
            )));
        }
        validate_sidebar_rows(rows).map_err(serde::de::Error::custom)?;
    }
    Ok(rows_by_agent)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentsSidebarConfig {
    pub row_gap: u16,
    #[serde(deserialize_with = "deserialize_sidebar_rows")]
    pub rows: AgentSidebarRows,
    #[serde(default, deserialize_with = "deserialize_rows_by_agent")]
    pub rows_by_agent: BTreeMap<String, AgentSidebarRows>,
}

impl AgentsSidebarConfig {
    pub(crate) fn rows_for_agent(&self, agent: Option<Agent>) -> &AgentSidebarRows {
        agent
            .and_then(|agent| self.rows_by_agent.get(crate::detect::agent_label(agent)))
            .unwrap_or(&self.rows)
    }
}

impl Default for AgentsSidebarConfig {
    fn default() -> Self {
        Self {
            row_gap: 0,
            rows: vec![vec![
                AgentSidebarToken::StateIcon,
                AgentSidebarToken::Agent,
                AgentSidebarToken::StateText,
            ]],
            rows_by_agent: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SpacesSidebarConfig {
    pub row_gap: u16,
    #[serde(deserialize_with = "deserialize_sidebar_rows")]
    pub rows: SpaceSidebarRows,
}

impl Default for SpacesSidebarConfig {
    fn default() -> Self {
        Self {
            row_gap: 0,
            rows: vec![
                vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
                vec![SpaceSidebarToken::Branch, SpaceSidebarToken::GitStatus],
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SidebarConfig {
    pub agents: AgentsSidebarConfig,
    pub spaces: SpacesSidebarConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m828d2_defaults_empty_layouts_and_complete_serialization() {
        let defaults = SidebarConfig::default();
        assert_eq!(
            serde_json::to_value(&defaults).unwrap(),
            serde_json::json!({
                "agents": {"row_gap": 0, "rows": [["state_icon", "agent", "state_text"]], "rows_by_agent": {}},
                "spaces": {"row_gap": 0, "rows": [["state_icon", "workspace"], ["branch", "git_status"]]}
            })
        );
        assert_eq!(toml::from_str::<SidebarConfig>("").unwrap(), defaults);
        let encoded = toml::to_string(&defaults).unwrap();
        assert_eq!(toml::from_str::<SidebarConfig>(&encoded).unwrap(), defaults);
        assert_eq!(
            defaults.agents.rows,
            vec![vec![
                AgentSidebarToken::StateIcon,
                AgentSidebarToken::Agent,
                AgentSidebarToken::StateText,
            ]]
        );
        assert_eq!(
            defaults.spaces.rows,
            vec![
                vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
                vec![SpaceSidebarToken::Branch, SpaceSidebarToken::GitStatus],
            ]
        );
        for rows in ["[]", "[[]]"] {
            let input = format!(
                "[agents]\nrow_gap = 2\nrows = {rows}\n[spaces]\nrow_gap = 3\nrows = {rows}\n"
            );
            assert!(input.parse::<toml::Value>().is_ok());
            let config: SidebarConfig = toml::from_str(&input).unwrap();
            let expected: serde_json::Value = serde_json::from_str(rows).unwrap();
            assert_eq!(
                serde_json::to_value(&config).unwrap(),
                serde_json::json!({
                    "agents": {"row_gap": 2, "rows": expected, "rows_by_agent": {}},
                    "spaces": {"row_gap": 3, "rows": expected}
                })
            );
            assert_ne!(config.agents.rows, defaults.agents.rows);
            assert_ne!(config.spaces.rows, defaults.spaces.rows);
            let encoded = toml::to_string(&config).unwrap();
            assert_eq!(toml::from_str::<SidebarConfig>(&encoded).unwrap(), config);

            let input = format!(
                "[agents]\nrows = [[\"agent\"]]\n[agents.rows_by_agent]\nclaude = {rows}\n"
            );
            assert!(input.parse::<toml::Value>().is_ok());
            let config: SidebarConfig = toml::from_str(&input).unwrap();
            assert_eq!(config.agents.rows, vec![vec![AgentSidebarToken::Agent]]);
            assert_eq!(
                serde_json::to_value(config.agents.rows_for_agent(Some(Agent::Claude))).unwrap(),
                expected
            );
            assert_eq!(config.agents.rows_for_agent(None), &config.agents.rows);
            assert_eq!(
                config.agents.rows_for_agent(Some(Agent::Pi)),
                &config.agents.rows
            );
            assert_ne!(
                config.agents.rows_for_agent(Some(Agent::Claude)),
                &config.agents.rows
            );
            assert_eq!(config.spaces, defaults.spaces);
            let encoded = toml::to_string(&config).unwrap();
            assert_eq!(toml::from_str::<SidebarConfig>(&encoded).unwrap(), config);
        }
        let key = "A".repeat(32);
        let input = format!("[agents]\nrows = [[\"${key}\"]]\n[spaces]\nrows = [[\"${key}\"]]\n");
        assert!(input.parse::<toml::Value>().is_ok());
        let config: SidebarConfig = toml::from_str(&input).unwrap();
        assert_eq!(
            config.agents.rows,
            vec![vec![AgentSidebarToken::Custom(key.clone())]]
        );
        assert_eq!(
            config.spaces.rows,
            vec![vec![SpaceSidebarToken::Custom(key)]]
        );
        let encoded = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<SidebarConfig>(&encoded).unwrap(), config);
    }

    #[test]
    fn m828d2_every_token_round_trips_as_a_canonical_string() {
        use crate::config::{AgentSidebarToken as AgentToken, SpaceSidebarToken as SpaceToken};
        for (name, token) in [
            ("state_icon", AgentToken::StateIcon),
            ("state_text", AgentToken::StateText),
            ("workspace", AgentToken::Workspace),
            ("tab", AgentToken::Tab),
            ("pane", AgentToken::Pane),
            ("agent", AgentToken::Agent),
            ("terminal_title", AgentToken::TerminalTitle),
            ("terminal_title_stripped", AgentToken::TerminalTitleStripped),
            ("$MiXeD_9-key", AgentToken::Custom("MiXeD_9-key".into())),
            (
                "$terminal_title",
                AgentToken::Custom("terminal_title".into()),
            ),
        ] {
            let value = serde_json::json!(name);
            assert_eq!(serde_json::to_value(&token).unwrap(), value);
            assert_eq!(serde_json::from_value::<AgentToken>(value).unwrap(), token);
        }
        for (name, token) in [
            ("state_icon", SpaceToken::StateIcon),
            ("state_text", SpaceToken::StateText),
            ("workspace", SpaceToken::Workspace),
            ("branch", SpaceToken::Branch),
            ("git_status", SpaceToken::GitStatus),
            ("$MiXeD_9-key", SpaceToken::Custom("MiXeD_9-key".into())),
        ] {
            let value = serde_json::json!(name);
            assert_eq!(serde_json::to_value(&token).unwrap(), value);
            assert_eq!(serde_json::from_value::<SpaceToken>(value).unwrap(), token);
        }
        let source = "[agents]\nrows = [[\"agent\", \"state_icon\", \"agent\"], [\"$terminal_title\", \"terminal_title_stripped\", \"terminal_title\"]]\n[spaces]\nrows = [[\"git_status\", \"workspace\", \"workspace\", \"state_icon\"]]\n";
        assert!(source.parse::<toml::Value>().is_ok());
        let config: SidebarConfig = toml::from_str(source).unwrap();
        assert_eq!(
            config.agents.rows,
            vec![
                vec![AgentToken::Agent, AgentToken::StateIcon, AgentToken::Agent],
                vec![
                    AgentToken::Custom("terminal_title".into()),
                    AgentToken::TerminalTitleStripped,
                    AgentToken::TerminalTitle
                ],
            ]
        );
        assert_eq!(
            config.spaces.rows,
            vec![vec![
                SpaceToken::GitStatus,
                SpaceToken::Workspace,
                SpaceToken::Workspace,
                SpaceToken::StateIcon
            ]]
        );
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            serde_json::json!({
                "agents": {"row_gap": 0, "rows": [["agent", "state_icon", "agent"], ["$terminal_title", "terminal_title_stripped", "terminal_title"]], "rows_by_agent": {}},
                "spaces": {"row_gap": 0, "rows": [["git_status", "workspace", "workspace", "state_icon"]]}
            })
        );
        assert!(
            serde_json::from_value::<AgentToken>(serde_json::json!({"token": "agent"})).is_err()
        );
        assert!(
            serde_json::from_value::<SpaceToken>(serde_json::json!({"token": "workspace"}))
                .is_err()
        );
    }

    #[test]
    fn m828d1_gap_defaults_and_value_round_trips() {
        let defaults = SidebarConfig::default();
        assert_eq!(defaults.agents.row_gap, 0);
        assert_eq!(defaults.spaces.row_gap, 0);
        assert_eq!(toml::from_str::<SidebarConfig>("").unwrap(), defaults);
        for (agents, spaces) in [(0, 0), (2, 7), (65535, 1), (3, 65535)] {
            let input = format!("[agents]\nrow_gap = {agents}\n[spaces]\nrow_gap = {spaces}\n");
            let config: SidebarConfig = toml::from_str(&input).unwrap();
            assert_eq!(config.agents.row_gap, agents);
            assert_eq!(config.spaces.row_gap, spaces);
            let encoded = toml::to_string(&config).unwrap();
            assert_eq!(toml::from_str::<SidebarConfig>(&encoded).unwrap(), config);
            assert_eq!(
                serde_json::to_value(&config).unwrap(),
                serde_json::json!({
                    "agents": {"row_gap": agents, "rows": [["state_icon", "agent", "state_text"]], "rows_by_agent": {}},
                    "spaces": {"row_gap": spaces, "rows": [["state_icon", "workspace"], ["branch", "git_status"]]}
                })
            );
        }
        let agents: SidebarConfig = toml::from_str("[agents]\nrow_gap = 5\n").unwrap();
        assert_eq!(agents.agents.row_gap, 5);
        assert_eq!(agents.spaces.row_gap, 0);
        let spaces: SidebarConfig = toml::from_str("[spaces]\nrow_gap = 6\n").unwrap();
        assert_eq!(spaces.spaces.row_gap, 6);
        assert_eq!(spaces.agents.row_gap, 0);
    }
}
