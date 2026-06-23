//! zynk fork (ADR 0002) M5a integration tests: the native `zynk zynk query`
//! command — an IN-PROCESS, READ-ONLY lexical/BM25 query over `messages_fts`.
//!
//! These exercise the *integration* truth the in-crate unit tests can't: that a
//! message SUBMITTED via the send path (`agent send`/`pane run`, which persists +
//! FTS-indexes in the same txn) is IMMEDIATELY retrievable by `zynk query`, that
//! the metadata prefilters restrict the candidate set, that the F4 envelope and
//! exit codes are stable, and that a query NEVER writes a delivery event (the
//! read-only / `PRAGMA query_only=1` invariant).
//!
//! The fixture is copied from `tests/zynk_receipt.rs`: an ISOLATED dev server is
//! spawned under `/tmp` with an isolated `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`/
//! `ZYNK_SOCKET_PATH`/`ZYNK_SQLITE_HOME`; the `zynk` CLI is driven as a
//! subprocess with the SAME `ZYNK_SQLITE_HOME` so the send-side persistence and
//! the in-process `zynk query` read hit the SAME DB file. The whole suite is
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
        "/tmp/zynk-query-test-{}-{nanos}",
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

/// A fully-isolated server + its runtime paths. Dropping it (via `cleanup` OR the
/// `Drop` impl on a panicking test) kills the server and removes the base dir.
struct Fixture {
    base: PathBuf,
    config_home: PathBuf,
    runtime_dir: PathBuf,
    socket_path: PathBuf,
    sqlite_home: PathBuf,
    server: Option<SpawnedZynk>,
    /// The `ZYNK_EMBED_PROVIDER` this fixture's server + queries run under (FIX 1).
    /// `Some("fastembed")` → an UNCOMPILED provider in the default build, so the
    /// embedding worker exits at boot (no vec0, no model row, no embeddings): the
    /// vector index is structurally absent → queries deterministically degrade to
    /// honest BM25 (no boot-sweep race). `None` → the default fake embedder (the
    /// hybrid e2e fixture, which must still reach `ranking="rrf"`). The query
    /// subprocess sets the SAME provider so `active_model_id()` matches the enqueued
    /// job's model_id and `pending_jobs` is counted consistently.
    embed_provider: Option<&'static str>,
}

impl Fixture {
    /// Tear down the server + base dir. Idempotent (safe to call then have `Drop`
    /// fire, or vice versa): the server `Option` and a `cleaned` flag guard against
    /// a double run. Mirrors the `Drop` logic.
    fn cleanup(mut self) {
        self.teardown();
    }

