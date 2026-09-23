// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_zynk_pid, unregister_spawned_zynk_pid,
};

#[test]
fn m828e_retired_cli_flags_refuse_before_socket_connection() {
    for args in [
        vec![
            "pane",
            "report-agent",
            "w1:p1",
            "--source",
            "zynk:claude",
            "--agent",
            "claude",
            "--state",
            "working",
            "--custom-status",
            "old",
        ],
        vec![
            "pane",
            "report-metadata",
            "w1:p1",
            "--source",
            "user:task",
            "--custom-status",
            "old",
        ],
        vec![
            "pane",
            "report-metadata",
            "w1:p1",
            "--source",
            "user:task",
            "--clear-custom-status",
        ],
    ] {
        let (request, run) = mock_snapshot_cli(
            &args,
            serde_json::json!({"id":"cli:request", "result":{"type":"ok"}}),
        );
        let error = String::from_utf8_lossy(&run.stderr);
        assert!(request.is_none(), "retired flag connected: {args:?}");
        assert_eq!(run.status.code(), Some(2), "{args:?}: {error}");
        assert!(error.contains("unknown option"), "{args:?}: {error}");
        assert!(error.contains("custom-status"), "{args:?}: {error}");
        assert!(run.stdout.is_empty());
    }
}

#[test]
fn m828e_cli_help_omits_retired_flags_and_keeps_token_replacement() {
    for command in ["report-agent", "report-metadata"] {
        let (request, run) = mock_snapshot_cli(
            &["pane", command, "--help"],
            serde_json::json!({"id":"unused", "result":{"type":"ok"}}),
        );
        let output = String::from_utf8_lossy(&run.stderr);
        assert!(request.is_none(), "help connected: {command}");
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let prefix = format!("zynk pane {command} <pane_id> ");
        let lines: Vec<_> = output
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with(&prefix))
            .collect();
        assert_eq!(lines.len(), 1, "{command}: {output}");
        let flags: Vec<_> = lines[0]
            .split([' ', '[', ']', '|'])
            .filter(|part| part.starts_with("--"))
            .collect();
        assert!(flags.contains(&"--source"));
        assert!(!flags.contains(&"--custom-status"), "{command}: {output}");
        assert!(
            !flags.contains(&"--clear-custom-status"),
            "{command}: {output}"
        );
        if command == "report-metadata" {
            assert!(flags.contains(&"--token") && flags.contains(&"--clear-token"));
            assert!(flags.contains(&"--title") && flags.contains(&"--state-label"));
        } else {
            assert!(flags.contains(&"--state") && flags.contains(&"--agent-session-id"));
        }
    }
}

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    support::test_root().join(format!("hcli-{}-{nanos}", std::process::id()))
}

fn run_git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    scrub_git_env(&mut command);
    let status = command.arg("-C").arg(repo).args(args).status().unwrap();
    assert!(
        status.success(),
        "git command failed: git -C {} {}",
        repo.display(),
        args.join(" ")
    );
}

/// Strip every inherited git environment variable that could point a fixture's git command at
/// ANOTHER repository, and pin the config files it may read.
///
/// The arbiter's reproduction: with `GIT_DIR` inherited, `git init` inside a child directory exits
/// 0 while leaving `child/.git` ABSENT — git initialises the directory `GIT_DIR` names — and the
/// `git -C child config …` that follows exits 0 too, writing into the OUTER repository, because
/// `git config` walks up to the nearest parent repository. Every status code is success, so nothing
/// in the fixture notices, and a real checkout is left authoring commits as
/// `Zynk Test <zynk@example.invalid>`. A sanitised caller environment hides this, so a fixture
/// cannot rely on having one.
fn scrub_git_env(command: &mut Command) -> &mut Command {
    let names: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| name.as_encoded_bytes().starts_with(b"GIT_"))
        .collect();
    for name in names {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
}

/// Seed the fixture repository's commit identity, with the write unable to leave the fixture.
///
/// Containment is asserted BEFORE the write — a `git init` that did not take aborts the test rather
/// than escaping — and the write then names the config file explicitly, which cannot walk up at all.
fn set_repo_identity(repo: &Path) {
    let dot_git = repo.join(".git");
    assert!(
        dot_git.exists(),
        "fixture repository was not initialised, so a config write would escape into a parent repository: {}",
        repo.display()
    );
    let config_path = if dot_git.is_dir() {
        dot_git.join("config")
    } else {
        // A linked worktree or a submodule: `.git` is a FILE naming the real gitdir.
        let mut command = Command::new("git");
        scrub_git_env(&mut command);
        let output = command
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "could not resolve the git dir of {}",
            repo.display()
        );
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("config")
    };
    let config_path = config_path.to_string_lossy().into_owned();
    for (key, value) in [
        ("user.email", "zynk@example.invalid"),
        ("user.name", "Zynk Test"),
    ] {
        run_git(repo, &["config", "--file", &config_path, key, value]);
    }
}

fn create_committed_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    run_git(path, &["init", "--quiet"]);
    set_repo_identity(path);
    fs::write(path.join("README.md"), "test\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "--quiet", "-m", "initial"]);
}

struct SpawnedZynk {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

struct SpawnedServerProcess {
    child: std::process::Child,
    /// Captured stdout+stderr of the server (was `Stdio::null()`): a server that dies during
    /// startup used to vanish silently and the socket wait would time out with no cause.
    log_path: PathBuf,
}

impl Drop for SpawnedServerProcess {
    fn drop(&mut self) {
        let pid = self.child.id();
        let _ = self.child.kill();
        let _ = self.child.wait();
        unregister_spawned_zynk_pid(Some(pid));
    }
}

impl Drop for SpawnedZynk {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_zynk_pid(Some(pid));
        }
    }
}

fn cleanup_spawned_zynk(spawned: SpawnedZynk, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    // M6 flake-hardening: under the full-parallel `just check` run (~32-wide nextest) a freshly
    // spawned named-session server can take longer than the caller's few-second budget to bind its
    // socket on a saturated host (this test spawns two servers at once). Floor the wait generously so
    // default `just check` is deterministic — the happy path still returns in <1s when the socket
    // appears; the larger ceiling only delays a genuine failure.
    let timeout = timeout.max(Duration::from_secs(30));
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn spawn_zynk(config_home: &Path, runtime_dir: &Path, socket_path: &Path) -> SpawnedZynk {
    spawn_zynk_with_config(
        config_home,
        runtime_dir,
        socket_path,
        None,
        "onboarding = false\n",
    )
}

fn spawn_zynk_with_pane_history(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
) -> SpawnedZynk {
    spawn_zynk_with_config(
        config_home,
        runtime_dir,
        socket_path,
        None,
        "onboarding = false\n[experimental]\npane_history = true\n",
    )
}

fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "zynk-dev"
    } else {
        "zynk"
    }
}

fn named_session_socket(config_home: &Path, session: &str) -> PathBuf {
    config_home
        .join(app_dir_name())
        .join("sessions")
        .join(session)
        .join("zynk.sock")
}

fn spawn_named_server(
    config_home: &Path,
    runtime_dir: &Path,
    session: &str,
) -> SpawnedServerProcess {
    spawn_named_server_with(
        config_home,
        runtime_dir,
        session,
        &config_home.join("sqlite"),
        false,
    )
}

/// `daemon_stdio` mirrors the real launcher (`build_server_daemon_command`): stdin/stdout/stderr are
/// all `/dev/null`, so the ONLY durable record is the server's own log file.
fn spawn_named_server_with(
    config_home: &Path,
    runtime_dir: &Path,
    session: &str,
    sqlite_home: &Path,
    daemon_stdio: bool,
) -> SpawnedServerProcess {
    spawn_named_server_with_env(
        config_home,
        runtime_dir,
        session,
        sqlite_home,
        daemon_stdio,
        &[],
    )
}

fn spawn_named_server_with_env(
    config_home: &Path,
    runtime_dir: &Path,
    session: &str,
    sqlite_home: &Path,
    daemon_stdio: bool,
    extra_env: &[(&str, &str)],
) -> SpawnedServerProcess {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command
        .args(["--session", session, "server"])
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("ZYNK_SQLITE_HOME", sqlite_home)
        .env_remove("ZYNK_HOME")
        .env_remove("ZYNK_SOCKET_PATH")
        .env_remove("ZYNK_CLIENT_SOCKET_PATH")
        .env_remove("ZYNK_ENV")
        // ADR 0014 debug seam: identity reports and receipts are accepted only from the
        // TARGET pane's process tree, and this harness process is outside every pane. The
        // seam makes this server treat each accepted connection as the pane's own child.
        // It is compiled only under `#[cfg(debug_assertions)]`, so it cannot exist in a
        // release binary, and the tests that must exercise the REAL binding spawn a
        // server without it.
        .env("ZYNK_TEST_TRUST_PEER_PID", "pane-child")
        .stdin(std::process::Stdio::null());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let log_path = if daemon_stdio {
        // Nothing is captured: assert on the server's durable log at
        // <config_home>/<app>/sessions/<session>/zynk-server.log instead.
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        config_home
            .join(app_dir_name())
            .join("sessions")
            .join(session)
            .join("zynk-server.log")
    } else {
        let log_path = config_home.join(format!("{session}.server.log"));
        let log = fs::File::create(&log_path).unwrap();
        command.stdout(log.try_clone().unwrap()).stderr(log);
        log_path
    };

    let child = command.spawn().unwrap();
    register_spawned_zynk_pid(Some(child.id()));
    SpawnedServerProcess { child, log_path }
}

/// Like `wait_for_socket`, but when the socket never appears it reports WHY: whether the server
/// process already exited (and with what status) plus the tail of its captured output.
fn wait_for_named_server_socket(
    server: &mut SpawnedServerProcess,
    session: &str,
    path: &Path,
    timeout: Duration,
) {
    let timeout = timeout.max(Duration::from_secs(30));
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok() {
            return;
        }
        if let Ok(Some(status)) = server.child.try_wait() {
            let output = fs::read_to_string(&server.log_path).unwrap_or_default();
            panic!(
                "named session `{session}` server exited during startup with {status} before \
                 binding {}; captured output:\n{output}",
                path.display()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
    let status = match server.child.try_wait() {
        Ok(Some(status)) => format!("exited with {status}"),
        Ok(None) => "still running".to_string(),
        Err(err) => format!("status unknown: {err}"),
    };
    let output = fs::read_to_string(&server.log_path).unwrap_or_default();
    panic!(
        "socket did not appear at {} within {timeout:?}; named session `{session}` server {status}; \
         captured output:\n{output}",
        path.display()
    );
}

fn run_named_cli(config_home: &Path, runtime_dir: &Path, args: &[&str]) -> std::process::Output {
    run_named_cli_with_socket_override(config_home, runtime_dir, args, None)
}

struct ConfigCheckFixture {
    base: PathBuf,
}

impl ConfigCheckFixture {
    fn new() -> Self {
        let base = unique_test_dir();
        fs::create_dir(&base).unwrap();
        Self { base }
    }

    fn config_path(&self) -> PathBuf {
        self.base
            .join("config")
            .join(app_dir_name())
            .join("config.toml")
    }

    fn write_config(&self, content: &str) {
        let path = self.config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn snapshot(&self) -> Vec<(PathBuf, &'static str, Vec<u8>)> {
        fn visit(base: &Path, dir: &Path, entries: &mut Vec<(PathBuf, &'static str, Vec<u8>)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path.strip_prefix(base).unwrap().to_path_buf();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    entries.push((relative, "directory", Vec::new()));
                    visit(base, &path, entries);
                } else if kind.is_symlink() {
                    entries.push((
                        relative,
                        "symlink",
                        fs::read_link(path)
                            .unwrap()
                            .as_os_str()
                            .as_encoded_bytes()
                            .to_vec(),
                    ));
                } else {
                    entries.push((relative, "file", fs::read(path).unwrap()));
                }
            }
        }
        let mut entries = Vec::new();
        visit(&self.base, &self.base, &mut entries);
        entries.sort();
        entries
    }

    fn run(&self, args: &[&str], override_path: Option<&Path>) -> std::process::Output {
        let before = self.snapshot();
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
            .env("ZYNK_SOCKET_PATH", self.base.join("runtime/s.sock"))
            .env("ZYNK_CLIENT_SOCKET_PATH", self.base.join("runtime/c.sock"))
            .current_dir(&self.base);
        if let Some(path) = override_path {
            command.env("ZYNK_CONFIG_PATH", path);
        }
        let output = command.output().unwrap();
        assert_eq!(
            self.snapshot(),
            before,
            "config inspection changed private files: {args:?}"
        );
        for name in ["home", "data", "cache", "state", "runtime", "db", "sqlite"] {
            assert!(
                !self.base.join(name).exists(),
                "config inspection created {name}"
            );
        }
        output
    }
}

impl Drop for ConfigCheckFixture {
    fn drop(&mut self) {
        cleanup_test_base(&self.base);
    }
}

fn config_check_outcome(output: std::process::Output) -> (Option<i32>, String, String) {
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn config_check_reports_invalid_config_without_server() {
    let fixture = ConfigCheckFixture::new();
    let outcomes: Vec<_> = [
        "[keys\nnew_workspace = \"g\"\n",
        "[ui]\nsidebar_min_width = 50\nsidebar_max_width = 30\n",
    ]
    .into_iter()
    .map(|content| {
        fixture.write_config(content);
        config_check_outcome(fixture.run(&["config", "check"], None))
    })
    .collect();
    assert_eq!(outcomes, vec![
        (Some(1), "config: issues found\nconfig parse error: TOML parse error at line 1, column 6\n  |\n1 | [keys\n  |      ^\ninvalid table header\nexpected `.`, `]`\n; using defaults\n".into(), String::new()),
        (Some(1), "config: issues found\nui.sidebar_min_width (50) is greater than sidebar_max_width (30)\n".into(), String::new()),
    ]);
}

#[test]
fn config_check_reports_ok_when_config_is_missing() {
    let fixture = ConfigCheckFixture::new();
    assert_eq!(
        config_check_outcome(fixture.run(&["config", "check"], None)),
        (Some(0), "config: ok\n".into(), String::new())
    );
    assert!(fixture.snapshot().is_empty());
}

#[test]
fn config_check_rejects_json_output() {
    let fixture = ConfigCheckFixture::new();
    assert_eq!(
        config_check_outcome(fixture.run(&["config", "check", "--json"], None)),
        (Some(2), String::new(), "usage: zynk config check\n".into())
    );
}

#[test]
fn m826_config_check_preserves_all_diagnostics_read_only() {
    let fixture = ConfigCheckFixture::new();
    fixture.write_config("onboarding = false\n[ui]\nmouse_capture = false\n");
    let valid = config_check_outcome(fixture.run(&["config", "check"], None));
    fixture.write_config("[keys]\nnew_tabb = \"prefix+t\"\n[ui]\nagent_panel_scope = \"workspace\"\nalpha = 1\nbravo = 2\ncharlie = 3\ndelta = 4\nmouse_captur = false\n[zynk]\nsqlite_hmoe = \"unused\"\n");
    let issues = config_check_outcome(fixture.run(&["config", "check"], None));
    assert_eq!(valid, (Some(0), "config: ok\n".into(), String::new()));
    assert_eq!(issues, (Some(1), concat!(
        "config: issues found\n",
        "ui.agent_panel_scope is no longer supported (removed in 3.1.0); the agent panel shows all workspaces. ui.agent_panel_sort controls ordering only and does not restore current-workspace filtering; ignoring key\n",
        "unknown config key keys.new_tabb; ignoring key\n",
        "unknown config key ui.alpha; ignoring key\n",
        "unknown config key ui.bravo; ignoring key\n",
        "unknown config key ui.charlie; ignoring key\n",
        "unknown config key ui.delta; ignoring key\n",
        "unknown config key ui.mouse_captur; ignoring key\n",
        "unknown config key zynk.sqlite_hmoe; ignoring key\n",
    ).into(), String::new()));
}

#[test]
fn m826_config_check_help_and_argument_errors() {
    let fixture = ConfigCheckFixture::new();
    fixture.write_config("[keys\n");
    let group_help = concat!(
        "zynk config commands:\n",
        "  zynk config check  validate config.toml and print diagnostics\n",
        "  zynk config reset-keys  back up config.toml and remove custom keybindings\n",
    );
    let cases = [
        (
            vec!["config", "check", "help"],
            0,
            "usage: zynk config check\n",
        ),
        (vec!["config", "check", "--help"], 0, group_help),
        (vec!["config", "check", "-h"], 0, group_help),
        (vec!["config", "unknown", "--help"], 0, group_help),
        (
            vec!["config", "check", "extra"],
            2,
            "usage: zynk config check\n",
        ),
        (
            vec!["config", "check", "help", "extra"],
            2,
            "usage: zynk config check\n",
        ),
    ];
    let observed: Vec<_> = cases
        .iter()
        .map(|(args, _, _)| config_check_outcome(fixture.run(args, None)))
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, code, stderr)| (Some(*code), String::new(), (*stderr).into()))
        .collect();
    assert_eq!(observed, expected);
}

#[test]
fn m826_config_check_honors_override_and_read_errors() {
    let fixture = ConfigCheckFixture::new();
    fixture.write_config("[keys\n");
    let custom = fixture.base.join("custom.toml");
    fs::write(&custom, "onboarding = false\n").unwrap();
    let directory = fixture.base.join("directory.toml");
    fs::create_dir(&directory).unwrap();
    let link = fixture.base.join("loop.toml");
    std::os::unix::fs::symlink("loop.toml", &link).unwrap();
    let observed: Vec<_> = [&custom, &directory, &link]
        .into_iter()
        .map(|path| config_check_outcome(fixture.run(&["config", "check"], Some(path))))
        .collect();
    assert_eq!(observed, vec![
        (Some(0), "config: ok\n".into(), String::new()),
        (Some(1), "config: issues found\nconfig read error: Is a directory (os error 21); using defaults\n".into(), String::new()),
        (Some(1), "config: issues found\nconfig read error: Too many levels of symbolic links (os error 40); using defaults\n".into(), String::new()),
    ]);
}

fn run_named_cli_with_socket_override(
    config_home: &Path,
    runtime_dir: &Path,
    args: &[&str],
    socket_override: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("ZYNK_SQLITE_HOME", config_home.join("sqlite"))
        .env_remove("ZYNK_HOME")
        .env_remove("ZYNK_CLIENT_SOCKET_PATH")
        .env_remove("ZYNK_ENV");
    if let Some(socket_override) = socket_override {
        command.env("ZYNK_SOCKET_PATH", socket_override);
    } else {
        command.env_remove("ZYNK_SOCKET_PATH");
    }
    command.output().unwrap()
}

fn run_named_cli_json(config_home: &Path, runtime_dir: &Path, args: &[&str]) -> serde_json::Value {
    let output = run_named_cli(config_home, runtime_dir, args);
    assert!(
        output.status.success(),
        "command failed: zynk {}\nstatus: {:?}\nstderr: {}\nstdout: {}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn spawn_zynk_with_path(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
    path_override: Option<&Path>,
) -> SpawnedZynk {
    spawn_zynk_with_config(
        config_home,
        runtime_dir,
        socket_path,
        path_override,
        "onboarding = false\n",
    )
}

fn spawn_zynk_with_config(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
    path_override: Option<&Path>,
    config_toml: &str,
) -> SpawnedZynk {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        config_toml,
    )
    .unwrap();

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
    // ADR 0014 debug seam: identity reports and receipts are accepted only from the
    // TARGET pane's process tree, and this harness process is outside every pane. The
    // seam makes this server treat each accepted connection as the pane's own child.
    // It is compiled only under `#[cfg(debug_assertions)]`, so it cannot exist in a
    // release binary, and the tests that must exercise the REAL binding spawn a
    // server without it.
    cmd.env("ZYNK_TEST_TRUST_PEER_PID", "pane-child");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("ZYNK_SOCKET_PATH", socket_path);
    cmd.env("ZYNK_SQLITE_HOME", config_home.join("sqlite"));
    cmd.env_remove("ZYNK_HOME");
    cmd.env_remove("ZYNK_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("ZYNK_ENV");
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_zynk_pid(child.process_id());
    SpawnedZynk {
        _master: pair.master,
        child,
    }
}

fn run_cli(socket_path: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command.args(args);
    command.env("ZYNK_SOCKET_PATH", socket_path);
    if let Some(parent) = socket_path.parent() {
        command.env("ZYNK_SQLITE_HOME", parent.join("sqlite"));
    }
    command.env_remove("ZYNK_HOME");
    command.output().unwrap()
}

fn run_cli_in_dir(socket_path: &Path, args: &[&str], current_dir: &Path) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command.args(args);
    command.current_dir(current_dir);
    command.env("ZYNK_SOCKET_PATH", socket_path);
    if let Some(parent) = socket_path.parent() {
        command.env("ZYNK_SQLITE_HOME", parent.join("sqlite"));
    }
    command.env_remove("ZYNK_HOME");
    command.output().unwrap()
}

fn run_cli_json(socket_path: &Path, args: &[&str]) -> serde_json::Value {
    let output = run_cli(socket_path, args);
    parse_cli_json_output(args, output)
}

fn run_cli_json_in_dir(socket_path: &Path, args: &[&str], current_dir: &Path) -> serde_json::Value {
    let output = run_cli_in_dir(socket_path, args, current_dir);
    parse_cli_json_output(args, output)
}

fn parse_cli_json_output(args: &[&str], output: std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "command failed: zynk {}\nstatus: {:?}\nstderr: {}\nstdout: {}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "failed to parse JSON response for `zynk {}`: {}\nstdout: {}\nstderr: {}",
            args.join(" "),
            err,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn wait_until(timeout: Duration, interval: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(interval);
    }
    false
}

fn pane_read_recent_contains(socket_path: &Path, pane_id: &str, expected: &str) -> bool {
    let output = run_cli(
        socket_path,
        &["pane", "read", pane_id, "--source", "recent"],
    );
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).contains(expected)
}

fn process_exists(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    !process_exists(pid)
}

fn wait_for_pid_file(pid_file: &Path, timeout: Duration) -> Result<u32, String> {
    const STABLE_PID_CONTENT_WINDOW: Duration = Duration::from_millis(250);

    let deadline = Instant::now() + timeout;
    let mut last_contents = String::new();
    let mut stable_candidate: Option<(String, u32, Instant)> = None;

    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(pid_file) {
            let trimmed = contents.trim().to_string();
            last_contents = contents;

            if let Ok(pid) = trimmed.parse::<u32>() {
                match &stable_candidate {
                    Some((candidate_text, candidate_pid, stable_since))
                        if candidate_text == &trimmed && *candidate_pid == pid =>
                    {
                        if stable_since.elapsed() >= STABLE_PID_CONTENT_WINDOW {
                            return Ok(pid);
                        }
                    }
                    _ => {
                        stable_candidate = Some((trimmed, pid, Instant::now()));
                    }
                }
            } else {
                stable_candidate = None;
            }
        }

        thread::sleep(Duration::from_millis(25));
    }

    Err(format!(
        "pid file {} did not contain stable parseable pid before timeout; last contents={:?}",
        pid_file.display(),
        last_contents
    ))
}

#[test]
fn wait_for_pid_file_retries_until_pid_is_written() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let pid_file = base.join("delayed.pid");
    fs::write(&pid_file, "").unwrap();

    let writer = thread::spawn({
        let pid_file = pid_file.clone();
        move || {
            thread::sleep(Duration::from_millis(100));
            fs::write(pid_file, "424242\n").unwrap();
        }
    });

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(2)).unwrap();
    assert_eq!(pid, 424242);

    writer.join().unwrap();
    cleanup_test_base(&base);
}

#[test]
fn wait_for_pid_file_errors_when_file_never_contains_pid() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let pid_file = base.join("empty.pid");
    fs::write(&pid_file, "").unwrap();

    let err = wait_for_pid_file(&pid_file, Duration::from_millis(150)).unwrap_err();
    assert!(
        err.contains("did not contain stable parseable pid"),
        "unexpected error: {err}"
    );

    cleanup_test_base(&base);
}

#[test]
fn wait_for_pid_file_rejects_unparseable_partial_write_until_stable_contents() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let pid_file = base.join("partial-race.pid");
    fs::write(&pid_file, "").unwrap();

    let writer = thread::spawn({
        let pid_file = pid_file.clone();
        move || {
            thread::sleep(Duration::from_millis(40));
            fs::write(&pid_file, "pid=").unwrap();
            thread::sleep(Duration::from_millis(40));
            fs::write(&pid_file, "pid=424242").unwrap();
            thread::sleep(Duration::from_millis(40));
            fs::write(&pid_file, "424242\n").unwrap();
        }
    });

    let start = Instant::now();
    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(2)).unwrap();
    assert_eq!(pid, 424242);
    assert!(
        start.elapsed() >= Duration::from_millis(300),
        "helper should wait for stable complete contents, elapsed={:?}",
        start.elapsed()
    );

    writer.join().unwrap();
    cleanup_test_base(&base);
}

fn send_request(socket_path: &Path, json: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.write_all(json.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();

    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn run_claude_hook(action: &str, hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/claude/zynk-agent-state.sh",
        &[action],
        hook_input,
    )
}

fn run_codex_hook(action: &str, hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/codex/zynk-agent-state.sh",
        &[action],
        hook_input,
    )
}

fn run_copilot_hook(hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/copilot/zynk-agent-state.sh",
        &[],
        hook_input,
    )
}

fn run_shell_hook(asset_path: &str, args: &[&str], hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook_with_env(asset_path, args, hook_input, &[])
}

