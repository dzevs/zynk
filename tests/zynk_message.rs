//! zynk fork (ADR 0002) M1 integration tests: the three `zynk` send commands
//! (`agent send`, `pane run`, `pane send-text`) drive zynk's native submit and
//! print the F4 `SendOutcome` JSON on stdout with HONEST delivery semantics.
//!
//! These run against an ISOLATED dev server spawned under `/tmp` with an
//! isolated `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`/`ZYNK_SOCKET_PATH`; they never
//! touch the live zynk runtime. The CLI binary (still named `zynk`) is driven
//! as a subprocess so the assertions observe the real stdout + exit code.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Row, SqliteConnection};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_zynk_pid,
    unregister_spawned_zynk_pid, wait_for_socket,
};

// Serialize the whole suite: each test spawns its own server, but they share the
// global PID/runtime registries and process-wide env scrubbing.
fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_base() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/zynk-message-test-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedZynk {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedZynk {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            unregister_spawned_zynk_pid(Some(pid));
        }
    }
}

/// A fully-isolated server + its runtime paths. Dropping it (via `cleanup`)
/// kills the server and removes the base dir.
struct Fixture {
    base: PathBuf,
    config_home: PathBuf,
    runtime_dir: PathBuf,
    socket_path: PathBuf,
    sqlite_home: PathBuf,
    server: Option<SpawnedZynk>,
}

impl Fixture {
    fn cleanup(mut self) {
        if let Some(server) = self.server.take() {
            drop(server);
        }
        cleanup_test_base(&self.base);
    }
}

fn app_dir() -> &'static str {
    if cfg!(debug_assertions) {
        "zynk-dev"
    } else {
        "zynk"
    }
}

fn spawn_fixture() -> Fixture {
    let base = unique_base();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = base.join("api.sock");
    let sqlite_home = base.join("sqlite");

    fs::create_dir_all(config_home.join(app_dir())).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::create_dir_all(&sqlite_home).unwrap();
    register_runtime_dir(&runtime_dir);
    fs::write(
        config_home.join(app_dir()).join("config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let server = spawn_server_process(&config_home, &runtime_dir, &socket_path, &sqlite_home);

    Fixture {
        base,
        config_home,
        runtime_dir,
        socket_path,
        sqlite_home,
        server: Some(server),
    }
}

fn spawn_server_process(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
    sqlite_home: &Path,
) -> SpawnedZynk {
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
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("ZYNK_SOCKET_PATH", socket_path);
    cmd.env("ZYNK_SQLITE_HOME", sqlite_home);
    cmd.env_remove("ZYNK_HOME");
    cmd.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("ZYNK_ENV");
    cmd.env_remove("ZYNK_PANE_ID");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_zynk_pid(child.process_id());
    drop(pair.slave);

    let server = SpawnedZynk {
        _master: pair.master,
        child,
    };

    wait_for_socket(socket_path, Duration::from_secs(10));
    server
}

fn send_json(socket_path: &Path, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("connect API socket");
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("response is valid JSON")
}

/// Create a workspace + focused root pane, returning the root pane id.
fn create_root_pane(socket_path: &Path, label: &str) -> String {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"ws\",\"method\":\"workspace.create\",\"params\":{{\"label\":\"{label}\",\"focus\":true}}}}"
        ),
    );
    assert!(
        response.get("error").is_none(),
        "workspace.create: {response}"
    );
    response
        .pointer("/result/root_pane/pane_id")
        .and_then(Value::as_str)
        .expect("root pane id")
        .to_string()
}

/// Split the given pane to get a second, independent pane id (for shell-pane tests
/// and for a 2nd agent under the same label).
fn split_pane(socket_path: &Path, pane_id: &str) -> String {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"split\",\"method\":\"pane.split\",\"params\":{{\"pane_id\":\"{pane_id}\",\"direction\":\"right\"}}}}"
        ),
    );
    assert!(response.get("error").is_none(), "pane.split: {response}");
    response
        .pointer("/result/root_pane/pane_id")
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .pointer("/result/pane/pane_id")
                .and_then(Value::as_str)
        })
        .expect("split pane id")
        .to_string()
}

