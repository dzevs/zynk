//! zynk fork (ADR 0002) M3a integration tests: the native receipt feature
//! (`zynk.message_received`) records an authoritative "received" delivery event
//! against a previously-submitted message, both over the raw socket and via the
//! `zynk zynk message-received` CLI shim, with HONEST receipt semantics.
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
        "/tmp/zynk-receipt-test-{}-{nanos}",
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

/// Report a `pi` agent via `pane.report_agent` WITH a session ref (the production
/// path: the pi asset reports state + `agent_session_id`). Because `pi` is NOT
/// reserved-native, a stateful report routes to `set_hook_authority_with_session_ref`,
/// so the pane BOTH surfaces `agent_session.agent == "pi"` (the send-side header gate)
/// AND has hook authority (the receipt-side identity gate). (`pane.report_agent_session`
/// alone would set a persisted session only — no hook authority — so its receipt would
/// be rejected.) Verified via `pane.get`.
fn report_pi_agent_session(socket_path: &Path, pane_id: &str, session_id: &str) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"sess\",\"method\":\"pane.report_agent\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"zynk:pi\",\"agent\":\"pi\",\"state\":\"idle\",\"agent_session_id\":\"{session_id}\"}}}}"
        ),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent_session: {response}"
    );
    let got = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"get\",\"method\":\"pane.get\",\"params\":{{\"pane_id\":\"{pane_id}\"}}}}"
        ),
    );
    assert_eq!(
        got.pointer("/result/pane/agent_session/agent")
            .and_then(Value::as_str),
        Some("pi"),
        "pane.get must surface agent_session.agent == pi (got {got})"
    );
}

fn pane_recent_text(socket_path: &Path, pane_id: &str) -> String {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"read\",\"method\":\"pane.read\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"recent\",\"lines\":50}}}}"
        ),
    );
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

/// The most recent delivery event for `message_id`, as `(event_type, proof_source)`.
fn latest_event(fixture: &Fixture, message_id: &str) -> (String, String) {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let row = sqlx::query(
            "SELECT event_type, proof_source FROM delivery_events WHERE message_id = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(message_id)
        .fetch_one(&mut conn)
        .await
        .unwrap();
        (
            row.try_get("event_type").unwrap(),
            row.try_get("proof_source").unwrap(),
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

/// Submit a message to the `codex` agent pane and return the F4 outcome. Asserts
/// the send was `submitted` (the precondition for any subsequent receipt).
fn agent_send_codex(fixture: &Fixture, body: &str) -> Value {
    let out = run_cli(
        fixture,
        None,
        &["agent", "send", "codex", "--type", "review", "--", body],
    );
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "agent send exit 0: stderr={}", out.stderr);
    assert_eq!(
        v["delivery_status"], "submitted",
        "agent send must submit: {v}"
    );
    v
}

/// Build the raw `zynk.message_received` socket request from an F4 send outcome,
/// receipted by `receiver_pane`.
fn receipt_request(sent: &Value, receiver_pane: &str) -> String {
    let message_id = sent["message_id"].as_str().expect("message_id");
    let conversation_id = sent["conversation_id"].as_str().expect("conversation_id");
    let conversation_seq = sent["conversation_seq"].as_i64().expect("conversation_seq");
    let runtime_session_id = sent["runtime_session_id"]
        .as_str()
        .expect("runtime_session_id");
    let socket_namespace = sent["socket_namespace"].as_str().expect("socket_namespace");
    format!(
        "{{\"id\":\"rcpt\",\"method\":\"zynk.message_received\",\"params\":{{\
\"pane_id\":\"{receiver_pane}\",\
\"message_id\":\"{message_id}\",\
\"conversation_id\":\"{conversation_id}\",\
\"conversation_seq\":{conversation_seq},\
\"runtime_session_id\":\"{runtime_session_id}\",\
\"socket_namespace\":\"{socket_namespace}\"}}}}"
    )
}

#[test]
fn receipt_records_received_via_raw_socket() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "receipt-socket");
    report_agent(&fixture.socket_path, &pane, "codex");

    let sent = agent_send_codex(&fixture, "socket receipt body");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    let response = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    assert!(
        response.get("error").is_none(),
        "receipt over socket failed: {response}"
    );
    assert_eq!(
        response["result"]["type"], "zynk_message_received",
        "{response}"
    );
    assert_eq!(
        response["result"]["receipt_status"], "received",
        "{response}"
    );
    assert_eq!(
        response["result"]["delivery_status"], "received",
        "{response}"
    );
    assert_eq!(response["result"]["receiver_pane_id"], pane, "{response}");
    assert_eq!(
        response["result"]["receiver_agent_label"], "codex",
        "{response}"
    );

    assert_eq!(
        latest_event(&fixture, &message_id),
        ("received".to_string(), "integration".to_string()),
        "latest delivery event must be received/integration"
    );

    fixture.cleanup();
}