fn run_shell_hook_with_env(
    asset_path: &str,
    args: &[&str],
    hook_input: &str,
    extra_env: &[(&str, &str)],
) -> Option<serde_json::Value> {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("zynk.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut line = String::new();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    reader.read_line(&mut line).unwrap();
                    let _ = stream.write_all(br#"{"id":"test","result":{"type":"ok"}}"#);
                    let _ = stream.write_all(b"\n");
                    let _ = stream.flush();
                    return Some(line);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        }
        None
    });

    let hook_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(asset_path);
    let mut child = Command::new("bash")
        .arg(hook_path)
        .args(args)
        .env("ZYNK_ENV", "1")
        .env("ZYNK_SOCKET_PATH", &socket_path)
        .env("ZYNK_PANE_ID", "p_test")
        .env_remove("CODEX_THREAD_ID")
        .envs(extra_env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(hook_input.as_bytes()).unwrap();
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "hook failed: status={:?} stderr={} stdout={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let request = server.join().unwrap();
    cleanup_test_base(&base);
    request.map(|line| serde_json::from_str(&line).unwrap())
}

#[test]
fn claude_hook_ignores_state_actions() {
    let subagent_input = r#"{"hook_event_name":"Notification","agent_id":"agent-abc123","agent_type":"Explore","notification_type":"permission_prompt"}"#;

    assert!(run_claude_hook("working", subagent_input).is_none());
    assert!(run_claude_hook("blocked", subagent_input).is_none());
}

#[test]
fn claude_hook_ignores_subagent_completion_reports() {
    let subagent_input =
        r#"{"hook_event_name":"SubagentStop","agent_id":"agent-abc123","agent_type":"Explore"}"#;

    assert!(run_claude_hook("working", subagent_input).is_none());
    assert!(run_claude_hook("idle", subagent_input).is_none());
    assert!(run_claude_hook("release", subagent_input).is_none());
}

#[test]
fn claude_hook_keeps_parent_agent_type_only_blocked() {
    let request = run_claude_hook(
        "blocked",
        r#"{"hook_event_name":"PermissionRequest","agent_type":"Explore"}"#,
    );

    assert!(request.is_none());
}

#[test]
fn claude_hook_reports_session_id_from_stdin() {
    let request = run_claude_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"claude-session"}"#,
    )
    .expect("session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "claude-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn codex_hook_reports_persisted_root_session_and_ignores_ephemeral_or_nested_sessions() {
    // Identity stays hook-payload-authoritative: the asset reports only a codex
    // session that persists a transcript, and only when the inherited
    // `CODEX_THREAD_ID` (if any) names that same session. A codex sub-session
    // nested inside another codex session would otherwise steal the pane.
    let request = run_codex_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"codex-session","transcript_path":"/tmp/codex-session.jsonl"}"#,
    )
    .expect("codex hook should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "codex-session");
    assert!(request["params"].get("state").is_none());

    let matching_request = run_shell_hook_with_env(
        "src/integration/assets/codex/zynk-agent-state.sh",
        &["session"],
        r#"{"hook_event_name":"SessionStart","session_id":"codex-session","transcript_path":"/tmp/codex-session.jsonl"}"#,
        &[("CODEX_THREAD_ID", "codex-session")],
    )
    .expect("matching inherited session should still report");
    assert_eq!(
        matching_request["params"]["agent_session_id"],
        "codex-session"
    );

    assert!(run_codex_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"side-session","transcript_path":null}"#,
    )
    .is_none());

    assert!(
        run_shell_hook_with_env(
            "src/integration/assets/codex/zynk-agent-state.sh",
            &["session"],
            r#"{"hook_event_name":"SessionStart","session_id":"nested-session","transcript_path":"/tmp/nested-session.jsonl"}"#,
            &[("CODEX_THREAD_ID", "parent-session")],
        )
        .is_none()
    );
}

#[test]
fn copilot_hook_reports_session_id_from_stdin() {
    let request = run_copilot_hook(
        r#"{"hook_event_name":"SessionStart","session_id":"copilot-session","source":"resume"}"#,
    )
    .expect("copilot session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent"], "copilot");
    assert_eq!(request["params"]["agent_session_id"], "copilot-session");
    assert!(request["params"].get("state").is_none());

    let camel = run_copilot_hook(
        r#"{"sessionId":"copilot-camel-session","source":"new","initialPrompt":"run tests"}"#,
    )
    .expect("copilot camelCase session start should report session identity");

    assert_eq!(camel["method"], "pane.report_agent_session");
    assert_eq!(camel["params"]["agent_session_id"], "copilot-camel-session");
    assert!(camel["params"].get("state").is_none());
}

#[test]
fn copilot_hook_does_not_report_lifecycle_state() {
    for payload in [
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"copilot-session","prompt":"run tests"}"#,
        r#"{"hook_event_name":"PreToolUse","session_id":"copilot-session","tool_name":"ask_user"}"#,
        r#"{"hook_event_name":"notification","session_id":"copilot-session","notification_type":"permission_prompt"}"#,
        r#"{"hook_event_name":"agentStop","session_id":"copilot-session","stop_reason":"end_turn"}"#,
        r#"{"hook_event_name":"SessionEnd","session_id":"copilot-session","reason":"user_exit"}"#,
    ] {
        assert!(
            run_copilot_hook(payload).is_none(),
            "copilot session-only hook should ignore lifecycle payload {payload}"
        );
    }
}

fn m835_reply_compatible_ping(stream: &mut UnixStream, request: &serde_json::Value) {
    assert_eq!(request["method"], "ping");
    assert_eq!(request["params"], serde_json::json!({}));
    writeln!(
        stream,
        "{}",
        serde_json::json!({"id": request["id"], "result": {
            "type": "pong", "version": "fixture-compatible", "protocol": support::CURRENT_PROTOCOL
        }})
    )
    .unwrap();
    stream.flush().unwrap();
}

#[test]
fn pane_run_sends_one_send_input_request_with_enter_key() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("zynk.sock");
    fs::write(base.join("runtime.id"), "rt_cli_wrapper\n").unwrap();
    let listener = UnixListener::bind(&socket_path).unwrap();

    // zynk fork (ADR 0002 / F4): `pane run` now resolves the target pane metadata
    // (a read-only `pane.get`) to populate the F4 response's `to` party, then
    // submits exactly once via `pane.send_input` with Enter. The honest-submit
    // invariant under test is "exactly ONE `pane.send_input` with Enter" (no
    // duplicate/second submit); any companion request is only the read-only
    // resolution `pane.get`, never another input.
    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut requests = Vec::new();
        let mut accepted_connections = 0;
        let mut pings = 0;
        let deadline = Instant::now() + Duration::from_millis(500);
        // Stop early once the submit has been observed AND the resolution settled.
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    accepted_connections += 1;
                    let mut line = String::new();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    reader.read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    if request["method"] == "ping" {
                        assert_eq!(pings, requests.len(), "duplicate compatibility ping");
                        pings += 1;
                        m835_reply_compatible_ping(&mut stream, &request);
                        continue;
                    }
                    assert_eq!(pings, requests.len() + 1);
                    stream
                        .write_all(br#"{"id":"cli:request","result":{"type":"ok"}}"#)
                        .unwrap();
                    stream.write_all(b"\n").unwrap();
                    stream.flush().unwrap();
                    requests.push(line);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        }
        assert_eq!(pings, requests.len());
        assert_eq!(accepted_connections, requests.len() * 2);
        requests
    });

    let run = run_cli(&socket_path, &["pane", "run", "1-1", "echo hello"]);
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let requests = server.join().unwrap();
    let parsed: Vec<serde_json::Value> = requests
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let send_inputs: Vec<&serde_json::Value> = parsed
        .iter()
        .filter(|req| req["method"] == "pane.send_input")
        .collect();
    assert_eq!(
        send_inputs.len(),
        1,
        "pane run must submit exactly once (no duplicate submit); requests: {parsed:?}"
    );
    let submit = send_inputs[0];
    assert_eq!(submit["params"]["pane_id"], "1-1");
    assert_eq!(submit["params"]["text"], "echo hello");
    assert_eq!(submit["params"]["keys"], serde_json::json!(["Enter"]));

    // Any other request is only the read-only target resolution; never another input.
    for req in &parsed {
        let method = req["method"].as_str().unwrap_or_default();
        assert!(
            matches!(method, "pane.send_input" | "pane.get"),
            "pane run issued an unexpected request: {req:?}"
        );
    }

    cleanup_test_base(&base);
}

#[test]
fn pane_report_metadata_sends_presentation_request() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("zynk.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let mut accepted_connections = 0;
        let (mut ping_stream, _) = listener.accept().unwrap();
        accepted_connections += 1;
        let mut ping_line = String::new();
        BufReader::new(ping_stream.try_clone().unwrap())
            .read_line(&mut ping_line)
            .unwrap();
        m835_reply_compatible_ping(&mut ping_stream, &serde_json::from_str(&ping_line).unwrap());
        let (mut stream, _) = listener.accept().unwrap();
        accepted_connections += 1;
        let mut line = String::new();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        reader.read_line(&mut line).unwrap();
        stream
            .write_all(br#"{"id":"cli:request","result":{"type":"ok"}}"#)
            .unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        assert_eq!(accepted_connections, 2);
        line
    });

    let run = run_cli(
        &socket_path,
        &[
            "pane",
            "report-metadata",
            "1-1",
            "--source",
            "user:claude-title",
            "--agent",
            "claude",
            "--title",
            "Refactor auth",
            "--display-agent",
            "Claude auth",
            "--state-label",
            "working=deep in the mines",
            "--ttl-ms",
            "3600000",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let line = server.join().unwrap();
    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["method"], "pane.report_metadata");
    assert_eq!(request["params"]["pane_id"], "1-1");
    assert_eq!(request["params"]["source"], "user:claude-title");
    assert_eq!(request["params"]["agent"], "claude");
    assert!(request["params"]["applies_to_source"].is_null());
    assert_eq!(request["params"]["title"], "Refactor auth");
    assert_eq!(request["params"]["display_agent"], "Claude auth");
    assert_eq!(
        request["params"]["state_labels"]["working"],
        "deep in the mines"
    );
    assert_eq!(request["params"]["ttl_ms"], 3_600_000);

    cleanup_test_base(&base);
}

#[test]
fn pane_report_metadata_rejects_blank_source_before_socket_request() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("missing.sock");

    let run = run_cli(
        &socket_path,
        &[
            "pane",
            "report-metadata",
            "1-1",
            "--source",
            "   ",
            "--title",
            "middleware",
        ],
    );

    assert_eq!(run.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("missing required --source"),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    cleanup_test_base(&base);
}

#[test]
fn pane_report_metadata_rejects_blank_applies_to_source_before_socket_request() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("missing.sock");

    let run = run_cli(
        &socket_path,
        &[
            "pane",
            "report-metadata",
            "1-1",
            "--source",
            "user:claude-title",
            "--applies-to-source",
            "   ",
            "--title",
            "middleware",
        ],
    );

    assert_eq!(run.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("missing value for --applies-to-source"),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    cleanup_test_base(&base);
}

#[test]
fn help_commands_exit_successfully() {
    let help_cases: &[&[&str]] = &[
        &["-h"],
        &["--help"],
        &["status", "-h"],
        &["server", "-h"],
        &["workspace", "-h"],
        &["worktree", "-h"],
        &["tab", "-h"],
        &["pane", "-h"],
        &["wait", "-h"],
        &["session", "-h"],
        &["session", "attach", "-h"],
        &["integration", "-h"],
    ];

    for args in help_cases {
        let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
            .args(*args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "zynk {} failed: status={:?} stdout={} stderr={}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Feature #107 review fix A: every implemented trace-capable command must advertise
/// its trace flag in its own usage/help, and `zynk trace` must appear in the top-level
/// help. Drive the REAL binary and assert against the usage/help it actually emits
/// (combined stdout+stderr), never an assumption. The DB is isolated (`ZYNK_SQLITE_HOME`
/// + scrubbed `ZYNK_HOME`/`ZYNK_SQLITE_HOME`) so a usage path can never touch the live DB.
#[test]
fn help_usage_advertises_trace_flags() {
    fn usage_output(args: &[&str]) -> String {
        let isolated = unique_test_dir();
        fs::create_dir_all(&isolated).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
            .args(args)
            .env("ZYNK_SQLITE_HOME", isolated.join("sqlite"))
            .env_remove("ZYNK_HOME")
            .env_remove("ZYNK_PANE_ID")
            .output()
            .unwrap();
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        let _ = fs::remove_dir_all(&isolated);
        combined
    }

    // Each trace-capable command advertises its own trace flag in its usage/help.
    let cases: &[(&[&str], &str)] = &[
        (&["send", "help"], "zynk send"),
        (&["reply", "help"], "zynk reply"),
        // `pane run` / `pane send-text` emit usage when given too few args.
        (&["pane", "run"], "zynk pane run"),
        (&["pane", "send-text"], "zynk pane send-text"),
        (&["query", "-h"], "zynk query"),
    ];
    for (args, needle) in cases {
        let out = usage_output(args);
        assert!(
            out.contains(needle),
            "`zynk {}` usage should name {needle:?}: {out}",
            args.join(" ")
        );
        assert!(
            out.contains("--trace"),
            "`zynk {}` usage should advertise --trace: {out}",
            args.join(" ")
        );
    }

    // `zynk pane -h` lists `pane run` / `pane send-text` with their trace flag.
    let pane_help = usage_output(&["pane", "-h"]);
    assert!(
        pane_help.contains("zynk pane run") && pane_help.contains("--trace"),
        "pane help should list `pane run` with --trace: {pane_help}"
    );
    assert!(
        pane_help.contains("zynk pane send-text") && pane_help.contains("--trace"),
        "pane help should list `pane send-text` with --trace: {pane_help}"
    );

    // Top-level help lists `zynk trace` (and advertises --trace on send/reply/query).
    let root_help = usage_output(&["--help"]);
    assert!(
        root_help.contains("zynk trace"),
        "top-level help should list `zynk trace`: {root_help}"
    );
    assert!(
        root_help.contains("--trace"),
        "top-level help should advertise --trace: {root_help}"
    );
}

#[test]
fn root_help_hides_explicit_client_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("zynk client"),
        "root help should not advertise the internal client command: {stdout}"
    );
}

#[test]
fn explicit_client_command_respects_nested_guard() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .arg("client")
        .env("ZYNK_ENV", "1")
        .env("XDG_CONFIG_HOME", &base)
        .env_remove("ZYNK_CONFIG_PATH")
        .output()
        .unwrap();

    cleanup_test_base(&base);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nested zynk is disabled by default"),
        "client should fail at the nested guard before connecting: {stderr}"
    );
}

#[test]
fn removed_show_changelog_flag_fails_before_nested_guard() {
    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .arg("--show-changelog")
        .env("ZYNK_ENV", "1")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown option: --show-changelog"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("nested zynk"),
        "unknown flag should be rejected before nested guard: {stderr}"
    );
}

#[test]
fn daemon_style_server_writes_the_fatal_db_cause_to_its_log() {
    // Gate-2 round 4 (item 3): the real launcher nulls stdio, so a fatal startup DB error must reach
    // the server's own durable log with a structured code — not only stderr.
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&config_home).unwrap();
    // A regular FILE where the SQLite home directory should be: create_dir_all fails => db_io_error.
    let blocker = config_home.join("not-a-dir");
    fs::write(&blocker, b"").unwrap();
    let sqlite_home = blocker.join("sqlite");

    let mut server =
        spawn_named_server_with(&config_home, &runtime_dir, "daemon", &sqlite_home, true);
    let socket = named_session_socket(&config_home, "daemon");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "server still running {:?} after a fatal DB error",
            started.elapsed()
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success(), "server must exit non-zero, got {status}");
    assert!(
        !socket.exists(),
        "no API socket may be bound after a fatal DB error"
    );
    let log = fs::read_to_string(&server.log_path).unwrap_or_default();
    assert!(
        log.contains("db_io_error") && log.contains("server startup aborted"),
        "durable server log must carry the structured cause; log at {}:\n{log}",
        server.log_path.display()
    );
    drop(server);
    cleanup_test_base(&base);
}

/// Gate-3 round 2 fixtures: real SQLite files planted with sqlx (the same driver the product uses).
fn sqlite_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A rollback-journal SQLite file at `path` with `sql` applied (all bytes in the main file).
fn plant_sqlite(path: &Path, sql: &str) {
    use sqlx::{Connection, Executor};
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        conn.execute(sql).await.unwrap();
        conn.close().await.unwrap();
    });
}

/// The `-wal` of a WAL-mode database whose committed rows were never checkpointed: written to
/// `wal_path` (only), as a copy/restore of a live WAL database leaves it.
fn plant_orphan_wal(wal_path: &Path, sql: &str) {
    use sqlx::{Connection, Executor};
    let src = wal_path.with_file_name("wal-source.db");
    fs::create_dir_all(wal_path.parent().unwrap()).unwrap();
    let holder = sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&src)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
        )
        .await
        .unwrap();
        conn.execute(sql).await.unwrap();
        conn
    });
    let src_wal = src.with_file_name("wal-source.db-wal");
    assert!(fs::metadata(&src_wal).unwrap().len() > 0);
    fs::copy(&src_wal, wal_path).unwrap();
    drop(holder);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&src_wal);
    let _ = fs::remove_file(src.with_file_name("wal-source.db-shm"));
}

/// Child-process half of `cli_fails_closed_on_a_hot_rollback_journal`: a rollback-mode writer that
/// spills past its page cache inside an open transaction and dies without committing.
#[test]
#[ignore = "helper: run only by its parent test"]
fn hot_journal_crashing_writer() {
    use sqlx::{Connection, Executor};
    let Ok(path) = std::env::var("ZYNK_TEST_HOT_JOURNAL_DB") else {
        return;
    };
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        conn.execute(
            "PRAGMA journal_mode = DELETE; PRAGMA cache_size = 8; \
             CREATE TABLE customer_data (id INTEGER PRIMARY KEY, payload BLOB); \
             INSERT INTO customer_data VALUES (0, zeroblob(1024))",
        )
        .await
        .unwrap();
        conn.execute("BEGIN IMMEDIATE").await.unwrap();
        for id in 1..400 {
            sqlx::query("INSERT INTO customer_data VALUES (?, randomblob(1024))")
                .bind(id)
                .execute(&mut conn)
                .await
                .unwrap();
        }
        std::mem::forget(conn);
    });
    std::process::exit(0);
}

#[test]
fn cli_fails_closed_on_a_hot_rollback_journal() {
    // Codex Gate-2 round 8 (P1), at the CLI boundary: a foreign database left by a crashed writer
    // (hot -journal) must be refused by `db status` and `query` before any connection exists — a
    // read-write open would roll the journal back (main file rewritten, journal deleted).
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let db = config_home.join("sqlite").join("zynk.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hot_journal_crashing_writer",
            "--ignored",
            "--nocapture",
        ])
        .env("ZYNK_TEST_HOT_JOURNAL_DB", &db)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "crashing writer helper failed: {status}");
    let journal = db.with_file_name("zynk.db-journal");
    let journal_bytes = fs::read(&journal).unwrap();
    assert!(
        journal_bytes.len() > 512 && journal_bytes[0] != 0,
        "fixture must leave a hot journal"
    );
    let before = (fs::read(&db).unwrap(), journal_bytes);

    let status = run_named_cli(&config_home, &runtime_dir, &["db", "status"]);
    assert!(
        !status.status.success(),
        "db status must fail closed: {status:?}"
    );
    assert!(String::from_utf8_lossy(&status.stderr).contains("db_hot_journal"));
    let query = run_named_cli(&config_home, &runtime_dir, &["query", "customer", "--json"]);
    assert!(!query.status.success(), "query must fail closed: {query:?}");
    assert!(String::from_utf8_lossy(&query.stdout).contains("db_hot_journal"));
    assert_eq!(
        (fs::read(&db).unwrap(), fs::read(&journal).unwrap()),
        before,
        "main/-journal bytes changed"
    );
    cleanup_test_base(&base);
}

#[test]
fn cli_fails_closed_on_a_hot_rollback_journal_behind_a_symlink() {
    // Codex Gate-2 round 9 (P1), at the CLI boundary: `zynk.db -> foreign.db` whose TARGET carries a
    // hot journal must be refused by `db status` and `query`, both files byte-identical.
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sqlite_home = config_home.join("sqlite");
    fs::create_dir_all(&sqlite_home).unwrap();
    let foreign = sqlite_home.join("foreign.db");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hot_journal_crashing_writer",
            "--ignored",
            "--nocapture",
        ])
        .env("ZYNK_TEST_HOT_JOURNAL_DB", &foreign)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "crashing writer helper failed: {status}");
    let journal = sqlite_home.join("foreign.db-journal");
    assert!(
        fs::read(&journal).unwrap()[0] != 0,
        "fixture must leave a hot journal"
    );
    let before = (fs::read(&foreign).unwrap(), fs::read(&journal).unwrap());
    std::os::unix::fs::symlink(&foreign, sqlite_home.join("zynk.db")).unwrap();

    let status = run_named_cli(&config_home, &runtime_dir, &["db", "status"]);
    assert!(
        !status.status.success(),
        "db status must fail closed: {status:?}"
    );
    assert!(String::from_utf8_lossy(&status.stderr).contains("db_hot_journal"));
    let query = run_named_cli(&config_home, &runtime_dir, &["query", "customer", "--json"]);
    assert!(!query.status.success(), "query must fail closed: {query:?}");
    assert!(String::from_utf8_lossy(&query.stdout).contains("db_hot_journal"));
    assert_eq!(
        (fs::read(&foreign).unwrap(), fs::read(&journal).unwrap()),
        before,
        "target bytes changed"
    );
    cleanup_test_base(&base);
}

#[test]
fn concurrent_named_server_cold_starts_fail_each_old_orphan_exactly_once() {
    // Gate-3 round 3 pre-read (arbiter r79-orphanrace): two named servers starting together both
    // ran orphan recovery over the same old event-less messages and each recorded a `failed`
    // event (delivery_seq bumped twice). Recovery must claim each orphan atomically.
    const ORPHANS: i64 = 300;
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sqlite_home = config_home.join("sqlite");
    let mut seed =
        spawn_named_server_with_env(&config_home, &runtime_dir, "seed", &sqlite_home, false, &[]);
    let socket_seed = named_session_socket(&config_home, "seed");
    wait_for_named_server_socket(&mut seed, "seed", &socket_seed, Duration::from_secs(15));
    let created = send_request(
        &socket_seed,
        r#"{"id":"test:workspace:create","method":"workspace.create","params":{"cwd":"/tmp","focus":true}}"#,
    );
    let pane = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let report = send_request(
        &socket_seed,
        &format!(
            r#"{{"id":"test:report","method":"pane.report_agent","params":{{"pane_id":"{pane}","source":"hook","agent":"codex","state":"idle"}}}}"#
        ),
    );
    assert!(report.get("error").is_none(), "{report}");
    let db = sqlite_home.join("zynk.db");
    let template = run_cli_json_with_env(
        &config_home,
        &runtime_dir,
        &socket_seed,
        &["send", &pane, "--", "template"],
    );
    let template_id = template["message_id"].as_str().unwrap().to_string();
    let _ = send_request(
        &socket_seed,
        r#"{"id":"test:stop","method":"server.stop","params":{}}"#,
    );
    if !wait_for_pid_exit(seed.child.id(), Duration::from_secs(10)) {
        let _ = seed.child.kill();
    }
    let _ = seed.child.wait();

    // Clone the template into ORPHANS old, event-less messages (a sender that died long ago).
    let columns = message_columns(&db);
    let exprs: Vec<String> = columns
        .iter()
        .map(|c| match c.as_str() {
            "id" => "'msg_orphan_' || printf('%04d', n.x)".to_string(),
            "conversation_seq" => "m.conversation_seq + n.x".to_string(),
            "delivery_seq" => "0".to_string(),
            "created_at" => "'2020-01-01T00:00:00Z'".to_string(),
            other => format!("m.{other}"),
        })
        .collect();
    sqlite_exec(
        &db,
        &format!(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < {ORPHANS}) \
             INSERT INTO messages ({}) SELECT {} FROM messages m, n WHERE m.id = '{template_id}'",
            columns.join(", "),
            exprs.join(", ")
        ),
    );

    let mut alpha = spawn_named_server_with_env(
        &config_home,
        &runtime_dir,
        "alpha",
        &sqlite_home,
        false,
        &[],
    );
    let mut beta =
        spawn_named_server_with_env(&config_home, &runtime_dir, "beta", &sqlite_home, false, &[]);
    let socket_a = named_session_socket(&config_home, "alpha");
    let socket_b = named_session_socket(&config_home, "beta");
    wait_for_named_server_socket(&mut alpha, "alpha", &socket_a, Duration::from_secs(30));
    wait_for_named_server_socket(&mut beta, "beta", &socket_b, Duration::from_secs(30));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let counts = orphan_event_counts(&db);
        let recovered = counts.iter().filter(|(_, n)| *n >= 1).count() as i64;
        if recovered == ORPHANS || Instant::now() > deadline {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    let counts = orphan_event_counts(&db);
    let doubled: Vec<&(String, i64)> = counts.iter().filter(|(_, n)| *n != 1).collect();
    for socket in [&socket_b, &socket_a] {
        let _ = send_request(
            socket,
            r#"{"id":"test:stop","method":"server.stop","params":{}}"#,
        );
    }
    for server in [&mut alpha, &mut beta] {
        if !wait_for_pid_exit(server.child.id(), Duration::from_secs(10)) {
            let _ = server.child.kill();
        }
        let _ = server.child.wait();
    }
    assert_eq!(
        counts.len() as i64,
        ORPHANS,
        "every old orphan is recovered exactly once"
    );
    assert!(
        doubled.is_empty(),
        "{} orphans got a wrong number of failed events, e.g. {:?}",
        doubled.len(),
        doubled.first()
    );
    cleanup_test_base(&base);
}

fn message_columns(db: &Path) -> Vec<String> {
    use sqlx::{Connection, Row};
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .create_if_missing(false),
        )
        .await
        .unwrap();
        sqlx::query("PRAGMA table_info(messages)")
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect()
    })
}