    /// The shared teardown body (FIX 4): drop the server child, then sweep the base
    /// dir + runtime-dir servers. Idempotent via the `server`/`base` take/clear so a
    /// later `Drop` is a no-op.
    fn teardown(&mut self) {
        if let Some(server) = self.server.take() {
            drop(server);
        }
        // `base` empty == already cleaned; clear it after so `Drop` doesn't re-sweep.
        if !self.base.as_os_str().is_empty() {
            cleanup_test_base(&self.base);
            self.base = PathBuf::new();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // FIX 4: on a panicking/asserting test the explicit `cleanup()` never runs, so
        // without this the `/tmp` base dir + the runtime-dir server sweep would leak.
        // Idempotent: a no-op when `cleanup()` already tore everything down.
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

/// The LEXICAL / BM25-only fixture (the M5a default, FIX 1): the server runs under
/// `ZYNK_EMBED_PROVIDER=fastembed`, which is UNCOMPILED in the default build (no
/// `fastembed` feature) → `embedder_from_env()` returns `ModelUnavailable`, so the
/// embedding worker exits at boot WITHOUT registering vec0 / creating a model row /
/// embedding anything. The vector index is structurally absent, so every query
/// deterministically degrades to `ranking="bm25"` + `vector_index.ready=false`. This
/// kills the boot-sweep race (the worker can't embed AT ALL — not "embeds late"), and
/// is a real, realistic config (a server set to fastembed but built without it). The
/// `ZYNK_EMBED_POLL_MS` pin is now MOOT (the worker never reaches the poll loop), so we
/// drop it. Queries on this fixture also set `ZYNK_EMBED_PROVIDER=fastembed` so
/// `active_model_id()` resolves the SAME id ("multilingual-e5-small@1") the send
/// enqueued for → `pending_jobs` is counted consistently.
fn spawn_fixture() -> Fixture {
    spawn_fixture_with(Some("3600000"), Some("fastembed"))
}

/// The FAST-WORKER fixture (M5c hybrid e2e): default (fake) embedder, fully functional,
/// the worker polls every `50` ms and embeds a freshly-sent message within ~1s. NOT
/// poll-pinned to "never embed" — paired with a BOUNDED POLL (`poll_query_until`),
/// never a fixed sleep, to assert the system REACHES the hybrid (RRF) state
/// deterministically (ARB-M5B-001 lesson). `embed_provider = None` → the default fake
/// embedder, so this MUST still reach `ranking="rrf"`.
fn spawn_fast_worker_fixture() -> Fixture {
    spawn_fixture_with(Some("50"), None)
}

fn spawn_fixture_with(
    embed_poll_ms: Option<&str>,
    embed_provider: Option<&'static str>,
) -> Fixture {
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

    let server = spawn_server_process(
        &config_home,
        &runtime_dir,
        &socket_path,
        &sqlite_home,
        embed_poll_ms,
        embed_provider,
    );

    Fixture {
        base,
        config_home,
        runtime_dir,
        socket_path,
        sqlite_home,
        server: Some(server),
        embed_provider,
    }
}

fn spawn_server_process(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
    sqlite_home: &Path,
    embed_poll_ms: Option<&str>,
    embed_provider: Option<&str>,
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
    // M5c: the server spawns the App-owned embedding worker. Two fixture configs:
    //   - LEXICAL / BM25-only (FIX 1): `ZYNK_EMBED_PROVIDER=fastembed`, which is
    //     UNCOMPILED in the default build → `embedder_from_env()` is ModelUnavailable →
    //     the worker exits at boot WITHOUT registering vec0 / a model row / embedding
    //     anything. The vector index is structurally absent, so the M5a lexical tests +
    //     the partial-freshness fallback test deterministically observe the no-vector
    //     (vector_index.ready=false → ranking=="bm25") state — with NO boot-sweep race
    //     (the worker can't embed at all). The `ZYNK_EMBED_POLL_MS` pin is moot here.
    //   - FAST / fake (e2e): provider unset → the default fake embedder, fully
    //     functional; `ZYNK_EMBED_POLL_MS=50` so it embeds a freshly-sent message in
    //     ~1s and the C4 hybrid e2e test (BOUNDED POLL, never a fixed sleep) observes
    //     the system REACH the hybrid (RRF) state.
    if let Some(ms) = embed_poll_ms {
        cmd.env("ZYNK_EMBED_POLL_MS", ms);
    }
    match embed_provider {
        Some(provider) => {
            cmd.env("ZYNK_EMBED_PROVIDER", provider);
        }
        None => {
            cmd.env_remove("ZYNK_EMBED_PROVIDER");
        }
    }

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

/// Create a workspace + focused root pane, returning the root pane id. Each
/// `workspace.create` yields a DISTINCT workspace_id (→ distinct conversation
/// scope), which the workspace/conversation prefilter tests rely on.
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

/// Register a pane as an agent terminal under `label` so `agent.get` resolves it
/// (sets the hook authority → `is_agent_terminal()` is true), which the send path
/// needs to address the agent by label.
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

struct CliOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Drive the `zynk` CLI binary against this fixture's isolated socket + DB. The
/// `ZYNK_SQLITE_HOME` it sets is the SAME the server uses, so the send-side
/// persistence and the in-process `zynk query` read hit the SAME `zynk.db`. When
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
    // FIX 1: drive every subprocess (send + query) under the SAME embed provider the
    // server runs under. For `query`, this makes the in-process `active_model_id()`
    // resolve the same model_id the server enqueued the embedding job for, so
    // `pending_jobs` is counted consistently. `None` → the default fake.
    match fixture.embed_provider {
        Some(provider) => {
            command.env("ZYNK_EMBED_PROVIDER", provider);
        }
        None => {
            command.env_remove("ZYNK_EMBED_PROVIDER");
        }
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

/// Count rows in `delivery_events` — used to pin the read-only invariant (a query
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

/// Submit a message to `agent` (must be `report_agent`'d first) with `type`/`body`
/// and return the message_id. Asserts the send was `submitted` (the precondition
/// for the message to be FTS-indexed and thus queryable).
fn agent_send(fixture: &Fixture, agent: &str, msg_type: &str, body: &str) -> String {
    let out = run_cli(
        fixture,
        None,
        &["agent", "send", agent, "--type", msg_type, "--", body],
    );
    let v = parse_outcome(&out);
    assert_eq!(out.code, 0, "agent send exit 0: stderr={}", out.stderr);
    assert_eq!(
        v["delivery_status"], "submitted",
        "agent send must submit (precondition for FTS freshness): {v}"
    );
    v["message_id"]
        .as_str()
        .expect("message_id on send outcome")
        .to_string()
}

/// Run `zynk query <args> --json` and return the parsed F4 envelope. Asserts a
/// JSON line is on stdout.
fn query_json(fixture: &Fixture, args: &[&str]) -> Value {
    let full: Vec<&str> = [&["zynk", "query"][..], args, &["--json"][..]].concat();
    let out = run_cli(fixture, None, &full);
    parse_outcome(&out)
}

/// Run `zynk query <args>` (no `--json` forced) and return the raw output for
/// human-text / exit-code assertions.
fn query_raw(fixture: &Fixture, args: &[&str]) -> CliOutput {
    let full: Vec<&str> = [&["zynk", "query"][..], args].concat();
    run_cli(fixture, None, &full)
}

/// BOUNDED POLL (the ARB-M5B-001 lesson): repeatedly run `zynk query <args> --json`,
/// re-parsing the F4 JSON each iteration, until `predicate(&json)` holds OR `deadline`
/// elapses. Returns the FINAL JSON either way (so a deadline-hit failure can surface
/// the last observed state) plus whether the predicate was satisfied and how long it
/// took. This is NOT a fixed sleep — it asserts the system REACHES the target state
/// within a generous bound (the fast worker + instant FakeEmbedder make this <1s in
/// practice; the bound only guards a real regression).
fn poll_query_until(
    fixture: &Fixture,
    args: &[&str],
    deadline: Duration,
    poll_every: Duration,
    predicate: impl Fn(&Value) -> bool,
) -> (Value, bool, Duration) {
    let start = Instant::now();
    let mut last = query_json(fixture, args);
    loop {
        if predicate(&last) {
            return (last, true, start.elapsed());
        }
        if start.elapsed() >= deadline {
            return (last, false, start.elapsed());
        }
        std::thread::sleep(poll_every);
        last = query_json(fixture, args);
    }
}

// 1. FTS freshness: a just-sent message is immediately retrievable, and the F4
//    envelope carries the expected provenance.
#[test]
fn fts_freshness_query_returns_just_sent_message() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-fresh");
    report_agent(&fixture.socket_path, &pane, "codex");

    let mid = agent_send(&fixture, "codex", "review", "zsentinelalpha hello world");

    let v = query_json(&fixture, &["zsentinelalpha"]);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "zynk query", "{v}");
    assert_eq!(v["type"], "zynk_query_result", "{v}");
    assert_eq!(v["ranking"], "bm25", "{v}");
    assert!(
        v["count"].as_u64().unwrap_or(0) >= 1,
        "just-sent message must be immediately retrievable: {v}"
    );
    assert_eq!(
        v["results"][0]["message_id"], mid,
        "the top hit must be the just-sent message: {v}"
    );
    assert!(
        v["results"][0]["from"].is_string(),
        "results[0].from present: {v}"
    );
    assert!(
        v["results"][0]["to"].is_string(),
        "results[0].to present: {v}"
    );
    assert!(
        v["results"][0]["delivery_status"].is_string(),
        "results[0].delivery_status present (latest delivery event): {v}"
    );
    assert!(v["next"].is_string(), "next is a string: {v}");

    fixture.cleanup();
}

// 2. `--type` prefilter restricts the candidate set before ranking.
#[test]
fn prefilter_type_restricts_results() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-type");
    report_agent(&fixture.socket_path, &pane, "codex");

    let review_id = agent_send(&fixture, "codex", "review", "zsentinelgamma a");
    let _question_id = agent_send(&fixture, "codex", "question", "zsentinelgamma b");

    let v = query_json(&fixture, &["--type", "review", "zsentinelgamma"]);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(
        v["count"], 1,
        "--type review must keep only the review send: {v}"
    );
    assert_eq!(
        v["results"][0]["message_id"], review_id,
        "the surviving hit must be the review message: {v}"
    );

    fixture.cleanup();
}

// 3. `--agent` prefilter restricts to messages touching that agent label.
#[test]
fn prefilter_agent_restricts_results() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // Each agent gets its OWN pane (a pane carries a single authoritative agent
    // label; re-reporting a second label on one pane just overwrites the first).
    // Distinct panes mean distinct workspaces/conversations, but `--agent` filters
    // on the participant label across conversations, so both remain queryable.
    let codex_pane = create_root_pane(&fixture.socket_path, "query-agent-codex");
    report_agent(&fixture.socket_path, &codex_pane, "codex");
    let kimi_pane = create_root_pane(&fixture.socket_path, "query-agent-kimi");
    report_agent(&fixture.socket_path, &kimi_pane, "kimi");

    let _codex_id = agent_send(&fixture, "codex", "review", "zsentineldelta to codex");
    let kimi_id = agent_send(&fixture, "kimi", "review", "zsentineldelta to kimi");

    let v = query_json(&fixture, &["--agent", "kimi", "zsentineldelta"]);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(
        v["count"], 1,
        "--agent kimi must keep only the kimi-addressed send: {v}"
    );
    assert_eq!(
        v["results"][0]["message_id"], kimi_id,
        "the surviving hit must be the kimi message: {v}"
    );
    assert_eq!(
        v["results"][0]["to"], "kimi",
        "the surviving hit must be addressed to kimi: {v}"
    );

    fixture.cleanup();
}

// 4. `--since` filter: a far-future bound returns an empty (but OK) result; a
//    far-past bound keeps the match.
#[test]
fn prefilter_since_future_returns_empty() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-since");
    report_agent(&fixture.socket_path, &pane, "codex");
    let _mid = agent_send(&fixture, "codex", "review", "zsentinelecho hi");

