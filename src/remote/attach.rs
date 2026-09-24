// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
//! Remote thin-client launcher over SSH command stdio.

use std::fs::{self, File};
use std::io::{self, IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::ListenerNonblockingMode;
use interprocess::TryClone as _;
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BRIDGE_ACCEPT_POLL: Duration = Duration::from_millis(50);
const BRIDGE_SOCKET_PERMISSION_MODE: u32 = 0o600;
const REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CURRENT_PROTOCOL: u32 = crate::protocol::PROTOCOL_VERSION;
const REMOTE_BINARY_ENV_VAR: &str = "ZYNK_REMOTE_BINARY";
pub(crate) const REATTACH_COMMAND_ENV_VAR: &str = "ZYNK_REATTACH_COMMAND";

pub(crate) const REMOTE_KEYBINDINGS_ENV_VAR: &str = "ZYNK_REMOTE_KEYBINDINGS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteKeybindings {
    Local,
    Server,
}

impl RemoteKeybindings {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local" => Ok(Self::Local),
            "server" => Ok(Self::Server),
            _ => Err("--remote-keybindings must be 'local' or 'server'".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunch {
    pub(crate) target: String,
    pub(crate) keybindings: RemoteKeybindings,
    pub(crate) live_handoff: bool,
}

pub(crate) fn extract_remote_args(
    args: &[String],
) -> Result<(Vec<String>, Option<RemoteLaunch>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut remote_target = None;
    let mut keybindings = RemoteKeybindings::Local;
    let mut keybindings_seen = false;
    let mut live_handoff = false;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        if arg == "--handoff" {
            live_handoff = true;
            index += 1;
            continue;
        }
        if arg == "--remote" {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote".to_string());
            };
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote=") {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 1;
            continue;
        }
        if arg == "--remote-keybindings" {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote-keybindings".to_string());
            };
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote-keybindings=") {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 1;
            continue;
        }

        cleaned.push(arg.clone());
        index += 1;
    }

    let remote = remote_target.map(|target| RemoteLaunch {
        target,
        keybindings,
        live_handoff,
    });
    if remote.is_none() && keybindings_seen {
        return Err("--remote-keybindings requires --remote".to_string());
    }
    if remote.is_none() && live_handoff {
        cleaned.push("--handoff".to_string());
    }

    Ok((cleaned, remote))
}

fn validate_remote_target(target: &str) -> Result<&str, String> {
    if target.is_empty() {
        return Err("missing value for --remote".to_string());
    }
    if target.starts_with('-') {
        return Err("--remote target must not start with '-'".to_string());
    }
    Ok(target)
}

pub(crate) fn run_remote(remote: RemoteLaunch) -> io::Result<()> {
    let session_name = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    let local_socket = local_forward_socket_path(&remote.target, &session_name);
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "zynk".to_string());
    let reattach_command = reattach_command(
        &program,
        &remote.target,
        &session_name,
        remote.keybindings,
        remote.live_handoff,
    );
    let manage_ssh_config = crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config;
    let remote_ssh = RemoteSsh::new(remote.target.clone(), manage_ssh_config);
    let prepared_remote = prepare_remote_zynk(&remote_ssh, remote.live_handoff)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_zynk,
        prepared_remote.installed_or_replaced,
        prepared_remote.stop_after_install_approved,
        remote.live_handoff,
    )?;

    let _bridge = SshStdioBridge::start(
        remote.target,
        prepared_remote.remote_zynk,
        local_socket.clone(),
        session_name,
        remote_ssh.options().cloned(),
    )?;

    run_client_process(&local_socket, &reattach_command, remote.keybindings)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
    os: &'static str,
    arch: &'static str,
}