/// `(message_id, failed-event count)` for every cloned orphan that has at least one event.
fn orphan_event_counts(db: &Path) -> Vec<(String, i64)> {
    use sqlx::{Connection, Row};
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .create_if_missing(false),
        )
        .await
        .unwrap();
        sqlx::query(
            "SELECT message_id, COUNT(*) AS n FROM delivery_events \
             WHERE message_id LIKE 'msg_orphan_%' AND event_type = 'failed' GROUP BY message_id",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get::<String, _>("message_id"), row.get::<i64, _>("n")))
        .collect()
    })
}

#[test]
fn a_second_named_session_does_not_fail_a_peers_in_flight_send() {
    // Gate-3 round 3: every ordinary named server runs cold-start orphan recovery on the shared
    // database. A live peer's freshly persisted message (no delivery event yet) must be left alone;
    // a genuinely old orphan (older than the grace window) is still recovered.
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sqlite_home = config_home.join("sqlite");
    let mut alpha = spawn_named_server_with_env(
        &config_home,
        &runtime_dir,
        "alpha",
        &sqlite_home,
        false,
        &[],
    );
    let socket_a = named_session_socket(&config_home, "alpha");
    wait_for_named_server_socket(&mut alpha, "alpha", &socket_a, Duration::from_secs(15));
    let created = send_request(
        &socket_a,
        r#"{"id":"test:workspace:create","method":"workspace.create","params":{"cwd":"/tmp","focus":true}}"#,
    );
    let pane = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let report = send_request(
        &socket_a,
        &format!(
            r#"{{"id":"test:report","method":"pane.report_agent","params":{{"pane_id":"{pane}","source":"hook","agent":"codex","state":"idle"}}}}"#
        ),
    );
    assert!(report.get("error").is_none(), "{report}");
    let db = sqlite_home.join("zynk.db");
    let fresh = run_cli_json_with_env(
        &config_home,
        &runtime_dir,
        &socket_a,
        &["send", &pane, "--", "in flight"],
    );
    let fresh_id = fresh["message_id"].as_str().unwrap().to_string();
    let old = run_cli_json_with_env(
        &config_home,
        &runtime_dir,
        &socket_a,
        &["send", &pane, "--", "long dead"],
    );
    let old_id = old["message_id"].as_str().unwrap().to_string();
    // Model the in-flight window (message committed, first event not yet recorded) and a genuine
    // old orphan (a sender that died long ago).
    sqlite_exec(&db, &format!("DELETE FROM delivery_events WHERE message_id IN ('{fresh_id}', '{old_id}'); UPDATE messages SET created_at = '2020-01-01T00:00:00Z' WHERE id = '{old_id}'"));

    let mut beta =
        spawn_named_server_with_env(&config_home, &runtime_dir, "beta", &sqlite_home, false, &[]);
    let socket_b = named_session_socket(&config_home, "beta");
    wait_for_named_server_socket(&mut beta, "beta", &socket_b, Duration::from_secs(15));
    assert_eq!(
        delivery_events_of(&db, &fresh_id),
        Vec::<String>::new(),
        "beta failed a live peer's in-flight send"
    );
    assert_eq!(
        delivery_events_of(&db, &old_id),
        vec!["failed"],
        "beta must still recover a genuine old orphan"
    );

    for socket in [&socket_b, &socket_a] {
        let _ = send_request(
            socket,
            r#"{"id":"test:stop","method":"server.stop","params":{}}"#,
        );
    }
    for server in [&mut alpha, &mut beta] {
        if !wait_for_pid_exit(server.child.id(), Duration::from_secs(10)) {
            let _ = server.child.kill();
        }
        let _ = server.child.wait();
    }
    cleanup_test_base(&base);
}

fn sqlite_exec(db: &Path, sql: &str) {
    use sqlx::{Connection, Executor};
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .create_if_missing(false),
        )
        .await
        .unwrap();
        conn.execute(sql).await.unwrap();
        conn.close().await.unwrap();
    });
}

fn delivery_events_count(db: &Path) -> i64 {
    use sqlx::{Connection, Row};
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .create_if_missing(false),
        )
        .await
        .unwrap();
        let count = sqlx::query("SELECT COUNT(*) AS count FROM delivery_events")
            .fetch_one(&mut conn)
            .await
            .unwrap()
            .get::<i64, _>("count");
        conn.close().await.unwrap();
        count
    })
}

fn delivery_events_of(db: &Path, message_id: &str) -> Vec<String> {
    use sqlx::{Connection, Row};
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .create_if_missing(false),
        )
        .await
        .unwrap();
        sqlx::query("SELECT event_type FROM delivery_events WHERE message_id = ? ORDER BY seq")
            .bind(message_id)
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("event_type"))
            .collect()
    })
}

#[test]
fn a_second_named_session_never_reruns_a_job_another_server_owns() {
    // Gate-3 round 3 (AUD-310-WORKER-001): every zynk server sharing the global database runs its
    // own embedding worker. Server A holds a job `running` (blocking provider); server B, started
    // on the same database, must neither recover (reset) that live claim nor run it: attempts stay
    // at 1 while A holds it, the job finishes once for A, and B keeps serving its own work.
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sqlite_home = config_home.join("sqlite");
    let release = base.join("release-embedding");
    let release_str = release.to_string_lossy().to_string();
    let mut a = spawn_named_server_with_env(
        &config_home,
        &runtime_dir,
        "alpha",
        &sqlite_home,
        false,
        &[
            ("ZYNK_EMBED_PROVIDER", "fake-blocking"),
            ("ZYNK_TEST_EMBED_RELEASE_FILE", &release_str),
            ("ZYNK_EMBED_POLL_MS", "50"),
        ],
    );
    let socket_a = named_session_socket(&config_home, "alpha");
    wait_for_named_server_socket(&mut a, "alpha", &socket_a, Duration::from_secs(15));
    let created = send_request(
        &socket_a,
        r#"{"id":"test:workspace:create","method":"workspace.create","params":{"cwd":"/tmp","focus":true}}"#,
    );
    let pane = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    // The send target needs a hook-authoritative agent identity (what the agent hooks report).
    let report = send_request(
        &socket_a,
        &format!(
            r#"{{"id":"test:report","method":"pane.report_agent","params":{{"pane_id":"{pane}","source":"hook","agent":"codex","state":"idle"}}}}"#
        ),
    );
    assert!(report.get("error").is_none(), "{report}");
    let sent = run_cli_json_with_env(
        &config_home,
        &runtime_dir,
        &socket_a,
        &["send", &pane, "--", "held by alpha"],
    );
    let message_id = sent["message_id"].as_str().unwrap().to_string();
    let db = sqlite_home.join("zynk.db");
    wait_for_job(&db, &message_id, ("running", 1), Duration::from_secs(10));

    // Server B on the same database (default fake provider: it WOULD run the job if it took it).
    let mut b = spawn_named_server_with_env(
        &config_home,
        &runtime_dir,
        "beta",
        &sqlite_home,
        false,
        &[("ZYNK_EMBED_POLL_MS", "50")],
    );
    let socket_b = named_session_socket(&config_home, "beta");
    wait_for_named_server_socket(&mut b, "beta", &socket_b, Duration::from_secs(15));
    thread::sleep(Duration::from_millis(600));
    assert_eq!(
        read_job(&db, &message_id),
        Some(("running".to_string(), 1)),
        "B must not reset or rerun A's live job"
    );

    fs::write(&release, b"go").unwrap();
    wait_for_job(&db, &message_id, ("done", 1), Duration::from_secs(10));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        read_job(&db, &message_id),
        Some(("done".to_string(), 1)),
        "exactly one attempt across both servers"
    );

    for socket in [&socket_b, &socket_a] {
        let _ = send_request(
            socket,
            r#"{"id":"test:stop","method":"server.stop","params":{}}"#,
        );
    }
    for server in [&mut a, &mut b] {
        if !wait_for_pid_exit(server.child.id(), Duration::from_secs(10)) {
            let _ = server.child.kill();
        }
        let _ = server.child.wait();
    }
    cleanup_test_base(&base);
}

fn read_job(db: &Path, message_id: &str) -> Option<(String, i64)> {
    use sqlx::{Connection, Row};
    sqlite_block_on(async {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .create_if_missing(false),
        )
        .await
        .ok()?;
        sqlx::query("SELECT status, attempts FROM embedding_jobs WHERE message_id = ?")
            .bind(message_id)
            .fetch_optional(&mut conn)
            .await
            .ok()
            .flatten()
            .map(|row| {
                (
                    row.get::<String, _>("status"),
                    row.get::<i64, _>("attempts"),
                )
            })
    })
}

fn wait_for_job(db: &Path, message_id: &str, wanted: (&str, i64), timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let state = read_job(db, message_id);
        if state.as_ref().map(|(s, a)| (s.as_str(), *a)) == Some(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "job for {message_id} never reached {wanted:?} (now {state:?})"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

/// `zynk <args>` against a named session's socket with the isolated DB env, parsing the F4 JSON
/// outcome line.
fn run_cli_json_with_env(
    config_home: &Path,
    runtime_dir: &Path,
    socket: &Path,
    args: &[&str],
) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("ZYNK_SOCKET_PATH", socket)
        .env("ZYNK_SQLITE_HOME", config_home.join("sqlite"))
        .env_remove("ZYNK_HOME")
        .env_remove("ZYNK_CLIENT_SOCKET_PATH")
        .env_remove("ZYNK_ENV")
        .env_remove("ZYNK_PANE_ID")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "zynk {} failed: {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON in: {stdout}"));
    serde_json::from_str(line).unwrap()
}

#[test]
fn cli_fails_closed_on_an_orphan_wal_beside_an_absent_db() {
    // Gate-3 round 2 (G3-R2-DB-002), at the CLI boundary: a nonempty `zynk.db-wal` with no
    // `zynk.db` is existing data (ADR 0011). `db status` and `query` must refuse, create no
    // database, and leave the WAL byte-identical.
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sqlite_home = config_home.join("sqlite");
    let wal = sqlite_home.join("zynk.db-wal");
    plant_orphan_wal(
        &wal,
        "CREATE TABLE foreign_records (v TEXT); INSERT INTO foreign_records VALUES ('sentinel-wal-only')",
    );
    let before = fs::read(&wal).unwrap();

    let status = run_named_cli(&config_home, &runtime_dir, &["db", "status"]);
    assert!(
        !status.status.success(),
        "db status must fail closed: {status:?}"
    );
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("db_orphan_sidecar"), "{stderr}");

    let query = run_named_cli(&config_home, &runtime_dir, &["query", "sentinel", "--json"]);
    assert!(!query.status.success(), "query must fail closed: {query:?}");
    let stdout = String::from_utf8_lossy(&query.stdout);
    assert!(stdout.contains("db_orphan_sidecar"), "{stdout}");

    assert!(
        !sqlite_home.join("zynk.db").exists(),
        "zynk.db was created over an orphan WAL"
    );
    assert_eq!(
        fs::read(&wal).unwrap(),
        before,
        "the orphan WAL bytes changed"
    );
    cleanup_test_base(&base);
}

#[test]
fn cli_reports_a_newer_lineage_as_not_ready_and_refuses_to_open_it() {
    // Gate-3 round 2 (G3-R2-DB-005): every built-in migration recorded plus a successful unknown
    // newer row is a database a NEWER zynk migrated — `db status` must never call it "ready", and
    // `query` must refuse it with a distinct code, leaving the file byte-identical.
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let db = config_home.join("sqlite").join("zynk.db");
    let seeded = run_named_cli(&config_home, &runtime_dir, &["query", "warmup", "--json"]);
    assert!(
        seeded.status.success(),
        "first query must initialize the DB: {seeded:?}"
    );
    assert!(db.exists());
    plant_sqlite(
        &db,
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'future', 1, x'00', 0)",
    );
    let before = fs::read(&db).unwrap();

    let status = run_named_cli(&config_home, &runtime_dir, &["db", "status"]);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success(), "{status:?}");
    assert!(
        stdout.contains("NEWER") && stdout.contains("9999"),
        "{stdout}"
    );
    assert!(!stdout.contains("ready"), "{stdout}");

    let query = run_named_cli(&config_home, &runtime_dir, &["query", "warmup", "--json"]);
    assert!(
        !query.status.success(),
        "query must refuse a newer database: {query:?}"
    );
    let stdout = String::from_utf8_lossy(&query.stdout);
    assert!(stdout.contains("db_newer_lineage"), "{stdout}");
    assert_eq!(
        fs::read(&db).unwrap(),
        before,
        "the newer database bytes changed"
    );
    cleanup_test_base(&base);
}

#[test]
fn cli_escapes_control_characters_in_foreign_schema_names() {
    // Gate-3 round 2 (G3-R2-DB-006): a foreign table name carrying LF/ESC must reach the terminal
    // escaped, never raw (terminal/log control injection).
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let db = config_home.join("sqlite").join("zynk.db");
    plant_sqlite(&db, "CREATE TABLE \"evil\n\u{1b}[31mred\u{1b}[0m\" (x)");

    let status = run_named_cli(&config_home, &runtime_dir, &["db", "status"]);
    assert!(status.status.success(), "{status:?}");
    assert!(
        !status.stdout.contains(&0x1b) && !status.stderr.contains(&0x1b),
        "raw ESC leaked: {:?}",
        String::from_utf8_lossy(&status.stdout)
    );
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("FOREIGN"), "{stdout}");
    assert!(
        stdout.contains("evil\\n\\u{1b}[31mred\\u{1b}[0m"),
        "{stdout}"
    );
    cleanup_test_base(&base);
}

#[test]
fn server_startup_is_fatal_when_the_db_init_lock_is_held_elsewhere() {
    // Gate-3 G3-STARTUP-001: a pathological external holder of the DB init lock must make the
    // server EXIT within the bounded wait — never bind a degraded API socket that a later
    // `session stop` cannot tear down within its contract.
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sqlite_home = config_home.join("sqlite");
    fs::create_dir_all(&sqlite_home).unwrap();
    let holder = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sqlite_home.join("zynk.db.init-lock"))
        .unwrap();
    holder.lock().unwrap();

    let mut server = spawn_named_server(&config_home, &runtime_dir, "held");
    let socket = named_session_socket(&config_home, "held");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "server still running {:?} after startup with a held init lock",
            started.elapsed()
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success(), "server must exit non-zero, got {status}");
    assert!(
        !socket.exists(),
        "no API socket may be bound by a server that failed to initialize"
    );
    let log = fs::read_to_string(&server.log_path).unwrap_or_default();
    assert!(
        log.contains("db_init_lock_timeout"),
        "startup must report the init-lock timeout; log:\n{log}"
    );
    holder.unlock().unwrap();
    drop(server);
    cleanup_test_base(&base);
}

#[test]
fn named_sessions_use_separate_servers_and_workspace_state() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");

    let mut alpha = spawn_named_server(&config_home, &runtime_dir, "alpha");
    let mut beta = spawn_named_server(&config_home, &runtime_dir, "beta");

    wait_for_named_server_socket(
        &mut alpha,
        "alpha",
        &named_session_socket(&config_home, "alpha"),
        Duration::from_secs(5),
    );
    wait_for_named_server_socket(
        &mut beta,
        "beta",
        &named_session_socket(&config_home, "beta"),
        Duration::from_secs(5),
    );

    run_named_cli_json(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "alpha",
            "workspace",
            "create",
            "--label",
            "alpha-ws",
            "--no-focus",
        ],
    );
    run_named_cli_json(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "beta",
            "workspace",
            "create",
            "--label",
            "beta-ws",
            "--no-focus",
        ],
    );

    let alpha_list = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "alpha", "workspace", "list"],
    );
    let beta_list = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "beta", "workspace", "list"],
    );

    let alpha_labels: Vec<_> = alpha_list["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["label"].as_str().unwrap())
        .collect();
    let beta_labels: Vec<_> = beta_list["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["label"].as_str().unwrap())
        .collect();

    assert_eq!(alpha_labels, vec!["alpha-ws"]);
    assert_eq!(beta_labels, vec!["beta-ws"]);

    let beta_via_explicit_session = run_named_cli_with_socket_override(
        &config_home,
        &runtime_dir,
        &["--session", "beta", "workspace", "list"],
        Some(&named_session_socket(&config_home, "alpha")),
    );
    assert!(
        beta_via_explicit_session.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&beta_via_explicit_session.stderr)
    );
    let beta_via_explicit_session: serde_json::Value =
        serde_json::from_slice(&beta_via_explicit_session.stdout).unwrap();
    let labels_via_explicit: Vec<_> = beta_via_explicit_session["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels_via_explicit, vec!["beta-ws"]);

    let human_sessions = run_named_cli(&config_home, &runtime_dir, &["session", "list"]);
    assert!(human_sessions.status.success());
    let human_sessions = String::from_utf8_lossy(&human_sessions.stdout);
    assert!(human_sessions.contains("name"), "stdout: {human_sessions}");
    assert!(
        human_sessions.contains("status"),
        "stdout: {human_sessions}"
    );
    assert!(human_sessions.contains("alpha"), "stdout: {human_sessions}");
    assert!(
        human_sessions.contains("running"),
        "stdout: {human_sessions}"
    );
    assert!(
        human_sessions.contains("/sessions/beta"),
        "stdout: {human_sessions}"
    );

    let sessions = run_named_cli_json(&config_home, &runtime_dir, &["session", "list", "--json"]);
    let sessions = sessions["sessions"].as_array().unwrap();
    let default_session = sessions
        .iter()
        .find(|session| session["name"] == "default")
        .unwrap();
    let alpha_session = sessions
        .iter()
        .find(|session| session["name"] == "alpha")
        .unwrap();
    let beta_session = sessions
        .iter()
        .find(|session| session["name"] == "beta")
        .unwrap();
    assert_eq!(default_session["default"], true);
    assert_eq!(default_session["running"], false);
    assert_eq!(alpha_session["running"], true);
    assert_eq!(beta_session["running"], true);
    assert!(alpha_session["socket_path"]
        .as_str()
        .unwrap()
        .ends_with("/sessions/alpha/zynk.sock"));
    assert!(beta_session["session_dir"]
        .as_str()
        .unwrap()
        .ends_with("/sessions/beta"));

    let delete_running = run_named_cli(&config_home, &runtime_dir, &["session", "delete", "alpha"]);
    assert_eq!(delete_running.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&delete_running.stderr).contains("stop it before deleting"),
        "stderr: {}",
        String::from_utf8_lossy(&delete_running.stderr)
    );

    let delete_default = run_named_cli(
        &config_home,
        &runtime_dir,
        &["session", "delete", "default"],
    );
    assert_eq!(delete_default.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&delete_default.stderr).contains("default session"),
        "stderr: {}",
        String::from_utf8_lossy(&delete_default.stderr)
    );

    let stopped_alpha = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["session", "stop", "alpha", "--json"],
    );
    assert_eq!(stopped_alpha["stopped"], true);
    assert_eq!(stopped_alpha["session"]["running"], false);

    let deleted_alpha = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["session", "delete", "alpha", "--json"],
    );
    assert_eq!(deleted_alpha["deleted"], true);
    assert!(!config_home
        .join(app_dir_name())
        .join("sessions")
        .join("alpha")
        .exists());

    let _ = run_named_cli(&config_home, &runtime_dir, &["session", "stop", "beta"]);
    drop(alpha);
    drop(beta);
    cleanup_test_base(&base);
}

