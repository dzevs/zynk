use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentsSidebarConfig {
    pub row_gap: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SpacesSidebarConfig {
    pub row_gap: u16,
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
                serde_json::json!({"agents": {"row_gap": agents}, "spaces": {"row_gap": spaces}})
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
