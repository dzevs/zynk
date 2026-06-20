//! zynk fork (ADR 0002/0005) integration tests: the SEND-SIDE agent-VISIBLE HEADER.
//! For EVERY native zynk message to an AGENT target (claude/codex/pi alike), the CLI
//! PREPENDS a readable `╭─ Zynk message ─…╰─…` box header before the pure body. The
//! header is UNIFORM — NOT an allowlist, NOT pi-only — and is for agent AWARENESS, not
//! receipt proof (a delivered header never advances `delivery_status`).
//!
//! `messages.body`/`body_hash`/FTS stay PURE (the header is wire-only); the persisted
//! `protocol_json` DB column carries the protocol IDs uniformly for ALL commands incl.
//! `pane send-text` drafts (ADR 0005). A plain shell pane (no agent identity) gets NO
//! header.
//!
//! These run against an ISOLATED dev server spawned under `/tmp` with an isolated
//! `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`/`ZYNK_SOCKET_PATH`; they never touch the live
//! zynk runtime. The CLI binary (still named `zynk`) is driven as a subprocess so
//! assertions observe the real stdout + exit code.

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

/// The header's box-top marker — the stable, agent-visible signal a header rode the
/// wire. (`render_header` in src/zynk/header.rs starts every header with this.)
const HEADER_TOP: &str = "╭─ Zynk message";
/// The awareness note prefix — pins that the header truthfully discloses it is for
/// awareness. Feature #107 (IM3, Q1): the `note:`/`reply:` lines are HIDDEN by default and
/// only appear under the verbose escape hatch (`ZYNK_HEADER_VERBOSE=1`). A SHORT prefix
/// (not the full "…not receipt proof" sentence) so an 80-col terminal wrap of the
/// recent-pane text never splits the asserted substring.
const HEADER_NOTE: &str = "note: header is for agent awareness";

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
        "/tmp/zynk-header-test-{}-{nanos}",
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

/// A fully-isolated server + its runtime paths. Dropping it (via `cleanup`) kills
/// the server and removes the base dir.
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

/// Report an OFFICIAL agent session (`source`/`agent` must be an official pair so
/// `session_ref_from_report` yields a non-None ref) and VERIFY via `pane.get` that
/// `result.pane.agent_session.agent == agent`. Panics if the session does not surface.
/// The header gate (`is_agent_target`) sees an agent identity here, so this both
/// surfaces the session AND makes the pane an agent target.
fn report_agent_session(
    socket_path: &Path,
    pane_id: &str,
    source: &str,
    agent: &str,
    session_id: &str,
) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"sess\",\"method\":\"pane.report_agent_session\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"{source}\",\"agent\":\"{agent}\",\"agent_session_id\":\"{session_id}\"}}}}"
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
    let surfaced = got
        .pointer("/result/pane/agent_session/agent")
        .and_then(Value::as_str);
    assert_eq!(
        surfaced,
        Some(agent),
        "pane.get must surface agent_session.agent == {agent:?} (got {got})"
    );
}

