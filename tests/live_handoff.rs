mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, client_handshake, register_runtime_dir, register_spawned_zynk_pid,
    send_input, unregister_spawned_zynk_pid, wait_for_disconnect, wait_for_socket,
};

struct SpawnedZynk {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

struct RequestError {
    retryable: bool,
    message: String,
}

impl Drop for SpawnedZynk {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        unregister_spawned_zynk_pid(pid);
    }
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_test_dir() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/hlh-{}-{n}", std::process::id()))
}

fn spawn_server(config_home: &Path, runtime_dir: &Path, api_socket: &Path) -> SpawnedZynk {
    spawn_server_with_env(config_home, runtime_dir, api_socket, &[])
}

fn spawn_server_with_env(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    extra_env: &[(&str, &str)],
) -> SpawnedZynk {
    fs::create_dir_all(config_home.join("zynk-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("zynk-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_zynk"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    // #117 DB isolation: pin the global DB to a per-test sqlite home + scrub ZYNK_HOME.
    cmd.env("ZYNK_SQLITE_HOME", config_home.join("sqlite"));
    cmd.env_remove("ZYNK_HOME");
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("ZYNK_SOCKET_PATH", api_socket);
    cmd.env(
        "ZYNK_CLIENT_SOCKET_PATH",
        runtime_dir.join("zynk-client.sock"),
    );
    cmd.env("SHELL", "/bin/sh");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_zynk_pid(child.process_id());
    SpawnedZynk {
        _master: pair.master,
        child,
    }
}

fn spawn_named_session_server(
    config_home: &Path,
    runtime_dir: &Path,
    session_name: &str,
) -> SpawnedZynk {
    fs::create_dir_all(config_home.join("zynk-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("zynk-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_zynk"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    // #117 DB isolation: pin the global DB to a per-test sqlite home + scrub ZYNK_HOME.
    cmd.env("ZYNK_SQLITE_HOME", config_home.join("sqlite"));
    cmd.env_remove("ZYNK_HOME");
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("ZYNK_SESSION", session_name);
    cmd.env_remove("ZYNK_SOCKET_PATH");
    cmd.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_zynk_pid(child.process_id());
    SpawnedZynk {
        _master: pair.master,
        child,
    }
}

fn spawn_default_session_server(config_home: &Path, runtime_dir: &Path) -> SpawnedZynk {
    fs::create_dir_all(config_home.join("zynk-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("zynk-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_zynk"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    // #117 DB isolation: pin the global DB to a per-test sqlite home + scrub ZYNK_HOME.
    cmd.env("ZYNK_SQLITE_HOME", config_home.join("sqlite"));
    cmd.env_remove("ZYNK_HOME");
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env_remove("ZYNK_SESSION");
    cmd.env_remove("ZYNK_SOCKET_PATH");
    cmd.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_zynk_pid(child.process_id());
    SpawnedZynk {
        _master: pair.master,
        child,
    }
}

fn try_request(
    socket_path: &Path,
    request: serde_json::Value,
) -> Result<serde_json::Value, RequestError> {
    let mut stream = UnixStream::connect(socket_path).map_err(|err| RequestError {
        retryable: true,
        message: format!("connect {}: {err}", socket_path.display()),
    })?;
    let request_text = request.to_string();
    stream
        .write_all(request_text.as_bytes())
        .map_err(|err| RequestError {
            retryable: true,
            message: format!("write request to {}: {err}", socket_path.display()),
        })?;
    stream.write_all(b"\n").map_err(|err| RequestError {
        retryable: true,
        message: format!("write newline to {}: {err}", socket_path.display()),
    })?;
    stream.flush().map_err(|err| RequestError {
        retryable: true,
        message: format!("flush request to {}: {err}", socket_path.display()),
    })?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|err| RequestError {
            retryable: true,
            message: format!("read response from {}: {err}", socket_path.display()),
        })?;
    if line.is_empty() {
        return Err(RequestError {
            retryable: true,
            message: format!(
                "empty response from {} for request {request_text}",
                socket_path.display()
            ),
        });
    }
    serde_json::from_str(&line).map_err(|err| RequestError {
        retryable: false,
        message: format!(
            "parse response from {} for request {request_text}: {err}; response was {line:?}",
            socket_path.display()
        ),
    })
}

fn request(socket_path: &Path, request: serde_json::Value) -> serde_json::Value {
    try_request(socket_path, request).unwrap_or_else(|err| panic!("{}", err.message))
}

fn assert_ok(response: serde_json::Value) {
    assert!(
        response.get("result").is_some(),
        "api request failed: {response}"
    );
}

fn wait_for_api(socket_path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match try_request(
            socket_path,
            serde_json::json!({"id":"test:ping","method":"ping","params":{}}),
        ) {
            Ok(response) if response.get("result").is_some() => return,
            Ok(response) => panic!("api ping returned non-success response: {response}"),
            Err(err) if !err.retryable => panic!("{}", err.message),
            Err(err) => {
                last_error = err.message;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "api did not become ready at {}; last error: {last_error}",
        socket_path.display()
    );
}

fn wait_for_output(socket_path: &Path, pane_id: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_text = String::new();
    let mut last_response = serde_json::Value::Null;
    while Instant::now() < deadline {
        let response = request(
            socket_path,
            serde_json::json!({
                "id": "test:pane:read",
                "method": "pane.read",
                "params": {
                    "pane_id": pane_id,
                    "source": "visible",
                    "lines": 20,
                    "format": "text",
                    "strip_ansi": true
                }
            }),
        );
        last_response = response.clone();
        let text = response["result"]["read"]["text"]
            .as_str()
            .unwrap_or_default();
        last_text = text.to_string();
        if text.contains(needle) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "pane output did not contain {needle:?}; last text was {last_text:?}; last response was {last_response}"
    );
}

fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last_text = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(path) {
            last_text = text;
            if last_text.contains(needle) {
                return last_text;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "{} did not contain {needle:?}; last text was {last_text:?}",
        path.display()
    );
}

fn server_ptmx_fd_count(pid: u32) -> usize {
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|target| target == Path::new("/dev/ptmx"))
        .count()
}

fn wait_for_server_ptmx_fd_count(pid: u32, expected: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_count = 0;
    while Instant::now() < deadline {
        last_count = server_ptmx_fd_count(pid);
        if last_count == expected {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("server pid {pid} had {last_count} /dev/ptmx fds; expected {expected}");
}

fn wait_for_replacement_server_pid(runtime_dir: &Path, old_pid: u32, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    let mut last_pids = Vec::new();
    while Instant::now() < deadline {
        last_pids = support::zynk_server_pids_for_runtime_dir(runtime_dir).unwrap_or_default();
        if let Some(pid) = last_pids.iter().copied().find(|pid| *pid != old_pid) {
            return pid;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "replacement server for {} did not appear; last pids: {:?}",
        runtime_dir.display(),
        last_pids
    );
}

fn unused_local_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_http_contains(port: u16, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last_response = String::new();
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ =
                stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            last_response = response;
            if last_response.contains(needle) {
                return last_response;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "http server on port {port} did not return {needle:?}; last response was {last_response:?}"
    );
}

#[test]
fn live_server_holds_one_pty_master_fd_per_pane() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let server_pid = spawned
        .child
        .process_id()
        .expect("test server should expose pid");
    wait_for_server_ptmx_fd_count(server_pid, 0, Duration::from_secs(5));

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_server_ptmx_fd_count(server_pid, 1, Duration::from_secs(5));

    let second = request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split-second",
            "method": "pane.split",
            "params": {
                "target_pane_id": pane_id,
                "direction": "right",
                "focus": true
            }
        }),
    );
    assert_ok(second.clone());
    let second_pane_id = second["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_server_ptmx_fd_count(server_pid, 2, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split-third",
            "method": "pane.split",
            "params": {
                "target_pane_id": second_pane_id,
                "direction": "down",
                "focus": true
            }
        }),
    ));
    wait_for_server_ptmx_fd_count(server_pid, 3, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    let replacement_pid =
        wait_for_replacement_server_pid(&runtime_dir, server_pid, Duration::from_secs(10));
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_server_ptmx_fd_count(replacement_pid, 3, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_named_session_socket_paths() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let session_dir = config_home.join("zynk-dev/sessions/work");
    let api_socket = session_dir.join("zynk.sock");
    let client_socket = session_dir.join("zynk-client.sock");

    let spawned = spawn_named_session_server(&config_home, &runtime_dir, "work");
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert!(
        !config_home.join("zynk-dev/zynk.sock").exists(),
        "named handoff unexpectedly bound the default session API socket"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_pane_process_io() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");
    let marker = base.join("child.pid");
    let second_marker = base.join("second-child.pid");
    let hup_marker = base.join("hup");
    let second_hup_marker = base.join("second-hup");
    let received_marker = base.join("received");
    let second_received_marker = base.join("second-received");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split = request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split",
            "method": "pane.split",
            "params": {
                "target_pane_id": pane_id,
                "direction": "right",
                "focus": false
            }
        }),
    );
    assert_ok(split.clone());
    let second_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let command = format!(
        "sh -c 'echo READY $$ > {}; trap \"echo HUP >> {}\" HUP; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        hup_marker.display(),
        received_marker.display()
    );
    let second_command = format!(
        "sh -c 'echo SECOND_READY $$ > {}; trap \"echo HUP >> {}\" HUP; while read line; do echo second:$line; echo second:$line >> {}; done'",
        second_marker.display(),
        second_hup_marker.display(),
        second_received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:second-pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": second_pane_id, "text": second_command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    support::wait_for_file(&second_marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();
    let second_pid_text = fs::read_to_string(&second_marker).unwrap();
    let second_child_pid: u32 = second_pid_text
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);
    assert_eq!(unsafe { libc::kill(second_child_pid as libc::pid_t, 0) }, 0);

    let protocol = request(
        &api_socket,
        serde_json::json!({"id":"test:protocol","method":"ping","params":{}}),
    )["result"]["protocol"]
        .as_u64()
        .unwrap() as u32;
    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_protocol, error) = client_handshake(&mut client_stream, protocol, 80, 24).unwrap();
    assert_eq!(server_protocol, protocol);
    assert!(error.is_none(), "client handshake failed: {error:?}");

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:before-log",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "before_replay", "keys": ["Enter"]}
        }),
    ));
    wait_for_output(&api_socket, &pane_id, "got:before_replay");

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    assert!(
        wait_for_disconnect(&mut client_stream, Duration::from_secs(5)).unwrap(),
        "connected clients should disconnect during live handoff"
    );
    thread::sleep(Duration::from_millis(300));
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);
    assert_eq!(unsafe { libc::kill(second_child_pid as libc::pid_t, 0) }, 0);
    assert!(
        !hup_marker.exists(),
        "pane process received HUP during handoff"
    );
    assert!(
        !second_hup_marker.exists(),
        "second pane process received HUP during handoff"
    );
    wait_for_output(&api_socket, &pane_id, "got:before_replay");

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "after-handoff", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        "got:after-handoff",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &pane_id, "got:after-handoff");
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:second-pane:send",
            "method": "pane.send_input",
            "params": {"pane_id": second_pane_id, "text": "after-handoff-second", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &second_received_marker,
        "second:after-handoff-second",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &second_pane_id, "second:after-handoff-sec");

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    let _ = client_socket;
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_keyboard_protocol_for_client_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");
    let script = base.join("read-raw.py");
    let ready_marker = base.join("keyboard-ready");
    let received_marker = base.join("keyboard-received");

    fs::create_dir_all(&base).unwrap();
    fs::write(
        &script,
        format!(
            r#"import os
import pathlib
import select
import sys
import tty

sys.stdout.buffer.write(b"\x1b[>5u")
sys.stdout.flush()
pathlib.Path({ready:?}).write_text("ready")
tty.setraw(sys.stdin.fileno())
ready_fds, _, _ = select.select([sys.stdin.fileno()], [], [], 5)
data = os.read(sys.stdin.fileno(), 32) if ready_fds else b""
pathlib.Path({received:?}).write_text(data.hex())
"#,
            ready = ready_marker.display().to_string(),
            received = received_marker.display().to_string()
        ),
    )
    .unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("python3 {}", script.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&ready_marker, Duration::from_secs(5));

    let protocol = request(
        &api_socket,
        serde_json::json!({"id":"test:protocol","method":"ping","params":{}}),
    )["result"]["protocol"]
        .as_u64()
        .unwrap() as u32;
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_protocol, error) = client_handshake(&mut client_stream, protocol, 80, 24).unwrap();
    assert_eq!(server_protocol, protocol);
    assert!(error.is_none(), "client handshake failed: {error:?}");
    send_input(&mut client_stream, b"\x1b[13;2u").unwrap();

    wait_for_file_contains(&received_marker, "1b5b31333b3275", Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_modify_other_keys_for_client_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");
    let script = base.join("read-raw.py");
    let ready_marker = base.join("modify-ready");
    let received_marker = base.join("modify-received");

    fs::create_dir_all(&base).unwrap();
    fs::write(
        &script,
        format!(
            r#"import os
import pathlib
import select
import sys
import tty

sys.stdout.buffer.write(b"\x1b[>4;2m")
sys.stdout.flush()
pathlib.Path({ready:?}).write_text("ready")
tty.setraw(sys.stdin.fileno())
ready_fds, _, _ = select.select([sys.stdin.fileno()], [], [], 5)
data = os.read(sys.stdin.fileno(), 32) if ready_fds else b""
pathlib.Path({received:?}).write_text(data.hex())
"#,
            ready = ready_marker.display().to_string(),
            received = received_marker.display().to_string()
        ),
    )
    .unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("python3 {}", script.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&ready_marker, Duration::from_secs(5));

    let protocol = request(
        &api_socket,
        serde_json::json!({"id":"test:protocol","method":"ping","params":{}}),
    )["result"]["protocol"]
        .as_u64()
        .unwrap() as u32;
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_protocol, error) = client_handshake(&mut client_stream, protocol, 80, 24).unwrap();
    assert_eq!(server_protocol, protocol);
    assert!(error.is_none(), "client handshake failed: {error:?}");
    send_input(&mut client_stream, b"\x1b[13;2u").unwrap();

    wait_for_file_contains(
        &received_marker,
        "1b5b32373b323b31337e",
        Duration::from_secs(5),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_accepts_canonical_pane_id_from_child_env() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let pane_id_marker = base.join("pane-id");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:print-id",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("printf '%s' \"$ZYNK_PANE_ID\" > {}", pane_id_marker.display()), "keys": ["Enter"]}
        }),
    ));
    let old_pane_id = wait_for_file_contains(&pane_id_marker, &pane_id, Duration::from_secs(5));
    assert!(
        old_pane_id == pane_id,
        "unexpected pane id from env: {old_pane_id:?}"
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:old-pane-report",
            "method": "pane.report_agent",
            "params": {
                "pane_id": old_pane_id,
                "source": "handoff-test",
                "agent": "pi",
                "state": "working"
            }
        }),
    ));
    let agents = request(
        &api_socket,
        serde_json::json!({"id":"test:agent-list","method":"agent.list","params":{}}),
    );
    let found = agents["result"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|agent| {
            agent["agent"].as_str() == Some("pi")
                && agent["agent_status"].as_str() == Some("working")
        });
    assert!(
        found,
        "old pane id report did not update restored pane: {agents}"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_agent_started_pane_after_agent_exits() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let started_marker = base.join("agent-started");
    let exited_marker = base.join("agent-exited");
    let shell_marker = base.join("shell-after-agent");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let command = format!(
        "echo started > {}; sleep 1; echo exited > {}",
        started_marker.display(),
        exited_marker.display()
    );
    let started = request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent-start",
            "method": "agent.start",
            "params": {
                "name": "handoff-agent",
                "cwd": "/tmp",
                "focus": true,
                "argv": ["/bin/sh", "-c", command]
            }
        }),
    );
    assert_ok(started.clone());
    let pane_id = started["result"]["agent"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    support::wait_for_file(&started_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    support::wait_for_file(&exited_marker, Duration::from_secs(5));
    thread::sleep(Duration::from_millis(300));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:shell-after-agent",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("echo alive > {}", shell_marker.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&shell_marker, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_shell_pane_after_foreground_process_exits() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let started_marker = base.join("foreground-started");
    let exited_marker = base.join("foreground-exited");
    let shell_marker = base.join("shell-after-foreground");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo started > {}; sleep 1; echo exited > {}'",
        started_marker.display(),
        exited_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run-foreground",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&started_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    support::wait_for_file(&exited_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:shell-after-foreground",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("echo alive > {}", shell_marker.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&shell_marker, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_python_http_server() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");
    let web_root = base.join("web");
    fs::create_dir_all(&web_root).unwrap();
    fs::write(
        web_root.join("index.html"),
        "hello-from-python-before-and-after",
    )
    .unwrap();
    let port = unused_local_port();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": web_root, "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run-python",
            "method": "pane.send_input",
            "params": {
                "pane_id": pane_id,
                "text": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                "keys": ["Enter"]
            }
        }),
    ));
    wait_for_http_contains(
        port,
        "hello-from-python-before-and-after",
        Duration::from_secs(10),
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_http_contains(
        port,
        "hello-from-python-before-and-after",
        Duration::from_secs(10),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    let _ = client_socket;
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_http_servers_across_multiple_sessions() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sessions = [
        (None, config_home.join("zynk-dev/zynk.sock")),
        (
            Some("work"),
            config_home.join("zynk-dev/sessions/work/zynk.sock"),
        ),
    ];
    let mut spawned = Vec::new();
    let mut ports = Vec::new();

    for (session_name, api_socket) in &sessions {
        let web_root = base.join(format!("web-{}", session_name.unwrap_or("default")));
        fs::create_dir_all(&web_root).unwrap();
        fs::write(
            web_root.join("index.html"),
            format!("hello-from-{}", session_name.unwrap_or("default")),
        )
        .unwrap();
        let port = unused_local_port();
        let server = if let Some(session_name) = session_name {
            spawn_named_session_server(&config_home, &runtime_dir, session_name)
        } else {
            spawn_default_session_server(&config_home, &runtime_dir)
        };
        wait_for_socket(api_socket, Duration::from_secs(10));
        let created = request(
            api_socket,
            serde_json::json!({
                "id": "test:workspace:create",
                "method": "workspace.create",
                "params": {"cwd": web_root, "focus": true}
            }),
        );
        let pane_id = created["result"]["root_pane"]["pane_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ok(request(
            api_socket,
            serde_json::json!({
                "id": "test:pane:run-python",
                "method": "pane.send_input",
                "params": {
                    "pane_id": pane_id,
                    "text": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                    "keys": ["Enter"]
                }
            }),
        ));
        wait_for_http_contains(
            port,
            &format!("hello-from-{}", session_name.unwrap_or("default")),
            Duration::from_secs(10),
        );
        spawned.push(server);
        ports.push((port, session_name.unwrap_or("default").to_string()));
    }
    register_runtime_dir(&runtime_dir);

    for (_session_name, api_socket) in &sessions {
        assert_ok(request(
            api_socket,
            serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
        ));
    }
    drop(spawned);

    for (_session_name, api_socket) in &sessions {
        wait_for_api(api_socket, Duration::from_secs(10));
    }
    for (port, label) in ports {
        wait_for_http_contains(
            port,
            &format!("hello-from-{label}"),
            Duration::from_secs(10),
        );
    }

    for (_session_name, api_socket) in &sessions {
        let _ = request(
            api_socket,
            serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
        );
    }
    cleanup_test_base(&base);
}

