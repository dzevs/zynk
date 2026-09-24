// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneGraphicsSetParams, PaneGraphicsStreamOpenParams,
    PaneGraphicsStreamParams, Request, ResponseResult, SuccessResponse,
};
use crate::api::{ApiCaller, ApiRequestSender};
use crate::ipc::{is_connection_closed_error, LocalStream};

use super::{
    api_response_outcome, dispatch_to_app_with_timeout, write_json_line,
    write_json_line_allow_disconnect, write_text_line_allow_disconnect, APP_RESPONSE_TIMEOUT,
    CONNECTION_POLL_INTERVAL,
};

const MAX_STREAM_FRAME_HEADER_BYTES: usize = 64 * 1024;
const STREAM_FRAME_BODY_CHUNK_BYTES: usize = 64 * 1024;
const STREAM_FRAME_HEADER_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_FRAME_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_FRAME_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_FRAME_BODY_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_FALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(1);
const STREAM_FALLBACK_FAST_POLLS: u8 = 32;
static NEXT_PANE_GRAPHICS_STREAM_OWNER: AtomicU64 = AtomicU64::new(1);

#[derive(serde::Deserialize)]
struct FrameFile {
    path: String,
}

struct ConnectionLifetime(Arc<AtomicBool>);

impl Drop for ConnectionLifetime {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(serde::Deserialize)]
struct FrameHeader {
    format: crate::api::schema::PaneGraphicsFormat,
    image_width: u32,
    image_height: u32,
    #[serde(default)]
    data_length: Option<usize>,
    #[serde(default)]
    file: Option<FrameFile>,
    #[serde(default)]
    sequence: u64,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    placement: crate::api::schema::PaneGraphicsPlacementParams,
}

#[derive(Clone, Copy)]
struct ReadTimeouts {
    header_idle: Duration,
    header_total: Duration,
    body_idle: Duration,
    body_total: Duration,
}

const READ_TIMEOUTS: ReadTimeouts = ReadTimeouts {
    header_idle: STREAM_FRAME_HEADER_IDLE_TIMEOUT,
    header_total: STREAM_FRAME_HEADER_TIMEOUT,
    body_idle: STREAM_FRAME_BODY_IDLE_TIMEOUT,
    body_total: STREAM_FRAME_BODY_TIMEOUT,
};

pub(super) fn serve(
    stream: LocalStream,
    request_id: String,
    params: PaneGraphicsStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    caller: ApiCaller,
) -> std::io::Result<()> {
    serve_with_open_timeout(
        stream,
        request_id,
        params,
        api_tx,
        running,
        APP_RESPONSE_TIMEOUT,
        caller,
    )
}

fn serve_with_open_timeout(
    stream: LocalStream,
    request_id: String,
    params: PaneGraphicsStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    open_timeout: Duration,
    caller: ApiCaller,
) -> std::io::Result<()> {
    serve_with_timeouts(
        stream,
        request_id,
        params,
        api_tx,
        running,
        open_timeout,
        READ_TIMEOUTS,
        caller,
    )
}

fn serve_with_timeouts(
    mut stream: LocalStream,
    request_id: String,
    mut params: PaneGraphicsStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    open_timeout: Duration,
    read_timeouts: ReadTimeouts,
    caller: ApiCaller,
) -> std::io::Result<()> {
    let pane_id = params.pane_id.clone();
    let layer_id = params.layer_id.clone();
    let z_index = params.z_index;
    let owner = next_owner();
    params.owner = owner.clone();
    let stream_active = Arc::new(AtomicBool::new(true));
    let lifetime = ConnectionLifetime(stream_active.clone());
    let open_response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params,
                active: stream_active.clone(),
            }),
        },
        api_tx,
        Some(open_timeout),
        caller,
    );
    if api_response_outcome(&open_response) != "ok" {
        drop(lifetime);
        let write_result = write_text_line_allow_disconnect(&mut stream, &open_response);
        clear_layer(
            &pane_id,
            layer_id.as_deref(),
            z_index,
            &owner,
            api_tx,
            caller,
        );
        write_result?;
        return Ok(());
    }

    if let Err(err) = write_json_line(
        &mut stream,
        &SuccessResponse {
            id: request_id.clone(),
            result: ResponseResult::Ok {},
        },
    ) {
        drop(lifetime);
        clear_layer(
            &pane_id,
            layer_id.as_deref(),
            z_index,
            &owner,
            api_tx,
            caller,
        );
        if is_connection_closed_error(&err) {
            return Ok(());
        }
        return Err(err);
    }

    let result = serve_frames(
        &mut stream,
        &request_id,
        &owner,
        &pane_id,
        layer_id.as_deref(),
        z_index,
        api_tx,
        running,
        &stream_active,
        read_timeouts,
        caller,
    );
    drop(lifetime);
    clear_layer(
        &pane_id,
        layer_id.as_deref(),
        z_index,
        &owner,
        api_tx,
        caller,
    );
    result
}

