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
) {
    unix_stdin_reader_loop(event_tx, should_quit, host_color_query_sent);
}

fn unix_stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
) {
    let stdin = io::stdin();
    unix_input_reader_loop(stdin.lock(), event_tx, should_quit, host_color_query_sent);
}

fn unix_input_reader_loop<R: Read + AsRawFd>(
    mut reader: R,
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
) {
    let mut scratch = [0u8; 4096];
    let mut framer = crate::raw_input::RawInputByteFramer::default();
    if host_color_query_sent {
        framer.host_color_query_sent();
        framer.enable_host_color_scheme_change_tracking();
        framer.enable_host_appearance_query_on_focus();
    }
    let mut pending_palette = Vec::new();

    while !should_quit.load(Ordering::Acquire) {
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => {
                if !send_unix_input_chunks(
                    framer.push(&scratch[..n]),
                    &event_tx,
                    &mut pending_palette,
                ) {
                    return;
                }

                if stdin_read_ready(&reader, 10) == Some(false) {
                    let had_pending = framer.has_pending_input();
                    let chunks = framer.flush_timeout();
                    let held_escape = had_pending && chunks.is_empty();
                    if !send_unix_input_chunks(chunks, &event_tx, &mut pending_palette)
                        || !flush_unix_palette_input(&event_tx, &mut pending_palette)
                    {
                        return;
                    }
                    if held_escape
                        && stdin_read_ready(&reader, 10) == Some(false)
                        && !send_unix_input_chunks(
                            framer.flush_timeout(),
                            &event_tx,
                            &mut pending_palette,
                        )
                    {
                        return;
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

fn send_unix_input_chunks(
    chunks: Vec<Vec<u8>>,
    event_tx: &mpsc::Sender<ClientLoopEvent>,
    pending_palette: &mut Vec<Vec<u8>>,
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
        if event_tx
            .blocking_send(ClientLoopEvent::StdinInput(data))
            .is_err()
        {
            return false;
        }
    }
    true
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
    // The stdin reader thread is hard to unit test since it reads from actual stdin.
    // Integration tests will verify the full client→server input flow.
    // Here we test the event type construction.

    use super::*;

    #[tokio::test]
    async fn input_reader_releases_escape_without_another_read_after_focus() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (reader, mut host) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let reader_quit = Arc::clone(&should_quit);
        let thread = std::thread::spawn(move || {
            unix_input_reader_loop(reader, tx, &reader_quit, true);
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
}
