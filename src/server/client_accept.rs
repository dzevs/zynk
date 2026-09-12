// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::io;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};

use interprocess::local_socket::traits::{Listener as _, Stream as _};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::ipc::LocalListener;
use crate::server::client_transport::{self, ServerEvent};

/// Accepts pending thin-client connections and starts their handshake readers.
pub(crate) fn accept_pending_client_connections(
    listener: &LocalListener,
    next_client_id: &mut u64,
    should_quit: &Arc<AtomicBool>,
    server_event_tx: &mpsc::Sender<ServerEvent>,
) -> io::Result<()> {
    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }
        match listener.accept() {
            Ok(stream) => {
                let client_id = *next_client_id;
                *next_client_id = next_client_id.saturating_add(1);

                if let Err(err) = stream.set_nonblocking(true) {
                    warn!(err = %err, "failed to set client stream nonblocking");
                    continue;
                }

                let should_quit = should_quit.clone();
                let server_event_tx = server_event_tx.clone();
                std::thread::spawn(move || {
                    if let Err(err) = client_transport::handle_client_handshake(
                        stream,
                        client_id,
                        &server_event_tx,
                        &should_quit,
                    ) {
                        debug!(client_id, err = %err, "client handshake failed");
                    }
                });
            }
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                error!(err = %err, "client listener accept failed");
                break;
            }
        }
    }

    Ok(())
}

/// Drains pending thin-client connections without starting handshakes.
///
/// During live handoff the old server must not let clients sit in the Unix
/// listener backlog waiting for a welcome frame that will never be sent.
pub(crate) fn reject_pending_client_connections(listener: &LocalListener) -> io::Result<()> {
    loop {
        match listener.accept() {
            Ok(_stream) => {}
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                error!(err = %err, "client listener reject failed");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::ListenerNonblockingMode;

    struct TestSocketPath(std::path::PathBuf);

    impl Drop for TestSocketPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn unique_test_path(name: &str) -> TestSocketPath {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        TestSocketPath(std::env::temp_dir().join(format!(
            "zynk-client-accept-{name}-{}-{nanos}.sock",
            std::process::id()
        )))
    }

    #[test]
    fn stop_preflight_leaves_pending_clients_unaccepted() {
        let path = unique_test_path("stopped");
        let listener = crate::ipc::bind_local_listener(&path.0).unwrap();
        listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .unwrap();
        let client = crate::ipc::connect_local_stream(&path.0).unwrap();
        let should_quit = Arc::new(AtomicBool::new(true));
        assert!(should_quit.load(Ordering::Acquire));
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let mut next_client_id = 17;

        accept_pending_client_connections(
            &listener,
            &mut next_client_id,
            &should_quit,
            &server_event_tx,
        )
        .unwrap();

        drop(client);
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(next_client_id, 17, "stopped server accepted a client");
        assert!(server_event_rx.try_recv().is_err());
    }
}
