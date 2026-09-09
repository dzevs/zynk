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

/// Debug-only test seam (ADR 0014). Integration tests report identity from the
/// harness process, which is outside every pane, so a server started with this
/// variable set to the literal `pane-child` treats each accepted connection as
/// the target pane's own child. The constant and its only reader are compiled
/// ONLY under `#[cfg(debug_assertions)]`, so neither the name nor the behaviour
/// exists in a release binary. `the_debug_peer_trust_seam_cannot_exist_in_a_release_build`
/// (`src/app/api/caller.rs`) pins that gating in the source, and the release-binary
/// string audit must assert the same absence in the built artifact.
#[cfg(debug_assertions)]
pub const TEST_TRUST_PEER_PID_ENV: &str = "ZYNK_TEST_TRUST_PEER_PID";

#[cfg(debug_assertions)]
fn accept_trusts_pane_child() -> bool {
    std::env::var(TEST_TRUST_PEER_PID_ENV).as_deref() == Ok("pane-child")
}

/// Who is on the other end of an API connection (ADR 0014).
///
/// `Default` is the fail-closed value — no peer, no trust — so any request that
/// did not arrive over the API socket is refused by the pane-bound methods.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApiCaller {
    /// Socket peer credentials read with `SO_PEERCRED` when the connection was
    /// accepted. `None` when the request did not arrive over the API socket, or
    /// when the kernel would not report them.
    pub peer: Option<crate::platform::PeerCredentials>,
    /// Debug-only: this connection is to be treated as the target pane's own
    /// child process. See `TEST_TRUST_PEER_PID_ENV`.
    #[cfg(debug_assertions)]
    pub trusted_as_pane_child: bool,
}

impl ApiCaller {
    /// The caller of a request that arrived on the API socket. The debug seam is
    /// resolved here, at accept, and never from anything in the request.
    pub(crate) fn from_socket(peer: Option<crate::platform::PeerCredentials>) -> Self {
        Self {
            peer,
            #[cfg(debug_assertions)]
            trusted_as_pane_child: accept_trusts_pane_child(),
        }
    }
}

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
    /// The connection this request arrived on (ADR 0014). Pane-bound methods
    /// refuse a caller that cannot be placed inside the target pane's tree.
    pub caller: ApiCaller,
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}