/// Register a pane as an agent terminal under `label` so `agent.get`/`AgentGet`
/// resolves it (sets the hook authority → `is_agent_terminal()` is true).
fn report_agent(socket_path: &Path, pane_id: &str, label: &str) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"report\",\"method\":\"pane.report_agent\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"hook\",\"agent\":\"{label}\",\"state\":\"idle\"}}}}"
        ),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent: {response}"
    );
}

fn pane_recent_text(socket_path: &Path, pane_id: &str) -> String {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"read\",\"method\":\"pane.read\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"recent\",\"lines\":50}}}}"
        ),
    );
    assert!(response.get("error").is_none(), "pane.read: {response}");
    response
        .pointer("/result/read/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn wait_for_pane_text(socket_path: &Path, pane_id: &str, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pane_recent_text(socket_path, pane_id).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    false
}

struct CliOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Drive the `zynk` CLI binary against this fixture's isolated socket. When
/// `zynk_pane_id` is `Some`, set `ZYNK_PANE_ID`; when `None`, remove it.
fn run_cli(fixture: &Fixture, zynk_pane_id: Option<&str>, args: &[&str]) -> CliOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command.args(args);
    command.env("XDG_CONFIG_HOME", &fixture.config_home);
    command.env("XDG_RUNTIME_DIR", &fixture.runtime_dir);
    command.env("ZYNK_SOCKET_PATH", &fixture.socket_path);
    command.env("ZYNK_SQLITE_HOME", &fixture.sqlite_home);
    command.env_remove("ZYNK_HOME");
    command.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    command.env_remove("ZYNK_ENV");
    match zynk_pane_id {
        Some(id) => {
            command.env("ZYNK_PANE_ID", id);
        }
        None => {
            command.env_remove("ZYNK_PANE_ID");
        }
    }
    let output = command.output().expect("run zynk CLI");
    CliOutput {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

#[derive(Debug)]
struct PersistedSnapshot {
    conversation_id: String,
    conversation_seq: i64,
    body_hash: String,
    runtime_session_id: String,
    socket_namespace: String,
    event_type: String,
    proof_source: String,
    fts_hits: i64,
}

fn db_path(fixture: &Fixture) -> PathBuf {
    fixture.sqlite_home.join("zynk.db")
}

fn sqlite_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn open_test_db(fixture: &Fixture) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(db_path(fixture))
            .create_if_missing(false),
    )
    .await
    .unwrap()
}

fn load_persisted_snapshot(
    fixture: &Fixture,
    message_id: &str,
    fts_query: &str,
) -> PersistedSnapshot {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let message = sqlx::query(
            "SELECT conversation_id, conversation_seq, body_hash, runtime_session_id, socket_namespace FROM messages WHERE id = ?",
        )
        .bind(message_id)
        .fetch_one(&mut conn)
        .await
        .unwrap();
        let event = sqlx::query(
            "SELECT event_type, proof_source FROM delivery_events WHERE message_id = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(message_id)
        .fetch_one(&mut conn)
        .await
        .unwrap();
        let fts = sqlx::query("SELECT count(*) AS c FROM messages_fts WHERE messages_fts MATCH ?")
            .bind(fts_query)
            .fetch_one(&mut conn)
            .await
            .unwrap();
        PersistedSnapshot {
            conversation_id: message.try_get("conversation_id").unwrap(),
            conversation_seq: message.try_get("conversation_seq").unwrap(),
            body_hash: message.try_get("body_hash").unwrap(),
            runtime_session_id: message.try_get("runtime_session_id").unwrap(),
            socket_namespace: message.try_get("socket_namespace").unwrap(),
            event_type: event.try_get("event_type").unwrap(),
            proof_source: event.try_get("proof_source").unwrap(),
            fts_hits: fts.try_get("c").unwrap(),
        }
    })
}

fn message_count(fixture: &Fixture) -> i64 {
    if !db_path(fixture).exists() {
        return 0;
    }
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        sqlx::query("SELECT count(*) AS c FROM messages")
            .fetch_one(&mut conn)
            .await
            .unwrap()
            .try_get("c")
            .unwrap()
    })
}

fn delete_delivery_events(fixture: &Fixture, message_id: &str) {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        sqlx::query("DELETE FROM delivery_events WHERE message_id = ?")
            .bind(message_id)
            .execute(&mut conn)
            .await
            .unwrap();
    });
}

