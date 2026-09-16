// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
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

impl Fixture {
    fn new() -> Self {
        let base = support::test_root().join(format!(
            "cli-runtime-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        Self { base }
    }

    fn exchange(&self, args: &[&str], response: &Value) -> (Option<Value>, Output) {
        let socket = self.base.join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let done = AtomicBool::new(false);
        let result = thread::scope(|scope| {
            let server = scope.spawn(|| {
                let started = Instant::now();
                let mut accepted_connections = 0;
                loop {
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                accepted_connections += 1;
                                break stream;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                if done.load(Ordering::Acquire)
                                    || started.elapsed() >= Duration::from_secs(3)
                                {
                                    assert_eq!(accepted_connections, 0);
                                    return None;
                                }
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("mock accept: {error}"),
                        }
                    };
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
                    let request: Value = serde_json::from_str(&line).unwrap();
                    if request["method"] == "ping" {
                        assert_eq!(accepted_connections, 1);
                        assert_eq!(request["params"], json!({}));
                        writeln!(
                            stream,
                            "{}",
                            json!({"id": request["id"], "result": {
                                "type": "pong", "version": "fixture-compatible", "protocol": support::CURRENT_PROTOCOL
                            }})
                        )
                        .unwrap();
                        continue;
                    }
                    assert_eq!(accepted_connections, 2);
                    writeln!(stream, "{response}").unwrap();
                    return Some(request);
                }
            });
            // The guard lives inside the scope: panic cleanup reaps before pipe-reader joins.
            let mut child = ReapedChild(
                Command::new(env!("CARGO_BIN_EXE_zynk"))
                    .args(args)
                    .env_clear()
                    .env("HOME", self.base.join("home"))
                    .env("XDG_CONFIG_HOME", self.base.join("config"))
                    .env("XDG_DATA_HOME", self.base.join("data"))
                    .env("XDG_CACHE_HOME", self.base.join("cache"))
                    .env("XDG_RUNTIME_DIR", self.base.join("runtime"))
                    .env("ZYNK_HOME", self.base.join("db"))
                    .env("ZYNK_SQLITE_HOME", self.base.join("sqlite"))
                    .env("ZYNK_SOCKET_PATH", &socket)
                    .env("ZYNK_CLIENT_SOCKET_PATH", self.base.join("client.sock"))
                    .env("ZYNK_PANE_ID", "caller:p9")
                    .current_dir(&self.base)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
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
                assert!(
                    started.elapsed() < Duration::from_secs(3),
                    "CLI timed out: {args:?}"
                );
                thread::sleep(Duration::from_millis(5));
            };
            done.store(true, Ordering::Release);
            let output = Output {
                status,
                stdout: out.join().unwrap(),
                stderr: err.join().unwrap(),
            };
            (server.join().unwrap(), output)
        });
        drop(listener);
        fs::remove_file(socket).unwrap();
        for name in ["home", "config", "data", "cache", "runtime", "db", "sqlite"] {
            assert!(!self.base.join(name).exists(), "CLI created {name}");
        }
        result
    }

    fn assert_case(&self, args: &[&str], expected: Value) {
        for failed in [false, true] {
            let response = if failed {
                json!({"id": expected["id"], "error": {"code": "fixture_error", "message": "kept verbatim", "data": {"nested": [1,2]}}, "future_envelope": "preserved"})
            } else {
                json!({"id": expected["id"], "result": {"future_result": ["kept", 7]}, "future_envelope": {"unknown": true}})
            };
            let (request, output) = self.exchange(args, &response);
            assert_eq!(request, Some(expected.clone()), "request for {args:?}");
            assert_eq!(
                output.status.code(),
                Some(i32::from(failed)),
                "exit for {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let (payload, empty) = if failed {
                (&output.stderr, &output.stdout)
            } else {
                (&output.stdout, &output.stderr)
            };
            assert!(empty.is_empty(), "wrong output channel for {args:?}");
            assert_eq!(
                serde_json::from_slice::<Value>(payload).unwrap(),
                response,
                "response for {args:?}"
            );
            assert!(payload.ends_with(b"\n"));
        }
    }