fn serve_frames(
    stream: &mut LocalStream,
    request_id: &str,
    owner: &str,
    pane_id: &str,
    layer_id: Option<&str>,
    z_index: i32,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    stream_active: &Arc<AtomicBool>,
    timeouts: ReadTimeouts,
    caller: ApiCaller,
) -> std::io::Result<()> {
    let mut frame_seq = 0_u64;
    while stream_is_running(running, stream_active) {
        let Some(header_line) = read_line(
            stream,
            running,
            stream_active,
            MAX_STREAM_FRAME_HEADER_BYTES,
            timeouts.header_idle,
            timeouts.header_total,
        )?
        else {
            return Ok(());
        };
        let header_line = header_line.trim();
        if header_line.is_empty() {
            continue;
        }
        let header = match serde_json::from_str::<FrameHeader>(header_line) {
            Ok(header) => header,
            Err(err) => {
                write_json_line_allow_disconnect(
                    stream,
                    &ErrorResponse {
                        id: request_id.to_string(),
                        error: ErrorBody {
                            code: "invalid_frame".into(),
                            message: format!("invalid frame header: {err}"),
                        },
                    },
                )?;
                return Ok(());
            }
        };
        if let Some(file) = header.file {
            if !matches!(
                header.format,
                crate::api::schema::PaneGraphicsFormat::Rgba
                    | crate::api::schema::PaneGraphicsFormat::Bgra
            ) {
                write_json_line_allow_disconnect(
                    stream,
                    &ErrorResponse {
                        id: request_id.to_string(),
                        error: ErrorBody {
                            code: "invalid_frame".into(),
                            message: "file frames require rgba or bgra".into(),
                        },
                    },
                )?;
                return Ok(());
            }
            let response = dispatch_to_app_with_timeout(
                Request {
                    id: format!("{request_id}:file:{}", header.sequence),
                    method: Method::PaneGraphicsStreamDirect(
                        crate::api::schema::PaneGraphicsDirectParams {
                            pane_id: pane_id.to_owned(),
                            layer_id: layer_id.map(str::to_owned),
                            z_index,
                            owner: owner.to_owned(),
                            image_width: header.image_width,
                            image_height: header.image_height,
                            format: header.format,
                            path: file.path,
                            sequence: header.sequence,
                            revision: header.revision,
                            placement: header.placement,
                        },
                    ),
                },
                api_tx,
                Some(crate::app::pane_graphics::DIRECT_OUTER_TIMEOUT),
                caller,
            );
            write_text_line_allow_disconnect(stream, &response)?;
            if api_response_outcome(&response) != "ok" {
                return Ok(());
            }
            continue;
        }

        let Some(data_length) = header.data_length else {
            write_json_line_allow_disconnect(
                stream,
                &ErrorResponse {
                    id: request_id.to_string(),
                    error: ErrorBody {
                        code: "invalid_frame".into(),
                        message: "frame requires data_length or file".into(),
                    },
                },
            )?;
            return Ok(());
        };
        if data_length == 0 {
            write_json_line_allow_disconnect(
                stream,
                &ErrorResponse {
                    id: request_id.to_string(),
                    error: ErrorBody {
                        code: "invalid_frame".into(),
                        message: "frame data_length must be greater than zero".into(),
                    },
                },
            )?;
            return Ok(());
        }
        if data_length > crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES {
            write_json_line_allow_disconnect(
                stream,
                &ErrorResponse {
                    id: request_id.to_string(),
                    error: ErrorBody {
                        code: "image_too_large".into(),
                        message: "frame data is too large".into(),
                    },
                },
            )?;
            return Ok(());
        }

        let Some(data) = read_exact(
            stream,
            data_length,
            running,
            stream_active,
            timeouts.body_idle,
            timeouts.body_total,
        )?
        else {
            return Ok(());
        };

        frame_seq = frame_seq.saturating_add(1);
        let frame_id = format!("{request_id}:frame:{frame_seq}");
        let response = dispatch_to_app_with_timeout(
            Request {
                id: frame_id,
                method: Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                    pane_id: pane_id.to_string(),
                    layer_id: layer_id.map(str::to_owned),
                    z_index,
                    owner: owner.to_string(),
                    format: header.format,
                    image_width: header.image_width,
                    image_height: header.image_height,
                    data: Some(data),
                    data_base64: String::new(),
                    placement: header.placement,
                }),
            },
            api_tx,
            Some(APP_RESPONSE_TIMEOUT),
            caller,
        );
        if api_response_outcome(&response) != "ok" {
            write_text_line_allow_disconnect(stream, &response)?;
            return Ok(());
        }
    }

    Ok(())
}