/// Gate-3 round 2 fixture: a WAL-mode foreign SQLite database (main file + nonempty `-wal`,
/// no `-shm`) planted with sqlx, the driver the product uses.
fn plant_foreign_wal_db(path: &Path, sql: &str) {
    use sqlx::{Connection, Executor};
    let src = path.with_file_name("foreign-source.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let holder = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut conn = sqlx::SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&src)
                    .create_if_missing(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
            conn.execute(sql).await.unwrap();
            conn
        });
    fs::copy(&src, path).unwrap();
    fs::copy(
        src.with_file_name("foreign-source.db-wal"),
        wal_sidecar(path),
    )
    .unwrap();
    drop(holder);
}

fn wal_sidecar(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push("-wal");
    path.with_file_name(name)
}

fn count_in_file(path: &Path, needle: &str) -> usize {
    fs::read_to_string(path)
        .unwrap_or_default()
        .matches(needle)
        .count()
}

#[test]
fn live_handoff_fails_closed_on_a_foreign_db_and_rolls_back_old_server() {
    // Gate-3 round 2 (G3-R2-SRV-001): the replacement runs the same fail-closed DB pre-flight as a
    // primary start. A foreign WAL database renamed over the live `zynk.db` (the old server keeps
    // its own open inode) makes the replacement exit before any public service; the old server
    // rolls back and keeps serving, and the foreign main/-wal bytes stay identical.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let marker = base.join("child.pid");
    let received_marker = base.join("received");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo READY $$ > {}; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();

    // The DB home is swapped as a whole: the old server keeps its open inodes (and its own
    // `-wal`/`-shm`) under the moved-aside directory, and the replacement resolves the same
    // `ZYNK_SQLITE_HOME` to a directory that now holds only the foreign pair.
    let sqlite_home = config_home.join("sqlite");
    let live_db = sqlite_home.join("zynk.db");
    assert!(
        live_db.exists(),
        "the running server must have initialized its DB"
    );
    fs::rename(&sqlite_home, base.join("sqlite.old")).unwrap();
    plant_foreign_wal_db(
        &live_db,
        "CREATE TABLE secrets (v TEXT); INSERT INTO secrets VALUES ('FOREIGN-HANDOFF-SECRET')",
    );
    let foreign_before = (
        fs::read(&live_db).unwrap(),
        fs::read(wal_sidecar(&live_db)).unwrap(),
    );
    let server_log = config_home.join("zynk-dev").join("zynk-server.log");
    let aborted_before = count_in_file(&server_log, "server startup aborted");

    let failed = request(
        &api_socket,
        serde_json::json!({"id":"test:handoff-foreign","method":"server.live_handoff","params":{}}),
    );
    assert!(
        failed.get("error").is_some(),
        "handoff onto a foreign DB must fail: {failed}"
    );
    wait_for_api(&api_socket, Duration::from_secs(10));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send-after-foreign-handoff",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "after-foreign-handoff", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        "got:after-foreign-handoff",
        Duration::from_secs(5),
    );
    assert_eq!(
        (
            fs::read(&live_db).unwrap(),
            fs::read(wal_sidecar(&live_db)).unwrap()
        ),
        foreign_before,
        "the foreign main/-wal bytes changed"
    );
    let log = wait_for_file_contains(&server_log, "db_foreign_conflict", Duration::from_secs(5));
    assert!(
        count_in_file(&server_log, "server startup aborted") > aborted_before,
        "the replacement must log the fail-closed cause durably:\n{log}"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

// ---- Codex Gate-2 round 8: DB workers and in-flight sends across a live handoff ----

/// Run the `zynk` CLI against the spawned server (same isolation as the server env).
fn run_zynk_cli(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    args: &[&str],
) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("ZYNK_SOCKET_PATH", api_socket)
        .env("ZYNK_SQLITE_HOME", config_home.join("sqlite"))
        .env_remove("ZYNK_HOME")
        .env_remove("ZYNK_CLIENT_SOCKET_PATH")
        .env_remove("ZYNK_ENV")
        .env_remove("ZYNK_PANE_ID")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "zynk {} failed ({:?}): {}
{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// `zynk send <pane> -- <body>` → the F4 outcome JSON (message_id, conversation_id, ...).
fn zynk_send(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    pane_id: &str,
    body: &str,
) -> serde_json::Value {
    let stdout = run_zynk_cli(
        config_home,
        runtime_dir,
        api_socket,
        &["send", pane_id, "--", body],
    );
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON outcome in: {stdout}"));
    let outcome: serde_json::Value = serde_json::from_str(line).unwrap();
    assert!(outcome.get("error").is_none(), "send failed: {outcome}");
    outcome
}

/// Hook-authoritative agent identity for `pane_id` the way a shipped (non-native) integration
/// reports it: the official `zynk:<agent>` source WITH the agent's session id, which grants hook
/// authority and the session ref in one report. The session is the durable receipt anchor — it
/// survives the live handoff (terminal ids do not: the replacement allocates new ones). A generic
/// `hook` source cannot carry a session id, and a reserved-native label (claude/codex) gets its
/// authority from the server's native path, so the tests report as `pi`.
fn report_agent(api_socket: &Path, pane_id: &str, label: &str) {
    assert_ok(request(
        api_socket,
        serde_json::json!({
            "id": "test:report-agent",
            "method": "pane.report_agent",
            "params": {
                "pane_id": pane_id,
                "source": format!("zynk:{label}"),
                "agent": label,
                "state": "idle",
                "agent_session_id": format!("sess-{pane_id}")
            }
        }),
    ));
}

/// The receipt request an agent hook sends for a delivered message.
fn receipt_request(sent: &serde_json::Value, receiver_pane: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "test:receipt",
        "method": "zynk.message_received",
        "params": {
            "pane_id": receiver_pane,
            "message_id": sent["message_id"],
            "conversation_id": sent["conversation_id"],
            "conversation_seq": sent["conversation_seq"],
            "runtime_session_id": sent["runtime_session_id"],
            "socket_namespace": sent["socket_namespace"]
        }
    })
}