/// Report an OFFICIAL, NON-reserved-native agent via `pane.report_agent` WITH a
/// session ref (the production hook path). Routes to
/// `set_hook_authority_with_session_ref`, so the pane BOTH surfaces
/// `agent_session.agent == agent` AND gains HOOK AUTHORITY — so `agent.get(<agent>)`
/// RESOLVES the pane (which `agent send <target>` requires). Verified via `pane.get`.
fn report_hook_agent_session(
    socket_path: &Path,
    pane_id: &str,
    source: &str,
    agent: &str,
    session_id: &str,
) {
    let response = send_json(
        socket_path,
        &format!(
            "{{\"id\":\"sess\",\"method\":\"pane.report_agent\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"{source}\",\"agent\":\"{agent}\",\"state\":\"idle\",\"agent_session_id\":\"{session_id}\"}}}}"
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
    let surfaced = got
        .pointer("/result/pane/agent_session/agent")
        .and_then(Value::as_str);
    assert_eq!(
        surfaced,
        Some(agent),
        "pane.get must surface agent_session.agent == {agent:?} (got {got})"
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

/// Drive the `zynk` CLI binary against this fixture's isolated socket. The header
/// injection happens in the CLI process for EVERY agent target — no env opt-in. When
/// `zynk_pane_id` is `Some`, set `ZYNK_PANE_ID`; when `None`, remove it.
fn run_cli(fixture: &Fixture, zynk_pane_id: Option<&str>, args: &[&str]) -> CliOutput {
    run_cli_env(fixture, zynk_pane_id, args, &[])
}

/// As [`run_cli`], but with extra env vars (e.g. `ZYNK_HEADER_VERBOSE=1` to exercise the
/// Feature #107 IM3 verbose escape hatch end-to-end).
fn run_cli_env(
    fixture: &Fixture,
    zynk_pane_id: Option<&str>,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> CliOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command.args(args);
    command.env("XDG_CONFIG_HOME", &fixture.config_home);
    command.env("XDG_RUNTIME_DIR", &fixture.runtime_dir);
    command.env("ZYNK_SOCKET_PATH", &fixture.socket_path);
    command.env("ZYNK_SQLITE_HOME", &fixture.sqlite_home);
    command.env_remove("ZYNK_HOME");
    command.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    command.env_remove("ZYNK_ENV");
    command.env_remove("ZYNK_HEADER_VERBOSE");
    for (key, value) in extra_env {
        command.env(key, value);
    }
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

/// The persisted (pure) `messages.body` for `message_id` — must NEVER contain the
/// wire header (header is wire-only; body stays pure).
fn body_of(fixture: &Fixture, message_id: &str) -> String {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let row = sqlx::query("SELECT body FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_one(&mut conn)
            .await
            .unwrap();
        row.try_get("body").unwrap()
    })
}

/// The persisted structured `protocol_json` for `message_id`, parsed as JSON. Carries
/// the protocol IDs uniformly for ALL commands incl. drafts (ADR 0005). The wire is now
/// a header; this column records the structured protocol IDs.
fn protocol_json_of(fixture: &Fixture, message_id: &str) -> Value {
    sqlite_runtime().block_on(async {
        let mut conn = open_test_db(fixture).await;
        let row = sqlx::query("SELECT protocol_json FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let raw: String = row.try_get("protocol_json").unwrap();
        serde_json::from_str(&raw).expect("protocol_json is valid JSON")
    })
}

/// Shared body of the uniform-header test: a `pane run` to an agent pane (any official
/// agent label, surfaced via `pane.report_agent_session`) PREPENDS the visible header,
/// while `messages.body` stays pure and `protocol_json` carries the protocol IDs. This is
/// run for claude/codex/pi alike to prove the header is UNIFORM (not an allowlist).
fn assert_header_prepended_for_agent(agent: &str) {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, &format!("header-{agent}"));
    report_agent_session(
        &fixture.socket_path,
        &pane,
        &format!("zynk:{agent}"),
        agent,
        &format!("sess-{agent}"),
    );

    let out = run_cli(
        &fixture,
        None,
        &["pane", "run", &pane, "--", "zbodysentinel", "hi"],
    );
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "pane run exit 0: stderr={}", out.stderr);
    // The header is awareness, not receipt proof: delivery_status stays `submitted`.
    assert_eq!(v["delivery_status"], "submitted", "{v}");
    let message_id = v["message_id"].as_str().expect("message_id").to_string();

    // The delivered pane text carries the visible header box + the awareness note.
    assert!(
        wait_for_pane_text(
            &fixture.socket_path,
            &pane,
            HEADER_TOP,
            Duration::from_secs(5)
        ),
        "agent {agent}: pane run must PREPEND the visible header box; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );
    let pane_text = pane_recent_text(&fixture.socket_path, &pane);
    // Feature #107 (IM3, Q1): by DEFAULT the `note:`/`reply:` lines are HIDDEN.
    assert!(
        !pane_text.contains(HEADER_NOTE),
        "agent {agent}: the awareness note is HIDDEN by default; pane text: {pane_text:?}"
    );
    assert!(
        !pane_text.contains("reply: zynk reply"),
        "agent {agent}: the reply affordance is HIDDEN by default; pane text: {pane_text:?}"
    );
    // The header carries id + conv lines (awareness) and the message body.
    assert!(
        pane_text.contains(&format!("id:   {message_id}")),
        "agent {agent}: header carries the message id; pane text: {pane_text:?}"
    );
    assert!(
        pane_text.contains("conv:"),
        "agent {agent}: header carries the conv line; pane text: {pane_text:?}"
    );
    assert!(
        pane_text.contains("zbodysentinel"),
        "agent {agent}: the body is delivered after the header; pane text: {pane_text:?}"
    );

    // The persisted body stays PURE — the header rode the wire only, never the body.
    assert_eq!(
        body_of(&fixture, &message_id),
        "zbodysentinel hi",
        "agent {agent}: messages.body must stay the pure body (no header)"
    );
    assert!(
        !body_of(&fixture, &message_id).contains(HEADER_TOP),
        "agent {agent}: the persisted body must NOT contain the header box"
    );

    // Protocol IDs are present in protocol_json (the persisted IDs).
    let protocol = protocol_json_of(&fixture, &message_id);
    assert!(
        protocol.get("message_id").is_some(),
        "agent {agent}: protocol_json must carry protocol IDs (message_id): {protocol}"
    );

    fixture.cleanup();
}

/// UNIFORM: claude gets the header (it is an agent target — no allowlist gate).
#[test]
fn header_prepended_for_claude() {
    assert_header_prepended_for_agent("claude");
}

/// UNIFORM: codex gets the header. Under the OLD receipt mechanism codex was excluded
/// (reserved-native, no env opt-in); the header is symmetric, so codex gets it too.
#[test]
fn header_prepended_for_codex() {
    assert_header_prepended_for_agent("codex");
}

/// UNIFORM: pi gets the header — same path as every other agent (no pi-special-casing).
#[test]
fn header_prepended_for_pi() {
    assert_header_prepended_for_agent("pi");
}

/// Feature #107 (IM3, Q1) verbose escape hatch end-to-end: `ZYNK_HEADER_VERBOSE=1`
/// re-adds the `reply:`/`note:` lines that are HIDDEN by default — proving the env
/// override flows from the CLI through `resolve_header_options` into `render_header`.
#[test]
fn header_verbose_env_shows_reply_and_note() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "header-verbose");
    report_agent_session(&fixture.socket_path, &pane, "zynk:pi", "pi", "sess-verbose");

    let out = run_cli_env(
        &fixture,
        None,
        &["pane", "run", &pane, "--", "zbodysentinel", "hi"],
        &[("ZYNK_HEADER_VERBOSE", "1")],
    );
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "pane run exit 0: stderr={}", out.stderr);
    let _message_id = v["message_id"].as_str().expect("message_id");

    assert!(
        wait_for_pane_text(
            &fixture.socket_path,
            &pane,
            HEADER_NOTE,
            Duration::from_secs(5)
        ),
        "ZYNK_HEADER_VERBOSE=1 must SHOW the awareness note; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );
    assert!(
        pane_recent_text(&fixture.socket_path, &pane).contains("reply: zynk reply"),
        "ZYNK_HEADER_VERBOSE=1 must SHOW the reply affordance; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );

    fixture.cleanup();
}

/// A plain shell pane (no `agent_session`, no agent label) is NOT an agent target, so
/// it gets the bare body — no header is prepended.
#[test]
fn no_header_for_plain_shell_pane() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "header-plain");
    // NO report_agent_session → no agent identity → not an agent target.

    let out = run_cli(
        &fixture,
        None,
        &["pane", "run", &pane, "--", "zbodysentinel", "hi"],
    );
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "pane run exit 0: stderr={}", out.stderr);
    let _message_id = v["message_id"].as_str().expect("message_id");

    assert!(
        wait_for_pane_text(
            &fixture.socket_path,
            &pane,
            "zbodysentinel",
            Duration::from_secs(5)
        ),
        "the body should still be submitted to a plain shell pane"
    );
    assert!(
        !pane_recent_text(&fixture.socket_path, &pane).contains(HEADER_TOP),
        "a plain shell pane must NOT get a header; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );

    fixture.cleanup();
}

/// `pane send-text` (a DRAFT) is NEVER wire-headered — even to an agent target — so the
/// pane text stays byte-exact (no header box). BUT `protocol_json` still carries the
/// protocol IDs uniformly (ADR 0005).
#[test]
fn pane_send_text_no_header_but_protocol_json_has_ids() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "header-draft");
    report_agent_session(&fixture.socket_path, &pane, "zynk:pi", "pi", "sess-draft");

    let out = run_cli(
        &fixture,
        None,
        &["pane", "send-text", &pane, "--", "draftbody"],
    );
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "pane send-text exit 0: stderr={}", out.stderr);
    assert_eq!(v["delivery_status"], "drafted", "{v}");
    let message_id = v["message_id"].as_str().expect("message_id").to_string();

    // The draft is staged into the pane but must carry NO header box.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pane_recent_text(&fixture.socket_path, &pane).contains(HEADER_TOP),
        "a draft (pane send-text) must NEVER be wire-headered; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );

    // …but the structured protocol_json still carries the protocol IDs (uniformity).
    let protocol = protocol_json_of(&fixture, &message_id);
    assert!(
        protocol.get("message_id").is_some(),
        "protocol_json must carry protocol IDs even for drafts (ADR 0005): {protocol}"
    );

    fixture.cleanup();
}

