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
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if done.load(Ordering::Acquire)
                                || started.elapsed() >= Duration::from_secs(3)
                            {
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
                let request = serde_json::from_str(&line).unwrap();
                writeln!(stream, "{response}").unwrap();
                Some(request)
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