fn db_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

async fn open_db(db: &Path) -> sqlx::SqliteConnection {
    use sqlx::Connection;
    sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(db)
            .create_if_missing(false),
    )
    .await
    .unwrap()
}

fn delivery_event_types(db: &Path, message_id: &str) -> Vec<String> {
    use sqlx::Row;
    db_block_on(async {
        let mut conn = open_db(db).await;
        sqlx::query("SELECT event_type FROM delivery_events WHERE message_id = ? ORDER BY seq")
            .bind(message_id)
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("event_type"))
            .collect()
    })
}

/// Model the sender's legitimate in-flight window: its message row is committed, but its first
/// transport event is not recorded yet (`pane send` persists, then dispatches, then records).
fn erase_delivery_events(db: &Path, message_id: &str) {
    db_block_on(async {
        let mut conn = open_db(db).await;
        sqlx::query("DELETE FROM delivery_events WHERE message_id = ?")
            .bind(message_id)
            .execute(&mut conn)
            .await
            .unwrap();
    });
}

fn embedding_job(db: &Path, message_id: &str) -> Option<(String, i64)> {
    use sqlx::Row;
    db_block_on(async {
        let mut conn = open_db(db).await;
        sqlx::query("SELECT status, attempts FROM embedding_jobs WHERE message_id = ?")
            .bind(message_id)
            .fetch_optional(&mut conn)
            .await
            .unwrap()
            .map(|row| {
                (
                    row.get::<String, _>("status"),
                    row.get::<i64, _>("attempts"),
                )
            })
    })
}

