// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use tracing::{debug, error, info, warn};

#[cfg(test)]
use std::fs;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, Request, ResponseResult, ServerCapabilities, SuccessResponse,
};
use crate::api::subscriptions::ActiveSubscription;
use crate::api::wait::{wait_for_event, wait_for_output};
use crate::api::{
    request_changes_ui, socket_path, ApiCaller, ApiRequestMessage, ApiRequestSender, EventHub,
};
use crate::ipc::{
    bind_local_listener, is_connection_closed_error, local_stream_peer_closed,
    poll_local_stream_read, remove_socket_file_if_owned, set_local_stream_polling,
    socket_file_identity, LocalStream, LocalStreamRead, SocketFileIdentity,
};

const SOCKET_PERMISSION_MODE: u32 = 0o600;
pub(super) const CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(super) const APP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_INITIAL_REQUEST_BYTES: usize = 1024 * 1024;

pub struct ServerHandle {
    _thread: std::thread::JoinHandle<()>,
    path: PathBuf,
    identity: SocketFileIdentity,
    running: Arc<AtomicBool>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);

        if let Err(err) = self.remove_socket_file_if_owned() {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.path.display(), err = %err, "failed to remove api socket on shutdown");
            }
        }
    }
}

impl ServerHandle {
    pub(crate) fn remove_socket_file_if_owned(&self) -> std::io::Result<()> {
        remove_socket_file_if_owned(&self.path, &self.identity)
    }
}

pub(crate) fn start_server_with_stop_control(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    server_stop: Arc<AtomicBool>,
) -> std::io::Result<ServerHandle> {
    start_server_inner(api_tx, event_hub, default_capabilities(), Some(server_stop))
}

pub fn start_server_with_capabilities(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    capabilities: Option<ServerCapabilities>,
) -> std::io::Result<ServerHandle> {
    start_server_inner(api_tx, event_hub, capabilities, None)
}

fn default_capabilities() -> Option<ServerCapabilities> {
    Some(ServerCapabilities {
        live_handoff: crate::platform::capabilities().live_handoff,
        detached_server_daemon: crate::platform::current_process_is_detached_server_daemon(),
    })
}

fn start_server_inner(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    capabilities: Option<ServerCapabilities>,
    server_stop: Option<Arc<AtomicBool>>,
) -> std::io::Result<ServerHandle> {
    let path = socket_path();
    prepare_socket_path(&path)?;

    let listener = bind_local_listener(&path)?;
    restrict_socket_permissions(&path)?;
    let identity = socket_file_identity(&path)?;
    info!(path = %path.display(), "api server listening");

    let running = Arc::new(AtomicBool::new(true));
    let listener_running = Arc::clone(&running);
    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let api_tx = api_tx.clone();
                    let event_hub = event_hub.clone();
                    let capabilities = capabilities.clone();
                    let server_stop = server_stop.clone();
                    let connection_running = Arc::clone(&listener_running);
                    std::thread::spawn(move || {
                        if let Err(err) = handle_connection_with_stop(
                            stream,
                            &api_tx,
                            &event_hub,
                            &connection_running,
                            capabilities,
                            server_stop.as_ref(),
                        ) {
                            warn!(err = %err, "api connection failed");
                        }
                    });
                }
                Err(err) => {
                    error!(err = %err, "api listener accept failed");
                    break;
                }
            }
        }
        debug!("api server thread exiting");
    });

    Ok(ServerHandle {
        _thread: thread,
        path,
        identity,
        running,
    })
}

fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    crate::ipc::prepare_socket_path(path, |path| {
        format!(
            "zynk is already running (socket busy at {})",
            path.display()
        )
    })
}

fn restrict_socket_permissions(path: &Path) -> std::io::Result<()> {
    crate::ipc::restrict_socket_permissions(path, SOCKET_PERMISSION_MODE)
}

#[cfg(test)]
fn handle_connection(
    stream: LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    capabilities: Option<ServerCapabilities>,
) -> std::io::Result<()> {
    handle_connection_with_stop(stream, api_tx, event_hub, running, capabilities, None)
}