impl RemotePlatform {
    /// ADR 0013: zynk is Linux x86_64 only, so a remote host is supported only when its `uname`
    /// says exactly that. Every other operating system and architecture is refused here rather
    /// than mapped to a platform zynk never builds for.
    fn from_uname(os: &str, arch: &str) -> Option<Self> {
        let os = match os.trim() {
            "Linux" => "linux",
            _ => return None,
        };
        let arch = match arch.trim() {
            "x86_64" | "amd64" => "x86_64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    /// The local platform. ADR 0013 makes any other `target_os`/`target_arch` a `compile_error!`,
    /// so this build can only ever be Linux x86_64.
    fn local() -> Self {
        Self {
            os: "linux",
            arch: "x86_64",
        }
    }

    fn platform_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

#[derive(Debug, Clone)]
struct RemoteZynk {
    install_suffix: String,
    shell_path: String,
    platform: RemotePlatform,
    expected_sha256: Option<String>,
}

impl RemoteZynk {
    fn for_platform(platform: RemotePlatform) -> Self {
        let install_suffix = ".local/bin/zynk".to_string();
        let shell_path = format!("\"$HOME/{install_suffix}\"");
        Self {
            install_suffix,
            shell_path,
            platform,
            expected_sha256: None,
        }
    }

    fn with_shell_path(mut self, shell_path: String) -> Self {
        self.shell_path = shell_path;
        self
    }

    fn with_custody(mut self, custody: &InstallCustody) -> Self {
        self.expected_sha256 = Some(custody.sha256.clone());
        self
    }
}

fn current_version() -> String {
    crate::build_info::version()
}

/// Where the binary copied to the remote host comes from. ADR 0013 leaves exactly two sources —
/// the running executable and `ZYNK_REMOTE_BINARY` — so there is nothing temporary to clean up.
struct InstallSource {
    path: PathBuf,
}

/// What a remote binary must be able to prove about itself before this client will run it (ADR 0013
/// Decision 3): the exact bytes of the reviewed local binary, the source commit those bytes attest,
/// and the version and protocol the local end is about to speak to it over.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallCustody {
    sha256: String,
    build_sha: String,
    version: String,
    protocol: u32,
}

/// Why a remote binary is not the reviewed one. A reuse decision turns this into "install the
/// reviewed binary instead"; the post-copy check turns it into a hard failure. One comparator, two
/// consequences, so the two paths cannot drift apart about what custody means.
#[derive(Debug)]
struct CustodyRefusal(String);

impl CustodyRefusal {
    fn reason(&self) -> &str {
        &self.0
    }

    fn into_error(self) -> io::Error {
        io::Error::other(self.0)
    }
}

struct PreparedRemoteZynk {
    remote_zynk: RemoteZynk,
    installed_or_replaced: bool,
    stop_after_install_approved: bool,
}

#[derive(Clone)]
struct ManagedSshOptions {
    config_path: PathBuf,
    control_path: PathBuf,
}

struct ManagedSshConfig {
    options: ManagedSshOptions,
}

impl Drop for ManagedSshConfig {
    fn drop(&mut self) {
        if let Some(dir) = self.options.config_path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

struct RemoteSsh {
    target: String,
    managed_config: Option<ManagedSshConfig>,
}

impl RemoteSsh {
    fn new(target: String, manage_ssh_config: bool) -> Self {
        let managed_config = if manage_ssh_config {
            write_managed_ssh_config()
                .inspect_err(|err| {
                    tracing::debug!(%err, "could not write managed ssh config; using plain ssh");
                })
                .ok()
        } else {
            None
        };
        Self {
            target,
            managed_config,
        }
    }

    fn options(&self) -> Option<&ManagedSshOptions> {
        self.managed_config.as_ref().map(|config| &config.options)
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new("ssh");
        apply_managed_ssh_options(&mut command, self.options());
        command
    }
}

impl Drop for RemoteSsh {
    fn drop(&mut self) {
        if self.managed_config.is_none() {
            return;
        }
        let _ = self
            .base_command()
            .arg("-O")
            .arg("exit")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(&self.target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

trait RemoteSshConnection {
    fn target(&self) -> &str;

    fn options(&self) -> Option<&ManagedSshOptions> {
        None
    }

    fn command(&self) -> Command {
        let mut command = Command::new("ssh");
        apply_managed_ssh_options(&mut command, self.options());
        command.arg("-T").arg(self.target());
        command
    }

    fn sh_output(&self, script: &str) -> io::Result<Output> {
        ssh_sh_output(self, script)
    }

    fn user_shell_output(&self, command: &str) -> io::Result<Output> {
        self.command().arg(command).output()
    }

    fn install_zynk(&self, remote_zynk: &RemoteZynk, source_path: &Path) -> io::Result<()> {
        let output = self.sh_output(&remote_install_prepare_script(remote_zynk))?;
        if !output.status.success() {
            return Err(command_failed("remote install preparation failed", &output));
        }
        let (tmp_path, dest_path) = parse_remote_install_paths(&output.stdout)?;

        let mut source = match File::open(source_path) {
            Ok(source) => source,
            Err(err) => {
                let _ = self.sh_output(&remote_install_abort_script(&tmp_path));
                return Err(err);
            }
        };
        let mut child = self
            .command()
            .arg(remote_install_stream_command(&tmp_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| {
                let _ = self.sh_output(&remote_install_abort_script(&tmp_path));
                io::Error::new(err.kind(), format!("failed to start ssh install: {err}"))
            })?;
        let copy_result = if let Some(mut stdin) = child.stdin.take() {
            io::copy(&mut source, &mut stdin).map(|_| ())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh install stdin missing",
            ))
        };
        let status = match child.wait() {
            Ok(status) => status,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = self.sh_output(&remote_install_abort_script(&tmp_path));
                return Err(err);
            }
        };
        if let Err(err) = copy_result {
            let _ = self.sh_output(&remote_install_abort_script(&tmp_path));
            return Err(err);
        }
        if !status.success() {
            let _ = self.sh_output(&remote_install_abort_script(&tmp_path));
            return Err(io::Error::other(format!(
                "remote install exited with {status}"
            )));
        }

        let output = self.sh_output(&remote_install_commit_script(&tmp_path, &dest_path))?;
        if output.status.success() {
            Ok(())
        } else {
            let err = command_failed("remote install commit failed", &output);
            let _ = self.sh_output(&remote_install_abort_script(&tmp_path));
            Err(err)
        }
    }
}

impl RemoteSshConnection for RemoteSsh {
    fn target(&self) -> &str {
        &self.target
    }

    fn options(&self) -> Option<&ManagedSshOptions> {
        self.options()
    }
}

impl RemoteSshConnection for str {
    fn target(&self) -> &str {
        self
    }
}

fn apply_managed_ssh_options(command: &mut Command, options: Option<&ManagedSshOptions>) {
    let Some(options) = options else {
        return;
    };
    command
        .arg("-F")
        .arg(&options.config_path)
        .arg("-S")
        .arg(&options.control_path)
        .arg("-o")
        .arg("ControlMaster=auto")
        .arg("-o")
        .arg("ControlPersist=yes");
}

impl InstallSource {
    fn persistent(path: PathBuf) -> Self {
        Self { path }
    }
}

fn prepare_remote_zynk(
    ssh: &(impl RemoteSshConnection + ?Sized),
    live_handoff_enabled: bool,
) -> io::Result<PreparedRemoteZynk> {
    let platform = detect_remote_platform(ssh)?;
    let remote_zynk = RemoteZynk::for_platform(platform);
    let override_binary = remote_binary_override_path()?;
    let remote_binary_candidates = remote_binary_candidates(ssh, &remote_zynk)?;

    // ADR 0013 Decision 3: reusing a binary that is already on the remote host is as much a
    // decision to RUN it as installing one is, so the reviewed local binary is read before either
    // decision. A local build that cannot attest a clean, well-formed source commit authorises
    // neither — it cannot say what the far end would have to match.
    let custody_source = custody_source_path(override_binary.as_deref())?;
    let custody = local_install_custody(&custody_source)?;

    if override_binary.is_none() {
        for candidate in &remote_binary_candidates {
            if remote_binary_is_the_reviewed_one(ssh, candidate, &custody).unwrap_or(false) {
                return Ok(PreparedRemoteZynk {
                    remote_zynk: candidate.clone().with_custody(&custody),
                    installed_or_replaced: false,
                    stop_after_install_approved: false,
                });
            }
        }
        if remote_binary_is_the_reviewed_one(ssh, &remote_zynk, &custody)? {
            return Ok(PreparedRemoteZynk {
                remote_zynk: remote_zynk.with_custody(&custody),
                installed_or_replaced: false,
                stop_after_install_approved: false,
            });
        }
    }

    let mut stop_after_install_approved = false;
    if let Some(status_probe_zynk) = remote_binary_candidates.first().or_else(|| {
        remote_binary_exists(ssh, &remote_zynk)
            .ok()
            .and_then(|exists| exists.then_some(&remote_zynk))
    }) {
        stop_after_install_approved = confirm_remote_install_with_running_server(
            ssh,
            status_probe_zynk,
            live_handoff_enabled,
        )?;
    }
    confirm_remote_install(
        ssh,
        &remote_zynk,
        &install_source_description(&remote_zynk.platform, override_binary.as_deref()),
    )?;
    let source = resolve_install_source(&remote_zynk.platform, override_binary)?;
    if source.path != custody_source {
        return Err(io::Error::other(format!(
            "the install source {} is not the binary custody was taken of ({}), so nothing has attested the file about to be copied (ADR 0013 custody)",
            source.path.display(),
            custody_source.display()
        )));
    }
    install_remote_zynk(ssh, &remote_zynk, &source.path)?;
    // The same comparator the reuse decision uses, over the same single round trip: bytes, source
    // commit, version and protocol. The version-only re-check this replaces was both weaker than
    // custody and a second probe, so a binary swapped between the two could pass the pair.
    verify_remote_custody(ssh, &remote_zynk, &custody)?;
    warn_if_remote_bin_not_on_path(ssh)?;

    Ok(PreparedRemoteZynk {
        remote_zynk: remote_zynk.with_custody(&custody),
        installed_or_replaced: true,
        stop_after_install_approved,
    })
}

fn detect_remote_platform(ssh: &(impl RemoteSshConnection + ?Sized)) -> io::Result<RemotePlatform> {
    let output = ssh.sh_output("uname -s\nuname -m\n")?;
    if !output.status.success() {
        return Err(command_failed("remote platform detection failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    RemotePlatform::from_uname(os, arch).ok_or_else(|| unsupported_remote_platform_error(os, arch))
}

/// ADR 0013: the refusal has to say what zynk actually supports, and where that was decided.
fn unsupported_remote_platform_error(os: &str, arch: &str) -> io::Error {
    io::Error::other(format!(
        "unsupported remote platform: {} {} — zynk supports Linux x86_64 only (docs/zynk/decisions/0013-linux-only-platform-scope.md). Install zynk on the remote host from source, or attach to a Linux x86_64 host.",
        os.trim(),
        arch.trim()
    ))
}

fn remote_binary_on_path_any(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
) -> io::Result<Option<RemoteZynk>> {
    let output = ssh.user_shell_output("command -v zynk")?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(candidate) = remote_zynk_from_path_discovery(remote_zynk, &stdout) {
            return Ok(Some(candidate));
        }
    }

    // Non-POSIX login shells such as xonsh reject `command -v`; retry through
    // /bin/sh while retaining the login-shell probe for shell-initialized PATHs.
    let output = ssh.sh_output("command -v zynk\n")?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(remote_zynk_from_path_discovery(remote_zynk, &stdout))
}

fn remote_binary_candidates(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
) -> io::Result<Vec<RemoteZynk>> {
    let mut candidates = Vec::new();
    if let Some(candidate) = remote_binary_on_path_any(ssh, remote_zynk)? {
        push_if_new_remote_binary_candidate(&mut candidates, candidate);
    }

    let output = ssh.sh_output(&known_remote_binary_candidate_script(&remote_zynk.platform))?;
    if !output.status.success() {
        return Err(command_failed("remote binary discovery failed", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for candidate in remote_zynks_from_path_discovery(remote_zynk, &stdout) {
        push_if_new_remote_binary_candidate(&mut candidates, candidate);
    }
    Ok(candidates)
}

fn push_if_new_remote_binary_candidate(candidates: &mut Vec<RemoteZynk>, candidate: RemoteZynk) {
    if !candidates
        .iter()
        .any(|existing| existing.shell_path == candidate.shell_path)
    {
        candidates.push(candidate);
    }
}

fn known_remote_binary_candidate_script(_platform: &RemotePlatform) -> String {
    let mut script = String::from(
        r#"home=${HOME:-}
user=${USER:-}
version="#,
    );
    script.push_str(&shell_quote(&current_version()));
    script.push_str(
        r#"
emit() {
    path=$1
    if [ -n "$path" ] && [ -x "$path" ]; then
        printf '%s\n' "$path"
    fi
}
if [ -n "$home" ]; then
    emit "$home/.local/bin/zynk"
fi
emit "/home/linuxbrew/.linuxbrew/bin/zynk"
if [ -n "$home" ]; then
    emit "$home/.local/share/mise/installs/zynk/$version/bin/zynk"
    emit "$home/.local/share/mise/installs/zynk/$version/zynk"
    emit "$home/.nix-profile/bin/zynk"
fi
if [ -n "$user" ]; then
    emit "/etc/profiles/per-user/$user/bin/zynk"
fi
emit "/nix/var/nix/profiles/default/bin/zynk"
emit "/run/current-system/sw/bin/zynk"
"#,
    );
    script
}

fn remote_zynks_from_path_discovery(remote_zynk: &RemoteZynk, stdout: &str) -> Vec<RemoteZynk> {
    stdout
        .lines()
        .filter_map(|path| remote_zynk_from_path(remote_zynk, path))
        .collect()
}

fn remote_zynk_from_path_discovery(remote_zynk: &RemoteZynk, stdout: &str) -> Option<RemoteZynk> {
    stdout
        .lines()
        .find_map(|path| remote_zynk_from_path(remote_zynk, path))
}

fn remote_zynk_from_path(remote_zynk: &RemoteZynk, path: &str) -> Option<RemoteZynk> {
    let path = path.trim();
    if !path.starts_with('/') {
        return None;
    }
    if path.ends_with("/mise/shims/zynk") {
        return None;
    }
    Some(remote_zynk.clone().with_shell_path(shell_quote(path)))
}

/// Open fd 3 INSIDE the remote shell, hash it, and run every command through that
/// same descriptor. Atomic pathname replacement cannot change the inode selected.
/// This trusts the SSH account/kernel and tools, not a hostile in-place writer.
fn remote_executable_script(remote_zynk: &RemoteZynk, sha256: &str, body: &str) -> String {
    format!(
        r#"test -d /proc/self/fd || {{ printf '%s\n' 'ADR 0013 remote custody requires mounted procfs with /proc/self/fd access' >&2; exit 78; }}
exec 3<{path}
test -f /proc/self/fd/3 && test -x /proc/self/fd/3 || {{ printf '%s\n' 'ADR 0013 remote custody requires an executable regular file accessible through /proc/self/fd/3' >&2; exit 78; }}
actual=$(sha256sum /proc/self/fd/3) || {{ printf '%s\n' 'ADR 0013 remote custody: sha256sum failed; executable not run' >&2; exit 78; }}
actual=${{actual%% *}}
test "$actual" = {sha256} || {{ printf '%s\n' 'ADR 0013 remote custody: executable bytes changed; refusing execution' >&2; exit 1; }}
{body}
"#,
        path = remote_zynk.shell_path,
        sha256 = shell_quote(sha256),
    )
}

fn prepared_remote_script(remote_zynk: &RemoteZynk, body: &str) -> io::Result<String> {
    let sha256 = remote_zynk
        .expected_sha256
        .as_deref()
        .ok_or_else(|| io::Error::other("remote executable has no prepared custody (ADR 0013)"))?;
    Ok(remote_executable_script(remote_zynk, sha256, body))
}

/// One remote round trip, with bytes and metadata bound to the descriptor opened
/// there. The hash is checked BEFORE executing even the version/status queries.
fn remote_custody_probe(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
    custody: &InstallCustody,
) -> io::Result<Output> {
    let command = remote_executable_script(remote_zynk, &custody.sha256,
        "printf '%s\\n' \"$actual\" && /proc/self/fd/3 --version && /proc/self/fd/3 status client --json");
    ssh.sh_output(&command)
}

/// Reuse is custody-gated exactly as an install is (ADR 0013 Decision 3): a binary already on the
/// remote host is used only when it is the same bytes, from the same reviewed commit, at the version
/// and protocol this client speaks. A refusal here is not fatal — the caller falls through to the
/// confirmed install path, which copies the reviewed binary and verifies it with this same
/// comparator.
fn remote_binary_is_the_reviewed_one(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
    custody: &InstallCustody,
) -> io::Result<bool> {
    let output = remote_custody_probe(ssh, remote_zynk, custody)?;
    if !output.status.success() {
        if output.status.code() == Some(78) {
            return Err(command_failed(
                "remote executable custody prerequisite failed",
                &output,
            ));
        }
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (remote_sha256, remote_build_sha) = probed_identifiers(&stdout);
    tracing::info!(
        target = %ssh.target(),
        path = %remote_zynk.shell_path,
        local_sha256 = %custody.sha256,
        remote_sha256 = %remote_sha256,
        local_build_sha = %custody.build_sha,
        remote_build_sha = %remote_build_sha,
        "deciding whether the remote zynk binary may be reused"
    );

    match check_remote_custody(&stdout, &remote_zynk.shell_path, custody) {
        Ok(()) => Ok(true),
        Err(refusal) => {
            tracing::info!(
                target = %ssh.target(),
                path = %remote_zynk.shell_path,
                reason = %refusal.reason(),
                "not reusing the remote zynk binary; installing the reviewed one instead"
            );
            Ok(false)
        }
    }
}

/// The two identifiers a custody decision turns on, pulled out of a probe's stdout so both the reuse
/// decision and the post-copy check can log the pair they compared.
fn probed_identifiers(remote_stdout: &str) -> (String, String) {
    let mut lines = remote_stdout.lines();
    let sha256 = lines.next().unwrap_or_default().trim().to_ascii_lowercase();
    let build_sha = parse_version_line(lines.next().unwrap_or_default())
        .and_then(|(_, sha)| sha)
        .unwrap_or_else(|| "<none>".to_string());
    (sha256, build_sha)
}

/// Split a `zynk <version>` or `zynk <version> (<source sha>)` line into its parts. Returns `None`
/// for anything that is not a zynk version line at all.
fn parse_version_line(line: &str) -> Option<(String, Option<String>)> {
    let rest = line.trim().strip_prefix("zynk ")?.trim();
    let Some((version, sha)) = rest.split_once(" (") else {
        return (!rest.is_empty()).then(|| (rest.to_string(), None));
    };
    let version = version.trim();
    let sha = sha.strip_suffix(')')?.trim();
    if version.is_empty() || sha.is_empty() {
        return None;
    }
    Some((version.to_string(), Some(sha.to_string())))
}

fn remote_binary_exists(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
) -> io::Result<bool> {
    let command = format!("test -x {}", remote_zynk.shell_path);
    Ok(ssh.sh_output(&command)?.status.success())
}

fn remote_binary_override_path() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(REMOTE_BINARY_ENV_VAR) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{REMOTE_BINARY_ENV_VAR} must not be empty"),
        ));
    }

    let path = PathBuf::from(value);
    let metadata = fs::metadata(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to inspect {REMOTE_BINARY_ENV_VAR} path {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{REMOTE_BINARY_ENV_VAR} path is not a file: {}",
                path.display()
            ),
        ));
    }

    Ok(Some(path))
}

fn install_source_description(platform: &RemotePlatform, override_binary: Option<&Path>) -> String {
    install_source_description_for(
        platform,
        override_binary,
        local_binary_can_seed_remote(platform),
    )
}

fn install_source_description_for(
    platform: &RemotePlatform,
    override_binary: Option<&Path>,
    local_binary_can_seed_remote: bool,
) -> String {
    if let Some(path) = override_binary {
        return format!("{REMOTE_BINARY_ENV_VAR} ({})", path.display());
    }

    if local_binary_can_seed_remote {
        "the current local zynk binary".to_string()
    } else {
        // ADR 0013: there is no release-asset download path; the only other source is an
        // explicitly named local build.
        format!(
            "no install source for {} (set {REMOTE_BINARY_ENV_VAR})",
            platform.platform_key()
        )
    }
}

/// The binary whose bytes and attested commit define what the remote must be: the explicit
/// `ZYNK_REMOTE_BINARY` when one is set, otherwise this running executable. It is the reference even
/// when it cannot itself seed the remote — a package-manager-managed install is not zynk's file to
/// copy, but it is still the reviewed binary this client speaks for, so reuse stays available to it.
fn custody_source_path(override_binary: Option<&Path>) -> io::Result<PathBuf> {
    match override_binary {
        Some(path) => Ok(path.to_path_buf()),
        None => std::env::current_exe(),
    }
}

fn resolve_install_source(
    platform: &RemotePlatform,
    override_binary: Option<PathBuf>,
) -> io::Result<InstallSource> {
    if let Some(path) = override_binary {
        return Ok(InstallSource::persistent(path));
    }

    if *platform == RemotePlatform::local() {
        let path = std::env::current_exe()?;
        if !crate::update::is_package_manager_managed_exe_path(&path) {
            return Ok(InstallSource::persistent(path));
        }
    }

    // ADR 0013: zynk ships no release assets, so there is nothing to download. The remote is
    // seeded from a reviewed local build or not at all.
    Err(io::Error::other(format!(
        "no install source for a {} remote: this build is managed by a package manager, so its file is not zynk's to copy, and zynk has no release-asset download path (docs/zynk/decisions/0013-linux-only-platform-scope.md). Set {REMOTE_BINARY_ENV_VAR}=path/to/zynk (a Linux x86_64 build of the same source commit) or install zynk on the remote host from source.",
        platform.platform_key()
    )))
}

fn local_binary_can_seed_remote(platform: &RemotePlatform) -> bool {
    if *platform != RemotePlatform::local() {
        return false;
    }

    std::env::current_exe()
        .map(|path| !crate::update::is_package_manager_managed_exe_path(&path))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteServerStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
        live_handoff: bool,
        detached_server_daemon: bool,
    },
    NotRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteServerRestartReason {
    ProtocolMismatch,
    DaemonDetachMissing,
    BinaryUpdated,
    VersionMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteInstallRunningServerPlan {
    KeepRunning,
    LiveHandoff,
    StopRequired(RemoteServerRestartReason),
}

fn ensure_remote_server_ready(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
    remote_binary_changed: bool,
    stop_after_install_approved: bool,
    live_handoff_enabled: bool,
) -> io::Result<()> {
    let target = ssh.target();
    let status = remote_server_status(ssh, remote_zynk)?;
    let RemoteServerStatus::Running {
        version,
        protocol,
        live_handoff,
        detached_server_daemon,
    } = status
    else {
        return Ok(());
    };

    let Some(reason) = remote_server_restart_reason(
        version.as_deref(),
        protocol,
        detached_server_daemon,
        remote_binary_changed,
    ) else {
        return Ok(());
    };

    if live_handoff_enabled && live_handoff {
        match live_handoff_remote_server(ssh, remote_zynk) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("remote live handoff failed: {err}");
                eprintln!("falling back to remote server restart.");
            }
        }
    }

    if stop_after_install_approved {
        stop_remote_server(ssh, remote_zynk)?;
        return Ok(());
    }

    if confirm_remote_server_stop(target, version.as_deref(), protocol, reason)? {
        stop_remote_server(ssh, remote_zynk)?;
    }
    Ok(())
}

fn remote_server_restart_reason(
    version: Option<&str>,
    protocol: Option<u32>,
    detached_server_daemon: bool,
    remote_binary_changed: bool,
) -> Option<RemoteServerRestartReason> {
    if protocol != Some(CURRENT_PROTOCOL) {
        return Some(RemoteServerRestartReason::ProtocolMismatch);
    }
    if !detached_server_daemon {
        return Some(RemoteServerRestartReason::DaemonDetachMissing);
    }
    if version != Some(current_version().as_str()) {
        return Some(RemoteServerRestartReason::VersionMismatch);
    }
    if remote_binary_changed {
        return Some(RemoteServerRestartReason::BinaryUpdated);
    }
    None
}

fn confirm_remote_install_with_running_server(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
    live_handoff_enabled: bool,
) -> io::Result<bool> {
    let target = ssh.target();
    let status = match remote_server_status(ssh, remote_zynk) {
        Ok(status) => status,
        Err(err) => {
            if !io::stdin().is_terminal() {
                return Err(io::Error::other(format!(
                    "could not inspect the running remote zynk server on {target} before installing: {err}; run from an interactive terminal to approve updating the remote binary"
                )));
            }
            eprintln!(
                "could not inspect the running remote zynk server on {target} before installing: {err}"
            );
            eprint!("continue installing the remote zynk binary? [y/N] ");
            io::stderr().flush()?;

            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            let answer = answer.trim().to_ascii_lowercase();
            if answer != "y" && answer != "yes" {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote zynk install cancelled",
                ));
            }
            return Ok(false);
        }
    };
    let RemoteServerStatus::Running {
        version,
        protocol,
        live_handoff,
        detached_server_daemon,
    } = &status
    else {
        return Ok(false);
    };
    let plan = remote_install_running_server_plan(
        version.as_deref(),
        *protocol,
        *detached_server_daemon,
        true,
        *live_handoff,
        live_handoff_enabled,
    );

    if plan == RemoteInstallRunningServerPlan::KeepRunning {
        if io::stdin().is_terminal() {
            eprintln!("remote zynk server on {target} is already compatible:");
            eprintln!("  server: v{}", version_label(version.as_deref()));
            eprintln!(
                "Zynk will install {} without stopping the running remote server.",
                current_version()
            );
        }
        return Ok(false);
    }

    if !io::stdin().is_terminal() {
        match plan {
            RemoteInstallRunningServerPlan::LiveHandoff => return Ok(false),
            RemoteInstallRunningServerPlan::StopRequired(_) => {
                return Err(io::Error::other(format!(
                    "remote zynk server on {target} is running v{}; run from an interactive terminal to approve stopping it for the update",
                    version_label(version.as_deref())
                )));
            }
            RemoteInstallRunningServerPlan::KeepRunning => return Ok(false),
        }
    }

    if plan == RemoteInstallRunningServerPlan::LiveHandoff {
        eprintln!("remote zynk server on {target} is currently running:");
        eprintln!("  server: v{}", version_label(version.as_deref()));
        eprintln!(
            "Zynk will install {} and hand off live pane processes to the prepared server.",
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote zynk server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version.as_deref()));
    eprintln!(
        "To complete the remote update, Zynk must stop the running remote server after installing."
    );
    eprintln!("This stops active remote pane processes, including shells, dev servers, and tests.");
    eprintln!();
    eprint!(
        "Install {} and stop the remote server now? [y/N] ",
        current_version()
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote zynk install cancelled",
        ));
    }

