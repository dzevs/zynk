// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarTokenColor {
    r: u8,
    g: u8,
    b: u8,
}

impl SidebarTokenColor {
    pub(crate) fn ratatui(self) -> ratatui::style::Color {
        ratatui::style::Color::Rgb(self.r, self.g, self.b)
    }
}

impl Serialize for SidebarTokenColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b))
    }
}

impl<'de> Deserialize<'de> for SidebarTokenColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        fn nibble(byte: u8) -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            }
        }

        let value = String::deserialize(deserializer)?;
        let invalid = || serde::de::Error::custom("sidebar token fg must be #RGB or #RRGGBB");
        let Some(hex) = value.strip_prefix('#').filter(|hex| hex.is_ascii()) else {
            return Err(invalid());
        };
        let (r, g, b) = match hex.as_bytes() {
            [r, g, b] => (
                nibble(*r).ok_or_else(invalid)? * 17,
                nibble(*g).ok_or_else(invalid)? * 17,
                nibble(*b).ok_or_else(invalid)? * 17,
            ),
            [r1, r2, g1, g2, b1, b2] => (
                nibble(*r1).ok_or_else(invalid)? * 16 + nibble(*r2).ok_or_else(invalid)?,
                nibble(*g1).ok_or_else(invalid)? * 16 + nibble(*g2).ok_or_else(invalid)?,
                nibble(*b1).ok_or_else(invalid)? * 16 + nibble(*b2).ok_or_else(invalid)?,
            ),
            _ => return Err(invalid()),
        };
        Ok(Self { r, g, b })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidebarTokenStyle {
    pub fg: Option<SidebarTokenColor>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
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
    Styled {
        token: Box<AgentSidebarToken>,
        style: SidebarTokenStyle,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceSidebarToken {
    StateIcon,
    StateText,
    Workspace,
    Branch,
    GitStatus,
    Custom(String),
    Styled {
        token: Box<SpaceSidebarToken>,
        style: SidebarTokenStyle,
    },
}

impl AgentSidebarToken {
    pub(crate) fn parts(&self) -> (&Self, SidebarTokenStyle) {
        match self {
            Self::Styled { token, style } => (token, *style),
            token => (token, SidebarTokenStyle::default()),
        }
    }
}

impl SpaceSidebarToken {
    pub(crate) fn parts(&self) -> (&Self, SidebarTokenStyle) {
        match self {
            Self::Styled { token, style } => (token, *style),
            token => (token, SidebarTokenStyle::default()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStyledSidebarToken {
    token: String,
    #[serde(default)]
    fg: Option<SidebarTokenColor>,
    #[serde(default)]
    bold: Option<bool>,
    #[serde(default)]
    dim: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSidebarToken {
    Plain(String),
    Styled(RawStyledSidebarToken),
}

impl RawSidebarToken {
    fn parts(self) -> (String, Option<SidebarTokenStyle>) {
        match self {
            Self::Plain(token) => (token, None),
            Self::Styled(token) => (
                token.token,
                Some(SidebarTokenStyle {
                    fg: token.fg,
                    bold: token.bold,
                    dim: token.dim,
                }),
            ),
        }
    }
}

fn parse_sidebar_token<T>(value: String, builtins: &[(&str, T)]) -> Result<T, String>
where
    T: Clone + From<String>,
{
    if let Some((_, token)) = builtins.iter().find(|(name, _)| *name == value) {
        return Ok(token.clone());
    }
    let Some(name) = value.strip_prefix('$') else {
        return Err(format!(
            "unknown sidebar token `{value}`; custom tokens must start with `$`"
        ));
    };
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(format!("invalid custom sidebar token `{value}`"));
    }
    Ok(T::from(name.to_string()))
}

fn serialize_styled_token<S>(
    name: String,
    style: SidebarTokenStyle,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(None)?;
    map.serialize_entry("token", &name)?;
    if let Some(fg) = style.fg {
        map.serialize_entry("fg", &fg)?;
    }
    if let Some(bold) = style.bold {
        map.serialize_entry("bold", &bold)?;
    }
    if let Some(dim) = style.dim {
        map.serialize_entry("dim", &dim)?;
    }
    map.end()
}

fn agent_token_name(token: &AgentSidebarToken) -> String {
    match token {
        AgentSidebarToken::StateIcon => "state_icon".into(),
        AgentSidebarToken::StateText => "state_text".into(),
        AgentSidebarToken::Workspace => "workspace".into(),
        AgentSidebarToken::Tab => "tab".into(),
        AgentSidebarToken::Pane => "pane".into(),
        AgentSidebarToken::Agent => "agent".into(),
        AgentSidebarToken::TerminalTitle => "terminal_title".into(),
        AgentSidebarToken::TerminalTitleStripped => "terminal_title_stripped".into(),
        AgentSidebarToken::Custom(name) => format!("${name}"),
        AgentSidebarToken::Styled { token, .. } => agent_token_name(token),
    }
}

fn space_token_name(token: &SpaceSidebarToken) -> String {
    match token {
        SpaceSidebarToken::StateIcon => "state_icon".into(),
        SpaceSidebarToken::StateText => "state_text".into(),
        SpaceSidebarToken::Workspace => "workspace".into(),
        SpaceSidebarToken::Branch => "branch".into(),
        SpaceSidebarToken::GitStatus => "git_status".into(),
        SpaceSidebarToken::Custom(name) => format!("${name}"),
        SpaceSidebarToken::Styled { token, .. } => space_token_name(token),
    }
}

impl Serialize for AgentSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Styled { token, style } => {
                serialize_styled_token(agent_token_name(token), *style, serializer)
            }
            token => serializer.serialize_str(&agent_token_name(token)),
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
        let (value, style) = RawSidebarToken::deserialize(deserializer)?.parts();
        let token = parse_sidebar_token(
            value,
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
        .map_err(serde::de::Error::custom)?;
        Ok(style.map_or(token.clone(), |style| Self::Styled {
            token: Box::new(token),
            style,
        }))
    }
}

impl Serialize for SpaceSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Styled { token, style } => {
                serialize_styled_token(space_token_name(token), *style, serializer)
            }
            token => serializer.serialize_str(&space_token_name(token)),
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
        let (value, style) = RawSidebarToken::deserialize(deserializer)?.parts();
        let token = parse_sidebar_token(
            value,
            &[
                ("state_icon", Self::StateIcon),
                ("state_text", Self::StateText),
                ("workspace", Self::Workspace),
                ("branch", Self::Branch),
                ("git_status", Self::GitStatus),
            ],
        )
        .map_err(serde::de::Error::custom)?;
        Ok(style.map_or(token.clone(), |style| Self::Styled {
            token: Box::new(token),
            style,
        }))
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
    }

    #[test]
    fn m93_sidebar_occurrence_styles_preserve_plain_tokens_and_round_trip() {
        let config: SidebarConfig = toml::from_str(
            r##"
[agents]
rows = [[{ token = "workspace", fg = "#abc", bold = false }, "workspace"]]

[agents.rows_by_agent]
claude = [[{ token = "agent", fg = "#112233", bold = true, dim = false }]]

[spaces]
rows = [[{ token = "git_status", fg = "#ff00aa" }], [{ token = "$jj", dim = true }]]
"##,
        )
        .unwrap();

        let (token, style) = config.agents.rows[0][0].parts();
        assert_eq!(token, &AgentSidebarToken::Workspace);
        assert_eq!(style.bold, Some(false));
        assert_eq!(
            style.fg.map(SidebarTokenColor::ratatui),
            Some(ratatui::style::Color::Rgb(0xaa, 0xbb, 0xcc))
        );
        assert_eq!(config.agents.rows[0][1], AgentSidebarToken::Workspace);

        let (token, style) = config.agents.rows_by_agent["claude"][0][0].parts();
        assert_eq!(token, &AgentSidebarToken::Agent);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.dim, Some(false));

        let encoded = serde_json::to_value(&config).unwrap();
        assert_eq!(encoded["agents"]["rows"][0][1], "workspace");
        assert_eq!(
            encoded["agents"]["rows"][0][0],
            serde_json::json!({"token": "workspace", "fg": "#aabbcc", "bold": false})
        );
        assert_eq!(
            serde_json::from_value::<SidebarConfig>(encoded).unwrap(),
            config
        );
    }

    #[test]
    fn m93_sidebar_occurrence_styles_reject_invalid_colors_and_fields() {
        for entry in [
            r##"{ token = "workspace", fg = "red" }"##,
            r##"{ token = "workspace", fg = "#abcd" }"##,
            r##"{ token = "workspace", underline = true }"##,
        ] {
            let input = format!("[agents]\nrows = [[{entry}]]\n");
            assert!(
                toml::from_str::<SidebarConfig>(&input).is_err(),
                "accepted {entry}"
            );
        }
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