    fn assert_invalid(&self, args: &[&str]) {
        let (request, output) = self.exchange(args, &json!({"id":"unexpected", "result":{}}));
        assert_eq!(request, None, "invalid args connected: {args:?}");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn runtime_workspace_cli_contract() {
    let fixture = Fixture::new();
    let cases = [
        (
            vec!["workspace", "list"],
            json!({"id":"cli:workspace:list", "method":"workspace.list", "params":{}}),
        ),
        (
            vec![
                "workspace",
                "create",
                "--cwd",
                "relative/path",
                "--label",
                "two words",
                "--no-focus",
                "--focus",
            ],
            json!({"id":"cli:workspace:create", "method":"workspace.create", "params":{"cwd":"relative/path", "label":"two words", "focus":true}}),
        ),
        (
            vec![
                "workspace",
                "create",
                "--cwd",
                "~/literal",
                "--focus",
                "--no-focus",
            ],
            json!({"id":"cli:workspace:create", "method":"workspace.create", "params":{"cwd":"~/literal", "focus":false}}),
        ),
        (
            vec!["workspace", "create"],
            json!({"id":"cli:workspace:create", "method":"workspace.create", "params":{"focus":false}}),
        ),
        (
            vec!["workspace", "get", "workspace-spelling"],
            json!({"id":"cli:workspace:get", "method":"workspace.get", "params":{"workspace_id":"workspace-spelling"}}),
        ),
        (
            vec!["workspace", "focus", "w8"],
            json!({"id":"cli:workspace:focus", "method":"workspace.focus", "params":{"workspace_id":"w8"}}),
        ),
        (
            vec!["workspace", "rename", "w3", "two", "words"],
            json!({"id":"cli:workspace:rename", "method":"workspace.rename", "params":{"workspace_id":"w3", "label":"two words"}}),
        ),
        (
            vec!["workspace", "close", "w4"],
            json!({"id":"cli:workspace:close", "method":"workspace.close", "params":{"workspace_id":"w4"}}),
        ),
    ];
    for (args, expected) in cases {
        fixture.assert_case(&args, expected);
    }
    for args in [
        vec!["workspace", "list", "extra"],
        vec!["workspace", "create", "--env", "X=y"],
        vec!["workspace", "get"],
        vec!["workspace", "focus", "w1", "extra"],
        vec!["workspace", "rename", "w1"],
        vec!["workspace", "close"],
    ] {
        fixture.assert_invalid(&args);
    }
}

#[test]
fn runtime_tab_cli_contract() {
    let fixture = Fixture::new();
    let cases = [
        (
            vec!["tab", "list", "--workspace", "w5"],
            json!({"id":"cli:tab:list", "method":"tab.list", "params":{"workspace_id":"w5"}}),
        ),
        (
            vec!["tab", "list"],
            json!({"id":"cli:tab:list", "method":"tab.list", "params":{}}),
        ),
        (
            vec![
                "tab",
                "create",
                "--workspace",
                "w6",
                "--cwd",
                "~/literal",
                "--label",
                "two words",
                "--no-focus",
                "--focus",
            ],
            json!({"id":"cli:tab:create", "method":"tab.create", "params":{"workspace_id":"w6", "cwd":"~/literal", "label":"two words", "focus":true}}),
        ),
        (
            vec![
                "tab",
                "create",
                "--cwd",
                "relative/path",
                "--focus",
                "--no-focus",
            ],
            json!({"id":"cli:tab:create", "method":"tab.create", "params":{"cwd":"relative/path", "focus":false}}),
        ),
        (
            vec!["tab", "create"],
            json!({"id":"cli:tab:create", "method":"tab.create", "params":{"focus":false}}),
        ),
        (
            vec!["tab", "get", "tab-spelling"],
            json!({"id":"cli:tab:get", "method":"tab.get", "params":{"tab_id":"tab-spelling"}}),
        ),
        (
            vec!["tab", "focus", "w8:t2"],
            json!({"id":"cli:tab:focus", "method":"tab.focus", "params":{"tab_id":"w8:t2"}}),
        ),
        (
            vec!["tab", "rename", "w3:t4", "two", "words"],
            json!({"id":"cli:tab:rename", "method":"tab.rename", "params":{"tab_id":"w3:t4", "label":"two words"}}),
        ),
        (
            vec!["tab", "close", "w4:t9"],
            json!({"id":"cli:tab:close", "method":"tab.close", "params":{"tab_id":"w4:t9"}}),
        ),
    ];
    for (args, expected) in cases {
        fixture.assert_case(&args, expected);
    }
    for args in [
        vec!["tab", "list", "--workspace"],
        vec!["tab", "create", "--env", "X=y"],
        vec!["tab", "get"],
        vec!["tab", "focus", "t1", "extra"],
        vec!["tab", "rename", "t1"],
        vec!["tab", "close"],
    ] {
        fixture.assert_invalid(&args);
    }
}

#[test]
fn runtime_worktree_cli_contract() {
    let fixture = Fixture::new();
    let cases = [
        (
            vec!["worktree", "list", "--cwd", "relative/repo", "--json"],
            json!({"id":"cli:worktree:list", "method":"worktree.list", "params":{"cwd":fixture.base.join("relative/repo")}}),
        ),
        (
            vec!["worktree", "list", "--workspace", "w4"],
            json!({"id":"cli:worktree:list", "method":"worktree.list", "params":{"workspace_id":"w4"}}),
        ),
        (
            vec![
                "worktree",
                "create",
                "--cwd",
                "~/repo",
                "--branch",
                "feature/new",
                "--base",
                "base/ref",
                "--path",
                "relative/tree",
                "--label",
                "two words",
                "--no-focus",
                "--focus",
                "--json",
            ],
            json!({"id":"cli:worktree:create", "method":"worktree.create", "params":{"cwd":fixture.base.join("home/repo"), "branch":"feature/new", "base":"base/ref", "path":fixture.base.join("relative/tree"), "label":"two words", "focus":true}}),
        ),
        (
            vec![
                "worktree",
                "create",
                "--workspace",
                "w7",
                "--focus",
                "--no-focus",
            ],
            json!({"id":"cli:worktree:create", "method":"worktree.create", "params":{"workspace_id":"w7", "focus":false}}),
        ),
        (
            vec![
                "worktree",
                "open",
                "--workspace",
                "w8",
                "--path",
                "~/tree",
                "--label",
                "two words",
                "--no-focus",
                "--focus",
                "--json",
            ],
            json!({"id":"cli:worktree:open", "method":"worktree.open", "params":{"workspace_id":"w8", "path":fixture.base.join("home/tree"), "label":"two words", "focus":true}}),
        ),
        (
            vec![
                "worktree",
                "open",
                "--cwd",
                "relative/repo",
                "--branch",
                "feature/old",
                "--focus",
                "--no-focus",
            ],
            json!({"id":"cli:worktree:open", "method":"worktree.open", "params":{"cwd":fixture.base.join("relative/repo"), "branch":"feature/old", "focus":false}}),
        ),
        (
            vec![
                "worktree",
                "remove",
                "--workspace",
                "w4",
                "--force",
                "--json",
            ],
            json!({"id":"cli:worktree:remove", "method":"worktree.remove", "params":{"workspace_id":"w4", "force":true}}),
        ),
        (
            vec!["worktree", "remove", "--workspace", "w5"],
            json!({"id":"cli:worktree:remove", "method":"worktree.remove", "params":{"workspace_id":"w5", "force":false}}),
        ),
    ];
    for (args, expected) in cases {
        fixture.assert_case(&args, expected);
    }
    for args in [
        vec!["worktree", "list", "--workspace", "w1", "--cwd", "path"],
        vec!["worktree", "create", "--workspace", "w1", "--cwd", "path"],
        vec!["worktree", "open"],
        vec!["worktree", "open", "--path", "path", "--branch", "main"],
        vec!["worktree", "remove", "--force"],
    ] {
        fixture.assert_invalid(&args);
    }
}

#[test]
fn runtime_pane_focus_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(&["pane", "focus", "--direction", "left", "--pane", "chosen:p2"], json!({"id":"cli:pane:focus", "method":"pane.focus_direction", "params":{"pane_id":"chosen:p2", "direction":"left"}}));
    fixture.assert_case(&["pane", "focus", "--direction", "up", "--pane", "discarded:p1", "--current"], json!({"id":"cli:pane:focus", "method":"pane.focus_direction", "params":{"direction":"up"}}));
    fixture.assert_invalid(&["pane", "focus", "--direction", "diagonal"]);
}

#[test]
fn runtime_pane_resize_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(&["pane", "resize", "--direction", "down", "--amount", "0.125", "--pane", "chosen:p3"], json!({"id":"cli:pane:resize", "method":"pane.resize", "params":{"pane_id":"chosen:p3", "direction":"down", "amount":0.125}}));
    fixture.assert_case(
        &["pane", "resize", "--current", "--direction", "right"],
        json!({"id":"cli:pane:resize", "method":"pane.resize", "params":{"direction":"right"}}),
    );
    fixture.assert_invalid(&["pane", "resize", "--direction", "right", "--amount", "NaN"]);
}