fn embedding_rows(db: &Path, message_id: &str) -> i64 {
    use sqlx::Row;
    db_block_on(async {
        let mut conn = open_db(db).await;
        sqlx::query("SELECT count(*) AS c FROM message_embeddings WHERE message_id = ?")
            .bind(message_id)
            .fetch_one(&mut conn)
            .await
            .unwrap()
            .get::<i64, _>("c")
    })
}

fn wait_for_embedding_job(db: &Path, message_id: &str, wanted: (&str, i64), timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let state = embedding_job(db, message_id);
        if state.as_ref().map(|(s, a)| (s.as_str(), *a)) == Some(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "embedding job for {message_id} never reached {wanted:?} (now {state:?})"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn socket_ino(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|meta| meta.ino())
}

/// Send `server.live_handoff` on its own thread (the old server blocks inside it while it waits
/// for its workers / the replacement); the response arrives on the returned channel.
fn spawn_handoff_request(api_socket: &Path) -> std::sync::mpsc::Receiver<serde_json::Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    let api_socket = api_socket.to_path_buf();
    thread::spawn(move || {
        let _ = tx.send(request(
            &api_socket,
            serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
        ));
    });
    rx
}

/// After a successful handoff: the replacement's pid, registered for unconditional cleanup.
fn register_replacement(runtime_dir: &Path, old_pid: Option<u32>) {
    if let Some(old_pid) = old_pid {
        let pid = wait_for_replacement_server_pid(runtime_dir, old_pid, Duration::from_secs(10));
        register_spawned_zynk_pid(Some(pid));
    }
}

