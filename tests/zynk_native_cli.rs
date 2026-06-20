//! zynk fork (M6 / ADR 0007 §2) integration tests: the native top-level command
//! surface — `send`, `reply`, `thread`, `inbox`, `whoami`, `who`, and the promoted
//! top-level `query`.
//!
//! These exercise the integration truth the in-crate unit tests can't:
//!  - `send`/`reply` are thin native verbs over the EXISTING transport (resolve the
//!    target agent → pane, persist via `begin_send_attempt`, submit via the IPC
//!    `pane.send_input` atomic path). They emit the F4 `SendOutcome` envelope; the
//!    persisted body stays pure (the header rides only the wire text); `reply`'s parent
//!    auto-derives (no `--reply-to` flag).
//!  - `thread`/`inbox` are READ-ONLY (`open_query_readonly`, `PRAGMA query_only=1`),
//!    runtime-scoped on `socket_namespace`, and write ZERO delivery events.
//!  - `whoami`/`who` compose live-socket identity/topology, hook-authoritative.
//!
//! The fixture is copied from `tests/zynk_query.rs`: an ISOLATED dev server is
//! spawned under `/tmp` with isolated `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`/
//! `ZYNK_SOCKET_PATH`/`ZYNK_SQLITE_HOME`; the binary (`CARGO_BIN_EXE_zynk`; the
//! produced product binary is renamed to `zynk` only at final integration) is driven
//! as a subprocess sharing the SAME `ZYNK_SQLITE_HOME` so the send-side persistence
//! and the in-process `thread`/`inbox` reads hit the SAME DB file. The whole suite is
//! serialized via `test_lock()`.

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
        "/tmp/zynk-native-cli-test-{}-{nanos}",
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
        self.teardown();
    }

    fn teardown(&mut self) {
        if let Some(server) = self.server.take() {
            drop(server);
        }
        if !self.base.as_os_str().is_empty() {
            cleanup_test_base(&self.base);
            self.base = PathBuf::new();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn app_dir() -> &'static str {
    if cfg!(debug_assertions) {
        "zynk-dev"
    } else {
        "zynk"
    }
}

/// Spawn an isolated server. The embedding worker runs under
/// `ZYNK_EMBED_PROVIDER=fastembed` (UNCOMPILED in the default build) so it exits at
/// boot — no embeddings, no boot-sweep race; these tests never depend on the vector
/// path. We DON'T pin `ZYNK_EMBED_POLL_MS` (moot when the worker exits at boot).
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
    cmd.env("ZYNK_EMBED_PROVIDER", "fastembed");

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

/// Report an OFFICIAL, NON-reserved-native agent (e.g. `kimi`) via `pane.report_agent`
/// with the production hook source `zynk:<agent>` + a session id. Because the agent
/// is NOT reserved-native, this routes to `set_hook_authority_with_session_ref`, so the
/// pane gains HOOK AUTHORITY and surfaces `agent_session.agent == label` (the
/// hook-authoritative identity whoami/who must report). VERIFIED via `pane.get`.
fn report_agent_session(socket_path: &Path, pane_id: &str, label: &str, session_id: &str) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"report\",\"method\":\"pane.report_agent\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"zynk:{label}\",\"agent\":\"{label}\",\"state\":\"idle\",\"agent_session_id\":\"{session_id}\"}}}}"
        ),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent: {response}"
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
        Some(label),
        "pane.get must surface a hook-authoritative agent_session.agent == {label:?} (got {got})"
    );
}

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

/// Split `pane_id` to the right, yielding a second pane in the SAME tab (so two
/// agents share a workspace/tab → the SAME conversation scope, which the reply
/// auto-derivation needs).
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

struct CliOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Drive the binary against this fixture's isolated socket + DB. When
/// `zynk_pane_id` is `Some`, set `ZYNK_PANE_ID` (the caller's source pane).
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
    command.env("ZYNK_EMBED_PROVIDER", "fastembed");
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

