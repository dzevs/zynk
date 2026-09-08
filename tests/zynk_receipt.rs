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

/// State-only report (hook authority for `agent`, no session).
fn report_state(socket_path: &Path, pane_id: &str, source: &str, agent: &str) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"state\",\"method\":\"pane.report_agent\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"{source}\",\"agent\":\"{agent}\",\"state\":\"idle\"}}}}"
        ),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent: {response}"
    );
}

/// Session-only report (`pane.report_agent_session`): persists a session for `agent`.
fn report_session(socket_path: &Path, pane_id: &str, source: &str, agent: &str, session: &str) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"sess\",\"method\":\"pane.report_agent_session\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"{source}\",\"agent\":\"{agent}\",\"agent_session_id\":\"{session}\"}}}}"
        ),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent_session: {response}"
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

/// Turn a freshly-created shell pane into a PASSIVE reader (`cat`) before it is reported as an agent
/// target. A plain shell EXECUTES the injected multi-line message: its `command not found` errors are
/// written while the tty is still echoing the paste, and the two streams interleave at byte level, so an
/// asserted substring (the body, a header line) may never be contiguous in the rendered pane — hosted
/// Ubuntu CI rendered the body as `h` / `ello there`. With echo off, `cat` writes each delivered line
/// verbatim and in order, which still exercises real dispatch into an agent-target PTY.
fn start_passive_cat(fixture: &Fixture, pane: &str) {
    // The ready marker is shell-quote-split so the ECHOED command line does not contain it verbatim —
    // only the `printf` output (emitted just before `exec cat`) does.
    run_in_pane_until_ready(
        fixture,
        pane,
        "passive cat",
        "stty -echo 2>/dev/null; printf '__zynk''_cat_ready__\\n'; exec cat",
        "__zynk_cat_ready__",
    );
}