fn handle_connection_with_stop(
    mut stream: LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    capabilities: Option<ServerCapabilities>,
    server_stop: Option<&Arc<AtomicBool>>,
) -> std::io::Result<()> {
    if let Err(err) = stream.set_send_timeout(Some(STREAM_WRITE_TIMEOUT)) {
        debug!(err = %err, "api connection write timeout unavailable");
    }

    // ADR 0014: the caller's identity comes from the kernel, at accept, for the
    // whole connection — never from anything the request carries.
    let caller = ApiCaller::from_socket(crate::ipc::stream_peer_credentials(&stream));

    let Some(line) = read_initial_request_line(&mut stream)? else {
        return Ok(());
    };

    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let request = match serde_json::from_str::<Request>(line) {
        Ok(request) => request,
        Err(err) => {
            write_json_line_allow_disconnect(
                &mut stream,
                &ErrorResponse {
                    id: String::new(),
                    error: ErrorBody {
                        code: "invalid_request".into(),
                        message: format!("invalid request: {err}"),
                    },
                },
            )?;
            return Ok(());
        }
    };

    let request_id = request.id.clone();
    let method = api_method_name(&request.method);
    let changes_ui = request_changes_ui(&request);
    crate::logging::api_request_started(&request_id, method, changes_ui);

    // Fork adaptation: subscriptions and event/output waits branch before
    // `handle_request`, so they need the same stop preflight here.
    if let Some(response) = priority_stop_response(&request, server_stop) {
        let result = write_text_line_allow_disconnect(&mut stream, &response);
        match &result {
            Ok(()) => crate::logging::api_request_completed(
                &request_id,
                method,
                api_response_outcome(&response),
                changes_ui,
            ),
            Err(err) => crate::logging::api_request_failed(&request_id, method, &err.to_string()),
        }
        return result;
    }

    match request.method {
        Method::EventsSubscribe(params) => {
            let result = stream_subscriptions(
                stream,
                request_id.clone(),
                params,
                api_tx,
                event_hub,
                running,
            );
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "stream_closed",
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
        Method::EventsWait(params) => {
            let Some(response) = wait_for_event(
                request_id.clone(),
                params,
                &mut stream,
                api_tx,
                event_hub,
                running,
            )?
            else {
                crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "client_disconnected",
                    changes_ui,
                );
                return Ok(());
            };
            let result = write_text_line_allow_disconnect(&mut stream, &response);
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    api_response_outcome(&response),
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
        Method::PaneWaitForOutput(params) => {
            let Some(response) =
                wait_for_output(request_id.clone(), params, &mut stream, api_tx, running)?
            else {
                crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "client_disconnected",
                    changes_ui,
                );
                return Ok(());
            };
            let result = write_text_line_allow_disconnect(&mut stream, &response);
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    api_response_outcome(&response),
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
        method_body => {
            let response = handle_request(
                Request {
                    id: request_id.clone(),
                    method: method_body,
                },
                api_tx,
                capabilities,
                caller,
                server_stop,
            );
            let result = write_text_line_allow_disconnect(&mut stream, &response);
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    api_response_outcome(&response),
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
    }
}

fn handle_request(
    request: Request,
    api_tx: &ApiRequestSender,
    capabilities: Option<ServerCapabilities>,
    caller: ApiCaller,
    server_stop: Option<&Arc<AtomicBool>>,
) -> String {
    if matches!(&request.method, Method::Ping(_)) {
        return serde_json::to_string(&SuccessResponse {
            id: request.id,
            result: ResponseResult::Pong {
                version: crate::build_info::version(),
                protocol: crate::protocol::PROTOCOL_VERSION,
                capabilities,
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
                .to_string()
        });
    }

    // Keep this upstream-shaped inner fence even though production socket
    // requests also pass the outer preflight in `handle_connection_with_stop`.
    if let Some(response) = priority_stop_response(&request, server_stop) {
        return response;
    }

    dispatch_to_app(request, api_tx, caller)
}

fn priority_stop_response(
    request: &Request,
    server_stop: Option<&Arc<AtomicBool>>,
) -> Option<String> {
    if matches!(&request.method, Method::Ping(_)) {
        return None;
    }

    if matches!(&request.method, Method::ServerStop(_)) {
        let server_stop = server_stop?;
        server_stop.store(true, Ordering::Release);
        return Some(
            serde_json::to_string(&SuccessResponse {
                id: request.id.clone(),
                result: ResponseResult::Ok {},
            })
            .unwrap_or_else(|_| "{}".to_string()),
        );
    }

    server_stop
        .is_some_and(|stop| stop.load(Ordering::Acquire))
        .then(|| {
            error_response_json(
                request.id.clone(),
                "server_unavailable",
                "server is shutting down".into(),
            )
        })
}

fn api_method_name(method: &Method) -> &'static str {
    match method {
        Method::Ping(_) => "ping",
        Method::SessionSnapshot(_) => "session.snapshot",
        Method::ServerStop(_) => "server.stop",
        Method::ServerLiveHandoff(_) => "server.live_handoff",
        Method::ServerReloadConfig(_) => "server.reload_config",
        Method::ServerAgentManifests(_) => "server.agent_manifests",
        Method::ServerReloadAgentManifests(_) => "server.reload_agent_manifests",
        Method::NotificationShow(_) => "notification.show",
        Method::ClientWindowTitleSet(_) => "client.window_title.set",
        Method::ClientWindowTitleClear(_) => "client.window_title.clear",
        Method::WorkspaceCreate(_) => "workspace.create",
        Method::WorkspaceList(_) => "workspace.list",
        Method::WorkspaceGet(_) => "workspace.get",
        Method::WorkspaceFocus(_) => "workspace.focus",
        Method::WorkspaceRename(_) => "workspace.rename",
        Method::WorkspaceMove(_) => "workspace.move",
        Method::WorkspaceReportMetadata(_) => "workspace.report_metadata",
        Method::WorkspaceClose(_) => "workspace.close",
        Method::WorktreeList(_) => "worktree.list",
        Method::WorktreeCreate(_) => "worktree.create",
        Method::WorktreeOpen(_) => "worktree.open",
        Method::WorktreeRemove(_) => "worktree.remove",
        Method::TabCreate(_) => "tab.create",
        Method::TabList(_) => "tab.list",
        Method::TabGet(_) => "tab.get",
        Method::TabFocus(_) => "tab.focus",
        Method::TabRename(_) => "tab.rename",
        Method::TabMove(_) => "tab.move",
        Method::TabClose(_) => "tab.close",
        Method::AgentList(_) => "agent.list",
        Method::AgentGet(_) => "agent.get",
        Method::AgentRead(_) => "agent.read",
        Method::AgentExplain(_) => "agent.explain",
        Method::AgentSend(_) => "agent.send",
        Method::AgentRename(_) => "agent.rename",
        Method::AgentFocus(_) => "agent.focus",
        Method::AgentStart(_) => "agent.start",
        Method::PaneSplit(_) => "pane.split",
        Method::PaneSwap(_) => "pane.swap",
        Method::PaneMove(_) => "pane.move",
        Method::PaneZoom(_) => "pane.zoom",
        Method::PaneLayout(_) => "pane.layout",
        Method::PaneProcessInfo(_) => "pane.process_info",
        Method::LayoutExport(_) => "layout.export",
        Method::LayoutApply(_) => "layout.apply",
        Method::LayoutSetSplitRatio(_) => "layout.set_split_ratio",
        Method::PaneNeighbor(_) => "pane.neighbor",
        Method::PaneEdges(_) => "pane.edges",
        Method::PaneFocusDirection(_) => "pane.focus_direction",
        Method::PaneResize(_) => "pane.resize",
        Method::PaneList(_) => "pane.list",
        Method::PaneCurrent(_) => "pane.current",
        Method::PaneGet(_) => "pane.get",
        Method::PaneFocus(_) => "pane.focus",
        Method::PaneRename(_) => "pane.rename",
        Method::PaneSendText(_) => "pane.send_text",
        Method::PaneSendKeys(_) => "pane.send_keys",
        Method::PaneSendInput(_) => "pane.send_input",
        Method::PaneRead(_) => "pane.read",
        Method::PaneReportAgent(_) => "pane.report_agent",
        Method::PaneReportAgentSession(_) => "pane.report_agent_session",
        Method::PaneReportMetadata(_) => "pane.report_metadata",
        Method::PaneClearAgentAuthority(_) => "pane.clear_agent_authority",
        Method::PaneReleaseAgent(_) => "pane.release_agent",
        Method::PaneClose(_) => "pane.close",
        Method::EventsSubscribe(_) => "events.subscribe",
        Method::EventsWait(_) => "events.wait",
        Method::PaneWaitForOutput(_) => "pane.wait_for_output",
        Method::IntegrationInstall(_) => "integration.install",
        Method::IntegrationUninstall(_) => "integration.uninstall",
        Method::PluginLink(_) => "plugin.link",
        Method::PluginList(_) => "plugin.list",
        Method::PluginUnlink(_) => "plugin.unlink",
        Method::PluginEnable(_) => "plugin.enable",
        Method::PluginDisable(_) => "plugin.disable",
        Method::PluginActionList(_) => "plugin.action.list",
        Method::PluginActionInvoke(_) => "plugin.action.invoke",
        Method::PluginLogList(_) => "plugin.log.list",
        Method::PluginPaneOpen(_) => "plugin.pane.open",
        Method::PluginPaneFocus(_) => "plugin.pane.focus",
        Method::PluginPaneClose(_) => "plugin.pane.close",
        Method::ZynkMessageReceived(_) => "zynk.message_received",
    }
}

fn api_response_outcome(response: &str) -> &'static str {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(response) else {
        return "error";
    };

    match value
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_str())
    {
        Some("timeout") => "timeout",
        Some(_) => "error",
        None => "ok",
    }
}