#[test]
fn runtime_pane_zoom_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(&["pane", "zoom", "chosen:p4", "--on"], json!({"id":"cli:pane:zoom", "method":"pane.zoom", "params":{"pane_id":"chosen:p4", "mode":"on"}}));
    fixture.assert_case(
        &["pane", "zoom", "--current", "--off"],
        json!({"id":"cli:pane:zoom", "method":"pane.zoom", "params":{"mode":"off"}}),
    );
    fixture.assert_case(&["pane", "zoom", "discarded:p1", "--pane", "chosen:p5"], json!({"id":"cli:pane:zoom", "method":"pane.zoom", "params":{"pane_id":"chosen:p5", "mode":"toggle"}}));
    fixture.assert_invalid(&["pane", "zoom", "--on", "--off"]);
}

#[test]
fn runtime_pane_rename_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(&["pane", "rename", "chosen:p6", "two", "words"], json!({"id":"cli:pane:rename", "method":"pane.rename", "params":{"pane_id":"chosen:p6", "label":"two words"}}));
    fixture.assert_case(
        &["pane", "rename", "chosen:p7", "--clear"],
        json!({"id":"cli:pane:rename", "method":"pane.rename", "params":{"pane_id":"chosen:p7"}}),
    );
    fixture.assert_invalid(&["pane", "rename", "chosen:p6"]);
}