#[test]
fn receipt_via_cli_shim_matches_socket() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "receipt-cli");
    report_agent(&fixture.socket_path, &pane, "codex");

    let sent = agent_send_codex(&fixture, "cli shim receipt body");
    let mid = sent["message_id"].as_str().expect("message_id").to_string();
    let cid = sent["conversation_id"]
        .as_str()
        .expect("conversation_id")
        .to_string();
    let seq = sent["conversation_seq"].as_i64().expect("conversation_seq");
    let rt = sent["runtime_session_id"]
        .as_str()
        .expect("runtime_session_id")
        .to_string();
    let sock = sent["socket_namespace"]
        .as_str()
        .expect("socket_namespace")
        .to_string();
    let seq_str = seq.to_string();

    let out = run_cli(
        &fixture,
        None,
        &[
            "zynk",
            "message-received",
            "--pane-id",
            &pane,
            "--message-id",
            &mid,
            "--conversation-id",
            &cid,
            "--conversation-seq",
            &seq_str,
            "--runtime-session-id",
            &rt,
            "--socket-namespace",
            &sock,
        ],
    );

    assert_eq!(out.code, 0, "CLI receipt exit 0: stderr={}", out.stderr);
    let v = parse_outcome(&out);
    assert!(v.get("error").is_none(), "CLI receipt error: {v}");
    assert_eq!(v["result"]["type"], "zynk_message_received", "{v}");
    assert_eq!(v["result"]["receipt_status"], "received", "{v}");
    assert_eq!(v["result"]["delivery_status"], "received", "{v}");
    assert_eq!(v["result"]["receiver_agent_label"], "codex", "{v}");

    assert_eq!(
        latest_event(&fixture, &mid),
        ("received".to_string(), "integration".to_string()),
        "CLI shim must record received/integration just like the socket"
    );

    fixture.cleanup();
}

#[test]
fn duplicate_receipt_returns_already_received() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "receipt-dup");
    report_agent(&fixture.socket_path, &pane, "codex");

    let sent = agent_send_codex(&fixture, "duplicate receipt body");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();
    let request = receipt_request(&sent, &pane);

    let first = send_json(&fixture.socket_path, &request);
    assert!(
        first.get("error").is_none(),
        "first receipt failed: {first}"
    );
    assert_eq!(
        first["result"]["receipt_status"], "received",
        "first receipt is fresh: {first}"
    );
    assert_eq!(
        latest_event(&fixture, &message_id),
        ("received".to_string(), "integration".to_string()),
        "first receipt records received/integration"
    );

    let second = send_json(&fixture.socket_path, &request);
    assert!(
        second.get("error").is_none(),
        "duplicate receipt failed: {second}"
    );
    assert_eq!(
        second["result"]["receipt_status"], "already_received",
        "second receipt is idempotent: {second}"
    );

    // The idempotent re-receipt must NOT append a new event — latest stays the
    // single received/integration row.
    assert_eq!(
        latest_event(&fixture, &message_id),
        ("received".to_string(), "integration".to_string()),
        "duplicate receipt must not overwrite the received event"
    );

    fixture.cleanup();
}

#[test]
fn receipt_from_pane_without_hook_authority_is_unverified() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // The codex pane has hook authority and receives the message.
    let codex_pane = create_root_pane(&fixture.socket_path, "receipt-codex");
    report_agent(&fixture.socket_path, &codex_pane, "codex");
    // A SECOND plain pane with NO report_agent → no hook-authoritative identity.
    let plain_pane = create_root_pane(&fixture.socket_path, "receipt-plain");

    let sent = agent_send_codex(&fixture, "unverified receiver body");

    // Receipting from the plain pane (no hook authority) must be rejected before
    // any DB write.
    let response = send_json(&fixture.socket_path, &receipt_request(&sent, &plain_pane));
    assert!(
        response.get("result").is_none(),
        "unverified receiver must not succeed: {response}"
    );
    assert_eq!(
        response["error"]["code"], "receiver_identity_unverified",
        "{response}"
    );
    // Honest error envelope: no `context` field.
    assert!(
        response["error"].get("context").is_none(),
        "error body must have no context field: {response}"
    );

    fixture.cleanup();
}

