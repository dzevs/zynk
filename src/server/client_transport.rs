// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
//! Blocking client socket transport for the headless server.
//!
//! This module owns the thin-client handshake, read loop, and writer loop.
//! It converts socket I/O into [`ServerEvent`] values consumed by
//! `HeadlessServer`.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::ipc::LocalStream;
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, ClientInputEvent, ClientKeybindings,
    ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage, MAX_CLIPBOARD_IMAGE_PAYLOAD,
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};

/// Minimum accepted attached client size.
///
/// Narrow observers must be allowed to drive narrow renders, otherwise the
/// server wraps pane content against a wider width and the client sees the
/// right edge clipped.
const MIN_CLIENT_COLS: u16 = 1;
const MIN_CLIENT_ROWS: u16 = 1;

/// How long to wait for a client handshake before closing the connection.
/// Set to 4 seconds (rather than 5) to guarantee the connection is closed
/// within the 5-second deadline, even with OS timer slack, thread scheduling,
/// and cleanup overhead.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);

/// Maximum input payload size (bytes) for a single `ClientMessage::Input`.
const MAX_INPUT_PAYLOAD: usize = 1024 * 1024; // 1 MB
/// Maximum structured input events accepted in one client message.
const MAX_INPUT_EVENT_BATCH: usize = 4096;

#[derive(Debug)]
enum RawPasteEnvelopeState {
    Idle,
    StartPrefix { bytes: Vec<u8>, mixed_prefix: bool },
    Paste { bytes: Vec<u8>, mixed_prefix: bool },
    ForwardedEscape { continuation: Vec<u8> },
}

#[derive(Debug, PartialEq, Eq)]
enum RawPasteEnvelopeOutcome {
    Forward(Vec<u8>),
    Hold,
    RejectPaste { size: usize },
    Disconnect { size: usize, reason: &'static str },
}

#[derive(Debug)]
struct RawPasteEnvelopeGuard {
    state: RawPasteEnvelopeState,
}

impl Default for RawPasteEnvelopeGuard {
    fn default() -> Self {
        Self {
            state: RawPasteEnvelopeState::Idle,
        }
    }
}

impl RawPasteEnvelopeGuard {
    fn push(&mut self, data: Vec<u8>) -> Vec<RawPasteEnvelopeOutcome> {
        let state = std::mem::replace(&mut self.state, RawPasteEnvelopeState::Idle);

        if data.len() > MAX_INPUT_PAYLOAD {
            return match state {
                RawPasteEnvelopeState::Idle
                    if crate::raw_input::is_complete_text_bracketed_paste(&data) =>
                {
                    vec![RawPasteEnvelopeOutcome::RejectPaste { size: data.len() }]
                }
                RawPasteEnvelopeState::Idle => vec![RawPasteEnvelopeOutcome::Disconnect {
                    size: data.len(),
                    reason: "oversized input is not one complete UTF-8 bracketed paste",
                }],
                _ => vec![RawPasteEnvelopeOutcome::Disconnect {
                    size: data.len(),
                    reason: "oversized input arrived while a bracketed paste was pending",
                }],
            };
        }

        match state {
            RawPasteEnvelopeState::Idle => self.scan_idle(data, true, false),
            RawPasteEnvelopeState::StartPrefix {
                mut bytes,
                mixed_prefix,
            } => {
                let size = match bytes.len().checked_add(data.len()) {
                    Some(size) => size,
                    None => {
                        return vec![RawPasteEnvelopeOutcome::Disconnect {
                            size: usize::MAX,
                            reason: "bracketed-paste prefix length overflow",
                        }];
                    }
                };
                bytes.extend_from_slice(&data);
                let compared = bytes
                    .len()
                    .min(crate::raw_input::BRACKETED_PASTE_START.len());
                if bytes[..compared] != crate::raw_input::BRACKETED_PASTE_START[..compared] {
                    self.scan_idle(bytes, false, mixed_prefix)
                } else if bytes.len() < crate::raw_input::BRACKETED_PASTE_START.len() {
                    self.state = RawPasteEnvelopeState::StartPrefix {
                        bytes,
                        mixed_prefix,
                    };
                    vec![RawPasteEnvelopeOutcome::Hold]
                } else {
                    debug_assert_eq!(size, bytes.len());
                    self.resolve_paste(bytes, mixed_prefix)
                }
            }
            RawPasteEnvelopeState::Paste {
                mut bytes,
                mixed_prefix,
            } => {
                if bytes.len().checked_add(data.len()).is_none() {
                    return vec![RawPasteEnvelopeOutcome::Disconnect {
                        size: usize::MAX,
                        reason: "bracketed-paste envelope length overflow",
                    }];
                }
                bytes.extend_from_slice(&data);
                self.resolve_paste(bytes, mixed_prefix)
            }
            RawPasteEnvelopeState::ForwardedEscape { mut continuation } => {
                if continuation.len().checked_add(data.len()).is_none() {
                    return vec![RawPasteEnvelopeOutcome::Disconnect {
                        size: usize::MAX,
                        reason: "split paste introducer length overflow",
                    }];
                }
                continuation.extend_from_slice(&data);
                let expected = &crate::raw_input::BRACKETED_PASTE_START[1..];
                let compared = continuation.len().min(expected.len());
                if continuation[..compared] != expected[..compared] {
                    self.scan_idle(continuation, false, false)
                } else if continuation.len() < expected.len() {
                    self.state = RawPasteEnvelopeState::ForwardedEscape { continuation };
                    vec![RawPasteEnvelopeOutcome::Hold]
                } else {
                    vec![RawPasteEnvelopeOutcome::Disconnect {
                        size: continuation.len().saturating_add(1),
                        reason: "input tried to extend a forwarded Escape into a bracketed paste",
                    }]
                }
            }
        }
    }

    fn scan_idle(
        &mut self,
        data: Vec<u8>,
        definitive_lone_escape: bool,
        prior_mixed_prefix: bool,
    ) -> Vec<RawPasteEnvelopeOutcome> {
        if data.is_empty() {
            return forward_raw_input(data);
        }

        if definitive_lone_escape && data.as_slice() == b"\x1b" {
            self.state = RawPasteEnvelopeState::ForwardedEscape {
                continuation: Vec::new(),
            };
            return vec![RawPasteEnvelopeOutcome::Forward(data)];
        }

        if let Some(start) = incomplete_bracketed_paste_start(&data) {
            let mut outcomes = Vec::with_capacity(2);
            if start > 0 {
                outcomes.extend(forward_raw_input(data[..start].to_vec()));
            }
            self.state = RawPasteEnvelopeState::Paste {
                bytes: data[start..].to_vec(),
                mixed_prefix: prior_mixed_prefix || start > 0,
            };
            outcomes.push(RawPasteEnvelopeOutcome::Hold);
            return outcomes;
        }

        if let Some(prefix_len) = trailing_bracketed_paste_start_prefix(&data) {
            let start = data.len() - prefix_len;
            let mut outcomes = Vec::with_capacity(2);
            if start > 0 {
                outcomes.extend(forward_raw_input(data[..start].to_vec()));
            }
            self.state = RawPasteEnvelopeState::StartPrefix {
                bytes: data[start..].to_vec(),
                mixed_prefix: prior_mixed_prefix || start > 0,
            };
            outcomes.push(RawPasteEnvelopeOutcome::Hold);
            return outcomes;
        }

        forward_raw_input(data)
    }