#[test]
fn runtime_pane_split_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(&["pane", "split", "discarded:p1", "--current", "--direction", "down", "--ratio", "0.25", "--cwd", "~/literal", "--no-focus", "--focus"], json!({"id":"cli:pane:split", "method":"pane.split", "params":{"target_pane_id":"caller:p9", "direction":"down", "ratio":0.25, "cwd":"~/literal", "focus":true}}));
    fixture.assert_case(&["pane", "split", "--current", "--pane", "chosen:p8", "--direction", "right", "--focus", "--no-focus"], json!({"id":"cli:pane:split", "method":"pane.split", "params":{"target_pane_id":"chosen:p8", "direction":"right", "focus":false}}));
    fixture.assert_case(&["pane", "split", "--direction", "right"], json!({"id":"cli:pane:split", "method":"pane.split", "params":{"direction":"right", "focus":false}}));
    fixture.assert_case(&["pane", "split", "positional:p4", "--direction", "down"], json!({"id":"cli:pane:split", "method":"pane.split", "params":{"target_pane_id":"positional:p4", "direction":"down", "focus":false}}));
    fixture.assert_invalid(&["pane", "split", "--direction", "right", "--ratio", "inf"]);
    fixture.assert_invalid(&["pane", "split", "--direction", "right", "--env", "X=y"]);
}

#[test]
fn runtime_pane_swap_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(&["pane", "swap", "--source-pane", "source:p3", "--target-pane", "target:p8"], json!({"id":"cli:pane:swap", "method":"pane.swap", "params":{"source_pane_id":"source:p3", "target_pane_id":"target:p8"}}));
    fixture.assert_case(&["pane", "swap", "--pane", "chosen:p9", "--direction", "right"], json!({"id":"cli:pane:swap", "method":"pane.swap", "params":{"pane_id":"chosen:p9", "direction":"right"}}));
    fixture.assert_case(
        &["pane", "swap", "--current", "--direction", "down"],
        json!({"id":"cli:pane:swap", "method":"pane.swap", "params":{"direction":"down"}}),
    );
    fixture.assert_invalid(&["pane", "swap", "--source-pane", "source:p3"]);
}

#[test]
fn runtime_pane_move_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(&["pane", "move", "source:p1", "--tab", "w7:t2", "--target-pane", "target:p3", "--split", "down", "--ratio", "0.75", "--no-focus"], json!({"id":"cli:pane:move", "method":"pane.move", "params":{"pane_id":"source:p1", "destination":{"type":"tab", "tab_id":"w7:t2", "target_pane_id":"target:p3", "split":"down", "ratio":0.75}, "focus":false}}));
    fixture.assert_case(&["pane", "move", "source:p2", "--new-tab", "--workspace", "w8", "--label", "new tab"], json!({"id":"cli:pane:move", "method":"pane.move", "params":{"pane_id":"source:p2", "destination":{"type":"new_tab", "workspace_id":"w8", "label":"new tab"}, "focus":true}}));
    fixture.assert_case(&["pane", "move", "source:p3", "--new-workspace", "--label", "new workspace", "--tab-label", "new tab", "--no-focus", "--focus"], json!({"id":"cli:pane:move", "method":"pane.move", "params":{"pane_id":"source:p3", "destination":{"type":"new_workspace", "label":"new workspace", "tab_label":"new tab"}, "focus":true}}));
    fixture.assert_invalid(&["pane", "move", "source:p1", "--new-workspace", "--new-tab"]);
}

#[test]
fn runtime_pane_close_contract() {
    let fixture = Fixture::new();
    fixture.assert_case(
        &["pane", "close", "chosen:p10"],
        json!({"id":"cli:pane:close", "method":"pane.close", "params":{"pane_id":"chosen:p10"}}),
    );
    fixture.assert_invalid(&["pane", "close", "chosen:p10", "extra"]);
}