/// Count rows in `delivery_events` — pins the read-only invariant (a read command
/// must NOT synthesize any delivery/recovery event).
fn delivery_events_count(fixture: &Fixture) -> i64 {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let row = sqlx::query("SELECT COUNT(*) AS c FROM delivery_events")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        row.try_get::<i64, _>("c").unwrap()
    })
}

/// Read the persisted (PURE) body of a message — to prove the header never pollutes
/// the stored body.
fn stored_body(fixture: &Fixture, message_id: &str) -> String {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let row = sqlx::query("SELECT body FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_one(&mut conn)
            .await
            .unwrap();
        row.try_get::<String, _>("body").unwrap()
    })
}

/// Read the `derived_parent_id` of a message (NULL → None).
fn derived_parent(fixture: &Fixture, message_id: &str) -> Option<String> {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let row = sqlx::query("SELECT derived_parent_id FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_one(&mut conn)
            .await
            .unwrap();
        row.try_get::<Option<String>, _>("derived_parent_id")
            .unwrap()
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

/// Top-level `zynk send <target> <text...>` returning the parsed F4 outcome.
fn native_send(fixture: &Fixture, from_pane: Option<&str>, target: &str, body: &str) -> Value {
    let out = run_cli(fixture, from_pane, &["send", target, "--", body]);
    parse_outcome(&out)
}

/// `zynk send <target> --trace <id> -- <text...>` (Feature #107) returning the outcome.
fn native_send_traced(
    fixture: &Fixture,
    from_pane: Option<&str>,
    target: &str,
    trace: &str,
    body: &str,
) -> Value {
    let out = run_cli(
        fixture,
        from_pane,
        &["send", target, "--trace", trace, "--", body],
    );
    parse_outcome(&out)
}

// ----------------------------------------------------------------------------
// 1. `send` emits the F4 ok envelope and keeps the stored body PURE.
// ----------------------------------------------------------------------------
#[test]
fn native_send_emits_f4_ok_and_keeps_body_pure() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-send");
    report_agent(&fixture.socket_path, &pane, "codex");

    let v = native_send(&fixture, None, "codex", "znativehello world");
    assert_eq!(v["result"], "ok", "{v}");
    // F4 `command` is the native `zynk send` label (NOT `agent send`) so the
    // envelope is honest about which native verb ran (M6-NATIVE-003).
    assert_eq!(v["command"], "zynk send", "{v}");
    assert_eq!(v["delivery_status"], "submitted", "{v}");
    assert!(v["message_id"].is_string(), "message_id present: {v}");
    assert!(
        v["conversation_id"].is_string(),
        "conversation_id present: {v}"
    );
    assert!(
        v["socket_namespace"].is_string(),
        "socket_namespace present: {v}"
    );
    assert!(v["next"].is_string(), "next present: {v}");

    // Body purity: the PERSISTED body never carries the wire header.
    let mid = v["message_id"].as_str().unwrap();
    let body = stored_body(&fixture, mid);
    assert_eq!(
        body, "znativehello world",
        "stored body must be the pure text (no header): {body:?}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 2. `reply` is `send` with auto-derived linkage (no --reply-to flag): a reply to
//    a target's latest message links to it via `derived_parent_id`.
// ----------------------------------------------------------------------------
#[test]
fn native_reply_auto_derives_parent() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // Two agents in the SAME tab (codex in the root pane, kimi in its split) so they
    // share a workspace/tab → the SAME conversation scope. codex sends to kimi; kimi
    // (a second caller) replies to codex → the reply's parent auto-derives to codex's
    // latest message in that one conversation.
    let codex_pane = create_root_pane(&fixture.socket_path, "native-reply");
    let kimi_pane = split_pane(&fixture.socket_path, &codex_pane);
    report_agent(&fixture.socket_path, &codex_pane, "codex");
    report_agent(&fixture.socket_path, &kimi_pane, "kimi");

    // codex (ZYNK_PANE_ID=codex_pane) sends to kimi.
    let first = native_send(&fixture, Some(&codex_pane), "kimi", "znativethread first");
    assert_eq!(first["result"], "ok", "{first}");
    assert_eq!(first["command"], "zynk send", "{first}");
    let first_id = first["message_id"].as_str().unwrap().to_string();

    // kimi replies to codex → reply derives its parent from codex's latest message.
    let out = run_cli(
        &fixture,
        Some(&kimi_pane),
        &["reply", "codex", "--", "znativethread second"],
    );
    let reply = parse_outcome(&out);
    assert_eq!(reply["result"], "ok", "{reply}");
    // `zynk reply` carries the distinct native `command` label (M6-NATIVE-003): the
    // reply is no longer indistinguishable from a send in the F4 envelope.
    assert_eq!(reply["command"], "zynk reply", "{reply}");
    assert_eq!(reply["delivery_status"], "submitted", "{reply}");
    let reply_id = reply["message_id"].as_str().unwrap().to_string();

    let parent = derived_parent(&fixture, &reply_id);
    assert_eq!(
        parent.as_deref(),
        Some(first_id.as_str()),
        "reply parent must auto-derive to codex's latest message (no --reply-to): \
         reply={reply_id} parent={parent:?} expected={first_id}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 3. `reply` does NOT accept a --reply-to flag (SPEC §5 / ADR 0002: parent is
//    auto-derived). An unknown flag must not silently succeed.
// ----------------------------------------------------------------------------
#[test]
fn native_reply_rejects_reply_to_flag() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-noreplyto");
    report_agent(&fixture.socket_path, &pane, "codex");

    // `--reply-to X` must be treated as message TEXT (no such flag), never consumed as
    // a parent selector. The send still goes through (the flag-looking token is body),
    // but there is provably no `--reply-to` parsing producing a distinct linkage.
    let out = run_cli(
        &fixture,
        None,
        &["reply", "codex", "--", "--reply-to", "abc", "hello"],
    );
    let v = parse_outcome(&out);
    assert_eq!(v["result"], "ok", "{v}");
    let mid = v["message_id"].as_str().unwrap();
    let body = stored_body(&fixture, mid);
    assert_eq!(
        body, "--reply-to abc hello",
        "after `--`, `--reply-to` is literal body text, NOT a flag: {body:?}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 3b. The native F4 `command` label is honest on the FAILURE branch too
//     (M6-NATIVE-003): `zynk send` to an unresolvable target emits
//     `command:"zynk send"`, and `zynk reply` emits `command:"zynk reply"` —
//     never the transport-layer `agent send` label.
// ----------------------------------------------------------------------------
#[test]
fn native_send_and_reply_command_label_on_failure() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // No agent is reported, so the target never resolves → target_not_found failure.
    let send_out = run_cli(&fixture, None, &["send", "ghostagent", "--", "hi"]);
    let send = parse_outcome(&send_out);
    assert_eq!(send["result"], "failed", "{send}");
    assert_eq!(send["command"], "zynk send", "{send}");
    assert_eq!(
        send["target_resolution"], "not_found",
        "unresolved target: {send}"
    );

    let reply_out = run_cli(&fixture, None, &["reply", "ghostagent", "--", "hi"]);
    let reply = parse_outcome(&reply_out);
    assert_eq!(reply["result"], "failed", "{reply}");
    assert_eq!(reply["command"], "zynk reply", "{reply}");
    assert_eq!(
        reply["target_resolution"], "not_found",
        "unresolved target: {reply}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 4. `thread` is READ-ONLY: it returns the ordered transcript and writes ZERO
//    delivery events.
// ----------------------------------------------------------------------------
#[test]
fn native_thread_is_read_only_and_ordered() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-thread");
    report_agent(&fixture.socket_path, &pane, "codex");

    let a = native_send(&fixture, None, "codex", "znativethread alpha");
    let b = native_send(&fixture, None, "codex", "znativethread beta");
    let conv = a["conversation_id"].as_str().unwrap().to_string();
    let a_id = a["message_id"].as_str().unwrap().to_string();
    let b_id = b["message_id"].as_str().unwrap().to_string();

    let before = delivery_events_count(&fixture);
    assert!(before >= 2, "two submits recorded delivery events");

    let out = run_cli(&fixture, None, &["thread", &conv, "--json"]);
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "thread exit 0: stderr={}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "zynk thread", "{v}");
    assert_eq!(v["conversation_id"], conv, "{v}");
    let msgs = v["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 2, "two messages in the thread: {v}");
    // Ordered by conversation_seq ascending.
    assert_eq!(msgs[0]["message_id"], a_id, "first message: {v}");
    assert_eq!(msgs[1]["message_id"], b_id, "second message: {v}");
    assert!(
        msgs[0]["conversation_seq"].as_i64().unwrap()
            < msgs[1]["conversation_seq"].as_i64().unwrap(),
        "ordered by conversation_seq ascending: {v}"
    );
    // Honest delivery status surfaced.
    assert!(
        msgs[0]["delivery_status"].is_string(),
        "delivery_status surfaced: {v}"
    );

    let after = delivery_events_count(&fixture);
    assert_eq!(
        before, after,
        "thread is read-only: it must NEVER write a delivery event (before={before} after={after})"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 5. `thread` accepts a MESSAGE id (resolves to its conversation) as well.
// ----------------------------------------------------------------------------
#[test]
fn native_thread_accepts_message_id() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-thread-mid");
    report_agent(&fixture.socket_path, &pane, "codex");

    let a = native_send(&fixture, None, "codex", "znativemid alpha");
    let _b = native_send(&fixture, None, "codex", "znativemid beta");
    let a_id = a["message_id"].as_str().unwrap().to_string();
    let conv = a["conversation_id"].as_str().unwrap().to_string();

    let out = run_cli(&fixture, None, &["thread", &a_id, "--json"]);
    let v = parse_outcome(&out);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(
        v["conversation_id"], conv,
        "a message id resolves to its conversation: {v}"
    );
    assert_eq!(
        v["messages"].as_array().unwrap().len(),
        2,
        "full transcript returned: {v}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 6. `inbox` is READ-ONLY, runtime-scoped, lists messages addressed to the caller
//    with honest delivery_status, writes ZERO delivery events.
// ----------------------------------------------------------------------------
#[test]
fn native_inbox_lists_addressed_messages_read_only() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-inbox");
    report_agent(&fixture.socket_path, &pane, "codex");

    let m = native_send(&fixture, None, "codex", "znativeinbox hi");
    let mid = m["message_id"].as_str().unwrap().to_string();

    let before = delivery_events_count(&fixture);

    let out = run_cli(&fixture, None, &["inbox", "--agent", "codex", "--json"]);
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "inbox exit 0: stderr={}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "zynk inbox", "{v}");
    assert_eq!(v["agent"], "codex", "{v}");
    let msgs = v["messages"].as_array().expect("messages array");
    assert!(
        msgs.iter().any(|m| m["message_id"] == mid),
        "the message addressed to codex must appear in codex's inbox: {v}"
    );
    assert!(
        msgs[0]["delivery_status"].is_string(),
        "honest delivery_status surfaced: {v}"
    );

    // An inbox for a DIFFERENT agent must NOT include codex's message.
    let out2 = run_cli(&fixture, None, &["inbox", "--agent", "kimi", "--json"]);
    let v2 = parse_outcome(&out2);
    assert_eq!(v2["result"], "ok", "{v2}");
    assert!(
        !v2["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["message_id"] == mid),
        "kimi's inbox must NOT contain a message addressed to codex: {v2}"
    );

    let after = delivery_events_count(&fixture);
    assert_eq!(
        before, after,
        "inbox is read-only: it must NEVER write a delivery event (before={before} after={after})"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 7. `whoami` composes hook-authoritative identity from the caller's pane.
// ----------------------------------------------------------------------------
#[test]
fn native_whoami_is_hook_authoritative() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-whoami");
    // Hook-authoritative agent_session (non-reserved-native `kimi` via `zynk:kimi`).
    report_agent_session(&fixture.socket_path, &pane, "kimi", "kimi-session-xyz");

    let out = run_cli(&fixture, Some(&pane), &["whoami", "--json"]);
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "whoami exit 0: stderr={}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "zynk whoami", "{v}");
    assert_eq!(v["pane_id"], pane, "pane resolved from ZYNK_PANE_ID: {v}");
    // Identity is HOOK-AUTHORITATIVE: agent comes from agent_session, not detection.
    assert_eq!(
        v["agent"], "kimi",
        "agent is the hook-authoritative agent_session label: {v}"
    );
    assert_eq!(
        v["agent_session"]["source"], "zynk:kimi",
        "agent_session source is the hook source: {v}"
    );
    assert!(
        v["socket_namespace"].is_string(),
        "socket_namespace present: {v}"
    );
    assert!(v["workspace_id"].is_string(), "workspace_id present: {v}");
    assert!(v["tab_id"].is_string(), "tab_id present: {v}");

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 8. `whoami` surfaces a detection-only label as explicitly `detected`, NEVER as the
//    authoritative identity (no agent_session → no authoritative agent).
// ----------------------------------------------------------------------------
#[test]
fn native_whoami_marks_detected_label_as_detected() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-whoami-detected");
    // report_agent WITHOUT a session id → no hook-authoritative agent_session. The
    // reported `agent` label is non-authoritative provenance here.
    report_agent(&fixture.socket_path, &pane, "codex");

    let out = run_cli(&fixture, Some(&pane), &["whoami", "--json"]);
    let v = parse_outcome(&out);
    assert_eq!(v["result"], "ok", "{v}");
    // No hook-authoritative agent_session ⇒ the authoritative `agent` is null, and the
    // label is surfaced under `detected` (never promoted to authoritative).
    assert!(
        v["agent_session"].is_null() || v["agent_session"].get("source").is_none(),
        "no hook agent_session present: {v}"
    );
    assert!(
        v["agent"].is_null(),
        "without a hook-authoritative agent_session, `agent` must NOT claim authority: {v}"
    );
    assert_eq!(
        v["detected"]["agent"], "codex",
        "the non-authoritative label is surfaced as explicitly detected: {v}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 8b. `whoami` resolves the caller pane from ZYNK_PANE_ID.
// ----------------------------------------------------------------------------
#[test]
fn native_whoami_uses_zynk_pane_id() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let zynk_pane = create_root_pane(&fixture.socket_path, "native-whoami-zynk");
    report_agent_session(&fixture.socket_path, &zynk_pane, "kimi", "kimi-zps");

    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command.args(["whoami", "--json"]);
    command.env("XDG_CONFIG_HOME", &fixture.config_home);
    command.env("XDG_RUNTIME_DIR", &fixture.runtime_dir);
    command.env("ZYNK_SOCKET_PATH", &fixture.socket_path);
    command.env("ZYNK_SQLITE_HOME", &fixture.sqlite_home);
    command.env_remove("ZYNK_HOME");
    command.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    command.env_remove("ZYNK_ENV");
    command.env("ZYNK_EMBED_PROVIDER", "fastembed");
    command.env("ZYNK_PANE_ID", &zynk_pane);
    let output = command.output().expect("run zynk whoami");
    let out = CliOutput {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };
    let v = parse_outcome(&out);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(
        v["pane_id"], zynk_pane,
        "whoami resolves the caller pane from ZYNK_PANE_ID: {v}"
    );
    assert_eq!(v["agent"], "kimi", "the ZYNK_PANE_ID pane's agent: {v}");

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 9. `who` composes the participant topology from the live socket.
// ----------------------------------------------------------------------------
#[test]
fn native_who_lists_participants() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let kimi_pane = create_root_pane(&fixture.socket_path, "native-who-kimi");
    report_agent_session(&fixture.socket_path, &kimi_pane, "kimi", "kimi-s");
    let opencode_pane = create_root_pane(&fixture.socket_path, "native-who-opencode");
    report_agent_session(&fixture.socket_path, &opencode_pane, "opencode", "oc-s");

    let out = run_cli(&fixture, None, &["who", "--json"]);
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "who exit 0: stderr={}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "zynk who", "{v}");
    let participants = v["participants"].as_array().expect("participants array");
    let labels: Vec<&str> = participants
        .iter()
        .filter_map(|p| p["agent"].as_str())
        .collect();
    assert!(labels.contains(&"kimi"), "kimi listed: {v}");
    assert!(labels.contains(&"opencode"), "opencode listed: {v}");
    assert!(
        v["socket_namespace"].is_string(),
        "socket_namespace present: {v}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 10. `query` is promoted to a TOP-LEVEL verb (in addition to `zynk query`).
// ----------------------------------------------------------------------------
#[test]
fn native_query_top_level_returns_results() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-query");
    report_agent(&fixture.socket_path, &pane, "codex");
    let _m = native_send(&fixture, None, "codex", "znativequery sentinel body");

    let out = run_cli(&fixture, None, &["query", "znativequery", "--json"]);
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "query exit 0: stderr={}", out.stderr);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "zynk query", "{v}");
    assert!(
        v["count"].as_u64().unwrap_or(0) >= 1,
        "top-level query retrieves the just-sent message: {v}"
    );

    // The legacy `zynk query` subcommand group stays intact (back-compat).
    let out2 = run_cli(&fixture, None, &["zynk", "query", "znativequery", "--json"]);
    let v2 = parse_outcome(&out2);
    assert_eq!(v2["result"], "ok", "legacy `zynk query` still works: {v2}");

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 11. `thread`/`inbox` are runtime-scoped: a thread/inbox query never crosses
//     `socket_namespace`. (Single-runtime fixture: a foreign socket_namespace yields
//     an empty inbox, proving the scope filter is applied.)
// ----------------------------------------------------------------------------
#[test]
fn native_inbox_is_runtime_scoped() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-scope");
    report_agent(&fixture.socket_path, &pane, "codex");
    let _m = native_send(&fixture, None, "codex", "znativescope hi");

    // The caller's inbox (same runtime) sees the message.
    let out = run_cli(&fixture, None, &["inbox", "--agent", "codex", "--json"]);
    let v = parse_outcome(&out);
    assert!(
        v["messages"].as_array().unwrap().iter().count() >= 1,
        "same-runtime inbox sees the message: {v}"
    );
    // socket_namespace is reported so the scope is auditable.
    assert!(
        v["socket_namespace"].is_string(),
        "inbox reports its runtime scope (socket_namespace): {v}"
    );

    fixture.cleanup();
}

// ----------------------------------------------------------------------------
// 12. Feature #107 (IM2): a per-message trace_id is surfaced across the read
//     surfaces (`thread`, `inbox`, `query --trace`, and `trace`), and OMITTED for a
//     message with no trace. `query --trace` and `trace` prefilter to only the traced
//     message; an old/no-trace message is excluded.
// ----------------------------------------------------------------------------
#[test]
fn native_trace_id_surfaced_and_queryable() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "native-trace");
    report_agent(&fixture.socket_path, &pane, "codex");

    // One traced message and one UN-traced message (both to codex, both match "zt107").
    let traced = native_send_traced(&fixture, None, "codex", "trace-im2", "zt107 traced body");
    let untraced = native_send(&fixture, None, "codex", "zt107 plain body");
    let traced_id = traced["message_id"].as_str().unwrap().to_string();
    let untraced_id = untraced["message_id"].as_str().unwrap().to_string();
    let conv = traced["conversation_id"].as_str().unwrap().to_string();

    let before = delivery_events_count(&fixture);

    // (a) thread surfaces trace_id when present + omits it when absent.
    let thread = parse_outcome(&run_cli(&fixture, None, &["thread", &conv, "--json"]));
    let tmsgs = thread["messages"].as_array().expect("thread messages");
    let t_traced = tmsgs
        .iter()
        .find(|m| m["message_id"] == traced_id.as_str())
        .expect("traced message in thread");
    assert_eq!(
        t_traced["trace_id"], "trace-im2",
        "thread surfaces trace_id: {thread}"
    );
    let t_plain = tmsgs
        .iter()
        .find(|m| m["message_id"] == untraced_id.as_str())
        .expect("untraced message in thread");
    assert!(
        t_plain.get("trace_id").is_none(),
        "thread omits trace_id for an untraced message: {thread}"
    );

    // (b) inbox surfaces trace_id likewise.
    let inbox = parse_outcome(&run_cli(
        &fixture,
        None,
        &["inbox", "--agent", "codex", "--json"],
    ));
    let imsgs = inbox["messages"].as_array().expect("inbox messages");
    let i_traced = imsgs
        .iter()
        .find(|m| m["message_id"] == traced_id.as_str())
        .expect("traced message in inbox");
    assert_eq!(
        i_traced["trace_id"], "trace-im2",
        "inbox surfaces trace_id: {inbox}"
    );
    let i_plain = imsgs
        .iter()
        .find(|m| m["message_id"] == untraced_id.as_str())
        .expect("untraced message in inbox");
    assert!(
        i_plain.get("trace_id").is_none(),
        "inbox omits trace_id for an untraced message: {inbox}"
    );

    // (c) query --trace prefilters to ONLY the traced message + surfaces trace_id.
    let q = parse_outcome(&run_cli(
        &fixture,
        None,
        &["query", "zt107", "--trace", "trace-im2", "--json"],
    ));
    assert_eq!(q["result"], "ok", "query --trace ok: {q}");
    let qres = q["results"].as_array().expect("query results");
    assert_eq!(
        qres.len(),
        1,
        "query --trace returns only the traced message: {q}"
    );
    assert_eq!(qres[0]["message_id"], traced_id.as_str(), "{q}");
    assert_eq!(
        qres[0]["trace_id"], "trace-im2",
        "query surfaces trace_id: {q}"
    );

    // A query without --trace sees BOTH (proving the filter is what excluded the plain one).
    let q_all = parse_outcome(&run_cli(&fixture, None, &["query", "zt107", "--json"]));
    assert_eq!(
        q_all["results"].as_array().unwrap().len(),
        2,
        "without --trace both messages match the term: {q_all}"
    );

    // (d) `zynk trace <id>` lists exactly the traced message across conversations.
    let tr = parse_outcome(&run_cli(&fixture, None, &["trace", "trace-im2", "--json"]));
    assert_eq!(tr["result"], "ok", "trace ok: {tr}");
    assert_eq!(tr["command"], "zynk trace", "{tr}");
    assert_eq!(tr["trace"], "trace-im2", "{tr}");
    let trmsgs = tr["messages"].as_array().expect("trace messages");
    assert_eq!(trmsgs.len(), 1, "trace lists only the traced message: {tr}");
    assert_eq!(trmsgs[0]["message_id"], traced_id.as_str(), "{tr}");
    assert_eq!(trmsgs[0]["trace_id"], "trace-im2", "{tr}");

    // A trace nobody carries → empty (count 0, ok).
    let tr_none = parse_outcome(&run_cli(
        &fixture,
        None,
        &["trace", "no-such-trace", "--json"],
    ));
    assert_eq!(tr_none["result"], "ok", "{tr_none}");
    assert_eq!(tr_none["count"], 0, "absent trace → no rows: {tr_none}");

    // All four read surfaces are READ-ONLY: zero new delivery events.
    let after = delivery_events_count(&fixture);
    assert_eq!(
        before, after,
        "thread/inbox/query/trace are read-only (before={before} after={after})"
    );

    fixture.cleanup();
}