    fn resolve_paste(
        &mut self,
        bytes: Vec<u8>,
        mixed_prefix: bool,
    ) -> Vec<RawPasteEnvelopeOutcome> {
        debug_assert!(bytes.starts_with(crate::raw_input::BRACKETED_PASTE_START));
        let content_start = crate::raw_input::BRACKETED_PASTE_START.len();
        let Some(relative_end) = find_bytes(
            &bytes[content_start..],
            crate::raw_input::BRACKETED_PASTE_END,
        ) else {
            if bytes.len() > MAX_INPUT_PAYLOAD {
                return vec![RawPasteEnvelopeOutcome::Disconnect {
                    size: bytes.len(),
                    reason: "unterminated bracketed paste exceeded the input limit",
                }];
            }
            self.state = RawPasteEnvelopeState::Paste {
                bytes,
                mixed_prefix,
            };
            return vec![RawPasteEnvelopeOutcome::Hold];
        };

        let end = content_start + relative_end;
        let envelope_len = end + crate::raw_input::BRACKETED_PASTE_END.len();
        if envelope_len > MAX_INPUT_PAYLOAD {
            let is_exact = !mixed_prefix
                && envelope_len == bytes.len()
                && std::str::from_utf8(&bytes[content_start..end]).is_ok();
            return if is_exact {
                vec![RawPasteEnvelopeOutcome::RejectPaste { size: envelope_len }]
            } else {
                vec![RawPasteEnvelopeOutcome::Disconnect {
                    size: bytes.len(),
                    reason: "oversized bracketed paste was mixed, trailing, or invalid UTF-8",
                }]
            };
        }

        if envelope_len == bytes.len() {
            return vec![RawPasteEnvelopeOutcome::Forward(bytes)];
        }

        let mut outcomes = vec![RawPasteEnvelopeOutcome::Forward(
            bytes[..envelope_len].to_vec(),
        )];
        outcomes.extend(self.scan_idle(bytes[envelope_len..].to_vec(), false, false));
        outcomes
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn forward_raw_input(mut data: Vec<u8>) -> Vec<RawPasteEnvelopeOutcome> {
    if data.len() <= MAX_INPUT_PAYLOAD {
        return vec![RawPasteEnvelopeOutcome::Forward(data)];
    }

    let trailing = data.split_off(MAX_INPUT_PAYLOAD);
    vec![
        RawPasteEnvelopeOutcome::Forward(data),
        RawPasteEnvelopeOutcome::Forward(trailing),
    ]
}

fn incomplete_bracketed_paste_start(data: &[u8]) -> Option<usize> {
    let mut search_from = 0;
    while let Some(relative_start) = find_bytes(
        &data[search_from..],
        crate::raw_input::BRACKETED_PASTE_START,
    ) {
        let start = search_from + relative_start;
        let content_start = start + crate::raw_input::BRACKETED_PASTE_START.len();
        let Some(relative_end) = find_bytes(
            &data[content_start..],
            crate::raw_input::BRACKETED_PASTE_END,
        ) else {
            return Some(start);
        };
        search_from = content_start + relative_end + crate::raw_input::BRACKETED_PASTE_END.len();
    }
    None
}

fn trailing_bracketed_paste_start_prefix(data: &[u8]) -> Option<usize> {
    (1..crate::raw_input::BRACKETED_PASTE_START.len())
        .rev()
        .find(|&len| data.ends_with(&crate::raw_input::BRACKETED_PASTE_START[..len]))
}

/// Channels owned by the server side of a client writer thread.
#[derive(Clone, Debug)]
pub(crate) struct ClientWriter {
    /// Reliable control messages such as shutdown, notifications, and clipboard writes.
    pub(crate) control: ClientControlWriter,
    /// Droppable render messages. Capacity is one so slow clients cannot build lag.
    pub(crate) render: ClientRenderWriter,
}

#[cfg(test)]
impl ClientWriter {
    pub(crate) fn test_channel(
        control: std::sync::mpsc::Sender<Vec<u8>>,
        render: std::sync::mpsc::SyncSender<Vec<u8>>,
    ) -> Self {
        Self {
            control: ClientControlWriter {
                target: ClientControlTarget::Channel(control),
            },
            render: ClientRenderWriter {
                target: ClientRenderTarget::Channel(render),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct ClientControlWriter {
    target: ClientControlTarget,
}

#[derive(Debug)]
enum ClientControlTarget {
    Queue(Arc<ClientWriterQueue>),
    #[cfg(test)]
    Channel(std::sync::mpsc::Sender<Vec<u8>>),
}

#[derive(Debug)]
pub(crate) struct ClientRenderWriter {
    target: ClientRenderTarget,
}

#[derive(Debug)]
enum ClientRenderTarget {
    Queue(Arc<ClientWriterQueue>),
    #[cfg(test)]
    Channel(std::sync::mpsc::SyncSender<Vec<u8>>),
}

impl Clone for ClientControlWriter {
    fn clone(&self) -> Self {
        match &self.target {
            ClientControlTarget::Queue(queue) => {
                queue.add_sender();
                Self {
                    target: ClientControlTarget::Queue(queue.clone()),
                }
            }
            #[cfg(test)]
            ClientControlTarget::Channel(sender) => Self {
                target: ClientControlTarget::Channel(sender.clone()),
            },
        }
    }
}

impl Drop for ClientControlWriter {
    fn drop(&mut self) {
        match &self.target {
            ClientControlTarget::Queue(queue) => queue.remove_sender(),
            #[cfg(test)]
            ClientControlTarget::Channel(_) => {}
        }
    }
}

impl ClientControlWriter {
    fn queue(queue: Arc<ClientWriterQueue>) -> Self {
        queue.add_sender();
        Self {
            target: ClientControlTarget::Queue(queue),
        }
    }

    pub(crate) fn send(&self, data: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        match &self.target {
            ClientControlTarget::Queue(queue) => queue.send_control(data),
            #[cfg(test)]
            ClientControlTarget::Channel(sender) => sender.send(data),
        }
    }
}

impl Clone for ClientRenderWriter {
    fn clone(&self) -> Self {
        match &self.target {
            ClientRenderTarget::Queue(queue) => {
                queue.add_sender();
                Self {
                    target: ClientRenderTarget::Queue(queue.clone()),
                }
            }
            #[cfg(test)]
            ClientRenderTarget::Channel(sender) => Self {
                target: ClientRenderTarget::Channel(sender.clone()),
            },
        }
    }
}

impl Drop for ClientRenderWriter {
    fn drop(&mut self) {
        match &self.target {
            ClientRenderTarget::Queue(queue) => queue.remove_sender(),
            #[cfg(test)]
            ClientRenderTarget::Channel(_) => {}
        }
    }
}

impl ClientRenderWriter {
    fn queue(queue: Arc<ClientWriterQueue>) -> Self {
        queue.add_sender();
        Self {
            target: ClientRenderTarget::Queue(queue),
        }
    }

    pub(crate) fn try_send(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        match &self.target {
            ClientRenderTarget::Queue(queue) => queue.try_send_render(data),
            #[cfg(test)]
            ClientRenderTarget::Channel(sender) => sender.try_send(data),
        }
    }
}

#[derive(Debug)]
struct ClientWriterQueue {
    state: Mutex<ClientWriterQueueState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct ClientWriterQueueState {
    control: VecDeque<Vec<u8>>,
    render: Option<Vec<u8>>,
    senders: usize,
    writer_alive: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ClientWriteItem {
    Control(Vec<u8>),
    Render(Vec<u8>),
}

impl ClientWriterQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ClientWriterQueueState {
                writer_alive: true,
                ..ClientWriterQueueState::default()
            }),
            ready: Condvar::new(),
        })
    }

    fn add_sender(&self) {
        let mut state = self.lock_state();
        state.senders = state.senders.saturating_add(1);
    }

    fn remove_sender(&self) {
        let mut state = self.lock_state();
        state.senders = state.senders.saturating_sub(1);
        self.ready.notify_one();
    }

    fn send_control(&self, data: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        let mut state = self.lock_state();
        if !state.writer_alive {
            return Err(SendError(data));
        }
        state.control.push_back(data);
        self.ready.notify_one();
        Ok(())
    }

    fn try_send_render(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        let mut state = self.lock_state();
        if !state.writer_alive {
            return Err(TrySendError::Disconnected(data));
        }
        if state.render.is_some() {
            return Err(TrySendError::Full(data));
        }
        state.render = Some(data);
        self.ready.notify_one();
        Ok(())
    }

    fn recv(&self) -> Option<ClientWriteItem> {
        let mut state = self.lock_state();
        loop {
            if let Some(data) = state.control.pop_front() {
                return Some(ClientWriteItem::Control(data));
            }
            if let Some(data) = state.render.take() {
                self.ready.notify_one();
                return Some(ClientWriteItem::Render(data));
            }
            if state.senders == 0 {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn close_writer(&self) {
        let mut state = self.lock_state();
        state.writer_alive = false;
        self.ready.notify_all();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ClientWriterQueueState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Internal event sent from client transport threads to the main event loop.
#[derive(Debug)]
pub(crate) enum ServerEvent {
    /// A new client completed the handshake.
    ClientConnected {
        client_id: u64,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        render_encoding: RenderEncoding,
        keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
        direct_attach_requested: bool,
        writer: ClientWriter,
    },
    /// A client sent an input message.
    ClientInput { client_id: u64, data: Vec<u8> },
    /// A client sent structured input events.
    ClientInputEvents {
        client_id: u64,
        events: Vec<crate::protocol::ClientInputEvent>,
    },
    /// A fully decoded interactive paste exceeded the text-input limit.
    ClientPasteRejected {
        client_id: u64,
        size: usize,
        max: usize,
    },
    /// A client sent local clipboard image bytes to paste into a remote pane.
    ClientClipboardImage {
        client_id: u64,
        extension: String,
        data: Vec<u8>,
    },
    /// A client requested direct attach to one terminal.
    ClientAttachTerminal {
        client_id: u64,
        terminal_id: String,
        takeover: bool,
    },
    /// A client requested read-only observation of one terminal.
    ClientObserveTerminal { client_id: u64, target: String },
    /// A client requested writable control of one terminal.
    ClientControlTerminal {
        client_id: u64,
        target: String,
        takeover: bool,
    },
    /// A direct terminal attach client requested scrollback movement.
    ClientAttachScroll {
        client_id: u64,
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    },
    /// A client sent a resize message.
    ClientResize {
        client_id: u64,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    },
    /// A client detached gracefully.
    ClientDetach { client_id: u64 },
    /// A client connection was lost.
    ClientDisconnected { client_id: u64 },
    /// A client writer drained its render slot and can accept another render.
    ClientWriterDrained { client_id: u64 },
    /// Ctrl+C or external shutdown signal received.
    QuitSignal,
}

/// Clamp client-reported terminal dimensions to a minimum viable size.
pub(crate) fn clamp_terminal_size(cols: u16, rows: u16) -> (u16, u16) {
    let clamped_cols = cols.max(MIN_CLIENT_COLS);
    let clamped_rows = rows.max(MIN_CLIENT_ROWS);
    (clamped_cols, clamped_rows)
}

fn parse_client_keybindings(
    keybindings: ClientKeybindings,
) -> Result<Option<Box<crate::config::LiveKeybindConfig>>, String> {
    match keybindings {
        ClientKeybindings::Server => Ok(None),
        ClientKeybindings::Local { keys_toml } => {
            let mut config = toml::from_str::<crate::config::Config>(&keys_toml)
                .map_err(|err| format!("invalid client keybindings: {err}"))?;
            config.keys.command.clear();
            Ok(Some(Box::new(crate::config::LiveKeybindConfig {
                prefix: config.prefix_key(),
                keybinds: config.keybinds(),
            })))
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum InputEventLimit {
    WithinLimits,
    TooManyEvents,
    PasteTooLarge { size: usize },
    InputPayloadTooLarge { size: usize },
}

fn input_event_limit(events: &[ClientInputEvent]) -> InputEventLimit {
    let mut expanded_events = 0usize;
    let mut paste_bytes = 0usize;
    let mut input_bytes = 0usize;
    for event in events {
        expanded_events = expanded_events.saturating_add(match event {
            ClientInputEvent::Key { repeat_count, .. } => usize::from((*repeat_count).max(1)),
            _ => 1,
        });
        match event {
            ClientInputEvent::Key {
                repeat_count,
                generated_text,
                source,
                ..
            } => {
                let repetitions = usize::from((*repeat_count).max(1));
                if let Some(text) = generated_text {
                    input_bytes =
                        input_bytes.saturating_add(text.len().saturating_mul(repetitions));
                }
                if let crate::protocol::ClientKeySource::Vt { bytes } = source {
                    input_bytes =
                        input_bytes.saturating_add(bytes.len().saturating_mul(repetitions));
                }
            }
            ClientInputEvent::TextCommit(text) => {
                input_bytes = input_bytes.saturating_add(text.len());
            }
            ClientInputEvent::Paste { text } => {
                paste_bytes = paste_bytes.saturating_add(text.len());
            }
            ClientInputEvent::Mouse { .. }
            | ClientInputEvent::FocusGained
            | ClientInputEvent::FocusLost => {}
        }
    }

    if expanded_events > MAX_INPUT_EVENT_BATCH {
        return InputEventLimit::TooManyEvents;
    }

    let payload_bytes = paste_bytes.saturating_add(input_bytes);
    if payload_bytes <= MAX_INPUT_PAYLOAD {
        InputEventLimit::WithinLimits
    } else if input_bytes == 0 {
        InputEventLimit::PasteTooLarge {
            size: payload_bytes,
        }
    } else {
        InputEventLimit::InputPayloadTooLarge {
            size: payload_bytes,
        }
    }
}

fn set_client_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    _context: &'static str,
    _client_id: u64,
) -> io::Result<()> {
    stream.set_recv_timeout(timeout)
}

/// Handles the client handshake on a blocking thread.
///
/// Reads the `Hello` message, validates the version, sends `Welcome`,
/// and then enters a read loop forwarding messages to the server event channel.
pub(crate) fn handle_client_handshake(
    stream: LocalStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    handle_client_handshake_inner(stream, client_id, server_event_tx, should_quit, |_| {})
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HandshakeStopCheckpoint {
    BeforeRead,
    AfterHelloRead,
    BeforeRegistration,
}

fn handle_client_handshake_inner(
    mut stream: LocalStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
    mut checkpoint: impl FnMut(HandshakeStopCheckpoint),
) -> io::Result<()> {
    checkpoint(HandshakeStopCheckpoint::BeforeRead);
    if should_quit.load(Ordering::Acquire) {
        return Ok(());
    }

    // Reset to blocking mode — the accept loop sets nonblocking but
    // the handshake thread needs blocking I/O for read_message/write_message.
    stream.set_nonblocking(false)?;

    set_client_recv_timeout(
        &stream,
        Some(HANDSHAKE_TIMEOUT),
        "client handshake read timeout unavailable",
        client_id,
    )?;

    // Read the Hello message.
    let hello: ClientMessage = match protocol::read_message(&mut stream, MAX_FRAME_SIZE) {
        Ok(msg) => msg,
        Err(protocol::FramingError::UnexpectedEof) => {
            debug!(client_id, "client disconnected before handshake");
            return Ok(());
        }
        Err(protocol::FramingError::Oversized { claimed, max }) => {
            warn!(client_id, claimed, max, "oversized handshake from client");
            return Ok(());
        }
        Err(err) => {
            debug!(client_id, err = %err, "failed to read client hello");
            return Ok(());
        }
    };

    let (
        client_cols,
        client_rows,
        cell_width_px,
        cell_height_px,
        render_encoding,
        keybindings,
        direct_attach_requested,
    ) = match hello {
        ClientMessage::Hello {
            version,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            requested_encoding,
            keybindings,
            launch_mode,
        } => {
            // Version check.
            match protocol::check_client_version(version) {
                protocol::VersionCheck::Compatible => {}
                protocol::VersionCheck::Incompatible(reason) => {
                    // Send rejection Welcome.
                    let welcome = ServerMessage::Welcome {
                        version: PROTOCOL_VERSION,
                        encoding: RenderEncoding::SemanticFrame,
                        error: Some(reason),
                    };
                    let _ = protocol::write_message(&mut stream, &welcome);
                    return Ok(());
                }
            }

            let keybindings = match parse_client_keybindings(keybindings) {
                Ok(keybindings) => keybindings,
                Err(error) => {
                    let welcome = ServerMessage::Welcome {
                        version: PROTOCOL_VERSION,
                        encoding: RenderEncoding::SemanticFrame,
                        error: Some(error),
                    };
                    let _ = protocol::write_message(&mut stream, &welcome);
                    return Ok(());
                }
            };

            // Clamp size.
            let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
            (
                clamped_cols,
                clamped_rows,
                cell_width_px,
                cell_height_px,
                requested_encoding,
                keybindings,
                launch_mode == ClientLaunchMode::TerminalAttach,
            )
        }
        _ => {
            // First message must be Hello.
            debug!(client_id, "first message was not Hello, closing");
            let welcome = ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                encoding: RenderEncoding::SemanticFrame,
                error: Some("expected Hello as first message".to_owned()),
            };
            let _ = protocol::write_message(&mut stream, &welcome);
            return Ok(());
        }
    };

    checkpoint(HandshakeStopCheckpoint::AfterHelloRead);
    if should_quit.load(Ordering::Acquire) {
        return Ok(());
    }

    // Send Welcome.
    let welcome = ServerMessage::Welcome {
        version: PROTOCOL_VERSION,
        encoding: render_encoding,
        error: None,
    };
    protocol::write_message(&mut stream, &welcome).map_err(|e| io::Error::other(e.to_string()))?;

    set_client_recv_timeout(
        &stream,
        None,
        "failed to clear client handshake read timeout",
        client_id,
    )?;

    // Create separate channels for reliable control messages and droppable renders.
    let writer_queue = ClientWriterQueue::new();
    let writer = ClientWriter {
        control: ClientControlWriter::queue(writer_queue.clone()),
        render: ClientRenderWriter::queue(writer_queue.clone()),
    };

    // Spawn a writer thread that forwards messages from the channels to the stream.
    let write_stream = stream.try_clone()?;
    let writer_event_tx = server_event_tx.clone();
    std::thread::spawn(move || {
        client_writer_loop(write_stream, client_id, writer_queue, writer_event_tx);
    });

    checkpoint(HandshakeStopCheckpoint::BeforeRegistration);
    if should_quit.load(Ordering::Acquire) {
        send_shutdown_to_unregistered_client(&writer);
        return Ok(());
    }

    // Notify the main loop about the new client.
    let connected = ServerEvent::ClientConnected {
        client_id,
        cols: client_cols,
        rows: client_rows,
        cell_width_px,
        cell_height_px,
        render_encoding,
        keybindings,
        direct_attach_requested,
        writer,
    };
    if let Err(err) = server_event_tx.blocking_send(connected) {
        if let ServerEvent::ClientConnected { writer, .. } = err.0 {
            send_shutdown_to_unregistered_client(&writer);
        }
        return Ok(());
    }

    // Enter read loop — read client messages and forward to main loop.
    client_read_loop(stream, client_id, server_event_tx, should_quit)
}

fn send_shutdown_to_unregistered_client(writer: &ClientWriter) {
    let mut framed = Vec::new();
    if protocol::write_message(
        &mut framed,
        &ServerMessage::ServerShutdown {
            reason: Some("server is shutting down".to_owned()),
        },
    )
    .is_ok()
    {
        let _ = writer.control.send(framed);
    }
}

/// The client writer loop — prioritizes control messages over render frames.
fn client_writer_loop(
    mut stream: LocalStream,
    client_id: u64,
    writer_queue: Arc<ClientWriterQueue>,
    server_event_tx: mpsc::Sender<ServerEvent>,
) {
    while let Some(item) = writer_queue.recv() {
        match item {
            ClientWriteItem::Control(data) => {
                if !write_framed_bytes(&mut stream, &data) {
                    break;
                }
            }
            ClientWriteItem::Render(data) => {
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientWriterDrained { client_id });
                if !write_framed_bytes(&mut stream, &data) {
                    break;
                }
            }
        }
    }
    writer_queue.close_writer();
    debug!("client writer thread exiting");
}

fn write_framed_bytes(stream: &mut LocalStream, data: &[u8]) -> bool {
    if let Err(err) = stream.write_all(data) {
        debug!(err = %err, "client write failed, closing writer");
        return false;
    }
    if let Err(err) = stream.flush() {
        debug!(err = %err, "client flush failed, closing writer");
        return false;
    }
    true
}

/// The client read loop — reads messages from the client and forwards to the server event channel.
fn client_read_loop(
    mut stream: LocalStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    let mut raw_paste_guard = RawPasteEnvelopeGuard::default();
    'read_loop: while !should_quit.load(Ordering::Acquire) {
        let msg: ClientMessage = match protocol::read_message(&mut stream, MAX_GRAPHICS_FRAME_SIZE)
        {
            Ok(msg) => msg,
            Err(protocol::FramingError::UnexpectedEof) => {
                // Client disconnected.
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
            Err(protocol::FramingError::Oversized { claimed, max }) => {
                warn!(
                    client_id,
                    claimed, max, "oversized message from client, closing"
                );
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
            Err(protocol::FramingError::Bincode(err)) => {
                warn!(
                    client_id,
                    err = %err,
                    "client protocol decode failed, closing"
                );
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
            Err(err) => {
                debug!(client_id, err = %err, "client read error, closing");
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
        };

        let msg = match msg {
            ClientMessage::Input { data } => {
                for outcome in raw_paste_guard.push(data) {
                    let event = match outcome {
                        RawPasteEnvelopeOutcome::Forward(data) => {
                            ServerEvent::ClientInput { client_id, data }
                        }
                        RawPasteEnvelopeOutcome::Hold => continue,
                        RawPasteEnvelopeOutcome::RejectPaste { size } => {
                            warn!(
                                client_id,
                                size,
                                max = MAX_INPUT_PAYLOAD,
                                "oversized bracketed paste from client, rejecting"
                            );
                            ServerEvent::ClientPasteRejected {
                                client_id,
                                size,
                                max: MAX_INPUT_PAYLOAD,
                            }
                        }
                        RawPasteEnvelopeOutcome::Disconnect { size, reason } => {
                            warn!(
                                client_id,
                                size, reason, "invalid raw input from client, closing"
                            );
                            let _ = server_event_tx
                                .blocking_send(ServerEvent::ClientDisconnected { client_id });
                            break 'read_loop;
                        }
                    };
                    if server_event_tx.blocking_send(event).is_err() {
                        break 'read_loop;
                    }
                }
                continue;
            }
            msg => msg,
        };

        let event = match msg {
            ClientMessage::Input { .. } => unreachable!("raw input handled before dispatch"),
            ClientMessage::InputEvents { events } => match input_event_limit(&events) {
                InputEventLimit::WithinLimits => {
                    ServerEvent::ClientInputEvents { client_id, events }
                }
                limit @ (InputEventLimit::TooManyEvents
                | InputEventLimit::InputPayloadTooLarge { .. }) => {
                    warn!(
                        client_id,
                        count = events.len(),
                        reason = ?limit,
                        "oversized input events from client, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                }
                InputEventLimit::PasteTooLarge { size } => {
                    warn!(
                        client_id,
                        size,
                        max = MAX_INPUT_PAYLOAD,
                        "oversized structured paste from client, rejecting"
                    );
                    ServerEvent::ClientPasteRejected {
                        client_id,
                        size,
                        max: MAX_INPUT_PAYLOAD,
                    }
                }
            },
            ClientMessage::ObserveTerminal { target } => {
                ServerEvent::ClientObserveTerminal { client_id, target }
            }
            ClientMessage::ControlTerminal { target, takeover } => {
                ServerEvent::ClientControlTerminal {
                    client_id,
                    target,
                    takeover,
                }
            }
            ClientMessage::ClipboardImage { extension, data } => {
                if data.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
                    warn!(
                        client_id,
                        size = data.len(),
                        "oversized clipboard image from client, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                } else {
                    ServerEvent::ClientClipboardImage {
                        client_id,
                        extension,
                        data,
                    }
                }
            }
            ClientMessage::Resize {
                cols,
                rows,
                cell_width_px,
                cell_height_px,
            } => {
                let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
                ServerEvent::ClientResize {
                    client_id,
                    cols: clamped_cols,
                    rows: clamped_rows,
                    cell_width_px,
                    cell_height_px,
                }
            }
            ClientMessage::Detach => ServerEvent::ClientDetach { client_id },
            ClientMessage::AttachTerminal {
                terminal_id,
                takeover,
            } => ServerEvent::ClientAttachTerminal {
                client_id,
                terminal_id,
                takeover,
            },
            ClientMessage::AttachScroll {
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            } => ServerEvent::ClientAttachScroll {
                client_id,
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            },
            ClientMessage::Hello { .. } => {
                // Duplicate Hello — ignore.
                continue;
            }
        };

        if server_event_tx.blocking_send(event).is_err() {
            break; // Main loop gone.
        }
    }

    debug!(client_id, "client read thread exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::traits::Listener as _;
    use std::path::PathBuf;

    struct TestSocketPath(PathBuf);

    impl Drop for TestSocketPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn unique_test_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let filename = format!("h{}-{nanos}.sock", std::process::id());
        let _ = name;
        PathBuf::from("/tmp").join(filename)
    }

    fn local_stream_pair(name: &str) -> (LocalStream, LocalStream, TestSocketPath) {
        let path = unique_test_path(name);
        let _ = std::fs::remove_file(&path);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, TestSocketPath(path))
    }

    fn write_valid_hello(stream: &mut LocalStream) {
        protocol::write_message(
            stream,
            &ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                cols: 100,
                rows: 30,
                cell_width_px: 8,
                cell_height_px: 16,
                requested_encoding: RenderEncoding::TerminalAnsi,
                keybindings: ClientKeybindings::Server,
                launch_mode: ClientLaunchMode::App,
            },
        )
        .expect("write hello");
    }

    fn recv_server_event(receiver: &mut mpsc::Receiver<ServerEvent>, context: &str) -> ServerEvent {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match receiver.try_recv() {
                Ok(event) => return event,
                Err(mpsc::error::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(err) => panic!("{context}: {err}"),
            }
        }
    }

    fn bracketed_paste_with_total_len(total_len: usize) -> Vec<u8> {
        const DELIMITER_BYTES: usize = b"\x1b[200~".len() + b"\x1b[201~".len();
        assert!(total_len >= DELIMITER_BYTES);
        let mut data = Vec::with_capacity(total_len);
        data.extend_from_slice(b"\x1b[200~");
        data.resize(total_len - b"\x1b[201~".len(), b'x');
        data.extend_from_slice(b"\x1b[201~");
        data
    }

    fn split_bracketed_paste(total_len: usize, first_len: usize) -> (Vec<u8>, Vec<u8>) {
        let mut second = bracketed_paste_with_total_len(total_len);
        let first = second.drain(..first_len).collect();
        (first, second)
    }

    fn write_input_message(stream: &mut LocalStream, data: Vec<u8>) {
        protocol::write_message(stream, &ClientMessage::Input { data }).expect("write input frame");
    }

    fn write_resize_marker(stream: &mut LocalStream, cols: u16) {
        protocol::write_message(
            stream,
            &ClientMessage::Resize {
                cols,
                rows: 24,
                cell_width_px: 0,
                cell_height_px: 0,
            },
        )
        .expect("write resize marker");
    }

    fn assert_resize_marker(event: ServerEvent, expected_cols: u16) {
        match event {
            ServerEvent::ClientResize {
                client_id: 7,
                cols,
                rows: 24,
                cell_width_px: 0,
                cell_height_px: 0,
            } => assert_eq!(cols, expected_cols),
            other => panic!("expected resize marker, got {other:?}"),
        }
    }

    fn assert_client_message_disconnects(name: &str, message: ClientMessage) {
        let (mut client_stream, server_stream, _path) = local_stream_pair(name);
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(&mut client_stream, &message).expect("write rejected input");
        assert!(matches!(
            recv_server_event(&mut server_event_rx, name),
            ServerEvent::ClientDisconnected { client_id: 7 }
        ));

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    fn test_queue_writer() -> (ClientWriter, Arc<ClientWriterQueue>) {
        let queue = ClientWriterQueue::new();
        (
            ClientWriter {
                control: ClientControlWriter::queue(queue.clone()),
                render: ClientRenderWriter::queue(queue.clone()),
            },
            queue,
        )
    }

    fn frame_server_message(message: &ServerMessage) -> Vec<u8> {
        let mut bytes = Vec::new();
        protocol::write_message(&mut bytes, message).expect("frame server message");
        bytes
    }

    #[test]
    fn client_writer_queue_keeps_render_slot_bounded() {
        let (writer, _queue) = test_queue_writer();
        let first = frame_server_message(&ServerMessage::WindowTitle {
            title: Some("first".into()),
        });
        let second = frame_server_message(&ServerMessage::WindowTitle {
            title: Some("second".into()),
        });

        writer.render.try_send(first).expect("first render fits");
        assert!(matches!(
            writer.render.try_send(second),
            Err(TrySendError::Full(_))
        ));
    }

    #[test]
    fn client_writer_prioritizes_control_and_reports_render_drain() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-writer-priority");
        let (writer, queue) = test_queue_writer();
        writer
            .render
            .try_send(frame_server_message(&ServerMessage::WindowTitle {
                title: Some("render".into()),
            }))
            .expect("queue render");
        writer
            .control
            .send(frame_server_message(&ServerMessage::ReloadSoundConfig))
            .expect("queue control");

        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let handle = std::thread::spawn(move || {
            client_writer_loop(server_stream, 9, queue, server_event_tx);
        });

        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read control") {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected control message first, got {other:?}"),
        }
        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read render") {
            ServerMessage::WindowTitle { title } => assert_eq!(title.as_deref(), Some("render")),
            other => panic!("expected render message second, got {other:?}"),
        }
        match server_event_rx
            .blocking_recv()
            .expect("writer drained render slot")
        {
            ServerEvent::ClientWriterDrained { client_id } => assert_eq!(client_id, 9),
            other => panic!("expected writer drained event, got {other:?}"),
        }

        drop(writer);
        handle.join().expect("writer exits after senders drop");
    }

    #[test]
    fn client_writer_exits_when_all_writer_handles_drop() {
        let (_client_stream, server_stream, _path) = local_stream_pair("client-writer-drop");
        let (writer, queue) = test_queue_writer();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 11, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(writer);
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer exits without polling after senders drop");
    }

    #[test]
    fn client_writer_clone_keeps_loop_alive_until_final_drop() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-writer-clone-drop");
        let (writer, queue) = test_queue_writer();
        let cloned_writer = writer.clone();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 12, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(writer);
        cloned_writer
            .control
            .send(frame_server_message(&ServerMessage::ReloadSoundConfig))
            .expect("cloned writer still sends after original drops");
        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE)
            .expect("read control from cloned writer")
        {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected cloned control message, got {other:?}"),
        }
        assert!(
            done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "writer exited while cloned handles were still alive"
        );

        drop(cloned_writer);
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer exits after final cloned writer drops");
    }

    #[test]
    fn client_writer_closes_queue_after_socket_write_failure() {
        let (client_stream, server_stream, _path) =
            local_stream_pair("client-writer-socket-failure");
        server_stream
            .set_send_timeout(Some(Duration::from_millis(100)))
            .expect("set test send timeout");
        let (writer, queue) = test_queue_writer();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 13, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(client_stream);
        writer
            .control
            .send(vec![b'x'; 1024 * 1024])
            .expect("message is accepted before the writer observes socket failure");
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer exits after socket write failure");

        assert!(matches!(writer.control.send(vec![b'y']), Err(SendError(_))));
        assert!(matches!(
            writer.render.try_send(vec![b'z']),
            Err(TrySendError::Disconnected(_))
        ));
    }

    #[test]
    fn clamp_terminal_size_zero_zero() {
        assert_eq!(
            clamp_terminal_size(0, 0),
            (MIN_CLIENT_COLS, MIN_CLIENT_ROWS)
        );
    }

    #[test]
    fn clamp_terminal_size_one_one() {
        assert_eq!(clamp_terminal_size(1, 1), (1, 1));
    }

    #[test]
    fn clamp_terminal_size_preserves_narrow_client_size() {
        assert_eq!(clamp_terminal_size(40, 12), (40, 12));
    }

    #[test]
    fn clamp_terminal_size_valid() {
        assert_eq!(clamp_terminal_size(120, 40), (120, 40));
    }

    #[test]
    fn clamp_terminal_size_exact_minimum() {
        assert_eq!(
            clamp_terminal_size(MIN_CLIENT_COLS, MIN_CLIENT_ROWS),
            (MIN_CLIENT_COLS, MIN_CLIENT_ROWS)
        );
    }

    #[test]
    fn parse_client_keybindings_accepts_local_profile() {
        let keybindings = parse_client_keybindings(ClientKeybindings::Local {
            keys_toml: r#"
[keys]
prefix = "ctrl+a"
new_tab = "prefix+t"

[[keys.command]]
key = "prefix+g"
command = "lazygit"
"#
            .to_owned(),
        })
        .expect("valid client keybindings")
        .expect("local profile");

        assert_eq!(keybindings.prefix.0, crossterm::event::KeyCode::Char('a'));
        assert!(keybindings
            .keybinds
            .new_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+t"));
        assert!(keybindings.keybinds.custom_commands.is_empty());
    }

    #[test]
    fn parse_client_keybindings_tolerates_disabled_bindings() {
        let keybindings = parse_client_keybindings(ClientKeybindings::Local {
            keys_toml: r#"
[keys]
new_tab = "ctrl+notakey"
"#
            .to_owned(),
        })
        .expect("diagnostic-only client keybindings should be accepted")
        .expect("local profile");

        assert!(keybindings.keybinds.new_tab.bindings.is_empty());
        assert!(keybindings
            .keybinds
            .next_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+n"));
    }

    #[test]
    fn handshake_stop_before_read_avoids_every_later_checkpoint() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-handshake-stop-before-read");
        write_valid_hello(&mut client_stream);
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let mut seen = Vec::new();

        handle_client_handshake_inner(
            server_stream,
            42,
            &server_event_tx,
            &should_quit,
            |checkpoint| {
                seen.push(checkpoint);
                if checkpoint == HandshakeStopCheckpoint::BeforeRead {
                    should_quit.store(true, Ordering::Release);
                }
            },
        )
        .unwrap();

        assert_eq!(seen, [HandshakeStopCheckpoint::BeforeRead]);
        assert!(server_event_rx.try_recv().is_err());
    }

    #[test]
    fn handshake_stop_after_hello_avoids_welcome_and_registration() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-handshake-stop-after-hello");
        write_valid_hello(&mut client_stream);
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let mut seen = Vec::new();

        handle_client_handshake_inner(
            server_stream,
            42,
            &server_event_tx,
            &should_quit,
            |checkpoint| {
                seen.push(checkpoint);
                if checkpoint == HandshakeStopCheckpoint::AfterHelloRead {
                    should_quit.store(true, Ordering::Release);
                }
            },
        )
        .unwrap();

        assert_eq!(
            seen,
            [
                HandshakeStopCheckpoint::BeforeRead,
                HandshakeStopCheckpoint::AfterHelloRead,
            ]
        );
        assert!(server_event_rx.try_recv().is_err());
        assert!(matches!(
            protocol::read_message::<_, ServerMessage>(&mut client_stream, MAX_FRAME_SIZE),
            Err(protocol::FramingError::UnexpectedEof)
        ));
    }

    #[test]
    fn handshake_stop_before_registration_sends_shutdown_without_connecting() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-handshake-stop-before-registration");
        write_valid_hello(&mut client_stream);
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let mut seen = Vec::new();

        handle_client_handshake_inner(
            server_stream,
            42,
            &server_event_tx,
            &should_quit,
            |checkpoint| {
                seen.push(checkpoint);
                if checkpoint == HandshakeStopCheckpoint::BeforeRegistration {
                    should_quit.store(true, Ordering::Release);
                }
            },
        )
        .unwrap();

        assert_eq!(
            seen,
            [
                HandshakeStopCheckpoint::BeforeRead,
                HandshakeStopCheckpoint::AfterHelloRead,
                HandshakeStopCheckpoint::BeforeRegistration,
            ]
        );
        assert!(server_event_rx.try_recv().is_err());
        assert!(matches!(
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).unwrap(),
            ServerMessage::Welcome { error: None, .. }
        ));
        assert!(matches!(
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).unwrap(),
            ServerMessage::ServerShutdown { reason: Some(reason) }
                if reason == "server is shutting down"
        ));
    }

    #[test]
    fn handshake_registration_failure_sends_shutdown_to_unregistered_client() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-handshake-registration-closed");
        client_stream
            .set_recv_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let (server_event_tx, server_event_rx) = mpsc::channel(1);
        drop(server_event_rx);
        let should_quit = Arc::new(AtomicBool::new(false));
        let handshake_quit = should_quit.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result =
                handle_client_handshake(server_stream, 42, &server_event_tx, &handshake_quit);
            done_tx.send(result).unwrap();
        });

        write_valid_hello(&mut client_stream);
        assert!(matches!(
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).unwrap(),
            ServerMessage::Welcome { error: None, .. }
        ));
        assert!(matches!(
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).unwrap(),
            ServerMessage::ServerShutdown { reason: Some(reason) }
                if reason == "server is shutting down"
        ));