    let future = query_json(
        &fixture,
        &["--since", "2999-01-01T00:00:00Z", "zsentinelecho"],
    );
    assert_eq!(
        future["result"], "ok",
        "future-since is still a valid query: {future}"
    );
    assert_eq!(
        future["count"], 0,
        "no message is after year 2999: {future}"
    );
    assert_eq!(
        future["results"],
        serde_json::json!([]),
        "future-since results must be the empty array: {future}"
    );

    let past = query_json(&fixture, &["--since", "2000-01-01", "zsentinelecho"]);
    assert_eq!(past["result"], "ok", "{past}");
    assert!(
        past["count"].as_u64().unwrap_or(0) >= 1,
        "a year-2000 since-bound must keep the recent message: {past}"
    );

    fixture.cleanup();
}

// 5. A no-match query is a successful empty result with exit 0 (not an error).
#[test]
fn no_match_is_ok_empty() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-nomatch");
    report_agent(&fixture.socket_path, &pane, "codex");
    let _mid = agent_send(&fixture, "codex", "review", "zsentinelfoo bar");

    let out = query_raw(&fixture, &["znonexistenttoken9999", "--json"]);
    assert_eq!(out.code, 0, "no-match must exit 0: stderr={}", out.stderr);
    let v = parse_outcome(&out);
    assert_eq!(v["result"], "ok", "no match is not an error: {v}");
    assert_eq!(v["count"], 0, "no match → count 0: {v}");

    fixture.cleanup();
}