#[test]
fn integration_commands_run_locally_when_server_is_missing() {
    let base = unique_test_dir();
    let home_dir = base.join("home");
    let extensions_dir = home_dir.join(".pi/agent/extensions");
    fs::create_dir_all(&extensions_dir).unwrap();

    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let missing_socket = runtime_dir.join("missing.sock");

    let expected_extension = extensions_dir.join("zynk-agent-state.ts");
    assert!(
        !expected_extension.exists(),
        "test setup should start without extension file"
    );

    let workspace_list = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(["workspace", "list"])
        .env("ZYNK_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(workspace_list.status.code(), Some(1));

    let integration_install = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(["integration", "install", "pi"])
        .env("ZYNK_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(integration_install.status.code(), Some(0));
    assert!(
        expected_extension.exists(),
        "integration install should write local files without a server"
    );

    let integration_status = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(["integration", "status"])
        .env("ZYNK_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(integration_status.status.code(), Some(0));
    let status_stdout = String::from_utf8_lossy(&integration_status.stdout);
    assert!(status_stdout.contains("pi: current (v9)"));
    assert!(status_stdout.contains("claude: not installed"));

    let integration_uninstall = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(["integration", "uninstall", "pi"])
        .env("ZYNK_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(integration_uninstall.status.code(), Some(0));
    assert!(
        !expected_extension.exists(),
        "integration uninstall should remove local files without a server"
    );

    cleanup_test_base(&base);
}

#[test]
fn integration_status_outdated_only_prints_action_for_legacy_install() {
    let base = unique_test_dir();
    let home_dir = base.join("home");
    let extensions_dir = home_dir.join(".pi/agent/extensions");
    fs::create_dir_all(&extensions_dir).unwrap();
    fs::write(
        extensions_dir.join("zynk-agent-state.ts"),
        "// legacy zynk integration\n",
    )
    .unwrap();

    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let missing_socket = runtime_dir.join("missing.sock");

    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(["integration", "status", "--outdated-only"])
        .env("ZYNK_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("installed zynk integrations need updating"));
    assert!(stderr.contains("zynk integration install pi"));

    cleanup_test_base(&base);
}

#[test]
fn integration_status_rejects_unknown_flags() {
    let base = unique_test_dir();
    let home_dir = base.join("home");
    fs::create_dir_all(&home_dir).unwrap();
    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let missing_socket = runtime_dir.join("missing.sock");

    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(["integration", "status", "--wat"])
        .env("ZYNK_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));

    cleanup_test_base(&base);
}

#[test]
fn status_commands_report_client_and_server_versions() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let full = run_cli(&socket_path, &["status"]);
    assert!(
        full.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&full.stderr)
    );
    let full_stdout = String::from_utf8_lossy(&full.stdout);
    assert!(full_stdout.contains("client:\n"), "stdout: {full_stdout}");
    assert!(
        full_stdout.contains(&format!("  version: {}", env!("CARGO_PKG_VERSION"))),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains("  protocol: 19"),
        "stdout: {full_stdout}"
    );
    assert!(full_stdout.contains("server:\n"), "stdout: {full_stdout}");
    assert!(
        full_stdout.contains("  status: running"),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains("  compatible: yes"),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains("  restart_needed: no"),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains(&socket_path.display().to_string()),
        "stdout: {full_stdout}"
    );

    let server = run_cli(&socket_path, &["status", "server"]);
    assert!(server.status.success());
    let server_stdout = String::from_utf8_lossy(&server.stdout);
    assert!(
        server_stdout.contains("status: running"),
        "stdout: {server_stdout}"
    );
    assert!(
        server_stdout.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
        "stdout: {server_stdout}"
    );
    assert!(
        server_stdout.contains("protocol: 19"),
        "stdout: {server_stdout}"
    );

    let client = run_cli(&socket_path, &["status", "client"]);
    assert!(client.status.success());
    let client_stdout = String::from_utf8_lossy(&client.stdout);
    assert!(
        client_stdout.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
        "stdout: {client_stdout}"
    );
    assert!(
        client_stdout.contains("protocol: 19"),
        "stdout: {client_stdout}"
    );
    assert!(
        client_stdout.contains("binary: "),
        "stdout: {client_stdout}"
    );

    let full_json = run_cli_json(&socket_path, &["status", "--json"]);
    assert_eq!(full_json["client"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(full_json["client"]["protocol"], 19);
    assert_eq!(full_json["server"]["status"], "running");
    assert_eq!(full_json["server"]["running"], true);
    assert_eq!(full_json["server"]["compatible"], true);
    assert_eq!(
        full_json["server"]["socket"],
        socket_path.display().to_string()
    );
    assert_eq!(full_json["server"]["restart_needed"], false);
    assert_eq!(full_json["update"]["restart_needed"], false);

    let server_json = run_cli_json(&socket_path, &["status", "server", "--json"]);
    assert_eq!(server_json["status"], "running");
    assert_eq!(server_json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(server_json["protocol"], 19);
    assert_eq!(server_json["compatible"], true);

    let client_json = run_cli_json(&socket_path, &["status", "client", "--json"]);
    assert_eq!(client_json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(client_json["protocol"], 19);
    assert!(client_json["binary"]
        .as_str()
        .is_some_and(|path| !path.is_empty()));

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn a_remote_bridge_bootstraps_its_running_image_after_path_replacement() {
    use interprocess::local_socket::traits::StreamCommon as _;
    use interprocess::local_socket::{prelude::*, GenericFilePath, Stream};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket = runtime_dir.join("zynk.sock");
    fs::create_dir_all(config_home.join("zynk-dev")).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::write(
        config_home.join("zynk-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();
    let path = base.join("zynk");
    fs::copy(env!("CARGO_BIN_EXE_zynk"), &path).unwrap();
    let image = fs::File::open(&path).unwrap();
    let marker = base.join("wrong-image");
    let replacement = base.join("replacement");
    fs::write(
        &replacement,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&replacement, &path).unwrap();
    let log_path = base.join("bridge.log");
    let child = Command::new(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        image.as_raw_fd()
    ))
    .arg("remote-client-bridge")
    .env("XDG_CONFIG_HOME", &config_home)
    .env("XDG_RUNTIME_DIR", &runtime_dir)
    .env("ZYNK_SQLITE_HOME", config_home.join("sqlite"))
    .env("ZYNK_SOCKET_PATH", &socket)
    .env(
        "ZYNK_CLIENT_SOCKET_PATH",
        runtime_dir.join("zynk-client.sock"),
    )
    .env_remove("ZYNK_HOME")
    .env_remove("ZYNK_SESSION")
    .env_remove("ZYNK_ENV")
    .env_remove("ZYNK_PANE_ID")
    .env_remove("ZYNK_TEST_TRUST_PEER_PID")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(fs::File::create(&log_path).unwrap())
    .spawn()
    .unwrap();
    register_spawned_zynk_pid(Some(child.id()));
    register_runtime_dir(&runtime_dir);
    let mut bridge = SpawnedServerProcess {
        child,
        log_path: log_path.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        if bridge.child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let started = socket.exists();
    // A copied/deleted executable is invisible to the usual pathname-based PID
    // scanner. Ask the isolated socket for its real kernel-reported daemon PID.
    let pid = socket
        .as_path()
        .to_fs_name::<GenericFilePath>()
        .and_then(Stream::connect)
        .ok()
        .and_then(|stream| stream.peer_creds().ok().and_then(|creds| creds.pid()))
        .and_then(|pid| u32::try_from(pid).ok());
    register_spawned_zynk_pid(pid);
    let image_meta = image.metadata().unwrap();
    let same_image = pid.is_some_and(|pid| {
        fs::metadata(format!("/proc/{pid}/exe"))
            .is_ok_and(|meta| (meta.dev(), meta.ino()) == (image_meta.dev(), image_meta.ino()))
    });
    let diagnostic = fs::read_to_string(&log_path).unwrap_or_default();
    let wrong_image = marker.exists();
    if started {
        let _ = run_cli(&socket, &["server", "stop"]);
    }
    drop(bridge);
    cleanup_test_base(&base);
    assert!(
        started && same_image,
        "bridge did not bootstrap its pinned image: {diagnostic}; pid={pid:?}"
    );
    assert!(!wrong_image, "the bridge bootstrapped substituted bytes");
}

#[test]
fn status_reports_not_running_when_server_socket_is_missing() {
    let base = unique_test_dir();
    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let socket_path = runtime_dir.join("missing.sock");

    let status = run_cli(&socket_path, &["status"]);
    assert!(status.status.success());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("  status: not running"), "stdout: {stdout}");
    assert!(stdout.contains("  restart_needed: no"), "stdout: {stdout}");
    assert!(
        stdout.contains(&socket_path.display().to_string()),
        "stdout: {stdout}"
    );

    let status_json = run_cli_json(&socket_path, &["status", "--json"]);
    assert_eq!(status_json["server"]["status"], "not_running");
    assert_eq!(status_json["server"]["running"], false);
    assert_eq!(
        status_json["server"]["socket"],
        socket_path.display().to_string()
    );
    assert_eq!(status_json["server"]["restart_needed"], false);
    assert_eq!(status_json["update"]["restart_needed"], false);

    cleanup_test_base(&base);
}

#[test]
fn server_stop_command_shuts_down_running_server() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");

    let mut zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    let db = config_home.join("sqlite").join("zynk.db");
    let delivery_events_before = delivery_events_count(&db);

    let stopped = run_cli(&socket_path, &["server", "stop"]);
    assert!(
        stopped.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(
        stopped.stdout.is_empty(),
        "server stop should not print stdout: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );

    assert!(
        !socket_path.exists() || UnixStream::connect(&socket_path).is_err(),
        "api socket should be removed or stale before server stop returns"
    );
    assert!(
        !client_socket.exists() || UnixStream::connect(&client_socket).is_err(),
        "client socket should be removed or stale before server stop returns"
    );

    let pid = zynk.child.process_id();
    let started = Instant::now();
    let exit_status = loop {
        if let Some(status) = zynk.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "server.stop returned success but the server did not exit"
        );
        thread::sleep(Duration::from_millis(25));
    };
    unregister_spawned_zynk_pid(pid);
    assert!(exit_status.success(), "server stop should exit cleanly");
    assert_eq!(
        delivery_events_count(&db),
        delivery_events_before,
        "server.stop must not create a zynk delivery event"
    );

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn server_stop_then_restart_restores_pane_history() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");
    let marker = "PERSISTED_HISTORY_AFTER_STOP";

    let mut zynk = spawn_zynk_with_pane_history(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let created = run_cli_json(
        &socket_path,
        &[
            "workspace",
            "create",
            "--cwd",
            base.to_str().expect("test path should be utf-8"),
            "--label",
            "history-restart",
        ],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("workspace create should return root pane id")
        .to_string();
    let sent = run_cli(
        &socket_path,
        &["pane", "send-text", &pane_id, &format!("echo {marker}\n")],
    );
    assert!(
        sent.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(3), Duration::from_millis(25), || {
            pane_read_recent_contains(&socket_path, &pane_id, marker)
        }),
        "pane should contain marker before server stop"
    );

    let stopped = run_cli(&socket_path, &["server", "stop"]);
    assert!(
        stopped.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );

    let pid = zynk.child.process_id();
    let exit_status = zynk.child.wait().unwrap();
    unregister_spawned_zynk_pid(pid);
    assert!(exit_status.success(), "server stop should exit cleanly");
    drop(zynk);

    let restarted = spawn_zynk_with_pane_history(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let workspaces = run_cli_json(&socket_path, &["workspace", "list"]);
    let workspace_id = workspaces["result"]["workspaces"]
        .as_array()
        .expect("workspace.list should return workspaces")
        .iter()
        .find(|workspace| workspace["label"] == "history-restart")
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .expect("restored workspace should exist")
        .to_string();
    let panes = run_cli_json(
        &socket_path,
        &["pane", "list", "--workspace", &workspace_id],
    );
    let restored_pane_id = panes["result"]["panes"]
        .as_array()
        .expect("pane.list should return panes")
        .first()
        .and_then(|pane| pane["pane_id"].as_str())
        .expect("restored pane should exist")
        .to_string();

    assert!(
        wait_until(Duration::from_secs(3), Duration::from_millis(25), || {
            pane_read_recent_contains(&socket_path, &restored_pane_id, marker)
        }),
        "restarted server should restore saved pane history"
    );

    cleanup_spawned_zynk(restarted, base);
}

#[test]
fn server_start_restores_legacy_session_through_api_identity() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let client_socket = runtime_dir.join("zynk-client.sock");
    let data_dir = config_home.join(app_dir_name());
    let pion_cwd = base.join("legacy-pion");
    let zynk_cwd = base.join("legacy-zynk");

    fs::create_dir_all(&pion_cwd).unwrap();
    fs::create_dir_all(&zynk_cwd).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    let pion_cwd = pion_cwd.to_str().expect("test cwd should be UTF-8");
    let zynk_cwd = zynk_cwd.to_str().expect("test cwd should be UTF-8");
    let legacy_session = include_str!("fixtures/session/legacy-pre-tabs-v2.json")
        .replace("/tmp/pion", pion_cwd)
        .replace("/tmp/zynk", zynk_cwd);
    fs::write(data_dir.join("session.json"), legacy_session).unwrap();

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let workspaces = run_cli_json(&socket_path, &["workspace", "list"]);
    let restored_workspace = workspaces["result"]["workspaces"]
        .as_array()
        .expect("workspace.list should return workspaces")
        .iter()
        .find(|workspace| workspace["label"] == "legacy")
        .expect("legacy workspace should restore");
    let workspace_id = restored_workspace["workspace_id"]
        .as_str()
        .expect("restored workspace should have public id")
        .to_string();
    assert_eq!(restored_workspace["pane_count"], 2);
    assert_eq!(restored_workspace["tab_count"], 1);
    assert_eq!(
        restored_workspace["active_tab_id"],
        format!("{workspace_id}:t1")
    );

    let panes = run_cli_json(
        &socket_path,
        &["pane", "list", "--workspace", &workspace_id],
    );
    let panes = panes["result"]["panes"]
        .as_array()
        .expect("pane.list should return panes");
    assert_eq!(panes.len(), 2);
    let root_pane_id = format!("{workspace_id}:p1");
    let focused_pane_id = format!("{workspace_id}:p2");
    assert!(panes.iter().any(|pane| {
        pane["pane_id"] == root_pane_id
            && pane["tab_id"] == format!("{workspace_id}:t1")
            && pane["cwd"] == pion_cwd
            && pane["focused"] == false
    }));
    assert!(panes.iter().any(|pane| {
        pane["pane_id"] == focused_pane_id
            && pane["tab_id"] == format!("{workspace_id}:t1")
            && pane["cwd"] == zynk_cwd
            && pane["focused"] == true
    }));

    let reported = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &focused_pane_id,
            "--source",
            "test",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    );
    assert!(
        reported.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&reported.stderr)
    );

    let agents = run_cli_json(&socket_path, &["agent", "list"]);
    let agents = agents["result"]["agents"]
        .as_array()
        .expect("agent.list should return agents");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["pane_id"], focused_pane_id);
    assert_eq!(agents[0]["workspace_id"], workspace_id);
    assert_eq!(agents[0]["agent"], "pi");
    assert_eq!(agents[0]["agent_status"], "working");

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn workspace_and_pane_management_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let reloaded = run_cli(&socket_path, &["server", "reload-config"]);
    assert!(
        reloaded.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&reloaded.stderr)
    );
    let reload_json: serde_json::Value = serde_json::from_slice(&reloaded.stdout).unwrap();
    assert_eq!(reload_json["result"]["type"], "config_reload");
    assert_eq!(reload_json["result"]["status"], "applied");

    let listed = run_cli(&socket_path, &["workspace", "list"]);
    assert!(listed.status.success());
    let listed_json: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed_json["result"]["type"], "workspace_list");
    assert_eq!(
        listed_json["result"]["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let workspace_id = created_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let panes = run_cli(&socket_path, &["pane", "list", "--workspace", "1"]);
    assert!(panes.status.success());
    let panes_json: serde_json::Value = serde_json::from_slice(&panes.stdout).unwrap();
    assert_eq!(panes_json["result"]["panes"].as_array().unwrap().len(), 1);

    let split = run_cli(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right"],
    );
    assert!(
        split.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&split.stderr)
    );
    let split_json: serde_json::Value = serde_json::from_slice(&split.stdout).unwrap();
    let split_pane_id = split_json["result"]["pane"]["pane_id"].as_str().unwrap();

    let fetched = run_cli(&socket_path, &["pane", "get", split_pane_id]);
    assert!(fetched.status.success());
    let fetched_json: serde_json::Value = serde_json::from_slice(&fetched.stdout).unwrap();
    assert_eq!(fetched_json["result"]["pane"]["pane_id"], split_pane_id);

    let closed = run_cli(&socket_path, &["pane", "close", split_pane_id]);
    assert!(closed.status.success());
    let closed_json: serde_json::Value = serde_json::from_slice(&closed.stdout).unwrap();
    assert_eq!(closed_json["result"]["type"], "ok");

    let renamed = run_cli(
        &socket_path,
        &["workspace", "rename", &workspace_id, "demo"],
    );
    assert!(renamed.status.success());
    let renamed_json: serde_json::Value = serde_json::from_slice(&renamed.stdout).unwrap();
    assert_eq!(renamed_json["result"]["workspace"]["label"], "demo");

    let focused = run_cli(&socket_path, &["workspace", "focus", &workspace_id]);
    assert!(focused.status.success());

    let closed_workspace = run_cli(&socket_path, &["workspace", "close", &workspace_id]);
    assert!(closed_workspace.status.success());
    let closed_workspace_json: serde_json::Value =
        serde_json::from_slice(&closed_workspace.stdout).unwrap();
    assert_eq!(closed_workspace_json["result"]["type"], "ok");

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn worktree_management_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let repo = base.join("repo");
    let checkout = base.join("checkout");
    create_committed_repo(&repo);

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let branch = "worktree/cli-wrapper";
    let created = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "create",
            "--cwd",
            repo.to_str().unwrap(),
            "--branch",
            branch,
            "--path",
            checkout.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(created["result"]["type"], "worktree_created");
    assert_eq!(created["result"]["worktree"]["branch"], branch);
    let child_workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(checkout.join("README.md").exists());

    let workspaces = run_cli_json(&socket_path, &["workspace", "list"]);
    let workspace_list = workspaces["result"]["workspaces"].as_array().unwrap();
    let parent_workspace_id = workspace_list
        .iter()
        .find(|workspace| workspace["worktree"]["is_linked_worktree"].as_bool() == Some(false))
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .unwrap()
        .to_string();
    assert!(workspace_list.iter().any(|workspace| {
        workspace["workspace_id"].as_str() == Some(child_workspace_id.as_str())
            && workspace["worktree"]["is_linked_worktree"].as_bool() == Some(true)
    }));

    let listed = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "list",
            "--workspace",
            &parent_workspace_id,
            "--json",
        ],
    );
    let listed_entry = listed["result"]["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["branch"].as_str() == Some(branch))
        .unwrap();
    assert_eq!(
        listed_entry["open_workspace_id"].as_str(),
        Some(child_workspace_id.as_str())
    );

    let opened = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "open",
            "--workspace",
            &parent_workspace_id,
            "--branch",
            branch,
            "--json",
        ],
    );
    assert_eq!(opened["result"]["type"], "worktree_opened");
    assert_eq!(opened["result"]["already_open"], true);
    assert_eq!(
        opened["result"]["workspace"]["workspace_id"].as_str(),
        Some(child_workspace_id.as_str())
    );

    fs::write(checkout.join("README.md"), "dirty\n").unwrap();
    let safe_remove = run_cli(
        &socket_path,
        &[
            "worktree",
            "remove",
            "--workspace",
            &child_workspace_id,
            "--json",
        ],
    );
    assert_eq!(safe_remove.status.code(), Some(1));
    let safe_remove_json: serde_json::Value = serde_json::from_slice(&safe_remove.stderr).unwrap();
    assert_eq!(
        safe_remove_json["error"]["code"],
        "dirty_worktree_requires_force"
    );
    assert!(checkout.exists());

    let force_removed = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "remove",
            "--workspace",
            &child_workspace_id,
            "--force",
            "--json",
        ],
    );
    assert_eq!(force_removed["result"]["type"], "worktree_removed");
    assert_eq!(force_removed["result"]["forced"], true);
    assert!(!checkout.exists());

    cleanup_spawned_zynk(zynk, base);
}

// upstream 46a2b25: a forced worktree remove now tears down terminal runtimes
// inside the checkout (shutdown policy) so processes holding the directory are
// killed before `git worktree remove --force`, then recovers the leftover dir.
#[test]
fn forced_worktree_remove_terminates_processes_inside_checkout() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let repo = base.join("repo");
    let checkout = base.join("checkout-with-process");
    create_committed_repo(&repo);

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "create",
            "--cwd",
            repo.to_str().unwrap(),
            "--branch",
            "worktree/force-process",
            "--path",
            checkout.to_str().unwrap(),
            "--json",
        ],
    );
    let child_workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let pid_file = base.join("worktree-remove-force.pid");
    let command = format!(
        "python3 -c 'import os,time,pathlib; pathlib.Path(r\"{}\").write_text(str(os.getpid())); time.sleep(1000)'",
        pid_file.display()
    );
    let ran = run_cli(&socket_path, &["pane", "run", &pane_id, &command]);
    assert!(
        ran.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(5)).unwrap_or_else(|err| {
        panic!("failed to read pane child pid: {err}");
    });
    assert!(process_exists(pid), "child process was not running");

    let removed = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "remove",
            "--workspace",
            &child_workspace_id,
            "--force",
            "--json",
        ],
    );
    assert_eq!(removed["result"]["type"], "worktree_removed");
    assert!(wait_for_pid_exit(pid, Duration::from_secs(3)));
    assert!(!checkout.exists());

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn worktree_open_existing_checkout_by_path_and_branch() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let repo = base.join("repo");
    let checkout = base.join("external-checkout");
    create_committed_repo(&repo);
    let branch = "worktree/cli-open-existing";
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            checkout.to_str().unwrap(),
            "HEAD",
        ],
    );

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let opened = run_cli_json_in_dir(
        &socket_path,
        &[
            "worktree",
            "open",
            "--cwd",
            "repo",
            "--path",
            "external-checkout",
            "--json",
        ],
        &base,
    );
    assert_eq!(opened["result"]["type"], "worktree_opened");
    assert_eq!(opened["result"]["already_open"], false);
    assert_eq!(opened["result"]["worktree"]["branch"], branch);
    assert_eq!(
        opened["result"]["workspace"]["worktree"]["is_linked_worktree"],
        true
    );
    let child_workspace_id = opened["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let workspaces = run_cli_json(&socket_path, &["workspace", "list"]);
    let workspace_list = workspaces["result"]["workspaces"].as_array().unwrap();
    let parent_workspace_id = workspace_list
        .iter()
        .find(|workspace| workspace["worktree"]["is_linked_worktree"].as_bool() == Some(false))
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .unwrap()
        .to_string();

    let listed = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "list",
            "--workspace",
            &parent_workspace_id,
            "--json",
        ],
    );
    let listed_entry = listed["result"]["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["branch"].as_str() == Some(branch))
        .unwrap();
    assert_eq!(
        listed_entry["open_workspace_id"].as_str(),
        Some(child_workspace_id.as_str())
    );

    let reopened = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "open",
            "--workspace",
            &parent_workspace_id,
            "--branch",
            branch,
            "--json",
        ],
    );
    assert_eq!(reopened["result"]["type"], "worktree_opened");
    assert_eq!(reopened["result"]["already_open"], true);
    assert_eq!(
        reopened["result"]["workspace"]["workspace_id"].as_str(),
        Some(child_workspace_id.as_str())
    );

    let removed = run_cli_json(
        &socket_path,
        &[
            "worktree",
            "remove",
            "--workspace",
            &child_workspace_id,
            "--force",
            "--json",
        ],
    );
    assert_eq!(removed["result"]["type"], "worktree_removed");

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn worktree_cli_rejects_local_argument_errors_before_socket_use() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("missing.sock");
    let cases: &[&[&str]] = &[
        &["worktree", "list", "--workspace", "1", "--cwd", "/tmp"],
        &["worktree", "create", "--workspace", "1", "--cwd", "/tmp"],
        &["worktree", "open", "--workspace", "1"],
        &[
            "worktree",
            "open",
            "--workspace",
            "1",
            "--path",
            "a",
            "--branch",
            "b",
        ],
        &[
            "worktree",
            "open",
            "--workspace",
            "1",
            "--cwd",
            "/tmp",
            "--branch",
            "b",
        ],
    ];

    for args in cases {
        let output = run_cli(&socket_path, args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "zynk {} should fail as local parse error; stdout={} stderr={}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    cleanup_test_base(&base);
}

#[test]
fn tab_management_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let workspace_id = created_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_tab_id = created_json["result"]["workspace"]["active_tab_id"]
        .as_str()
        .unwrap()
        .to_string();

    let created_tab = run_cli(
        &socket_path,
        &["tab", "create", "--workspace", &workspace_id],
    );
    assert!(created_tab.status.success());
    let created_tab_json: serde_json::Value = serde_json::from_slice(&created_tab.stdout).unwrap();
    let second_tab_id = created_tab_json["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second_tab_id, format!("{workspace_id}:t2"));

    let listed_tabs = run_cli(&socket_path, &["tab", "list", "--workspace", &workspace_id]);
    assert!(listed_tabs.status.success());
    let listed_tabs_json: serde_json::Value = serde_json::from_slice(&listed_tabs.stdout).unwrap();
    assert_eq!(
        listed_tabs_json["result"]["tabs"].as_array().unwrap().len(),
        2
    );

    let renamed_tab = run_cli(&socket_path, &["tab", "rename", &second_tab_id, "logs"]);
    assert!(renamed_tab.status.success());
    let renamed_tab_json: serde_json::Value = serde_json::from_slice(&renamed_tab.stdout).unwrap();
    assert_eq!(renamed_tab_json["result"]["tab"]["label"], "logs");

    let focused_tab = run_cli(&socket_path, &["tab", "focus", &first_tab_id]);
    assert!(focused_tab.status.success());
    let focused_tab_json: serde_json::Value = serde_json::from_slice(&focused_tab.stdout).unwrap();
    assert_eq!(focused_tab_json["result"]["tab"]["tab_id"], first_tab_id);

    let tab_get = run_cli(&socket_path, &["tab", "get", &second_tab_id]);
    assert!(tab_get.status.success());
    let tab_get_json: serde_json::Value = serde_json::from_slice(&tab_get.stdout).unwrap();
    assert_eq!(tab_get_json["result"]["tab"]["tab_id"], second_tab_id);

    let closed_tab = run_cli(&socket_path, &["tab", "close", &second_tab_id]);
    assert!(closed_tab.status.success());
    let closed_tab_json: serde_json::Value = serde_json::from_slice(&closed_tab.stdout).unwrap();
    assert_eq!(closed_tab_json["result"]["type"], "ok");

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn m839a_agent_start_cli_uses_existing_pane_and_preserves_child_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let base = unique_test_dir();
    fs::create_dir_all(base.join("bin")).unwrap();
    let shell = base.join("shell");
    fs::write(&shell, b"#!/bin/sh\nroot=${0%/*}\nexport HOME=\"$root\" PATH=\"$root/bin\" ZYNK_AGENT= ENV=/dev/null BASH_ENV=/dev/null INPUTRC=/dev/null PROMPT_COMMAND= PS1= PS2=\nexec /bin/bash --noprofile --norc --noediting -i\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    let agent = base.join("bin/codex");
    fs::write(&agent, b"#!/bin/sh\nexport ZYNK_AGENT=codex\nprintf '%s\\n' \"$@\" > \"$HOME/child-args\"\nprintf '\\033]2;m839-native-idle\\007'\nexec /bin/cat\n").unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
    let config_home = base.join("config");
    let runtime = base.join("runtime");
    let socket = runtime.join("zynk.sock");
    let config = toml::to_string(&serde_json::json!({"onboarding":false,"terminal":{"default_shell":shell,"shell_mode":"non_login"}})).unwrap();
    let server = spawn_zynk_with_config(
        &config_home,
        &runtime,
        &socket,
        Some(&base.join("bin")),
        &config,
    );
    wait_for_socket(&socket, Duration::from_secs(5));
    let created = m837_exchange(
        &socket,
        serde_json::json!({"id":"m839-create","method":"workspace.create","params":{"cwd":base,"focus":true}}),
    );
    let pane = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    let terminal = created["result"]["root_pane"]["terminal_id"]
        .as_str()
        .unwrap();
    let setup = m837_exchange(
        &socket,
        serde_json::json!({"id":"m839-shell","method":"pane.send_input","params":{"pane_id":pane,"text":"printf ready > \"$HOME/ready\"","keys":["Enter"]}}),
    );
    assert_eq!(setup["result"]["type"], "ok");
    support::wait_for_file(&base.join("ready"), Duration::from_secs(5));
    let output = m839_managed_cli_bounded(
        &base,
        &socket,
        &[
            "agent",
            "start",
            "main",
            "--kind",
            "codex",
            "--pane",
            pane,
            "--timeout",
            "8000",
            "--",
            "--session",
            "child-session",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(started["result"]["type"], "agent_started");
    assert_eq!(started["result"]["agent"]["name"], "main");
    assert_eq!(started["result"]["agent"]["terminal_id"], terminal);
    assert_eq!(started["result"]["agent"]["pane_id"], pane);
    assert_eq!(started["result"]["agent"]["interactive_ready"], true);
    assert!(started["result"]["agent"].get("agent_session").is_none());
    assert_eq!(
        started["result"]["argv"],
        serde_json::json!(["codex", "--session", "child-session"])
    );
    assert_eq!(
        fs::read(base.join("child-args")).unwrap(),
        b"--session\nchild-session\n"
    );
    let listed = m839_managed_cli_bounded(&base, &socket, &["agent", "list"]);
    assert_eq!(listed.status.code(), Some(0));
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["result"]["agents"][0]["terminal_id"], terminal);
    assert_eq!(listed["result"]["agents"][0]["name"], "main");
    let duplicate = m839_managed_cli_bounded(
        &base,
        &socket,
        &["agent", "start", "main", "--kind", "codex", "--pane", pane],
    );
    assert_eq!(duplicate.status.code(), Some(1));
    let duplicate: serde_json::Value = serde_json::from_slice(&duplicate.stderr).unwrap();
    assert_eq!(duplicate["error"]["code"], "agent_name_taken");
    assert!(duplicate["error"]["message"]
        .as_str()
        .unwrap()
        .contains(terminal));
    cleanup_spawned_zynk(server, base);
}

#[test]
fn agent_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let root_pane_id = created_json["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let terminal_id = created_json["result"]["root_pane"]["terminal_id"]
        .as_str()
        .unwrap()
        .to_string();

    let renamed = run_cli(&socket_path, &["agent", "rename", &root_pane_id, "worker"]);
    assert!(renamed.status.success());

    let listed = run_cli_json(&socket_path, &["agent", "list"]);
    assert_eq!(listed["result"]["type"], "agent_list");
    assert_eq!(listed["result"]["agents"][0]["terminal_id"], terminal_id);
    assert_eq!(listed["result"]["agents"][0]["name"], "worker");

    let fetched = run_cli_json(&socket_path, &["agent", "get", "worker"]);
    assert_eq!(fetched["result"]["agent"]["pane_id"], root_pane_id);

    let waited = run_cli(
        &socket_path,
        &["agent", "wait", "worker", "--timeout", "100"],
    );
    assert_eq!(waited.status.code(), Some(1));
    assert!(waited.stdout.is_empty());
    let waited: serde_json::Value = serde_json::from_slice(&waited.stderr).unwrap();
    assert_eq!(waited["error"]["code"], "agent_not_running");

    let read = run_cli_json(
        &socket_path,
        &["agent", "read", &terminal_id, "--source", "visible"],
    );
    assert_eq!(read["result"]["type"], "pane_read");

    let sent = run_cli(
        &socket_path,
        &["agent", "send", "worker", "echo cli-agent-ok\n"],
    );
    assert!(sent.status.success());

    let agent_renamed = run_cli_json(&socket_path, &["agent", "rename", "worker", "reviewer"]);
    assert_eq!(agent_renamed["result"]["agent"]["name"], "reviewer");

    let focused = run_cli_json(&socket_path, &["agent", "focus", "reviewer"]);
    assert_eq!(focused["result"]["agent"]["focused"], true);

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn pane_close_only_removes_the_target_tab_when_other_tabs_exist() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let workspace_id = created_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let created_tab = run_cli(
        &socket_path,
        &["tab", "create", "--workspace", &workspace_id],
    );
    assert!(created_tab.status.success());
    let created_tab_json: serde_json::Value = serde_json::from_slice(&created_tab.stdout).unwrap();
    let second_root_pane_id = created_tab_json["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let closed = run_cli(&socket_path, &["pane", "close", &second_root_pane_id]);
    assert!(closed.status.success());
    let closed_json: serde_json::Value = serde_json::from_slice(&closed.stdout).unwrap();
    assert_eq!(closed_json["result"]["type"], "ok");

    let workspaces = run_cli(&socket_path, &["workspace", "list"]);
    assert!(workspaces.status.success());
    let workspaces_json: serde_json::Value = serde_json::from_slice(&workspaces.stdout).unwrap();
    assert_eq!(
        workspaces_json["result"]["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        workspaces_json["result"]["workspaces"][0]["workspace_id"],
        workspace_id
    );

    let tabs = run_cli(&socket_path, &["tab", "list", "--workspace", &workspace_id]);
    assert!(tabs.status.success());
    let tabs_json: serde_json::Value = serde_json::from_slice(&tabs.stdout).unwrap();
    assert_eq!(tabs_json["result"]["tabs"].as_array().unwrap().len(), 1);

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn pane_close_removes_the_workspace_when_it_closes_the_last_pane() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let root_pane_id = created_json["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let closed = run_cli(&socket_path, &["pane", "close", &root_pane_id]);
    assert!(closed.status.success());
    let closed_json: serde_json::Value = serde_json::from_slice(&closed.stdout).unwrap();
    assert_eq!(closed_json["result"]["type"], "ok");

    let workspaces = run_cli(&socket_path, &["workspace", "list"]);
    assert!(workspaces.status.success());
    let workspaces_json: serde_json::Value = serde_json::from_slice(&workspaces.stdout).unwrap();
    assert!(workspaces_json["result"]["workspaces"]
        .as_array()
        .unwrap()
        .is_empty());

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn pane_run_read_and_wait_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let create = run_cli(
        &socket_path,
        &[
            "pane",
            "run",
            "1-1",
            "echo alpha && echo beta && printf 'ready\\n'",
        ],
    );
    assert!(create.status.success());

    let started = Instant::now();
    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "output",
            "1-1",
            "--match",
            "ready",
            "--source",
            "recent",
            "--lines",
            "40",
            "--timeout",
            "5000",
        ],
    );
    let elapsed = started.elapsed();
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "already-matching wait took {elapsed:?}"
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["result"]["type"], "output_matched");

    let read = run_cli(
        &socket_path,
        &["pane", "read", "1-1", "--source", "recent", "--lines", "40"],
    );
    assert!(read.status.success());
    let text = String::from_utf8(read.stdout).unwrap();
    assert!(text.contains("alpha"));
    assert!(text.contains("ready"));

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn wait_output_matches_recent_unwrapped_text() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let token = "WRAP_WAIT_TEST_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789";
    let script = base.join("emit-long-token.sh");
    std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{token}'\n")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }

    let run = run_cli(
        &socket_path,
        &["pane", "run", "1-1", &format!("sh {}", script.display())],
    );
    assert!(run.status.success());

    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "output",
            "1-1",
            "--match",
            token,
            "--source",
            "recent",
            "--lines",
            "80",
            "--timeout",
            "5000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {} stdout: {}",
        String::from_utf8_lossy(&waited.stderr),
        String::from_utf8_lossy(&waited.stdout)
    );

    let read = run_cli(
        &socket_path,
        &[
            "pane",
            "read",
            "1-1",
            "--source",
            "recent-unwrapped",
            "--lines",
            "80",
        ],
    );
    assert!(read.status.success());
    let text = String::from_utf8(read.stdout).unwrap();
    assert!(text.contains(token));

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn closing_pane_terminates_processes_inside_it() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let split = run_cli(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right"],
    );
    assert!(split.status.success());
    let split_json: serde_json::Value = serde_json::from_slice(&split.stdout).unwrap();
    let pane_id = split_json["result"]["pane"]["pane_id"].as_str().unwrap();

    let pid_file = base.join("pane-close.pid");
    let command = format!(
        "python3 -c 'import os,time,pathlib; pathlib.Path(r\"{}\").write_text(str(os.getpid())); time.sleep(1000)'",
        pid_file.display()
    );
    let ran = run_cli(&socket_path, &["pane", "run", pane_id, &command]);
    assert!(
        ran.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !pid_file.exists() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(pid_file.exists(), "pid file was not created");

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(3)).unwrap_or_else(|err| {
        panic!("failed to read pane child pid: {err}");
    });
    assert!(process_exists(pid), "child process was not running");

    let closed = run_cli(&socket_path, &["pane", "close", pane_id]);
    assert!(
        closed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "process {pid} survived pane close"
    );

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn closing_workspace_terminates_processes_inside_it() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let pid_file = base.join("workspace-close.pid");
    let command = format!(
        "python3 -c 'import os,time,pathlib; pathlib.Path(r\"{}\").write_text(str(os.getpid())); time.sleep(1000)'",
        pid_file.display()
    );
    let ran = run_cli(&socket_path, &["pane", "run", "1-1", &command]);
    assert!(
        ran.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !pid_file.exists() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(pid_file.exists(), "pid file was not created");

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(3)).unwrap_or_else(|err| {
        panic!("failed to read pane child pid: {err}");
    });
    assert!(process_exists(pid), "child process was not running");

    let closed = run_cli(&socket_path, &["workspace", "close", "1"]);
    assert!(
        closed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "process {pid} survived workspace close"
    );

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn workspace_ids_and_public_pane_ids_are_stable() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let ws1_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let ws1_id = ws1_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let split_12_json = run_cli_json(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right", "--no-focus"],
    );
    assert_eq!(
        split_12_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}:p2")
    );

    let split_13_json = run_cli_json(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "down", "--no-focus"],
    );
    assert_eq!(
        split_13_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}:p3")
    );

    let ws2_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", "/tmp", "--no-focus"],
    );
    let ws2_id = ws2_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ws2_id, ws1_id);

    let ws2_focus = run_cli(&socket_path, &["workspace", "focus", &ws2_id]);
    assert!(
        ws2_focus.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ws2_focus.stderr)
    );

    let ws2_split_json = run_cli_json(
        &socket_path,
        &["pane", "split", "2-1", "--direction", "right", "--no-focus"],
    );
    assert_eq!(
        ws2_split_json["result"]["pane"]["pane_id"],
        format!("{ws2_id}:p2")
    );

    let ws3_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", "/", "--no-focus"],
    );
    let ws3_id = ws3_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ws3_id, ws1_id);
    assert_ne!(ws3_id, ws2_id);

    let close_ws2 = run_cli(&socket_path, &["workspace", "close", &ws2_id]);
    assert!(
        close_ws2.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&close_ws2.stderr)
    );

    let workspaces_json = run_cli_json(&socket_path, &["workspace", "list"]);
    let ids: Vec<String> = workspaces_json["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ws| ws["workspace_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![ws1_id.clone(), ws3_id.clone()]);

    let new_ws_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", "/var/tmp", "--no-focus"],
    );
    let new_ws_id = new_ws_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_ws_id, ws1_id);
    assert_ne!(new_ws_id, ws2_id);
    assert_ne!(new_ws_id, ws3_id);

    let ws3_panes_json = run_cli_json(&socket_path, &["pane", "list", "--workspace", &ws3_id]);
    assert_eq!(
        ws3_panes_json["result"]["panes"][0]["pane_id"],
        format!("{ws3_id}:p1")
    );

    let close_middle = run_cli(&socket_path, &["pane", "close", &format!("{ws1_id}-2")]);
    assert!(
        close_middle.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&close_middle.stderr)
    );

    let ws1_panes_json = run_cli_json(&socket_path, &["pane", "list", "--workspace", &ws1_id]);
    let pane_ids: Vec<String> = ws1_panes_json["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| pane["pane_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        pane_ids,
        vec![format!("{ws1_id}:p1"), format!("{ws1_id}:p3")]
    );

    let closed_lookup = run_cli(&socket_path, &["pane", "get", &format!("{ws1_id}:p2")]);
    assert!(
        !closed_lookup.status.success(),
        "closed pane id should not retarget: {}",
        String::from_utf8_lossy(&closed_lookup.stdout)
    );

    let split_14_json = run_cli_json(
        &socket_path,
        &[
            "pane",
            "split",
            &format!("{ws1_id}:p1"),
            "--direction",
            "right",
            "--no-focus",
        ],
    );
    assert_eq!(
        split_14_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}:p4")
    );

    cleanup_spawned_zynk(zynk, base);
}