    Ok(true)
}

fn remote_install_running_server_plan(
    version: Option<&str>,
    protocol: Option<u32>,
    detached_server_daemon: bool,
    remote_binary_changed: bool,
    live_handoff: bool,
    live_handoff_enabled: bool,
) -> RemoteInstallRunningServerPlan {
    let Some(reason) = remote_server_restart_reason(
        version,
        protocol,
        detached_server_daemon,
        remote_binary_changed,
    ) else {
        return RemoteInstallRunningServerPlan::KeepRunning;
    };

    if live_handoff_enabled && live_handoff {
        return RemoteInstallRunningServerPlan::LiveHandoff;
    }
    RemoteInstallRunningServerPlan::StopRequired(reason)
}

fn remote_server_status(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
) -> io::Result<RemoteServerStatus> {
    let command = if remote_zynk.expected_sha256.is_some() {
        prepared_remote_script(remote_zynk, "/proc/self/fd/3 status server --json")?
    } else {
        // Legacy pre-install server discovery is only a compatibility query. It
        // cannot authorize reuse; every prepared executable takes the bound path.
        format!("{} status server --json", remote_zynk.shell_path)
    };
    let output = ssh.sh_output(&command)?;
    if !output.status.success() {
        return Err(command_failed("remote server status failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_remote_server_status_json(stdout.trim())
}

#[derive(Debug, Deserialize)]
struct RemoteClientStatusJson {
    protocol: u32,
}

#[derive(Debug, Deserialize)]
struct RemoteServerStatusJson {
    running: bool,
    version: Option<String>,
    protocol: Option<u32>,
    capabilities: Option<RemoteServerCapabilitiesJson>,
}

#[derive(Debug, Deserialize)]
struct RemoteServerCapabilitiesJson {
    live_handoff: bool,
    #[serde(default)]
    detached_server_daemon: bool,
}

fn parse_client_status_json(status: &str) -> Option<RemoteClientStatusJson> {
    serde_json::from_str(status).ok()
}

fn parse_remote_server_status_json(status: &str) -> io::Result<RemoteServerStatus> {
    let parsed: RemoteServerStatusJson = serde_json::from_str(status).map_err(|err| {
        io::Error::other(format!(
            "could not parse remote server status JSON from `{status}`: {err}"
        ))
    })?;
    if !parsed.running {
        return Ok(RemoteServerStatus::NotRunning);
    }

    Ok(RemoteServerStatus::Running {
        version: parsed.version,
        protocol: parsed.protocol,
        live_handoff: parsed
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.live_handoff),
        detached_server_daemon: parsed
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.detached_server_daemon),
    })
}

fn confirm_remote_server_stop(
    target: &str,
    version: Option<&str>,
    _protocol: Option<u32>,
    reason: RemoteServerRestartReason,
) -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        if reason == RemoteServerRestartReason::ProtocolMismatch {
            return Err(io::Error::other(format!(
                "remote zynk server on {target} must stop before this client can attach; run from an interactive terminal to approve stopping it"
            )));
        }

        eprintln!(
            "remote zynk server on {target} is still running v{}; it will use {} after it restarts.",
            version_label(version),
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote zynk server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version));
    eprintln!("  prepared binary: {}", current_version());
    eprintln!();

    match reason {
        RemoteServerRestartReason::ProtocolMismatch => {
            eprintln!("the remote server must stop before this client can attach.");
        }
        RemoteServerRestartReason::DaemonDetachMissing => {
            eprintln!(
                "the remote server was started by a zynk build that may not survive SSH connection loss. restart it so network drops disconnect only this client."
            );
        }
        RemoteServerRestartReason::BinaryUpdated => {
            eprintln!(
                "the remote zynk binary was installed or replaced. restart the remote server so it uses the prepared binary."
            );
        }
        RemoteServerRestartReason::VersionMismatch => {
            eprintln!(
                "the remote server is still running a different zynk version. restart it so it uses the prepared binary."
            );
        }
    }

    let prompt = if reason == RemoteServerRestartReason::ProtocolMismatch {
        "stop the remote server and continue attaching? [Y/n] "
    } else {
        "restart the remote server now? [y/N] "
    };
    eprint!("{prompt}");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        return Ok(true);
    }
    if answer.is_empty() && reason == RemoteServerRestartReason::ProtocolMismatch {
        return Ok(true);
    }
    if reason == RemoteServerRestartReason::ProtocolMismatch {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote zynk server stop cancelled",
        ));
    }

    Ok(false)
}

fn live_handoff_remote_server(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
) -> io::Result<()> {
    let target = ssh.target();
    // Keep the SSH shell alive until the old server has opened the import image.
    // /proc/self here would name the OLD server's fd table, not our held file.
    let command = prepared_remote_script(remote_zynk, &format!(
        "/proc/self/fd/3 server live-handoff --import-exe \"/proc/$$/fd/3\" --expected-protocol {} --expected-version {}",
        CURRENT_PROTOCOL,
        shell_quote(&current_version())
    ))?;
    let output = ssh.sh_output(&command)?;
    if !output.status.success() {
        return Err(command_failed("remote server live handoff failed", &output));
    }

    eprintln!(
        "handed off the remote zynk server on {target}; reconnecting to the prepared server."
    );
    Ok(())
}

fn stop_remote_server(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
) -> io::Result<()> {
    let target = ssh.target();
    let command = prepared_remote_script(remote_zynk, "/proc/self/fd/3 server stop")?;
    let output = ssh.sh_output(&command)?;
    if !output.status.success() {
        return Err(command_failed("remote server stop failed", &output));
    }

    wait_for_remote_server_shutdown(ssh, remote_zynk)?;
    eprintln!("stopped the remote zynk server on {target}; it will restart when the remote client bridge attaches.");
    Ok(())
}

fn wait_for_remote_server_shutdown(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
) -> io::Result<()> {
    let target = ssh.target();
    let deadline = Instant::now() + REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT;
    loop {
        if remote_server_status(ssh, remote_zynk)? == RemoteServerStatus::NotRunning {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "shutdown was requested, but the old remote zynk server on {target} is still responding after {} seconds",
                    REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT.as_secs()
                ),
            ));
        }
        thread::sleep(REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL);
    }
}

fn version_label(version: Option<&str>) -> &str {
    version.unwrap_or("unknown")
}

fn warn_if_remote_bin_not_on_path(ssh: &(impl RemoteSshConnection + ?Sized)) -> io::Result<()> {
    let output = ssh.user_shell_output("command -v zynk")?;
    if output.status.success()
        && remote_shell_resolves_managed_install(&String::from_utf8_lossy(&output.stdout))
    {
        return Ok(());
    }

    eprintln!(
        "zynk: installed remote binary to ~/.local/bin/zynk, but the remote shell does not resolve `zynk` to that path"
    );
    Ok(())
}

fn remote_shell_resolves_managed_install(stdout: &str) -> bool {
    stdout
        .lines()
        .next()
        .map(str::trim)
        .is_some_and(|path| path.ends_with("/.local/bin/zynk"))
}

fn confirm_remote_install(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
    source_description: &str,
) -> io::Result<()> {
    let target = ssh.target();
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "matching remote zynk {} is not installed at {}; run from an interactive terminal to approve installation",
            current_version(),
            remote_zynk.shell_path
        )));
    }

    eprintln!(
        "matching zynk {} is not installed on {target} for {}.",
        current_version(),
        remote_zynk.platform.platform_key()
    );
    eprint!(
        "Install {} to {}? [Y/n] ",
        source_description, remote_zynk.shell_path
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "n" || answer == "no" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote zynk installation cancelled",
        ));
    }

    Ok(())
}

/// ADR 0013 Decision 3: read what the reviewed local binary is, before letting it authorise
/// anything. A version string is not custody — the exact source commit is — so a binary that cannot
/// attest one, or whose attestation is dirty or malformed, is refused outright,
/// `ZYNK_REMOTE_BINARY` files included. This is the authority for BOTH remote decisions: what to
/// copy, and whether a binary already on the remote host may be reused instead.
fn local_install_custody(path: &Path) -> io::Result<InstallCustody> {
    let source = File::open(path)?;
    local_install_custody_from_open_file(path, &source)
}