        let result = match done_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(result) => result,
            Err(err) => {
                should_quit.store(true, Ordering::Release);
                drop(client_stream);
                handle.join().unwrap();
                panic!("unregistered handshake did not exit after shutdown: {err}");
            }
        };
        result.unwrap();
        handle.join().unwrap();
        assert!(!should_quit.load(Ordering::Acquire));
    }

    #[test]
    fn handshake_negotiates_terminal_ansi_encoding() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-handshake-ansi");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let handshake_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            handle_client_handshake(server_stream, 42, &server_event_tx, &handshake_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                cols: 100,
                rows: 30,
                cell_width_px: 8,
                cell_height_px: 16,
                requested_encoding: RenderEncoding::TerminalAnsi,
                keybindings: ClientKeybindings::Server,
                launch_mode: ClientLaunchMode::App,
            },
        )
        .expect("write hello");

        let welcome: ServerMessage =
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read welcome");
        match welcome {
            ServerMessage::Welcome {
                version,
                encoding,
                error,
            } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert_eq!(encoding, RenderEncoding::TerminalAnsi);
                assert_eq!(error, None);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }

        match server_event_rx
            .blocking_recv()
            .expect("client connected event")
        {
            ServerEvent::ClientConnected {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                render_encoding,
                keybindings,
                direct_attach_requested,
                writer,
            } => {
                assert_eq!(client_id, 42);
                assert_eq!((cols, rows), (100, 30));
                assert_eq!((cell_width_px, cell_height_px), (8, 16));
                assert_eq!(render_encoding, RenderEncoding::TerminalAnsi);
                assert!(keybindings.is_none());
                assert!(!direct_attach_requested);
                drop(writer);
            }
            other => panic!("expected ClientConnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("handshake thread join")
            .expect("handshake thread result");
    }

    #[test]
    fn handshake_marks_terminal_attach_launch_mode() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-handshake-terminal-attach");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let handshake_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            handle_client_handshake(server_stream, 42, &server_event_tx, &handshake_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                cols: 100,
                rows: 30,
                cell_width_px: 8,
                cell_height_px: 16,
                requested_encoding: RenderEncoding::TerminalAnsi,
                keybindings: ClientKeybindings::Server,
                launch_mode: ClientLaunchMode::TerminalAttach,
            },
        )
        .expect("write hello");

        let welcome: ServerMessage =
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read welcome");
        match welcome {
            ServerMessage::Welcome {
                version,
                encoding,
                error,
            } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert_eq!(encoding, RenderEncoding::TerminalAnsi);
                assert_eq!(error, None);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }

        match server_event_rx
            .blocking_recv()
            .expect("client connected event")
        {
            ServerEvent::ClientConnected {
                direct_attach_requested,
                writer,
                ..
            } => {
                assert!(direct_attach_requested);
                drop(writer);
            }
            other => panic!("expected ClientConnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("handshake thread join")
            .expect("handshake thread result");
    }

    #[test]
    fn client_read_loop_rejects_oversized_bracketed_paste_without_disconnect() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-oversized");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD),
            },
        )
        .expect("write maximum-size bracketed paste");

        match recv_server_event(&mut server_event_rx, "maximum-size paste event") {
            ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 7);
                assert_eq!(data.len(), MAX_INPUT_PAYLOAD);
            }
            other => panic!("expected maximum-size ClientInput, got {other:?}"),
        }

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1),
            },
        )
        .expect("write oversized bracketed paste");

        match recv_server_event(&mut server_event_rx, "oversized paste rejection") {
            ServerEvent::ClientPasteRejected {
                client_id,
                size,
                max,
            } => {
                assert_eq!(client_id, 7);
                assert_eq!(size, MAX_INPUT_PAYLOAD + 1);
                assert_eq!(max, MAX_INPUT_PAYLOAD);
            }
            ServerEvent::ClientDisconnected { .. } => {
                panic!("oversized bracketed paste must not disconnect the client")
            }
            other => panic!("expected ClientPasteRejected, got {other:?}"),
        }

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: b"still connected".to_vec(),
            },
        )
        .expect("write valid input after rejection");

        match recv_server_event(&mut server_event_rx, "valid input after rejection") {
            ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 7);
                assert_eq!(data, b"still connected");
            }
            other => panic!("expected ClientInput after rejection, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_holds_fragmented_at_limit_paste_until_complete() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("fragmented-at-limit");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(8);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });
        let (first, second) = split_bracketed_paste(MAX_INPUT_PAYLOAD, (MAX_INPUT_PAYLOAD / 2) + 1);
        let mut expected = first.clone();
        expected.extend_from_slice(&second);

        write_input_message(&mut client_stream, first);
        write_resize_marker(&mut client_stream, 91);
        assert_resize_marker(
            recv_server_event(&mut server_event_rx, "marker after held fragment"),
            91,
        );

        write_input_message(&mut client_stream, second);
        match recv_server_event(&mut server_event_rx, "completed at-limit paste") {
            ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 7);
                assert_eq!(data, expected);
                let mut app_framer = crate::raw_input::RawInputFramer::default();
                let events = app_framer.push(&data);
                assert_eq!(events.len(), 1);
                let crate::raw_input::RawInputEvent::Paste(text) = &events[0] else {
                    panic!("expected one semantic paste event");
                };
                assert_eq!(
                    text.len(),
                    MAX_INPUT_PAYLOAD
                        - crate::raw_input::BRACKETED_PASTE_START.len()
                        - crate::raw_input::BRACKETED_PASTE_END.len()
                );
            }
            other => panic!("expected one reassembled ClientInput, got {other:?}"),
        }

        write_input_message(&mut client_stream, b"after exact limit".to_vec());
        match recv_server_event(&mut server_event_rx, "input after at-limit paste") {
            ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 7);
                assert_eq!(data, b"after exact limit");
            }
            other => panic!("expected ClientInput after at-limit paste, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_rejects_fragmented_max_plus_one_paste_and_recovers() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("fragmented-max-plus-one");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(8);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });
        let (first, second) =
            split_bracketed_paste(MAX_INPUT_PAYLOAD + 1, (MAX_INPUT_PAYLOAD / 2) + 1);

        write_input_message(&mut client_stream, first);
        write_resize_marker(&mut client_stream, 92);
        assert_resize_marker(
            recv_server_event(&mut server_event_rx, "marker after held oversized fragment"),
            92,
        );

        write_input_message(&mut client_stream, second);
        match recv_server_event(&mut server_event_rx, "fragmented paste rejection") {
            ServerEvent::ClientPasteRejected {
                client_id,
                size,
                max,
            } => {
                assert_eq!(client_id, 7);
                assert_eq!(size, MAX_INPUT_PAYLOAD + 1);
                assert_eq!(max, MAX_INPUT_PAYLOAD);
            }
            other => panic!("expected recoverable ClientPasteRejected, got {other:?}"),
        }

        write_input_message(&mut client_stream, b"still connected".to_vec());
        match recv_server_event(&mut server_event_rx, "input after fragmented rejection") {
            ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 7);
                assert_eq!(data, b"still connected");
            }
            other => panic!("expected ClientInput after rejection, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_disconnects_cumulative_unterminated_paste() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("fragmented-unterminated");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(8);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });
        let mut first = b"\x1b[200~".to_vec();
        first.resize((MAX_INPUT_PAYLOAD / 2) + 1, b'x');
        let second = vec![b'x'; MAX_INPUT_PAYLOAD + 1 - first.len()];

        write_input_message(&mut client_stream, first);
        write_resize_marker(&mut client_stream, 93);
        assert_resize_marker(
            recv_server_event(&mut server_event_rx, "marker after unterminated fragment"),
            93,
        );
        write_input_message(&mut client_stream, second);
        assert!(matches!(
            recv_server_event(&mut server_event_rx, "unterminated paste disconnect"),
            ServerEvent::ClientDisconnected { client_id: 7 }
        ));

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_handles_every_fragmented_paste_start_boundary() {
        const START: &[u8] = b"\x1b[200~";
        for split in 1..START.len() {
            let (mut client_stream, server_stream, _path) =
                local_stream_pair("fragmented-start-boundary");
            let (server_event_tx, mut server_event_rx) = mpsc::channel(8);
            let should_quit = Arc::new(AtomicBool::new(false));
            let read_quit = should_quit.clone();
            let handle = std::thread::spawn(move || {
                client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
            });
            let paste = bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1);

            write_input_message(&mut client_stream, paste[..split].to_vec());
            if split == 1 {
                match recv_server_event(&mut server_event_rx, "definitive lone escape") {
                    ServerEvent::ClientInput { client_id, data } => {
                        assert_eq!(client_id, 7);
                        assert_eq!(data, b"\x1b");
                    }
                    other => panic!("expected definitive lone Escape, got {other:?}"),
                }
            } else {
                write_resize_marker(&mut client_stream, 94);
                assert_resize_marker(
                    recv_server_event(&mut server_event_rx, "marker after split start"),
                    94,
                );
            }

            write_input_message(&mut client_stream, paste[split..].to_vec());
            let outcome = recv_server_event(&mut server_event_rx, "split-start outcome");
            if split == 1 {
                assert!(matches!(
                    outcome,
                    ServerEvent::ClientDisconnected { client_id: 7 }
                ));
            } else {
                assert!(matches!(
                    outcome,
                    ServerEvent::ClientPasteRejected {
                        client_id: 7,
                        size,
                        max: MAX_INPUT_PAYLOAD,
                    } if size == MAX_INPUT_PAYLOAD + 1
                ));
            }

            drop(client_stream);
            should_quit.store(true, Ordering::Release);
            handle
                .join()
                .expect("read thread join")
                .expect("read thread result");
        }
    }

    #[test]
    fn client_read_loop_disconnects_fragmented_invalid_trailing_and_multiple_pastes() {
        let mut invalid = bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1);
        invalid[b"\x1b[200~".len()] = 0xff;
        let mut trailing = bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1);
        trailing.push(b'x');
        let mut multiple = bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1);
        multiple.extend_from_slice(b"\x1b[200~two\x1b[201~");

        for (name, bytes) in [
            ("fragmented-invalid", invalid),
            ("fragmented-trailing", trailing),
            ("fragmented-multiple", multiple),
        ] {
            let (mut client_stream, server_stream, _path) = local_stream_pair(name);
            let (server_event_tx, mut server_event_rx) = mpsc::channel(8);
            let should_quit = Arc::new(AtomicBool::new(false));
            let read_quit = should_quit.clone();
            let handle = std::thread::spawn(move || {
                client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
            });
            let first_len = (MAX_INPUT_PAYLOAD / 2) + 1;

            write_input_message(&mut client_stream, bytes[..first_len].to_vec());
            write_resize_marker(&mut client_stream, 95);
            assert_resize_marker(
                recv_server_event(&mut server_event_rx, "marker after malformed fragment"),
                95,
            );
            write_input_message(&mut client_stream, bytes[first_len..].to_vec());
            assert!(matches!(
                recv_server_event(&mut server_event_rx, "malformed fragmented disconnect"),
                ServerEvent::ClientDisconnected { client_id: 7 }
            ));

            drop(client_stream);
            should_quit.store(true, Ordering::Release);
            handle
                .join()
                .expect("read thread join")
                .expect("read thread result");
        }
    }

    #[tokio::test]
    async fn client_read_loop_guards_fragmented_paste_before_direct_terminal_consumer() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("fragmented-direct-terminal");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(12);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 4,
            );

        let (first, second) = split_bracketed_paste(MAX_INPUT_PAYLOAD, (MAX_INPUT_PAYLOAD / 2) + 1);
        write_input_message(&mut client_stream, first);
        write_resize_marker(&mut client_stream, 96);
        assert_resize_marker(
            recv_server_event(&mut server_event_rx, "direct marker after held paste"),
            96,
        );
        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        write_input_message(&mut client_stream, second);
        let ServerEvent::ClientInput { client_id: 7, data } =
            recv_server_event(&mut server_event_rx, "direct completed paste")
        else {
            panic!("expected completed paste for direct consumer");
        };
        runtime
            .try_send_bytes(bytes::Bytes::from(data))
            .expect("deliver guarded paste to direct terminal");
        assert_eq!(
            input_rx
                .try_recv()
                .expect("direct terminal receives completed paste")
                .len(),
            MAX_INPUT_PAYLOAD
        );

        let (first, second) =
            split_bracketed_paste(MAX_INPUT_PAYLOAD + 1, (MAX_INPUT_PAYLOAD / 2) + 1);
        write_input_message(&mut client_stream, first);
        write_resize_marker(&mut client_stream, 97);
        assert_resize_marker(
            recv_server_event(
                &mut server_event_rx,
                "direct marker after held oversized paste",
            ),
            97,
        );
        write_input_message(&mut client_stream, second);
        assert!(matches!(
            recv_server_event(&mut server_event_rx, "direct oversized paste rejection"),
            ServerEvent::ClientPasteRejected {
                client_id: 7,
                size,
                max: MAX_INPUT_PAYLOAD,
            } if size == MAX_INPUT_PAYLOAD + 1
        ));
        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        write_input_message(&mut client_stream, b"direct after rejection".to_vec());
        let ServerEvent::ClientInput { client_id: 7, data } =
            recv_server_event(&mut server_event_rx, "direct input after rejection")
        else {
            panic!("expected direct input after rejection");
        };
        runtime
            .try_send_bytes(bytes::Bytes::from(data))
            .expect("deliver follow-up input to direct terminal");
        assert_eq!(
            input_rx.try_recv().expect("direct follow-up input"),
            bytes::Bytes::from_static(b"direct after rejection")
        );

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn raw_paste_guard_releases_false_start_prefix_without_oversized_forward() {
        let mut guard = RawPasteEnvelopeGuard::default();
        assert_eq!(
            guard.push(b"\x1b[2".to_vec()),
            vec![RawPasteEnvelopeOutcome::Hold]
        );

        let mut continuation = vec![b'x'; MAX_INPUT_PAYLOAD];
        continuation[0] = b'x';
        let outcomes = guard.push(continuation);
        let chunks = outcomes
            .into_iter()
            .map(|outcome| match outcome {
                RawPasteEnvelopeOutcome::Forward(data) => data,
                other => panic!("expected only released input, got {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|chunk| chunk.len() <= MAX_INPUT_PAYLOAD));
        assert_eq!(
            chunks.iter().map(Vec::len).sum::<usize>(),
            MAX_INPUT_PAYLOAD + 3
        );
        assert_eq!(&chunks[0][..4], b"\x1b[2x");
        assert!(chunks.iter().flatten().skip(4).all(|byte| *byte == b'x'));
        assert!(matches!(guard.state, RawPasteEnvelopeState::Idle));
    }

    #[test]
    fn raw_paste_guard_handles_every_fragmented_paste_end_boundary() {
        let mut paste_prefix = crate::raw_input::BRACKETED_PASTE_START.to_vec();
        paste_prefix.extend_from_slice(b"end-boundary");

        for split in 1..crate::raw_input::BRACKETED_PASTE_END.len() {
            let mut guard = RawPasteEnvelopeGuard::default();
            let mut first = paste_prefix.clone();
            first.extend_from_slice(&crate::raw_input::BRACKETED_PASTE_END[..split]);
            assert_eq!(
                guard.push(first),
                vec![RawPasteEnvelopeOutcome::Hold],
                "end split {split} must remain pending"
            );

            let outcomes = guard.push(crate::raw_input::BRACKETED_PASTE_END[split..].to_vec());
            let [RawPasteEnvelopeOutcome::Forward(completed)] = outcomes.as_slice() else {
                panic!("end split {split} did not produce one completed paste: {outcomes:?}");
            };
            let mut expected = paste_prefix.clone();
            expected.extend_from_slice(crate::raw_input::BRACKETED_PASTE_END);
            assert_eq!(completed, &expected, "end split {split} changed bytes");
            assert!(matches!(guard.state, RawPasteEnvelopeState::Idle));
        }
    }

    #[test]
    fn client_read_loop_disconnects_oversized_non_paste_and_malformed_pastes() {
        let mut partial = b"\x1b[200~".to_vec();
        partial.resize(MAX_INPUT_PAYLOAD + 1, b'x');

        let mut trailing = bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1);
        trailing.push(b'x');

        let first_len = (MAX_INPUT_PAYLOAD / 2) + 1;
        let mut multiple = bracketed_paste_with_total_len(first_len);
        multiple.extend_from_slice(&bracketed_paste_with_total_len(
            MAX_INPUT_PAYLOAD - first_len + 1,
        ));

        let mut invalid_utf8 = bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1);
        invalid_utf8[b"\x1b[200~".len()] = 0xff;

        let cases = [
            ("non-paste", vec![b'x'; MAX_INPUT_PAYLOAD + 1]),
            ("partial-paste", partial),
            ("trailing-paste", trailing),
            ("multiple-pastes", multiple),
            ("invalid-utf8-paste", invalid_utf8),
        ];
        for (name, data) in cases {
            assert_client_message_disconnects(name, ClientMessage::Input { data });
        }
    }

    #[test]
    fn client_read_loop_rejects_unknown_client_tag_fail_closed() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-unknown-tag");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        client_stream
            .write_all(&1_u32.to_le_bytes())
            .expect("write frame length");
        client_stream.write_all(&[10]).expect("write unknown tag");
        client_stream.flush().expect("flush unknown tag");

        match server_event_rx
            .blocking_recv()
            .expect("client disconnected event")
        {
            ServerEvent::ClientDisconnected { client_id } => assert_eq!(client_id, 7),
            other => panic!("expected ClientDisconnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_forwards_input_events() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-events");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });
        let events = vec![
            ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Enter,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            },
            ClientInputEvent::FocusGained,
        ];

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents {
                events: events.clone(),
            },
        )
        .expect("write input events");

        match server_event_rx
            .blocking_recv()
            .expect("client input events event")
        {
            ServerEvent::ClientInputEvents {
                client_id,
                events: actual,
            } => {
                assert_eq!(client_id, 7);
                assert_eq!(actual, events);
            }
            other => panic!("expected ClientInputEvents, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_rejects_oversized_input_event_batch() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-oversized-events");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents {
                events: vec![ClientInputEvent::FocusGained; MAX_INPUT_EVENT_BATCH + 1],
            },
        )
        .expect("write oversized input events");

        match server_event_rx
            .blocking_recv()
            .expect("client disconnected event")
        {
            ServerEvent::ClientDisconnected { client_id } => assert_eq!(client_id, 7),
            other => panic!("expected ClientDisconnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_rejects_oversized_structured_paste_without_disconnect() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-oversized-paste");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        let maximum = vec![
            ClientInputEvent::Paste {
                text: "x".repeat(MAX_INPUT_PAYLOAD / 2),
            },
            ClientInputEvent::Paste {
                text: "y".repeat(MAX_INPUT_PAYLOAD - (MAX_INPUT_PAYLOAD / 2)),
            },
        ];
        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents {
                events: maximum.clone(),
            },
        )
        .expect("write maximum-size structured paste");

        match recv_server_event(&mut server_event_rx, "maximum-size structured paste") {
            ServerEvent::ClientInputEvents { client_id, events } => {
                assert_eq!(client_id, 7);
                assert_eq!(events, maximum);
            }
            other => panic!("expected maximum-size ClientInputEvents, got {other:?}"),
        }

        let oversized = vec![
            ClientInputEvent::FocusGained,
            ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 1,
                row: 2,
                modifiers: 0,
            },
            ClientInputEvent::Paste {
                text: "x".repeat(MAX_INPUT_PAYLOAD / 2),
            },
            ClientInputEvent::Paste {
                text: "y".repeat(MAX_INPUT_PAYLOAD - (MAX_INPUT_PAYLOAD / 2) + 1),
            },
            ClientInputEvent::FocusLost,
            ClientInputEvent::Paste {
                text: "tail".to_owned(),
            },
        ];
        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents { events: oversized },
        )
        .expect("write oversized structured paste");

        match recv_server_event(&mut server_event_rx, "oversized structured paste rejection") {
            ServerEvent::ClientPasteRejected {
                client_id,
                size,
                max,
            } => {
                assert_eq!(client_id, 7);
                assert_eq!(size, MAX_INPUT_PAYLOAD + 5);
                assert_eq!(max, MAX_INPUT_PAYLOAD);
            }
            ServerEvent::ClientDisconnected { .. } => {
                panic!("oversized structured paste must not disconnect the client")
            }
            other => panic!("expected ClientPasteRejected, got {other:?}"),
        }

        let valid = vec![ClientInputEvent::FocusGained];
        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents {
                events: valid.clone(),
            },
        )
        .expect("write valid structured input after rejection");

        match recv_server_event(&mut server_event_rx, "structured input after rejection") {
            ServerEvent::ClientInputEvents { client_id, events } => {
                assert_eq!(client_id, 7);
                assert_eq!(events, valid);
            }
            other => panic!("expected ClientInputEvents after rejection, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_disconnects_oversized_non_paste_structured_payloads() {
        let generated_text = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: 1,
            generated_text: Some("x".repeat(MAX_INPUT_PAYLOAD + 1)),
            source: crate::protocol::ClientKeySource::Synthesized,
        };
        let vt = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Esc,
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: 1,
            generated_text: None,
            source: crate::protocol::ClientKeySource::Vt {
                bytes: vec![b'x'; MAX_INPUT_PAYLOAD + 1],
            },
        };
        let repeated_vt = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: MAX_INPUT_EVENT_BATCH as u16,
            generated_text: None,
            source: crate::protocol::ClientKeySource::Vt {
                bytes: vec![b'x'; (MAX_INPUT_PAYLOAD / MAX_INPUT_EVENT_BATCH) + 1],
            },
        };
        let cases = [
            ("generated-text", vec![generated_text]),
            (
                "text-commit",
                vec![ClientInputEvent::TextCommit(
                    "x".repeat(MAX_INPUT_PAYLOAD + 1),
                )],
            ),
            ("vt-source", vec![vt]),
            ("repeated-vt-source", vec![repeated_vt]),
            (
                "paste-plus-text",
                vec![
                    ClientInputEvent::Paste {
                        text: "p".repeat(MAX_INPUT_PAYLOAD),
                    },
                    ClientInputEvent::TextCommit("x".to_owned()),
                ],
            ),
        ];

        for (name, events) in cases {
            assert_client_message_disconnects(name, ClientMessage::InputEvents { events });
        }
    }

    #[test]
    fn structured_input_limits_charge_grouped_repeats_and_text_payloads() {
        let grouped = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: (MAX_INPUT_EVENT_BATCH + 1) as u16,
            generated_text: None,
            source: crate::protocol::ClientKeySource::Synthesized,
        };
        assert_eq!(
            input_event_limit(&[grouped]),
            InputEventLimit::TooManyEvents
        );

        let repeated_text = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: MAX_INPUT_EVENT_BATCH as u16,
            generated_text: Some("x".repeat((MAX_INPUT_PAYLOAD / MAX_INPUT_EVENT_BATCH) + 1)),
            source: crate::protocol::ClientKeySource::Synthesized,
        };
        assert!(matches!(
            input_event_limit(&[repeated_text]),
            InputEventLimit::InputPayloadTooLarge { size } if size > MAX_INPUT_PAYLOAD
        ));

        let text = ClientInputEvent::TextCommit("x".repeat(MAX_INPUT_PAYLOAD + 1));
        assert_eq!(
            input_event_limit(&[text]),
            InputEventLimit::InputPayloadTooLarge {
                size: MAX_INPUT_PAYLOAD + 1
            }
        );

        let repeated_vt = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: MAX_INPUT_EVENT_BATCH as u16,
            generated_text: None,
            source: crate::protocol::ClientKeySource::Vt {
                bytes: vec![b'x'; (MAX_INPUT_PAYLOAD / MAX_INPUT_EVENT_BATCH) + 1],
            },
        };
        assert!(matches!(
            input_event_limit(&[repeated_vt]),
            InputEventLimit::InputPayloadTooLarge { size } if size > MAX_INPUT_PAYLOAD
        ));
    }

    #[test]
    fn structured_input_limits_bound_combined_payload_and_zero_repeats() {
        let key = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: 0,
            generated_text: Some("x".to_owned()),
            source: crate::protocol::ClientKeySource::Vt {
                bytes: b"x".to_vec(),
            },
        };
        let mut events = vec![
            key.clone(),
            ClientInputEvent::TextCommit("t".to_owned()),
            ClientInputEvent::Paste {
                text: "p".repeat(MAX_INPUT_PAYLOAD - 3),
            },
        ];
        assert_eq!(input_event_limit(&events), InputEventLimit::WithinLimits);
        events.push(ClientInputEvent::TextCommit("!".to_owned()));
        assert_eq!(
            input_event_limit(&events),
            InputEventLimit::InputPayloadTooLarge {
                size: MAX_INPUT_PAYLOAD + 1
            }
        );

        let mut events = vec![key; MAX_INPUT_EVENT_BATCH];
        assert_eq!(input_event_limit(&events), InputEventLimit::WithinLimits);
        events.push(ClientInputEvent::FocusLost);
        assert_eq!(input_event_limit(&events), InputEventLimit::TooManyEvents);

        let vt = ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Esc,
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Release,
            repeat_count: 1,
            generated_text: None,
            source: crate::protocol::ClientKeySource::Vt {
                bytes: vec![0; MAX_INPUT_PAYLOAD + 1],
            },
        };
        assert_eq!(
            input_event_limit(&[vt]),
            InputEventLimit::InputPayloadTooLarge {
                size: MAX_INPUT_PAYLOAD + 1
            }
        );
    }

    #[test]
    fn handshake_timeout_is_within_five_second_deadline() {
        // The handshake timeout must be short enough that
        // the connection is guaranteed to close within 5 seconds even with
        // OS overhead (thread scheduling, timer slack, cleanup).
        assert!(
            HANDSHAKE_TIMEOUT < Duration::from_secs(5),
            "HANDSHAKE_TIMEOUT ({:?}) must be less than 5 seconds to guarantee \
             connection close within the 5-second deadline",
            HANDSHAKE_TIMEOUT
        );
    }
}
