// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
//! Stdin input reading for the thin client.
//!
//! Reads stdin bytes and forwards framed input to the main event loop.
//! The server handles semantic parsing.
//!
//! This is simpler and more reliable because:
//! - The server has the same input parsing code
//! - We avoid duplicating parsing logic in the client
//! - Host terminal control replies can be buffered or discarded before they leak

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::ClientLoopEvent;

// ---------------------------------------------------------------------------
// Stdin reader thread
// ---------------------------------------------------------------------------

/// Reads raw bytes from stdin and sends them to the main event loop.
///
/// This runs on a dedicated thread because stdin reading is blocking.
/// The main loop receives the raw bytes and forwards them as
/// `ClientMessage::Input` to the server.
pub fn stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
    host_cell_size_query_sent: bool,
    host_mouse_capture_active: Arc<AtomicBool>,
    host_sgr_pixels_active: Arc<AtomicBool>,
    direct_response: Arc<std::sync::Mutex<super::direct_graphics::ResponseMatcher>>,
    direct_response_active: Arc<AtomicBool>,
) {
    unix_stdin_reader_loop(
        event_tx,
        should_quit,
        host_color_query_sent,
        host_cell_size_query_sent,
        host_mouse_capture_active,
        host_sgr_pixels_active,
        direct_response,
        direct_response_active,
    );
}

fn unix_stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
    host_cell_size_query_sent: bool,
    host_mouse_capture_active: Arc<AtomicBool>,
    host_sgr_pixels_active: Arc<AtomicBool>,
    direct_response: Arc<std::sync::Mutex<super::direct_graphics::ResponseMatcher>>,
    direct_response_active: Arc<AtomicBool>,
) {
    let stdin = io::stdin();
    unix_input_reader_loop(
        stdin.lock(),
        event_tx,
        should_quit,
        host_color_query_sent,
        host_cell_size_query_sent,
        host_mouse_capture_active,
        host_sgr_pixels_active,
        direct_response,
        direct_response_active,
    );
}

