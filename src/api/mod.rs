pub mod client;
mod event_hub;
pub mod schema;
mod server;
mod status;
mod subscriptions;
mod wait;

pub use event_hub::EventHub;
pub use server::{start_server, start_server_with_capabilities, ServerHandle};
pub use status::{read_runtime_status_at, RuntimeStatus};

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::api::schema::{Method, Request};

/// Zynk-branded API-socket-path override (ADR 0007 §5): the primary, documented name.
pub const ZYNK_SOCKET_PATH_ENV_VAR: &str = "ZYNK_SOCKET_PATH";
/// Transitional `ZYNK_*` compat alias for the API-socket-path override. Kept
/// working; `ZYNK_SOCKET_PATH` wins when both are set.
pub const SOCKET_PATH_ENV_VAR: &str = "ZYNK_SOCKET_PATH";

/// Resolve the API-socket-path override, preferring the Zynk-branded
/// `ZYNK_SOCKET_PATH` over the retained `ZYNK_SOCKET_PATH` compat alias.
pub fn socket_path_override() -> Option<String> {
    crate::config::env_first(&[ZYNK_SOCKET_PATH_ENV_VAR])
}

/// True when any API-socket-path override (Zynk-branded or compat) is set.
pub fn socket_path_override_present() -> bool {
    std::env::var_os(ZYNK_SOCKET_PATH_ENV_VAR).is_some()
        || std::env::var_os(SOCKET_PATH_ENV_VAR).is_some()
}

pub(crate) fn request_changes_ui(request: &Request) -> bool {
    matches!(
        &request.method,
        Method::ServerReloadConfig(_)
            | Method::ServerReloadAgentManifests(_)
            | Method::NotificationShow(_)
            | Method::WorkspaceCreate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
            | Method::WorkspaceClose(_)
            | Method::WorktreeCreate(_)
            | Method::WorktreeOpen(_)
            | Method::WorktreeRemove(_)
            | Method::TabCreate(_)
            | Method::TabFocus(_)
            | Method::TabRename(_)
            | Method::TabClose(_)
            | Method::LayoutApply(_)
            | Method::AgentRename(_)
            | Method::AgentFocus(_)
            | Method::AgentStart(_)
            | Method::PaneSplit(_)
            | Method::PaneSwap(_)
            | Method::PaneMove(_)
            | Method::PaneZoom(_)
            | Method::PaneFocusDirection(_)
            | Method::PaneResize(_)
            | Method::PaneRename(_)
            | Method::PaneReportAgent(_)
            | Method::PaneReportAgentSession(_)
            | Method::PaneReportMetadata(_)
            | Method::PaneClearAgentAuthority(_)
            | Method::PaneReleaseAgent(_)
            | Method::PaneClose(_)
            | Method::PluginActionInvoke(_)
            | Method::PluginPaneOpen(_)
            | Method::PluginPaneFocus(_)
            | Method::PluginPaneClose(_)
    )
}

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}
