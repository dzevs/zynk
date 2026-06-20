use serde::{Deserialize, Serialize};

// zynk fork (M3a): params for `zynk.message_received`. `Eq` is omitted because
// `receiver_agent_session` is a `serde_json::Value` (not `Eq`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZynkMessageReceivedParams {
    pub pane_id: String,
    pub message_id: String,
    pub conversation_id: String,
    pub conversation_seq: i64,
    pub runtime_session_id: String,
    pub socket_namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_session: Option<serde_json::Value>,
}