// 6. Empty query text is `invalid_query`, exit 1.
#[test]
fn invalid_query_for_empty_text() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let out = query_raw(&fixture, &["--json"]);
    assert_eq!(
        out.code, 1,
        "empty query must exit 1: stderr={}",
        out.stderr
    );
    let v = parse_outcome(&out);
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(v["code"], "invalid_query", "{v}");
    assert_eq!(v["command"], "zynk query", "{v}");

    fixture.cleanup();
}

// 6b. Malformed FTS5 MATCH expressions are `invalid_query` (not `db_unavailable`).
#[test]
fn malformed_fts_query_is_invalid_query() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // Each is a malformed FTS5 MATCH that the bundled SQLite reports WITHOUT an
    // "fts5"/"syntax error" substring (unbalanced quote -> "unterminated string";
    // bad column filter -> "no such column"; trailing operator -> fts5 syntax
    // error). All must classify as invalid_query (caller error), NOT db_unavailable
    // — routed on the SQLITE_ERROR (code 1) raised while evaluating MATCH.
    for bad in ["foo\"bar", "nope: hello", "hello AND"] {
        let out = query_raw(&fixture, &[bad, "--json"]);
        assert_eq!(
            out.code, 1,
            "malformed FTS {bad:?} must exit 1: stdout={} stderr={}",
            out.stdout, out.stderr
        );
        let v = parse_outcome(&out);
        assert_eq!(v["result"], "failed", "{bad:?} -> {v}");
        assert_eq!(v["command"], "zynk query", "{bad:?} -> {v}");
        assert_eq!(
            v["code"], "invalid_query",
            "malformed FTS {bad:?} must be invalid_query: {v}"
        );
        // The failure envelope omits the success-only `type` discriminator (plan §7).
        assert!(v.get("type").is_none(), "{bad:?} failure has no type: {v}");
    }

    fixture.cleanup();
}

// 7. Bad `--since` / `--limit` are `invalid_filter`, exit 1, rejected before any
//    DB access.
#[test]
fn invalid_filter_for_bad_since() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let bad_since = query_raw(&fixture, &["zsentinelx", "--since", "not-a-date", "--json"]);
    assert_eq!(
        bad_since.code, 1,
        "bad --since must exit 1: stderr={}",
        bad_since.stderr
    );
    let v = parse_outcome(&bad_since);
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(v["code"], "invalid_filter", "{v}");

    let bad_limit = query_raw(&fixture, &["zsentinelx", "--limit", "abc", "--json"]);
    assert_eq!(
        bad_limit.code, 1,
        "bad --limit must exit 1: stderr={}",
        bad_limit.stderr
    );
    let v2 = parse_outcome(&bad_limit);
    assert_eq!(v2["result"], "failed", "{v2}");
    assert_eq!(v2["code"], "invalid_filter", "{v2}");

    fixture.cleanup();
}