fn latest_event(fixture: &Fixture, message_id: &str) -> (String, String, String) {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let row = sqlx::query(
            "SELECT event_type, proof_source, payload_json FROM delivery_events WHERE message_id = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(message_id)
        .fetch_one(&mut conn)
        .await
        .unwrap();
        (
            row.try_get("event_type").unwrap(),
            row.try_get("proof_source").unwrap(),
            row.try_get("payload_json").unwrap(),
        )
    })
}

fn parse_outcome(out: &CliOutput) -> Value {
    let line = out
        .stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| {
            panic!(
                "no JSON line on stdout: stdout={:?} stderr={:?}",
                out.stdout, out.stderr
            )
        });
    serde_json::from_str(line).unwrap_or_else(|e| panic!("stdout JSON parse failed ({e}): {line}"))
}

#[test]
fn agent_send_to_real_agent_submits_and_returns_f4_ok() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "agent-ok");
    report_agent(&fixture.socket_path, &pane, "codex");

    let out = run_cli(
        &fixture,
        None,
        &[
            "agent", "send", "codex", "--type", "review", "--", "hello", "there",
        ],
    );
    let v = parse_outcome(&out);

    assert_eq!(
        out.code,
        0,
        "exit 0 on ok: {out_stderr}",
        out_stderr = out.stderr
    );
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "agent send", "{v}");
    assert_eq!(v["target_resolution"], "resolved", "{v}");
    assert_eq!(v["delivery_status"], "submitted", "{v}");
    assert_eq!(v["proof"]["proof_source"], "pane.send_input", "{v}");
    assert_eq!(v["type"], "review", "{v}");
    let message_id = v["message_id"].as_str().expect("message_id");
    assert!(!message_id.is_empty(), "message_id non-empty: {v}");
    let snapshot = load_persisted_snapshot(&fixture, message_id, "hello");
    assert_eq!(v["conversation_id"], snapshot.conversation_id, "{v}");
    assert_eq!(v["conversation_seq"], snapshot.conversation_seq, "{v}");
    assert_eq!(v["body_hash"], snapshot.body_hash, "{v}");
    assert_eq!(snapshot.body_hash.len(), 64, "sha256 hex body hash");
    assert_eq!(v["runtime_session_id"], snapshot.runtime_session_id, "{v}");
    assert_eq!(v["socket_namespace"], snapshot.socket_namespace, "{v}");
    assert_eq!(snapshot.event_type, "submitted");
    assert_eq!(snapshot.proof_source, "pane.send_input");
    assert!(
        snapshot.fts_hits >= 1,
        "FTS must see committed body immediately"
    );

    // The text was SUBMITTED into the target pane (rendered, not a draft).
    assert!(
        wait_for_pane_text(
            &fixture.socket_path,
            &pane,
            "hello there",
            Duration::from_secs(5)
        ),
        "submitted text should render in the target pane; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );

    fixture.cleanup();
}

#[test]
fn agent_send_to_nonexistent_agent_fails_not_found_and_sends_nothing() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // A real shell pane exists, but no agent named `ghost`.
    let pane = create_root_pane(&fixture.socket_path, "agent-missing");

    let out = run_cli(
        &fixture,
        None,
        &["agent", "send", "ghost", "--", "should not arrive"],
    );
    let v = parse_outcome(&out);

    assert_ne!(out.code, 0, "failed send must exit nonzero: {v}");
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(v["target_resolution"], "not_found", "{v}");
    assert_eq!(v["error"]["code"], "target_not_found", "{v}");
    assert!(
        v.get("delivery_status").is_none(),
        "no delivery on failure: {v}"
    );
    assert!(v.get("proof").is_none(), "no proof on failure: {v}");
    assert!(
        v.get("submitted_at").is_none(),
        "no submitted_at on failure: {v}"
    );
    assert_eq!(
        message_count(&fixture),
        0,
        "pre-resolution failure writes no DB row"
    );

    // Nothing was sent anywhere — the only pane has no trace of the message.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !pane_recent_text(&fixture.socket_path, &pane).contains("should not arrive"),
        "a failed send must not deliver text"
    );

    fixture.cleanup();
}