struct HandoffFixture {
    base: PathBuf,
    config_home: PathBuf,
    runtime_dir: PathBuf,
    api_socket: PathBuf,
    db: PathBuf,
    pane_id: String,
    spawned: SpawnedZynk,
}

/// A server with one root pane; `extra_env` customizes the server process (test hooks).
fn handoff_fixture(extra_env: &[(&str, &str)]) -> HandoffFixture {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let spawned = spawn_server_with_env(&config_home, &runtime_dir, &api_socket, extra_env);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    HandoffFixture {
        db: config_home.join("sqlite").join("zynk.db"),
        base,
        config_home,
        runtime_dir,
        api_socket,
        pane_id,
        spawned,
    }
}

fn in_flight_send_is_not_failed_by_a_handoff(rollback: bool) {
    // Codex Gate-2 round 8 (P1): the replacement's DB pre-flight must not run orphan-message
    // recovery — the old server is alive and a sender may be between persisting its message and its
    // first transport event. A synthesized `failed` would make the sender's later `submitted`
    // invalid (`invalid_delivery_transition`) and the message could never be receipted.
    let _lock = test_lock();
    let env: &[(&str, &str)] = if rollback {
        &[("ZYNK_TEST_HANDOFF_IMPORT_FAIL", "after_restored")]
    } else {
        &[]
    };
    let f = handoff_fixture(env);
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let sent = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "in flight",
    );
    let message_id = sent["message_id"].as_str().unwrap().to_string();
    assert_eq!(delivery_event_types(&f.db, &message_id), vec!["submitted"]);
    erase_delivery_events(&f.db, &message_id);

    let response = request(
        &f.api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    );
    if rollback {
        assert!(
            response.get("error").is_some(),
            "handoff should roll back: {response}"
        );
    } else {
        assert_ok(response);
        register_replacement(&f.runtime_dir, f.spawned.child.process_id());
        drop(f.spawned);
    }
    wait_for_api(&f.api_socket, Duration::from_secs(10));
    assert_eq!(
        delivery_event_types(&f.db, &message_id),
        Vec::<String>::new(),
        "the handoff synthesized a delivery event for an in-flight send"
    );
    // A later send on the same server still works end to end (the sender records `submitted`);
    // the receiver's hook re-reports its identity as agent hooks do on every event.
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let later = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "after handoff",
    );
    assert_eq!(
        delivery_event_types(&f.db, later["message_id"].as_str().unwrap()),
        vec!["submitted"]
    );
    let _ = request(
        &f.api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&f.base);
}

