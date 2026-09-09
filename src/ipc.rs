use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

pub(crate) type LocalListener = interprocess::local_socket::Listener;
pub(crate) type LocalStream = interprocess::local_socket::Stream;

/// Kernel-reported credentials of the peer on an accepted API connection
/// (ADR 0014).
///
/// `interprocess`' `peer_creds` is `getsockopt(SOL_SOCKET, SO_PEERCRED)` into a
/// `libc::ucred` on Linux — the same syscall a hand-rolled call would make,
/// filled in by the kernel when the peer connected and unforgeable by the
/// client. It is the only route to those credentials here: the local-socket
/// `Stream` deliberately exposes no raw fd. `None` (no pid, no euid, or the
/// option unavailable) is a refusal for every caller.
pub(crate) fn stream_peer_credentials(
    stream: &LocalStream,
) -> Option<crate::platform::PeerCredentials> {
    use interprocess::local_socket::traits::StreamCommon as _;
    let credentials = stream.peer_creds().ok()?;
    let pid = credentials.pid()?;
    if pid <= 0 {
        return None;
    }
    Some(crate::platform::PeerCredentials {
        pid: pid as u32,
        uid: credentials.euid()?,
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

    #[test]
    fn stale_socket_connect_errors_keep_unix_would_block_strict() {
        assert!(stale_socket_connect_error(io::ErrorKind::ConnectionRefused));
        assert!(stale_socket_connect_error(io::ErrorKind::NotFound));
        assert!(stale_socket_connect_error(io::ErrorKind::TimedOut));
        assert!(!stale_socket_connect_error(io::ErrorKind::WouldBlock));
    }
}