// 8. The success JSON envelope has EXACTLY the F4 top-level keys.
#[test]
fn json_envelope_stability() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-envelope");
    report_agent(&fixture.socket_path, &pane, "codex");
    let _mid = agent_send(&fixture, "codex", "review", "zsentinelenvelope hi");

    let v = query_json(&fixture, &["zsentinelenvelope"]);
    for key in [
        "result", "command", "type", "query", "filters", "ranking", "count", "results", "next",
    ] {
        assert!(
            v.get(key).is_some(),
            "F4 success envelope must carry top-level key {key:?}: {v}"
        );
    }
    assert_eq!(v["command"], "zynk query", "{v}");
    // failure-only fields must be absent on success.
    assert!(v.get("code").is_none(), "no code on success: {v}");
    assert!(v.get("message").is_none(), "no message on success: {v}");

    fixture.cleanup();
}

// 9. Human (non-JSON) output is non-empty and informative for a match; the
//    no-match case says "0 results".
#[test]
fn human_output_non_empty() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-human");
    report_agent(&fixture.socket_path, &pane, "codex");
    let mid = agent_send(&fixture, "codex", "review", "zsentinelhuman hi");

    let hit = query_raw(&fixture, &["zsentinelhuman"]);
    assert_eq!(hit.code, 0, "human match exit 0: stderr={}", hit.stderr);
    assert!(
        !hit.stdout.trim().is_empty(),
        "human output must be non-empty"
    );
    assert!(
        hit.stdout.contains(&mid) || hit.stdout.contains("result(s)"),
        "human output must reference the message_id or a result count: {:?}",
        hit.stdout
    );

    let miss = query_raw(&fixture, &["znohumantoken9999"]);
    assert_eq!(
        miss.code, 0,
        "human no-match exit 0: stderr={}",
        miss.stderr
    );
    assert!(
        miss.stdout.contains("0 results"),
        "human no-match output must say \"0 results\": {:?}",
        miss.stdout
    );

    fixture.cleanup();
}

// 10. A query writes ZERO delivery events — pins the read-only / no-recovery
//     invariant (`PRAGMA query_only=1`).
#[test]
fn query_writes_zero_delivery_events() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-readonly");
    report_agent(&fixture.socket_path, &pane, "codex");
    let _mid = agent_send(&fixture, "codex", "review", "zsentinelreadonly hi");

    let before = delivery_events_count(&fixture);
    assert!(
        before >= 1,
        "the seeded submit must have recorded at least one delivery event"
    );

    // Several reads: a hit, a miss, and a filtered query.
    let _ = query_raw(&fixture, &["zsentinelreadonly", "--json"]);
    let _ = query_raw(&fixture, &["znothinghere0000", "--json"]);
    let _ = query_raw(
        &fixture,
        &["--type", "review", "zsentinelreadonly", "--json"],
    );
    let _ = query_raw(&fixture, &["--limit", "5", "zsentinelreadonly", "--json"]);

    let after = delivery_events_count(&fixture);
    assert_eq!(
        before, after,
        "a read-only query must NEVER synthesize a delivery/recovery event \
         (before={before} after={after})"
    );

    fixture.cleanup();
}

// 11. BM25 orders the more-relevant (repeated-token) message first; both hits get
//     distinct, contiguous bm25_rank (1, 2).
#[test]
fn bm25_orders_more_relevant_first() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-bm25");
    report_agent(&fixture.socket_path, &pane, "codex");

    let dense_id = agent_send(
        &fixture,
        "codex",
        "review",
        "zsentinelzeta zsentinelzeta zsentinelzeta",
    );
    let sparse_id = agent_send(&fixture, "codex", "review", "zsentinelzeta once");

    let v = query_json(&fixture, &["zsentinelzeta"]);
    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["count"], 2, "both zsentinelzeta messages must match: {v}");

    // Ranks must be the distinct, contiguous 1 and 2.
    assert_eq!(v["results"][0]["bm25_rank"], 1, "{v}");
    assert_eq!(v["results"][1]["bm25_rank"], 2, "{v}");

    // The repeated-token (denser) message is the more relevant top hit.
    assert_eq!(
        v["results"][0]["message_id"], dense_id,
        "the repeated-token message must rank first (more relevant): {v}"
    );
    assert_eq!(
        v["results"][1]["message_id"], sparse_id,
        "the single-token message must rank second: {v}"
    );

    fixture.cleanup();
}