fn read_initial_request_line(stream: &mut LocalStream) -> std::io::Result<Option<String>> {
    read_initial_request_line_with_timeout(stream, INITIAL_REQUEST_TIMEOUT)
}

fn read_initial_request_line_with_timeout(
    stream: &mut LocalStream,
    timeout: Duration,
) -> std::io::Result<Option<String>> {
    read_initial_request_line_with_limits(stream, timeout, MAX_INITIAL_REQUEST_BYTES)
}

fn read_initial_request_line_with_limits(
    stream: &mut LocalStream,
    timeout: Duration,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    set_local_stream_polling(stream, true)?;
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];

    let result = loop {
        let read = match poll_local_stream_read(stream, &mut byte) {
            Ok(read) => read,
            Err(err) => break Err(err),
        };
        match read {
            LocalStreamRead::Closed => break Ok(None),
            LocalStreamRead::Data => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break String::from_utf8(bytes)
                        .map(Some)
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
                }
                if bytes.len() > max_bytes {
                    break Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "api request line is too large",
                    ));
                }
            }
            LocalStreamRead::Pending => {
                if Instant::now() >= deadline {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out reading api request",
                    ));
                }
                std::thread::sleep(CONNECTION_POLL_INTERVAL);
            }
        }
    };
    set_local_stream_polling(stream, false)?;
    result
}

fn stream_subscriptions(
    mut stream: LocalStream,
    request_id: String,
    params: crate::api::schema::EventsSubscribeParams,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    let mut subscriptions = Vec::with_capacity(params.subscriptions.len());
    for (index, subscription) in params.subscriptions.into_iter().enumerate() {
        let active =
            match ActiveSubscription::new(subscription, &request_id, index, api_tx, event_hub) {
                Ok(active) => active,
                Err(response) => {
                    if let Err(err) = write_json_line(&mut stream, &response) {
                        if is_connection_closed_error(&err) {
                            return Ok(());
                        }
                        return Err(err);
                    }
                    return Ok(());
                }
            };
        subscriptions.push(active);
    }

    if let Err(err) = write_json_line(
        &mut stream,
        &SuccessResponse {
            id: request_id,
            result: ResponseResult::SubscriptionStarted {},
        },
    ) {
        if is_connection_closed_error(&err) {
            return Ok(());
        }
        return Err(err);
    }

    loop {
        if should_stop_connection(&mut stream, running)? {
            return Ok(());
        }

        for subscription in &mut subscriptions {
            if let Some(event) = subscription.poll(api_tx, event_hub) {
                if let Err(err) = write_json_line(&mut stream, &event) {
                    if is_connection_closed_error(&err) {
                        return Ok(());
                    }
                    return Err(err);
                }
            }
        }
        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

fn write_text_line(stream: &mut LocalStream, value: &str) -> std::io::Result<()> {
    stream.write_all(value.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn write_text_line_allow_disconnect(stream: &mut LocalStream, value: &str) -> std::io::Result<()> {
    match write_text_line(stream, value) {
        Err(err) if is_connection_closed_error(&err) => Ok(()),
        result => result,
    }
}

fn write_json_line<T: serde::Serialize>(
    stream: &mut LocalStream,
    value: &T,
) -> std::io::Result<()> {
    let encoded = serde_json::to_string(value)
        .map_err(|err| std::io::Error::other(format!("failed to encode json: {err}")))?;
    write_text_line(stream, &encoded)
}

fn write_json_line_allow_disconnect<T: serde::Serialize>(
    stream: &mut LocalStream,
    value: &T,
) -> std::io::Result<()> {
    let encoded = serde_json::to_string(value)
        .map_err(|err| std::io::Error::other(format!("failed to encode json: {err}")))?;
    write_text_line_allow_disconnect(stream, &encoded)
}

pub(super) fn should_stop_connection(
    stream: &mut LocalStream,
    running: &Arc<AtomicBool>,
) -> std::io::Result<bool> {
    if !running.load(Ordering::Relaxed) {
        return Ok(true);
    }

    local_stream_peer_closed(stream)
}

fn dispatch_to_app(request: Request, api_tx: &ApiRequestSender, caller: ApiCaller) -> String {
    dispatch_to_app_with_timeout(request, api_tx, None, caller)
}

pub(super) fn dispatch_to_app_with_timeout(
    request: Request,
    api_tx: &ApiRequestSender,
    timeout: Option<Duration>,
    caller: ApiCaller,
) -> String {
    let request_id = request.id.clone();
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    if let Err(err) = api_tx.send(ApiRequestMessage {
        request,
        respond_to,
        caller,
    }) {
        return error_response_json(
            request_id,
            "server_unavailable",
            format!("failed to dispatch request: {err}"),
        );
    }

    let response = match timeout {
        Some(timeout) => response_rx.recv_timeout(timeout).map_err(|err| match err {
            std::sync::mpsc::RecvTimeoutError::Timeout => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "timed out waiting for app response after {} ms",
                    timeout.as_millis()
                ),
            ),
            std::sync::mpsc::RecvTimeoutError::Disconnected => std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "app response channel closed",
            ),
        }),
        None => response_rx
            .recv()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::BrokenPipe, err)),
    };

    match response {
        Ok(response) => response,
        Err(err) => error_response_json(
            request_id,
            "server_unavailable",
            format!("request handling failed: {err}"),
        ),
    }
}