fn local_install_custody_from_open_file(path: &Path, source: &File) -> io::Result<InstallCustody> {
    use std::os::fd::AsRawFd;
    // The parent keeps this file open while both hashing and the child query run.
    let executable = PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        source.as_raw_fd()
    ));
    let sha256 = crate::checksum::file_sha256(&executable)?;

    let output = Command::new(&executable)
        .arg("--version")
        .output()
        .map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("failed to run {} --version: {err}", path.display()),
            )
        })?;
    if !output.status.success() {
        return Err(command_failed(
            &format!("{} --version failed", path.display()),
            &output,
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or_default();
    let (_, build_sha) = parse_version_line(line).ok_or_else(|| {
        io::Error::other(format!(
            "{} does not report a zynk version line, so it must not be copied to a remote host: got {line:?}",
            path.display()
        ))
    })?;
    let build_sha = build_sha.ok_or_else(|| {
        io::Error::other(format!(
            "{} cannot attest the source commit it was built from, so it must not be copied to a remote host: ADR 0013 custody needs the exact reviewed source SHA, not a version string (docs/zynk/decisions/0013-linux-only-platform-scope.md). Rebuild it from a git checkout, or set ZYNK_BUILD_SHA at build time.",
            path.display()
        ))
    })?;
    if let Some(problem) = crate::build_sha::attested_sha_problem(&build_sha) {
        return Err(io::Error::other(format!(
            "{} attests the source commit {build_sha:?}, which cannot authorise a remote host: {problem}. ADR 0013 custody needs the exact reviewed source SHA (docs/zynk/decisions/0013-linux-only-platform-scope.md). Commit or stash the tree and rebuild, or set ZYNK_BUILD_SHA to the reviewed commit.",
            path.display()
        )));
    }

    // The version and protocol are the LOCAL client's, not the source file's: a remote binary is
    // only usable if it answers on the wire this end is about to speak, which is the pair the
    // version-only check used to compare. Carrying them here gives the one comparator everything.
    Ok(InstallCustody {
        sha256,
        build_sha,
        version: current_version(),
        protocol: CURRENT_PROTOCOL,
    })
}

/// ADR 0013 Decision 3: after the copy, make the REMOTE file prove it is the same bytes from the
/// same source commit — over the same ssh channel, and before any version/protocol check, so a
/// substituted binary is caught by its hash rather than by its self-reported version.
fn verify_remote_custody(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
    custody: &InstallCustody,
) -> io::Result<()> {
    let output = remote_custody_probe(ssh, remote_zynk, custody)?;
    if !output.status.success() {
        return Err(command_failed(
            "remote custody verification failed (requires sha256sum and executable access through /proc/self/fd)",
            &output,
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (remote_sha256, remote_build_sha) = probed_identifiers(&stdout);

    tracing::info!(
        target = %ssh.target(),
        path = %remote_zynk.shell_path,
        local_sha256 = %custody.sha256,
        remote_sha256 = %remote_sha256,
        local_build_sha = %custody.build_sha,
        remote_build_sha = %remote_build_sha,
        "verifying custody of the remote zynk binary"
    );

    check_remote_custody(&stdout, &remote_zynk.shell_path, custody)
        .map_err(CustodyRefusal::into_error)
}

/// The custody comparison itself, over one probe's `sha256sum` + `--version` + `status client
/// --json` output. The single place that decides whether a remote binary IS the reviewed one, used
/// by the reuse decision and by the post-copy check alike.
///
/// The hash is checked first: a substituted binary must fail on its bytes, not on what it says about
/// itself. Then the attestation — which must be well-formed and clean before it is compared, because
/// a foreign build can report any string it likes, and the warden's probe reported a well-formed
/// all-ones SHA. Only then the version and protocol, which are what the bridge needs but are not
/// custody on their own.
fn check_remote_custody(
    remote_stdout: &str,
    remote_path: &str,
    custody: &InstallCustody,
) -> Result<(), CustodyRefusal> {
    let refuse = |reason: String| Err(CustodyRefusal(reason));

    let mut lines = remote_stdout.lines();
    let remote_sha256 = lines.next().unwrap_or_default().trim().to_ascii_lowercase();
    let remote_version_line = lines.next().unwrap_or_default();
    let remote_status_line = lines.next().unwrap_or_default().trim();

    if remote_sha256 != custody.sha256 {
        return refuse(format!(
            "remote zynk at {remote_path} is not the binary that was copied: sha256 {remote_sha256} != {} (ADR 0013 custody)",
            custody.sha256
        ));
    }

    let Some((remote_version, remote_build_sha)) = parse_version_line(remote_version_line) else {
        return refuse(format!(
            "remote zynk at {remote_path} does not report a zynk version line: got {remote_version_line:?}"
        ));
    };
    let Some(remote_build_sha) = remote_build_sha else {
        return refuse(format!(
            "remote zynk at {remote_path} cannot attest the source commit it was built from (ADR 0013 custody)"
        ));
    };
    if let Some(problem) = crate::build_sha::attested_sha_problem(&remote_build_sha) {
        return refuse(format!(
            "remote zynk at {remote_path} attests the source commit {remote_build_sha:?}, which cannot serve as custody: {problem} (ADR 0013 custody)"
        ));
    }
    if remote_build_sha != custody.build_sha {
        return refuse(format!(
            "remote zynk at {remote_path} reports source commit {remote_build_sha}, but the reviewed binary was built from {} (ADR 0013 custody)",
            custody.build_sha
        ));
    }

    if remote_version != custody.version {
        return refuse(format!(
            "remote zynk at {remote_path} reports version {remote_version}, not the {} this client speaks (ADR 0013 custody)",
            custody.version
        ));
    }

    let Some(status) = parse_client_status_json(remote_status_line) else {
        return refuse(format!(
            "remote zynk at {remote_path} did not report its client protocol: got {remote_status_line:?} (ADR 0013 custody)"
        ));
    };
    if status.protocol != custody.protocol {
        return refuse(format!(
            "remote zynk at {remote_path} speaks protocol {}, not the {} this client speaks (ADR 0013 custody)",
            status.protocol, custody.protocol
        ));
    }

    Ok(())
}

fn remote_install_prepare_script(remote_zynk: &RemoteZynk) -> String {
    format!(
        r#"set -eu
dest="$HOME/{install_suffix}"
dir="${{dest%/*}}"
mkdir -p "$dir"
tmp="${{dest}}.tmp.$$"
printf '%s\0%s\0' "$tmp" "$dest"
"#,
        install_suffix = remote_zynk.install_suffix
    )
}

fn parse_remote_install_paths(stdout: &[u8]) -> io::Result<(String, String)> {
    let mut parts = stdout.split(|byte| *byte == 0);
    let tmp_path = parts.next().unwrap_or_default();
    let dest_path = parts.next().unwrap_or_default();
    if tmp_path.is_empty() || dest_path.is_empty() {
        return Err(io::Error::other(
            "remote install preparation did not return destination paths",
        ));
    }
    let tmp_path = String::from_utf8(tmp_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install temporary path is not valid UTF-8: {err}"
        ))
    })?;
    let dest_path = String::from_utf8(dest_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install destination path is not valid UTF-8: {err}"
        ))
    })?;
    Ok((tmp_path, dest_path))
}

fn remote_install_stream_command(tmp_path: &str) -> String {
    format!("tee {}", shell_quote(tmp_path))
}

fn remote_install_commit_script(tmp_path: &str, dest_path: &str) -> String {
    format!(
        "set -eu\nchmod 755 {tmp_path}\nmv {tmp_path} {dest_path}\n",
        tmp_path = shell_quote(tmp_path),
        dest_path = shell_quote(dest_path)
    )
}

fn remote_install_abort_script(tmp_path: &str) -> String {
    format!("rm -f {}\n", shell_quote(tmp_path))
}

fn install_remote_zynk(
    ssh: &(impl RemoteSshConnection + ?Sized),
    remote_zynk: &RemoteZynk,
    source_path: &Path,
) -> io::Result<()> {
    ssh.install_zynk(remote_zynk, source_path)
}

// Test-only ssh seam: `ssh_sh_output` returns a queued canned `Output` instead of spawning ssh, so
// the custody probe and the reuse decision can be driven over a captured remote stdout — the same
// way `check_remote_custody` is exercised over one. An empty queue falls through to the real ssh,
// and nothing outside `#[cfg(test)]` can reach it.
#[cfg(test)]
thread_local! {
    static STUBBED_SSH_OUTPUT: std::cell::RefCell<std::collections::VecDeque<Output>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    // Run the actual generated remote script in a disposable local Linux shell.
    // A one-shot environment lets tests replace the pathname during hashing.
    static SSH_SCRIPT_ENV: std::cell::RefCell<Option<Vec<(String, String)>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn push_stubbed_ssh_output(stdout: &str, success: bool) {
    use std::os::unix::process::ExitStatusExt as _;
    let output = Output {
        status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    };
    STUBBED_SSH_OUTPUT.with(|queue| queue.borrow_mut().push_back(output));
}

#[cfg(test)]
fn take_stubbed_ssh_output() -> Option<Output> {
    STUBBED_SSH_OUTPUT.with(|queue| queue.borrow_mut().pop_front())
}

fn ssh_sh_output(ssh: &(impl RemoteSshConnection + ?Sized), script: &str) -> io::Result<Output> {
    #[cfg(test)]
    if let Some(output) = take_stubbed_ssh_output() {
        return Ok(output);
    }
    #[cfg(test)]
    if let Some(env) = SSH_SCRIPT_ENV.with(|slot| slot.borrow_mut().take()) {
        return Command::new("/bin/sh")
            .args(["-c", script])
            .envs(env)
            .output();
    }
    // Feed POSIX bootstrap scripts to /bin/sh so the user's login shell only
    // has to parse a simple executable invocation.
    let mut child = ssh
        .command()
        .arg("/bin/sh -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let write_result = if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(script.as_bytes())
    } else {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "ssh bootstrap stdin missing",
        ))
    };
    let output = child.wait_with_output()?;
    write_result?;
    Ok(output)
}

fn remote_bridge_command(remote_zynk: &RemoteZynk, session_name: &str) -> io::Result<String> {
    let mut command = "exec /proc/self/fd/3".to_string();
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&shell_quote(session_name));
    }
    command.push_str(" remote-client-bridge");
    let script = prepared_remote_script(remote_zynk, &command)?;
    Ok(format!("exec /bin/sh -c {}", shell_quote(&script)))
}

fn reattach_command(
    program: &str,
    target: &str,
    session_name: &str,
    keybindings: RemoteKeybindings,
    live_handoff: bool,
) -> String {
    let program = crate::platform::remote_reattach_program(program);
    let target = crate::platform::remote_reattach_argument(target);
    let mut command = format!("{program} --remote {target}");
    if keybindings != RemoteKeybindings::Local {
        command.push_str(" --remote-keybindings ");
        command.push_str(keybindings.as_str());
    }
    if live_handoff {
        command.push_str(" --handoff");
    }
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&crate::platform::remote_reattach_argument(session_name));
    }
    command
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_failed(context: &str, output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        io::Error::other(format!("{context}: {}", output.status))
    } else {
        io::Error::other(format!("{context}: {stderr}"))
    }
}