fn stream_is_running(running: &AtomicBool, stream_active: &AtomicBool) -> bool {
    running.load(Ordering::Relaxed) && stream_active.load(Ordering::Acquire)
}

fn next_owner() -> String {
    let id = NEXT_PANE_GRAPHICS_STREAM_OWNER.fetch_add(1, Ordering::Relaxed);
    format!("pane.graphics.stream:{}:{id}", std::process::id())
}

fn clear_layer(
    pane_id: &str,
    layer_id: Option<&str>,
    z_index: i32,
    owner: &str,
    api_tx: &ApiRequestSender,
    caller: ApiCaller,
) {
    let _response = dispatch_to_app_with_timeout(
        Request {
            id: format!("pane.graphics.stream.clear:{pane_id}"),
            method: Method::PaneGraphicsStreamClose(PaneGraphicsStreamParams {
                pane_id: pane_id.to_string(),
                layer_id: layer_id.map(str::to_owned),
                z_index,
                owner: owner.to_string(),
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
        caller,
    );
}

pub(super) fn read_line(
    stream: &mut LocalStream,
    running: &Arc<AtomicBool>,
    stream_active: &Arc<AtomicBool>,
    max_bytes: usize,
    idle_timeout: Duration,
    total_timeout: Duration,
) -> std::io::Result<Option<String>> {
    with_timed_reads(stream, |stream, mut wait| {
        let mut bytes = Vec::with_capacity(max_bytes);
        let mut byte = [0_u8; 1];
        let mut total_deadline = None;
        let mut idle_deadline = None;

        loop {
            if !stream_is_running(running, stream_active) {
                return Ok(None);
            }
            ensure_before_deadlines(
                idle_deadline,
                total_deadline,
                "timed out reading stream frame header",
            )?;
            match stream.read(&mut byte) {
                Ok(0) => return Ok(None),
                Ok(_) => {
                    wait.on_progress();
                    let now = Instant::now();
                    let total_deadline_at =
                        *total_deadline.get_or_insert_with(|| now + total_timeout);
                    idle_deadline = Some(now + idle_timeout);
                    if now >= total_deadline_at {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out reading stream frame header",
                        ));
                    }
                    if bytes.len() >= max_bytes {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "stream frame header is too large",
                        ));
                    }
                    bytes.push(byte[0]);
                    if byte[0] == b'\n' {
                        return String::from_utf8(bytes)
                            .map(Some)
                            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
                    }
                }
                Err(err) if read_should_retry(&err) => {
                    wait.after_retry(idle_deadline, total_deadline);
                }
                Err(err) if is_connection_closed_error(&err) => return Ok(None),
                Err(err) => return Err(err),
            }
        }
    })
}

