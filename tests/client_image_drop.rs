//! Real interactive client input, with a private peer instead of SSH or a server.

mod support;

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::de::DeserializeOwned;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\noriginal image bytes";
const WAIT: Duration = Duration::from_secs(10);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Self(
            support::test_root().join(format!("zynk-image-drop-{}-{stamp}", std::process::id())),
        );
        fs::create_dir_all(root.0.join("runtime")).unwrap();
        fs::create_dir_all(root.0.join("config")).unwrap();
        fs::create_dir_all(root.0.join("home")).unwrap();
        fs::write(root.0.join("config/config.toml"), "onboarding = false\n").unwrap();
        root
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        support::cleanup_test_base(&self.0);
    }
}

struct ClientProcess {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
}

impl Drop for ClientProcess {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            let reaped = match self.child.try_wait() {
                Ok(Some(_)) => true,
                Err(error) => error.raw_os_error() == Some(libc::ECHILD),
                Ok(None) => false,
            };
            if reaped {
                support::unregister_spawned_zynk_pid(pid);
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Keep the PID registered for the support cleanup hook on an abnormal exit.
        assert!(
            thread::panicking(),
            "owned image-drop client was not reaped"
        );
    }
}

struct ClientPeer {
    process: ClientProcess,
    _root: TestRoot,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    stream: UnixStream,
}

#[derive(Debug, PartialEq, Eq)]
enum InputMessage {
    Raw(Vec<u8>),
    Image(String, Vec<u8>),
}

fn decode_fields<T: DeserializeOwned>(bytes: &[u8]) -> T {
    let (value, consumed) = bincode::serde::decode_from_slice(bytes, bincode::config::standard())
        .expect("decode real client message fields");
    assert_eq!(consumed, bytes.len(), "no ignored trailing wire fields");
    value
}