#[test]
fn agent_send_to_ambiguous_label_fails_ambiguous_and_sends_nothing() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // Two distinct panes, both reported under the SAME agent label → ambiguous.
    let pane_a = create_root_pane(&fixture.socket_path, "agent-ambiguous");
    let pane_b = split_pane(&fixture.socket_path, &pane_a);
    report_agent(&fixture.socket_path, &pane_a, "dup");
    report_agent(&fixture.socket_path, &pane_b, "dup");

    let out = run_cli(
        &fixture,
        None,
        &["agent", "send", "dup", "--", "ambiguous body"],
    );
    let v = parse_outcome(&out);

    assert_ne!(out.code, 0, "ambiguous send must exit nonzero: {v}");
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(v["target_resolution"], "ambiguous", "{v}");
    assert_eq!(v["error"]["code"], "agent_target_ambiguous", "{v}");
    assert!(
        v.get("delivery_status").is_none(),
        "no delivery on ambiguous: {v}"
    );
    assert!(v.get("proof").is_none(), "no proof on ambiguous: {v}");

    std::thread::sleep(Duration::from_millis(200));
    for pane in [&pane_a, &pane_b] {
        assert!(
            !pane_recent_text(&fixture.socket_path, pane).contains("ambiguous body"),
            "an ambiguous send must not deliver to any candidate"
        );
    }

    fixture.cleanup();
}

#[test]
fn pane_send_text_drafts_into_a_shell_pane() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "send-text");

    let out = run_cli(
        &fixture,
        None,
        &["pane", "send-text", &pane, "--", "some draft"],
    );
    let v = parse_outcome(&out);

    assert_eq!(out.code, 0, "exit 0: {}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "pane send-text", "{v}");
    assert_eq!(v["delivery_status"], "drafted", "{v}");
    assert_eq!(v["proof"]["proof_source"], "pane.send_text", "{v}");
    let message_id = v["message_id"].as_str().expect("message_id");
    let snapshot = load_persisted_snapshot(&fixture, message_id, "draft");
    assert_eq!(snapshot.event_type, "drafted");
    assert_eq!(snapshot.proof_source, "pane.send_text");
    assert_eq!(snapshot.conversation_seq, 1);
    assert_eq!(v["body_hash"], snapshot.body_hash, "{v}");

    fixture.cleanup();
}

#[test]
fn pane_run_submits_into_a_shell_pane_with_sparser_to() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "run");

    let out = run_cli(&fixture, None, &["pane", "run", &pane, "--", "echo", "hi"]);
    let v = parse_outcome(&out);

    assert_eq!(out.code, 0, "exit 0: {}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "pane run", "{v}");
    assert_eq!(v["delivery_status"], "submitted", "{v}");
    assert_eq!(v["proof"]["proof_source"], "pane.send_input", "{v}");
    let message_id = v["message_id"].as_str().expect("message_id");
    let snapshot = load_persisted_snapshot(&fixture, message_id, "echo");
    assert_eq!(snapshot.event_type, "submitted");
    assert_eq!(snapshot.proof_source, "pane.send_input");
    assert_eq!(v["conversation_seq"], snapshot.conversation_seq, "{v}");
    // A plain shell pane: the `to` party has no agent_session (sparser).
    assert!(
        v["to"].get("agent_session").is_none(),
        "shell-pane target should have no agent_session: {v}"
    );

    fixture.cleanup();
}

#[test]
fn missing_runtime_id_fails_closed_before_writing_pane_or_db() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "missing-runtime");
    let runtime_id_path = fixture.socket_path.parent().unwrap().join("runtime.id");
    fs::remove_file(&runtime_id_path).expect("remove runtime.id");

    let out = run_cli(
        &fixture,
        None,
        &["pane", "send-text", &pane, "--", "must not persist"],
    );
    let v = parse_outcome(&out);
    assert_ne!(out.code, 0, "missing runtime id fails: {v}");
    assert_eq!(v["error"]["code"], "runtime_identity_missing", "{v}");
    assert_eq!(message_count(&fixture), 0, "no DB row without namespace");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!pane_recent_text(&fixture.socket_path, &pane).contains("must not persist"));

    fixture.cleanup();
}