#[test]
fn live_handoff_does_not_fail_an_in_flight_send() {
    in_flight_send_is_not_failed_by_a_handoff(false);
}

#[test]
fn rolled_back_live_handoff_does_not_fail_an_in_flight_send() {
    in_flight_send_is_not_failed_by_a_handoff(true);
}

fn db_workers_hand_over_a_blocked_job(commit_fails: bool) {
    // Codex Gate-2 rounds 8/9: a job blocked inside the old server's embedding worker keeps the
    // handoff from moving service at all until the worker is idle (bounded pause BEFORE any socket
    // is withdrawn); once released within the deadline the old worker finishes it, the replacement
    // starts its workers only after "committed", and a failed commit restores the old server's
    // workers. The job is attempted exactly once with exactly one embedding row either way.
    let _lock = test_lock();
    let base_hint = unique_test_dir();
    let release = base_hint.join("release-embedding");
    let release_str = release.to_string_lossy().to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("ZYNK_EMBED_PROVIDER", "fake-blocking"),
        ("ZYNK_TEST_EMBED_RELEASE_FILE", &release_str),
        ("ZYNK_EMBED_POLL_MS", "50"),
    ];
    if commit_fails {
        env.push(("ZYNK_TEST_HANDOFF_COMMIT_FAIL", "1"));
    }
    let f = handoff_fixture(&env);
    fs::create_dir_all(&base_hint).unwrap();
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let sent = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "before handoff",
    );
    let message_id = sent["message_id"].as_str().unwrap().to_string();
    wait_for_embedding_job(&f.db, &message_id, ("running", 1), Duration::from_secs(10));

    let before_ino = socket_ino(&f.api_socket).expect("old API socket");
    let old_pid = f.spawned.child.process_id();
    let handoff = spawn_handoff_request(&f.api_socket);
    // Phase acknowledgement: the request has entered the worker pause (durable log line), and the
    // job is still held — the handoff must neither complete nor withdraw service.
    let server_log = f.config_home.join("zynk-dev").join("zynk-server.log");
    wait_for_file_contains(
        &server_log,
        "pausing DB workers before withdrawing service",
        Duration::from_secs(10),
    );
    thread::sleep(Duration::from_millis(150));
    assert!(
        matches!(
            handoff.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "the handoff completed while the old worker still owned a running job"
    );
    assert_eq!(
        embedding_job(&f.db, &message_id),
        Some(("running".to_string(), 1))
    );
    assert_eq!(
        socket_ino(&f.api_socket),
        Some(before_ino),
        "service was withdrawn while blocked"
    );
    fs::write(&release, b"go").unwrap();
    let response = handoff
        .recv_timeout(Duration::from_secs(30))
        .expect("handoff response within the bounded wait");
    if commit_fails {
        assert_eq!(response["error"]["code"], "handoff_failed", "{response}");
        assert_eq!(
            response["error"]["message"], "test handoff commit failure",
            "only the injected commit failure counts: {response}"
        );
    } else {
        assert_ok(response);
        register_replacement(&f.runtime_dir, old_pid);
        drop(f.spawned);
    }
    wait_for_api(&f.api_socket, Duration::from_secs(10));

    wait_for_embedding_job(&f.db, &message_id, ("done", 1), Duration::from_secs(10));
    assert_eq!(
        embedding_rows(&f.db, &message_id),
        1,
        "exactly one embedding row"
    );

    // The serving server (replacement, or the restored old one) owns a working receipt worker ...
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let receipt = request(&f.api_socket, receipt_request(&sent, &f.pane_id));
    assert!(receipt.get("error").is_none(), "receipt failed: {receipt}");
    assert_eq!(
        receipt["result"]["delivery_status"], "received",
        "{receipt}"
    );
    // ... and a working embedding worker.
    let later = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "after handoff",
    );
    let later_id = later["message_id"].as_str().unwrap().to_string();
    wait_for_embedding_job(&f.db, &later_id, ("done", 1), Duration::from_secs(10));
    assert_eq!(
        embedding_job(&f.db, &message_id),
        Some(("done".to_string(), 1)),
        "no stale overwrite"
    );

    let _ = request(
        &f.api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&f.base);
    let _ = fs::remove_dir_all(&base_hint);
}