impl ClientPeer {
    fn start(root: TestRoot, remote: bool) -> Self {
        let socket = root.0.join("runtime/c.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let fd = pair.master.as_raw_fd().unwrap();
        // SAFETY: this fixture owns the live master; preserve its existing flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_ne!(flags, -1);
        // SAFETY: O_NONBLOCK applies to our master and its cloned reader, not the slave.
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            -1
        );
        // SAFETY: inspect the owned descriptor before any read could block.
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_NONBLOCK,
            0
        );
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_zynk"));
        command.arg("client");
        command.env_clear();
        command.cwd(&root.0);
        command.env("HOME", root.0.join("home"));
        command.env("PATH", root.0.join("no-providers"));
        command.env("TERM", "xterm-256color");
        command.env("SHELL", "/bin/sh");
        command.env("ZYNK_DISABLE_SOUND", "1");
        command.env("ZYNK_CLIENT_SOCKET_PATH", &socket);
        command.env("ZYNK_CONFIG_PATH", root.0.join("config/config.toml"));
        command.env("ZYNK_HOME", root.0.join("home"));
        command.env("ZYNK_SQLITE_HOME", root.0.join("home/sqlite"));
        for (key, directory) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_RUNTIME_DIR", "runtime"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_DATA_HOME", "data"),
        ] {
            command.env(key, root.0.join(directory));
        }
        if remote {
            command.env("ZYNK_REMOTE_KEYBINDINGS", "server");
        }
        let child = pair.slave.spawn_command(command).unwrap();
        support::register_spawned_zynk_pid(child.process_id());
        let mut process = ClientProcess {
            child,
            _master: pair.master,
        };
        drop(pair.slave);
        let started = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(started.elapsed() < WAIT, "client handshake deadline");
                    if let Some(status) = process.child.try_wait().unwrap() {
                        let mut output = [0; 8192];
                        let n = reader.read(&mut output).unwrap_or(0);
                        panic!(
                            "client exited before Hello ({status:?}): {}",
                            String::from_utf8_lossy(&output[..n])
                        );
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept client: {error}"),
            }
        };
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.set_write_timeout(Some(WAIT)).unwrap();
        let (variant, hello) = support::read_server_message(&mut stream).unwrap();
        assert_eq!(variant, 0, "first client message is Hello");
        let (protocol, _) = support::decode_varint_u32(&hello, 0).unwrap();
        let welcome = bincode::serde::encode_to_vec(
            (0_u32, protocol, 0_u32, None::<String>),
            bincode::config::standard(),
        )
        .unwrap();
        stream.write_all(&support::frame_message(&welcome)).unwrap();
        let mut peer = Self {
            process,
            _root: root,
            reader,
            writer,
            stream,
        };
        peer.await_terminal_setup();
        peer.send(b"q");
        assert_eq!(
            peer.next_input(),
            InputMessage::Raw(b"q".to_vec()),
            "real loop readiness"
        );
        peer
    }

    fn await_terminal_setup(&mut self) {
        let started = Instant::now();
        let mut output = Vec::new();
        let mut bytes = [0; 2048];
        loop {
            assert!(
                started.elapsed() < WAIT,
                "raw-mode setup deadline: {output:?}"
            );
            match self.reader.read(&mut bytes) {
                Ok(0) => panic!("client closed before terminal setup: {output:?}"),
                Ok(n) => {
                    output.extend_from_slice(&bytes[..n]);
                    assert!(output.len() <= 64 * 1024, "bounded setup output");
                    if output
                        .windows(b"\x1b[?2004h".len())
                        .any(|w| w == b"\x1b[?2004h")
                    {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("read terminal setup: {error}"),
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn next_input(&mut self) -> InputMessage {
        for _ in 0..16 {
            let (variant, bytes) = support::read_server_message(&mut self.stream).unwrap();
            match variant {
                1 => return InputMessage::Raw(decode_fields(&bytes)),
                2 => {
                    let (extension, data) = decode_fields(&bytes);
                    return InputMessage::Image(extension, data);
                }
                3 => continue, // Initial geometry may be reported independently of input.
                _ => panic!("unexpected client message {variant}: {bytes:?}"),
            }
        }
        panic!("too many resizes without input");
    }

    fn finish(mut self) {
        let shutdown =
            bincode::serde::encode_to_vec((4_u32, Some("detached")), bincode::config::standard())
                .unwrap();
        self.stream
            .write_all(&support::frame_message(&shutdown))
            .unwrap();
        let started = Instant::now();
        loop {
            if let Some(status) = self.process.child.try_wait().unwrap() {
                assert!(status.success(), "client exit: {status:?}");
                return;
            }
            assert!(
                started.elapsed() < WAIT,
                "client did not exit after shutdown"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn bracketed_path(path: &Path) -> Vec<u8> {
    format!("\x1b[200~{}\x1b[201~", path.display()).into_bytes()
}

#[test]
fn remote_image_drop_real_client_sends_image_without_duplicate_path() {
    let root = TestRoot::new();
    let path = root.0.join("image.PNG");
    fs::write(&path, PNG).unwrap();
    let mut peer = ClientPeer::start(root, true);
    peer.send(&bracketed_path(&path));
    assert_eq!(
        peer.next_input(),
        InputMessage::Image("png".into(), PNG.to_vec())
    );
    peer.send(b"k");
    assert_eq!(
        peer.next_input(),
        InputMessage::Raw(b"k".to_vec()),
        "no duplicate path"
    );
    peer.finish();
}

#[test]
fn local_image_drop_real_client_preserves_bracketed_path() {
    let root = TestRoot::new();
    let path = root.0.join("local.png");
    fs::write(&path, PNG).unwrap();
    let input = bracketed_path(&path);
    let mut peer = ClientPeer::start(root, false);
    peer.send(&input);
    assert_eq!(peer.next_input(), InputMessage::Raw(input));
    peer.finish();
}

#[test]
fn remote_image_drop_real_client_preserves_invalid_and_missing_envelopes() {
    let root = TestRoot::new();
    let invalid = root.0.join("invalid.png");
    let missing = root.0.join("missing.png");
    fs::write(&invalid, b"not an image").unwrap();
    let mut peer = ClientPeer::start(root, true);
    for path in [invalid, missing] {
        let input = bracketed_path(&path);
        peer.send(&input);
        assert_eq!(
            peer.next_input(),
            InputMessage::Raw(input),
            "fallback keeps both delimiters"
        );
    }
    peer.finish();
}

#[test]
fn remote_image_drop_real_client_accepts_quoted_and_escaped_image_paths() {
    let root = TestRoot::new();
    let jpeg = b"\xff\xd8\xffjpeg payload";
    let quoted = root.0.join("quoted image.JPEG");
    let escaped = root.0.join("escaped image.png");
    fs::write(&quoted, jpeg).unwrap();
    fs::write(&escaped, PNG).unwrap();
    let mut peer = ClientPeer::start(root, true);
    for (path_text, extension, bytes) in [
        (
            format!("'{}'\r\n", quoted.display()),
            "jpg",
            jpeg.as_slice(),
        ),
        (escaped.to_str().unwrap().replace(' ', "\\ "), "png", PNG),
    ] {
        peer.send(format!("\x1b[200~{path_text}\x1b[201~").as_bytes());
        assert_eq!(
            peer.next_input(),
            InputMessage::Image(extension.into(), bytes.to_vec())
        );
    }
    peer.finish();
}