// ARB-M5A-001: an unknown query flag is an F4 invalid_filter (NOT raw usage/exit 2),
// and --json is honored regardless of where it appears.
#[test]
fn unknown_flag_is_f4_invalid_filter() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    for args in [
        vec!["--bogus", "--json"],
        vec!["--json", "--bogus"], // --json BEFORE the bad flag
        vec!["zsentinelu", "--bogus", "--json"], // --json AFTER the bad flag
    ] {
        let out = query_raw(&fixture, &args);
        assert_ne!(
            out.code, 0,
            "unknown flag must exit nonzero: args={args:?} stderr={}",
            out.stderr
        );
        let v = parse_outcome(&out); // F4 JSON on stdout, not raw stderr usage
        assert_eq!(v["result"], "failed", "args={args:?} -> {v}");
        assert_eq!(v["command"], "zynk query", "args={args:?} -> {v}");
        assert_eq!(v["code"], "invalid_filter", "args={args:?} -> {v}");
        assert_eq!(v["context"]["flag"], "--bogus", "args={args:?} -> {v}");
    }

    fixture.cleanup();
}

// Non-blocking coverage: --workspace and --conversation prefilters restrict.
#[test]
fn prefilter_workspace_and_conversation_restrict() {
    let _guard = test_lock();
    let fixture = spawn_fixture();

    // Two panes => two workspaces => two conversations. Distinct agent labels so
    // `agent send` resolves unambiguously.
    let pane_a = create_root_pane(&fixture.socket_path, "ws-a");
    report_agent(&fixture.socket_path, &pane_a, "codex");
    let pane_b = create_root_pane(&fixture.socket_path, "ws-b");
    report_agent(&fixture.socket_path, &pane_b, "kimi");

    let codex_id = agent_send(&fixture, "codex", "review", "zsentinelws shared");
    let _kimi_id = agent_send(&fixture, "kimi", "review", "zsentinelws shared");

    let all = query_json(&fixture, &["zsentinelws"]);
    assert_eq!(all["count"], 2, "both sends match unfiltered: {all}");

    let hits = all["results"].as_array().unwrap();
    let codex_hit = hits
        .iter()
        .find(|h| h["to"] == "codex")
        .expect("codex hit present");
    let ws = codex_hit["workspace_id"].as_str().unwrap();
    let conv = codex_hit["conversation_id"].as_str().unwrap();

    let by_ws = query_json(&fixture, &["--workspace", ws, "zsentinelws"]);
    assert_eq!(by_ws["count"], 1, "--workspace restricts to one: {by_ws}");
    assert_eq!(by_ws["results"][0]["message_id"], codex_id, "{by_ws}");

    let by_conv = query_json(&fixture, &["--conversation", conv, "zsentinelws"]);
    assert_eq!(
        by_conv["count"], 1,
        "--conversation restricts to one: {by_conv}"
    );
    assert_eq!(by_conv["results"][0]["message_id"], codex_id, "{by_conv}");

    fixture.cleanup();
}

// Non-blocking coverage: --exact phrase match vs wrong-order no-match.
#[test]
fn exact_phrase_match_and_no_match() {
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "exact");
    report_agent(&fixture.socket_path, &pane, "codex");
    agent_send(
        &fixture,
        "codex",
        "review",
        "zexactalpha zexactbeta zexactgamma",
    );

    let yes = query_json(&fixture, &["--exact", "zexactbeta zexactgamma"]);
    assert_eq!(yes["count"], 1, "a contiguous phrase must match: {yes}");
    let no = query_json(&fixture, &["--exact", "zexactgamma zexactalpha"]);
    assert_eq!(
        no["count"], 0,
        "a non-contiguous phrase must NOT match: {no}"
    );

    fixture.cleanup();
}

// Non-blocking coverage: the FTS5 snippet brackets the matched token.
#[test]
fn snippet_brackets_the_match() {
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "snip");
    report_agent(&fixture.socket_path, &pane, "codex");
    agent_send(&fixture, "codex", "review", "before zsentinelsnip after");

    let v = query_json(&fixture, &["zsentinelsnip"]);
    let snip = v["results"][0]["snippet"].as_str().unwrap_or("");
    assert!(
        snip.contains("[zsentinelsnip]"),
        "snippet must bracket the matched token: {snip:?}"
    );

    fixture.cleanup();
}

// Non-blocking coverage: e2e --limit caps a larger result set.
#[test]
fn limit_caps_result_set() {
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "limit");
    report_agent(&fixture.socket_path, &pane, "codex");
    agent_send(&fixture, "codex", "review", "zsentinellim one");
    agent_send(&fixture, "codex", "review", "zsentinellim two");
    agent_send(&fixture, "codex", "review", "zsentinellim three");

    let capped = query_json(&fixture, &["zsentinellim", "--limit", "2"]);
    assert_eq!(
        capped["count"], 2,
        "--limit 2 caps a 3-message set: {capped}"
    );
    let uncapped = query_json(&fixture, &["zsentinellim"]);
    assert_eq!(
        uncapped["count"], 3,
        "unfiltered returns all three: {uncapped}"
    );

    fixture.cleanup();
}

