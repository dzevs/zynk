use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use interprocess::local_socket::traits::Stream as _;

pub(crate) type LocalListener = interprocess::local_socket::Listener;
pub(crate) type LocalStream = interprocess::local_socket::Stream;

pub(crate) enum LocalStreamRead {
    Data,
    Pending,
    Closed,
}

/// Kernel-reported credentials of the peer on an accepted API connection
/// (ADR 0014).
///
/// `interprocess`' `peer_creds` is `getsockopt(SOL_SOCKET, SO_PEERCRED)` into a
/// `libc::ucred` on Linux — the same syscall a hand-rolled call would make,
/// filled in by the kernel when the peer connected and unforgeable by the
/// client. It is the only route to those credentials here: the local-socket
/// `Stream` deliberately exposes no raw fd. `None` (no pid, no euid, or the
/// option unavailable) is a refusal for every caller.
///
/// The peer's start time is read here, at accept, so the connection is bound to
/// a PROCESS rather than to a pid the kernel may hand to someone else before the
/// request is checked (ARCH-E8-ADR14-PID-REUSE-001). A start time that cannot be
/// read means the peer is already gone, and travels as `None` so the pane-tree
/// check refuses it by name instead of silently passing a bare pid.
pub(crate) fn stream_peer_credentials(
    stream: &LocalStream,
) -> Option<crate::platform::PeerCredentials> {
    use interprocess::local_socket::traits::StreamCommon as _;
    let credentials = stream.peer_creds().ok()?;
    let pid = credentials.pid()?;
    if pid <= 0 {
        return None;
    }
    let pid = pid as u32;
    Some(crate::platform::PeerCredentials {
        pid,
        uid: credentials.euid()?,
        start_time: crate::platform::process_start_time(pid),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SocketFileIdentity {
    dev: u64,
    ino: u64,
}

pub(crate) fn connect_local_stream(path: &Path) -> io::Result<LocalStream> {
    use interprocess::local_socket::{prelude::*, GenericFilePath};

    let name = path.to_fs_name::<GenericFilePath>()?;
    LocalStream::connect(name)
}

pub(crate) fn bind_local_listener(path: &Path) -> io::Result<LocalListener> {
    use interprocess::local_socket::{prelude::*, GenericFilePath, ListenerOptions};

    let name = path.to_fs_name::<GenericFilePath>()?;
    ListenerOptions::new()
        .name(name)
        .reclaim_name(false)
        .create_sync()
}

pub(crate) fn prepare_socket_path(
    path: &Path,
    busy_message: impl FnOnce(&Path) -> String,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if !path.exists() {
        return Ok(());
    }

    match connect_local_stream(path) {
        Ok(_) => {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, busy_message(path)));
        }
        Err(err) if stale_socket_connect_error(err.kind()) => {}
        Err(err) => return Err(err),
    }

    if let Err(err) = fs::remove_file(path) {
        if err.kind() != io::ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

fn stale_socket_connect_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound | io::ErrorKind::TimedOut
    )
}

/// Probe a one-request-per-connection API stream, consuming any unexpected extra
/// byte as a reason to close. This is not a general-purpose, non-consuming peek.
pub(crate) fn local_stream_peer_closed(stream: &mut LocalStream) -> io::Result<bool> {
    stream.set_nonblocking(true)?;
    let mut probe = [0u8; 1];
    let status = match stream.read(&mut probe) {
        Ok(0) => Ok(true),
        Ok(_) => Ok(true),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(err) if is_connection_closed_error(&err) => Ok(true),
        Err(err) => Err(err),
    };
    stream.set_nonblocking(false)?;
    status
}

pub(crate) fn set_local_stream_polling(stream: &mut LocalStream, enabled: bool) -> io::Result<()> {
    stream.set_nonblocking(enabled)
}

pub(crate) fn poll_local_stream_read(
    stream: &mut LocalStream,
    buf: &mut [u8],
) -> io::Result<LocalStreamRead> {
    match stream.read(buf) {
        Ok(0) => Ok(LocalStreamRead::Closed),
        Ok(_) => Ok(LocalStreamRead::Data),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(LocalStreamRead::Pending),
        Err(err) => Err(err),
    }
}

pub(crate) fn is_connection_closed_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::WriteZero
    )
}