#[test]
fn repeated_sends_to_same_tab_allocate_monotonic_conversation_seq() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "seq");

    let first = parse_outcome(&run_cli(
        &fixture,
        None,
        &["pane", "send-text", &pane, "--", "first seq body"],
    ));
    let second = parse_outcome(&run_cli(
        &fixture,
        None,
        &["pane", "send-text", &pane, "--", "second seq body"],
    ));

    assert_eq!(first["conversation_id"], second["conversation_id"]);
    assert_eq!(first["conversation_seq"], 1);
    assert_eq!(second["conversation_seq"], 2);

    fixture.cleanup();
}

#[test]
fn concurrent_sends_to_same_tab_get_unique_monotonic_conversation_seq() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "concurrent-seq");
    let mut handles = Vec::new();
    for i in 0..4 {
        let config_home = fixture.config_home.clone();
        let runtime_dir = fixture.runtime_dir.clone();
        let socket_path = fixture.socket_path.clone();
        let sqlite_home = fixture.sqlite_home.clone();
        let pane = pane.clone();
        handles.push(std::thread::spawn(move || {
            let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
            let body = format!("concurrent body {i}");
            command.args(["pane", "send-text", pane.as_str(), "--", body.as_str()]);
            command.env("XDG_CONFIG_HOME", &config_home);
            command.env("XDG_RUNTIME_DIR", &runtime_dir);
            command.env("ZYNK_SOCKET_PATH", &socket_path);
            command.env("ZYNK_SQLITE_HOME", &sqlite_home);
            command.env_remove("ZYNK_HOME");
            command.env_remove("ZYNK_CLIENT_SOCKET_PATH");
            command.env_remove("ZYNK_ENV");
            command.env_remove("ZYNK_PANE_ID");
            let output = command.output().expect("run concurrent zynk CLI");
            CliOutput {
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            }
        }));
    }

    let mut seqs = Vec::new();
    let mut conversation_id = None;
    for handle in handles {
        let out = handle.join().expect("thread joins");
        let v = parse_outcome(&out);
        assert_eq!(out.code, 0, "concurrent send ok: {v} stderr={}", out.stderr);
        assert_eq!(v["delivery_status"], "drafted", "{v}");
        if let Some(existing) = &conversation_id {
            assert_eq!(v["conversation_id"], *existing, "same tab conversation");
        } else {
            conversation_id = Some(v["conversation_id"].clone());
        }
        seqs.push(v["conversation_seq"].as_i64().expect("seq"));
    }
    seqs.sort_unstable();
    assert_eq!(seqs, vec![1, 2, 3, 4]);

    fixture.cleanup();
}

#[test]
fn orphaned_message_without_event_recovers_as_failed_system_recovery() {
    let _guard = test_lock();
    let mut fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "recovery");
    let first = parse_outcome(&run_cli(
        &fixture,
        None,
        &["pane", "send-text", &pane, "--", "orphan me"],
    ));
    let orphan_id = first["message_id"]
        .as_str()
        .expect("message_id")
        .to_string();
    delete_delivery_events(&fixture, &orphan_id);

    if let Some(server) = fixture.server.take() {
        drop(server);
    }
    let _ = fs::remove_file(&fixture.socket_path);
    fixture.server = Some(spawn_server_process(
        &fixture.config_home,
        &fixture.runtime_dir,
        &fixture.socket_path,
        &fixture.sqlite_home,
    ));
    let pane_after_restart = create_root_pane(&fixture.socket_path, "recovery-after-restart");

    let second = parse_outcome(&run_cli(
        &fixture,
        None,
        &[
            "pane",
            "send-text",
            &pane_after_restart,
            "--",
            "trigger recovery",
        ],
    ));
    assert_eq!(second["result"], "ok", "{second}");

    let (event_type, proof_source, payload_json) = latest_event(&fixture, &orphan_id);
    assert_eq!(event_type, "failed");
    assert_eq!(proof_source, "system.recovery");
    assert!(payload_json.contains("orphaned_message_without_event"));

    fixture.cleanup();
}

#[test]
fn send_with_zynk_pane_id_unset_still_ok_with_sparse_from() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "no-source");

    // ZYNK_PANE_ID removed: no source pane to resolve. A send is still valid.
    let out = run_cli(
        &fixture,
        None,
        &["pane", "send-text", &pane, "--", "headless draft"],
    );
    let v = parse_outcome(&out);

    assert_eq!(out.code, 0, "exit 0: {}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    // `from` is present but sparse: no source pane/agent metadata.
    assert!(
        v["from"].get("pane").is_none(),
        "no source pane when ZYNK_PANE_ID unset: {v}"
    );
    assert!(
        v["from"].get("agent").is_none(),
        "no source agent when ZYNK_PANE_ID unset: {v}"
    );

    fixture.cleanup();
}