fn unix_input_reader_loop<R: Read + AsRawFd>(
    mut reader: R,
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
    host_cell_size_query_sent: bool,
    host_mouse_capture_active: Arc<AtomicBool>,
    host_sgr_pixels_active: Arc<AtomicBool>,
    direct_response: Arc<std::sync::Mutex<super::direct_graphics::ResponseMatcher>>,
    direct_response_active: Arc<AtomicBool>,
) {
    let mut scratch = [0u8; 4096];
    let mut framer = crate::raw_input::RawInputByteFramer::for_host_input();
    if host_color_query_sent {
        framer.host_color_query_sent();
        framer.enable_host_color_scheme_change_tracking();
        framer.enable_host_appearance_query_on_focus();
    }
    if host_cell_size_query_sent {
        framer.host_cell_size_query_sent();
    }
    let mut pending_palette = Vec::new();
    let mut pending_mode = None;
    let mut last_geometry = None;
    let mut direct_filter = super::direct_graphics::InputFilter::default();

    while !should_quit.load(Ordering::Acquire) {
        if direct_filter.has_pending()
            && stdin_read_ready(&reader, crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS)
                == Some(false)
        {
            let released = direct_response
                .lock()
                .ok()
                .and_then(|mut matcher| direct_filter.flush_if_inactive(&mut matcher));
            if let Some(data) = released {
                if event_tx
                    .blocking_send(ClientLoopEvent::StdinInput(data))
                    .is_err()
                {
                    return;
                }
            }
            continue;
        }
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => {
                let sgr_pixels = *pending_mode
                    .get_or_insert_with(|| host_sgr_pixels_active.load(Ordering::Acquire));
                if sgr_pixels {
                    last_geometry = retain_geometry(
                        last_geometry,
                        crate::input::mouse::HostGeometry::current(),
                    );
                }
                let filtered = filter_direct_input(
                    &scratch[..n],
                    &mut direct_filter,
                    &direct_response,
                    &direct_response_active,
                );
                let chunks = if let Some((raw_chunks, responses)) = filtered {
                    for response in responses {
                        if event_tx
                            .blocking_send(ClientLoopEvent::DirectGraphicsResponse(response))
                            .is_err()
                        {
                            return;
                        }
                    }
                    raw_chunks
                        .into_iter()
                        .flat_map(|chunk| framer.push(&chunk))
                        .collect()
                } else {
                    framer.push(&scratch[..n])
                };
                if !framer.has_pending_input() {
                    pending_mode = None;
                }
                if !send_unix_input_chunks(
                    chunks,
                    &event_tx,
                    &mut pending_palette,
                    sgr_pixels,
                    last_geometry,
                ) {
                    return;
                }

                let timeout_ms = idle_flush_timeout_ms(
                    &framer,
                    host_mouse_capture_active.load(Ordering::Acquire),
                );
                if stdin_read_ready(&reader, timeout_ms) == Some(false) {
                    let had_pending = framer.has_pending_input();
                    let chunks = framer.flush_timeout();
                    let held_escape = had_pending && chunks.is_empty();
                    let sgr_pixels = pending_mode
                        .unwrap_or_else(|| host_sgr_pixels_active.load(Ordering::Acquire));
                    if !framer.has_pending_input() {
                        pending_mode = None;
                    }
                    if !send_unix_input_chunks(
                        chunks,
                        &event_tx,
                        &mut pending_palette,
                        sgr_pixels,
                        last_geometry,
                    ) || !flush_unix_palette_input(&event_tx, &mut pending_palette)
                    {
                        return;
                    }
                    if held_escape
                        && stdin_read_ready(
                            &reader,
                            crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS,
                        ) == Some(false)
                    {
                        let chunks = framer.flush_timeout();
                        if !framer.has_pending_input() {
                            pending_mode = None;
                        }
                        if !send_unix_input_chunks(
                            chunks,
                            &event_tx,
                            &mut pending_palette,
                            sgr_pixels,
                            last_geometry,
                        ) {
                            return;
                        }
                    }
                }
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }
}

fn filter_direct_input(
    bytes: &[u8],
    filter: &mut super::direct_graphics::InputFilter,
    response: &std::sync::Mutex<super::direct_graphics::ResponseMatcher>,
    active: &AtomicBool,
) -> Option<(Vec<Vec<u8>>, Vec<super::direct_graphics::Response>)> {
    if !active.load(Ordering::Acquire) && !filter.has_pending() {
        return None;
    }
    Some(
        response
            .lock()
            .map(|mut matcher| filter.push(bytes, &mut matcher))
            .unwrap_or_else(|_| (vec![bytes.to_vec()], Vec::new())),
    )
}

fn idle_flush_timeout_ms(
    framer: &crate::raw_input::RawInputByteFramer,
    host_mouse_capture_active: bool,
) -> i32 {
    if host_mouse_capture_active
        && (framer.has_pending_lone_escape() || framer.has_pending_incomplete_mouse_sequence())
    {
        crate::raw_input::MOUSE_ACTIVE_ESCAPE_SEQUENCE_FLUSH_TIMEOUT_MS
    } else {
        crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS
    }
}

fn send_unix_input_chunks(
    chunks: Vec<Vec<u8>>,
    event_tx: &mpsc::Sender<ClientLoopEvent>,
    pending_palette: &mut Vec<Vec<u8>>,
    sgr_pixels: bool,
    geometry: Option<crate::input::mouse::HostGeometry>,
) -> bool {
    for data in chunks {
        let palette_response = std::str::from_utf8(&data)
            .ok()
            .and_then(crate::terminal_theme::parse_palette_color_response)
            .is_some();
        if palette_response {
            pending_palette.push(data);
            if pending_palette.len() == 256 && !flush_unix_palette_input(event_tx, pending_palette)
            {
                return false;
            }
            continue;
        }
        let default_color_response = std::str::from_utf8(&data)
            .ok()
            .and_then(crate::terminal_theme::parse_default_color_response)
            .is_some();
        if !default_color_response && !flush_unix_palette_input(event_tx, pending_palette) {
            return false;
        }
        let Some(event) = classify_unix_input(data, sgr_pixels, geometry) else {
            continue;
        };
        if event_tx.blocking_send(event).is_err() {
            return false;
        }
    }
    true
}

fn retain_geometry(
    last: Option<crate::input::mouse::HostGeometry>,
    observed: Option<crate::input::mouse::HostGeometry>,
) -> Option<crate::input::mouse::HostGeometry> {
    observed.or(last)
}

fn classify_unix_input(
    data: Vec<u8>,
    sgr_pixels: bool,
    geometry: Option<crate::input::mouse::HostGeometry>,
) -> Option<ClientLoopEvent> {
    if sgr_pixels && crate::input::mouse::parse_report(&data).is_some() {
        return geometry.map(|geometry| ClientLoopEvent::PixelMouse(data, geometry));
    }
    Some(ClientLoopEvent::StdinInput(data))
}

fn flush_unix_palette_input(
    event_tx: &mpsc::Sender<ClientLoopEvent>,
    pending_palette: &mut Vec<Vec<u8>>,
) -> bool {
    if pending_palette.is_empty() {
        return true;
    }
    let data = std::mem::take(pending_palette).concat();
    event_tx
        .blocking_send(ClientLoopEvent::StdinInput(data))
        .is_ok()
}

fn stdin_read_ready<R: AsRawFd>(reader: &R, timeout_ms: i32) -> Option<bool> {
    poll_read_ready(reader.as_raw_fd(), timeout_ms)
}

fn poll_read_ready(fd: i32, timeout_ms: i32) -> Option<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }

    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
    }

    const POLLIN: i16 = 0x0001;

    let mut pfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };

    let result = unsafe { poll(&mut pfd as *mut PollFd, 1, timeout_ms) };
    if result < 0 {
        None
    } else {
        Some(result > 0)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_input_state() -> (
        Arc<std::sync::Mutex<super::super::direct_graphics::ResponseMatcher>>,
        Arc<AtomicBool>,
    ) {
        let matcher = Arc::new(std::sync::Mutex::new(
            super::super::direct_graphics::ResponseMatcher::default(),
        ));
        let active = matcher.lock().unwrap().active_handle();
        (matcher, active)
    }

    #[test]
    fn m831_client_timeout_requires_capture_and_mouse_prefix_state() {
        for (bytes, active_timeout) in [
            (b"".as_slice(), 10),
            (b"\x1b".as_slice(), 150),
            (b"\x1b[".as_slice(), 10),
            (b"x".as_slice(), 10),
            (b"\x1b[200~".as_slice(), 10),
            (b"\x1b[49:33;2:".as_slice(), 10),
            (b"\x1b[<".as_slice(), 150),
            (b"\x1b[<3".as_slice(), 150),
            (b"\x1b[<35;58;".as_slice(), 150),
        ] {
            let mut framer = crate::raw_input::RawInputByteFramer::default();
            framer.push(bytes);
            assert_eq!(idle_flush_timeout_ms(&framer, false), 10, "{bytes:?}");
            assert_eq!(
                idle_flush_timeout_ms(&framer, true),
                active_timeout,
                "{bytes:?}"
            );
            assert_eq!(idle_flush_timeout_ms(&framer, false), 10, "{bytes:?}");
        }

        for introducer in [b"\x1b]".as_slice(), b"\x1bP".as_slice()] {
            let mut discarding = crate::raw_input::RawInputByteFramer::default();
            assert!(discarding.push(introducer).is_empty());
            assert!(discarding.flush_timeout().is_empty());
            assert!(discarding.push(b"\x1b[<3").is_empty());
            assert!(discarding.has_pending_input());
            assert_eq!(idle_flush_timeout_ms(&discarding, false), 10);
            assert_eq!(idle_flush_timeout_ms(&discarding, true), 10);
        }
    }

    #[test]
    fn m810_mouse_poll_window_only_applies_to_active_lone_escape() {
        let capture = Arc::new(AtomicBool::new(false));
        for (bytes, active_timeout) in [
            (b"".as_slice(), 10),
            (b"\x1b".as_slice(), 150),
            (b"\x1b[".as_slice(), 10),
            (b"x".as_slice(), 10),
            (b"\x1b[200~".as_slice(), 10),
        ] {
            let mut framer = crate::raw_input::RawInputByteFramer::default();
            framer.push(bytes);
            capture.store(false, Ordering::Release);
            assert_eq!(
                idle_flush_timeout_ms(&framer, capture.load(Ordering::Acquire)),
                10
            );
            capture.store(true, Ordering::Release);
            assert_eq!(
                idle_flush_timeout_ms(&framer, capture.load(Ordering::Acquire)),
                active_timeout
            );
            capture.store(false, Ordering::Release);
            assert_eq!(
                idle_flush_timeout_ms(&framer, capture.load(Ordering::Acquire)),
                10
            );
        }
    }

    #[tokio::test]
    async fn m810_input_reader_reassembles_bytewise_mouse_from_active_host() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        struct Bytewise(UnixStream);
        impl Read for Bytewise {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let len = bytes.len().min(1);
                self.0.read(&mut bytes[..len])
            }
        }
        impl AsRawFd for Bytewise {
            fn as_raw_fd(&self) -> i32 {
                self.0.as_raw_fd()
            }
        }

        let (reader, mut host) = UnixStream::pair().unwrap();
        host.write_all(b"\x1b[<0;43;26Mx").unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let should_quit = Arc::new(AtomicBool::new(false));
        let reader_quit = should_quit.clone();
        let (matcher, active) = direct_input_state();
        let thread = std::thread::spawn(move || {
            unix_input_reader_loop(
                Bytewise(reader),
                tx,
                &reader_quit,
                false,
                false,
                Arc::new(AtomicBool::new(true)),
                Arc::new(AtomicBool::new(false)),
                matcher,
                active,
            );
        });
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            (rx.recv().await, rx.recv().await)
        })
        .await;
        should_quit.store(true, Ordering::Release);
        drop(host);
        thread.join().unwrap();

        let (mouse, key) =
            received.expect("bytewise SGR input must finish without another host write");
        assert!(
            matches!(mouse, Some(ClientLoopEvent::StdinInput(data)) if data == b"\x1b[<0;43;26M")
        );
        assert!(matches!(key, Some(ClientLoopEvent::StdinInput(data)) if data == b"x"));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn input_reader_forwards_cell_report_without_swallowing_following_keys() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (reader, mut host) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let reader_quit = Arc::clone(&should_quit);
        let (matcher, active) = direct_input_state();
        let thread = std::thread::spawn(move || {
            unix_input_reader_loop(
                reader,
                tx,
                &reader_quit,
                false,
                true,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                matcher,
                active,
            );
        });
        host.write_all(b"\x1b[6;21;10tx\x1b").unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            (rx.recv().await, rx.recv().await, rx.recv().await)
        })
        .await;
        should_quit.store(true, Ordering::Release);
        drop(host);
        thread.join().unwrap();

        let (report, key, escape) = received.expect("cell report must not consume later input");
        let Some(ClientLoopEvent::StdinInput(data)) = report else {
            panic!("expected cell size report");
        };
        assert_eq!(data, b"\x1b[6;21;10t");
        let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
        assert_eq!(
            super::super::reported_cell_size_from_events(&events),
            Some((10, 21))
        );
        assert!(matches!(key, Some(ClientLoopEvent::StdinInput(data)) if data == b"x"));
        assert!(matches!(escape, Some(ClientLoopEvent::StdinInput(data)) if data == b"\x1b"));
    }

    #[tokio::test]
    async fn input_reader_releases_escape_when_cell_size_query_is_unanswered() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (reader, mut host) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let reader_quit = Arc::clone(&should_quit);
        let (matcher, active) = direct_input_state();
        let thread = std::thread::spawn(move || {
            unix_input_reader_loop(
                reader,
                tx,
                &reader_quit,
                false,
                true,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                matcher,
                active,
            );
        });
        host.write_all(b"\x1b").unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await;
        should_quit.store(true, Ordering::Release);
        drop(host);
        thread.join().unwrap();

        assert!(matches!(
            received.expect("unanswered cell query must not hold Escape indefinitely"),
            Some(ClientLoopEvent::StdinInput(data)) if data == b"\x1b"
        ));
    }

    #[tokio::test]
    async fn input_reader_releases_escape_without_another_read_after_focus() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (reader, mut host) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let reader_quit = Arc::clone(&should_quit);
        let (matcher, active) = direct_input_state();
        let thread = std::thread::spawn(move || {
            unix_input_reader_loop(
                reader,
                tx,
                &reader_quit,
                true,
                false,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                matcher,
                active,
            );
        });
        host.write_all(b"\x1b[I\x1b").unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            (rx.recv().await, rx.recv().await)
        })
        .await;
        should_quit.store(true, Ordering::Release);
        drop(host);
        thread.join().unwrap();

        let (focus, escape) = received.expect("Escape must not wait for another host byte");
        assert!(matches!(focus, Some(ClientLoopEvent::StdinInput(data)) if data == b"\x1b[I"));
        assert!(matches!(escape, Some(ClientLoopEvent::StdinInput(data)) if data == b"\x1b"));
    }

    #[test]
    fn palette_replies_are_forwarded_as_one_input_batch() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut pending = Vec::new();
        assert!(send_unix_input_chunks(
            vec![
                b"\x1b]4;0;rgb:1111/2222/3333\x1b\\".to_vec(),
                b"\x1b]4;1;rgb:4444/5555/6666\x1b\\".to_vec(),
            ],
            &tx,
            &mut pending,
            false,
            None,
        ));
        assert!(rx.try_recv().is_err());

        assert!(flush_unix_palette_input(&tx, &mut pending));
        let ClientLoopEvent::StdinInput(data) = rx.try_recv().unwrap() else {
            panic!("expected palette input batch");
        };
        assert_eq!(
            data.windows(4)
                .filter(|window| *window == b"\x1b]4;")
                .count(),
            2
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn stdin_input_event_carries_raw_bytes() {
        let data = vec![0x1b, b'[', b'A']; // Up arrow escape sequence
        let event = ClientLoopEvent::StdinInput(data.clone());
        match event {
            ClientLoopEvent::StdinInput(d) => assert_eq!(d, data),
            _ => panic!("expected StdinInput event"),
        }
    }

    #[test]
    fn inactive_direct_input_bypasses_filter() {
        let response =
            std::sync::Mutex::new(super::super::direct_graphics::ResponseMatcher::default());
        let active = response.lock().unwrap().active_handle();
        let mut filter = super::super::direct_graphics::InputFilter::default();
        assert!(filter_direct_input(b"typed", &mut filter, &response, &active).is_none());
        assert!(!filter.has_pending());
    }

    #[test]
    fn pixel_mouse_classification_is_narrow_and_uses_read_geometry() {
        let geometry = crate::input::mouse::HostGeometry::new(80, 24, 800, 480).unwrap();
        let report = b"\x1b[<35;321;241M".to_vec();
        let Some(ClientLoopEvent::PixelMouse(data, captured)) =
            classify_unix_input(report.clone(), true, Some(geometry))
        else {
            panic!("expected dedicated pixel mouse event");
        };
        assert_eq!(data, report);
        assert_eq!(captured, geometry);
        assert!(classify_unix_input(report, true, None).is_none());

        for raw in [
            b"key".as_slice(),
            b"\x1b[200~paste\x1b[201~".as_slice(),
            b"\x1b_Gi=7;unrelated\x1b\\".as_slice(),
            b"\x1b[<35;2;3Mtail".as_slice(),
        ] {
            let Some(ClientLoopEvent::StdinInput(data)) =
                classify_unix_input(raw.to_vec(), true, Some(geometry))
            else {
                panic!("unrelated input must remain raw");
            };
            assert_eq!(data, raw);
        }
    }

    #[test]
    fn transient_geometry_failure_keeps_last_real_value() {
        let geometry = crate::input::mouse::HostGeometry::new(80, 24, 800, 480).unwrap();
        assert_eq!(retain_geometry(Some(geometry), None), Some(geometry));
    }
}