pub(crate) fn socket_file_identity(path: &Path) -> io::Result<SocketFileIdentity> {
    let metadata = fs::metadata(path)?;
    Ok(SocketFileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

pub(crate) fn remove_socket_file_if_owned(
    path: &Path,
    identity: &SocketFileIdentity,
) -> io::Result<()> {
    let current = match socket_file_identity(path) {
        Ok(current) => current,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    if current != *identity {
        return Ok(());
    }

    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub(crate) fn restrict_socket_permissions(path: &Path, mode: u32) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m827_is_nonblocking(stream: &LocalStream) -> bool {
        use std::os::fd::AsRawFd;
        let LocalStream::UdSocket(socket) = stream;
        // SAFETY: the borrowed socket owns this live descriptor throughout the query.
        let flags = unsafe { libc::fcntl(socket.inner().as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "F_GETFL failed: {}", io::Error::last_os_error());
        flags & libc::O_NONBLOCK != 0
    }

    #[test]
    fn m827_polling_mode_and_read_outcomes() {
        use std::io::Write;
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut server = LocalStream::UdSocket(server.into());
        set_local_stream_polling(&mut server, true).unwrap();
        assert!(m827_is_nonblocking(&server));
        let mut byte = [0; 1];
        assert!(matches!(
            poll_local_stream_read(&mut server, &mut byte).unwrap(),
            LocalStreamRead::Pending
        ));
        client.write_all(b"xy").unwrap();
        for expected in b"xy" {
            assert!(matches!(
                poll_local_stream_read(&mut server, &mut byte).unwrap(),
                LocalStreamRead::Data
            ));
            assert_eq!(byte[0], *expected);
        }
        assert!(matches!(
            poll_local_stream_read(&mut server, &mut byte).unwrap(),
            LocalStreamRead::Pending
        ));
        drop(client);
        assert!(matches!(
            poll_local_stream_read(&mut server, &mut byte).unwrap(),
            LocalStreamRead::Closed
        ));
        set_local_stream_polling(&mut server, false).unwrap();
        assert!(!m827_is_nonblocking(&server));
    }

    #[test]
    fn m827_polling_preserves_non_pending_read_error() {
        use std::os::fd::{FromRawFd, OwnedFd};
        // SAFETY: socket returns a new descriptor, which is checked and owned below.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(fd >= 0, "socket failed: {}", io::Error::last_os_error());
        // SAFETY: this successful socket descriptor has no other owner.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut stream = LocalStream::UdSocket(std::os::unix::net::UnixStream::from(owned).into());
        set_local_stream_polling(&mut stream, true).unwrap();
        assert!(m827_is_nonblocking(&stream));
        let error = poll_local_stream_read(&mut stream, &mut [0; 1])
            .err()
            .expect("an unconnected stream must preserve its read error");
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
        assert!(m827_is_nonblocking(&stream));
    }

    #[test]
    fn m811_api_peer_probe_distinguishes_idle_extra_bytes_and_closed() {
        use interprocess::local_socket::traits::Listener as _;
        use std::io::Write;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("zynk-probe-{}-{nanos}", std::process::id()));
        let listener = bind_local_listener(&path).unwrap();
        let mut client = connect_local_stream(&path).unwrap();
        let mut server = listener.accept().unwrap();
        assert!(!local_stream_peer_closed(&mut server).unwrap());
        client.write_all(b"x").unwrap();
        client.flush().unwrap();
        assert!(local_stream_peer_closed(&mut server).unwrap());
        assert!(
            !local_stream_peer_closed(&mut server).unwrap(),
            "extra byte was consumed"
        );
        drop(client);
        assert!(local_stream_peer_closed(&mut server).unwrap());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn m811_connection_closed_error_kinds_do_not_include_idle_or_timeout() {
        for kind in [
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::NotConnected,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::WriteZero,
        ] {
            assert!(
                is_connection_closed_error(&io::Error::from(kind)),
                "{kind:?}"
            );
        }
        for kind in [
            io::ErrorKind::WouldBlock,
            io::ErrorKind::Interrupted,
            io::ErrorKind::TimedOut,
            io::ErrorKind::PermissionDenied,
        ] {
            assert!(
                !is_connection_closed_error(&io::Error::from(kind)),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn stale_socket_connect_errors_keep_unix_would_block_strict() {
        assert!(stale_socket_connect_error(io::ErrorKind::ConnectionRefused));
        assert!(stale_socket_connect_error(io::ErrorKind::NotFound));
        assert!(stale_socket_connect_error(io::ErrorKind::TimedOut));
        assert!(!stale_socket_connect_error(io::ErrorKind::WouldBlock));
    }
}