/// Drive the `zynk` CLI against an isolated, fully-scrubbed env where
/// `ZYNK_SOCKET_PATH` points at a path with NO server listening — the transport
/// can never connect. Used to prove a transport failure is reported HONESTLY (a
/// `transport_failed`/`unknown` resolution), not as a `not_found`/`resolved`.
fn run_cli_dead_socket(base: &Path, args: &[&str]) -> CliOutput {
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = base.join("does-not-exist.sock");
    fs::create_dir_all(config_home.join(app_dir())).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    // No server is spawned and no socket file is created: the connect must fail.
    assert!(!socket_path.exists(), "dead socket must not exist");

    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command.args(args);
    command.env("XDG_CONFIG_HOME", &config_home);
    command.env("XDG_RUNTIME_DIR", &runtime_dir);
    command.env("ZYNK_SOCKET_PATH", &socket_path);
    command.env("ZYNK_SQLITE_HOME", base.join("sqlite"));
    command.env_remove("ZYNK_HOME");
    command.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    command.env_remove("ZYNK_ENV");
    command.env_remove("ZYNK_PANE_ID");
    let output = command.output().expect("run zynk CLI");
    CliOutput {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

#[test]
fn agent_send_with_dead_socket_is_transport_failed_unknown_and_sends_nothing() {
    let _guard = test_lock();
    let base = unique_base();
    fs::create_dir_all(&base).unwrap();

    let out = run_cli_dead_socket(
        &base,
        &[
            "agent",
            "send",
            "codex",
            "--",
            "should never reach a server",
        ],
    );
    let v = parse_outcome(&out);

    assert_ne!(out.code, 0, "transport failure must exit nonzero: {v}");
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(
        v["target_resolution"], "unknown",
        "a transport failure is NOT a resolution: {v}"
    );
    assert_eq!(v["error"]["code"], "transport_failed", "{v}");
    assert!(
        v.get("delivery_status").is_none(),
        "no delivery on transport failure: {v}"
    );
    assert!(
        v.get("proof").is_none(),
        "no proof on transport failure: {v}"
    );
    assert!(
        v.get("submitted_at").is_none(),
        "no submitted_at on transport failure: {v}"
    );

    cleanup_test_base(&base);
}

#[test]
fn pane_send_text_with_dead_socket_is_transport_failed_unknown_and_sends_nothing() {
    let _guard = test_lock();
    let base = unique_base();
    fs::create_dir_all(&base).unwrap();

    let out = run_cli_dead_socket(
        &base,
        &[
            "pane",
            "send-text",
            "w0-1",
            "--",
            "should never reach a server",
        ],
    );
    let v = parse_outcome(&out);

    assert_ne!(out.code, 0, "transport failure must exit nonzero: {v}");
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(
        v["target_resolution"], "unknown",
        "a transport failure is NOT a resolution: {v}"
    );
    assert_eq!(v["error"]["code"], "transport_failed", "{v}");
    assert!(
        v.get("delivery_status").is_none(),
        "no delivery on transport failure: {v}"
    );
    assert!(
        v.get("proof").is_none(),
        "no proof on transport failure: {v}"
    );

    cleanup_test_base(&base);
}

#[test]
fn pane_send_text_to_bogus_pane_is_not_found_and_sends_nothing() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // A LIVE server, but the pane id does not exist → the SERVER answers with a
    // logical `pane_not_found` (this is NOT a transport failure).
    let out = run_cli(
        &fixture,
        None,
        &[
            "pane",
            "send-text",
            "w0-nonexistent-9",
            "--",
            "should not arrive",
        ],
    );
    let v = parse_outcome(&out);

    assert_ne!(out.code, 0, "missing pane must exit nonzero: {v}");
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(v["target_resolution"], "not_found", "{v}");
    assert_eq!(v["error"]["code"], "pane_not_found", "{v}");
    assert!(
        v.get("delivery_status").is_none(),
        "no delivery to a missing pane: {v}"
    );
    assert!(v.get("proof").is_none(), "no proof to a missing pane: {v}");

    fixture.cleanup();
}