fn read_exact(
    stream: &mut LocalStream,
    len: usize,
    running: &Arc<AtomicBool>,
    stream_active: &Arc<AtomicBool>,
    idle_timeout: Duration,
    total_timeout: Duration,
) -> std::io::Result<Option<Vec<u8>>> {
    with_timed_reads(stream, |stream, mut wait| {
        let mut data = Vec::new();
        let mut chunk = vec![0_u8; STREAM_FRAME_BODY_CHUNK_BYTES.min(len)];
        let total_deadline = Instant::now() + total_timeout;
        let mut idle_deadline = Instant::now() + idle_timeout;

        while data.len() < len {
            if !stream_is_running(running, stream_active) {
                return Ok(None);
            }
            ensure_before_deadlines(
                Some(idle_deadline),
                Some(total_deadline),
                "timed out reading stream frame body",
            )?;
            let remaining = len - data.len();
            let read_len = remaining.min(chunk.len());
            match stream.read(&mut chunk[..read_len]) {
                Ok(0) if data.is_empty() => return Ok(None),
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "stream ended mid-frame",
                    ));
                }
                Ok(n) => {
                    wait.on_progress();
                    let now = Instant::now();
                    if now >= total_deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out reading stream frame body",
                        ));
                    }
                    data.extend_from_slice(&chunk[..n]);
                    idle_deadline = now + idle_timeout;
                }
                Err(err) if read_should_retry(&err) => {
                    wait.after_retry(Some(idle_deadline), Some(total_deadline));
                }
                Err(err) if is_connection_closed_error(&err) && data.is_empty() => return Ok(None),
                Err(err) => return Err(err),
            }
        }

        Ok(Some(data))
    })
}

#[derive(Clone, Copy)]
enum ReadWait {
    SocketTimeout,
    Poll(PollBackoff),
}

impl ReadWait {
    fn after_retry(&mut self, idle_deadline: Option<Instant>, total_deadline: Option<Instant>) {
        if let Self::Poll(backoff) = self {
            sleep_until_poll(idle_deadline, total_deadline, backoff.interval);
            backoff.advance();
        }
    }

    fn on_progress(&mut self) {
        if let Self::Poll(backoff) = self {
            backoff.reset();
        }
    }
}

#[derive(Clone, Copy)]
struct PollBackoff {
    interval: Duration,
    fast_polls_remaining: u8,
}

impl PollBackoff {
    fn new() -> Self {
        Self {
            interval: STREAM_FALLBACK_POLL_INTERVAL,
            fast_polls_remaining: STREAM_FALLBACK_FAST_POLLS,
        }
    }

