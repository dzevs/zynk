//! zynk fork (ADR 0008) M6 integration tests: native DB path + wrapper-cutover
//! SAFETY guard, exercised through the REAL `zynk` binary (the crate/package stays
//! `zynk` internally; the produced binary + user-facing text are `zynk`).
//!
//! These spawn an ISOLATED dev server under `/tmp` with an isolated
//! `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`/`ZYNK_SOCKET_PATH`/`ZYNK_SQLITE_HOME`;
//! they NEVER touch the live zynk runtime or the operator's real `~/.zynk`.
//!
//! The safety-critical claim proved here: when a FOREIGN (non-native) database
//! sits at the resolved native path, the product open path FAILS CLOSED and
//! leaves the foreign bytes BYTE-IDENTICAL — zynk never auto-migrates/overwrites
//! foreign data (ADR 0008). The exhaustive classifier / path-precedence /
//! adopt-backup / fresh-init / native-open behavior is covered by the fast
//! in-crate unit tests in `src/zynk/db.rs`, `src/zynk/db_path.rs`, and
//! `src/zynk/db_cutover.rs` (which can call `crate::zynk::*` directly; this
//! integration crate can only drive the binary).

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_zynk_pid,
    unregister_spawned_zynk_pid, wait_for_socket,
};

// The foreign DB is planted with the system `sqlite3` CLI; if it is unavailable
// the foreign-guard test self-skips (the in-crate unit tests in
// `src/zynk/db.rs` cover the guard with `sqlx` regardless).

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
        "/tmp/zynk-db-cutover-test-{}-{nanos}",
        std::process::id()
    ))
}

fn app_dir() -> &'static str {
    if cfg!(debug_assertions) {
        "zynk-dev"
    } else {
        "zynk"
    }
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

fn spawn_server(
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
    SpawnedZynk {
        _master: pair.master,
        child,
    }
}

/// Plant a real, non-native SQLite database file at `path` using the system
/// `sqlite3` CLI. Returns false (so the test self-skips) if `sqlite3` is absent.
fn plant_foreign_sqlite(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let status = std::process::Command::new("sqlite3")
        .arg(path)
        .arg("CREATE TABLE projects (id TEXT PRIMARY KEY); CREATE TABLE agents (id TEXT PRIMARY KEY); INSERT INTO projects VALUES ('p1');")
        .status();
    matches!(status, Ok(s) if s.success() && path.exists())
}

/// The product DB path the server resolves from `ZYNK_SQLITE_HOME` (exact dir +
/// `zynk.db`) — must match `db_path::resolve_db_path_with` for the env case.
fn resolved_db_path(sqlite_home: &Path) -> PathBuf {
    sqlite_home.join("zynk.db")
}

fn write_min_config(config_home: &Path) {
    let dir = config_home.join(app_dir());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), "onboarding = false\n").unwrap();
}

#[test]
fn foreign_db_at_native_path_makes_server_fail_closed_and_preserves_bytes() {
    let _guard = test_lock();
    let base = unique_base();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = base.join("api.sock");
    let sqlite_home = base.join("sqlite");
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::create_dir_all(&sqlite_home).unwrap();
    register_runtime_dir(&runtime_dir);
    write_min_config(&config_home);

    let db = resolved_db_path(&sqlite_home);
    if !plant_foreign_sqlite(&db) {
        eprintln!("skipping: system sqlite3 unavailable to plant a foreign DB");
        cleanup_test_base(&base);
        return;
    }
    let before = fs::read(&db).unwrap();

    // Spawn the server. Its startup calls the product open path
    // (db::open_migrated) against the foreign DB. ADR 0008 (fail-closed): the
    // server must REFUSE to come up on foreign data — exit non-zero with the
    // branded error — and never auto-migrate/overwrite. We read the PTY output
    // for the branded message, confirm a non-zero exit, and assert the foreign
    // bytes are byte-identical afterward.
    let mut server = spawn_server(&config_home, &runtime_dir, &socket_path, &sqlite_home);

    // Drain the PTY master in a background thread (the server writes the branded
    // error to stderr, which the PTY routes to the master).
    let mut reader = server._master.try_clone_reader().unwrap();
    let output = std::sync::Arc::new(Mutex::new(String::new()));
    let output_writer = std::sync::Arc::clone(&output);
    let drain = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut s) = output_writer.lock() {
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
                Err(_) => break,
            }
        }
    });

    // The server should exit on its own (process::exit(1)); wait for it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exit_status = None;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = server.child.try_wait() {
            exit_status = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = drain.join();
    let captured = output.lock().map(|s| s.clone()).unwrap_or_default();

    let status =
        exit_status.expect("server must exit on a foreign DB (fail-closed), not keep running");
    assert!(
        !status.success(),
        "SAFETY: server must exit NON-ZERO on a foreign DB at the native path (got success); output:\n{captured}"
    );
    assert!(
        !socket_path.exists(),
        "SAFETY: server must NOT bring up its socket on a foreign DB; output:\n{captured}"
    );
    // The branded fail-closed message names zynk, refuses, and points at the path.
    assert!(
        captured.contains("zynk: refusing to open a non-native database"),
        "expected branded fail-closed error in server output, got:\n{captured}"
    );

    let after = fs::read(&db).unwrap();
    assert_eq!(
        before, after,
        "SAFETY: foreign DB at native path must be byte-identical after server startup (no auto-migrate/overwrite)"
    );

    drop(server);
    cleanup_test_base(&base);
}