struct SshStdioBridge {
    local_socket: PathBuf,
    socket_identity: crate::ipc::SocketFileIdentity,
    should_stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SshStdioBridge {
    fn start(
        target: String,
        remote_zynk: RemoteZynk,
        local_socket: PathBuf,
        session_name: String,
        ssh_options: Option<ManagedSshOptions>,
    ) -> io::Result<Self> {
        crate::ipc::prepare_socket_path(&local_socket, |path| {
            format!("remote bridge is already listening at {}", path.display())
        })?;
        let listener = crate::ipc::bind_private_local_listener(&local_socket)?;
        let socket_identity = crate::ipc::socket_file_identity(&local_socket)?;
        if let Err(err) =
            crate::ipc::restrict_socket_permissions(&local_socket, BRIDGE_SOCKET_PERMISSION_MODE)
        {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }
        if let Err(err) = listener.set_nonblocking(ListenerNonblockingMode::Accept) {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }

        let should_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&should_stop);
        let thread_ssh_options = ssh_options;
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok(stream) => {
                        let stream = match prepare_remote_bridge_stream(stream) {
                            Ok(stream) => stream,
                            Err(err) => {
                                eprintln!(
                                    "zynk: remote bridge failed to prepare client socket: {err}"
                                );
                                continue;
                            }
                        };
                        if let Err(err) = bridge_connection(
                            stream,
                            &target,
                            &remote_zynk,
                            &session_name,
                            thread_ssh_options.as_ref(),
                            &thread_stop,
                        ) {
                            eprintln!("zynk: remote bridge failed: {err}");
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(BRIDGE_ACCEPT_POLL);
                    }
                    Err(err) => {
                        eprintln!("zynk: remote bridge listener failed: {err}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            local_socket,
            socket_identity,
            should_stop,
            thread: Some(thread),
        })
    }
}

impl Drop for SshStdioBridge {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Release);
        let _ = crate::ipc::remove_socket_file_if_owned(&self.local_socket, &self.socket_identity);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn prepare_remote_bridge_stream(
    mut stream: crate::ipc::LocalStream,
) -> io::Result<crate::ipc::LocalStream> {
    crate::ipc::set_local_stream_polling(&mut stream, false)?;
    Ok(stream)
}

/// Quotes a path for an ssh_config `Include` so a path containing spaces (or
/// glob metacharacters) is treated as one literal token instead of being split
/// or expanded by ssh — otherwise the user's config might not be Included and
/// zynk's fallback would wrongly take effect.
fn ssh_config_quote(path: &str) -> String {
    format!("\"{path}\"")
}

/// Builds a temporary ssh config that keeps the bridge tunnel alive without
/// overriding the user's own settings, returning its path.
///
/// The file `Include`s the user's real ssh config first, so ssh's
/// first-value-wins rule keeps any `ServerAlive*` the user set there (including
/// an explicit `0` to disable it); zynk's values apply only when the user has
/// none.
fn write_managed_ssh_config() -> io::Result<ManagedSshConfig> {
    let paths = crate::platform::remote_ssh_config_paths();
    let dir = crate::platform::create_remote_ssh_config_dir("ctl")?;
    let path = dir.join("config");
    let control_path = dir.join("ctl");

    let mut contents = String::new();
    if let Some(user_config) = paths.user_config.filter(|path| path.is_file()) {
        contents.push_str(&format!(
            "Include {}\n",
            ssh_config_quote(&user_config.to_string_lossy())
        ));
    }
    if let Some(system_config) = paths.system_config.filter(|path| path.is_file()) {
        contents.push_str(&format!(
            "Include {}\n",
            ssh_config_quote(&system_config.to_string_lossy())
        ));
    }
    contents.push_str("Host *\n");
    contents.push_str("  ServerAliveInterval 15\n");
    contents.push_str("  ServerAliveCountMax 4\n");

    let write_result = (|| {
        let mut file = crate::platform::create_remote_ssh_config_file(&path)?;
        file.write_all(contents.as_bytes())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(ManagedSshConfig {
        options: ManagedSshOptions {
            config_path: path,
            control_path,
        },
    })
}

fn bridge_connection(
    stream: crate::ipc::LocalStream,
    target: &str,
    remote_zynk: &RemoteZynk,
    session_name: &str,
    ssh_options: Option<&ManagedSshOptions>,
    bridge_stop: &Arc<AtomicBool>,
) -> io::Result<()> {
    let mut command = Command::new("ssh");
    apply_managed_ssh_options(&mut command, ssh_options);
    command
        .arg("-T")
        .arg(target)
        .arg(remote_bridge_command(remote_zynk, session_name)?);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    bridge_connection_with_command(stream, command, bridge_stop)
}

fn bridge_connection_with_command(
    stream: crate::ipc::LocalStream,
    mut command: Command,
    bridge_stop: &Arc<AtomicBool>,
) -> io::Result<()> {
    let mut child = command
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}")))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdin missing"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdout missing"))?;
    let mut stream_to_child = stream.try_clone()?;
    let cancel_stream = stream.try_clone()?;
    let mut child_to_stream = stream;

    let upload = thread::spawn(move || copy_flush(&mut stream_to_child, &mut child_stdin));
    let download = thread::spawn(move || {
        let result = copy_flush(&mut child_stdout, &mut child_to_stream);
        let _ = crate::ipc::shutdown_local_stream_write(&child_to_stream);
        result
    });

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if bridge_stop.load(Ordering::Acquire) {
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(BRIDGE_ACCEPT_POLL);
    };
    let stopping = bridge_stop.load(Ordering::Acquire);
    let shutdown = if stopping {
        std::net::Shutdown::Both
    } else {
        std::net::Shutdown::Read
    };
    let _ = crate::ipc::shutdown_local_stream(&cancel_stream, shutdown);
    let upload_result = upload
        .join()
        .map_err(|_| io::Error::other("remote bridge upload worker panicked"))?;
    let download_result = download
        .join()
        .map_err(|_| io::Error::other("remote bridge download worker panicked"))?;

    if stopping {
        return Ok(());
    }
    upload_result
        .map_err(|err| io::Error::new(err.kind(), format!("remote bridge upload failed: {err}")))?;
    download_result.map_err(|err| {
        io::Error::new(err.kind(), format!("remote bridge download failed: {err}"))
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("ssh bridge exited with {status}"),
        ))
    }
}

fn copy_flush<R: io::Read, W: io::Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };

        writer.write_all(&buffer[..bytes_read])?;
        writer.flush()?;
        total += bytes_read as u64;
    }
}

fn run_client_process(
    local_socket: &Path,
    reattach_command: &str,
    keybindings: RemoteKeybindings,
) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let status = Command::new(exe)
        .arg("client")
        .env(
            crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
            local_socket,
        )
        .env("ZYNK_RENDER_ENCODING", "terminal-ansi")
        .env(REATTACH_COMMAND_ENV_VAR, reattach_command)
        .env(REMOTE_KEYBINDINGS_ENV_VAR, keybindings.as_str())
        .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("remote client exited with {status}"),
        ))
    }
}

fn local_forward_socket_path(target: &str, session_name: &str) -> PathBuf {
    let pid = std::process::id();
    let target_clean = sanitize_path_component(target);
    let session_clean = sanitize_path_component(session_name);

    let readable_name = format!("zynk-remote-{pid}-{target_clean}-{session_clean}.sock");
    let target_prefix: String = target_clean.chars().take(8).collect();
    let hash = short_socket_hash(target, session_name);
    let short_name = format!("zynk-r-{pid}-{target_prefix}-{hash}.sock");
    crate::platform::remote_bridge_endpoint_path(&readable_name, &short_name)
}

#[cfg(test)]
fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    // sun_path is byte-limited: 104 bytes on macOS, 108 on Linux. Reserve
    // 1 byte for the trailing NUL and use the smaller cap for portability.
    const MAX: usize = 103;
    path.as_os_str().as_bytes().len() <= MAX
}