    fn advance(&mut self) {
        if self.fast_polls_remaining > 0 {
            self.fast_polls_remaining -= 1;
            return;
        }
        self.interval = (self.interval * 2).min(CONNECTION_POLL_INTERVAL);
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

fn with_timed_reads<T>(
    stream: &mut LocalStream,
    read: impl FnOnce(&mut LocalStream, ReadWait) -> std::io::Result<Option<T>>,
) -> std::io::Result<Option<T>> {
    let setup = stream.set_recv_timeout(Some(CONNECTION_POLL_INTERVAL));
    with_timed_read_setup(stream, setup, read)
}

fn with_timed_read_setup<T>(
    stream: &mut LocalStream,
    setup: std::io::Result<()>,
    read: impl FnOnce(&mut LocalStream, ReadWait) -> std::io::Result<Option<T>>,
) -> std::io::Result<Option<T>> {
    match setup {
        Ok(()) => {
            let result = read(stream, ReadWait::SocketTimeout);
            finish_timed_read(result, || stream.set_recv_timeout(None))
        }
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            stream.set_nonblocking(true)?;
            let result = read(stream, ReadWait::Poll(PollBackoff::new()));
            finish_timed_read(result, || stream.set_nonblocking(false))
        }
        Err(err) => Err(err),
    }
}

fn finish_timed_read<T>(
    result: std::io::Result<Option<T>>,
    reset: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<Option<T>> {
    match result {
        // None is terminal for this dedicated stream.
        Ok(None) => Ok(None),
        Ok(value) => {
            reset()?;
            Ok(value)
        }
        Err(err) => {
            let _ = reset();
            Err(err)
        }
    }
}

fn ensure_before_deadlines(
    idle_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    message: &str,
) -> std::io::Result<()> {
    let now = Instant::now();
    if idle_deadline.is_some_and(|deadline| now >= deadline)
        || total_deadline.is_some_and(|deadline| now >= deadline)
    {
        return Err(io::Error::new(io::ErrorKind::TimedOut, message));
    }
    Ok(())
}

fn sleep_until_poll(
    idle_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    poll_interval: Duration,
) {
    let now = Instant::now();
    let until_deadline = [idle_deadline, total_deadline]
        .into_iter()
        .flatten()
        .filter_map(|deadline| deadline.checked_duration_since(now))
        .min()
        .unwrap_or(poll_interval);
    std::thread::sleep(poll_interval.min(until_deadline));
}

fn read_should_retry(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn browser_file_header_accepts_damage_but_keeps_full_canonical_frame() {
        let header: FrameHeader = serde_json::from_str(
            r#"{"format":"rgba","image_width":2,"image_height":3,"sequence":7,"revision":8,"file":{"path":"/private/frame"},"damage":{"x":1,"y":1,"width":1,"height":1},"transport":"direct-kitty","placement":{"grid_cols":2,"grid_rows":3}}"#,
        )
        .unwrap();
        assert_eq!(header.data_length, None);
        assert_eq!(header.file.unwrap().path, "/private/frame");
        assert_eq!((header.sequence, header.revision), (7, 8));
        assert_eq!((header.image_width, header.image_height), (2, 3));
        assert_eq!(
            (header.placement.grid_cols, header.placement.grid_rows),
            (2, 3)
        );
    }

    #[test]
    fn m836_terminal_none_never_invokes_reset() {
        for reset_fails in [false, true] {
            let mut calls = 0;
            let result = finish_timed_read::<u8>(Ok(None), || {
                calls += 1;
                if reset_fails {
                    Err(io::Error::new(io::ErrorKind::InvalidInput, "reset-error"))
                } else {
                    Ok(())
                }
            });
            assert_eq!((calls, result.map_err(m836_error)), (0, Ok(None)));
        }
    }

    #[test]
    fn m836_some_resets_once_and_preserves_value() {
        let mut calls = 0;
        let result = finish_timed_read(Ok(Some(17)), || {
            calls += 1;
            Ok(())
        });
        assert_eq!((calls, result.map_err(m836_error)), (1, Ok(Some(17))));
    }

    #[test]
    fn m836_some_propagates_reset_error() {
        let mut calls = 0;
        let result = finish_timed_read(Ok(Some(17)), || {
            calls += 1;
            Err(io::Error::new(io::ErrorKind::InvalidInput, "reset-error"))
        });
        assert_eq!(
            (calls, result.map_err(m836_error)),
            (1, Err((io::ErrorKind::InvalidInput, "reset-error".into())))
        );
    }

    #[test]
    fn m836_read_error_wins_after_one_reset_attempt() {
        for reset_fails in [false, true] {
            let mut calls = 0;
            let result = finish_timed_read::<u8>(
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "read-error")),
                || {
                    calls += 1;
                    if reset_fails {
                        Err(io::Error::new(io::ErrorKind::InvalidInput, "reset-error"))
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(
                (calls, result.map_err(m836_error)),
                (1, Err((io::ErrorKind::BrokenPipe, "read-error".into())))
            );
        }
    }

    #[test]
    fn m836_setup_errors_propagate_without_read_or_mode_change() {
        let (_peer, mut stream, _path) = local_stream_pair("m836-setup-errors");
        for kind in [io::ErrorKind::InvalidInput, io::ErrorKind::PermissionDenied] {
            assert!(!m836_nonblocking(&stream));
            let mut reads = 0;
            let result = with_timed_read_setup(
                &mut stream,
                Err(io::Error::new(kind, "setup-error")),
                |_, _| {
                    reads += 1;
                    Ok(Some(17))
                },
            );
            assert_eq!(
                (reads, result.map_err(m836_error)),
                (0, Err((kind, "setup-error".into())))
            );
            assert!(!m836_nonblocking(&stream));
        }
    }

    #[test]
    fn m836_injected_unsupported_executes_poll_strategy_and_completion() {
        for outcome in [0, 1, 2] {
            let (_peer, mut stream, _path) = local_stream_pair("m836-fallback");
            let mut reads = 0;
            let result = with_timed_read_setup(
                &mut stream,
                Err(io::Error::new(io::ErrorKind::Unsupported, "injected")),
                |stream, wait| {
                    reads += 1;
                    assert!(matches!(wait, ReadWait::Poll(_)));
                    assert!(m836_nonblocking(stream));
                    match outcome {
                        0 => Ok(None),
                        1 => Ok(Some(17)),
                        _ => Err(io::Error::new(io::ErrorKind::BrokenPipe, "read-error")),
                    }
                },
            );
            let expected = match outcome {
                0 => Ok(None),
                1 => Ok(Some(17)),
                _ => Err((io::ErrorKind::BrokenPipe, "read-error".into())),
            };
            assert_eq!((reads, result.map_err(m836_error)), (1, expected));
            assert_eq!(m836_nonblocking(&stream), outcome == 0);
        }
    }

    fn m836_error(error: io::Error) -> (io::ErrorKind, String) {
        (error.kind(), error.to_string())
    }

    fn m836_nonblocking(stream: &LocalStream) -> bool {
        use std::os::fd::AsRawFd;
        let LocalStream::UdSocket(socket) = stream;
        // SAFETY: the borrowed socket owns the live descriptor throughout this query.
        let flags = unsafe { libc::fcntl(socket.inner().as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "F_GETFL: {}", io::Error::last_os_error());
        flags & libc::O_NONBLOCK != 0
    }

    fn m836_timeout(stream: &LocalStream) -> Option<Duration> {
        let LocalStream::UdSocket(socket) = stream;
        socket.inner().read_timeout().unwrap()
    }

    #[test]
    fn m836_real_timeout_setup_selects_timed_read() {
        for outcome in [0, 1, 2] {
            let (mut peer, mut stream, _path) = local_stream_pair("m836-timed");
            assert_eq!(m836_timeout(&stream), None);
            peer.write_all(b"T").unwrap();
            let mut reads = 0;
            let mut installed = None;
            let result = with_timed_reads(&mut stream, |stream, wait| {
                reads += 1;
                assert!(matches!(wait, ReadWait::SocketTimeout));
                installed = m836_timeout(stream);
                assert!(installed.is_some());
                let mut byte = [0];
                stream.read_exact(&mut byte)?;
                assert_eq!(byte, [b'T']);
                match outcome {
                    0 => Ok(None),
                    1 => Ok(Some(byte)),
                    _ => Err(io::Error::new(io::ErrorKind::BrokenPipe, "read-error")),
                }
            });
            let expected = match outcome {
                0 => Ok(None),
                1 => Ok(Some(*b"T")),
                _ => Err((io::ErrorKind::BrokenPipe, "read-error".into())),
            };
            assert_eq!((reads, result.map_err(m836_error)), (1, expected));
            assert_eq!(
                m836_timeout(&stream),
                if outcome == 0 { installed } else { None }
            );
        }
    }

    #[test]
    fn m834_stream_pair_names_are_short_unique_and_independent_of_label() {
        let long_label = "label".repeat(100);
        let (mut first, mut first_peer, first_path) = local_stream_pair(&long_label);
        let (mut second, mut second_peer, second_path) = local_stream_pair(&long_label);
        assert_ne!(first_path, second_path);
        for path in [&first_path, &second_path] {
            let name = path.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with(&format!("hpg-{}-", std::process::id())));
            assert!(name.len() <= 45);
            assert!(!name.contains("label"));
        }
        first_peer
            .set_recv_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        second_peer
            .set_recv_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        first.write_all(b"A").unwrap();
        second.write_all(b"B").unwrap();
        let mut byte = [0];
        first_peer.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'A']);
        second_peer.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'B']);
    }

    use super::*;
    use crate::api::schema::{ErrorResponse, Method, ResponseResult, SuccessResponse};
    use crate::api::{ApiRequestMessage, EventHub};
    use crate::ipc::LocalStream;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    static NEXT_LOCAL_STREAM_ID: AtomicU64 = AtomicU64::new(1);

    fn local_stream_pair(_name: &str) -> (LocalStream, LocalStream, PathBuf) {
        let unique = format!(
            "hpg-{}-{}.sock",
            std::process::id(),
            NEXT_LOCAL_STREAM_ID.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, path)
    }

    fn read_response_line(stream: &mut LocalStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    fn assert_server_stream_owner(owner: &str) {
        assert!(owner.starts_with("pane.graphics.stream:"));
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_dispatches_binary_frames() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-pane-graphics-stream");
        client
            .write_all(
                br#"{"id":"stream_1","method":"pane.graphics.stream","params":{"pane_id":"pane_1"}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                let params = &params.params;
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_1");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let ack: SuccessResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(ack.id, "stream_1");
        assert_eq!(ack.result, ResponseResult::Ok {});

        client
            .write_all(
                br#"{"format":"png","image_width":2,"image_height":1,"data_length":4,"placement":{"grid_cols":10,"grid_rows":5}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.write_all(&[1_u8, 2, 3, 4]).unwrap();
        client.flush().unwrap();

        let msg = api_rx.blocking_recv().unwrap();
        match &msg.request.method {
            Method::PaneGraphicsStreamSet(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
                assert_eq!(params.image_width, 2);
                assert_eq!(params.image_height, 1);
                assert_eq!(params.data.as_deref(), Some(&[1_u8, 2, 3, 4][..]));
                assert!(params.data_base64.is_empty());
                assert_eq!(params.placement.grid_cols, 10);
                assert_eq!(params.placement.grid_rows, 5);
            }
            other => panic!("unexpected request: {other:?}"),
        }
        msg.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: msg.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(client);
        running.store(false, Ordering::Relaxed);
        let clear = api_rx.blocking_recv().unwrap();
        match &clear.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected clear request: {other:?}"),
        }
        clear
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: clear.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_reports_open_errors_before_ack() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-pane-graphics-stream-error");
        client
            .write_all(
                br#"{"id":"stream_2","method":"pane.graphics.stream","params":{"pane_id":"pane_1"}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                let params = &params.params;
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_2");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(super::super::error_response_json(
                open.request.id,
                "feature_disabled",
                "pane graphics require experimental.kitty_graphics".into(),
            ))
            .unwrap();

        let response: ErrorResponse =
            serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(response.id, "stream_2");
        assert_eq!(response.error.code, "feature_disabled");

        let close = api_rx.blocking_recv().unwrap();
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_closes_claim_after_open_timeout() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-pane-graphics-stream-timeout");
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server_thread = std::thread::spawn(move || {
            serve_with_open_timeout(
                server,
                "stream_timeout".into(),
                PaneGraphicsStreamParams {
                    pane_id: "pane_1".into(),
                    layer_id: None,
                    z_index: 0,
                    owner: String::new(),
                },
                &api_tx,
                &server_running,
                Duration::from_millis(10),
                crate::api::ApiCaller::default(),
            )
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                let params = &params.params;
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_timeout");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };

        let response: ErrorResponse =
            serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(response.id, "stream_timeout");
        assert_eq!(response.error.code, "server_unavailable");
        assert!(response.error.message.contains("timed out"));

        let close = api_rx.blocking_recv().unwrap();
        let Method::PaneGraphicsStreamOpen(pending) = &open.request.method else {
            unreachable!()
        };
        assert!(
            !pending.active.load(Ordering::Acquire),
            "open clone retained before close response"
        );
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(open);
        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_closes_claim_when_client_disconnects_before_ack() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) =
            local_stream_pair("api-pane-graphics-stream-ack-disconnect");
        client
            .write_all(
                br#"{"id":"stream_3","method":"pane.graphics.stream","params":{"pane_id":"pane_1"}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                let params = &params.params;
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_3");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };

        drop(client);
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let (close_tx, close_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            close_tx.send(api_rx.blocking_recv()).unwrap();
        });
        let close = close_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[test]
    fn idle_graphics_stream_waits_for_header_without_timing_out() {
        let (_client, mut server, _path) = local_stream_pair("graphics-idle-header");
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::new(AtomicBool::new(true));
        let stop = Arc::clone(&running);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            stop.store(false, Ordering::Relaxed);
        });

        let result = read_line(
            &mut server,
            &running,
            &active,
            MAX_STREAM_FRAME_HEADER_BYTES,
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();

        assert!(result.is_none());
        stopper.join().unwrap();
    }

    #[test]
    fn inactive_owner_cancels_idle_stream_and_dispatches_close() {
        let (mut client, server, _path) = local_stream_pair("graphics-owner-cancel");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server_thread = std::thread::spawn(move || {
            serve_with_timeouts(
                server,
                "stream-cancel".into(),
                PaneGraphicsStreamParams {
                    pane_id: "pane_1".into(),
                    layer_id: None,
                    z_index: 0,
                    owner: String::new(),
                },
                &api_tx,
                &server_running,
                Duration::from_secs(1),
                READ_TIMEOUTS,
                crate::api::ApiCaller::default(),
            )
        });

        let open = api_rx.blocking_recv().unwrap();
        let (owner, active) = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                (params.params.owner.clone(), params.active.clone())
            }
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();
        let ack: SuccessResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(ack.id, "stream-cancel");

        active.store(false, Ordering::Release);

        let (close_tx, close_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || close_tx.send(api_rx.blocking_recv()).unwrap());
        let close = close_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("canceled idle stream should dispatch a close")
            .expect("API request channel should remain open");
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[test]
    fn fallback_poll_backoff_preserves_fast_window_then_reaches_poll_ceiling() {
        let mut backoff = PollBackoff::new();
        for _ in 0..STREAM_FALLBACK_FAST_POLLS {
            backoff.advance();
            assert_eq!(backoff.interval, STREAM_FALLBACK_POLL_INTERVAL);
        }

        backoff.advance();
        assert_eq!(backoff.interval, Duration::from_millis(2));
        for _ in 0..6 {
            backoff.advance();
        }
        assert_eq!(backoff.interval, CONNECTION_POLL_INTERVAL);

        backoff.reset();
        assert_eq!(backoff.interval, STREAM_FALLBACK_POLL_INTERVAL);
        assert_eq!(backoff.fast_polls_remaining, STREAM_FALLBACK_FAST_POLLS);
    }

    #[test]
    fn partial_graphics_header_times_out_after_first_byte() {
        let (mut client, mut server, _path) = local_stream_pair("graphics-partial-header");
        client.write_all(b"{").unwrap();
        client.flush().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::new(AtomicBool::new(true));

        let error = read_line(
            &mut server,
            &running,
            &active,
            MAX_STREAM_FRAME_HEADER_BYTES,
            Duration::from_millis(20),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn trickled_graphics_body_obeys_absolute_deadline() {
        let (mut client, mut server, _path) = local_stream_pair("graphics-trickle-body");
        client.write_all(&[1_u8]).unwrap();
        client.flush().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::new(AtomicBool::new(true));
        let writer_running = Arc::clone(&running);
        let writer = std::thread::spawn(move || {
            while writer_running.load(Ordering::Relaxed) {
                if client.write_all(&[1_u8]).is_err() {
                    break;
                }
                let _ = client.flush();
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let started = Instant::now();
        let error = read_exact(
            &mut server,
            1024,
            &running,
            &active,
            Duration::from_millis(20),
            Duration::from_millis(60),
        )
        .unwrap_err();

        running.store(false, Ordering::Relaxed);
        writer.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn timed_out_header_dispatches_owner_scoped_stream_close() {
        let (mut client, server, _path) = local_stream_pair("graphics-timeout-close");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server_thread = std::thread::spawn(move || {
            serve_with_timeouts(
                server,
                "stream-timeout".into(),
                PaneGraphicsStreamParams {
                    pane_id: "pane_1".into(),
                    layer_id: None,
                    z_index: 0,
                    owner: String::new(),
                },
                &api_tx,
                &server_running,
                Duration::from_secs(1),
                ReadTimeouts {
                    header_idle: Duration::from_millis(20),
                    header_total: Duration::from_millis(100),
                    body_idle: Duration::from_millis(20),
                    body_total: Duration::from_millis(100),
                },
                crate::api::ApiCaller::default(),
            )
        });

        let open = api_rx.blocking_recv().unwrap();
        let owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => params.params.owner.clone(),
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();
        let ack: SuccessResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(ack.id, "stream-timeout");
        client.write_all(b"{").unwrap();
        client.flush().unwrap();

        let (close_tx, close_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || close_tx.send(api_rx.blocking_recv()).unwrap());
        let close = close_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let error = server_thread.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn oversized_stream_frame_is_rejected_before_body_or_app_dispatch() {
        let (mut client, mut server, _path) = local_stream_pair("graphics-oversized-frame");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let stream_active = Arc::new(AtomicBool::new(true));
        let server_thread = std::thread::spawn(move || {
            serve_frames(
                &mut server,
                "stream-oversized",
                "owner-1",
                "pane_1",
                None,
                0,
                &api_tx,
                &server_running,
                &stream_active,
                READ_TIMEOUTS,
                crate::api::ApiCaller::default(),
            )
        });
        let header = serde_json::json!({
            "format": "png",
            "image_width": 1,
            "image_height": 1,
            "data_length": crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES + 1,
        });
        client.write_all(format!("{header}\n").as_bytes()).unwrap();
        client.flush().unwrap();

        let response: ErrorResponse =
            serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(response.error.code, "image_too_large");
        assert!(server_thread.join().unwrap().is_ok());
        assert!(api_rx.try_recv().is_err());
    }
}