#[test]
fn fresh_sqlite_home_initializes_native_and_server_comes_up() {
    let _guard = test_lock();
    let base = unique_base();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = base.join("api.sock");
    let sqlite_home = base.join("sqlite");
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::create_dir_all(&sqlite_home).unwrap();
    register_runtime_dir(&runtime_dir);
    write_min_config(&config_home);

    let db = resolved_db_path(&sqlite_home);
    assert!(!db.exists(), "fresh sqlite_home must start empty");

    let server = spawn_server(&config_home, &runtime_dir, &socket_path, &sqlite_home);
    wait_for_socket(&socket_path, Duration::from_secs(10));
    // The startup open path created + migrated a native DB at the resolved path.
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        db.exists(),
        "server startup must initialize a native DB at {}",
        db.display()
    );
    drop(server);

    cleanup_test_base(&base);
}

#[test]
fn db_status_cli_reports_foreign_classification() {
    // `zynk db status` (ADR 0008 cutover surface) is wired in `src/cli.rs` and
    // handled in-process (no socket round-trip). Drive the real binary against a
    // planted FOREIGN DB at the resolved native path and assert it prints the
    // zynk-branded classification + the resolved path.
    let _guard = test_lock();
    let base = unique_base();
    let sqlite_home = base.join("sqlite");
    fs::create_dir_all(&sqlite_home).unwrap();

    let db = resolved_db_path(&sqlite_home);
    if !plant_foreign_sqlite(&db) {
        eprintln!("skipping: system sqlite3 unavailable to plant a foreign DB");
        cleanup_test_base(&base);
        return;
    }
    let before = fs::read(&db).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_zynk"))
        .arg("db")
        .arg("status")
        .env("ZYNK_SQLITE_HOME", &sqlite_home)
        .env_remove("ZYNK_HOME")
        .output()
        .expect("failed to run `zynk db status`");

    assert!(
        output.status.success(),
        "`zynk db status` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("zynk db path: {}", db.display())),
        "expected zynk-branded path line, got:\n{stdout}"
    );
    assert!(
        stdout.contains("FOREIGN"),
        "expected FOREIGN classification, got:\n{stdout}"
    );
    assert!(
        stdout.contains("zynk db adopt"),
        "expected the cutover-action hint, got:\n{stdout}"
    );

    // `status` must never mutate the foreign bytes.
    let after = fs::read(&db).unwrap();
    assert_eq!(before, after, "`zynk db status` must not mutate the DB");

    cleanup_test_base(&base);
}

/// #117 regression: the test isolation CANNOT be defeated by an inherited LIVE-looking
/// env. We launch the real binary with a live-looking `ZYNK_HOME` (a fake home whose
/// `~/.zynk` exists) AND a live-looking `ZYNK_SQLITE_HOME` set FIRST, then apply the
/// harness pattern (override `ZYNK_SQLITE_HOME` to an isolated temp + `env_remove`
/// `ZYNK_HOME`). `zynk db status` prints the RESOLVED native path WITHOUT mutating it.
/// The resolved path MUST be the isolated temp — never the fake `~/.zynk` — proving the
/// scrub/override wins over an inherited live value.
#[test]
fn inherited_live_env_cannot_defeat_test_db_isolation() {
    let _guard = test_lock();
    let base = unique_base();
    let isolated_sqlite = base.join("isolated-sqlite");
    let fake_home = base.join("fake-home");
    fs::create_dir_all(&isolated_sqlite).unwrap();
    fs::create_dir_all(fake_home.join(".zynk")).unwrap();

    let isolated_db = resolved_db_path(&isolated_sqlite);
    let live_looking_db = fake_home.join(".zynk").join("zynk.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_zynk"))
        .arg("db")
        .arg("status")
        // Inherit a LIVE-looking environment first (as if the operator's shell had it).
        .env("ZYNK_HOME", &fake_home)
        .env("ZYNK_SQLITE_HOME", fake_home.join(".zynk"))
        // Now the harness isolation pattern: override the sqlite home + scrub ZYNK_HOME.
        .env("ZYNK_SQLITE_HOME", &isolated_sqlite)
        .env_remove("ZYNK_HOME")
        .output()
        .expect("failed to run `zynk db status`");

    assert!(
        output.status.success(),
        "`zynk db status` should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("zynk db path: {}", isolated_db.display())),
        "resolved DB must be the ISOLATED temp, got:\n{stdout}"
    );
    assert!(
        !stdout.contains(&live_looking_db.display().to_string()),
        "resolved DB must NEVER be the live-looking ~/.zynk, got:\n{stdout}"
    );
    // The live-looking ~/.zynk must remain pristine (no DB file created there).
    assert!(
        !live_looking_db.exists(),
        "isolation must not create a DB under the inherited live ~/.zynk"
    );

    cleanup_test_base(&base);
}