/// Send `command` into a freshly created shell pane and wait until the shell has actually RUN it,
/// proven by `marker` appearing in the pane text.
///
/// WHY a marker rather than the `pane run` exit code: `pane run` reports success as soon as the bytes
/// are written to the pane PTY, which can be before that pane's shell has finished starting. A shell
/// still initializing its line editor discards whatever is already queued on the tty (the editor's
/// `tcsetattr` flushes pending input), so an early `pane run` can vanish leaving an `ok` result and a
/// `submitted` delivery event behind while the command never runs at all. Every command passed here
/// therefore `printf`s `marker` immediately before it starts its long-running process: the marker in the
/// pane text is proof the shell both consumed and ran the line. A window without it means the input was
/// dropped, and the only cure is to send it again — no later wait recovers a command the tty threw away.
///
/// The resend is safe. `marker` stays in the pane text, so a first send that merely landed late is
/// picked up by the next wait; and a resend into an already-started reader costs one echoed line,
/// which cannot fake readiness because the command spells the marker quote-split.
fn run_in_pane_until_ready(fixture: &Fixture, pane: &str, what: &str, command: &str, marker: &str) {
    const ATTEMPTS: usize = 3;
    for attempt in 1..=ATTEMPTS {
        // Resends lead with a newline so a partial line left behind by a half-delivered send is
        // submitted on its own instead of being glued to the front of this command.
        let text = if attempt == 1 {
            command.to_string()
        } else {
            format!("\n{command}")
        };
        let out = run_cli(fixture, None, &["pane", "run", pane, "--", text.as_str()]);
        assert_eq!(
            out.code, 0,
            "{what}: `pane run` failed on attempt {attempt}: stderr={}",
            out.stderr
        );
        if wait_for_pane_text(&fixture.socket_path, pane, marker, Duration::from_secs(10)) {
            return;
        }
    }
    panic!(
        "{what}: the pane shell never ran the command — ready marker {marker} absent after {ATTEMPTS} sends; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, pane)
    );
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

/// Run a process whose argv[0] is `hermes` in `pane` and wait until DETECTION
/// reports it. A session-identity-only integration leaves lifecycle to the screen,
/// so its tests need a really-detected process rather than a hook state report.
///
/// Two stages with separate failure messages: the pane shell must actually RUN the command
/// (proven by `__zynk_hermes_ready__`), and only then can foreground detection report `hermes`.
/// Keeping them apart is what identified the real cause of the ~1-in-5 failure under parallel suite
/// load — the command always ran, so the deadline was never the problem.
fn start_detected_hermes(fixture: &Fixture, pane: &str) {
    // The fake agent must run as a CHILD of the pane shell, never `exec` in place, because that is
    // the only shape the server's process probe reliably notices. `should_probe_foreground_job`
    // (`src/pane.rs`) re-probes a pane that has no agent yet only when the foreground PROCESS GROUP
    // changes, or while the content-driven acquisition window (8 s from the pane's first output) is
    // still open. An `exec`d agent inherits the pane shell's own process group and then, being a
    // silent `cat`, emits nothing that could reopen that window: once it closed, the pane was never
    // probed again and detection could never report `hermes` — which is why raising the deadline from
    // 10 s to 30 s in the previous commit changed nothing. A forked child gets its OWN foreground
    // process group, forcing the probe on the next tick, and it is also how a real agent starts.
    //
    // The ready marker is shell-quote-split so the ECHOED command line does not contain it verbatim —
    // only the `printf` output (emitted just before the agent starts) does. The `bash -c` layer keeps
    // argv[0] = `hermes` (what `identify_agent_in_job` reads) without assuming the pane's own shell
    // implements `exec -a`.
    run_in_pane_until_ready(
        fixture,
        pane,
        "fake hermes",
        "stty -echo 2>/dev/null; printf '__zynk''_hermes_ready__\\n'; bash -c 'exec -a hermes cat'",
        "__zynk_hermes_ready__",
    );
    // The process is running in its own foreground process group. Detection is still polled by the
    // server, so it keeps room under parallel suite load — but a timeout here now means detection,
    // not a command the pane never ran.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let got = send_json(
            &fixture.socket_path,
            &format!(
                "{{\"id\":\"get\",\"method\":\"pane.get\",\"params\":{{\"pane_id\":\"{pane}\"}}}}"
            ),
        );
        if got.pointer("/result/pane/agent").and_then(Value::as_str) == Some("hermes") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the fake hermes process started (ready marker seen) but detection never reported it: {got}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Report through the COMBINED session-identity-only reporter shape: one
/// `pane.report_agent` carrying BOTH a lifecycle `state` and `agent_session_id`.
///
/// The bundled hermes asset (`src/integration/assets/hermes/__init__.py`) no longer
/// emits this shape -- since the upstream `5f1957c2` asset half it reports identity
/// through `pane.report_agent_session` only, which
/// `both_report_shapes_bind_the_addressed_session_only` covers. The combined shape is
/// still a live wire contract every OTHER integration uses, and it is the one an
/// installed pre-`5f1957c2` hermes plugin keeps sending, so the identity-only routing
/// must keep accepting it: these tests are its characterization.
fn report_identity_only_agent(socket_path: &Path, pane_id: &str, state: &str, session: &str) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"hook\",\"method\":\"pane.report_agent\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"zynk:hermes\",\"agent\":\"hermes\",\"state\":\"{state}\",\"agent_session_id\":\"{session}\"}}}}"
        ),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent: {response}"
    );
}