#[derive(Clone, Copy)]
enum SplitSelector {
    Current,
    Omitted,
    Positional,
    PaneOption,
}

#[derive(Clone, Copy)]
enum SplitCallerEnv {
    Pane,
    Missing,
    Blank,
    NonUnicode,
}

fn assert_cli_split_selection(selector: SplitSelector, caller_env: SplitCallerEnv) {
    use std::os::unix::ffi::OsStringExt;

    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let caller_workspace = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    )["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let focused_workspace = run_cli_json(
        &socket_path,
        &[
            "workspace",
            "create",
            "--cwd",
            base.to_str().unwrap(),
            "--focus",
        ],
    )["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(caller_workspace, focused_workspace);
    let caller_panes = run_cli_json(
        &socket_path,
        &["pane", "list", "--workspace", &caller_workspace],
    );
    let caller_pane = caller_panes["result"]["panes"][0]["pane_id"]
        .as_str()
        .unwrap();
    let before = send_request(
        &socket_path,
        r#"{"id":"before","method":"session.snapshot","params":{}}"#,
    );
    assert!(
        before.get("error").is_none(),
        "snapshot setup failed: {before}"
    );
    assert_eq!(before["result"]["type"], "session_snapshot");
    let before = &before["result"]["snapshot"];
    assert_eq!(before["focused_workspace_id"], focused_workspace);
    let focused_pane = before["focused_pane_id"].as_str().unwrap();
    assert_ne!(caller_pane, focused_pane);

    let mut args = vec!["pane", "split"];
    match selector {
        SplitSelector::Current => args.push("--current"),
        SplitSelector::Omitted => {}
        SplitSelector::Positional => args.push(focused_pane),
        SplitSelector::PaneOption => args.extend(["--pane", focused_pane]),
    }
    args.extend(["--direction", "right", "--no-focus"]);
    let mut command = Command::new(env!("CARGO_BIN_EXE_zynk"));
    command
        .args(&args)
        .env("ZYNK_SOCKET_PATH", &socket_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("ZYNK_SQLITE_HOME", runtime_dir.join("sqlite"))
        .env_remove("ZYNK_HOME")
        .env_remove("ZYNK_PANE_ID");
    match caller_env {
        SplitCallerEnv::Pane => {
            command.env("ZYNK_PANE_ID", caller_pane);
        }
        SplitCallerEnv::Missing => {}
        SplitCallerEnv::Blank => {
            command.env("ZYNK_PANE_ID", " \t ");
        }
        SplitCallerEnv::NonUnicode => {
            command.env("ZYNK_PANE_ID", std::ffi::OsString::from_vec(vec![0xff]));
        }
    }
    let result = parse_cli_json_output(&args, command.output().unwrap());
    let expected_workspace = if matches!(selector, SplitSelector::Current)
        && matches!(caller_env, SplitCallerEnv::Pane)
    {
        &caller_workspace
    } else {
        &focused_workspace
    };
    assert_eq!(
        result["result"]["pane"]["workspace_id"], *expected_workspace,
        "split must select caller only for explicit --current with a usable caller env"
    );

    let after = send_request(
        &socket_path,
        r#"{"id":"after","method":"session.snapshot","params":{}}"#,
    );
    assert!(
        after.get("error").is_none(),
        "snapshot verification failed: {after}"
    );
    assert_eq!(after["result"]["type"], "session_snapshot");
    let after = &after["result"]["snapshot"];
    for field in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(after[field], before[field], "split changed global {field}");
    }
    for workspace in [&caller_workspace, &focused_workspace] {
        let pane_ids = |snapshot: &serde_json::Value| -> Vec<String> {
            snapshot["panes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|pane| pane["workspace_id"] == *workspace)
                .map(|pane| pane["pane_id"].as_str().unwrap().to_string())
                .collect()
        };
        let prior_ids = pane_ids(before);
        let after_ids = pane_ids(after);
        assert_eq!(prior_ids.len(), 1, "fixture needs one pane per workspace");
        if workspace == expected_workspace {
            assert_eq!(after_ids.len(), prior_ids.len() + 1);
            assert!(prior_ids.iter().all(|id| after_ids.contains(id)));
        } else {
            assert_eq!(
                after_ids, prior_ids,
                "unselected workspace must be unchanged"
            );
        }
    }
    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn pane_split_current_uses_caller_without_moving_global_focus() {
    assert_cli_split_selection(SplitSelector::Current, SplitCallerEnv::Pane);
}

#[test]
fn pane_split_omitted_target_keeps_focus_fallback_with_caller_env() {
    assert_cli_split_selection(SplitSelector::Omitted, SplitCallerEnv::Pane);
}

#[test]
fn pane_split_explicit_targets_are_not_overridden_by_caller_env() {
    for selector in [SplitSelector::Positional, SplitSelector::PaneOption] {
        assert_cli_split_selection(selector, SplitCallerEnv::Pane);
    }
}

#[test]
fn pane_split_current_ignores_missing_blank_and_nonunicode_caller_env() {
    for env in [
        SplitCallerEnv::Missing,
        SplitCallerEnv::Blank,
        SplitCallerEnv::NonUnicode,
    ] {
        assert_cli_split_selection(SplitSelector::Current, env);
    }
}

#[test]
fn pane_shell_gets_zynk_socket_and_pane_env() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_env_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let env_capture = base.join("pane-env.txt");
    let ran = run_cli(
        &socket_path,
        &[
            "pane",
            "run",
            "1-1",
            &format!(
                "printf '%s\\n%s\\n' \"$ZYNK_SOCKET_PATH\" \"$ZYNK_PANE_ID\" > {}",
                env_capture.display()
            ),
        ],
    );
    assert!(ran.status.success());

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut text = String::new();
    while Instant::now() < deadline {
        if env_capture.exists() {
            text = fs::read_to_string(&env_capture).unwrap();
            if text.contains(&socket_path.display().to_string()) && text.contains(&pane_id) {
                break;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(env_capture.exists(), "env capture file was not created");
    assert!(
        text.contains(&socket_path.display().to_string()),
        "env file was: {text:?}"
    );
    assert!(text.contains(&pane_id), "env file was: {text:?}");

    cleanup_spawned_zynk(zynk, base);
}

fn run_snapshot_cli_bounded(base: &Path, socket: &Path, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(args)
        .env_clear()
        .env("HOME", base.join("cli-home"))
        .env("XDG_CONFIG_HOME", base.join("cli-config"))
        .env("XDG_DATA_HOME", base.join("cli-data"))
        .env("XDG_CACHE_HOME", base.join("cli-cache"))
        .env("XDG_RUNTIME_DIR", base.join("cli-runtime"))
        .env("ZYNK_HOME", base.join("cli-db"))
        .env("ZYNK_SQLITE_HOME", base.join("cli-sqlite"))
        .env("ZYNK_SOCKET_PATH", socket)
        .env(
            "ZYNK_CLIENT_SOCKET_PATH",
            base.join("cli-runtime/client.sock"),
        )
        .current_dir(base)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes while waiting so a complete snapshot can exceed pipe capacity.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    thread::scope(|scope| {
        let read = |mut pipe: Box<dyn Read + Send>| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        };
        let stdout = scope.spawn(move || read(Box::new(stdout)));
        let stderr = scope.spawn(move || read(Box::new(stderr)));
        let finished = wait_until(Duration::from_secs(3), Duration::from_millis(5), || {
            child.try_wait().unwrap().is_some()
        });
        if !finished {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        let output = std::process::Output {
            status,
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        };
        assert!(finished, "snapshot CLI exceeded 3s; child reaped");
        output
    })
}

#[test]
fn m814_cli_snapshot_matches_live_read_only_projection() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket = runtime_dir.join("zynk.sock");
    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket);
    wait_for_socket(&socket, Duration::from_secs(5));
    let created = send_request(
        &socket,
        &serde_json::json!({
            "id": "create_snapshot_target", "method": "workspace.create",
            "params": {"cwd": base, "focus": true}
        })
        .to_string(),
    );
    let workspace = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap();
    let pane = format!("{workspace}:p1");
    let listed = send_request(
        &socket,
        r#"{"id":"panes","method":"pane.list","params":{}}"#,
    );
    assert!(listed["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["pane_id"] == pane));
    let ping = send_request(&socket, r#"{"id":"ping","method":"ping","params":{}}"#);
    let db = config_home.join("sqlite/zynk.db");
    let before = delivery_events_count(&db);
    let output = run_snapshot_cli_bounded(&base, &socket, &["api", "snapshot"]);
    assert!(
        output.status.success(),
        "api snapshot must exist: {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["id"], "cli:api:snapshot");
    assert_eq!(response["result"]["type"], "session_snapshot");
    let snapshot = &response["result"]["snapshot"];
    assert_eq!(snapshot["panes"], listed["result"]["panes"]);
    assert_eq!(snapshot["focused_pane_id"], pane);
    assert_eq!(snapshot["protocol"], ping["result"]["protocol"]);
    assert_eq!(snapshot["version"], ping["result"]["version"]);
    assert_eq!(snapshot["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(delivery_events_count(&db), before);
    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn m821_live_scroll_get_list_snapshot_agree_without_delivery_events() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket = runtime_dir.join("zynk.sock");
    // No shell prompt or startup output can change history between observations.
    let zynk = spawn_zynk_with_config(
        &config_home,
        &runtime_dir,
        &socket,
        None,
        "[terminal]\ndefault_shell = \"/bin/cat\"\nshell_mode = \"non_login\"\n",
    );
    wait_for_socket(&socket, Duration::from_secs(5));
    let created = send_request(&socket, &serde_json::json!({
        "id": "scroll-create", "method": "workspace.create", "params": {"cwd": base, "focus": true}
    }).to_string());
    let pane_id = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    let db = config_home.join("sqlite/zynk.db");
    let before = delivery_events_count(&db);
    let get = send_request(
        &socket,
        &serde_json::json!({
            "id": "scroll-get", "method": "pane.get", "params": {"pane_id": pane_id}
        })
        .to_string(),
    );
    let scroll = &get["result"]["pane"]["scroll"];
    assert_eq!(scroll["offset_from_bottom"], 0);
    assert_eq!(scroll["max_offset_from_bottom"], 0);
    assert!(scroll["viewport_rows"].as_u64().unwrap() > 0);
    let listed = send_request(
        &socket,
        r#"{"id":"scroll-list","method":"pane.list","params":{}}"#,
    );
    let listed_pane = listed["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pane| pane["pane_id"] == pane_id)
        .unwrap();
    assert_eq!(&listed_pane["scroll"], scroll);
    let snapshot = send_request(
        &socket,
        r#"{"id":"scroll-snapshot","method":"session.snapshot","params":{}}"#,
    );
    let snapshot_pane = snapshot["result"]["snapshot"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pane| pane["pane_id"] == pane_id)
        .unwrap();
    assert_eq!(&snapshot_pane["scroll"], scroll);
    assert_eq!(snapshot["result"]["snapshot"]["focused_pane_id"], pane_id);
    assert_eq!(delivery_events_count(&db), before);
    cleanup_spawned_zynk(zynk, base);
}

struct SnapshotCliFixture {
    base: PathBuf,
}

impl SnapshotCliFixture {
    fn new() -> Self {
        let base = unique_test_dir();
        fs::create_dir_all(&base).unwrap();
        Self { base }
    }

    fn assert_no_runtime_created(&self) {
        for name in ["home", "config", "data", "cache", "runtime", "db", "sqlite"] {
            assert!(
                !self.base.join(format!("cli-{name}")).exists(),
                "CLI created {name}"
            );
        }
    }
}

impl Drop for SnapshotCliFixture {
    fn drop(&mut self) {
        cleanup_test_base(&self.base);
    }
}

fn mock_snapshot_cli(
    args: &[&str],
    response: serde_json::Value,
) -> (Option<serde_json::Value>, std::process::Output) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let fixture = SnapshotCliFixture::new();
    let socket = fixture.base.join("snapshot.sock");
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
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            if done.load(Ordering::Acquire)
                                || started.elapsed() >= Duration::from_secs(3)
                            {
                                assert_eq!(accepted_connections, 0);
                                return None;
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(err) => panic!("snapshot mock accept: {err}"),
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
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                if request["method"] == "ping" {
                    assert_eq!(accepted_connections, 1);
                    m835_reply_compatible_ping(&mut stream, &request);
                    continue;
                }
                assert_eq!(accepted_connections, 2);
                writeln!(stream, "{response}").unwrap();
                stream.flush().unwrap();
                return Some(request);
            }
        });
        let output = run_snapshot_cli_bounded(&fixture.base, &socket, args);
        done.store(true, Ordering::Release);
        (server.join().unwrap(), output)
    });
    fixture.assert_no_runtime_created();
    result
}

#[test]
fn m814_cli_snapshot_sends_exact_request_and_preserves_complete_response() {
    let response = serde_json::json!({
        "id": "cli:api:snapshot",
        "future_envelope": {"marker": "unchanged"},
        "result": {"type": "session_snapshot", "snapshot": {
            "panes": [{"pane_id": "observed:p8", "agent_session": {"value": "observation", "source": "hook"}}],
            "agents": [{"pane_id": "observed:p8", "agent": "pi"}],
            "future_payload": "x".repeat(128 * 1024)
        }}
    });
    let (request, output) = mock_snapshot_cli(&["api", "snapshot"], response.clone());
    assert_eq!(
        request.unwrap(),
        serde_json::json!({
            "id": "cli:api:snapshot", "method": "session.snapshot", "params": {}
        })
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        response
    );
    assert!(output.stdout.ends_with(b"\n"));
}

#[test]
fn m828b_pane_token_cli_preserves_legacy_request_and_output() {
    let response = serde_json::json!({"id": "cli:request", "result": {"type": "ok"}, "future_envelope": "kept silent"});
    let (request, output) = mock_snapshot_cli(
        &[
            "pane",
            "report-metadata",
            "w7:p2",
            "--source",
            " user:build ",
            "--agent",
            "claude",
            "--applies-to-source",
            " legacy/source ",
            "--title",
            "Task",
            "--display-agent",
            "Builder",
            "--state-label",
            "working=Building",
            "--seq",
            "0",
            "--ttl-ms",
            "500",
            "--token",
            "build=old",
            "--clear-token",
            "build",
            "--token",
            "build=ok=x",
            "--token",
            "old=x",
            "--clear-token",
            "old",
        ],
        response.clone(),
    );
    assert_eq!(
        request,
        Some(
            serde_json::json!({"id": "cli:request", "method": "pane.report_metadata", "params": {
                "pane_id": "w7:p2", "source": "user:build", "agent": "claude", "applies_to_source": " legacy/source ",
                "title": "Task", "display_agent": "Builder", "state_labels": {"working": "Building"},
                "clear_title": false, "clear_display_agent": false, "clear_state_labels": false,
                "seq": 0, "ttl_ms": 500, "tokens": {"build": "ok=x", "old": null}
            }})
        ),
        "status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let (request, output) = mock_snapshot_cli(
        &[
            "pane",
            "report-metadata",
            "w7:p2",
            "--source",
            "user:build",
            "--clear-token",
            "build",
            "--clear-title",
            "--clear-display-agent",
            "--clear-state-labels",
        ],
        response,
    );
    assert_eq!(
        request,
        Some(
            serde_json::json!({"id": "cli:request", "method": "pane.report_metadata", "params": {
                "pane_id": "w7:p2", "source": "user:build", "tokens": {"build": null},
                "clear_title": true, "clear_display_agent": true, "clear_state_labels": true
            }})
        )
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn m828b_pane_token_cli_invalid_args_and_help_do_not_connect() {
    for args in [
        vec!["pane", "report-metadata", "--help"],
        vec!["pane", "--help"],
    ] {
        let (request, output) = mock_snapshot_cli(&args, serde_json::json!({}));
        assert!(request.is_none(), "help connected: {args:?}");
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty());
        let help = String::from_utf8(output.stderr).unwrap();
        assert!(help.contains("pane report-metadata"), "{help}");
        for flag in ["--token", "--clear-token", "--source", "--seq", "--ttl-ms"] {
            assert!(help.contains(flag), "missing {flag}: {help}");
        }
    }
    for (tail, exit) in [
        (vec!["--token"], 2),
        (vec!["--clear-token"], 2),
        (vec!["--token", "no-equals"], 2),
        (vec!["--token", "=missing-name"], 2),
        (vec!["--token", "x=y", "--seq", "bad"], 1),
        (vec!["--token", "x=y", "--ttl-ms", "bad"], 1),
        (vec!["--token", "x=y", "--seq"], 2),
        (vec!["--token", "x=y", "--ttl-ms"], 2),
        (
            vec!["--token", "x=y", "--title", "text", "--clear-title"],
            2,
        ),
        (
            vec![
                "--token",
                "x=y",
                "--display-agent",
                "text",
                "--clear-display-agent",
            ],
            2,
        ),
        (
            vec![
                "--token",
                "x=y",
                "--custom-status",
                "text",
                "--clear-custom-status",
            ],
            2,
        ),
        (
            vec![
                "--token",
                "x=y",
                "--state-label",
                "working=text",
                "--clear-state-labels",
            ],
            2,
        ),
        (vec!["--token", "x=y", "--applies-to-source", " "], 2),
        (vec!["--token", "x=y", "--unknown"], 2),
    ] {
        let mut args = vec!["pane", "report-metadata", "w7:p2", "--source", "user:build"];
        args.extend(tail);
        let (request, output) = mock_snapshot_cli(&args, serde_json::json!({}));
        assert!(request.is_none(), "invalid args connected: {args:?}");
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn m828b_pane_token_cli_preserves_api_errors_and_legacy_limits() {
    let response = serde_json::json!({"id": "cli:request", "error": {"code": "invalid_metadata_ttl", "message": "server policy refusal"}, "context": {"kept": true}});
    for tokens in [false, true] {
        for ttl in ["0", "86400001"] {
            let mut args = vec![
                "pane",
                "report-metadata",
                "w7:p2",
                "--source",
                " legacy/source ",
                "--title",
                "legacy",
                "--ttl-ms",
                ttl,
            ];
            if tokens {
                args.extend(["--token", "build=x"]);
            }
            let (request, output) = mock_snapshot_cli(&args, response.clone());
            let mut expected = serde_json::json!({"id": "cli:request", "method": "pane.report_metadata", "params": {
                "pane_id": "w7:p2", "source": "legacy/source", "title": "legacy", "ttl_ms": ttl.parse::<u64>().unwrap(),
                "clear_title": false, "clear_display_agent": false, "clear_state_labels": false
            }});
            if tokens {
                expected["params"]["tokens"] = serde_json::json!({"build": "x"});
            }
            assert_eq!(
                request,
                Some(expected),
                "tokens={tokens}, ttl={ttl}, stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                format!("{response}\n")
            );
        }
    }
}

#[test]
fn m828a_workspace_metadata_cli_preserves_request_and_success_json() {
    let response = serde_json::json!({
        "id": "cli:workspace:report-metadata", "result": {"type": "ok"},
        "future_envelope": {"retained": true}
    });
    let (request, output) = mock_snapshot_cli(
        &[
            "workspace",
            "report-metadata",
            "w7",
            "--source",
            "user:build",
            "--seq",
            "0",
            "--ttl-ms",
            "500",
            "--token",
            "build=old",
            "--clear-token",
            "build",
            "--token",
            "build=ok=x",
            "--token",
            "old=x",
            "--clear-token",
            "old",
        ],
        response.clone(),
    );
    assert_eq!(
        request,
        Some(serde_json::json!({
            "id": "cli:workspace:report-metadata", "method": "workspace.report_metadata",
            "params": {"workspace_id": "w7", "source": "user:build", "seq": 0, "ttl_ms": 500,
                "tokens": {"build": "ok=x", "old": null}}
        })),
        "status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        response
    );
}

#[test]
fn m828a_workspace_metadata_cli_preserves_structured_error() {
    let response = serde_json::json!({
        "id": "cli:workspace:report-metadata",
        "error": {"code": "metadata_token_limit", "message": "resource is full"},
        "context": {"retained": "error envelope"}
    });
    let (request, output) = mock_snapshot_cli(
        &[
            "workspace",
            "report-metadata",
            "w7",
            "--source",
            "user:build",
            "--token",
            "build=x",
        ],
        response.clone(),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        request,
        Some(serde_json::json!({
            "id": "cli:workspace:report-metadata", "method": "workspace.report_metadata",
            "params": {"workspace_id": "w7", "source": "user:build", "tokens": {"build": "x"}}
        }))
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap(),
        response
    );
}

#[test]
fn m828a_workspace_metadata_cli_help_and_invalid_args_do_not_connect() {
    for args in [
        vec!["workspace", "report-metadata", "--help"],
        vec!["workspace", "--help"],
    ] {
        let (request, output) = mock_snapshot_cli(&args, serde_json::json!({}));
        assert!(request.is_none(), "help connected: {args:?}");
        assert_eq!(output.status.code(), Some(0));
        let help = String::from_utf8(output.stderr).unwrap();
        assert!(
            help.contains("workspace report-metadata"),
            "missing workspace metadata help: {help}"
        );
        for flag in ["--source", "--token", "--clear-token", "--seq", "--ttl-ms"] {
            assert!(help.contains(flag), "{flag}: {help}");
        }
    }
    for (tail, exit) in [
        (vec![], 2),
        (vec!["w7"], 2),
        (vec!["w7", "--source"], 2),
        (vec!["w7", "--source", " ", "--token", "x=y"], 2),
        (vec!["w7", "--token", "x=y"], 2),
        (vec!["w7", "--source", "user:build"], 2),
        (vec!["w7", "--source", "user:build", "--token"], 2),
        (
            vec!["w7", "--source", "user:build", "--token", "missing-equals"],
            2,
        ),
        (
            vec!["w7", "--source", "user:build", "--token", "=empty-name"],
            2,
        ),
        (vec!["w7", "--source", "user:build", "--clear-token"], 2),
        (
            vec![
                "w7",
                "--source",
                "user:build",
                "--token",
                "x=y",
                "--seq",
                "bad",
            ],
            1,
        ),
        (
            vec![
                "w7",
                "--source",
                "user:build",
                "--token",
                "x=y",
                "--ttl-ms",
                "bad",
            ],
            1,
        ),
        (
            vec![
                "w7",
                "--source",
                "user:build",
                "--token",
                "x=y",
                "--unknown",
            ],
            2,
        ),
    ] {
        let mut args = vec!["workspace", "report-metadata"];
        args.extend(tail);
        let (request, output) = mock_snapshot_cli(&args, serde_json::json!({}));
        assert!(request.is_none(), "invalid args connected: {args:?}");
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn m814_cli_snapshot_preserves_server_error_and_exit_status() {
    let response = serde_json::json!({
        "id": "cli:api:snapshot", "future_envelope": "retained",
        "error": {"code": "server_unavailable", "message": "server stopping", "data": {"retry": false}}
    });
    let (request, output) = mock_snapshot_cli(&["api", "snapshot"], response.clone());
    assert!(request.is_some());
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap(),
        response
    );
}

#[test]
fn m814_cli_snapshot_refuses_arguments_before_connecting() {
    for args in [
        vec!["api", "snapshot", "extra"],
        vec!["api", "snapshot", "--json"],
        vec!["api", "snapshot", "--help"],
        vec!["api", "snapshot", "--output", "snapshot.json"],
    ] {
        let (request, output) = mock_snapshot_cli(
            &args,
            serde_json::json!({
                "id": "cli:api:snapshot", "result": {"type": "ok"}
            }),
        );
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(request, None, "{args:?} must not connect");
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "usage: zynk api snapshot\n"
        );
    }
}

fn run_event_wait_cli_bounded(socket: &Path, pane: &str, timeout: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args([
            "wait",
            "agent-status",
            pane,
            "--status",
            "blocked",
            "--timeout",
            timeout,
        ])
        .env_clear()
        .env("ZYNK_SOCKET_PATH", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let finished = wait_until(Duration::from_secs(3), Duration::from_millis(5), || {
        child.try_wait().unwrap().is_some()
    });
    if !finished {
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        finished,
        "events.wait CLI did not finish in 3s; child reaped"
    );
    output
}

fn mock_event_wait_cli(response: serde_json::Value) -> (serde_json::Value, std::process::Output) {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket = base.join("wait.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let started = Instant::now();
        let mut accepted_connections = 0;
        loop {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        accepted_connections += 1;
                        break stream;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            started.elapsed() < Duration::from_secs(3),
                            "CLI did not connect"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => panic!("mock accept: {err}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            if request["method"] == "ping" {
                assert_eq!(accepted_connections, 1);
                m835_reply_compatible_ping(&mut stream, &request);
                continue;
            }
            assert_eq!(accepted_connections, 2);
            let mut response = response;
            response["id"] = request["id"].clone();
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
            return request;
        }
    });
    let output = run_event_wait_cli_bounded(&socket, "caller:p7", "4321");
    let request = server.join().unwrap();
    cleanup_test_base(&base);
    (request, output)
}

#[test]
fn m811_cli_wait_status_sends_event_wait_and_preserves_subscription_stdout() {
    let data = serde_json::json!({
        "pane_id": "caller:p7", "workspace_id": "caller", "agent_status": "blocked",
        "agent": "pi", "title": "Question", "display_agent": "Reviewer",
        "state_labels": {"blocked": "Needs input"}
    });
    let mut wire_data = data.clone();
    wire_data["type"] = "pane_agent_status_changed".into();
    let (request, output) = mock_event_wait_cli(serde_json::json!({
        "result": {"type": "wait_matched", "event": {
            "event": "pane_agent_status_changed", "data": wire_data
        }}
    }));
    assert_eq!(
        request,
        serde_json::json!({
            "id": "cli:wait:agent-status", "method": "events.wait", "params": {
                "match_event": {"event": "pane_agent_status_changed", "pane_id": "caller:p7", "agent_status": "blocked"},
                "timeout_ms": 4321
            }
        })
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        printed,
        serde_json::json!({"event": "pane.agent_status_changed", "data": data})
    );
}

#[test]
fn m811_cli_wait_preserves_error_body_and_refuses_unexpected_result() {
    let error = serde_json::json!({"code": "not_found", "message": "pane does not exist"});
    let (_, output) = mock_event_wait_cli(serde_json::json!({"error": error}));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap(),
        serde_json::json!({"id": "cli:wait:agent-status", "error": error})
    );
    let (_, output) = mock_event_wait_cli(serde_json::json!({"result": {"type": "ok"}}));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected wait response result"));
}

#[test]
fn m811_cli_wait_real_server_timeout_is_read_only() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket = runtime_dir.join("zynk.sock");
    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket);
    wait_for_socket(&socket, Duration::from_secs(5));
    let created = send_request(&socket, &serde_json::json!({
        "id": "create_wait_target", "method": "workspace.create", "params": {"cwd": base, "focus": true}
    }).to_string());
    let workspace = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap();
    let pane = format!("{workspace}:p1");
    let observed = send_request(
        &socket,
        &serde_json::json!({
            "id": "observe_wait_target", "method": "pane.get", "params": {"pane_id": pane}
        })
        .to_string(),
    );
    assert_eq!(observed["result"]["pane"]["agent_status"], "unknown");
    let db = config_home.join("sqlite/zynk.db");
    let before = delivery_events_count(&db);
    let output = run_event_wait_cli_bounded(&socket, &pane, "30");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "timed out waiting for agent status change\n"
    );
    assert_eq!(delivery_events_count(&db), before);
    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn wait_agent_status_exits_when_idle_status_matches() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(
        &fake_pi,
        "#!/bin/sh\nprintf 'starting\\n'\nsleep 4\nprintf 'Working...\\n'\nsleep 1\nprintf '\\033[2J\\033[Hdone\\n'\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let zynk = spawn_zynk_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );

    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_2","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert!(created["result"]["workspace"]["workspace_id"].is_string());

    let start_pi = run_cli(&socket_path, &["pane", "run", "1-1", "pi"]);
    assert!(start_pi.status.success());

    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "agent-status",
            "1-1",
            "--status",
            "idle",
            "--timeout",
            "10000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["event"], "pane.agent_status_changed");
    assert_eq!(waited_json["data"]["agent_status"], "idle");
    assert_eq!(waited_json["data"]["agent"], "pi");

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn wait_agent_status_exits_immediately_when_status_already_matches() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");

    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_immediate_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let pane_id = format!("{workspace_id}:p1");

    let reported = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_immediate_2","method":"pane.report_agent","params":{{"pane_id":"{}","source":"zynk:pi","agent":"pi","state":"idle"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(reported["result"]["type"], "ok");

    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "agent-status",
            "1-1",
            "--status",
            "idle",
            "--timeout",
            "1000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["event"], "pane.agent_status_changed");
    assert_eq!(waited_json["data"]["agent_status"], "idle");
    assert_eq!(waited_json["data"]["agent"], "pi");

    cleanup_spawned_zynk(zynk, base);
}

#[test]
fn wait_agent_status_exits_when_done_status_matches() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("zynk.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(
        &fake_pi,
        "#!/bin/sh\nprintf 'starting\\n'\nsleep 4\nprintf 'Working...\\n'\nsleep 1\nprintf '\\033[2J\\033[Hdone\\n'\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let zynk = spawn_zynk_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );

    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_status_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let tab_created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_status_2","method":"tab.create","params":{{"workspace_id":"{}","focus":true}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(tab_created["result"]["type"], "tab_created");

    let start_pi = run_cli(&socket_path, &["pane", "run", "1-1", "pi"]);
    assert!(start_pi.status.success());

    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "agent-status",
            "1-1",
            "--status",
            "done",
            "--timeout",
            "10000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["event"], "pane.agent_status_changed");
    assert_eq!(waited_json["data"]["agent_status"], "done");
    assert_eq!(waited_json["data"]["agent"], "pi");

    cleanup_spawned_zynk(zynk, base);
}

/// Run `zynk <args>` capturing stdout+stderr. Used for help-string assertions;
/// the socket is pointed at a nonexistent path so a help path never touches a
/// live runtime (every asserted command returns before dispatch).
fn run_zynk_help(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(args)
        .env(
            "ZYNK_SOCKET_PATH",
            "/tmp/zynk-clihelp-test-nonexistent.sock",
        )
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

#[test]
fn read_help_lists_detection_source() {
    // The `detection` read source is accepted by the parser; help must advertise it.
    for cmd in [
        ["pane", "read", "--help"],
        ["agent", "read", "--help"],
        ["wait", "output", "--help"],
    ] {
        let help = run_zynk_help(&cmd);
        assert!(
            help.contains("detection"),
            "`zynk {}` help must list the detection source:\n{help}",
            cmd.join(" ")
        );
    }
}

#[test]
fn root_help_common_rows_show_type_for_send_and_reply() {
    let help = run_zynk_help(&["--help"]);
    assert!(
        help.contains("zynk send <target> [--type T] [--trace <id|inherit>] -- <text>"),
        "root help send common row must show --type:\n{help}"
    );
    assert!(
        help.contains("zynk reply <target> [--type T] [--trace <id|inherit>] -- <text>"),
        "root help reply common row must show --type:\n{help}"
    );
}

fn run_zynk_help_status(args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(args)
        .env(
            "ZYNK_SOCKET_PATH",
            "/tmp/zynk-clihelp-test-nonexistent.sock",
        )
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), text)
}

#[test]
fn positional_read_get_help_flag_exits_zero_with_command_usage() {
    // `<leaf> <positional> --help` is friendly command help (these leaves take no
    // body, so a help flag in option position is unambiguous). Returns before any
    // socket dispatch.
    for (cmd, needle) in [
        (
            vec!["pane", "read", "w1:p1", "--help"],
            "usage: zynk pane read",
        ),
        (
            vec!["pane", "get", "w1:p1", "--help"],
            "usage: zynk pane get",
        ),
        (
            vec!["agent", "read", "tgt", "--help"],
            "usage: zynk agent read",
        ),
        (vec!["agent", "get", "tgt", "-h"], "usage: zynk agent get"),
    ] {
        let (code, out) = run_zynk_help_status(&cmd);
        assert_eq!(code, 0, "`zynk {}` should exit 0:\n{out}", cmd.join(" "));
        assert!(
            out.contains(needle),
            "`zynk {}` should show command usage:\n{out}",
            cmd.join(" ")
        );
    }
}

#[test]
fn read_help_flag_as_option_value_is_not_hijacked() {
    // `--help` consumed as the value of a value-taking flag must NOT be treated as
    // help — it is a bad value and exits non-zero, proving the help handling is
    // position-aware, not a blanket arg scan.
    let (code, _out) = run_zynk_help_status(&["pane", "read", "w1:p1", "--lines", "--help"]);
    assert_ne!(
        code, 0,
        "`pane read w1:p1 --lines --help` must not be treated as help"
    );
}

fn m835_scripted_cli(
    args: &[&str],
    responses: Vec<serde_json::Value>,
    replace_after_first_accept: bool,
) -> (Vec<serde_json::Value>, std::process::Output) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = SnapshotCliFixture::new();
    let socket = fixture.base.join("compat.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let mut listener = listener;
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(Ordering::Acquire) { break; }
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("compat accept: {e}"),
                };
                stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                if replace_after_first_accept && requests.is_empty() {
                    drop(listener);
                    fs::remove_file(&socket).unwrap();
                    listener = UnixListener::bind(&socket).unwrap();
                    listener.set_nonblocking(true).unwrap();
                }
                let mut response = responses.get(requests.len()).cloned().unwrap_or_else(|| serde_json::json!({"error":{"code":"unexpected_request", "message":"unexpected additional connection"}}));
                requests.push(request.clone());
                if let Some(raw) = response.as_str() { writeln!(stream, "{raw}").unwrap(); }
                else if !response.is_null() {
                    response["id"] = request["id"].clone();
                    writeln!(stream, "{response}").unwrap();
                }
            }
            requests
        });
        let output = run_snapshot_cli_bounded(&fixture.base, &socket, args);
        done.store(true, Ordering::Release);
        (worker.join().unwrap(), output)
    })
}

