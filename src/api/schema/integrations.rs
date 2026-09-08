use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationInstallParams {
    pub target: IntegrationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationUninstallParams {
    pub target: IntegrationTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationTarget {
    Pi,
    Omp,
    Claude,
    Codex,
    Copilot,
    Devin,
    Droid,
    Kimi,
    Opencode,
    Kilo,
    Hermes,
    Qodercli,
    Cursor,
    Mastracode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationInstallResult {
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationUninstallResult {
    pub messages: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characterization of the `IntegrationTarget` wire contract. The variant names
    /// reach the socket through `#[serde(rename_all = "snake_case")]`, so every id
    /// below is a published wire value: renaming a variant, reordering the enum, or
    /// adding one without its wire id breaks existing clients. Mirrors the `Method`
    /// `serde(rename)` characterization tests in `src/api/schema.rs`.
    #[test]
    fn integration_target_wire_ids_are_stable() {
        for (target, wire_id) in [
            (IntegrationTarget::Pi, "pi"),
            (IntegrationTarget::Omp, "omp"),
            (IntegrationTarget::Claude, "claude"),
            (IntegrationTarget::Codex, "codex"),
            (IntegrationTarget::Copilot, "copilot"),
            (IntegrationTarget::Devin, "devin"),
            (IntegrationTarget::Droid, "droid"),
            (IntegrationTarget::Kimi, "kimi"),
            (IntegrationTarget::Opencode, "opencode"),
            (IntegrationTarget::Kilo, "kilo"),
            (IntegrationTarget::Hermes, "hermes"),
            (IntegrationTarget::Qodercli, "qodercli"),
            (IntegrationTarget::Cursor, "cursor"),
            (IntegrationTarget::Mastracode, "mastracode"),
        ] {
            assert_eq!(
                serde_json::to_value(target).unwrap(),
                serde_json::Value::String(wire_id.to_string()),
                "{wire_id} integration target must serialize to its wire id"
            );
            assert_eq!(
                serde_json::from_value::<IntegrationTarget>(serde_json::json!(wire_id)).unwrap(),
                target,
                "{wire_id} must deserialize back to its integration target"
            );
        }
    }

    #[test]
    fn integration_install_params_round_trip_for_mastracode() {
        let params = IntegrationInstallParams {
            target: IntegrationTarget::Mastracode,
        };

        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["target"], "mastracode");
        let restored: IntegrationInstallParams = serde_json::from_value(json).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn integration_uninstall_params_round_trip_for_mastracode() {
        let params = IntegrationUninstallParams {
            target: IntegrationTarget::Mastracode,
        };

        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["target"], "mastracode");
        let restored: IntegrationUninstallParams = serde_json::from_value(json).unwrap();
        assert_eq!(restored, params);
    }
}