/// Read `pane.get` for `pane_id` on this fixture's isolated socket.
fn pane_get(socket_path: &Path, pane_id: &str) -> Value {
    send_json(
        socket_path,
        &format!(
            "{{\"id\":\"get\",\"method\":\"pane.get\",\"params\":{{\"pane_id\":\"{pane_id}\"}}}}"
        ),
    )
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
fn receipt_from_a_same_label_pane_that_is_not_the_target_is_rejected() {
    // Gate-3 round 3 (AUD-310-RECEIPT-001): a receipt binds to the STORED target participant, not to
    // an agent label. Two hook-authoritative panes both labeled "pi", each reporting its own agent
    // session the way the shipped integration does: the message is sent to pane 1 (its session is
    // the stored target); pane 2's receipt must be rejected and pane 1's accepted.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let target = create_root_pane(&fixture.socket_path, "receipt-target");
    let impostor = create_root_pane(&fixture.socket_path, "receipt-impostor");
    report_pi_agent_session(&fixture.socket_path, &target, "pi-session-1");
    report_pi_agent_session(&fixture.socket_path, &impostor, "pi-session-2");
    let out = run_cli(&fixture, None, &["send", &target, "--", "bound to pane 1"]);
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send must succeed: {}", out.stderr);
    assert_eq!(sent["delivery_status"], "submitted", "{sent}");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    let wrong = send_json(&fixture.socket_path, &receipt_request(&sent, &impostor));
    assert_eq!(
        wrong["error"]["code"], "receiver_identity_mismatch",
        "a same-label pane that is not the stored target must not receipt: {wrong}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "submitted");

    let right = send_json(&fixture.socket_path, &receipt_request(&sent, &target));
    assert!(
        right.get("error").is_none(),
        "the target pane's receipt failed: {right}"
    );
    assert_eq!(right["result"]["delivery_status"], "received", "{right}");
    assert_eq!(latest_event(&fixture, &message_id).0, "received");
}

#[test]
fn a_session_persisted_for_another_owner_is_not_a_receipt_anchor() {
    // Codex Gate-2 R13 on da2dca7 (r16_identity_probe): panes A and B each hold hook authority for
    // `pi` (state report, no session) AND a persisted session reported for `codex` with the SAME
    // id. That session belongs to another owner: it must not make B the addressee of a message
    // sent to A. A's own receipt (its terminal) is still accepted.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let a = create_root_pane(&fixture.socket_path, "owner-a");
    let b = create_root_pane(&fixture.socket_path, "owner-b");
    for pane in [&a, &b] {
        report_state(&fixture.socket_path, pane, "zynk:pi", "pi");
        report_session(
            &fixture.socket_path,
            pane,
            "zynk:codex",
            "codex",
            "shared-codex-session",
        );
    }
    let out = run_cli(&fixture, None, &["send", &a, "--", "to pane a"]);
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send must succeed: {}", out.stderr);
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    let wrong = send_json(&fixture.socket_path, &receipt_request(&sent, &b));
    assert_eq!(
        wrong["error"]["code"], "receiver_identity_mismatch",
        "another owner's persisted session anchored the receipt: {wrong}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "submitted");
    let right = send_json(&fixture.socket_path, &receipt_request(&sent, &a));
    assert!(
        right.get("error").is_none(),
        "the addressee's own receipt failed: {right}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "received");
}

#[test]
fn a_coherent_persisted_session_still_anchors_the_receipt() {
    // Matching owner: hook authority `pi` + a persisted `zynk:pi` session (the restored-session
    // shape) is the participant identity and receipts as before.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let a = create_root_pane(&fixture.socket_path, "coherent-a");
    report_state(&fixture.socket_path, &a, "zynk:pi", "pi");
    report_session(&fixture.socket_path, &a, "zynk:pi", "pi", "pi-session-a");
    let out = run_cli(&fixture, None, &["send", &a, "--", "coherent"]);
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send must succeed: {}", out.stderr);
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();
    let right = send_json(&fixture.socket_path, &receipt_request(&sent, &a));
    assert!(right.get("error").is_none(), "receipt failed: {right}");
    assert_eq!(latest_event(&fixture, &message_id).0, "received");
}

/// Rewrite the stored TARGET participant of `message_id` (isolated fixture DB only): models a
/// legacy/sparse row that a current send never produces.
fn mutate_target_participant(fixture: &Fixture, message_id: &str, set_clause: &str) {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        sqlx::query(&format!(
            "UPDATE conversation_participants SET {set_clause} WHERE id = \
             (SELECT to_participant_id FROM messages WHERE id = ?)"
        ))
        .bind(message_id)
        .execute(&mut conn)
        .await
        .unwrap();
    });
}

#[test]
fn a_legacy_target_row_without_an_anchor_is_not_receipt_capable() {
    // Gate-3 round 5 (ARCH-RECEIPT-ANCHOR-001 / AUD-310-RECEIPT-003): a stored target with neither
    // a hook session nor a terminal used to bind by label alone, so any same-label pane could
    // receipt it. Such a row is refused for everyone — the impostor and the original pane alike.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let target = create_root_pane(&fixture.socket_path, "legacy-target");
    let impostor = create_root_pane(&fixture.socket_path, "legacy-impostor");
    report_pi_agent_session(&fixture.socket_path, &target, "pi-session-target");
    report_pi_agent_session(&fixture.socket_path, &impostor, "pi-session-impostor");
    let out = run_cli(
        &fixture,
        None,
        &["send", &target, "--", "to a row that loses its anchor"],
    );
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send must succeed: {}", out.stderr);
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();
    mutate_target_participant(
        &fixture,
        &message_id,
        "terminal_id = NULL, agent_session_source = NULL, agent_session_kind = NULL, \
         agent_session_value = NULL",
    );
    for pane in [&impostor, &target] {
        let response = send_json(&fixture.socket_path, &receipt_request(&sent, pane));
        assert_eq!(
            response["error"]["code"], "receiver_identity_mismatch",
            "an unanchored stored target must not be receipted: {response}"
        );
    }
    assert_eq!(latest_event(&fixture, &message_id).0, "submitted");
}

#[test]
fn a_partial_stored_session_triple_is_not_receipt_capable() {
    // A stored session missing its source is not an anchor: the impostor is refused, and so is the
    // original pane (fail closed rather than degrading to a value-only or terminal check).
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let target = create_root_pane(&fixture.socket_path, "partial-target");
    let impostor = create_root_pane(&fixture.socket_path, "partial-impostor");
    report_pi_agent_session(&fixture.socket_path, &target, "pi-session-shared");
    report_pi_agent_session(&fixture.socket_path, &impostor, "pi-session-shared");
    let out = run_cli(&fixture, None, &["send", &target, "--", "to a partial row"]);
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send must succeed: {}", out.stderr);
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();
    mutate_target_participant(&fixture, &message_id, "agent_session_source = NULL");
    for pane in [&impostor, &target] {
        let response = send_json(&fixture.socket_path, &receipt_request(&sent, pane));
        assert_eq!(
            response["error"]["code"], "receiver_identity_mismatch",
            "a partial stored session must not bind: {response}"
        );
    }
    assert_eq!(latest_event(&fixture, &message_id).0, "submitted");
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
fn retired_session_must_not_receipt_after_a_late_report() {
    // Codex Gate-2 M3 extension finding (msg_fd6336c8e9038990): once the
    // identity-only owner is released, its own late hook callback must not restore the
    // receipt authority of the session that was retired.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "retired-identity");
    start_detected_hermes(&fixture, &pane);
    report_session(
        &fixture.socket_path,
        &pane,
        "zynk:hermes",
        "hermes",
        "hermes-1",
    );
    let out = run_cli(
        &fixture,
        None,
        &["send", &pane, "--", "message for the original session"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let sent = parse_outcome(&out);
    let released = send_json(
        &fixture.socket_path,
        &serde_json::json!({
            "id": "release", "method": "pane.release_agent", "params": {
                "pane_id": pane, "source": "zynk:hermes", "agent": "hermes", "seq": 21
            }
        })
        .to_string(),
    );
    assert!(released.get("error").is_none(), "{released}");
    let before = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    assert_eq!(
        before["error"]["code"], "receiver_identity_unverified",
        "{before}"
    );

    let late = send_json(
        &fixture.socket_path,
        &serde_json::json!({
            "id": "late", "method": "pane.report_agent", "params": {
                "pane_id": pane, "source": "zynk:hermes", "agent": "hermes", "seq": 22,
                "state": "idle", "agent_session_id": "hermes-1"
            }
        })
        .to_string(),
    );
    assert!(late.get("error").is_none(), "{late}");

    let after = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    fixture.cleanup();
    assert_eq!(
        after["error"]["code"], "receiver_identity_unverified",
        "a late report restored a retired session's receipt authority: {after}"
    );
}

#[test]
fn a_bare_same_session_resume_must_not_receipt_for_a_retired_session() {
    // Codex Gate-2 M3 extension finding (msg_5fe4c9a3eff5f1a8): the same-session resume
    // that fresh process evidence admits must NOT be admitted on the reason alone. A
    // released owner replaying `SessionStart:resume` for the session it just retired,
    // with no process observed since, is the late callback the retirement refuses.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "resumed-identity");
    start_detected_hermes(&fixture, &pane);
    report_session(
        &fixture.socket_path,
        &pane,
        "zynk:hermes",
        "hermes",
        "hermes-1",
    );
    let out = run_cli(
        &fixture,
        None,
        &["send", &pane, "--", "message for the original session"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let sent = parse_outcome(&out);
    let released = send_json(
        &fixture.socket_path,
        &serde_json::json!({
            "id": "release", "method": "pane.release_agent", "params": {
                "pane_id": pane, "source": "zynk:hermes", "agent": "hermes", "seq": 21
            }
        })
        .to_string(),
    );
    assert!(released.get("error").is_none(), "{released}");

    let resumed = send_json(
        &fixture.socket_path,
        &serde_json::json!({
            "id": "resume", "method": "pane.report_agent_session", "params": {
                "pane_id": pane, "source": "zynk:hermes", "agent": "hermes", "seq": 22,
                "agent_session_id": "hermes-1", "session_start_source": "resume"
            }
        })
        .to_string(),
    );
    assert!(resumed.get("error").is_none(), "{resumed}");

    let after = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    fixture.cleanup();
    assert_eq!(
        after["error"]["code"], "receiver_identity_unverified",
        "a bare resume report restored a retired session's receipt authority: {after}"
    );
}

#[test]
fn both_report_shapes_bind_the_addressed_session_only() {
    // Positive control for the retirement guards: both reporter shapes still anchor a
    // live receipt on the addressed pane, and a same-label pane with a different
    // session still fails the stored-triple check.
    let _guard = test_lock();
    for session_only in [false, true] {
        let fixture = spawn_fixture();
        let a = create_root_pane(&fixture.socket_path, "identity-a");
        let b = create_root_pane(&fixture.socket_path, "identity-b");
        for (pane, session) in [(&a, "hermes-a"), (&b, "hermes-b")] {
            start_detected_hermes(&fixture, pane);
            let method = if session_only {
                "pane.report_agent_session"
            } else {
                "pane.report_agent"
            };
            let mut params = serde_json::json!({"pane_id": pane, "source": "zynk:hermes",
                "agent": "hermes", "agent_session_id": session});
            if !session_only {
                params["state"] = serde_json::json!("blocked");
            }
            let report = send_json(
                &fixture.socket_path,
                &serde_json::json!({
                    "id": "report", "method": method, "params": params
                })
                .to_string(),
            );
            assert!(report.get("error").is_none(), "{report}");
        }
        let out = run_cli(&fixture, None, &["send", &a, "--", "for a only"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let sent = parse_outcome(&out);
        let wrong = send_json(&fixture.socket_path, &receipt_request(&sent, &b));
        let right = send_json(&fixture.socket_path, &receipt_request(&sent, &a));
        fixture.cleanup();
        assert_eq!(
            wrong["error"]["code"], "receiver_identity_mismatch",
            "{wrong}"
        );
        assert_eq!(right["result"]["delivery_status"], "received", "{right}");
    }
}

#[test]
fn identity_only_hook_report_keeps_its_session_identity() {
    // Codex Gate-2 M3 finding (msg_34f2e9b655927aaf): a session-identity-only
    // integration reports lifecycle state AND `agent_session_id` through the one
    // `pane.report_agent` call. Only the LIFECYCLE half is dropped — the reported
    // session is hook-derived IDENTITY, so `pane.get` must still surface it.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "identity-only-session");
    start_passive_cat(&fixture, &pane);

    let report = send_json(
        &fixture.socket_path,
        &format!(
            "{{\"id\":\"hook\",\"method\":\"pane.report_agent\",\"params\":{{\"pane_id\":\"{pane}\",\"source\":\"zynk:hermes\",\"agent\":\"hermes\",\"state\":\"idle\",\"agent_session_id\":\"hermes-session-1\"}}}}"
        ),
    );
    assert!(report.get("error").is_none(), "pane.report_agent: {report}");

    let got = pane_get(&fixture.socket_path, &pane);
    assert_eq!(
        got.pointer("/result/pane/agent_session/value")
            .and_then(Value::as_str),
        Some("hermes-session-1"),
        "the identity-only hook report lost its session identity: {got}"
    );
    assert_eq!(
        got.pointer("/result/pane/agent_session/agent")
            .and_then(Value::as_str),
        Some("hermes"),
        "{got}"
    );
    assert_eq!(
        got.pointer("/result/pane/agent_session/source")
            .and_then(Value::as_str),
        Some("zynk:hermes"),
        "{got}"
    );
    // The lifecycle half IS dropped: the pane runs a plain `cat`, so the reported
    // `idle` must never become the pane's state.
    assert_eq!(
        got.pointer("/result/pane/agent_status")
            .and_then(Value::as_str),
        Some("unknown"),
        "the reported lifecycle state must stay screen-detected: {got}"
    );

    fixture.cleanup();
}

#[test]
fn identity_only_hook_session_anchors_its_own_receipt() {
    // The same finding on the receipt path: a detected session-identity-only agent
    // that reported its session over `pane.report_agent_session` IS hook-identified,
    // so it can receipt the message addressed to it. Before the identity/authority
    // split this returned `receiver_identity_unverified`.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "identity-only-receipt");
    start_detected_hermes(&fixture, &pane);
    report_session(
        &fixture.socket_path,
        &pane,
        "zynk:hermes",
        "hermes",
        "hermes-session-1",
    );

    let out = run_cli(
        &fixture,
        None,
        &["send", &pane, "--", "for the hook-identified session"],
    );
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send: stderr={} {sent}", out.stderr);
    assert_eq!(sent["delivery_status"], "submitted", "{sent}");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    let receipt = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    assert!(
        receipt.get("error").is_none(),
        "the hook-identified addressee could not receipt its own message: {receipt}"
    );
    assert_eq!(
        receipt["result"]["delivery_status"], "received",
        "{receipt}"
    );
    assert_eq!(
        receipt["result"]["receiver_agent_label"], "hermes",
        "{receipt}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "received");

    fixture.cleanup();
}

#[test]
fn identity_only_shipped_reporter_path_anchors_its_receipt() {
    // The SHIPPED reporter path end to end: the asset sends state AND
    // `agent_session_id` in ONE `pane.report_agent`, never `pane.report_agent_session`.
    // That single call must leave the pane receipt-capable while its status stays
    // screen-detected.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "identity-only-shipped");
    start_detected_hermes(&fixture, &pane);
    report_identity_only_agent(&fixture.socket_path, &pane, "idle", "hermes-shipped-1");

    let got = pane_get(&fixture.socket_path, &pane);
    assert_eq!(
        got.pointer("/result/pane/agent_session/value")
            .and_then(Value::as_str),
        Some("hermes-shipped-1"),
        "{got}"
    );

    let out = run_cli(
        &fixture,
        None,
        &["send", &pane, "--", "via the shipped reporter"],
    );
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send: stderr={} {sent}", out.stderr);
    assert_eq!(sent["delivery_status"], "submitted", "{sent}");
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    let receipt = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    assert!(
        receipt.get("error").is_none(),
        "the shipped reporter path could not receipt its own message: {receipt}"
    );
    assert_eq!(
        receipt["result"]["delivery_status"], "received",
        "{receipt}"
    );
    assert_eq!(
        receipt["result"]["receiver_agent_label"], "hermes",
        "{receipt}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "received");

    fixture.cleanup();
}

#[test]
fn a_same_label_identity_only_pane_with_another_session_cannot_receipt() {
    // Owner coherence and the stored target triple are UNWEAKENED by the identity
    // split: two panes both detected as the same identity-only agent, each with its own
    // reported session. Only the addressed session may receipt; the same-label impostor
    // is refused on the stored (source, kind, value) triple.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let target = create_root_pane(&fixture.socket_path, "identity-only-target");
    let impostor = create_root_pane(&fixture.socket_path, "identity-only-impostor");
    start_detected_hermes(&fixture, &target);
    start_detected_hermes(&fixture, &impostor);
    report_identity_only_agent(&fixture.socket_path, &target, "idle", "hermes-target-1");
    report_identity_only_agent(&fixture.socket_path, &impostor, "idle", "hermes-impostor-2");

    let out = run_cli(
        &fixture,
        None,
        &["send", &target, "--", "bound to the target session"],
    );
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send: stderr={} {sent}", out.stderr);
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    let wrong = send_json(&fixture.socket_path, &receipt_request(&sent, &impostor));
    assert_eq!(
        wrong["error"]["code"], "receiver_identity_mismatch",
        "a same-label pane holding another session must not receipt: {wrong}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "submitted");

    let right = send_json(&fixture.socket_path, &receipt_request(&sent, &target));
    assert!(
        right.get("error").is_none(),
        "the addressed session's own receipt failed: {right}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "received");

    fixture.cleanup();
}

#[test]
fn a_detection_only_agent_pane_is_still_not_receipt_capable() {
    // The negative half of the split: a REALLY DETECTED agent process that never
    // reported through any hook has a detection-derived label only, which is still
    // not receipt-capable. Detection must never manufacture receipt identity.
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "detection-only-receipt");
    start_detected_hermes(&fixture, &pane);

    let got = pane_get(&fixture.socket_path, &pane);
    assert_eq!(
        got.pointer("/result/pane/agent").and_then(Value::as_str),
        Some("hermes"),
        "precondition: the pane carries a detection-only label: {got}"
    );
    assert!(
        got.pointer("/result/pane/agent_session").is_none()
            || got
                .pointer("/result/pane/agent_session")
                .map(Value::is_null)
                == Some(true),
        "precondition: no hook reported a session: {got}"
    );

    let out = run_cli(
        &fixture,
        None,
        &["send", &pane, "--", "to a detected-only pane"],
    );
    let sent = parse_outcome(&out);
    assert_eq!(out.code, 0, "send: stderr={} {sent}", out.stderr);
    let message_id = sent["message_id"].as_str().expect("message_id").to_string();

    let receipt = send_json(&fixture.socket_path, &receipt_request(&sent, &pane));
    assert!(
        receipt.get("result").is_none(),
        "a detection-only pane must not receipt: {receipt}"
    );
    assert_eq!(
        receipt["error"]["code"], "receiver_identity_unverified",
        "{receipt}"
    );
    assert_eq!(latest_event(&fixture, &message_id).0, "submitted");

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
    start_passive_cat(&fixture, &pane);
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
