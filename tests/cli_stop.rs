use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

mod support;

struct Fixture {
    base: PathBuf,
}

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Finished<'a>(&'a AtomicBool);

impl Drop for Finished<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct MockStop {
    api: PathBuf,
    client: PathBuf,
    reply: Option<String>,
    keep_api: bool,
    keep_client: bool,
    minimum_wait: Option<Duration>,
    decoys: Vec<PathBuf>,
}

impl MockStop {
    fn new(api: PathBuf, client: PathBuf) -> Self {
        Self {
            api,
            client,
            reply: Some(json!({"id":"fixture:ack", "result":{}}).to_string()),
            keep_api: false,
            keep_client: false,
            minimum_wait: None,
            decoys: Vec::new(),
        }
    }
}

fn bind(path: &Path) -> UnixListener {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(path).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn drain(listener: &UnixListener) -> usize {
    let mut count = 0;
    loop {
        match listener.accept() {
            Ok(_) => count += 1,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return count,
            Err(error) => panic!("drain listener: {error}"),
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let base = support::test_root().join(format!(
            "st-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        Self { base }
    }

    fn api(&self) -> PathBuf {
        self.base.join("api.sock")
    }

    fn client(&self) -> PathBuf {
        self.base.join("api-client.sock")
    }

    fn session_dir(&self) -> PathBuf {
        self.base.join("config").join(if cfg!(debug_assertions) {
            "zynk-dev"
        } else {
            "zynk"
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
        command
            .args(args)
            .env_clear()
            .env("HOME", self.base.join("home"))
            .env("XDG_CONFIG_HOME", self.base.join("config"))
            .env("XDG_DATA_HOME", self.base.join("data"))
            .env("XDG_CACHE_HOME", self.base.join("cache"))
            .env("XDG_STATE_HOME", self.base.join("state"))
            .env("XDG_RUNTIME_DIR", self.base.join("runtime"))
            .env("ZYNK_HOME", self.base.join("db"))
            .env("ZYNK_SQLITE_HOME", self.base.join("sqlite"))
            .env("ZYNK_SOCKET_PATH", self.api())
            .current_dir(&self.base)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn exchange(&self, mut command: Command, scenario: MockStop) -> (Option<Value>, Output) {
        let mut api = Some(bind(&scenario.api));
        let mut client = Some(bind(&scenario.client));
        let decoys: Vec<_> = scenario.decoys.iter().map(|path| bind(path)).collect();
        let done = AtomicBool::new(false);
        let (result, elapsed) = thread::scope(|scope| {
            let server = scope.spawn(|| {
                let started = Instant::now();
                let mut request = None;
                let mut decoy_connections = 0;
                while !done.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(8) {
                    if let Some(accepted) = api.as_ref().map(UnixListener::accept) {
                        match accepted {
                            Ok((mut stream, _)) if request.is_none() => {
                                stream
                                    .set_read_timeout(Some(Duration::from_secs(3)))
                                    .unwrap();
                                stream
                                    .set_write_timeout(Some(Duration::from_secs(3)))
                                    .unwrap();
                                let mut line = String::new();
                                BufReader::new(stream.try_clone().unwrap())
                                    .read_line(&mut line)
                                    .unwrap();
                                if !line.is_empty() {
                                    request = Some(serde_json::from_str(&line).unwrap());
                                    // Close selected listeners before replying, but leave stale files.
                                    if !scenario.keep_api {
                                        api.take();
                                    }
                                    if !scenario.keep_client {
                                        client.take();
                                    }
                                    if let Some(reply) = &scenario.reply {
                                        writeln!(stream, "{reply}").unwrap();
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(error) => panic!("accept stop request: {error}"),
                        }
                    }
                    if let Some(client) = &client {
                        drain(client);
                    }
                    for listener in &decoys {
                        decoy_connections += drain(listener);
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                (request, decoy_connections)
            });
            // On panic, reap before joining pipe readers, then stop the listener worker.
            let finish = Finished(&done);
            let mut child = ReapedChild(command.spawn().unwrap());
            let mut stdout = child.0.stdout.take().unwrap();
            let mut stderr = child.0.stderr.take().unwrap();
            let out = scope.spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).unwrap();
                bytes
            });
            let err = scope.spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).unwrap();
                bytes
            });
            let started = Instant::now();
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                if started.elapsed() >= Duration::from_secs(20) {
                    let elapsed = started.elapsed();
                    let _ = child.0.kill();
                    let status = child.0.wait();
                    drop(finish);
                    let _ = server.join();
                    let stdout = out.join().unwrap_or_default();
                    let stderr = err.join().unwrap_or_default();
                    panic!(
                        "stop CLI exceeded fixture bound after {elapsed:?}; status={status:?}; stdout={}; stderr={}",
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr)
                    );
                }
                thread::sleep(Duration::from_millis(5));
            };
            let elapsed = started.elapsed();
            drop(finish);
            let (request, decoy_connections) = server.join().unwrap();
            assert_eq!(decoy_connections, 0, "stop touched an unselected socket");
            (
                (
                    request,
                    Output {
                        status,
                        stdout: out.join().unwrap(),
                        stderr: err.join().unwrap(),
                    },
                ),
                elapsed,
            )
        });
        if let Some(minimum_wait) = scenario.minimum_wait {
            assert!(
                elapsed >= minimum_wait,
                "stop CLI returned after {elapsed:?}, before the required {minimum_wait:?}; stdout={}; stderr={}",
                String::from_utf8_lossy(&result.1.stdout),
                String::from_utf8_lossy(&result.1.stderr)
            );
        }
        assert!(scenario.api.exists(), "fixture must leave a stale API path");
        assert!(
            scenario.client.exists(),
            "fixture must leave a stale client path"
        );
        assert!(!self.base.join("db").exists());
        assert!(!self.base.join("sqlite").exists());
        if result.0.is_none() {
            eprintln!(
                "no stop request; CLI stderr: {}",
                String::from_utf8_lossy(&result.1.stderr)
            );
        }
        result
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn assert_request(request: Option<Value>, id: &str) {
    assert_eq!(
        request,
        Some(json!({"id":id, "method":"server.stop", "params":{}}))
    );
}

fn assert_timeout(output: &Output, path: &Path) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "an ACK is not socket cleanup"
    );
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("did not stop"), "{error}");
    assert!(error.contains(path.to_str().unwrap()), "{error}");
}

#[test]
fn stop_active_ack_with_api_still_reachable_fails() {
    let f = Fixture::new();
    let mut scenario = MockStop::new(f.api(), f.client());
    scenario.keep_api = true;
    scenario.minimum_wait = Some(Duration::from_secs(14));
    let (request, output) = f.exchange(f.command(&["server", "stop"]), scenario);
    assert_request(request, "cli:request");
    assert_timeout(&output, &f.api());
}

#[test]
fn stop_active_ack_with_client_still_reachable_fails() {
    let f = Fixture::new();
    let mut scenario = MockStop::new(f.api(), f.client());
    scenario.keep_client = true;
    scenario.minimum_wait = Some(Duration::from_secs(14));
    let (request, output) = f.exchange(f.command(&["server", "stop"]), scenario);
    assert_request(request, "cli:request");
    assert_timeout(&output, &f.client());
}

#[test]
fn stop_active_ack_with_stale_socket_files_succeeds_quietly() {
    let f = Fixture::new();
    let (request, output) = f.exchange(
        f.command(&["server", "stop"]),
        MockStop::new(f.api(), f.client()),
    );
    assert_request(request, "cli:request");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn stop_active_eof_requires_both_sockets_unreachable() {
    for keep_client in [true, false] {
        let f = Fixture::new();
        let mut scenario = MockStop::new(f.api(), f.client());
        scenario.keep_client = keep_client;
        scenario.reply = None;
        scenario.minimum_wait = keep_client.then_some(Duration::from_secs(14));
        let (request, output) = f.exchange(f.command(&["server", "stop"]), scenario);
        assert_request(request, "cli:request");
        if keep_client {
            assert_timeout(&output, &f.client());
        } else {
            assert_eq!(output.status.code(), Some(0));
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        }
    }
}

#[test]
fn stop_active_preserves_full_remote_error_even_after_socket_cleanup() {
    for error in [
        json!({"code":"denied", "message":"no stop", "unknown":[1,2]}),
        Value::Null,
    ] {
        let f = Fixture::new();
        let response = json!({"id":"remote:reply", "error":error, "future_envelope":{"kept":true}});
        let mut scenario = MockStop::new(f.api(), f.client());
        scenario.reply = Some(response.to_string());
        let (request, output) = f.exchange(f.command(&["server", "stop"]), scenario);
        assert_request(request, "cli:request");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, format!("{response}\n").as_bytes());
    }
}

#[test]
fn stop_named_preserves_both_remote_error_layers() {
    for error in [
        json!({"code":"denied", "message":"no stop", "unknown":[1,2]}),
        Value::Null,
    ] {
        let f = Fixture::new();
        let api = f.session_dir().join("sessions/n/zynk.sock");
        let client = api.with_file_name("zynk-client.sock");
        let mut scenario = MockStop::new(api, client);
        scenario.reply = Some(
            json!({"id":"remote:reply", "error":error, "future_envelope":"ignored"}).to_string(),
        );
        let (request, output) = f.exchange(f.command(&["session", "stop", "n"]), scenario);
        assert_request(request, "cli:session:stop");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let expected = json!({"error":{"code":"session_stop_failed", "message":error.to_string()}});
        assert_eq!(output.stderr, format!("{expected}\n").as_bytes());
    }
}

#[test]
fn stop_active_malformed_response_fails_after_socket_cleanup() {
    let f = Fixture::new();
    let mut scenario = MockStop::new(f.api(), f.client());
    scenario.reply = Some("not json".into());
    let (request, output) = f.exchange(f.command(&["server", "stop"]), scenario);
    assert_request(request, "cli:request");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected ident"));
}

#[test]
fn stop_cli_uses_selected_api_and_client_paths() {
    for selection in [
        "explicit",
        "api_override",
        "client_override",
        "default",
        "named",
    ] {
        let f = Fixture::new();
        let default_api = f.session_dir().join("zynk.sock");
        let named_api = f.session_dir().join("sessions/n/zynk.sock");
        let override_client = f.base.join("other.sock");
        let mut command = f.command(&["server", "stop"]);
        let mut scenario = match selection {
            "explicit" => {
                command
                    .args(["--session", "n"])
                    .env("ZYNK_CLIENT_SOCKET_PATH", &override_client);
                let mut scenario = MockStop::new(
                    named_api.clone(),
                    named_api.with_file_name("zynk-client.sock"),
                );
                scenario.decoys = vec![f.api(), f.client(), override_client.clone()];
                scenario
            }
            "api_override" => {
                command.env("ZYNK_CLIENT_SOCKET_PATH", &override_client);
                let mut scenario = MockStop::new(f.api(), f.client());
                scenario.decoys = vec![default_api.clone(), override_client.clone()];
                scenario
            }
            "client_override" => {
                command
                    .env_remove("ZYNK_SOCKET_PATH")
                    .env("ZYNK_CLIENT_SOCKET_PATH", &override_client);
                let mut scenario = MockStop::new(default_api.clone(), override_client.clone());
                scenario.decoys = vec![default_api.with_file_name("zynk-client.sock")];
                scenario
            }
            "default" => {
                command.env_remove("ZYNK_SOCKET_PATH");
                MockStop::new(
                    default_api.clone(),
                    default_api.with_file_name("zynk-client.sock"),
                )
            }
            "named" => {
                command = f.command(&["session", "stop", "n", "--json"]);
                command.env("ZYNK_CLIENT_SOCKET_PATH", &override_client);
                let mut scenario = MockStop::new(
                    named_api.clone(),
                    named_api.with_file_name("zynk-client.sock"),
                );
                scenario.decoys = vec![f.api(), f.client(), override_client.clone()];
                scenario
            }
            _ => unreachable!(),
        };
        // All selected sockets close. Unselected sentinels remain live until CLI exit.
        scenario.keep_api = false;
        let (request, output) = f.exchange(command, scenario);
        assert_request(
            request,
            if selection == "named" {
                "cli:session:stop"
            } else {
                "cli:request"
            },
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "{selection}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "{selection}");
        if selection == "named" {
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["stopped"], true);
            assert_eq!(response["session"]["name"], "n");
            assert_eq!(response["session"]["running"], false);
            assert_eq!(
                response["session"]["socket_path"],
                named_api.to_str().unwrap()
            );
        } else {
            assert!(output.stdout.is_empty(), "{selection}");
        }
    }
}

#[test]
fn stop_cli_invalid_arguments_never_connect() {
    let f = Fixture::new();
    let (request, output) = f.exchange(
        f.command(&["server", "stop", "extra"]),
        MockStop::new(f.api(), f.client()),
    );
    assert_eq!(request, None);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: zynk server stop"));
}
