// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
//! Linux remote-host side of the SSH stdio bridge.

use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use interprocess::TryClone as _;

pub(crate) fn run_remote_client_bridge() -> io::Result<()> {
    ensure_remote_server_running()?;

    let socket_path = crate::server::socket_paths::client_socket_path();
    let stream = crate::ipc::connect_local_stream(&socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to remote Zynk client socket {}: {err}",
                socket_path.display()
            ),
        )
    })?;

    let mut stdout = io::stdout().lock();
    let mut socket_to_stdout = stream.try_clone()?;
    let mut stdin_to_socket = stream;

    let _upload = thread::spawn(move || {
        let mut stdin = io::stdin();
        let _ = copy_flush(&mut stdin, &mut stdin_to_socket);
        let _ = crate::ipc::shutdown_local_stream_write(&stdin_to_socket);
    });

    copy_flush(&mut socket_to_stdout, &mut stdout).map(|_| ())
}

fn ensure_remote_server_running() -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if crate::server::autodetect::is_server_listening() {
        let status = crate::api::read_runtime_status_at(
            &crate::api::socket_path(),
            Duration::from_millis(500),
        )?
        .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
        if status.protocol == Some(crate::protocol::PROTOCOL_VERSION) {
            return Ok(());
        }
        return Err(io::Error::other(
            "remote zynk server must restart before this bridge can attach; rerun `zynk --remote` from an interactive terminal to approve stopping it",
        ));
    }

    // This helper itself was executed through a custody-verified open file.
    // Starting through /proc/self/exe avoids reopening a concurrently replaced
    // install pathname between verification and daemon launch.
    crate::server::autodetect::spawn_server_daemon_at(PathBuf::from("/proc/self/exe"))?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))
}

fn copy_flush<R: io::Read, W: io::Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        writer.write_all(&buffer[..read])?;
        writer.flush()?;
        total += read as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b3_host_bridge_copy_preserves_bytes_and_eof() {
        let input = b"host bridge\0payload";
        let mut reader = io::Cursor::new(input);
        let mut output = Vec::new();

        let copied = copy_flush(&mut reader, &mut output).unwrap();

        assert_eq!(copied, input.len() as u64);
        assert_eq!(output, input);
    }
}