#[test]
fn busy_db_workers_reject_a_live_handoff_within_the_deadline() {
    // Codex Gate-2 round 9 (P2): the worker quiescence is decided BEFORE service is withdrawn and
    // is bounded — a job held past ZYNK_HANDOFF_WORKER_IDLE_MS fails the handoff quickly, the old
    // server never removes a socket, keeps its workers (single owner), and a later handoff succeeds.
    let _lock = test_lock();
    let base_hint = unique_test_dir();
    let release = base_hint.join("release-embedding");
    let release_str = release.to_string_lossy().to_string();
    let f = handoff_fixture(&[
        ("ZYNK_EMBED_PROVIDER", "fake-blocking"),
        ("ZYNK_TEST_EMBED_RELEASE_FILE", &release_str),
        ("ZYNK_EMBED_POLL_MS", "50"),
        ("ZYNK_HANDOFF_WORKER_IDLE_MS", "500"),
    ]);
    fs::create_dir_all(&base_hint).unwrap();
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let sent = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "held job",
    );
    let message_id = sent["message_id"].as_str().unwrap().to_string();
    wait_for_embedding_job(&f.db, &message_id, ("running", 1), Duration::from_secs(10));

    let before_ino = socket_ino(&f.api_socket).expect("old API socket");
    let started = Instant::now();
    let response = spawn_handoff_request(&f.api_socket)
        .recv_timeout(Duration::from_secs(5))
        .expect("a busy handoff must fail within the bounded deadline");
    // ZYNK_HANDOFF_WORKER_IDLE_MS=500: the refusal must land well inside the deadline's order of
    // magnitude, with scheduling margin for saturated hosted runners (Gate-3 round 4).
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a 500 ms worker deadline took {:?}",
        started.elapsed()
    );
    assert_eq!(response["error"]["code"], "handoff_failed", "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{response}"
    );
    assert_eq!(
        socket_ino(&f.api_socket),
        Some(before_ino),
        "service was withdrawn"
    );
    assert_eq!(
        embedding_job(&f.db, &message_id),
        Some(("running".to_string(), 1))
    );
    wait_for_api(&f.api_socket, Duration::from_secs(5));

    fs::write(&release, b"go").unwrap();
    wait_for_embedding_job(&f.db, &message_id, ("done", 1), Duration::from_secs(10));
    assert_eq!(embedding_rows(&f.db, &message_id), 1);
    // Idle now: the same server hands off successfully and the replacement serves receipts.
    let old_pid = f.spawned.child.process_id();
    let response = spawn_handoff_request(&f.api_socket)
        .recv_timeout(Duration::from_secs(30))
        .expect("handoff response");
    assert_ok(response);
    register_replacement(&f.runtime_dir, old_pid);
    drop(f.spawned);
    wait_for_api(&f.api_socket, Duration::from_secs(10));
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let receipt = request(&f.api_socket, receipt_request(&sent, &f.pane_id));
    assert!(receipt.get("error").is_none(), "receipt failed: {receipt}");
    assert_eq!(
        embedding_job(&f.db, &message_id),
        Some(("done".to_string(), 1))
    );

    let _ = request(
        &f.api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&f.base);
    let _ = fs::remove_dir_all(&base_hint);
}

#[test]
fn receipt_backlog_rejects_a_live_handoff_then_drains_and_hands_over() {
    // Codex Gate-2 round 11 (P2): receipts whose API callers already timed out still sit in the
    // worker queue. Quiescence must account for them: while the backlog cannot drain within the
    // idle deadline the handoff is refused with service untouched; once it drained (each receipt
    // committed exactly once) the handoff succeeds. Deterministic via the receipt block-file hook
    // (the worker blocks before each job until the file exists) and a short API receipt timeout.
    let _lock = test_lock();
    let base_hint = unique_test_dir();
    let block = base_hint.join("release-receipts");
    let block_str = block.to_string_lossy().to_string();
    let f = handoff_fixture(&[
        ("ZYNK_TEST_RECEIPT_BLOCK_FILE", &block_str),
        ("ZYNK_RECEIPT_TIMEOUT_MS", "300"),
        ("ZYNK_HANDOFF_WORKER_IDLE_MS", "500"),
        ("ZYNK_EMBED_POLL_MS", "50"),
    ]);
    fs::create_dir_all(&base_hint).unwrap();
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let sent: Vec<serde_json::Value> = (0..3)
        .map(|i| {
            zynk_send(
                &f.config_home,
                &f.runtime_dir,
                &f.api_socket,
                &f.pane_id,
                &format!("receipt {i}"),
            )
        })
        .collect();
    // Three real receipts: each API call times out (the worker is blocked), each job stays queued.
    for message in &sent {
        let response = request(&f.api_socket, receipt_request(message, &f.pane_id));
        assert_eq!(
            response["error"]["code"], "receipt_result_unknown",
            "{response}"
        );
    }
    let before_ino = socket_ino(&f.api_socket).expect("old API socket");
    let started = Instant::now();
    let response = spawn_handoff_request(&f.api_socket)
        .recv_timeout(Duration::from_secs(5))
        .expect("a handoff with a receipt backlog must fail within the bounded deadline");
    // ZYNK_HANDOFF_WORKER_IDLE_MS=500: the refusal must land well inside the deadline's order of
    // magnitude, with scheduling margin for saturated hosted runners (Gate-3 round 4).
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a 500 ms worker deadline took {:?}",
        started.elapsed()
    );
    assert_eq!(response["error"]["code"], "handoff_failed", "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{response}"
    );
    assert_eq!(
        socket_ino(&f.api_socket),
        Some(before_ino),
        "service was withdrawn"
    );
    for message in &sent {
        let id = message["message_id"].as_str().unwrap();
        assert_eq!(
            delivery_event_types(&f.db, id),
            vec!["submitted"],
            "no receipt may commit while blocked"
        );
    }
    // Release: the backlog drains — every receipt commits exactly once — and the handoff succeeds.
    fs::write(&block, b"go").unwrap();
    for message in &sent {
        let id = message["message_id"].as_str().unwrap().to_string();
        let deadline = Instant::now() + Duration::from_secs(10);
        while delivery_event_types(&f.db, &id) != vec!["submitted", "received"] {
            assert!(Instant::now() < deadline, "receipt {id} never committed");
            thread::sleep(Duration::from_millis(25));
        }
    }
    let old_pid = f.spawned.child.process_id();
    let response = spawn_handoff_request(&f.api_socket)
        .recv_timeout(Duration::from_secs(30))
        .expect("handoff response");
    assert_ok(response);
    register_replacement(&f.runtime_dir, old_pid);
    drop(f.spawned);
    wait_for_api(&f.api_socket, Duration::from_secs(10));
    for message in &sent {
        let id = message["message_id"].as_str().unwrap();
        assert_eq!(
            delivery_event_types(&f.db, id),
            vec!["submitted", "received"],
            "exactly once"
        );
    }
    // The replacement serves a fresh receipt.
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let later = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "after handoff",
    );
    let receipt = request(&f.api_socket, receipt_request(&later, &f.pane_id));
    assert!(receipt.get("error").is_none(), "receipt failed: {receipt}");

    let _ = request(
        &f.api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&f.base);
    let _ = fs::remove_dir_all(&base_hint);
}