#[test]
fn report_agent_and_send_alone_do_not_create_received() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "receipt-no-promote");
    report_agent(&fixture.socket_path, &pane, "codex");

    let sent = agent_send_codex(&fixture, "submitted but not received body");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    // No `zynk.message_received` call: the latest event must remain `submitted`
    // (a submit must NOT auto-promote to received).
    assert_eq!(
        latest_event(&fixture, &message_id).0,
        "submitted",
        "submit without an explicit receipt must not become received"
    );

    fixture.cleanup();
}

#[test]
fn negative_receiver_seq_is_invalid_params() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "receipt-badseq");
    report_agent(&fixture.socket_path, &pane, "codex");
    let sent = agent_send_codex(&fixture, "negative receiver_seq body");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    // A present-but-non-positive `receiver_seq` is rejected with `invalid_params`
    // (plan D2) before any receipt is recorded.
    let request = format!(
        "{{\"id\":\"rcpt\",\"method\":\"zynk.message_received\",\"params\":{{\
\"pane_id\":\"{pane}\",\
\"message_id\":\"{message_id}\",\
\"conversation_id\":\"{cid}\",\
\"conversation_seq\":{seq},\
\"runtime_session_id\":\"{rt}\",\
\"socket_namespace\":\"{sock}\",\
\"receiver_seq\":-1}}}}",
        cid = sent["conversation_id"].as_str().unwrap(),
        seq = sent["conversation_seq"].as_i64().unwrap(),
        rt = sent["runtime_session_id"].as_str().unwrap(),
        sock = sent["socket_namespace"].as_str().unwrap(),
    );
    let response = send_json(&fixture.socket_path, &request);
    assert!(
        response.get("result").is_none(),
        "negative receiver_seq must not succeed: {response}"
    );
    assert_eq!(response["error"]["code"], "invalid_params", "{response}");

    // The message must remain `submitted` — a rejected receipt records nothing.
    assert_eq!(
        latest_event(&fixture, &message_id).0,
        "submitted",
        "rejected receipt must not advance the delivery state"
    );

    fixture.cleanup();
}

/// DORMANT-CAPABILITY END-TO-END (deterministic; no live pi). The send path NEVER
/// auto-fires receipt — a `pane run` to a pi pane PREPENDS the agent-visible header but
/// leaves `delivery_status` at `submitted`. The server-authoritative
/// `zynk.message_received` stays a DORMANT capability: a DIRECT call with valid
/// hook-authority + the F4 IDs still advances the message to `received`/`integration`
/// exactly once, idempotently. Nothing auto-fires it on send (the old wire-parsing pi
/// receiver that did is removed). The header observation is NEVER proof — only this
/// validated server event is.
#[test]
fn message_received_api_remains_dormant_capability() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "dormant-pi-e2e");
    report_pi_agent_session(&fixture.socket_path, &pane, "sess-dormant-pi");

    let out = run_cli(
        &fixture,
        None,
        &["pane", "run", &pane, "--", "zbodysentinel", "dormant"],
    );
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "pane run exit 0: stderr={}", out.stderr);
    assert_eq!(sent["delivery_status"], "submitted", "{sent}");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    // The agent-visible header was prepended into the delivered pane text (uniform).
    assert!(
        wait_for_pane_text(
            &fixture.socket_path,
            &pane,
            "╭─ Zynk message",
            Duration::from_secs(5),
        ),
        "pi must receive the visible header by default; pane: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );

    // The send did NOT auto-advance to received — the header is NOT receipt proof.
    assert_eq!(
        latest_event(&fixture, &message_id).0,
        "submitted",
        "the visible header must NOT auto-promote the message to received"
    );

    // The DORMANT server-authoritative receipt API still works when explicitly called
    // with valid hook-authority + the F4 IDs → exactly one received/integration event.
    let response = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    assert!(
        response.get("error").is_none(),
        "explicit receipt failed: {response}"
    );
    assert_eq!(
        response["result"]["receipt_status"], "received",
        "{response}"
    );
    assert_eq!(
        latest_event(&fixture, &message_id),
        ("received".to_string(), "integration".to_string()),
        "explicit dormant-capability receipt must record received/integration"
    );

    // Idempotent: a duplicate receipt is already_received, no second event.
    let dup = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    assert_eq!(dup["result"]["receipt_status"], "already_received", "{dup}");
    assert_eq!(
        latest_event(&fixture, &message_id),
        ("received".to_string(), "integration".to_string()),
    );

    fixture.cleanup();
}