// Non-blocking coverage: --branch binds + restricts (fixture messages have NULL
// branch, so a non-empty --branch excludes them).
#[test]
fn branch_filter_excludes_null_branch() {
    let _guard = test_lock();
    let fixture = spawn_fixture();
    let pane = create_root_pane(&fixture.socket_path, "branch");
    report_agent(&fixture.socket_path, &pane, "codex");
    agent_send(&fixture, "codex", "review", "zsentinelbr token");

    let none = query_json(&fixture, &["--branch", "nonexistent-branch", "zsentinelbr"]);
    assert_eq!(
        none["count"], 0,
        "--branch on NULL-branch messages excludes them: {none}"
    );
    let any = query_json(&fixture, &["zsentinelbr"]);
    assert!(
        any["count"].as_i64().unwrap() >= 1,
        "unfiltered finds the message: {any}"
    );

    fixture.cleanup();
}

// C4 (M5c) — Partial-freshness / HONEST BM25 fallback. Uses the LEXICAL fixture
// (FIX 1): the server runs under `ZYNK_EMBED_PROVIDER=fastembed`, which is UNCOMPILED
// in the default build → the embedding worker exits at boot and NEVER embeds. The
// just-sent message therefore has NO vector at query time, structurally (not "the
// worker embeds late") — so there is NO boot-sweep race. The query MUST honestly
// report BM25-only ranking (no false rrf claim) AND an honest vector_index
// (ready=false, pending_jobs>=1 for the enqueued-but-not-done job, model_id a string —
// here "multilingual-e5-small@1", the fastembed model id the send enqueued for and the
// query resolves via `active_model_id()`).
#[test]
fn partial_freshness_honest_bm25_fallback() {
    let _guard = test_lock();
    let fixture = spawn_fixture(); // fastembed (uncompiled): the worker can't embed at all

    let pane = create_root_pane(&fixture.socket_path, "query-partial-fresh");
    report_agent(&fixture.socket_path, &pane, "codex");

    let mid = agent_send(
        &fixture,
        "codex",
        "review",
        "zpartialfreshtoken hello world",
    );

    // Query IMMEDIATELY: the message's embedding job is enqueued (pending) but the
    // worker (fastembed, uncompiled) exited at boot, so the vector index has no vector
    // for it — and never will in this build.
    let v = query_json(&fixture, &["zpartialfreshtoken"]);

    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["command"], "zynk query", "{v}");
    // HONESTY: no false rrf claim while vectors are still landing.
    assert_eq!(
        v["ranking"], "bm25",
        "no vector landed → must report BM25-only, never a false rrf: {v}"
    );

    // The vector_index block is present and honest.
    let vi = &v["vector_index"];
    assert!(vi.is_object(), "vector_index present on success: {v}");
    assert_eq!(
        vi["ready"], false,
        "vectors not landed (pending job) → ready=false: {v}"
    );
    assert!(
        vi["pending_jobs"].as_i64().unwrap_or(0) >= 1,
        "the just-sent message's enqueued-but-not-done job must count as pending: {v}"
    );
    // model_id is the configured (fastembed) model id the send enqueued for and the
    // query resolves via `active_model_id()` — a non-empty string.
    let model_id = vi["model_id"].as_str().unwrap_or("");
    assert!(
        !model_id.is_empty(),
        "model_id is a non-empty string (here \"multilingual-e5-small@1\"): {v}"
    );
    assert_eq!(
        model_id, "multilingual-e5-small@1",
        "the configured fastembed provider resolves this model id: {v}"
    );

    // The sent message is a BM25 hit: bm25_rank set, NO vector_rank.
    assert!(
        v["count"].as_u64().unwrap_or(0) >= 1,
        "the just-sent message must be retrievable via BM25: {v}"
    );
    let hit = &v["results"][0];
    assert_eq!(
        hit["message_id"], mid,
        "the top hit is the just-sent msg: {v}"
    );
    assert!(
        hit["bm25_rank"].is_number(),
        "the BM25 hit carries a bm25_rank: {v}"
    );
    assert!(
        hit.get("vector_rank").is_none(),
        "no vector landed → the hit must NOT carry a vector_rank: {v}"
    );
    // matched_modes is bm25-only (no false vector mode).
    assert_eq!(
        hit["matched_modes"],
        serde_json::json!(["bm25"]),
        "a BM25-only hit lists only the bm25 mode: {v}"
    );

    fixture.cleanup();
}