#[test]
fn m835_ordinary_mismatch_has_one_json_error_and_no_operational_request() {
    for args in [
        vec!["pane", "list"],
        vec![
            "wait",
            "agent-status",
            "w1:p1",
            "--status",
            "idle",
            "--timeout",
            "100",
        ],
    ] {
        for protocol in [18, 20] {
            let (requests, output) = m835_scripted_cli(
                &args,
                vec![
                    serde_json::json!({"result":{"type":"pong", "version":"fixture", "protocol":protocol}}),
                ],
                false,
            );
            assert_eq!(
                requests
                    .iter()
                    .map(|r| r["method"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["ping"],
                "{args:?}"
            );
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty());
            let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "protocol_mismatch");
            assert_eq!(
                error["id"],
                if args[0] == "pane" {
                    "cli:pane:list"
                } else {
                    "cli:wait:agent-status"
                }
            );
        }
    }
}

#[test]
fn m835_matching_protocol_ignores_package_version_and_replacement_is_not_rechecked() {
    let (requests, output) = m835_scripted_cli(
        &["pane", "list"],
        vec![
            serde_json::json!({"result":{"type":"pong", "version":"999.999.999", "protocol":19}}),
            serde_json::json!({"result":{"type":"pane_list", "panes":[]}, "replacement":"observed"}),
        ],
        true,
    );
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["ping", "pane.list"]
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["replacement"], "observed");
    let (requests, output) = m835_scripted_cli(
        &["pane", "list"],
        vec![
            serde_json::json!({"result":{"type":"pong", "version":"fixture", "protocol":19}}),
            serde_json::Value::Null,
        ],
        true,
    );
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["ping", "pane.list"]
    );
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("protocol_mismatch"));
}

#[test]
fn m835_invalid_status_never_guesses_compatibility() {
    for response in [
        serde_json::json!({"result":{"type":"pong", "version":"fixture"}}),
        serde_json::json!({"result":{"type":"pong", "version":"fixture", "protocol":"19"}}),
        serde_json::json!("{not-json"),
        serde_json::Value::Null,
    ] {
        let (requests, output) = m835_scripted_cli(&["pane", "list"], vec![response], false);
        assert_eq!(
            requests
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["ping"]
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn m839c_poll_rechecks_after_resolution_without_opening_mismatched_request() {
    let (requests, output) = m835_scripted_cli(
        &["agent", "wait", "worker", "--timeout", "100"],
        vec![
            serde_json::json!({"result":{"type":"pong", "version":"fixture", "protocol":19}}),
            m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7)),
            serde_json::json!({"result":{"type":"pong", "version":"replacement", "protocol":20}}),
        ],
        false,
    );
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["ping", "agent.get", "ping"]
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["id"], "cli:agent:wait");
    assert_eq!(error["error"]["code"], "protocol_mismatch");
}

#[test]
fn m835_status_and_handoff_are_explicit_unchecked_recovery_routes() {
    let (requests, output) = m835_scripted_cli(
        &["status", "server", "--json"],
        vec![serde_json::json!({"result":{"type":"pong", "version":"fixture", "protocol":18}})],
        false,
    );
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["ping"]
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(status.to_string().contains("18"));
    let (requests, output) = m835_scripted_cli(
        &["server", "live-handoff"],
        vec![
            serde_json::json!({"error":{"code":"fixture_handoff_denied", "message":"unchecked request reached listener"}}),
        ],
        false,
    );
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["server.live_handoff"]
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture_handoff_denied"));
}

const M835_MUTATING_VERBS: &[&[&str]] = &[
    &["send"],
    &["reply"],
    &["agent", "send"],
    &["pane", "run"],
    &["pane", "send-text"],
];

fn m835_f4_refusal(verb: &[&str], late: bool, wrong_protocol: u32) {
    use serde_json::{json, Value};
    use sqlx::Connection;
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = SnapshotCliFixture::new();
    let socket = fixture.base.join("f4-guard.sock");
    let db = fixture.base.join("cli-sqlite/zynk.db");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    if late {
        fs::write(fixture.base.join("runtime.id"), "rt_m835\n").unwrap();
    }
    let (key, resolve) = if verb[0] == "pane" {
        ("pane", "pane.get")
    } else {
        ("agent", "agent.get")
    };
    let expected = if late {
        vec!["ping", resolve, "ping"]
    } else {
        vec!["ping"]
    };
    let done = AtomicBool::new(false);
    let (output, requests, recorded) = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut requests = Vec::new();
            let mut recorded = None;
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(Ordering::Acquire) { break; }
                        thread::sleep(Duration::from_millis(5)); continue;
                    }
                    Err(e) => panic!("F4 accept: {e}"),
                };
                stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                requests.push(request.clone());
                let n = requests.len();
                if late && n == expected.len() && request["method"] == "ping" {
                    recorded = Some(sqlite_block_on(async {
                        tokio::time::timeout(Duration::from_secs(1), async {
                            let options = sqlx::sqlite::SqliteConnectOptions::new()
                                .filename(&db).read_only(true).busy_timeout(Duration::from_millis(200));
                            let mut conn = sqlx::SqliteConnection::connect_with(&options).await.unwrap();
                            let ids = sqlx::query_scalar::<_, String>("SELECT id FROM messages").fetch_all(&mut conn).await.unwrap();
                            let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM delivery_events")
                                .fetch_one(&mut conn).await.unwrap();
                            assert_eq!(count, 0);
                            conn.close().await.unwrap();
                            ids
                        }).await.expect("bounded pre-refusal DB observation")
                    }));
                }
                let result = if request["method"] == "ping" {
                    json!({"type":"pong", "version":"fixture", "protocol":if n == expected.len() { wrong_protocol } else { 19 }})
                } else {
                    json!({"type":format!("{key}_info"), (key):{"pane_id":"w1:p1", "terminal_id":"term_m835", "workspace_id":"w1", "tab_id":"w1:t1"}})
                };
                writeln!(stream, "{}", json!({"id":request["id"], "result":result})).unwrap();
            }
            (requests, recorded)
        });
        let mut args = verb.to_vec();
        args.extend(["w1:p1", "--", "body"]);
        let output = run_snapshot_cli_bounded(&fixture.base, &socket, &args);
        done.store(true, Ordering::Release);
        let (requests, recorded) = worker.join().unwrap();
        (output, requests, recorded)
    });
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected,
        "{verb:?}"
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["command"],
        if verb.len() == 1 {
            format!("zynk {}", verb[0])
        } else {
            verb.join(" ")
        }
    );
    assert_eq!(value["result"], "failed");
    assert_eq!(
        value["target_resolution"],
        if late { "resolved" } else { "unknown" }
    );
    assert_eq!(value["error"]["code"], "transport_failed");
    assert!(value["error"].get("context").is_none());
    for field in ["delivery_status", "proof", "submitted_at"] {
        assert!(value.get(field).is_none(), "{value}");
    }
    if late {
        let id = value["message_id"].as_str().unwrap();
        assert_eq!(recorded.unwrap(), vec![id.to_owned()]);
        assert_eq!(delivery_events_of(&db, id), vec!["failed".to_owned()]);
    } else {
        assert!(value.get("conversation_id").is_none());
        assert_eq!(
            value["error"]["message"],
            if verb[0] == "pane" {
                "could not reach zynk to resolve pane 'w1:p1'"
            } else {
                "could not reach zynk to resolve the target 'w1:p1'"
            }
        );
        fixture.assert_no_runtime_created();
    }
}

#[test]
fn m835_pre_resolution_mismatch_preserves_unknown_f4_without_attempt() {
    for verb in M835_MUTATING_VERBS {
        for protocol in [18, 20] {
            m835_f4_refusal(verb, false, protocol);
        }
    }
}

#[test]
fn m835_post_recorded_attempt_mismatch_appends_failed_without_submit_or_receipt() {
    for verb in M835_MUTATING_VERBS {
        for protocol in [18, 20] {
            m835_f4_refusal(verb, true, protocol);
        }
    }
}

#[test]
fn m833_popup_cli_preserves_dimensions_and_close_wire_shape_under_guard() {
    for (args, expected) in [
        (
            vec![
                "plugin",
                "pane",
                "open",
                "--plugin",
                "example.popup",
                "--entrypoint",
                "board",
                "--placement",
                "popup",
                "--width",
                "80%",
                "--height",
                "12",
                "--no-focus",
            ],
            "plugin.pane.open",
        ),
        (vec!["popup", "close"], "popup.close"),
    ] {
        let (requests, output) = m835_scripted_cli(
            &args,
            vec![
                serde_json::json!({"result":{"type":"pong", "version":"fixture", "protocol":19}}),
                serde_json::json!({"result":{"type":"ok"}}),
            ],
            false,
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            requests
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["ping", expected]
        );
        if expected == "plugin.pane.open" {
            assert_eq!(requests[1]["params"]["placement"], "popup");
            assert_eq!(requests[1]["params"]["width"], "80%");
            assert_eq!(requests[1]["params"]["height"], 12);
            assert_eq!(requests[1]["params"]["focus"], false);
        } else {
            assert_eq!(requests[1]["params"], serde_json::json!({}));
        }
    }

    for (args, expected_code) in [
        (vec!["popup"], 2),
        (vec!["popup", "unknown"], 2),
        (vec!["popup", "close", "extra"], 2),
        (vec!["popup", "--help"], 0),
        (vec!["popup", "close", "--help"], 0),
    ] {
        let (requests, output) = m835_scripted_cli(&args, vec![], false);
        assert!(requests.is_empty(), "{args:?}: {requests:?}");
        assert_eq!(
            output.status.code(),
            Some(expected_code),
            "{args:?}: {output:?}"
        );
    }
}