fn short_socket_hash(target: &str, session: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let socket = std::env::temp_dir().join(format!(
            "zynk-bridge-permissions-test-{}.sock",
            std::process::id()
        ));
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_zynk,
            socket.clone(),
            "default".to_string(),
            None,
        )
        .expect("start bridge listener");

        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, BRIDGE_SOCKET_PERMISSION_MODE);

        drop(bridge);
        let _ = std::fs::remove_file(socket);
    }

    fn local_stream_is_nonblocking(stream: &crate::ipc::LocalStream) -> bool {
        use std::os::fd::AsRawFd;

        let fd = match stream {
            crate::ipc::LocalStream::UdSocket(stream) => stream.inner().as_raw_fd(),
        };
        // SAFETY: F_GETFL only reads descriptor flags for the borrowed live fd.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(
            flags >= 0,
            "fcntl(F_GETFL) failed: {}",
            io::Error::last_os_error()
        );
        flags & libc::O_NONBLOCK != 0
    }

    #[test]
    fn b3_accepted_bridge_stream_is_restored_to_blocking() {
        let (_client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut server = crate::ipc::LocalStream::UdSocket(server.into());
        crate::ipc::set_local_stream_polling(&mut server, true).unwrap();
        assert!(local_stream_is_nonblocking(&server));

        let server = prepare_remote_bridge_stream(server).unwrap();

        assert!(!local_stream_is_nonblocking(&server));
    }

    #[test]
    fn b3_bridge_cancellation_reaps_ssh_and_unblocks_local_io() {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let server = crate::ipc::LocalStream::UdSocket(server.into());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let worker = thread::spawn(move || {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg("cat >/dev/null");
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let result = bridge_connection_with_command(server, command, &worker_stop);
            let _ = done_tx.send(result);
        });

        thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::Release);
        let result = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("bridge cancellation must be bounded");
        assert!(result.is_ok(), "bridge cancellation failed: {result:?}");
        worker.join().unwrap();
        drop(client);
    }

    #[test]
    fn b3_bridge_copy_preserves_bytes_and_stops_at_eof() {
        let input = b"first\nsecond\0third";
        let mut reader = io::Cursor::new(input);
        let mut output = Vec::new();

        let copied = copy_flush(&mut reader, &mut output).unwrap();

        assert_eq!(copied, input.len() as u64);
        assert_eq!(output, input);
    }

    #[test]
    fn keepalive_ssh_config_includes_user_config_then_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let config = write_managed_ssh_config().expect("write managed config");
        let path = config.options.config_path.clone();
        let contents = std::fs::read_to_string(&path).expect("read keepalive config");

        // zynk's fallback keepalive is present...
        assert!(
            contents.contains("Host *"),
            "config should add a Host * fallback block: {contents}"
        );
        assert!(
            contents.contains("ServerAliveInterval 15"),
            "config should set the keepalive interval: {contents}"
        );
        assert!(
            contents.contains("ServerAliveCountMax 4"),
            "config should set the keepalive count: {contents}"
        );
        // ...and any user config is Included (quoted) BEFORE it so first-value-wins
        // keeps the user's own settings.
        if let Some(home) = std::env::var_os("HOME") {
            let user_config = PathBuf::from(home).join(".ssh").join("config");
            if user_config.is_file() {
                let include = format!(
                    "Include {}",
                    ssh_config_quote(&user_config.to_string_lossy())
                );
                let include_at = contents.find(&include).expect("user config Included");
                let fallback_at = contents.find("Host *").expect("fallback present");
                assert!(
                    include_at < fallback_at,
                    "user config must be Included before zynk's fallback: {contents}"
                );
            }
        }

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, BRIDGE_SOCKET_PERMISSION_MODE,
            "keepalive config must be user-only"
        );
        // The config lives in a private 0700 dir, not a predictable temp path.
        let dir = path.parent().expect("config has a parent dir");
        let dir_mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "ssh config dir must be user-only");

        drop(config);
    }

    #[test]
    fn b3_managed_ssh_paths_stay_inside_sentinel_roots_from_a_worker_thread() {
        let _guard = remote_env_lock().lock().unwrap();
        let root = PathBuf::from(format!(
            "/tmp/zynk-remote-hermetic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        let ssh_dir = home.join(".ssh");
        let tmp = root.join("tmp");
        fs::create_dir_all(&ssh_dir).unwrap();
        fs::create_dir_all(&tmp).unwrap();
        fs::write(ssh_dir.join("config"), "Host example\n  BatchMode yes\n").unwrap();

        let mut names = vec![
            "HOME".to_string(),
            "TMPDIR".to_string(),
            "XDG_CONFIG_HOME".to_string(),
            "XDG_DATA_HOME".to_string(),
            "XDG_STATE_HOME".to_string(),
            "XDG_CACHE_HOME".to_string(),
            "XDG_RUNTIME_DIR".to_string(),
        ];
        names.extend(
            std::env::vars_os()
                .filter_map(|(name, _)| name.into_string().ok())
                .filter(|name| name.starts_with("ZYNK_")),
        );
        names.sort();
        names.dedup();
        let mut restore = Vec::new();
        for name in &names {
            restore.push((name.clone(), std::env::var_os(name)));
            std::env::remove_var(name);
        }
        let _restore = TestEnvironmentRestore(restore);
        std::env::set_var("HOME", &home);
        std::env::set_var("TMPDIR", &tmp);
        for (name, suffix) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            std::env::set_var(name, root.join(suffix));
        }

        let config = thread::spawn(write_managed_ssh_config)
            .join()
            .expect("managed config worker panicked")
            .expect("managed config creation failed");
        assert!(config.options.config_path.starts_with(&root));
        assert!(config.options.control_path.starts_with(&root));
        let contents = fs::read_to_string(&config.options.config_path).unwrap();
        assert!(contents.contains(&ssh_config_quote(&ssh_dir.join("config").to_string_lossy())));

        drop(config);
        assert!(ssh_dir.join("config").is_file());
        assert!(
            fs::read_dir(&tmp).unwrap().next().is_none(),
            "managed SSH cleanup left a path outside the retained fixture files"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ssh_config_quote_wraps_path_with_spaces() {
        assert_eq!(
            ssh_config_quote("/home/a b/.ssh/config"),
            "\"/home/a b/.ssh/config\""
        );
    }

    #[test]
    fn extract_remote_args_removes_space_form() {
        let args = vec![
            "zynk".into(),
            "--remote".into(),
            "dev".into(),
            "--help".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["zynk", "--help"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_removes_equals_form() {
        let args = vec!["zynk".into(), "--remote=user@host".into()];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["zynk"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "user@host");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_server() {
        let args = vec![
            "zynk".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["zynk"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_space_form() {
        let args = vec![
            "zynk".into(),
            "--remote=dev".into(),
            "--remote-keybindings".into(),
            "server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["zynk"]);
        assert_eq!(remote.unwrap().keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_explicit_handoff() {
        let args = vec!["zynk".into(), "--remote=dev".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["zynk"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert!(remote.live_handoff);
    }

    #[test]
    fn extract_remote_args_preserves_child_remote_options_after_separator() {
        let args = vec![
            "zynk".into(),
            "agent".into(),
            "start".into(),
            "repro".into(),
            "--".into(),
            "child".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
            "--handoff".into(),
        ];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_preserves_handoff_without_remote() {
        let args = vec!["zynk".into(), "update".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_rejects_remote_keybindings_without_remote() {
        let args = vec!["zynk".into(), "--remote-keybindings=server".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings requires --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_remote_keybindings() {
        let args = vec![
            "zynk".into(),
            "--remote=dev".into(),
            "--remote-keybindings=local".into(),
            "--remote-keybindings=server".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings can only be specified once");
    }

    #[test]
    fn extract_remote_args_requires_value() {
        let args = vec!["zynk".into(), "--remote".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_empty_value() {
        let args = vec!["zynk".into(), "--remote=".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_values() {
        let args = vec!["zynk".into(), "--remote=dev".into(), "--remote=prod".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote can only be specified once");
    }

    #[test]
    fn extract_remote_args_rejects_option_like_target() {
        let args = vec!["zynk".into(), "--remote".into(), "-oProxyCommand=x".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote target must not start with '-'");
    }

    #[test]
    fn sanitize_path_component_removes_shell_sensitive_chars() {
        assert_eq!(sanitize_path_component("user@host:22"), "user-host-22");
    }

    #[test]
    fn remote_platform_accepts_only_linux_x86_64() {
        // ADR 0013: Linux x86_64 is the whole supported surface, on the remote host too.
        assert_eq!(
            RemotePlatform::from_uname("Linux", "x86_64")
                .unwrap()
                .platform_key(),
            "linux-x86_64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Linux", "amd64")
                .unwrap()
                .platform_key(),
            "linux-x86_64"
        );
    }

    #[test]
    fn remote_platform_refuses_every_other_os_and_architecture() {
        for (os, arch) in [
            ("Darwin", "arm64"),
            ("Darwin", "x86_64"),
            ("Linux", "aarch64"),
            ("Linux", "arm64"),
            ("Linux", "riscv64"),
            ("FreeBSD", "x86_64"),
        ] {
            assert!(
                RemotePlatform::from_uname(os, arch).is_none(),
                "{os} {arch} must be refused (ADR 0013: Linux x86_64 only)"
            );
        }
    }

    #[test]
    fn remote_platform_detection_error_names_the_linux_only_decision() {
        let err = RemotePlatform::from_uname("Darwin", "arm64")
            .ok_or_else(|| unsupported_remote_platform_error("Darwin", "arm64"))
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("Linux x86_64 only") && message.contains("0013"),
            "the refusal must name ADR 0013: {message}"
        );
    }

    #[test]
    fn reattach_command_includes_remote_and_session() {
        assert_eq!(
            reattach_command(
                "target/release/zynk",
                "user@host",
                "work",
                RemoteKeybindings::Local,
                false,
            ),
            "target/release/zynk --remote user@host --session work"
        );
        assert_eq!(
            reattach_command(
                "zynk",
                "host name",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                false,
            ),
            "zynk --remote 'host name'"
        );
        assert_eq!(
            reattach_command(
                "zynk",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Server,
                false,
            ),
            "zynk --remote host --remote-keybindings server"
        );
        assert_eq!(
            reattach_command(
                "zynk",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                true,
            ),
            "zynk --remote host --handoff"
        );
    }

    #[test]
    fn remote_bridge_command_uses_installed_binary() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert!(remote_bridge_command(&remote_zynk, crate::session::DEFAULT_SESSION_NAME).is_err());
        let command = remote_bridge_command(
            &remote_zynk.with_custody(&custody_fixture()),
            crate::session::DEFAULT_SESSION_NAME,
        )
        .unwrap();
        assert!(command.starts_with("exec /bin/sh -c "));
        assert!(command.contains("exec 3<\"$HOME/.local/bin/zynk\""));
        assert!(command.contains("exec /proc/self/fd/3 remote-client-bridge"));
    }

    #[test]
    fn remote_path_discovery_uses_path_binary() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_zynk =
            remote_zynk_from_path_discovery(&remote_zynk, "/usr/bin/zynk\n").expect("path binary");

        assert_eq!(remote_zynk.shell_path, "/usr/bin/zynk");
    }

    #[test]
    fn b3_remote_path_discovery_reads_package_candidates_and_skips_mise_shims() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform::local());
        let candidates = remote_zynks_from_path_discovery(
            &remote_zynk,
            "/home/user/.local/share/mise/shims/zynk\n/home/user/.local/share/mise/installs/zynk/3.1.0/bin/zynk\n/home/linuxbrew/.linuxbrew/bin/zynk\nrelative/zynk\n",
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].shell_path,
            "/home/user/.local/share/mise/installs/zynk/3.1.0/bin/zynk"
        );
        assert_eq!(
            candidates[1].shell_path,
            "/home/linuxbrew/.linuxbrew/bin/zynk"
        );

        let script = known_remote_binary_candidate_script(&RemotePlatform::local());
        assert!(script.contains("$home/.local/bin/zynk"));
        assert!(script.contains("$home/.local/share/mise/installs/zynk/$version/bin/zynk"));
        assert!(script.contains("$home/.local/share/mise/installs/zynk/$version/zynk"));
        assert!(script.contains("/home/linuxbrew/.linuxbrew/bin/zynk"));
        assert!(!script.contains("mise/shims/zynk"));
    }

    #[test]
    fn b3_remote_install_scripts_use_portable_atomic_replacement() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform::local());
        let prepare = remote_install_prepare_script(&remote_zynk);

        assert!(prepare.contains("mkdir -p \"$dir\""));
        assert!(prepare.contains("printf '%s\\0%s\\0' \"$tmp\" \"$dest\""));
        assert_eq!(
            parse_remote_install_paths(b"/home/a b/zynk.tmp.42\0/home/a b/zynk\0").unwrap(),
            (
                "/home/a b/zynk.tmp.42".to_string(),
                "/home/a b/zynk".to_string()
            )
        );
        assert_eq!(
            remote_install_stream_command("/home/a b/zynk.tmp.42"),
            "tee '/home/a b/zynk.tmp.42'"
        );
        assert_eq!(
            remote_install_commit_script("/home/a b/zynk.tmp.42", "/home/a b/zynk"),
            "set -eu\nchmod 755 '/home/a b/zynk.tmp.42'\nmv '/home/a b/zynk.tmp.42' '/home/a b/zynk'\n"
        );
        assert_eq!(
            remote_install_abort_script("/home/a b/zynk.tmp.42"),
            "rm -f '/home/a b/zynk.tmp.42'\n"
        );
        assert!(parse_remote_install_paths(b"/tmp/partial\0").is_err());
    }

    #[test]
    fn b3_managed_ssh_options_reuse_one_private_control_socket() {
        let options = ManagedSshOptions {
            config_path: PathBuf::from("/tmp/private/config"),
            control_path: PathBuf::from("/tmp/private/ctl"),
        };
        let mut command = Command::new("ssh");

        apply_managed_ssh_options(&mut command, Some(&options));

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-F",
                "/tmp/private/config",
                "-S",
                "/tmp/private/ctl",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=yes",
            ]
        );
    }

    #[test]
    fn remote_path_discovery_quotes_discovered_binary() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_zynk = remote_zynk_from_path_discovery(&remote_zynk, "/opt/zynk bin/zynk\n")
            .expect("path binary");

        assert_eq!(remote_zynk.shell_path, "'/opt/zynk bin/zynk'");
    }

    #[test]
    fn remote_path_discovery_keeps_the_linux_platform_key() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform::local());
        let remote_zynk = remote_zynk_from_path_discovery(&remote_zynk, "/usr/local/bin/zynk\n")
            .expect("path binary");

        assert_eq!(remote_zynk.shell_path, "/usr/local/bin/zynk");
        assert_eq!(remote_zynk.platform.platform_key(), "linux-x86_64");
    }

    #[test]
    fn remote_path_discovery_quotes_single_quotes_in_discovered_binary() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_zynk = remote_zynk_from_path_discovery(&remote_zynk, "/opt/zynk's/bin/zynk\n")
            .expect("path binary");

        assert_eq!(remote_zynk.shell_path, "'/opt/zynk'\\''s/bin/zynk'");
    }

    #[test]
    fn remote_path_discovery_ignores_relative_paths() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_zynk = remote_zynk_from_path_discovery(&remote_zynk, "bin/zynk\n");

        assert!(remote_zynk.is_none());
    }

    #[test]
    fn remote_path_discovery_ignores_empty_output() {
        let remote_zynk = RemoteZynk::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_zynk = remote_zynk_from_path_discovery(&remote_zynk, "\n");

        assert!(remote_zynk.is_none());
    }

    #[test]
    fn remote_shell_path_warning_accepts_managed_install() {
        assert!(remote_shell_resolves_managed_install(
            "/home/user/.local/bin/zynk\n"
        ));
        assert!(remote_shell_resolves_managed_install(
            "/Users/user/.local/bin/zynk\n"
        ));
        assert!(!remote_shell_resolves_managed_install(
            "/usr/local/bin/zynk\n"
        ));
        assert!(!remote_shell_resolves_managed_install(""));
    }

    #[test]
    fn parse_client_status_json_reads_protocol() {
        assert_eq!(
            parse_client_status_json(r#"{"version":"x","protocol":8,"binary":"/bin/zynk"}"#)
                .map(|status| status.protocol),
            Some(8)
        );
        assert!(parse_client_status_json(r#"{"protocol":"unknown"}"#).is_none());
    }

    #[test]
    fn parse_remote_server_status_json_reads_running_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8,"capabilities":{"live_handoff":true}}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                live_handoff: true,
                detached_server_daemon: false
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_treats_missing_capability_as_no_handoff() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                live_handoff: false,
                detached_server_daemon: false
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_stopped_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"not_running","running":false,"version":null,"protocol":null}"#
            )
            .unwrap(),
            RemoteServerStatus::NotRunning
        );
    }

    #[test]
    fn remote_server_restart_reason_requires_stop_for_protocol_mismatch() {
        assert_eq!(
            remote_server_restart_reason(Some(&current_version()), Some(0), true, false),
            Some(RemoteServerRestartReason::ProtocolMismatch)
        );
    }

    #[test]
    fn remote_server_restart_reason_offers_restart_after_binary_update() {
        assert_eq!(
            remote_server_restart_reason(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                true,
                true
            ),
            Some(RemoteServerRestartReason::BinaryUpdated)
        );
    }

    #[test]
    fn remote_server_restart_reason_offers_restart_for_version_mismatch() {
        assert_eq!(
            remote_server_restart_reason(Some("0.0.0"), Some(CURRENT_PROTOCOL), true, false),
            Some(RemoteServerRestartReason::VersionMismatch)
        );
        assert_eq!(
            remote_server_restart_reason(None, Some(CURRENT_PROTOCOL), true, false),
            Some(RemoteServerRestartReason::VersionMismatch)
        );
    }

    #[test]
    fn remote_server_restart_reason_allows_current_server() {
        assert_eq!(
            remote_server_restart_reason(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                true,
                false
            ),
            None
        );
    }

    #[test]
    fn remote_server_restart_reason_prefers_version_mismatch_over_helper_update() {
        assert_eq!(
            remote_server_restart_reason(Some("0.0.0"), Some(CURRENT_PROTOCOL), true, true),
            Some(RemoteServerRestartReason::VersionMismatch)
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_detached_daemon_capability() {
        for detached in [false, true] {
            let json = serde_json::json!({
                "running": true,
                "version": current_version(),
                "protocol": CURRENT_PROTOCOL,
                "capabilities": {"live_handoff": true, "detached_server_daemon": detached}
            });
            assert_eq!(
                parse_remote_server_status_json(&json.to_string()).unwrap(),
                RemoteServerStatus::Running {
                    version: Some(current_version()),
                    protocol: Some(CURRENT_PROTOCOL),
                    live_handoff: true,
                    detached_server_daemon: detached,
                }
            );
        }
    }

    #[test]
    fn remote_server_restart_reason_requires_restart_for_old_daemon() {
        for changed in [false, true] {
            assert_eq!(
                remote_server_restart_reason(
                    Some(&current_version()),
                    Some(CURRENT_PROTOCOL),
                    false,
                    changed
                ),
                Some(RemoteServerRestartReason::DaemonDetachMissing)
            );
        }
    }

    #[test]
    fn remote_server_restart_reason_prioritizes_protocol_then_daemon() {
        for protocol in [None, Some(0)] {
            assert_eq!(
                remote_server_restart_reason(Some("0.0.0"), protocol, false, true),
                Some(RemoteServerRestartReason::ProtocolMismatch)
            );
        }
        assert_eq!(
            remote_server_restart_reason(Some("0.0.0"), Some(CURRENT_PROTOCOL), false, true),
            Some(RemoteServerRestartReason::DaemonDetachMissing)
        );
    }

    #[test]
    fn remote_install_plan_covers_reasons_and_both_handoff_gates() {
        use RemoteInstallRunningServerPlan::{KeepRunning, LiveHandoff, StopRequired};
        use RemoteServerRestartReason::*;
        let current = current_version();
        let rows = [
            (current.as_str(), Some(0), false, true, ProtocolMismatch),
            (
                current.as_str(),
                Some(CURRENT_PROTOCOL),
                false,
                true,
                DaemonDetachMissing,
            ),
            ("0.0.0", Some(CURRENT_PROTOCOL), true, true, VersionMismatch),
            (
                current.as_str(),
                Some(CURRENT_PROTOCOL),
                true,
                true,
                BinaryUpdated,
            ),
        ];
        for (peer, enabled) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                remote_install_running_server_plan(
                    Some(&current),
                    Some(CURRENT_PROTOCOL),
                    true,
                    false,
                    peer,
                    enabled
                ),
                KeepRunning,
                "unchanged compatible peer={peer} enabled={enabled}"
            );
            for (version, protocol, detached, changed, reason) in rows {
                let expected = if peer && enabled {
                    LiveHandoff
                } else {
                    StopRequired(reason)
                };
                assert_eq!(
                    remote_install_running_server_plan(
                        Some(version),
                        protocol,
                        detached,
                        changed,
                        peer,
                        enabled
                    ),
                    expected,
                    "reason={reason:?} peer={peer} enabled={enabled}"
                );
            }
        }
    }

    struct RemoteStatusFixture {
        path: PathBuf,
        calls: PathBuf,
        remote: RemoteZynk,
    }

    impl RemoteStatusFixture {
        fn new(status: serde_json::Value) -> Self {
            let path = write_fake_zynk("install-decision", "unused");
            let calls = path.with_file_name("calls");
            fs::write(&path, format!(
                "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$*\" >> {}\ntest \"$*\" = 'status server --json' || exit 42\nprintf '%s\\n' {}\n",
                shell_quote(calls.to_str().unwrap()), shell_quote(&status.to_string())
            )).unwrap();
            let custody = InstallCustody {
                sha256: crate::checksum::file_sha256(&path).unwrap(),
                ..custody_fixture()
            };
            let remote = RemoteZynk::for_platform(RemotePlatform::local())
                .with_shell_path(shell_quote(path.to_str().unwrap()))
                .with_custody(&custody);
            Self {
                path,
                calls,
                remote,
            }
        }

        fn confirm_install(&self, enabled: bool) -> io::Result<bool> {
            assert!(
                !io::stdin().is_terminal(),
                "requires noninteractive test input"
            );
            SSH_SCRIPT_ENV.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(vec![("PATH".into(), "/usr/bin:/bin".into())]);
            });
            confirm_remote_install_with_running_server("isolated-shell", &self.remote, enabled)
        }
    }

    impl Drop for RemoteStatusFixture {
        fn drop(&mut self) {
            SSH_SCRIPT_ENV.with(|slot| *slot.borrow_mut() = None);
            let _ = fs::remove_dir_all(self.path.parent().unwrap());
        }
    }

    #[test]
    fn remote_install_wrapper_preserves_noninteractive_stop_authority() {
        for detached in [None, Some(false), Some(true)] {
            for (peer, enabled) in [(false, false), (true, false), (false, true), (true, true)] {
                let mut capabilities = serde_json::json!({"live_handoff": peer});
                if let Some(detached) = detached {
                    capabilities["detached_server_daemon"] = detached.into();
                }
                let fixture = RemoteStatusFixture::new(serde_json::json!({
                    "running": true, "version": current_version(),
                    "protocol": CURRENT_PROTOCOL, "capabilities": capabilities,
                }));
                let result = fixture.confirm_install(enabled);
                if peer && enabled {
                    assert!(!result.unwrap(), "handoff must not approve stopping");
                } else {
                    let error = result.unwrap_err().to_string();
                    assert!(
                        error.contains("approve stopping it for the update"),
                        "{error}"
                    );
                }
                assert_eq!(
                    fs::read_to_string(&fixture.calls).unwrap(),
                    "/proc/self/fd/3\nstatus server --json\n",
                    "decision may query only the pinned executable; no stop/install/handoff"
                );
            }
        }
        let fixture = RemoteStatusFixture::new(serde_json::json!({
            "running": true, "version": current_version(), "protocol": CURRENT_PROTOCOL,
        }));
        assert!(fixture
            .confirm_install(true)
            .unwrap_err()
            .to_string()
            .contains("approve stopping"));
        assert_eq!(
            fs::read_to_string(&fixture.calls).unwrap(),
            "/proc/self/fd/3\nstatus server --json\n"
        );
    }

    #[test]
    fn remote_install_wrapper_rejects_changed_prepared_bytes_before_status() {
        let fixture = RemoteStatusFixture::new(serde_json::json!({"running": false}));
        fs::OpenOptions::new()
            .append(true)
            .open(&fixture.path)
            .unwrap()
            .write_all(b"# changed after preparation\n")
            .unwrap();
        let error = fixture.confirm_install(true).unwrap_err().to_string();
        assert!(
            error.contains("executable bytes changed; refusing execution"),
            "{error}"
        );
        assert!(
            !fixture.calls.exists(),
            "no command may run before hash acceptance"
        );
    }

    #[test]
    fn install_source_description_uses_override_binary() {
        let platform = RemotePlatform::local();
        assert_eq!(
            install_source_description_for(&platform, Some(Path::new("/tmp/zynk-linux")), false),
            "ZYNK_REMOTE_BINARY (/tmp/zynk-linux)"
        );
    }

    #[test]
    fn install_source_description_uses_local_binary_when_allowed() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, true),
            "the current local zynk binary"
        );
    }

    #[test]
    fn install_source_description_has_no_source_when_local_binary_cannot_seed_remote() {
        // ADR 0013 removed the release-asset download path: an unusable local binary leaves
        // ZYNK_REMOTE_BINARY as the only source, and the description says so.
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, false),
            "no install source for linux-x86_64 (set ZYNK_REMOTE_BINARY)"
        );
    }

    fn write_fake_zynk(name: &str, version_line: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zynk-custody-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create custody fixture dir");
        let path = dir.join("zynk");
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' '{version_line}'\n"),
        )
        .expect("write custody fixture");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod fixture");
        path
    }

    #[test]
    fn parse_version_line_reads_the_version_and_the_source_commit() {
        assert_eq!(
            parse_version_line("zynk 3.1.0 (abc123)"),
            Some(("3.1.0".to_string(), Some("abc123".to_string())))
        );
        assert_eq!(
            parse_version_line("zynk 3.1.0"),
            Some(("3.1.0".to_string(), None))
        );
        assert_eq!(parse_version_line("something else 1.0"), None);
        assert_eq!(parse_version_line("zynk"), None);
    }

    #[test]
    fn local_install_custody_records_the_hash_and_the_source_commit() {
        // ADR 0013 Decision 3: custody is the exact source SHA plus the binary hash, and the
        // version and protocol the local client speaks are carried with them so one comparator
        // decides both a reuse and a post-copy check.
        let reviewed = "d".repeat(40);
        let path = write_fake_zynk(
            "attesting",
            &format!("zynk {} ({reviewed})", current_version()),
        );
        let custody = local_install_custody(&path).expect("custody");
        assert_eq!(custody.build_sha, reviewed);
        assert_eq!(
            custody.sha256,
            crate::checksum::file_sha256(&path).expect("hash")
        );
        assert_eq!(custody.version, current_version());
        assert_eq!(custody.protocol, CURRENT_PROTOCOL);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_dirty_local_binary_cannot_be_copied_to_a_remote_host() {
        // A -dirty attestation says the compiled tree was NOT the commit it names, so it cannot
        // authorise anything on another host — neither a copy nor a reuse.
        let path = write_fake_zynk("dirty", &format!("zynk 3.1.0 ({}-dirty)", "a".repeat(40)));
        let result = local_install_custody(&path);
        let _ = fs::remove_dir_all(path.parent().unwrap());
        let err = result.expect_err("a dirty local build must not authorise a remote host");
        let message = err.to_string();
        assert!(
            message.contains("-dirty") && message.contains("0013"),
            "the refusal must name the dirty tree and ADR 0013: {message}"
        );
    }

    #[test]
    fn a_malformed_attestation_is_refused() {
        // A foreign build can print any string it likes inside the parentheses, so the shape is
        // checked before the value is trusted for anything.
        for line in [
            "zynk 3.1.0 (not-a-sha)",
            "zynk 3.1.0 (deadbeefcafe)",
            &format!("zynk 3.1.0 ({})", "A".repeat(40)),
            &format!("zynk 3.1.0 ({})", "a".repeat(41)),
        ] {
            let path = write_fake_zynk("malformed", line);
            let result = local_install_custody(&path);
            let _ = fs::remove_dir_all(path.parent().unwrap());
            let err = result.expect_err("a malformed attestation must be refused");
            assert!(
                err.to_string().contains("0013"),
                "the refusal must name ADR 0013 for {line:?}: {err}"
            );
        }

        // And on the remote side, where the two attestations agree but neither is a commit.
        let mut custody = custody_fixture();
        custody.build_sha = "deadbeefcafe".to_string();
        let stdout = remote_probe_stdout(
            &custody.sha256,
            &custody.version,
            &custody.build_sha,
            custody.protocol,
        );
        let refusal = check_remote_custody(&stdout, "/remote/zynk", &custody)
            .expect_err("a malformed remote attestation must be refused");
        assert!(
            refusal.reason().contains("cannot serve as custody"),
            "unexpected refusal: {}",
            refusal.reason()
        );
    }

    #[test]
    fn local_install_custody_refuses_a_binary_that_cannot_attest_its_source() {
        // A version string is not custody: a binary with no attested commit must not be copied,
        // ZYNK_REMOTE_BINARY files included.
        let path = write_fake_zynk("unattested", "zynk 3.1.0");
        let err = local_install_custody(&path).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("cannot attest the source commit") && message.contains("0013"),
            "the refusal must name ADR 0013 custody: {message}"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn local_install_custody_refuses_a_binary_that_is_not_zynk() {
        let path = write_fake_zynk("foreign", "some-other-tool 1.0.0");
        let err = local_install_custody(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not report a zynk version line"),
            "unexpected error: {err}"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    fn custody_fixture() -> InstallCustody {
        InstallCustody {
            sha256: "a".repeat(64),
            build_sha: "d".repeat(40),
            version: current_version(),
            protocol: CURRENT_PROTOCOL,
        }
    }

    struct CustodyRaceFixture {
        path: PathBuf,
        replacement: PathBuf,
        marker: PathBuf,
        remote: RemoteZynk,
        custody: InstallCustody,
        env: Vec<(String, String)>,
    }

    impl CustodyRaceFixture {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let mut custody = custody_fixture();
            let path = write_fake_zynk("path-race", "unused");
            let root = path.parent().unwrap();
            let marker = root.join("unreviewed-executed");
            let replacement = root.join("replacement");
            let body = format!(
                "touch {}\ncase \"$1\" in\n--version) printf '%s\\n' {} ;;\nstatus) printf '%s\\n' {} ;;\nserver) \"$4\" --version 3<&- ;;\n*) printf 'reviewed-bridge\\n'; printf '%s\\n' \"$@\" ;;\nesac\n",
                shell_quote(root.join("any-execution").to_str().unwrap()),
                shell_quote(&format!("zynk {} ({})", custody.version, custody.build_sha)),
                shell_quote(&format!(r#"{{"protocol":{}}}"#, custody.protocol)),
            );
            fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            fs::write(
                &replacement,
                format!(
                    "#!/bin/sh\ntouch {}\n{body}",
                    shell_quote(marker.to_str().unwrap())
                ),
            )
            .unwrap();
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
            custody.sha256 = crate::checksum::file_sha256(&path).unwrap();
            let tools = root.join("tools");
            fs::create_dir(&tools).unwrap();
            let hasher = tools.join("sha256sum");
            fs::write(&hasher, "#!/bin/sh\nif test \"$HASH_FAIL\" = 1; then exit 42; fi\n/usr/bin/sha256sum \"$@\" || exit $?\nif test \"$SWAP_AFTER_HASH\" = 1 && test -e \"$REPLACEMENT\"; then mv -- \"$REPLACEMENT\" \"$TARGET\"; fi\n").unwrap();
            fs::set_permissions(&hasher, fs::Permissions::from_mode(0o755)).unwrap();
            let env = vec![
                ("PATH".into(), format!("{}:/usr/bin:/bin", tools.display())),
                ("REPLACEMENT".into(), replacement.display().to_string()),
                ("TARGET".into(), path.display().to_string()),
                ("SWAP_AFTER_HASH".into(), "1".into()),
            ];
            let remote = RemoteZynk::for_platform(RemotePlatform::local())
                .with_shell_path(shell_quote(path.to_str().unwrap()))
                .with_custody(&custody);
            Self {
                path,
                replacement,
                marker,
                remote,
                custody,
                env,
            }
        }

        fn run_probe(&self, post_copy: bool, hash_fails: bool) -> bool {
            let mut env = self.env.clone();
            env.push((
                "HASH_FAIL".into(),
                if hash_fails { "1" } else { "0" }.into(),
            ));
            SSH_SCRIPT_ENV.with(|slot| *slot.borrow_mut() = Some(env));
            if post_copy {
                verify_remote_custody("isolated-shell", &self.remote, &self.custody).is_ok()
            } else {
                remote_binary_is_the_reviewed_one("isolated-shell", &self.remote, &self.custody)
                    .unwrap()
            }
        }
    }

    impl Drop for CustodyRaceFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.path.parent().unwrap());
        }
    }

    #[test]
    fn local_custody_uses_the_open_file_not_a_replaced_pathname() {
        let fixture = CustodyRaceFixture::new();
        let source = File::open(&fixture.path).unwrap();
        fs::rename(&fixture.replacement, &fixture.path).unwrap();
        let custody = local_install_custody_from_open_file(&fixture.path, &source).unwrap();
        assert_eq!(
            custody.sha256, fixture.custody.sha256,
            "custody reopened the local pathname"
        );
        assert!(!fixture.marker.exists());
    }

    #[test]
    fn remote_reuse_keeps_the_executable_open_across_hash_and_metadata() {
        let fixture = CustodyRaceFixture::new();
        assert!(fixture.run_probe(false, false));
        assert!(
            !fixture.marker.exists(),
            "reuse executed the pathname substituted after hashing"
        );
    }

    #[test]
    fn remote_post_copy_keeps_the_executable_open_across_hash_and_metadata() {
        let fixture = CustodyRaceFixture::new();
        assert!(fixture.run_probe(true, false));
        assert!(
            !fixture.marker.exists(),
            "post-copy validation executed substituted bytes"
        );
    }

    #[test]
    fn remote_probe_checks_the_hash_before_executing_any_metadata() {
        for post_copy in [false, true] {
            let fixture = CustodyRaceFixture::new();
            fs::rename(&fixture.replacement, &fixture.path).unwrap();
            assert!(!fixture.run_probe(post_copy, false));
            assert!(
                !fixture.marker.exists(),
                "an unreviewed binary ran before hash comparison"
            );
        }
    }

    #[test]
    fn remote_bridge_revalidates_a_path_changed_after_preparation() {
        let fixture = CustodyRaceFixture::new();
        fs::rename(&fixture.replacement, &fixture.path).unwrap();
        let command =
            remote_bridge_command(&fixture.remote, crate::session::DEFAULT_SESSION_NAME).unwrap();
        let output = Command::new("/bin/sh")
            .args(["-c", &command])
            .envs(fixture.env.clone())
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "bridge ran changed bytes after preparation: {output:?}"
        );
        assert!(!fixture.marker.exists());
    }

    #[test]
    fn remote_bridge_executes_the_hashed_inode_even_if_its_path_is_replaced() {
        let mut fixture = CustodyRaceFixture::new();
        let quoted_path = fixture.path.with_file_name("zynk's reviewed binary");
        fs::rename(&fixture.path, &quoted_path).unwrap();
        fixture.path = quoted_path;
        fixture.remote.shell_path = shell_quote(fixture.path.to_str().unwrap());
        fixture
            .env
            .push(("TARGET".into(), fixture.path.display().to_string()));
        let command = remote_bridge_command(&fixture.remote, "work 'one'").unwrap();
        let output = Command::new("/bin/sh")
            .args(["-c", &command])
            .envs(fixture.env.clone())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "reviewed-bridge\n--session\nwork 'one'\nremote-client-bridge\n"
        );
        assert!(!fixture.marker.exists());
        assert_ne!(
            crate::checksum::file_sha256(&fixture.path).unwrap(),
            fixture.custody.sha256
        );
    }

    #[test]
    fn a_failed_remote_hash_query_runs_no_binary_and_names_the_requirement() {
        for post_copy in [false, true] {
            let fixture = CustodyRaceFixture::new();
            let mut env = fixture.env.clone();
            env.push(("HASH_FAIL".into(), "1".into()));
            SSH_SCRIPT_ENV.with(|slot| *slot.borrow_mut() = Some(env));
            let error = if post_copy {
                verify_remote_custody("isolated-shell", &fixture.remote, &fixture.custody)
                    .unwrap_err()
            } else {
                remote_binary_is_the_reviewed_one(
                    "isolated-shell",
                    &fixture.remote,
                    &fixture.custody,
                )
                .unwrap_err()
            };
            assert!(error.to_string().contains("sha256sum failed"), "{error}");
            assert!(!fixture
                .path
                .parent()
                .unwrap()
                .join("any-execution")
                .exists());
        }
    }

    #[test]
    fn remote_handoff_import_path_names_the_descriptor_holder() {
        let fixture = CustodyRaceFixture::new();
        SSH_SCRIPT_ENV.with(|slot| *slot.borrow_mut() = Some(fixture.env.clone()));
        // The fake CLI executes the received --import-exe in a child process. The
        // path must still refer to the SSH shell's held inode after replacement.
        live_handoff_remote_server("isolated-shell", &fixture.remote).unwrap();
        assert!(!fixture.marker.exists());
        assert_ne!(
            crate::checksum::file_sha256(&fixture.path).unwrap(),
            fixture.custody.sha256
        );
    }

    /// What one custody probe prints: the file's hash, its version line, its client status.
    fn remote_probe_stdout(sha256: &str, version: &str, build_sha: &str, protocol: u32) -> String {
        format!("{sha256}\nzynk {version} ({build_sha})\n{{\"protocol\":{protocol}}}\n")
    }

    fn probe_remote_zynk() -> RemoteZynk {
        RemoteZynk::for_platform(RemotePlatform::local())
            .with_shell_path("'/opt/foreign/zynk'".to_string())
    }

    #[test]
    fn remote_custody_accepts_the_same_bytes_from_the_same_commit() {
        let custody = custody_fixture();
        let stdout = remote_probe_stdout(
            &custody.sha256,
            &custody.version,
            &custody.build_sha,
            custody.protocol,
        );
        check_remote_custody(&stdout, "'$HOME/.local/bin/zynk'", &custody).expect("custody ok");
    }

    #[test]
    fn remote_custody_rejects_a_substituted_binary_even_with_a_matching_version() {
        // The hash is checked before the version/protocol validation, so a binary that reports the
        // right version but is not the bytes that were copied still fails.
        let custody = custody_fixture();
        let stdout = remote_probe_stdout(
            &"b".repeat(64),
            &custody.version,
            &custody.build_sha,
            custody.protocol,
        );
        let refusal = check_remote_custody(&stdout, "/remote/zynk", &custody).unwrap_err();
        assert!(
            refusal
                .reason()
                .contains("is not the binary that was copied"),
            "unexpected refusal: {}",
            refusal.reason()
        );
    }

    #[test]
    fn remote_custody_rejects_a_different_source_commit() {
        let custody = custody_fixture();
        let foreign = "1".repeat(40);
        let stdout = remote_probe_stdout(
            &custody.sha256,
            &custody.version,
            &foreign,
            custody.protocol,
        );
        let refusal = check_remote_custody(&stdout, "/remote/zynk", &custody).unwrap_err();
        assert!(
            refusal
                .reason()
                .contains(&format!("reports source commit {foreign}")),
            "unexpected refusal: {}",
            refusal.reason()
        );
    }

    #[test]
    fn remote_custody_rejects_a_remote_that_cannot_attest_its_source() {
        let custody = custody_fixture();
        let stdout = format!(
            "{}\nzynk {}\n{{\"protocol\":{}}}\n",
            custody.sha256, custody.version, custody.protocol
        );
        let refusal = check_remote_custody(&stdout, "/remote/zynk", &custody).unwrap_err();
        assert!(
            refusal.reason().contains("cannot attest the source commit"),
            "unexpected refusal: {}",
            refusal.reason()
        );
    }

    #[test]
    fn a_dirty_remote_attestation_is_refused() {
        // Even when both ends agree on the string: a -dirty tree is not the commit it names, so a
        // matching pair of dirty attestations is still not custody.
        let mut custody = custody_fixture();
        custody.build_sha = format!("{}-dirty", "b".repeat(40));
        let stdout = remote_probe_stdout(
            &custody.sha256,
            &custody.version,
            &custody.build_sha,
            custody.protocol,
        );
        let refusal = check_remote_custody(&stdout, "/remote/zynk", &custody)
            .expect_err("a dirty remote attestation must be refused");
        assert!(
            refusal.reason().contains("-dirty"),
            "the refusal must name the dirty tree: {}",
            refusal.reason()
        );
    }

    #[test]
    fn a_post_copy_check_uses_the_full_comparator_not_the_version_alone() {
        // The re-check after the copy runs this comparator, so version and protocol are part of
        // custody's single decision rather than a second, weaker probe behind it.
        let custody = custody_fixture();

        let foreign_version = remote_probe_stdout(
            &custody.sha256,
            "0.0.1-foreign",
            &custody.build_sha,
            custody.protocol,
        );
        let refusal = check_remote_custody(&foreign_version, "/remote/zynk", &custody)
            .expect_err("a foreign version must be refused");
        assert!(
            refusal.reason().contains("reports version 0.0.1-foreign"),
            "unexpected refusal: {}",
            refusal.reason()
        );

        let foreign_protocol = remote_probe_stdout(
            &custody.sha256,
            &custody.version,
            &custody.build_sha,
            custody.protocol + 1,
        );
        let refusal = check_remote_custody(&foreign_protocol, "/remote/zynk", &custody)
            .expect_err("a foreign protocol must be refused");
        assert!(
            refusal.reason().contains("speaks protocol"),
            "unexpected refusal: {}",
            refusal.reason()
        );

        let no_status = format!(
            "{}\nzynk {} ({})\n",
            custody.sha256, custody.version, custody.build_sha
        );
        let refusal = check_remote_custody(&no_status, "/remote/zynk", &custody)
            .expect_err("a remote that reports no protocol must be refused");
        assert!(
            refusal
                .reason()
                .contains("did not report its client protocol"),
            "unexpected refusal: {}",
            refusal.reason()
        );
    }

    #[test]
    fn a_reused_remote_binary_with_a_foreign_source_sha_is_not_reused() {
        // The warden's probe: a foreign binary at /opt/foreign/zynk reporting this client's version
        // and protocol with a well-formed all-ones source SHA. Shape validation alone would accept
        // it; only comparing it against the local canonical SHA refuses it.
        let custody = custody_fixture();
        push_stubbed_ssh_output(
            &remote_probe_stdout(
                &custody.sha256,
                &custody.version,
                &"1".repeat(40),
                custody.protocol,
            ),
            true,
        );
        assert!(
            !remote_binary_is_the_reviewed_one("fake-host", &probe_remote_zynk(), &custody)
                .expect("probe"),
            "a remote binary attesting a foreign source commit must not be reused"
        );
    }

    #[test]
    fn a_reused_remote_binary_with_a_different_sha256_is_not_reused() {
        let custody = custody_fixture();
        push_stubbed_ssh_output(
            &remote_probe_stdout(
                &"b".repeat(64),
                &custody.version,
                &custody.build_sha,
                custody.protocol,
            ),
            true,
        );
        assert!(
            !remote_binary_is_the_reviewed_one("fake-host", &probe_remote_zynk(), &custody)
                .expect("probe"),
            "a remote binary with different bytes must not be reused"
        );
    }

    #[test]
    fn a_reused_remote_binary_with_a_foreign_protocol_is_not_reused() {
        let custody = custody_fixture();
        push_stubbed_ssh_output(
            &remote_probe_stdout(
                &custody.sha256,
                &custody.version,
                &custody.build_sha,
                custody.protocol + 1,
            ),
            true,
        );
        assert!(
            !remote_binary_is_the_reviewed_one("fake-host", &probe_remote_zynk(), &custody)
                .expect("probe"),
            "a remote binary speaking another protocol must not be reused"
        );
    }

    #[test]
    fn a_reused_remote_binary_matching_source_and_bytes_is_reused() {
        // The positive control: same bytes, same reviewed commit, same version and protocol.
        let custody = custody_fixture();
        push_stubbed_ssh_output(
            &remote_probe_stdout(
                &custody.sha256,
                &custody.version,
                &custody.build_sha,
                custody.protocol,
            ),
            true,
        );
        assert!(
            remote_binary_is_the_reviewed_one("fake-host", &probe_remote_zynk(), &custody)
                .expect("probe"),
            "the reviewed binary must still be reused"
        );
    }

    #[test]
    fn an_absent_remote_binary_is_not_reused() {
        let custody = custody_fixture();
        push_stubbed_ssh_output("", false);
        assert!(
            !remote_binary_is_the_reviewed_one("fake-host", &probe_remote_zynk(), &custody)
                .expect("probe"),
            "a probe that failed must not be read as a reusable binary"
        );
    }

    #[test]
    fn resolve_install_source_uses_override_binary() {
        let platform = RemotePlatform::local();
        let source = resolve_install_source(&platform, Some(PathBuf::from("/tmp/zynk-linux")))
            .expect("override source");
        assert_eq!(source.path, PathBuf::from("/tmp/zynk-linux"));
    }

    struct TestEnvironmentRestore(Vec<(String, Option<std::ffi::OsString>)>);

    impl Drop for TestEnvironmentRestore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    fn remote_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn socket_path_byte_len(path: &Path) -> usize {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }

    #[test]
    fn local_forward_socket_path_uses_readable_name_when_it_fits() {
        let _guard = remote_env_lock().lock().unwrap();
        // Short target + session leave plenty of room — keep the human-
        // readable form so the socket path stays grep-friendly.
        let path = local_forward_socket_path("dev", "default");
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            filename.starts_with("zynk-remote-"),
            "expected readable name, got {filename}"
        );
        assert!(filename.contains("-dev-default."), "got {filename}");
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[test]
    fn local_forward_socket_path_fits_in_sun_path() {
        let _guard = remote_env_lock().lock().unwrap();
        // Worst case for the readable form: macOS-style 49-char TMPDIR +
        // max-length sanitized components. Should fall back to the hashed
        // short name, which fits under TMPDIR.
        let target = "longish-host.example.com";
        let session = "a-fairly-long-session-name-here";
        let path = local_forward_socket_path(target, session);
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long for sun_path: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[test]
    fn local_forward_socket_path_falls_back_to_tmp_when_dir_is_long() {
        let _guard = remote_env_lock().lock().unwrap();
        // Force a TMPDIR long enough that even the hashed short name cannot
        // fit inside it. The fallback should drop to /tmp.
        let prior = std::env::var_os("TMPDIR");
        let long_dir = std::env::temp_dir().join("a".repeat(80));
        let _ = fs::create_dir_all(&long_dir);
        std::env::set_var("TMPDIR", &long_dir);

        let path = local_forward_socket_path("longish-host.example.com", "default");
        let fits = fits_unix_socket_path(&path);
        let parent = path.parent().map(Path::to_path_buf);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        match prior {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
        let _ = fs::remove_dir_all(&long_dir);

        assert!(fits, "fallback path still overflows: {}", path.display());
        assert_eq!(parent.as_deref(), Some(Path::new("/tmp")));
        assert!(
            filename.starts_with("zynk-r-"),
            "expected hashed fallback, got {filename}"
        );
    }
}