/// DIRECT `agent send` coverage: the header injection lives in the CLI `agent_send`
/// path too, not only `pane run`. A `agent send` to a label-resolvable agent target
/// (`agent.get("pi")` resolves the pane via hook authority) PREPENDS the header — while
/// `messages.body` stays pure and `protocol_json` carries the protocol IDs.
#[test]
fn header_prepended_for_agent_send() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "header-send");
    // `pane.report_agent` (NOT `pane.report_agent_session`) so `agent.get("pi")`
    // RESOLVES this pane (hook authority) — `agent send` needs that.
    report_hook_agent_session(&fixture.socket_path, &pane, "zynk:pi", "pi", "sess-send");

    let out = run_cli(
        &fixture,
        None,
        &["agent", "send", "pi", "--", "zbodysentinel", "hi"],
    );
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "agent send exit 0: stderr={}", out.stderr);
    let message_id = v["message_id"].as_str().expect("message_id").to_string();

    // The delivered pane text carries the visible header box.
    assert!(
        wait_for_pane_text(
            &fixture.socket_path,
            &pane,
            HEADER_TOP,
            Duration::from_secs(5)
        ),
        "receipt-uniform agent send must PREPEND the header box; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );
    assert!(
        !pane_recent_text(&fixture.socket_path, &pane).contains(HEADER_NOTE),
        "agent send header HIDES the awareness note by default; pane text: {:?}",
        pane_recent_text(&fixture.socket_path, &pane)
    );

    // The persisted body stays pure (no header on the wire-only injection).
    assert_eq!(
        body_of(&fixture, &message_id),
        "zbodysentinel hi",
        "messages.body must stay the pure body (no header)"
    );

    // Protocol IDs are present in protocol_json.
    let protocol = protocol_json_of(&fixture, &message_id);
    assert!(
        protocol.get("message_id").is_some(),
        "protocol_json must carry protocol IDs (message_id): {protocol}"
    );

    fixture.cleanup();
}