fn m837_exchange(socket: &Path, request: serde_json::Value) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn m837_delivery_count(db: &Path) -> i64 {
    use sqlx::Connection;
    sqlite_block_on(async {
        tokio::time::timeout(Duration::from_secs(1), async {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .read_only(true)
                .busy_timeout(Duration::from_millis(200));
            let mut connection = sqlx::SqliteConnection::connect_with(&options)
                .await
                .unwrap();
            let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM delivery_events")
                .fetch_one(&mut connection)
                .await
                .unwrap();
            connection.close().await.unwrap();
            count
        })
        .await
        .expect("bounded delivery-event observation")
    })
}

#[test]
fn m837_real_wait_setup_error_writes_no_delivery_events() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket = runtime_dir.join("zynk.sock");
    let zynk = spawn_zynk(&config_home, &runtime_dir, &socket);
    wait_for_socket(&socket, Duration::from_secs(5));
    let created = m837_exchange(
        &socket,
        serde_json::json!({
            "id": "m837:create", "method": "workspace.create", "params": {"cwd": base, "focus": true}
        }),
    );
    assert!(created["result"]["workspace"]["workspace_id"].is_string());
    let db = config_home.join("sqlite/zynk.db");
    let before = m837_delivery_count(&db);
    let response = m837_exchange(
        &socket,
        serde_json::json!({
            "id": "m837:wait", "method": "events.wait", "params": {
                "match_event": {"event": "pane_agent_status_changed",
                    "pane_id": "missing-m837", "agent_status": "idle"},
                "timeout_ms": 100
            }
        }),
    );
    assert_eq!(
        response,
        serde_json::json!({"id": "m837:wait", "error": {
            "code": "pane_not_found", "message": "pane missing-m837 not found"
        }})
    );
    assert_eq!(m837_delivery_count(&db), before);
    cleanup_spawned_zynk(zynk, base);
}

fn m839_agent_json(status: &str, name: &str, terminal: &str, sequence: u64) -> serde_json::Value {
    serde_json::json!({
        "terminal_id": terminal, "name": name, "agent": "codex", "agent_status": status,
        "workspace_id": "w1", "tab_id": "w1:t1", "pane_id": "w1:p1",
        "focused": false, "revision": 0, "state_change_seq": sequence,
        "interactive_ready": status == "idle" || status == "blocked" || status == "done"
    })
}

fn m839_pong() -> serde_json::Value {
    serde_json::json!({"result":{"type":"pong", "version":"fixture", "protocol":19}})
}

fn m839_agent_reply(agent: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"result":{"type":"agent_info", "agent":agent}})
}

fn m839_cli_exchange<F>(
    args: &[&str],
    mut respond: F,
) -> (
    SnapshotCliFixture,
    Vec<serde_json::Value>,
    std::process::Output,
)
where
    F: FnMut(&serde_json::Value, &Path) -> serde_json::Value + Send,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = SnapshotCliFixture::new();
    let socket = fixture.base.join("m839.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = AtomicBool::new(false);
    let (requests, output) = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut requests = Vec::new();
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(err) => panic!("m839 accept: {err}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let mut response = respond(&request, &fixture.base);
                requests.push(request.clone());
                if let Some(raw) = response.as_str() {
                    writeln!(stream, "{raw}").unwrap();
                } else if !response.is_null() {
                    response["id"] = request["id"].clone();
                    writeln!(stream, "{response}").unwrap();
                }
            }
            requests
        });
        let output = run_snapshot_cli_bounded(&fixture.base, &socket, args);
        done.store(true, Ordering::Release);
        (worker.join().unwrap(), output)
    });
    (fixture, requests, output)
}

#[test]
fn m839c_agent_grammar_refuses_before_resolution_or_persistence() {
    for args in [
        vec!["agent", "start"],
        vec!["agent", "start", "worker", "--pane", "w1:p1"],
        vec!["agent", "start", "worker", "--kind", "codex"],
        vec![
            "agent", "start", "worker", "--kind", "omp", "--pane", "w1:p1",
        ],
        vec![
            "agent", "start", "worker", "--kind", "codex", "--pane", "w1:p1", "--cwd", "/tmp",
        ],
        vec![
            "agent", "start", "worker", "--kind", "codex", "--kind", "qwen", "--pane", "w1:p1",
        ],
        vec![
            "agent",
            "start",
            "worker",
            "--kind",
            "codex",
            "--pane",
            "w1:p1",
            "--timeout",
            "-1",
        ],
        vec![
            "agent",
            "start",
            "worker",
            "--kind",
            "codex",
            "--pane",
            "w1:p1",
            "--timeout",
            "18446744073709551616",
        ],
        vec!["agent", "prompt"],
        vec!["agent", "prompt", "worker"],
        vec!["agent", "prompt", "worker", ""],
        vec!["agent", "prompt", "worker", "--unknown", "body"],
        vec!["agent", "prompt", "worker", "--type"],
        vec!["agent", "prompt", "worker", "--trace"],
        vec!["agent", "prompt", "worker", "--wait", "--wait", "body"],
        vec![
            "agent", "prompt", "worker", "--type", "note", "--type", "status", "body",
        ],
        vec![
            "agent", "prompt", "worker", "--trace", "a", "--trace", "b", "body",
        ],
        vec!["agent", "prompt", "worker", "--timeout", "100", "body"],
        vec![
            "agent",
            "prompt",
            "worker",
            "--wait",
            "--timeout",
            "no",
            "body",
        ],
        vec!["agent", "wait"],
        vec!["agent", "wait", "worker", "--status", "idle"],
        vec!["agent", "wait", "worker", "--timeout"],
        vec!["agent", "wait", "worker", "--timeout", "-1"],
        vec![
            "agent",
            "wait",
            "worker",
            "--timeout",
            "1",
            "--timeout",
            "2",
        ],
    ] {
        let (fixture, requests, output) =
            m839_cli_exchange(&args, |_, _| panic!("syntax dispatched"));
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(requests.is_empty(), "{args:?}");
        fixture.assert_no_runtime_created();
    }
    let help = run_zynk_help(&["agent", "--help"]);
    assert!(help.contains("agent prompt <name>"));
    assert!(help.contains("idle, done or blocked"));
    assert!(!help.contains("agent wait <target> --status"));
}

#[test]
fn m839c_wait_current_completes_without_relabeling_blocked_or_receipts() {
    for status in ["idle", "done", "blocked"] {
        let mut agent = m839_agent_json(status, "worker", "term_original", 7);
        agent["interactive_ready"] = serde_json::json!(false);
        let (fixture, requests, output) = m839_cli_exchange(
            &["agent", "wait", "worker", "--timeout", "500"],
            |request, _| match request["method"].as_str().unwrap() {
                "ping" => m839_pong(),
                "agent.get" => m839_agent_reply(agent.clone()),
                other => panic!("unexpected {other}"),
            },
        );
        assert_eq!(
            requests
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["ping", "agent.get"]
        );
        assert_eq!(requests[1]["params"]["target"], "worker");
        assert_eq!(output.status.code(), Some(0), "{status}: {output:?}");
        assert!(output.stderr.is_empty());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["result"]["agent"]["agent_status"], status);
        assert_eq!(result["result"]["agent"]["terminal_id"], "term_original");
        assert!(result.get("message_id").is_none());
        assert!(result.get("delivery_status").is_none());
        fixture.assert_no_runtime_created();
    }
}

#[test]
fn m839c_wait_pins_terminal_once_and_guards_only_the_first_poll() {
    let working = m839_agent_json("working", "worker", "term_original", 7);
    let mut done = m839_agent_json("done", "worker", "term_original", 7);
    done["interactive_ready"] = serde_json::json!(false);
    let (requests, output) = m835_scripted_cli(
        &["agent", "wait", "worker", "--timeout", "1500"],
        vec![
            m839_pong(),
            m839_agent_reply(working.clone()),
            m839_pong(),
            m839_agent_reply(working),
            m839_agent_reply(done.clone()),
        ],
        false,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ping", "agent.get", "ping", "agent.get", "agent.get"]
    );
    assert_eq!(requests[1]["params"]["target"], "worker");
    for index in [3, 4] {
        assert_eq!(requests[index]["params"]["target"], "term_original");
        assert_eq!(requests[index]["id"], "cli:agent:wait");
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["agent"]["state_change_seq"], 7);
    assert!(matches!(
        value["result"]["agent"].get("interactive_ready"),
        None | Some(serde_json::Value::Bool(false))
    ));
    assert_eq!(value["result"]["agent"]["agent_status"], "done");
    assert_eq!(value["result"]["agent"]["terminal_id"], "term_original");
    assert_eq!(value["result"]["agent"]["name"], "worker");
}

#[test]
fn m839c_wait_checks_later_unchecked_responses_and_first_poll_guard() {
    for (reply, code) in [
        (m839_agent_reply(m839_agent_json("done", "worker", "term_replacement", 9)), "agent_name_not_found"),
        (serde_json::json!({"id":"wrong", "result":{"type":"agent_info", "agent":m839_agent_json("done", "worker", "term_original", 9)}}).to_string().into(), "invalid_response"),
    ] {
        let working = m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7));
        let (requests, output) = m835_scripted_cli(
            &["agent", "wait", "worker", "--timeout", "1500"],
            vec![m839_pong(), working.clone(), m839_pong(), working, reply], false,
        );
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[4]["params"]["target"], "term_original");
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], code);
    }
    let mut incompatible = m839_pong();
    incompatible["result"]["protocol"] = serde_json::json!(20);
    let (requests, output) = m835_scripted_cli(
        &["agent", "wait", "worker", "--timeout", "1500"],
        vec![
            m839_pong(),
            m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7)),
            incompatible,
        ],
        false,
    );
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ping", "agent.get", "ping"]
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "protocol_mismatch");
}

#[test]
fn m839c_wait_typed_identity_checks_precede_completion() {
    let valid = m839_agent_json("idle", "worker", "term_original", 7);
    let mut cases = Vec::new();
    for field in ["terminal_id", "agent_status", "focused", "revision"] {
        let mut agent = valid.clone();
        agent.as_object_mut().unwrap().remove(field);
        cases.push((m839_agent_reply(agent), "invalid_response"));
    }
    let mut empty_terminal = valid.clone();
    empty_terminal["terminal_id"] = serde_json::json!("");
    cases.push((m839_agent_reply(empty_terminal), "invalid_response"));
    let mut bad_sequence = valid.clone();
    bad_sequence["state_change_seq"] = serde_json::json!("7");
    cases.push((m839_agent_reply(bad_sequence), "invalid_response"));
    cases.push((
        serde_json::json!({"result":{"type":"pane_info", "agent":valid}}),
        "invalid_response",
    ));
    cases.push((
        m839_agent_reply(m839_agent_json("idle", "reused", "term_original", 7)),
        "agent_name_not_found",
    ));
    let wrong_id = serde_json::json!({"id":"wrong", "result":{"type":"agent_info", "agent":m839_agent_json("idle", "worker", "term_original", 7)}});
    cases.push((serde_json::json!(wrong_id.to_string()), "invalid_response"));
    for (response, code) in cases {
        let (requests, output) = m835_scripted_cli(
            &["agent", "wait", "worker"],
            vec![m839_pong(), response],
            false,
        );
        assert_eq!(requests.len(), 2);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], code);
    }
}

#[test]
fn m839c_wait_poll_error_precedence_refuses_retargeting() {
    for (reply, code) in [
        (m839_agent_reply(m839_agent_json("unknown", "reused", "term_replacement", 9)), "agent_name_not_found"),
        (m839_agent_reply(m839_agent_json("unknown", "reused", "term_original", 9)), "agent_name_not_found"),
        (m839_agent_reply(m839_agent_json("unknown", "worker", "term_original", 9)), "agent_not_running"),
        (m839_agent_reply(m839_agent_json("done", "worker", "term_replacement", 9)), "agent_name_not_found"),
        (serde_json::json!({"error":{"code":"agent_not_found", "message":"lost terminal"}}), "agent_not_found"),
        (serde_json::json!({"id":"wrong", "result":{"type":"agent_info", "agent":m839_agent_json("unknown", "reused", "term_replacement", 9)}}).to_string().into(), "invalid_response"),
    ] {
        let (requests, output) = m835_scripted_cli(
            &["agent", "wait", "worker", "--timeout", "1500"],
            vec![m839_pong(), m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7)), m839_pong(), reply], false,
        );
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[3]["params"]["target"], "term_original");
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], code);
        assert_eq!(error["id"], "cli:agent:wait");
    }

    let (requests, output) = m835_scripted_cli(
        &["agent", "wait", "worker", "--timeout", "1500"],
        vec![
            m839_pong(),
            m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7)),
            m839_pong(),
            m839_agent_reply(m839_agent_json("done", "worker", "", 9)),
        ],
        false,
    );
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[3]["params"]["target"], "term_original");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(
        (error["id"].as_str(), error["error"]["code"].as_str()),
        (Some("cli:agent:wait"), Some("agent_name_not_found"))
    );
}

#[test]
fn m839c_wait_zero_timeout_precedes_poll_and_does_not_bound_inflight_read() {
    let (requests, output) = m835_scripted_cli(
        &["agent", "wait", "worker", "--timeout", "0"],
        vec![
            m839_pong(),
            m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7)),
        ],
        false,
    );
    assert_eq!(requests.len(), 2);
    assert_eq!(output.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "timeout");

    let mut gets = 0;
    let (fixture, requests, output) = m839_cli_exchange(
        &["agent", "wait", "worker", "--timeout", "500"],
        |request, _| {
            if request["method"] == "ping" {
                return m839_pong();
            }
            assert_eq!(request["method"], "agent.get");
            gets += 1;
            if gets == 1 {
                return m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7));
            }
            assert_eq!(gets, 2);
            assert_eq!(request["params"]["target"], "term_original");
            thread::sleep(Duration::from_millis(600));
            m839_agent_reply(m839_agent_json("done", "worker", "term_original", 8))
        },
    );
    assert_eq!(gets, 2);
    assert_eq!(requests.len(), 4);
    assert_eq!(
        output.status.code(),
        Some(0),
        "late response, not request deadline: {output:?}"
    );
    fixture.assert_no_runtime_created();

    let mut gets = 0;
    let (fixture, requests, output) = m839_cli_exchange(
        &["agent", "wait", "worker", "--timeout", "500"],
        |request, _| {
            if request["method"] == "ping" {
                return m839_pong();
            }
            assert_eq!(request["method"], "agent.get");
            gets += 1;
            assert!(gets <= 2, "positive deadline ignored");
            if gets == 2 {
                thread::sleep(Duration::from_millis(600));
            }
            m839_agent_reply(m839_agent_json("working", "worker", "term_original", 7))
        },
    );
    assert_eq!(gets, 2);
    assert_eq!(requests.len(), 4);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "timeout");
    fixture.assert_no_runtime_created();
}

#[test]
fn m839c_start_waits_for_ready_and_refuses_nonpending_failure() {
    for failed in [false, true] {
        let mut pending = m839_agent_json("unknown", "worker", "term_original", 0);
        pending["launch_pending"] = serde_json::json!(true);
        let mut settled = m839_agent_json(
            if failed { "unknown" } else { "idle" },
            "worker",
            "term_original",
            1,
        );
        settled["launch_pending"] = serde_json::json!(false);
        let mut not_ready = m839_agent_json("idle", "worker", "term_original", 1);
        not_ready["interactive_ready"] = serde_json::json!(false);
        not_ready["launch_pending"] = serde_json::json!(true);
        let started = serde_json::json!({"result":{"type":"agent_started", "agent":pending, "argv":["codex"]}});
        let (requests, output) = m835_scripted_cli(
            &[
                "agent",
                "start",
                "worker",
                "--kind",
                "codex",
                "--pane",
                "w1:p1",
                "--timeout",
                "3001",
            ],
            vec![
                m839_pong(),
                started,
                m839_pong(),
                m839_agent_reply(pending),
                m839_agent_reply(not_ready),
                m839_agent_reply(settled),
            ],
            false,
        );
        assert_eq!(
            requests
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "ping",
                "agent.start",
                "ping",
                "agent.get",
                "agent.get",
                "agent.get"
            ]
        );
        assert_eq!(requests[5]["params"]["target"], "term_original");
        assert_eq!(
            output.status.code(),
            Some(if failed { 1 } else { 0 }),
            "{output:?}"
        );
        if failed {
            assert!(output.stdout.is_empty());
            let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "agent_start_failed");
        } else {
            assert!(output.stderr.is_empty());
            let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["result"]["agent"]["interactive_ready"], true);
        }
    }
}

#[test]
fn m839c_start_preserves_argv_and_checks_kind_before_name_after_terminal_pin() {
    for (terminal, kind, name, status, code) in [
        ("term_original", "qwen", "worker", "idle", None),
        (
            "term_original",
            "codex",
            "reused",
            "unknown",
            Some("agent_kind_mismatch"),
        ),
        (
            "term_replacement",
            "codex",
            "reused",
            "unknown",
            Some("agent_name_not_found"),
        ),
        (
            "term_original",
            "qwen",
            "reused",
            "idle",
            Some("agent_name_not_found"),
        ),
    ] {
        let mut launched = m839_agent_json("unknown", "worker", "term_original", 0);
        launched["agent"] = serde_json::json!("qwen");
        launched["launch_pending"] = serde_json::json!(true);
        let mut polled = m839_agent_json(status, name, terminal, 1);
        polled["agent"] = serde_json::json!(kind);
        let (requests, output) = m835_scripted_cli(
            &[
                "agent",
                "start",
                "worker",
                "--kind",
                "qwen",
                "--pane",
                "w1:p1",
                "--timeout",
                "3001",
                "--",
                "",
                "two words",
                "a'b",
                "$HOME",
                "one\\two",
            ],
            vec![
                m839_pong(),
                serde_json::json!({"result":{"type":"agent_started", "agent":launched, "argv":["qwen", "", "two words", "a'b", "$HOME", "one\\two"]}}),
                m839_pong(),
                m839_agent_reply(polled),
            ],
            false,
        );
        assert_eq!(
            requests
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["ping", "agent.start", "ping", "agent.get"]
        );
        assert_eq!(
            requests[1]["params"],
            serde_json::json!({"name":"worker", "kind":"qwen", "pane_id":"w1:p1", "timeout_ms":3001, "args":["", "two words", "a'b", "$HOME", "one\\two"]})
        );
        assert_eq!(requests[3]["params"]["target"], "term_original");
        assert_eq!(
            output.status.code(),
            Some(if code.is_some() { 1 } else { 0 }),
            "{output:?}"
        );
        if let Some(code) = code {
            assert!(output.stdout.is_empty());
            let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], code);
        } else {
            assert!(output.stderr.is_empty());
            let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["result"]["agent"]["name"], "worker");
            assert_eq!(response["result"]["agent"]["agent"], "qwen");
        }
    }
}

fn m839_session(value: &str) -> serde_json::Value {
    serde_json::json!({"source":"fixture-hook", "agent":"codex", "kind":"id", "value":value})
}

fn m839_original_party(session: Option<&str>) -> serde_json::Value {
    let mut party = serde_json::json!({
        "agent":"codex", "pane":"w1:p1", "terminal_id":"term_original",
        "workspace":"w1", "tab":"w1:t1"
    });
    if let Some(session) = session {
        party["agent_session"] = m839_session(session);
    }
    party
}

fn m839_delivery_rows(db: &Path) -> Vec<serde_json::Value> {
    use sqlx::{Connection, Row};
    sqlite_block_on(async {
        tokio::time::timeout(Duration::from_secs(1), async {
            let options = sqlx::sqlite::SqliteConnectOptions::new().filename(db).read_only(true)
                .busy_timeout(Duration::from_millis(200));
            let mut connection = sqlx::SqliteConnection::connect_with(&options).await.unwrap();
            let rows = sqlx::query("SELECT message_id, event_type, proof_source, timestamp, payload_json FROM delivery_events ORDER BY message_id, seq")
                .fetch_all(&mut connection).await.unwrap();
            let result = rows.into_iter().map(|row| serde_json::json!({
                "message_id":row.get::<String,_>("message_id"),
                "event_type":row.get::<String,_>("event_type"),
                "proof_source":row.get::<String,_>("proof_source"),
                "timestamp":row.get::<String,_>("timestamp"),
                "payload":serde_json::from_str::<serde_json::Value>(&row.get::<String,_>("payload_json")).unwrap()
            })).collect();
            connection.close().await.unwrap();
            result
        }).await.expect("bounded m839 delivery snapshot")
    })
}

fn m839_message_rows(db: &Path) -> Vec<serde_json::Value> {
    use sqlx::{Connection, Row};
    sqlite_block_on(async {
        tokio::time::timeout(Duration::from_secs(1), async {
            let options = sqlx::sqlite::SqliteConnectOptions::new().filename(db).read_only(true)
                .busy_timeout(Duration::from_millis(200));
            let mut connection = sqlx::SqliteConnection::connect_with(&options).await.unwrap();
            let rows = sqlx::query("SELECT m.id, m.body, m.body_hash, m.type, m.meta_json, p.agent_label, p.terminal_id, p.agent_session_source, p.agent_session_kind, p.agent_session_value FROM messages m JOIN conversation_participants p ON p.id=m.to_participant_id ORDER BY m.conversation_seq")
                .fetch_all(&mut connection).await.unwrap();
            let result = rows.into_iter().map(|row| serde_json::json!({
                "id":row.get::<String,_>("id"), "body":row.get::<String,_>("body"),
                "body_hash":row.get::<String,_>("body_hash"), "type":row.get::<Option<String>,_>("type"),
                "meta":serde_json::from_str::<serde_json::Value>(&row.get::<String,_>("meta_json")).unwrap(),
                "agent":row.get::<String,_>("agent_label"), "terminal_id":row.get::<Option<String>,_>("terminal_id"),
                "session_source":row.get::<Option<String>,_>("agent_session_source"),
                "session_kind":row.get::<Option<String>,_>("agent_session_kind"),
                "session_value":row.get::<Option<String>,_>("agent_session_value")
            })).collect();
            connection.close().await.unwrap();
            result
        }).await.expect("bounded m839 message snapshot")
    })
}

#[test]
fn m839c_prompt_persists_pure_body_and_resolved_party_before_single_dispatch() {
    use sha2::{Digest, Sha256};
    for (body_args, pure) in [
        (vec!["body\nwith spaces"], "body\nwith spaces"),
        (vec!["two", "parts"], "two parts"),
        (vec!["--", "--dash body"], "--dash body"),
        (vec!["   "], "   "),
    ] {
        let mut args = vec![
            "agent",
            "prompt",
            "worker",
            "--type",
            "note",
            "--trace",
            "M839_TRACE",
        ];
        args.extend(body_args);
        let mut resolved = 0;
        let mut submitted = 0;
        let (fixture, requests, output) = m839_cli_exchange(&args, |request, base| {
            if request["method"] == "ping" {
                return m839_pong();
            }
            if request["method"] == "agent.get" {
                resolved += 1;
                assert_eq!(resolved, 1);
                assert_eq!(request["params"]["target"], "worker");
                fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                return m839_agent_reply(m839_agent_json("idle", "worker", "term_original", 7));
            }
            assert_eq!(request["method"], "agent.prompt");
            submitted += 1;
            assert_eq!(submitted, 1);
            assert_eq!(request["params"]["target"], "worker");
            assert_eq!(request["params"]["expected_terminal_id"], "term_original");
            let db = base.join("cli-sqlite/zynk.db");
            let messages = m839_message_rows(&db);
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0]["body"], pure);
            assert_eq!(messages[0]["agent"], "codex");
            assert_eq!(messages[0]["terminal_id"], "term_original");
            assert_eq!(messages[0]["session_value"], serde_json::Value::Null);
            assert!(m839_delivery_rows(&db).is_empty());
            let wire = request["params"]["text"].as_str().unwrap();
            assert!(wire.ends_with(pure), "{wire:?}");
            assert!(wire.contains(messages[0]["id"].as_str().unwrap()));
            assert_eq!(wire.matches("Zynk message").count(), 1, "{wire:?}");
            serde_json::json!({"result":{"type":"agent_prompted", "agent":m839_agent_json("idle", "worker", "term_original", 7), "baseline_state_change_seq":7}})
        });
        assert_eq!((resolved, submitted), (1, 1));
        assert_eq!(
            requests
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["ping", "agent.get", "ping", "agent.prompt"]
        );
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["command"], "agent prompt");
        assert_eq!(value["result"], "ok");
        assert_eq!(value["delivery_status"], "submitted");
        assert_eq!(value["proof"]["proof_source"], "agent.prompt");
        assert_eq!(value["type"], "note");
        assert_eq!(value["to"], m839_original_party(None));
        assert!(value.get("wait").is_none());
        let messages = m839_message_rows(&fixture.base.join("cli-sqlite/zynk.db"));
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]["body_hash"],
            format!("{:x}", Sha256::digest(pure.as_bytes()))
        );
        assert_eq!(messages[0]["meta"]["trace_id"], "M839_TRACE");
        let events = m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event_type"], "submitted");
        assert_eq!(events[0]["proof_source"], "agent.prompt");
        assert_eq!(events[0]["payload"]["terminal_id"], "term_original");
        assert_eq!(events[0]["message_id"], value["message_id"]);
        assert_eq!(events[0]["timestamp"], value["submitted_at"]);
    }

    let (fixture, requests, output) = m839_cli_exchange(
        &["agent", "prompt", "worker", "--trace", " inherit ", "body"],
        |request, base| {
            if request["method"] == "ping" {
                return m839_pong();
            }
            if request["method"] == "agent.get" {
                fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                return m839_agent_reply(m839_agent_json("idle", "worker", "term_original", 7));
            }
            assert_eq!(request["method"], "agent.prompt");
            serde_json::json!({"result":{"type":"agent_prompted", "agent":m839_agent_json("idle", "worker", "term_original", 7), "baseline_state_change_seq":7}})
        },
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let messages = m839_message_rows(&fixture.base.join("cli-sqlite/zynk.db"));
    assert_eq!(
        messages[0]["meta"]["trace_id"], "inherit",
        "explicit trace remains explicit after trimming"
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "agent.prompt")
            .count(),
        1
    );
    assert_eq!(
        m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db")).len(),
        1
    );
}