fn error_response_json(id: String, code: &str, message: String) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: code.into(),
            message,
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode error response"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::sync::{Mutex, OnceLock};
    use tokio::sync::mpsc;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_test_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("zynk-{name}-{}-{nanos}", std::process::id()))
    }

    fn read_line(stream: &mut LocalStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    fn recv_api_request_for(
        receiver: &mut mpsc::UnboundedReceiver<ApiRequestMessage>,
        timeout: Duration,
    ) -> Option<ApiRequestMessage> {
        let deadline = Instant::now() + timeout;
        loop {
            match receiver.try_recv() {
                Ok(message) => return Some(message),
                Err(mpsc::error::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return None,
            }
        }
    }

    fn local_stream_pair(name: &str) -> (LocalStream, LocalStream, PathBuf) {
        let path = unique_test_path(name);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, path)
    }

    fn m827_socket_pair() -> (LocalStream, LocalStream) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        (
            LocalStream::UdSocket(client.into()),
            LocalStream::UdSocket(server.into()),
        )
    }

    fn m827_queued_stream(bytes: &[u8]) -> LocalStream {
        let (mut client, server) = m827_socket_pair();
        client.write_all(bytes).unwrap();
        drop(client);
        server
    }

    fn m827_is_nonblocking(stream: &LocalStream) -> bool {
        use std::os::fd::AsRawFd;
        let LocalStream::UdSocket(socket) = stream;
        // SAFETY: the borrowed socket owns this live descriptor throughout the query.
        let flags = unsafe { libc::fcntl(socket.inner().as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "F_GETFL failed: {}", io::Error::last_os_error());
        flags & libc::O_NONBLOCK != 0
    }

    fn m827_unconnected_stream() -> LocalStream {
        use std::os::fd::{FromRawFd, OwnedFd};
        // SAFETY: socket returns a new descriptor, which is checked and owned below.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(fd >= 0, "socket failed: {}", io::Error::last_os_error());
        // SAFETY: this successful socket descriptor has no other owner.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        LocalStream::UdSocket(std::os::unix::net::UnixStream::from(owned).into())
    }

    struct M827InitialConnection {
        client: Option<LocalStream>,
        worker: Option<std::thread::JoinHandle<()>>,
        done: std::sync::mpsc::Receiver<io::Result<()>>,
        requests: mpsc::UnboundedReceiver<ApiRequestMessage>,
        hub: EventHub,
        path: PathBuf,
    }

    impl M827InitialConnection {
        fn start() -> Self {
            let (client, server, path) = local_stream_pair("m827-initial");
            let (api_tx, requests) = mpsc::unbounded_channel();
            let (done_tx, done) = std::sync::mpsc::channel();
            let (ready_tx, ready) = std::sync::mpsc::channel();
            let mut connection = Self {
                client: Some(client),
                worker: None,
                done,
                requests,
                hub: EventHub::default(),
                path,
            };
            connection
                .client
                .as_ref()
                .unwrap()
                .set_nonblocking(true)
                .unwrap();
            server
                .set_recv_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let hub = connection.hub.clone();
            connection.worker = Some(std::thread::spawn(move || {
                let _ = ready_tx.send(());
                let result = handle_connection_with_stop(
                    server,
                    &api_tx,
                    &hub,
                    &Arc::new(AtomicBool::new(true)),
                    None,
                    None,
                );
                let _ = done_tx.send(result);
            }));
            ready.recv_timeout(Duration::from_secs(2)).unwrap();
            connection
        }

        fn assert_pending(&self, phase: &str, duration: Duration) {
            match self.done.recv_timeout(duration) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                observed => panic!("{phase}: initial handler completed early: {observed:?}"),
            }
        }

        fn response(&mut self) -> serde_json::Value {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                match self.client.as_mut().unwrap().read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("initial response read failed: {error}"),
                }
                assert!(bytes.len() <= 65536, "oversized initial response");
                if bytes.contains(&b'\n') {
                    break;
                }
                assert!(Instant::now() < deadline, "initial response did not arrive");
                std::thread::sleep(Duration::from_millis(1));
            }
            serde_json::from_slice(&bytes).expect("initial response must be JSON")
        }
    }

    impl Drop for M827InitialConnection {
        fn drop(&mut self) {
            if let Some(LocalStream::UdSocket(socket)) = self.client.take() {
                let _ = socket.inner().shutdown(std::net::Shutdown::Both);
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            let _ = fs::remove_file(&self.path);
        }
    }

    #[test]
    fn m827_delayed_partial_initial_request_returns_pong() {
        let mut connection = M827InitialConnection::start();
        connection.assert_pending("idle peer", Duration::from_millis(300));
        connection
            .client
            .as_mut()
            .unwrap()
            .write_all(br#"{"id":"m827-partial","method":"ping","params":{}}"#)
            .unwrap();
        connection.assert_pending("partial request", Duration::from_millis(150));
        connection
            .client
            .as_mut()
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let response = connection.response();
        let outcome = connection
            .done
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(outcome.is_ok(), "initial handler failed: {outcome:?}");
        assert_eq!(response["id"], "m827-partial");
        assert_eq!(response["result"]["type"], "pong");
        assert!(connection.requests.try_recv().is_err());
        assert_eq!(connection.hub.current_sequence(), 0);
        assert!(connection.hub.events_after(0).is_empty());
    }

    #[test]
    fn m827_disconnected_initial_request_returns_promptly() {
        let mut observations = Vec::new();
        for input in [b"".as_slice(), b"partial without newline".as_slice()] {
            let mut server = m827_queued_stream(input);
            let started = Instant::now();
            let result = read_initial_request_line(&mut server);
            observations.push((
                input.len(),
                result.unwrap(),
                m827_is_nonblocking(&server),
                started.elapsed() < Duration::from_secs(2),
            ));
        }
        assert_eq!(
            observations,
            vec![(0, None, false, true), (23, None, false, true)]
        );
    }

    #[test]
    fn m827_initial_request_rejects_invalid_utf8() {
        let mut server = m827_queued_stream(&[0xff, b'\n']);
        let error = read_initial_request_line(&mut server).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!m827_is_nonblocking(&server));
    }

    #[test]
    fn m827_initial_request_preserves_first_line_and_unread_tail() {
        let mut server = m827_queued_stream(b"first line\nuntouched tail\n");
        let first = read_initial_request_line(&mut server).unwrap();
        assert_eq!(first.as_deref(), Some("first line\n"));
        let mut tail = Vec::new();
        server.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, b"untouched tail\n");
        assert!(!m827_is_nonblocking(&server));
    }

    #[test]
    fn m827_idle_initial_request_honors_default_timeout() {
        let (_client, mut server) = m827_socket_pair();
        let started = Instant::now();
        let error = read_initial_request_line(&mut server).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "timed out reading api request");
        assert!(started.elapsed() >= Duration::from_secs(5));
        assert!(!m827_is_nonblocking(&server));
    }

    #[test]
    fn m827_initial_request_enforces_default_size_and_newline_boundary() {
        let mut observations = Vec::new();
        for (non_newline_bytes, newline) in
            [(1_048_576, true), (1_048_577, false), (1_048_577, true)]
        {
            let mut payload = vec![b'x'; non_newline_bytes];
            if newline {
                payload.push(b'\n');
            }
            let (mut client, mut server) = m827_socket_pair();
            let (result, writer_result) = std::thread::scope(|scope| {
                let writer = scope.spawn(|| client.write_all(&payload));
                let result = read_initial_request_line(&mut server);
                (result, writer.join().unwrap())
            });
            assert!(
                writer_result.is_ok(),
                "fixture write failed: {writer_result:?}"
            );
            // Keep byte equality while bounding the diagnostic for the 1 MiB row.
            let observation = match result {
                Ok(Some(line)) => (line.as_bytes() == payload, line.len(), None),
                Ok(None) => (false, 0, None),
                Err(error) => (false, 0, Some((error.kind(), error.to_string()))),
            };
            observations.push((observation, m827_is_nonblocking(&server)));
        }
        let size_error = Some((
            io::ErrorKind::InvalidData,
            "api request line is too large".to_string(),
        ));
        assert_eq!(
            observations,
            vec![
                ((true, 1_048_577, None), false),
                ((false, 0, size_error.clone()), false),
                ((false, 0, size_error), false),
            ]
        );
    }

    #[test]
    fn m827_initial_request_read_error_restores_blocking() {
        let mut server = m827_unconnected_stream();
        let error = read_initial_request_line(&mut server).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
        assert!(!m827_is_nonblocking(&server));
    }

    #[test]
    fn m827_zero_timeout_still_drains_queued_line() {
        let mut server = m827_queued_stream(b"queued line\n");
        let line = read_initial_request_line_with_limits(&mut server, Duration::ZERO, 64).unwrap();
        assert_eq!(line.as_deref(), Some("queued line\n"));
        assert!(!m827_is_nonblocking(&server));
    }

    #[test]
    fn m827_idle_initial_request_honors_private_timeout() {
        let (_client, mut server) = m827_socket_pair();
        let started = Instant::now();
        let error = read_initial_request_line_with_timeout(&mut server, Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "timed out reading api request");
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(!m827_is_nonblocking(&server));
    }

    #[test]
    fn m827_initial_request_honors_private_size_boundary() {
        let mut observations = Vec::new();
        for (input, limit) in [
            (b"1234\n".as_slice(), 4),
            (b"12345".as_slice(), 4),
            (b"12345\n".as_slice(), 4),
            (b"\n".as_slice(), 0),
        ] {
            let mut server = m827_queued_stream(input);
            let result =
                read_initial_request_line_with_limits(&mut server, Duration::from_secs(1), limit)
                    .map_err(|error| (error.kind(), error.to_string()));
            observations.push((result, m827_is_nonblocking(&server)));
        }
        let size_error = (
            io::ErrorKind::InvalidData,
            "api request line is too large".to_string(),
        );
        assert_eq!(
            observations,
            vec![
                (Ok(Some("1234\n".to_string())), false),
                (Err(size_error.clone()), false),
                (Err(size_error), false),
                (Ok(Some("\n".to_string())), false),
            ]
        );
    }

    struct EventWaitConnection {
        client: Option<LocalStream>,
        running: Arc<AtomicBool>,
        requests: Arc<std::sync::atomic::AtomicUsize>,
        server: Option<std::thread::JoinHandle<io::Result<()>>>,
        responder: Option<std::thread::JoinHandle<()>>,
        path: PathBuf,
    }

    impl EventWaitConnection {
        fn start(
            params: crate::api::schema::EventsWaitParams,
            event_hub: EventHub,
            stopped: bool,
            mut pane_response: impl FnMut(String, usize) -> String + Send + 'static,
        ) -> Self {
            let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request_count = requests.clone();
            let responder = std::thread::spawn(move || {
                while let Some(message) = api_rx.blocking_recv() {
                    let count = request_count.fetch_add(1, Ordering::Relaxed);
                    let response = match message.request.method {
                        Method::PaneGet(target) => {
                            assert_eq!(target.pane_id, "pane_1");
                            pane_response(message.request.id, count)
                        }
                        Method::EventsWait(_) => error_response_json(
                            message.request.id,
                            "not_implemented",
                            "parent App fallback has no events.wait handler".into(),
                        ),
                        other => panic!("unexpected wait dispatch: {other:?}"),
                    };
                    let _ = message.respond_to.send(response);
                }
            });
            let (mut client, server, path) = local_stream_pair("events-wait");
            let request = Request {
                id: "event_wait".into(),
                method: Method::EventsWait(params),
            };
            writeln!(client, "{}", serde_json::to_string(&request).unwrap()).unwrap();
            client.flush().unwrap();
            let running = Arc::new(AtomicBool::new(true));
            let server_running = running.clone();
            let server = std::thread::spawn(move || {
                handle_connection_with_stop(
                    server,
                    &api_tx,
                    &event_hub,
                    &server_running,
                    None,
                    Some(&Arc::new(AtomicBool::new(stopped))),
                )
            });
            Self {
                client: Some(client),
                running,
                requests,
                server: Some(server),
                responder: Some(responder),
                path,
            }
        }

        fn response(&mut self) -> serde_json::Value {
            let client = self.client.as_mut().unwrap();
            client.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                match client.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("wait response read failed: {error}"),
                }
                if bytes.contains(&b'\n') {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "events.wait did not answer in 3s"
                );
                assert!(bytes.len() <= 65536, "unexpected oversized wait response");
                std::thread::sleep(Duration::from_millis(1));
            }
            serde_json::from_slice(&bytes).unwrap()
        }
    }

    impl Drop for EventWaitConnection {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Relaxed);
            drop(self.client.take());
            if let Some(server) = self.server.take() {
                let _ = server.join();
            }
            if let Some(responder) = self.responder.take() {
                let _ = responder.join();
            }
            let _ = fs::remove_file(&self.path);
        }
    }

    fn layout_subscription_connection(
        stopped: bool,
    ) -> (
        EventWaitConnection,
        EventHub,
        mpsc::UnboundedReceiver<ApiRequestMessage>,
    ) {
        let (api_tx, api_rx) = mpsc::unbounded_channel();
        let event_hub = EventHub::default();
        let server_hub = event_hub.clone();
        let (mut client, server, path) = local_stream_pair("layout-sub");
        let request = Request {
            id: "layout-subscription".into(),
            method: Method::EventsSubscribe(crate::api::schema::EventsSubscribeParams {
                subscriptions: vec![crate::api::schema::Subscription::LayoutUpdated {}],
            }),
        };
        writeln!(client, "{}", serde_json::to_string(&request).unwrap()).unwrap();
        client.flush().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let server = std::thread::spawn(move || {
            handle_connection_with_stop(
                server,
                &api_tx,
                &server_hub,
                &server_running,
                None,
                Some(&Arc::new(AtomicBool::new(stopped))),
            )
        });
        (
            EventWaitConnection {
                client: Some(client),
                running,
                requests: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                server: Some(server),
                responder: None,
                path,
            },
            event_hub,
            api_rx,
        )
    }

    #[test]
    fn m813_layout_subscription_delivers_exact_event_over_local_stream() {
        let (mut connection, event_hub, mut api_rx) = layout_subscription_connection(false);
        // Read the acknowledgement before publishing: the bounded reader owns one line.
        assert_eq!(
            connection.response(),
            serde_json::json!({
                "id": "layout-subscription", "result": {"type": "subscription_started"}
            })
        );
        let event: crate::api::schema::EventEnvelope = serde_json::from_value(serde_json::json!({
            "event": "layout_updated",
            "data": {"type": "layout_updated", "layout": {
                "workspace_id": "w7", "tab_id": "w7:t3", "focused_pane_id": "w7:9",
                "area": {"x": 3, "y": 4, "width": 97, "height": 23},
                "zoomed": true, "panes": [], "splits": []
            }}
        }))
        .unwrap();
        event_hub.push(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::WorkspaceClosed,
            data: crate::api::schema::EventData::WorkspaceClosed {
                workspace_id: "other".into(),
            },
        });
        event_hub.push(event.clone());
        assert_eq!(connection.response(), serde_json::to_value(event).unwrap());
        assert!(
            api_rx.try_recv().is_err(),
            "layout subscription must not enqueue App work"
        );
    }

    #[test]
    fn m813_stopped_layout_subscription_rejects_before_setup() {
        let (mut connection, _, mut api_rx) = layout_subscription_connection(true);
        let response = connection.response();
        assert_eq!(response["id"], "layout-subscription");
        assert_eq!(response["error"]["code"], "server_unavailable");
        assert!(api_rx.try_recv().is_err());
    }

    fn m821_scroll_connection(
        stopped: bool,
    ) -> (
        EventWaitConnection,
        mpsc::UnboundedReceiver<ApiRequestMessage>,
    ) {
        let (api_tx, api_rx) = mpsc::unbounded_channel();
        let (mut client, server, path) = local_stream_pair("scroll-sub");
        writeln!(
            client,
            "{}",
            serde_json::json!({
                "id": "scroll-subscription", "method": "events.subscribe",
                "params": {"subscriptions": [{"type": "pane.scroll_changed", "pane_id": "legacy"}]}
            })
        )
        .unwrap();
        client.flush().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let server = std::thread::spawn(move || {
            handle_connection_with_stop(
                server,
                &api_tx,
                &EventHub::default(),
                &server_running,
                None,
                Some(&Arc::new(AtomicBool::new(stopped))),
            )
        });
        (
            EventWaitConnection {
                client: Some(client),
                running,
                requests: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                server: Some(server),
                responder: None,
                path,
            },
            api_rx,
        )
    }

    fn m821_answer_scroll_probe(message: ApiRequestMessage, offset: u64) {
        message.respond_to.send(serde_json::json!({
            "id": message.request.id, "result": {"type": "pane_info", "pane": {
                "pane_id": "w2:p4", "terminal_id": "term_4", "workspace_id": "w2",
                "tab_id": "w2:t1", "focused": false, "agent_status": "unknown", "revision": 7,
                "scroll": {"offset_from_bottom": offset, "max_offset_from_bottom": 240, "viewport_rows": 30}
            }}
        }).to_string()).unwrap();
    }

    #[test]
    fn m821_scroll_socket_acknowledges_before_exact_changed_event() {
        let (mut connection, mut rx) = m821_scroll_connection(false);
        let probe = recv_api_request_for(&mut rx, Duration::from_secs(1)).expect("setup probe");
        assert_eq!(
            serde_json::to_value(&probe.request).unwrap(),
            serde_json::json!({
                "id": "scroll-subscription:sub:0:probe", "method": "pane.get", "params": {"pane_id": "legacy"}
            })
        );
        m821_answer_scroll_probe(probe, 12);
        // Hold the first poll response until the single-line ACK has been consumed.
        assert_eq!(
            connection.response(),
            serde_json::json!({
                "id": "scroll-subscription", "result": {"type": "subscription_started"}
            })
        );
        let poll = recv_api_request_for(&mut rx, Duration::from_secs(1)).expect("canonical poll");
        assert_eq!(
            serde_json::to_value(&poll.request).unwrap(),
            serde_json::json!({
                "id": "scroll-subscription:sub:0:pane", "method": "pane.get", "params": {"pane_id": "w2:p4"}
            })
        );
        m821_answer_scroll_probe(poll, 13);
        assert_eq!(
            connection.response(),
            serde_json::json!({"event": "pane.scroll_changed", "data": {
                "pane_id": "w2:p4", "workspace_id": "w2",
                "scroll": {"offset_from_bottom": 13, "max_offset_from_bottom": 240, "viewport_rows": 30}
            }})
        );
        drop(rx);
    }

    #[test]
    fn m821_stopped_scroll_subscription_refuses_before_app_enqueue() {
        let (mut connection, mut rx) = m821_scroll_connection(true);
        let routed = recv_api_request_for(&mut rx, Duration::from_millis(100));
        let enqueued = routed.is_some();
        if let Some(message) = routed {
            // A bypass mutant gets an immediate answer, never a harness timeout.
            message
                .respond_to
                .send(error_response_json(
                    message.request.id,
                    "unexpected_dispatch",
                    "stopped request reached App".into(),
                ))
                .unwrap();
        }
        let response = connection.response();
        assert_eq!(response["id"], "scroll-subscription");
        assert_eq!(response["error"]["code"], "server_unavailable");
        assert!(!enqueued, "stopped subscription enqueued App work");
        assert!(rx.try_recv().is_err());
    }

    fn event_wait_params(timeout_ms: Option<u64>) -> crate::api::schema::EventsWaitParams {
        crate::api::schema::EventsWaitParams {
            match_event: crate::api::schema::EventMatch::PaneAgentStatusChanged {
                pane_id: "pane_1".into(),
                agent_status: crate::api::schema::AgentStatus::Idle,
            },
            timeout_ms,
        }
    }

    fn event_wait_presentation(status: &str) -> serde_json::Value {
        serde_json::json!({
            "pane_id": "pane_1", "workspace_id": "ws_1", "agent_status": status,
            "agent": "pi", "title": "Observed title", "display_agent": "Review",
            "state_labels": {"idle": "Ready"}
        })
    }

    fn event_wait_pane_response(id: String, status: &str) -> String {
        let mut pane = event_wait_presentation(status);
        pane["terminal_id"] = "term_1".into();
        pane["tab_id"] = "tab_1".into();
        pane["focused"] = false.into();
        pane["revision"] = 9.into();
        serde_json::json!({"id": id, "result": {"type": "pane_info", "pane": pane}}).to_string()
    }

    fn event_wait_wire_event(status: &str) -> serde_json::Value {
        let mut data = event_wait_presentation(status);
        data["type"] = "pane_agent_status_changed".into();
        serde_json::json!({"event": "pane_agent_status_changed", "data": data})
    }

    #[test]
    fn m811_events_wait_initial_match_precedes_zero_timeout() {
        let mut connection = EventWaitConnection::start(
            event_wait_params(Some(0)),
            EventHub::default(),
            false,
            |id, _| event_wait_pane_response(id, "idle"),
        );
        let response = connection.response();
        assert_eq!(response["id"], "event_wait");
        assert_eq!(response["result"]["type"], "wait_matched", "{response}");
        assert_eq!(response["result"]["event"], event_wait_wire_event("idle"));
        assert_eq!(connection.requests.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn m811_events_wait_server_timeout_returns_original_request_id() {
        let mut connection = EventWaitConnection::start(
            event_wait_params(Some(30)),
            EventHub::default(),
            false,
            |id, _| event_wait_pane_response(id, "working"),
        );
        let response = connection.response();
        assert_eq!(response["id"], "event_wait");
        assert_eq!(response["error"]["code"], "timeout", "{response}");
        assert!(connection.requests.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn m811_events_wait_later_match_filters_status_and_preserves_event_fields() {
        let hub = EventHub::default();
        let emitter = hub.clone();
        let mut connection = EventWaitConnection::start(
            event_wait_params(Some(1500)),
            hub,
            false,
            move |id, count| {
                if count == 1 || count == 2 {
                    let status = if count == 1 { "blocked" } else { "idle" };
                    emitter.push(serde_json::from_value(event_wait_wire_event(status)).unwrap());
                }
                event_wait_pane_response(id, "working")
            },
        );
        let response = connection.response();
        assert_eq!(response["result"]["type"], "wait_matched", "{response}");
        assert_eq!(response["result"]["event"], event_wait_wire_event("idle"));
        assert!(connection.requests.load(Ordering::Relaxed) >= 3);
    }

    #[test]
    fn m811_events_wait_unsupported_matches_never_enqueue() {
        use crate::api::schema::EventMatch;
        for match_event in [
            EventMatch::WorkspaceCreated { workspace_id: None },
            EventMatch::PaneOutputChanged {
                pane_id: "pane_1".into(),
                min_revision: None,
            },
        ] {
            let mut params = event_wait_params(Some(0));
            params.match_event = match_event;
            let mut connection =
                EventWaitConnection::start(params, EventHub::default(), false, |id, _| {
                    event_wait_pane_response(id, "idle")
                });
            let response = connection.response();
            assert_eq!(response["id"], "event_wait");
            assert_eq!(response["error"]["code"], "unsupported_event_wait_match");
            assert_eq!(connection.requests.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn m811_events_wait_setup_error_retains_body_but_rebinds_probe_id() {
        let mut connection = EventWaitConnection::start(
            event_wait_params(Some(0)),
            EventHub::default(),
            false,
            |id, _| {
                assert_eq!(id, "event_wait:sub:0:probe");
                "invalid JSON".into()
            },
        );
        assert_eq!(
            connection.response(),
            serde_json::json!({
                "id": "event_wait",
                "error": {"code": "internal_error", "message": "failed to decode pane get response"}
            })
        );
        assert_eq!(connection.requests.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn m811_events_wait_post_stop_rejects_before_setup() {
        let mut connection = EventWaitConnection::start(
            event_wait_params(Some(0)),
            EventHub::default(),
            true,
            |id, _| event_wait_pane_response(id, "idle"),
        );
        let response = connection.response();
        assert_eq!(response["id"], "event_wait");
        assert_eq!(response["error"]["code"], "server_unavailable");
        assert_eq!(connection.requests.load(Ordering::Relaxed), 0);
    }

    fn assert_event_wait_stops_on_close(server_stop: bool) {
        let mut connection = EventWaitConnection::start(
            event_wait_params(None),
            EventHub::default(),
            false,
            |id, _| event_wait_pane_response(id, "working"),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while connection.requests.load(Ordering::Relaxed) < 2 {
            assert!(Instant::now() < deadline, "wait never finished setup");
            std::thread::sleep(Duration::from_millis(1));
        }
        if server_stop {
            connection.running.store(false, Ordering::Relaxed);
        } else {
            drop(connection.client.take());
        }
        while !connection.server.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline, "wait did not notice close/stop");
            std::thread::sleep(Duration::from_millis(1));
        }
        connection.server.take().unwrap().join().unwrap().unwrap();
        if server_stop {
            let mut bytes = Vec::new();
            connection
                .client
                .as_mut()
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert!(
                bytes.is_empty(),
                "shutdown must not produce a matched event"
            );
        }
    }

    #[test]
    fn m811_events_wait_stops_on_client_disconnect() {
        assert_event_wait_stops_on_close(false);
    }

    #[test]
    fn m811_events_wait_stops_on_server_shutdown() {
        assert_event_wait_stops_on_close(true);
    }

    #[test]
    fn m811_events_wait_accepts_huge_timeout_without_instant_overflow() {
        let mut connection = EventWaitConnection::start(
            event_wait_params(Some(u64::MAX)),
            EventHub::default(),
            false,
            |id, _| event_wait_pane_response(id, "idle"),
        );
        assert_eq!(connection.response()["result"]["type"], "wait_matched");
    }

    #[test]
    fn socket_path_prefers_explicit_env_override() {
        let _guard = env_lock().lock().unwrap();
        let unique = format!("/tmp/zynk-test-{}.sock", std::process::id());
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, &unique);
        assert_eq!(socket_path(), PathBuf::from(&unique));
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn socket_path_defaults_to_config_dir_even_when_xdg_runtime_dir_is_set() {
        let _guard = env_lock().lock().unwrap();
        let config_home = unique_test_path("socket-default-config-home");
        let runtime_dir = unique_test_path("socket-default-runtime");
        std::env::remove_var(crate::api::ZYNK_SOCKET_PATH_ENV_VAR);
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);

        let expected = config_home
            .join(crate::config::app_dir_name())
            .join("zynk.sock");
        assert_eq!(socket_path(), expected);

        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn socket_path_uses_named_session_dir() {
        let _guard = env_lock().lock().unwrap();
        let config_home = unique_test_path("socket-named-config-home");
        std::env::remove_var(crate::api::ZYNK_SOCKET_PATH_ENV_VAR);
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        std::env::set_var(crate::session::SESSION_ENV_VAR, "work");
        std::env::set_var("XDG_CONFIG_HOME", &config_home);

        let expected = config_home
            .join(crate::config::app_dir_name())
            .join("sessions")
            .join("work")
            .join("zynk.sock");
        assert_eq!(socket_path(), expected);

        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn restrict_socket_permissions_sets_user_only_mode() {
        let dir = unique_test_path("socket-perms");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("api.sock");
        let _listener = UnixListener::bind(&path).unwrap();

        restrict_socket_permissions(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_PERMISSION_MODE);

        drop(_listener);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_response_outcome_uses_top_level_error_shape() {
        let ok_with_error_text = r#"{"id":"req","result":{"read":{"text":"user said \"error\": \"timeout\"","revision":1}}}"#;
        assert_eq!(api_response_outcome(ok_with_error_text), "ok");

        let timeout = r#"{"id":"req","error":{"code":"timeout","message":"timed out waiting for output match"}}"#;
        assert_eq!(api_response_outcome(timeout), "timeout");

        let generic_error =
            r#"{"id":"req","error":{"code":"server_unavailable","message":"boom"}}"#;
        assert_eq!(api_response_outcome(generic_error), "error");
    }

    #[test]
    fn default_server_capabilities_observe_current_session() {
        let capabilities = serde_json::to_value(default_capabilities().unwrap()).unwrap();
        let expected = unsafe { libc::getsid(0) == libc::getpid() };
        assert_eq!(capabilities["detached_server_daemon"], expected);
    }

    #[test]
    fn ping_request_returns_pong() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let response = handle_request(
            Request {
                id: "req_1".into(),
                method: Method::Ping(crate::api::schema::PingParams::default()),
            },
            &tx,
            Some(ServerCapabilities {
                live_handoff: true,
                detached_server_daemon: true,
            }),
            ApiCaller::default(),
            None,
        );

        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_1");
        assert!(matches!(parsed.result, ResponseResult::Pong { .. }));
    }

    #[test]
    fn server_stop_control_bypasses_app_channel_and_preserves_ping() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            handle_request(
                Request {
                    id: "priority_stop".into(),
                    method: Method::ServerStop(crate::api::schema::EmptyParams::default()),
                },
                &tx,
                None,
                ApiCaller::default(),
                Some(&thread_stop),
            )
        });

        let routed = recv_api_request_for(&mut rx, Duration::from_millis(100));
        if let Some(message) = &routed {
            message
                .respond_to
                .send(
                    serde_json::to_string(&SuccessResponse {
                        id: message.request.id.clone(),
                        result: ResponseResult::Ok {},
                    })
                    .unwrap(),
                )
                .unwrap();
        }
        let response: serde_json::Value = serde_json::from_str(&thread.join().unwrap()).unwrap();

        assert!(routed.is_none(), "server.stop reached the App queue");
        assert_eq!(response["id"], "priority_stop");
        assert_eq!(response["result"]["type"], "ok");
        assert!(stop.load(Ordering::Acquire));

        let rejected = handle_request(
            Request {
                id: "after_stop".into(),
                method: Method::WorkspaceList(crate::api::schema::EmptyParams::default()),
            },
            &mpsc::unbounded_channel().0,
            None,
            ApiCaller::default(),
            Some(&stop),
        );
        let rejected: serde_json::Value = serde_json::from_str(&rejected).unwrap();
        assert_eq!(rejected["error"]["code"], "server_unavailable");

        let ping = handle_request(
            Request {
                id: "ping_after_stop".into(),
                method: Method::Ping(crate::api::schema::PingParams::default()),
            },
            &mpsc::unbounded_channel().0,
            None,
            ApiCaller::default(),
            Some(&stop),
        );
        let ping: SuccessResponse = serde_json::from_str(&ping).unwrap();
        assert!(matches!(ping.result, ResponseResult::Pong { .. }));
    }

    #[test]
    fn post_stop_events_subscribe_is_rejected_before_stream_setup() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-sub-post-stop");
        client
            .write_all(
                br#"{"id":"sub_stopped","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"}]}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let stop = Arc::new(AtomicBool::new(true));
        let server_stop = stop.clone();
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            handle_connection_with_stop(
                server,
                &api_tx,
                &event_hub,
                &server_running,
                None,
                Some(&server_stop),
            )
        });

        let response: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        running.store(false, Ordering::Relaxed);
        drop(client);
        server_thread.join().unwrap().unwrap();

        assert_eq!(response["id"], "sub_stopped");
        assert_eq!(response["error"]["code"], "server_unavailable");
        assert!(api_rx.try_recv().is_err());
    }

    #[test]
    fn post_stop_wait_for_output_is_rejected_before_wait_setup() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-wait-post-stop");
        client
            .write_all(br#"{"id":"wait_stopped","method":"pane.wait_for_output","params":{"pane_id":"pane_1","source":"recent","match":{"type":"substring","value":"never"},"timeout_ms":0}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let stop = Arc::new(AtomicBool::new(true));
        let server_stop = stop.clone();
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            handle_connection_with_stop(
                server,
                &api_tx,
                &event_hub,
                &server_running,
                None,
                Some(&server_stop),
            )
        });

        let dispatched = recv_api_request_for(&mut api_rx, Duration::from_millis(100));
        if let Some(message) = &dispatched {
            message
                .respond_to
                .send(error_response_json(
                    message.request.id.clone(),
                    "unexpected_dispatch",
                    "wait setup reached the App queue".into(),
                ))
                .unwrap();
        }
        let response: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        drop(client);
        server_thread.join().unwrap().unwrap();

        assert!(
            dispatched.is_none(),
            "post-stop wait dispatched a pane read"
        );
        assert_eq!(response["id"], "wait_stopped");
        assert_eq!(response["error"]["code"], "server_unavailable");
    }

    #[test]
    fn request_dispatches_to_app_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let request = Request {
            id: "req_2".into(),
            method: Method::WorkspaceList(crate::api::schema::EmptyParams::default()),
        };

        let request_for_thread = request.clone();
        let thread = std::thread::spawn(move || {
            handle_request(request_for_thread, &tx, None, ApiCaller::default(), None)
        });

        let msg = rx.blocking_recv().unwrap();
        assert_eq!(msg.request.id, "req_2");
        msg.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: "req_2".into(),
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let response = thread.join().unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_2");
    }

    #[test]
    fn wait_for_output_stops_when_client_disconnects() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (first_read_tx, first_read_rx) = std::sync::mpsc::channel();
        let responder = std::thread::spawn(move || {
            let mut notified = false;
            while let Some(msg) = api_rx.blocking_recv() {
                assert!(matches!(msg.request.method, Method::PaneRead(_)));
                if !notified {
                    first_read_tx.send(()).unwrap();
                    notified = true;
                }
                msg.respond_to
                    .send(
                        serde_json::to_string(&SuccessResponse {
                            id: msg.request.id,
                            result: ResponseResult::PaneRead {
                                read: crate::api::schema::PaneReadResult {
                                    pane_id: "pane_1".into(),
                                    workspace_id: "ws_1".into(),
                                    tab_id: "tab_1".into(),
                                    source: crate::api::schema::ReadSource::RecentUnwrapped,
                                    format: crate::api::schema::ReadFormat::Text,
                                    text: String::new(),
                                    revision: 0,
                                    truncated: false,
                                },
                            },
                        })
                        .unwrap(),
                    )
                    .unwrap();
            }
        });

        let (mut client, server, _path) = local_stream_pair("api-wait-disconnect");
        client
            .write_all(br#"{"id":"req_wait","method":"pane.wait_for_output","params":{"pane_id":"pane_1","source":"recent","match":{"type":"substring","value":"never"}}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let result = handle_connection(server, &api_tx, &event_hub, &server_running, None);
            done_tx.send(result).unwrap();
        });

        first_read_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(client);

        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok());

        server_thread.join().unwrap();
        drop(running);
        responder.join().unwrap();
    }

    #[test]
    fn subscriptions_stop_when_client_disconnects() {
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-sub-disconnect");
        client
            .write_all(
                br#"{"id":"sub_1","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"}]}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let result = handle_connection(server, &api_tx, &event_hub, &server_running, None);
            done_tx.send(result).unwrap();
        });

        let ack = read_line(&mut client);
        let ack: serde_json::Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(ack["result"]["type"], "subscription_started");

        drop(client);

        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok());
        server_thread.join().unwrap();
    }

    #[test]
    fn subscriptions_stop_when_server_shuts_down() {
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-sub-shutdown");
        client
            .write_all(
                br#"{"id":"sub_2","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"}]}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let result = handle_connection(server, &api_tx, &event_hub, &server_running, None);
            done_tx.send(result).unwrap();
        });

        let ack = read_line(&mut client);
        let ack: serde_json::Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(ack["result"]["type"], "subscription_started");

        running.store(false, Ordering::Relaxed);

        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok());
        server_thread.join().unwrap();
    }
}