#[test]
fn legacy_handoff_destination_eof_is_reported_with_the_restart_hint() {
    // Codex Gate-2 round 10 (P2): a replacement older than the handoff-version fence (zynk 3.0.x)
    // closes on a manifest it does not understand without answering. The sender must keep serving,
    // and its requester + durable log must carry the transport cause AND the possible-incompatible-
    // peer / restart hint (the old child cannot log anything useful).
    let _lock = test_lock();
    let f = handoff_fixture(&[("ZYNK_TEST_HANDOFF_IMPORT_FAIL", "close_before_validate")]);
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let sent = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "before",
    );
    let message_id = sent["message_id"].as_str().unwrap().to_string();
    wait_for_embedding_job(&f.db, &message_id, ("done", 1), Duration::from_secs(10));
    let before_ino = socket_ino(&f.api_socket).expect("old API socket");

    let response = spawn_handoff_request(&f.api_socket)
        .recv_timeout(Duration::from_secs(30))
        .expect("handoff response");
    assert_eq!(response["error"]["code"], "handoff_failed", "{response}");
    let message = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("handoff stream closed while reading line")
            && message.contains("restart zynk normally"),
        "the original EOF cause and the restart hint must both reach the requester: {response}"
    );
    assert_eq!(
        socket_ino(&f.api_socket),
        Some(before_ino),
        "service was withdrawn"
    );
    wait_for_api(&f.api_socket, Duration::from_secs(5));
    let server_log = f.config_home.join("zynk-dev").join("zynk-server.log");
    let log = wait_for_file_contains(&server_log, "restart zynk normally", Duration::from_secs(5));
    assert!(
        log.contains("did not validate the manifest"),
        "the sender must log the failed validation durably:\n{log}"
    );
    let later = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "after",
    );
    let later_id = later["message_id"].as_str().unwrap().to_string();
    wait_for_embedding_job(&f.db, &later_id, ("done", 1), Duration::from_secs(10));
    let receipt = request(&f.api_socket, receipt_request(&sent, &f.pane_id));
    assert!(receipt.get("error").is_none(), "receipt failed: {receipt}");

    let _ = request(
        &f.api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&f.base);
}

#[test]
fn legacy_handoff_sender_is_rejected_before_service_moves() {
    // Codex Gate-2 round 9 (P2): the ordered DB-worker handover is a protocol contract. A sender
    // speaking handoff version 1 (zynk 3.0.x, which sends "committed" without quiescing its
    // workers) is rejected by the replacement before "validated": the old server rolls back with
    // its sockets and workers untouched, and the reason is in the durable server log.
    let _lock = test_lock();
    let f = handoff_fixture(&[("ZYNK_TEST_HANDOFF_MANIFEST_VERSION", "1")]);
    report_agent(&f.api_socket, &f.pane_id, "pi");
    let sent = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "before",
    );
    let message_id = sent["message_id"].as_str().unwrap().to_string();
    wait_for_embedding_job(&f.db, &message_id, ("done", 1), Duration::from_secs(10));
    let before_ino = socket_ino(&f.api_socket).expect("old API socket");

    let response = spawn_handoff_request(&f.api_socket)
        .recv_timeout(Duration::from_secs(30))
        .expect("handoff response");
    assert_eq!(response["error"]["code"], "handoff_failed", "{response}");
    let message = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("unsupported handoff version 1")
            && message.contains("restart zynk normally"),
        "the version mismatch and its remedy must reach the requester: {response}"
    );
    assert_eq!(
        socket_ino(&f.api_socket),
        Some(before_ino),
        "service was withdrawn"
    );
    wait_for_api(&f.api_socket, Duration::from_secs(5));
    let server_log = f.config_home.join("zynk-dev").join("zynk-server.log");
    wait_for_file_contains(
        &server_log,
        "unsupported handoff version 1",
        Duration::from_secs(5),
    );
    // Still serving with its own workers.
    let later = zynk_send(
        &f.config_home,
        &f.runtime_dir,
        &f.api_socket,
        &f.pane_id,
        "after",
    );
    let later_id = later["message_id"].as_str().unwrap().to_string();
    wait_for_embedding_job(&f.db, &later_id, ("done", 1), Duration::from_secs(10));
    let receipt = request(&f.api_socket, receipt_request(&sent, &f.pane_id));
    assert!(receipt.get("error").is_none(), "receipt failed: {receipt}");

    let _ = request(
        &f.api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&f.base);
}

#[test]
fn live_handoff_hands_the_db_workers_over_without_duplicate_processing() {
    db_workers_hand_over_a_blocked_job(false);
}

#[test]
fn failed_live_handoff_commit_restores_the_db_workers() {
    db_workers_hand_over_a_blocked_job(true);
}

#[test]
fn live_handoff_bad_expected_protocol_rolls_back_old_server() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let marker = base.join("child.pid");
    let received_marker = base.join("received");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo READY $$ > {}; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();

    let failed = request(
        &api_socket,
        serde_json::json!({
            "id": "test:bad-handoff",
            "method": "server.live_handoff",
            "params": {"expected_protocol": 999999}
        }),
    );
    assert!(
        failed.get("error").is_some(),
        "bad protocol handoff should fail: {failed}"
    );
    wait_for_api(&api_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send-after-failed-handoff",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "after-failed-handoff", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        "got:after-failed-handoff",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &pane_id, "got:after-failed-handoff");

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

fn live_handoff_import_failure_rolls_back_old_server_at(failure_point: &str) {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");
    let marker = base.join("child.pid");
    let received_marker = base.join("received");

    let spawned = spawn_server_with_env(
        &config_home,
        &runtime_dir,
        &api_socket,
        &[("ZYNK_TEST_HANDOFF_IMPORT_FAIL", failure_point)],
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo READY $$ > {}; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();

    let failed = request(
        &api_socket,
        serde_json::json!({"id":"test:handoff-fail","method":"server.live_handoff","params":{}}),
    );
    assert!(
        failed.get("error").is_some(),
        "{failure_point} handoff should fail: {failed}"
    );
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send-after-import-failure",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": failure_point, "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        &format!("got:{failure_point}"),
        Duration::from_secs(5),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_after_restored_failure_rolls_back_old_server() {
    live_handoff_import_failure_rolls_back_old_server_at("after_restored");
}