// C4 (M5c) — Hybrid RRF end-to-end. Uses the FAST-WORKER fixture + a BOUNDED POLL
// (never a fixed sleep). A message with a distinctive multi-token body is sent; the
// worker embeds it within ~1s. We poll `zynk query` until ranking becomes "rrf" (the
// vector index gains the row → the query goes hybrid), bounded at 20s. With the
// instant FakeEmbedder a 50ms-poll worker reaches this in <1s, so the bound only
// guards a real regression.
//
// With FakeEmbedder, a query for the EXACT body embeds to the SAME vector the worker
// stored → the message is the vector top hit AND a lexical (bm25) hit → a "both-mode"
// rrf hit. (A vector-only / lexical-only PARTITION is covered by the vector.rs DB
// tests + the fuse_results unit tests, so it is NOT reconstructed here.)
#[test]
fn hybrid_rrf_end_to_end() {
    let _guard = test_lock();
    let fixture = spawn_fast_worker_fixture(); // 50ms poll: embeds the send in ~1s

    let pane = create_root_pane(&fixture.socket_path, "query-hybrid-e2e");
    report_agent(&fixture.socket_path, &pane, "codex");

    let body = "hybrid embedding fusion sentinel";
    let mid = agent_send(&fixture, "codex", "review", body);

    // BOUNDED POLL: assert the system REACHES the hybrid (RRF) state within 20s.
    let (v, reached, took) = poll_query_until(
        &fixture,
        &[body],
        Duration::from_secs(20),
        Duration::from_millis(250),
        |json| json["ranking"] == "rrf",
    );
    assert!(
        reached,
        "query never reached ranking==\"rrf\" within 20s (worker should embed the body \
         and the vector index gain the row) — last JSON: {v}"
    );
    println!("hybrid_rrf_end_to_end: ranking reached \"rrf\" in {took:?}");

    assert_eq!(v["result"], "ok", "{v}");
    assert_eq!(v["ranking"], "rrf", "{v}");

    // The sent message is in the results as a BOTH-mode hit.
    let hits = v["results"].as_array().expect("results array");
    let hit = hits
        .iter()
        .find(|h| h["message_id"] == serde_json::json!(mid))
        .unwrap_or_else(|| panic!("the sent message must be in the rrf results: {v}"));
    assert!(
        hit["bm25_rank"].is_number(),
        "the hybrid hit carries a bm25_rank (Some): {v}"
    );
    assert!(
        hit["vector_rank"].is_number(),
        "the hybrid hit carries a vector_rank (Some): {v}"
    );
    let modes = hit["matched_modes"]
        .as_array()
        .expect("matched_modes array");
    assert!(
        modes.iter().any(|m| m == "bm25") && modes.iter().any(|m| m == "vector"),
        "the both-mode hit's matched_modes must contain BOTH bm25 and vector: {v}"
    );

    // Once embedded, the vector index is fully ready and the queue is drained.
    let vi = &v["vector_index"];
    assert_eq!(
        vi["ready"], true,
        "after embedding lands and the queue drains, vector_index.ready=true: {v}"
    );
    assert_eq!(
        vi["pending_jobs"], 0,
        "the message's job is done → pending_jobs back to 0: {v}"
    );

    fixture.cleanup();
}

// C4 (M5c) — prefilter applies to the HYBRID path. In the fast-worker fixture, after
// the message is embedded (rrf), a query with a NON-matching --workspace filter
// returns 0 results: the prefilter restricts the candidate set BEFORE fusion on both
// the BM25 and the vector legs.
#[test]
fn hybrid_prefilter_excludes_nonmatching_workspace() {
    let _guard = test_lock();
    let fixture = spawn_fast_worker_fixture();

    let pane = create_root_pane(&fixture.socket_path, "query-hybrid-prefilter");
    report_agent(&fixture.socket_path, &pane, "codex");

    let body = "hybridprefilter embedding fusion sentinel";
    let _mid = agent_send(&fixture, "codex", "review", body);

    // Wait for the hybrid (rrf) state, then assert the unfiltered query found it.
    let (v, reached, _took) = poll_query_until(
        &fixture,
        &[body],
        Duration::from_secs(20),
        Duration::from_millis(250),
        |json| json["ranking"] == "rrf",
    );
    assert!(reached, "query never reached rrf within 20s: {v}");
    assert!(
        v["count"].as_u64().unwrap_or(0) >= 1,
        "unfiltered hybrid query must find the message: {v}"
    );

    // A non-matching --workspace prefilter empties the result set even on the hybrid
    // path (the prefilter applies before fusion on both legs).
    let filtered = query_json(&fixture, &["--workspace", "ws-that-does-not-exist", body]);
    assert_eq!(filtered["result"], "ok", "{filtered}");
    assert_eq!(
        filtered["count"], 0,
        "a non-matching --workspace prefilter must return 0 results on the hybrid path: {filtered}"
    );
    assert_eq!(
        filtered["results"],
        serde_json::json!([]),
        "prefiltered-out hybrid query returns the empty array: {filtered}"
    );

    fixture.cleanup();
}
