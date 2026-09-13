use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema)]
pub struct PingParams {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerLiveHandoffParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerCapabilities {
    pub live_handoff: bool,
    #[serde(default)]
    pub detached_server_daemon: bool,
}

#[cfg(test)]
mod tests {
    use super::ServerCapabilities;

    #[test]
    fn server_capabilities_default_old_daemon_to_false() {
        let old: ServerCapabilities = serde_json::from_str(r#"{"live_handoff":true}"#).unwrap();
        let value = serde_json::to_value(old).unwrap();
        assert_eq!(value["live_handoff"], true);
        assert_eq!(value["detached_server_daemon"], false);
    }

    #[test]
    fn server_capabilities_schema_describes_optional_daemon_boolean() {
        let value = serde_json::to_value(schemars::schema_for!(ServerCapabilities)).unwrap();
        let field = &value["properties"]["detached_server_daemon"];
        assert_eq!(field["type"], "boolean");
        assert_eq!(field["default"], false);
        let required = value["required"].as_array().unwrap();
        assert!(required.iter().any(|name| name == "live_handoff"));
        assert!(!required.iter().any(|name| name == "detached_server_daemon"));
    }
}