#[test]
fn m839c_prompt_wait_failures_preserve_one_durable_submission_and_exit_three() {
    for (case, code, effect) in [
        ("timeout", "timeout", "submitted_wait_timeout"),
        (
            "transport",
            "transport_failed",
            "submitted_wait_transport_failed",
        ),
        (
            "protocol",
            "protocol_mismatch",
            "submitted_wait_protocol_mismatch",
        ),
        ("name", "agent_name_not_found", "submitted_wait_name_lost"),
        (
            "replacement",
            "agent_name_not_found",
            "submitted_wait_name_lost",
        ),
        ("missing", "agent_not_found", "submitted_wait_terminal_lost"),
        ("unknown", "agent_not_running", "submitted_wait_not_running"),
        (
            "invalid",
            "invalid_response",
            "submitted_wait_invalid_response",
        ),
        ("refused", "permission_denied", "submitted_wait_refused"),
    ] {
        let mut prompts = 0;
        let mut gets = 0;
        let mut before_wait = None;
        let timeout = if case == "timeout" { "0" } else { "1000" };
        let (fixture, requests, output) = m839_cli_exchange(
            &[
                "agent",
                "prompt",
                "worker",
                "--wait",
                "--timeout",
                timeout,
                "body",
            ],
            |request, base| {
                if request["method"] == "ping" {
                    if prompts == 1 {
                        before_wait = Some(m839_delivery_rows(&base.join("cli-sqlite/zynk.db")));
                        if case == "protocol" {
                            return serde_json::json!({"result":{"type":"pong", "version":"replacement", "protocol":20}});
                        }
                    }
                    return m839_pong();
                }
                if request["method"] == "agent.prompt" {
                    prompts += 1;
                    assert_eq!(prompts, 1);
                    assert!(m839_delivery_rows(&base.join("cli-sqlite/zynk.db")).is_empty());
                    let mut agent = m839_agent_json("idle", "worker", "term_original", 7);
                    agent["agent_session"] = m839_session("submission-session-B");
                    return serde_json::json!({"result":{"type":"agent_prompted", "agent":agent, "baseline_state_change_seq":7}});
                }
                assert_eq!(request["method"], "agent.get");
                gets += 1;
                if gets == 1 {
                    fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                    assert_eq!(request["params"]["target"], "worker");
                    let mut agent = m839_agent_json("idle", "worker", "term_original", 5);
                    agent["agent_session"] = m839_session("resolution-session-A");
                    return m839_agent_reply(agent);
                }
                assert_eq!(gets, 2);
                assert_eq!(request["params"]["target"], "term_original");
                match case {
                    "transport" => serde_json::Value::Null,
                    "name" => {
                        m839_agent_reply(m839_agent_json("done", "reused", "term_original", 8))
                    }
                    "replacement" => {
                        m839_agent_reply(m839_agent_json("done", "worker", "term_new", 8))
                    }
                    "missing" => {
                        serde_json::json!({"error":{"code":"agent_not_found", "message":"old terminal lost"}})
                    }
                    "unknown" => {
                        m839_agent_reply(m839_agent_json("unknown", "worker", "term_original", 8))
                    }
                    "invalid" => serde_json::json!({"result":{"type":"ok"}}),
                    "refused" => {
                        serde_json::json!({"error":{"code":"permission_denied", "message":"wait refused"}})
                    }
                    _ => panic!("unexpected polling request for {case}"),
                }
            },
        );
        assert_eq!(prompts, 1, "case={case}");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["method"] == "agent.prompt")
                .count(),
            1
        );
        assert_eq!(output.status.code(), Some(3), "case={case}: {output:?}");
        assert!(output.stderr.is_empty(), "case={case}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["result"], "ok");
        assert_eq!(value["delivery_status"], "submitted");
        assert_eq!(value["proof"]["proof_source"], "agent.prompt");
        assert_eq!(
            value["to"],
            m839_original_party(Some("resolution-session-A"))
        );
        assert!(value.get("error").is_none());
        assert_eq!(value["wait"]["result"], "failed");
        assert_eq!(value["wait"]["error"]["code"], code);
        assert_eq!(
            value["wait"]["error"]["context"]["transport_effect"],
            effect
        );
        assert!(value["next"].as_str().unwrap().contains("do not resubmit"));
        let events = m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db"));
        let messages = m839_message_rows(&fixture.base.join("cli-sqlite/zynk.db"));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["agent"], "codex");
        assert_eq!(messages[0]["terminal_id"], "term_original");
        assert_eq!(messages[0]["session_source"], "fixture-hook");
        assert_eq!(messages[0]["session_kind"], "id");
        assert_eq!(messages[0]["session_value"], "resolution-session-A");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event_type"], "submitted");
        assert_eq!(events[0]["payload"]["terminal_id"], "term_original");
        assert_eq!(events[0]["message_id"], value["message_id"]);
        assert_eq!(events[0]["timestamp"], value["submitted_at"]);
        if case == "timeout" {
            assert!(before_wait.is_none());
        } else {
            assert_eq!(
                before_wait.unwrap(),
                events,
                "durable before first wait guard: {case}"
            );
        }
    }

    for case in ["sequence_missing", "malformed", "both"] {
        let mut prompts = 0;
        let mut polls = 0;
        let (fixture, requests, output) = m839_cli_exchange(
            &[
                "agent",
                "prompt",
                "worker",
                "--wait",
                "--timeout",
                "150",
                "body",
            ],
            |request, base| {
                if request["method"] == "ping" {
                    return m839_pong();
                }
                if request["method"] == "agent.prompt" {
                    prompts += 1;
                    return serde_json::json!({"result":{"type":"agent_prompted", "agent":m839_agent_json("idle", "worker", "term_original", 7), "baseline_state_change_seq":7}});
                }
                assert_eq!(request["method"], "agent.get");
                if prompts == 0 {
                    fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                    return m839_agent_reply(m839_agent_json("idle", "worker", "term_original", 7));
                }
                polls += 1;
                if case == "malformed" {
                    return serde_json::Value::String("{malformed".into());
                }
                let mut agent = m839_agent_json("done", "worker", "term_original", 8);
                if case == "sequence_missing" {
                    agent.as_object_mut().unwrap().remove("state_change_seq");
                }
                let mut response = m839_agent_reply(agent);
                if case == "both" {
                    response["error"] = serde_json::json!({"code":"permission_denied", "message":"contradictory poll"});
                }
                response
            },
        );
        assert_eq!(output.status.code(), Some(3), "{case}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["wait"]["error"]["code"], "invalid_response", "{case}");
        assert_eq!(
            value["wait"]["error"]["context"]["transport_effect"],
            "submitted_wait_invalid_response"
        );
        assert_eq!(value["delivery_status"], "submitted");
        assert_eq!(value["result"], "ok");
        assert_eq!(value["to"], m839_original_party(None));
        assert!(value["next"].as_str().unwrap().contains("do not resubmit"));
        assert_eq!((prompts, polls), (1, 1));
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["method"] == "agent.prompt")
                .count(),
            1
        );
        let events = m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event_type"], "submitted");
        assert_eq!(events[0]["message_id"], value["message_id"]);
    }
}

#[test]
fn m839c_prompt_wait_requires_new_sequence_and_keeps_original_party() {
    let mut prompts = 0;
    let mut polls = 0;
    let mut before_wait = None;
    let (fixture, requests, output) = m839_cli_exchange(
        &[
            "agent",
            "prompt",
            "worker",
            "--wait",
            "--timeout",
            "1500",
            "body",
        ],
        |request, base| {
            if request["method"] == "ping" {
                if prompts == 1 {
                    before_wait = Some(m839_delivery_rows(&base.join("cli-sqlite/zynk.db")));
                }
                return m839_pong();
            }
            if request["method"] == "agent.prompt" {
                prompts += 1;
                let mut agent = m839_agent_json("idle", "worker", "term_original", 7);
                agent["agent_session"] = m839_session("submission-session-B");
                return serde_json::json!({"result":{"type":"agent_prompted", "agent":agent, "baseline_state_change_seq":7}});
            }
            assert_eq!(request["method"], "agent.get");
            if prompts == 0 {
                fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                let mut agent = m839_agent_json("idle", "worker", "term_original", 5);
                agent["agent_session"] = m839_session("resolution-session-A");
                return m839_agent_reply(agent);
            }
            polls += 1;
            assert!(polls <= 2);
            assert_eq!(request["params"]["target"], "term_original");
            let mut agent = m839_agent_json(
                "blocked",
                "worker",
                "term_original",
                if polls == 1 { 7 } else { 8 },
            );
            agent["agent_session"] = serde_json::json!({"agent":"codex", "source":"later-hook", "kind":"id", "value":"later-session"});
            m839_agent_reply(agent)
        },
    );
    assert_eq!((prompts, polls), (1, 2));
    assert_eq!(requests.iter().filter(|r| r["method"] == "ping").count(), 3);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"], "ok");
    assert_eq!(value["delivery_status"], "submitted");
    assert_eq!(value["proof"]["proof_source"], "agent.prompt");
    assert_eq!(value["wait"]["result"], "ok");
    assert_eq!(value["wait"]["agent"]["state_change_seq"], 8);
    assert_eq!(value["wait"]["agent"]["agent_status"], "blocked");
    assert_eq!(
        value["to"],
        m839_original_party(Some("resolution-session-A"))
    );
    let messages = m839_message_rows(&fixture.base.join("cli-sqlite/zynk.db"));
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["agent"], "codex");
    assert_eq!(messages[0]["terminal_id"], "term_original");
    assert_eq!(messages[0]["session_source"], "fixture-hook");
    assert_eq!(messages[0]["session_kind"], "id");
    assert_eq!(messages[0]["session_value"], "resolution-session-A");
    let events = m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db"));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "submitted");
    assert_eq!(events[0]["proof_source"], "agent.prompt");
    assert_eq!(events[0]["payload"]["terminal_id"], "term_original");
    assert_eq!(events[0]["message_id"], value["message_id"]);
    assert_eq!(events[0]["timestamp"], value["submitted_at"]);
    assert_eq!(before_wait.unwrap(), events);
}

#[test]
fn m839c_prompt_known_submit_append_failure_never_waits_or_records_failed() {
    use sqlx::{Connection, Executor};
    let mut prompts = 0;
    let mut gets = 0;
    let (fixture, requests, output) = m839_cli_exchange(
        &["agent", "prompt", "worker", "--wait", "body"],
        |request, base| {
            if request["method"] == "ping" {
                return m839_pong();
            }
            if request["method"] == "agent.get" {
                gets += 1;
                assert_eq!(gets, 1, "must not wait after append failure");
                fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                return m839_agent_reply(m839_agent_json("idle", "worker", "term_original", 7));
            }
            assert_eq!(request["method"], "agent.prompt");
            prompts += 1;
            assert_eq!(prompts, 1);
            sqlite_block_on(async {
                let options = sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(base.join("cli-sqlite/zynk.db"))
                    .busy_timeout(Duration::from_millis(200));
                let mut conn = sqlx::SqliteConnection::connect_with(&options)
                    .await
                    .unwrap();
                conn.execute("CREATE TRIGGER m839_reject_submit BEFORE INSERT ON delivery_events WHEN NEW.event_type='submitted' BEGIN SELECT RAISE(ABORT, 'm839 intentional append refusal'); END;").await.unwrap();
                conn.close().await.unwrap();
            });
            serde_json::json!({"result":{"type":"agent_prompted", "agent":m839_agent_json("idle", "worker", "term_original", 7), "baseline_state_change_seq":7}})
        },
    );
    assert_eq!((prompts, gets), (1, 1));
    assert_eq!(requests.len(), 4);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"], "failed");
    assert_eq!(value["error"]["code"], "delivery_event_persist_failed");
    assert_eq!(
        value["error"]["context"]["transport_effect"],
        "submitted_unrecorded"
    );
    assert!(value["next"].as_str().unwrap().contains("do not resubmit"));
    assert!(value.get("wait").is_none());
    assert!(value.get("delivery_status").is_none());
    assert_eq!(
        m839_message_rows(&fixture.base.join("cli-sqlite/zynk.db")).len(),
        1
    );
    assert!(m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db")).is_empty());
}

#[test]
fn m839c_prompt_unverified_response_never_claims_submitted_or_waits() {
    for case in [
        "id",
        "type",
        "name",
        "terminal",
        "baseline_missing",
        "baseline_type",
        "lost",
    ] {
        let mut gets = 0;
        let mut prompts = 0;
        let (fixture, requests, output) = m839_cli_exchange(
            &["agent", "prompt", "worker", "--wait", "body"],
            |request, base| {
                if request["method"] == "ping" {
                    return m839_pong();
                }
                if request["method"] == "agent.get" {
                    gets += 1;
                    assert_eq!(gets, 1, "no wait or re-resolution: {case}");
                    fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                    return m839_agent_reply(m839_agent_json("idle", "worker", "term_original", 5));
                }
                assert_eq!(request["method"], "agent.prompt");
                prompts += 1;
                assert_eq!(prompts, 1);
                assert_eq!(request["params"]["expected_terminal_id"], "term_original");
                let mut response = serde_json::json!({"result":{"type":"agent_prompted", "agent":m839_agent_json("idle", "worker", "term_original", 7), "baseline_state_change_seq":7}});
                match case {
                    "id" => {
                        response["id"] = serde_json::json!("wrong");
                        return response.to_string().into();
                    }
                    "type" => response["result"]["type"] = serde_json::json!("agent_info"),
                    "name" => {
                        response["result"]["agent"]["name"] = serde_json::json!("replacement")
                    }
                    "terminal" => {
                        response["result"]["agent"]["terminal_id"] = serde_json::json!("term_new")
                    }
                    "baseline_missing" => {
                        response["result"]
                            .as_object_mut()
                            .unwrap()
                            .remove("baseline_state_change_seq");
                    }
                    "baseline_type" => {
                        response["result"]["baseline_state_change_seq"] = serde_json::json!("7")
                    }
                    "lost" => return serde_json::Value::Null,
                    _ => unreachable!(),
                }
                response
            },
        );
        assert_eq!((gets, prompts), (1, 1));
        assert_eq!(requests.len(), 4);
        assert_eq!(output.status.code(), Some(1), "{case}: {output:?}");
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["result"], "failed");
        assert_eq!(
            value["error"]["code"],
            if case == "lost" {
                "transport_failed"
            } else {
                "invalid_response"
            }
        );
        assert_eq!(
            value["error"]["context"]["transport_effect"],
            "submission_unverified"
        );
        assert_eq!(value["to"], m839_original_party(None));
        assert!(value.get("delivery_status").is_none());
        assert!(value.get("submitted_at").is_none());
        assert!(value.get("wait").is_none());
        assert!(value["next"].as_str().unwrap().contains("do not resubmit"));
        let events = m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event_type"], "failed");
        assert_eq!(events[0]["proof_source"], "agent.prompt");
        assert_eq!(events[0]["message_id"], value["message_id"]);
        assert_eq!(
            m839_message_rows(&fixture.base.join("cli-sqlite/zynk.db")).len(),
            1
        );
    }

    for case in ["both", "malformed"] {
        let mut gets = 0;
        let (fixture, requests, output) = m839_cli_exchange(
            &["agent", "prompt", "worker", "--wait", "body"],
            |request, base| {
                if request["method"] == "ping" {
                    return m839_pong();
                }
                if request["method"] == "agent.get" {
                    gets += 1;
                    assert_eq!(gets, 1, "no wait after unverified response: {case}");
                    fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                    return m839_agent_reply(m839_agent_json("idle", "worker", "term_original", 7));
                }
                assert_eq!(request["method"], "agent.prompt");
                if case == "malformed" {
                    return serde_json::Value::String("{malformed".into());
                }
                serde_json::json!({
                    "result":{"type":"agent_prompted", "agent":m839_agent_json("idle", "worker", "term_original", 7), "baseline_state_change_seq":7},
                    "error":{"code":"permission_denied", "message":"contradictory refusal"}
                })
            },
        );
        assert_eq!(output.status.code(), Some(1), "{case}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "invalid_response", "{case}");
        assert_eq!(
            value["error"]["context"]["transport_effect"],
            "submission_unverified"
        );
        assert!(value["next"].as_str().unwrap().contains("do not resubmit"));
        assert!(value.get("delivery_status").is_none());
        assert!(value.get("wait").is_none());
        assert_eq!(value["to"], m839_original_party(None));
        assert_eq!(requests.len(), 4);
        let events = m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event_type"], "failed");
        assert_eq!(events[0]["proof_source"], "agent.prompt");
    }
}

#[test]
fn m839c_prompt_precondition_refusal_and_unresolved_transport_stay_distinct() {
    let mut gets = 0;
    let (fixture, requests, output) = m839_cli_exchange(
        &["agent", "prompt", "worker", "--wait", "body"],
        |request, base| {
            if request["method"] == "ping" {
                return m839_pong();
            }
            if request["method"] == "agent.get" {
                gets += 1;
                assert_eq!(gets, 1);
                fs::write(base.join("runtime.id"), "rt_m839\n").unwrap();
                return m839_agent_reply(m839_agent_json("idle", "worker", "term_original", 5));
            }
            assert_eq!(request["method"], "agent.prompt");
            assert_eq!(request["params"]["expected_terminal_id"], "term_original");
            serde_json::json!({"error":{"code":"agent_target_changed", "message":"terminal precondition refused"}})
        },
    );
    assert_eq!(requests.len(), 4);
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "agent_target_changed");
    assert_eq!(value["to"], m839_original_party(None));
    assert!(value.get("delivery_status").is_none());
    assert!(value.get("wait").is_none());
    let events = m839_delivery_rows(&fixture.base.join("cli-sqlite/zynk.db"));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "failed");
    assert_eq!(events[0]["proof_source"], "agent.prompt");

    for mismatch in [false, true] {
        let (fixture, requests, output) =
            m839_cli_exchange(&["agent", "prompt", "worker", "body"], |request, _| {
                assert_eq!(request["method"], "ping");
                if !mismatch {
                    return serde_json::Value::Null;
                }
                let mut pong = m839_pong();
                pong["result"]["protocol"] = serde_json::json!(20);
                pong
            });
        assert_eq!(requests.len(), 1);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["result"], "failed");
        assert_eq!(value["error"]["code"], "transport_failed");
        assert!(value["error"].get("context").is_none());
        assert_eq!(value["target_resolution"], "unknown");
        assert_eq!(value["to"], serde_json::json!({}));
        fixture.assert_no_runtime_created();
    }
}

fn m839_named_table_counts(db: &Path) -> Vec<(String, i64)> {
    use sqlx::Connection;
    sqlite_block_on(async {
        tokio::time::timeout(Duration::from_secs(1), async {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(db)
                .read_only(true)
                .busy_timeout(Duration::from_millis(200));
            let mut connection = sqlx::SqliteConnection::connect_with(&options)
                .await
                .unwrap();
            let mut counts = Vec::new();
            for table in [
                "conversations",
                "conversation_participants",
                "messages",
                "delivery_events",
                "embedding_models",
                "embedding_jobs",
                "message_embeddings",
                "_sqlx_migrations",
            ] {
                let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&mut connection)
                    .await
                    .unwrap();
                counts.push((table.into(), count));
            }
            connection.close().await.unwrap();
            counts
        })
        .await
        .expect("bounded m839 named-table snapshot")
    })
}

fn m839_seed_aged_read_orphan(db: &Path) {
    use sqlx::{Connection, Executor};
    sqlite_block_on(async {
        tokio::time::timeout(Duration::from_secs(1), async {
            let options = sqlx::sqlite::SqliteConnectOptions::new().filename(db).create_if_missing(false).busy_timeout(Duration::from_millis(200));
            let mut connection = sqlx::SqliteConnection::connect_with(&options).await.unwrap();
            connection.execute("INSERT INTO conversations (id,runtime_session_id,socket_namespace,workspace_id,tab_id,created_at,last_message_at) VALUES ('m839-read-c','rt','ns','w','t','2000-01-01T00:00:00Z','2000-01-01T00:00:00Z')").await.unwrap();
            for id in ["m839-read-from", "m839-read-to"] {
                sqlx::query("INSERT INTO conversation_participants (id,conversation_id,agent_label,participant_key,joined_at) VALUES (?,'m839-read-c',?,?,'2000-01-01T00:00:00Z')")
                    .bind(id).bind(id).bind(id).execute(&mut connection).await.unwrap();
            }
            connection.execute("INSERT INTO messages (id,conversation_id,conversation_seq,runtime_session_id,socket_namespace,created_at,target_arg,from_participant_id,to_participant_id,body,body_hash,workspace_id,tab_id) VALUES ('m839-read-orphan','m839-read-c',1,'rt','ns','2000-01-01T00:00:00Z','worker','m839-read-from','m839-read-to','body','hash','w','t')").await.unwrap();
            connection.close().await.unwrap();
        }).await.expect("bounded m839 orphan seed");
    });
}

#[test]
fn m839c_real_agent_reads_with_managed_state_add_no_persistence_rows() {
    use std::os::unix::fs::PermissionsExt;
    let base = unique_test_dir();
    fs::create_dir_all(base.join("bin")).unwrap();
    let shell = base.join("shell");
    fs::write(&shell, b"#!/bin/sh\nroot=${0%/*}\nexport HOME=\"$root\" PATH=\"$root/bin\" ZYNK_AGENT= ENV=/dev/null BASH_ENV=/dev/null INPUTRC=/dev/null PROMPT_COMMAND= PS1= PS2=\nexec /bin/bash --noprofile --norc --noediting -i\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    let agent = base.join("bin/codex");
    fs::copy("/bin/cat", &agent).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket = runtime_dir.join("zynk.sock");
    let config = toml::to_string(&serde_json::json!({"onboarding":false, "terminal":{"default_shell":shell, "shell_mode":"non_login"}})).unwrap();
    let zynk = spawn_zynk_with_config(
        &config_home,
        &runtime_dir,
        &socket,
        Some(&base.join("bin")),
        &config,
    );
    wait_for_socket(&socket, Duration::from_secs(5));
    let created = m837_exchange(
        &socket,
        serde_json::json!({"id":"m839:create", "method":"workspace.create", "params":{"cwd":base, "focus":true}}),
    );
    let pane = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    let ready = m837_exchange(
        &socket,
        serde_json::json!({"id":"m839:ready", "method":"pane.send_input", "params":{"pane_id":pane, "text":"printf '%s' ready > \"$HOME/m839-ready\"", "keys":["Enter"]}}),
    );
    assert_eq!(ready["result"]["type"], "ok");
    let deadline = Instant::now() + Duration::from_secs(3);
    while fs::read(base.join("m839-ready")).ok().as_deref() != Some(b"ready".as_slice()) {
        assert!(
            Instant::now() < deadline,
            "isolated shell did not initialize"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let started = m837_exchange(
        &socket,
        serde_json::json!({"id":"m839:start", "method":"agent.start", "params":{"name":"worker", "kind":"codex", "pane_id":pane, "timeout_ms":30000}}),
    );
    assert_eq!(started["result"]["type"], "agent_started", "{started}");
    let terminal = started["result"]["agent"]["terminal_id"].as_str().unwrap();
    let db = config_home.join("sqlite/zynk.db");
    m839_seed_aged_read_orphan(&db);
    let before = m839_named_table_counts(&db);
    let deliveries = m839_delivery_rows(&db);
    assert!(
        deliveries.is_empty(),
        "fixture orphan must have no delivery event: {deliveries:?}"
    );
    for method in ["agent.get", "agent.list", "agent.get", "agent.list"] {
        let params = if method == "agent.get" {
            serde_json::json!({"target":terminal})
        } else {
            serde_json::json!({})
        };
        let response = m837_exchange(
            &socket,
            serde_json::json!({"id":"m839:read", "method":method, "params":params}),
        );
        assert!(response.get("error").is_none(), "{response}");
        if method == "agent.get" {
            assert_eq!(response["result"]["agent"]["terminal_id"], terminal);
            assert_eq!(response["result"]["agent"]["name"], "worker");
        } else {
            assert!(response["result"]["agents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|agent| agent["terminal_id"] == terminal && agent["name"] == "worker"));
        }
        assert_eq!(m839_named_table_counts(&db), before, "{method}");
        assert_eq!(m839_delivery_rows(&db), deliveries, "{method}");
    }
    cleanup_spawned_zynk(zynk, base);
}

fn m839_managed_cli_bounded(base: &Path, socket: &Path, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zynk"))
        .args(args)
        .env_clear()
        .env("HOME", base.join("cli-home"))
        .env("XDG_CONFIG_HOME", base.join("cli-config"))
        .env("XDG_DATA_HOME", base.join("cli-data"))
        .env("XDG_CACHE_HOME", base.join("cli-cache"))
        .env("XDG_RUNTIME_DIR", base.join("cli-runtime"))
        .env("ZYNK_SQLITE_HOME", base.join("config/sqlite"))
        .env("ZYNK_SOCKET_PATH", socket)
        .current_dir(base)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    thread::scope(|scope| {
        let read = |mut pipe: Box<dyn Read + Send>| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        };
        let stdout = scope.spawn(move || read(Box::new(stdout)));
        let stderr = scope.spawn(move || read(Box::new(stderr)));
        let finished = wait_until(Duration::from_secs(12), Duration::from_millis(5), || {
            child.try_wait().unwrap().is_some()
        });
        if !finished {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        let output = std::process::Output {
            status,
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        };
        assert!(
            finished,
            "m839 managed CLI exceeded 12s; direct child reaped"
        );
        output
    })
}
