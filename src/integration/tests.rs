use super::command::*;
use super::config_edit::*;
use super::env::*;
use super::file_ops::*;
use super::registry::*;
use super::targets::*;
use super::types::*;
use super::version::*;
use super::*;

use std::fs;
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;
use serde_json::{json, Map, Value};

#[test]
fn apply_pane_base_env_exports_zynk_socket_path() {
    // The base env every spawned pane receives carries the Zynk-branded
    // `ZYNK_SOCKET_PATH`. Pane/tab/workspace identity is layered on top by
    // `apply_pane_launch_env` (src/pane.rs) and is asserted there.
    let mut cmd = CommandBuilder::new("/bin/sh");
    apply_pane_base_env(&mut cmd);

    let socket = crate::api::socket_path();
    let socket_os = socket.as_os_str();
    assert_eq!(
        cmd.get_env(crate::api::ZYNK_SOCKET_PATH_ENV_VAR),
        Some(socket_os),
        "ZYNK_SOCKET_PATH must be exported to the pane"
    );
    assert_eq!(
        cmd.get_env(crate::api::SOCKET_PATH_ENV_VAR),
        Some(socket_os),
        "ZYNK_SOCKET_PATH compat alias must carry the same value"
    );
}

#[test]
fn apply_pane_base_env_exports_the_running_binary_path() {
    // Hook assets shell out to `zynk pane report-agent-session` instead of
    // speaking the socket protocol, so every pane needs the path of the running
    // binary rather than whatever `zynk` a PATH lookup would find.
    let mut cmd = CommandBuilder::new("/bin/sh");
    apply_pane_base_env(&mut cmd);

    let executable = std::env::current_exe().unwrap();
    assert_eq!(
        cmd.get_env("ZYNK_BIN_PATH"),
        Some(executable.as_os_str()),
        "ZYNK_BIN_PATH must be exported to the pane"
    );
}

#[test]
fn hermes_dir_honors_the_hermes_home_override() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &base);

    assert_eq!(hermes_dir().unwrap(), base.join(".hermes"));

    let relocated = base.join("relocated-hermes");
    std::env::set_var(HERMES_HOME_ENV_VAR, &relocated);
    assert_eq!(hermes_dir().unwrap(), relocated);

    std::env::set_var(HERMES_HOME_ENV_VAR, "");
    assert_eq!(hermes_dir().unwrap(), base.join(".hermes"));

    std::env::remove_var(HERMES_HOME_ENV_VAR);
    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
}

#[test]
fn extract_version_triple_parses_common_outputs() {
    assert_eq!(extract_version_triple("0.14.0"), Some((0, 14, 0)));
    assert_eq!(extract_version_triple("v1.2.3"), Some((1, 2, 3)));
    assert_eq!(
        extract_version_triple("kimi-code 0.14.0 (linux/x64)"),
        Some((0, 14, 0))
    );
    assert_eq!(extract_version_triple("0.14"), Some((0, 14, 0)));
    assert_eq!(extract_version_triple("0.14.1-beta.2"), Some((0, 14, 1)));
    assert_eq!(extract_version_triple("no version here"), None);
    assert_eq!(extract_version_triple(""), None);
}

#[test]
fn extract_version_triple_orders_versions() {
    let old = extract_version_triple("0.12.1").unwrap();
    let min = extract_version_triple(KIMI_MIN_VERSION).unwrap();
    let new = extract_version_triple("0.15.0").unwrap();
    assert!(old < min);
    assert!(min <= min);
    assert!(min < new);
}

#[test]
fn agent_version_requirement_only_set_for_kimi() {
    let requirement = agent_version_requirement(crate::api::schema::IntegrationTarget::Kimi)
        .expect("kimi must have a version requirement");
    assert_eq!(requirement.binary, "kimi");
    assert_eq!(requirement.min_version, KIMI_MIN_VERSION);
    assert!(agent_version_requirement(crate::api::schema::IntegrationTarget::Claude).is_none());
    assert!(agent_version_requirement(crate::api::schema::IntegrationTarget::Codex).is_none());
}

#[test]
fn enforce_agent_version_warns_when_binary_missing() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "zynk-test-binary-that-does-not-exist",
        args: &["--version"],
        min_version: "0.14.0",
    };
    let warning = enforce_agent_version(&requirement)
        .expect("missing binary must not fail the install")
        .expect("missing binary must produce a warning");
    assert!(warning.contains("could not run"));
    assert!(warning.contains("0.14.0"));
}

#[test]
fn enforce_agent_version_rejects_old_version() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "echo",
        args: &["0.12.1"],
        min_version: "0.14.0",
    };
    let err = enforce_agent_version(&requirement).expect_err("old version must fail the install");
    let message = err.to_string();
    assert!(message.contains("0.12.1"));
    assert!(message.contains("0.14.0"));
    assert!(message.contains("upgrade"));
}

#[test]
fn enforce_agent_version_accepts_current_version() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "echo",
        args: &["0.14.0"],
        min_version: "0.14.0",
    };
    let result =
        enforce_agent_version(&requirement).expect("matching version must not fail the install");
    assert!(result.is_none(), "matching version must not warn");
}

fn clear_integration_path_env() {
    std::env::remove_var(PI_CODING_AGENT_DIR_ENV_VAR);
    std::env::remove_var(OMP_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(CLAUDE_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(CODEX_HOME_ENV_VAR);
    std::env::remove_var(COPILOT_HOME_ENV_VAR);
    std::env::remove_var(KIMI_CODE_HOME_ENV_VAR);
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(GROK_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(GROK_HOME_ENV_VAR);
    std::env::remove_var(HERMES_HOME_ENV_VAR);
}

fn kimi_hook_command(hook_path: &Path, action: &str) -> String {
    hook_command(hook_path, Some(action))
}

fn kimi_config_hooks(config: &str) -> Vec<toml::Value> {
    let parsed: toml::Value = toml::from_str(config).unwrap();
    parsed
        .get("hooks")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn assert_kimi_hook(
    config: &str,
    hook_path: &Path,
    event: &str,
    matcher: Option<&str>,
    action: &str,
) {
    let command = kimi_hook_command(hook_path, action);
    let hooks = kimi_config_hooks(config);
    assert!(
        hooks.iter().any(|hook| {
            hook.get("event").and_then(toml::Value::as_str) == Some(event)
                && hook.get("matcher").and_then(toml::Value::as_str) == matcher
                && hook.get("command").and_then(toml::Value::as_str) == Some(command.as_str())
                && hook.get("timeout").and_then(toml::Value::as_integer) == Some(10)
        }),
        "missing kimi hook for {event} ({matcher:?}) -> {action}"
    );
}

fn unique_base() -> PathBuf {
    clear_integration_path_env();
    std::env::temp_dir().join(format!(
        "zynk-integration-install-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn command_available_requires_executable_file_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    let command = bin.join("claude");
    fs::write(&command, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!command_available("claude"));

    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(command_available("claude"));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn codex_availability_finds_standalone_binary_under_codex_home() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let bin = home
        .join(".codex/packages/standalone/releases/0.137.0-test")
        .join("bin");
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(codex_executable_name());
    fs::write(&binary, "").unwrap();
    make_executable(&binary).unwrap();
    let original_home = std::env::var_os("HOME");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", "");

    assert!(integration_target_available(
        crate::api::schema::IntegrationTarget::Codex
    ));

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn integration_recommendations_mark_standalone_codex_available() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let bin = home
        .join(".codex/packages/standalone/releases/0.137.0-test")
        .join("bin");
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(codex_executable_name());
    fs::write(&binary, "").unwrap();
    make_executable(&binary).unwrap();
    let original_home = std::env::var_os("HOME");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", "");

    let codex = integration_recommendations()
        .into_iter()
        .find(|recommendation| {
            recommendation.target == crate::api::schema::IntegrationTarget::Codex
        })
        .expect("codex recommendation should be present");

    assert!(codex.available);
    assert_eq!(codex.state, IntegrationStatusKind::NotInstalled);
    assert!(codex.needs_install());

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn integration_recommendation_installs_available_or_outdated_targets() {
    let mut recommendation = IntegrationRecommendation {
        target: crate::api::schema::IntegrationTarget::Claude,
        label: "claude",
        command: "claude",
        available: false,
        path: PathBuf::from("/tmp/zynk-agent-state.sh"),
        state: IntegrationStatusKind::NotInstalled,
    };
    assert!(!recommendation.needs_install());

    recommendation.available = true;
    assert!(recommendation.needs_install());

    recommendation.available = false;
    recommendation.state = IntegrationStatusKind::Outdated;
    assert!(recommendation.needs_install());

    recommendation.available = true;
    recommendation.state = IntegrationStatusKind::Current;
    assert!(!recommendation.needs_install());
}

#[test]
fn install_pi_writes_embedded_asset_to_pi_extensions_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var("HOME", &home);

    let path = install_pi().unwrap();
    let content = fs::read_to_string(&path).unwrap();

    assert_eq!(path, ext_dir.join(PI_EXTENSION_INSTALL_NAME));
    assert_eq!(content, PI_EXTENSION_ASSET);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_creates_extensions_dir_when_agent_dir_exists() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let agent_dir = home.join(".pi/agent");
    fs::create_dir_all(&agent_dir).unwrap();
    std::env::set_var("HOME", &home);

    let path = install_pi().unwrap();

    assert_eq!(
        path,
        agent_dir.join("extensions").join(PI_EXTENSION_INSTALL_NAME)
    );
    assert!(path.is_file());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_uses_pi_coding_agent_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agent_dir = base.join("custom-pi-agent");
    let ext_dir = agent_dir.join("extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, &agent_dir);

    let path = install_pi().unwrap();

    assert_eq!(path, ext_dir.join(PI_EXTENSION_INSTALL_NAME));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_expands_tilde_in_pi_coding_agent_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join("custom-pi-agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, "~/custom-pi-agent");

    let path = install_pi().unwrap();

    assert_eq!(path, ext_dir.join(PI_EXTENSION_INSTALL_NAME));

    std::env::remove_var("HOME");
    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_writes_embedded_asset_to_omp_extensions_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();
    let content = fs::read_to_string(&installed.extension_path).unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(!installed.removed_legacy_pi_extension);
    assert_eq!(content, OMP_EXTENSION_ASSET);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_removes_legacy_pi_integration_from_omp_extensions_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let legacy_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&legacy_path, PI_EXTENSION_ASSET).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(installed.removed_legacy_pi_extension);
    assert!(!legacy_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_preserves_non_zynk_file_with_pi_install_name() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let user_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&user_path, "// user extension\n").unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(!installed.removed_legacy_pi_extension);
    assert_eq!(
        fs::read_to_string(user_path).unwrap(),
        "// user extension\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_uses_pi_config_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join("custom-omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var(OMP_CONFIG_DIR_ENV_VAR, "custom-omp");

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(!installed.removed_legacy_pi_extension);

    std::env::remove_var("HOME");
    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_refuses_shared_pi_extension_directory() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agent_dir = base.join("shared-agent");
    let ext_dir = agent_dir.join("extensions");
    let pi_extension = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(&pi_extension, PI_EXTENSION_ASSET).unwrap();
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, &agent_dir);
    std::env::set_var(OMP_CONFIG_DIR_ENV_VAR, "ignored-omp-config");

    let err = install_omp().unwrap_err().to_string();

    assert!(err.contains("Pi and OMP resolve to the same extension directory"));
    assert!(err.contains(&ext_dir.display().to_string()));
    assert!(pi_extension.is_file());
    assert!(!ext_dir.join(OMP_EXTENSION_INSTALL_NAME).exists());

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_creates_extensions_dir_when_agent_dir_exists() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let agent_dir = home.join(".omp/agent");
    let ext_dir = agent_dir.join("extensions");
    fs::create_dir_all(&agent_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(ext_dir.is_dir());
    assert!(!installed.removed_legacy_pi_extension);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_omp_removes_embedded_extension_when_present() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME),
        OMP_EXTENSION_ASSET,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_omp().unwrap();

    assert_eq!(
        result.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(result.removed_extension);
    assert!(!result.extension_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_errors_when_extension_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_omp().unwrap_err().to_string();

    assert!(err.contains("omp extension directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_pi_removes_embedded_extension_when_present() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(ext_dir.join(PI_EXTENSION_INSTALL_NAME), PI_EXTENSION_ASSET).unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_pi().unwrap();

    assert_eq!(
        result.extension_path,
        ext_dir.join(PI_EXTENSION_INSTALL_NAME)
    );
    assert!(result.removed_extension);
    assert!(!result.extension_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_treat_missing_version_marker_as_legacy() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let extension_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&extension_path, "// installed by zynk\n").unwrap();
    std::env::set_var("HOME", &home);

    let outdated = outdated_installed_integrations();

    assert_eq!(outdated.len(), 1);
    assert_eq!(
        outdated[0].target,
        crate::api::schema::IntegrationTarget::Pi
    );
    assert_eq!(outdated[0].path, extension_path);
    assert_eq!(outdated[0].installed_version, None);
    assert_eq!(outdated[0].expected_version, PI_INTEGRATION_VERSION);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_detect_previous_pi_version() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let extension_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(
        &extension_path,
        "// ZYNK_INTEGRATION_ID=pi\n// ZYNK_INTEGRATION_VERSION=4\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let outdated = outdated_installed_integrations();

    assert_eq!(outdated.len(), 1);
    assert_eq!(
        outdated[0].target,
        crate::api::schema::IntegrationTarget::Pi
    );
    assert_eq!(outdated[0].path, extension_path);
    assert_eq!(outdated[0].installed_version, Some(4));
    assert_eq!(outdated[0].expected_version, PI_INTEGRATION_VERSION);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_detect_previous_omp_version() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let extension_path = ext_dir.join(OMP_EXTENSION_INSTALL_NAME);
    fs::write(
        &extension_path,
        "// ZYNK_INTEGRATION_ID=omp\n// ZYNK_INTEGRATION_VERSION=4\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let outdated = outdated_installed_integrations();

    assert_eq!(outdated.len(), 1);
    assert_eq!(
        outdated[0].target,
        crate::api::schema::IntegrationTarget::Omp
    );
    assert_eq!(outdated[0].path, extension_path);
    assert_eq!(outdated[0].installed_version, Some(4));
    assert_eq!(outdated[0].expected_version, OMP_INTEGRATION_VERSION);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_accept_current_version_marker() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(ext_dir.join(PI_EXTENSION_INSTALL_NAME), PI_EXTENSION_ASSET).unwrap();
    std::env::set_var("HOME", &home);

    assert!(outdated_installed_integrations().is_empty());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_errors_when_extension_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_pi().unwrap_err().to_string();

    assert!(err.contains("pi extension directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(
        claude_dir.join("settings.json"),
        r#"{"permissions":{"allow":["Read"]},"hooks":{}}"#,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_claude().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();

    assert_eq!(
        installed.hook_path,
        claude_dir.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME)
    );
    assert_eq!(hook_content, CLAUDE_HOOK_ASSET);
    assert!(settings["permissions"]["allow"].is_array());
    assert_eq!(settings["hooks"]["SessionStart"][0]["matcher"], "*");
    assert!(settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(" session"));
    assert!(settings["hooks"].get("UserPromptSubmit").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("PostToolUseFailure").is_none());
    assert!(settings["hooks"].get("SubagentStop").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_uses_claude_config_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let claude_dir = base.join("custom-claude");
    fs::create_dir_all(&claude_dir).unwrap();
    std::env::set_var(CLAUDE_CONFIG_DIR_ENV_VAR, &claude_dir);

    let installed = install_claude().unwrap();

    assert_eq!(installed.settings_path, claude_dir.join("settings.json"));
    assert_eq!(
        installed.hook_path,
        claude_dir.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME)
    );

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    std::env::set_var("HOME", &home);

    install_claude().unwrap();
    install_claude().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["hooks"]["SessionStart"].as_array().unwrap().len(),
        1
    );
    assert!(settings["hooks"].get("UserPromptSubmit").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("PostToolUseFailure").is_none());
    assert!(settings["hooks"].get("SubagentStop").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_removes_deprecated_completion_hooks_and_preserves_user_hooks() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    let settings = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-post", "timeout": 10}
                ]
            }],
            "PostToolUseFailure": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-failure", "timeout": 10}
                ]
            }],
            "SubagentStop": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-subagent", "timeout": 10}
                ]
            }],
            "SessionEnd": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' release", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-session-end", "timeout": 10}
                ]
            }]
        }
    });
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_claude().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "echo keep-post"
    );
    assert_eq!(
        settings["hooks"]["PostToolUseFailure"][0]["hooks"][0]["command"],
        "echo keep-failure"
    );
    assert_eq!(
        settings["hooks"]["SubagentStop"][0]["hooks"][0]["command"],
        "echo keep-subagent"
    );
    assert_eq!(
        settings["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
        "echo keep-session-end"
    );
    assert!(settings["hooks"].get("UserPromptSubmit").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("Stop").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

// Round-trip fixture for the jsonc-parser rewrite (upstream d742e515): a real
// `install_claude()` + `uninstall_claude()` pass over a hand-formatted
// settings.json must leave every untouched byte alone -- 4-space indent, the
// user's own (non-alphabetical) key order, ` : ` separators, the `\u0061`
// escape, the `1e+02` literal, and the trailing blank line. The old
// serde_json::to_string_pretty round-trip destroyed all six.
#[test]
fn install_then_uninstall_claude_restores_original_settings_bytes() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let settings_path = claude_dir.join("settings.json");
    let original = concat!(
        "{\n",
        "    \"zeta\" : {\"escaped\":\"\\u0061\", \"number\":1e+02},\n",
        "    \"hooks\" : {\n",
        "        \"Notification\" : [{\"matcher\":\"keep\",\"hooks\":[{  \"type\" : \"command\", \"command\" : \"echo keep\"  }]}]\n",
        "    },\n",
        "    \"alpha\" : 1\n",
        "}\n\n",
    );
    fs::write(&settings_path, original).unwrap();
    std::env::set_var("HOME", &home);

    install_claude().unwrap();
    let installed = fs::read_to_string(&settings_path).unwrap();

    // Everything before and after the edited `hooks` object is byte-identical,
    // and the only textual change inside it is the appended SessionStart entry.
    assert!(
        installed.starts_with(concat!(
            "{\n",
            "    \"zeta\" : {\"escaped\":\"\\u0061\", \"number\":1e+02},\n",
            "    \"hooks\" : {\n",
            "        \"Notification\" : [{\"matcher\":\"keep\",\"hooks\":[{  \"type\" : \"command\", \"command\" : \"echo keep\"  }]}],\n",
        )),
        "{installed}"
    );
    assert!(
        installed.ends_with(concat!("\n    },\n", "    \"alpha\" : 1\n", "}\n\n")),
        "{installed}"
    );
    assert!(installed.contains("\"SessionStart\""), "{installed}");
    // The user's key order survives; a serde_json round-trip would sort it.
    let zeta = installed.find("\"zeta\"").unwrap();
    let hooks = installed.find("\"hooks\"").unwrap();
    let alpha = installed.find("\"alpha\"").unwrap();
    assert!(zeta < hooks && hooks < alpha, "{installed}");
    let parsed: Value = serde_json::from_str(&installed).unwrap();
    assert_eq!(parsed["zeta"]["number"], 100.0);
    assert_eq!(parsed["hooks"]["SessionStart"][0]["matcher"], "*");

    let removal = uninstall_claude().unwrap();

    assert!(removal.updated_settings);
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), original);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

// Comments are NOT preserved by the jsonc-parser rewrite -- despite the crate
// name, `parse_value` is plain `serde_json::from_str` and `strict_parse_options`
// sets `allow_comments: false`, so a JSONC settings.json is rejected exactly as
// it was before this port. Pin that: install fails with a parse error and leaves
// the user's file byte-for-byte untouched rather than rewriting it.
#[test]
fn install_claude_rejects_settings_with_comments_and_leaves_the_file_untouched() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let settings_path = claude_dir.join("settings.json");
    let original = concat!(
        "{\n",
        "    // keep my notes\n",
        "    \"alpha\" : 1\n",
        "}\n",
    );
    fs::write(&settings_path, original).unwrap();
    std::env::set_var("HOME", &home);

    let error = install_claude().unwrap_err().to_string();

    assert!(error.contains("failed to parse"), "{error}");
    assert!(
        error.contains(&settings_path.display().to_string()),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), original);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn claude_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_hooks_dir = home.join(".claude").join("hooks");
    fs::create_dir_all(&claude_hooks_dir).unwrap();
    let hook_path = claude_hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=claude\n# ZYNK_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let claude = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Claude)
        .unwrap();

    assert_eq!(claude.path, hook_path);
    assert_eq!(claude.installed_version, Some(1));
    assert_eq!(claude.expected_version, 7);
    assert_eq!(claude.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn claude_v2_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_hooks_dir = home.join(".claude").join("hooks");
    fs::create_dir_all(&claude_hooks_dir).unwrap();
    let hook_path = claude_hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=claude\n# ZYNK_INTEGRATION_VERSION=2\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let claude = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Claude)
        .unwrap();

    assert_eq!(claude.path, hook_path);
    assert_eq!(claude.installed_version, Some(2));
    assert_eq!(claude.expected_version, 7);
    assert_eq!(claude.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_claude_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CLAUDE_HOOK_ASSET).unwrap();
    let settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]
            }],
            "UserPromptSubmit": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep", "timeout": 10}
                ]
            }],
            "PermissionRequest": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' blocked", hook_path.display()), "timeout": 10}]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]
            }],
            "PostToolUseFailure": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]
            }],
            "SubagentStop": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]
            }],
            "Stop": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]
            }],
            "SessionEnd": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' release", hook_path.display()), "timeout": 10}]
            }]
        }
    });
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_claude().unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_settings);
    assert!(!result.hook_path.exists());
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("SessionStart").is_none());
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("PostToolUseFailure").is_none());
    assert!(settings["hooks"].get("SubagentStop").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_errors_when_claude_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_claude().unwrap_err().to_string();

    assert!(err.contains("claude directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn codex_v2_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=codex\n# ZYNK_INTEGRATION_VERSION=2\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let codex = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Codex)
        .unwrap();

    assert_eq!(codex.path, hook_path);
    assert_eq!(codex.installed_version, Some(2));
    assert_eq!(codex.expected_version, 7);
    assert_eq!(codex.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_writes_hook_and_updates_hooks_and_config() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.4\"\n").unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_codex().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(&installed.hooks_path).unwrap()).unwrap();
    let config = fs::read_to_string(&installed.config_path).unwrap();

    assert_eq!(installed.hook_path, codex_dir.join(CODEX_HOOK_INSTALL_NAME));
    assert_eq!(installed.hooks_path, codex_dir.join("hooks.json"));
    assert_eq!(installed.config_path, codex_dir.join("config.toml"));
    assert_eq!(hook_content, CODEX_HOOK_ASSET);
    assert!(hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(" session"));
    assert!(hooks["hooks"].get("UserPromptSubmit").is_none());
    assert!(hooks["hooks"].get("PreToolUse").is_none());
    assert!(hooks["hooks"].get("PermissionRequest").is_none());
    assert!(hooks["hooks"].get("Stop").is_none());
    assert!(config.contains("model = \"gpt-5.4\""));
    assert!(config.contains("[features]"));
    assert!(config.contains("hooks = true"));
    assert!(!config.contains("codex_hooks"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_uses_codex_home_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let codex_dir = base.join("custom-codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.4\"\n").unwrap();
    std::env::set_var(CODEX_HOME_ENV_VAR, &codex_dir);

    let installed = install_codex().unwrap();

    assert_eq!(installed.hook_path, codex_dir.join(CODEX_HOOK_INSTALL_NAME));
    assert_eq!(installed.hooks_path, codex_dir.join("hooks.json"));
    assert_eq!(installed.config_path, codex_dir.join("config.toml"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_is_idempotent_for_hook_entries_and_feature_flag() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        "[features]\ncodex_hooks = false\nother = true\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_codex().unwrap();
    install_codex().unwrap();

    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap()).unwrap();
    let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert_eq!(hooks["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert!(hooks["hooks"].get("UserPromptSubmit").is_none());
    assert!(hooks["hooks"].get("PreToolUse").is_none());
    assert!(hooks["hooks"].get("PermissionRequest").is_none());
    assert!(hooks["hooks"].get("Stop").is_none());
    assert_eq!(config.matches("hooks = true").count(), 1);
    assert!(!config.contains("codex_hooks"));
    assert!(config.contains("other = true"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_only_migrates_top_level_feature_flags() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        "profile = \"work\"\n\n[profiles.work.features]\nhooks = false\ncodex_hooks = false\n\n[features]\ncodex_hooks = true\nother = true\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_codex().unwrap();

    let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert!(config.contains("[profiles.work.features]\nhooks = false\ncodex_hooks = false"));
    assert!(config.contains("[features]\nhooks = true\nother = true"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_codex_removes_zynk_hooks_and_leaves_config_alone() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CODEX_HOOK_ASSET).unwrap();
    let hooks = serde_json::json!({
        "hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]}],
            "UserPromptSubmit": [{"hooks": [
                {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                {"type": "command", "command": "echo keep", "timeout": 10}
            ]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]}],
            "PermissionRequest": [{"hooks": [{"type": "command", "command": format!("bash '{}' blocked", hook_path.display()), "timeout": 10}]}],
            "Stop": [{"hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]}]
        }
    });
    fs::write(
        codex_dir.join("hooks.json"),
        serde_json::to_string(&hooks).unwrap(),
    )
    .unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        "[features]\nhooks = true\nother = true\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_codex().unwrap();
    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap()).unwrap();
    let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(!result.hook_path.exists());
    assert!(hooks["hooks"].get("SessionStart").is_none());
    assert!(hooks["hooks"].get("PreToolUse").is_none());
    assert!(hooks["hooks"].get("PermissionRequest").is_none());
    assert!(hooks["hooks"].get("Stop").is_none());
    assert_eq!(
        hooks["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert!(config.contains("hooks = true"));
    assert!(config.contains("other = true"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_codex().unwrap_err().to_string();

    assert!(err.contains("codex config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kimi_writes_hook_and_updates_config() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kimi_dir = home.join(".kimi-code");
    fs::create_dir_all(&kimi_dir).unwrap();
    fs::write(
        kimi_dir.join("config.toml"),
        "default_model = \"moonshot\"\n\n[[hooks]]\nevent = \"Notification\"\nmatcher = \"task.completed\"\ncommand = \"echo keep\"\ntimeout = 3\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_kimi().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let config = fs::read_to_string(&installed.config_path).unwrap();
    let hooks = kimi_config_hooks(&config);

    assert_eq!(
        installed.hook_path,
        kimi_dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.config_path, kimi_dir.join("config.toml"));
    assert_eq!(hook_content, KIMI_HOOK_ASSET);
    assert_eq!(hooks.len(), KIMI_HOOK_EVENTS.len() + 1);
    assert!(config.contains("default_model = \"moonshot\""));
    assert!(config.contains("command = \"echo keep\""));
    assert!(config.contains(KIMI_CONFIG_BLOCK_BEGIN));
    assert!(config.contains(KIMI_CONFIG_BLOCK_END));
    for (event, matcher, action) in KIMI_HOOK_EVENTS {
        assert_kimi_hook(&config, &installed.hook_path, event, matcher, action);
    }

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn kimi_question_hooks_report_blocked_until_the_question_finishes() {
    assert!(KIMI_HOOK_EVENTS.contains(&(
        "PreToolUse",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "blocked",
    )));
    assert!(KIMI_HOOK_EVENTS.contains(&(
        "PostToolUse",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "working",
    )));
    assert!(KIMI_HOOK_EVENTS.contains(&(
        "PostToolUseFailure",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "working",
    )));
    assert!(KIMI_HOOK_EVENTS.contains(&("PreToolUse", Some(KIMI_OTHER_TOOL_MATCHER), "working",)));
}

#[test]
fn install_kimi_uses_kimi_code_home_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let kimi_dir = base.join("custom-kimi");
    fs::create_dir_all(&kimi_dir).unwrap();
    std::env::set_var(KIMI_CODE_HOME_ENV_VAR, &kimi_dir);

    let installed = install_kimi().unwrap();

    assert_eq!(
        installed.hook_path,
        kimi_dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.config_path, kimi_dir.join("config.toml"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kimi_is_idempotent_for_config_block() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kimi_dir = home.join(".kimi-code");
    fs::create_dir_all(&kimi_dir).unwrap();
    std::env::set_var("HOME", &home);

    install_kimi().unwrap();
    install_kimi().unwrap();

    let config = fs::read_to_string(kimi_dir.join("config.toml")).unwrap();
    let hooks = kimi_config_hooks(&config);

    assert_eq!(config.matches(KIMI_CONFIG_BLOCK_BEGIN).count(), 1);
    assert_eq!(config.matches(KIMI_CONFIG_BLOCK_END).count(), 1);
    assert_eq!(hooks.len(), KIMI_HOOK_EVENTS.len());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_kimi_removes_hook_and_config_block_preserves_other_hooks() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kimi_dir = home.join(".kimi-code");
    fs::create_dir_all(&kimi_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_kimi().unwrap();
    fs::write(
        &installed.config_path,
        format!(
            "default_model = \"moonshot\"\n\n[[hooks]]\nevent = \"Notification\"\ncommand = \"echo keep\"\n\n{}",
            fs::read_to_string(&installed.config_path).unwrap()
        ),
    )
    .unwrap();

    let result = uninstall_kimi().unwrap();
    let config = fs::read_to_string(kimi_dir.join("config.toml")).unwrap();
    let hooks = kimi_config_hooks(&config);

    assert!(result.removed_hook_file);
    assert!(result.updated_config);
    assert!(!result.hook_path.exists());
    assert!(config.contains("default_model = \"moonshot\""));
    assert!(config.contains("command = \"echo keep\""));
    assert!(!config.contains(KIMI_CONFIG_BLOCK_BEGIN));
    assert!(!config.contains(KIMI_CONFIG_BLOCK_END));
    assert_eq!(hooks.len(), 1);
    assert_eq!(
        hooks[0].get("event").and_then(toml::Value::as_str),
        Some("Notification")
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kimi_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_kimi().unwrap_err().to_string();

    assert!(err.contains("kimi code config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_copilot_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let copilot_dir = home.join(".copilot");
    fs::create_dir_all(&copilot_dir).unwrap();
    let hook_path = copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME);
    let stale_session_start_command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    fs::write(
        copilot_dir.join("settings.json"),
        format!(
            r#"{{"theme":"dark","hooks":{{"PreToolUse":[{{"type":"command","command":"echo keep","timeoutSec":10}}],"sessionStart":[{{"type":"command","bash":{},"timeoutSec":10}}]}}}}"#,
            serde_json::to_string(&stale_session_start_command).unwrap()
        ),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_copilot().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();

    assert_eq!(
        installed.hook_path,
        copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.settings_path, copilot_dir.join("settings.json"));
    assert_eq!(hook_content, COPILOT_HOOK_ASSET);
    assert_eq!(settings["theme"], "dark");
    assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(settings["hooks"]["PreToolUse"][0]["command"], "echo keep");
    assert!(settings["hooks"]["SessionStart"][0][direct_command_field()]
        .as_str()
        .unwrap()
        .contains(COPILOT_HOOK_INSTALL_NAME));
    for event in COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS {
        if let Some(entries) = settings["hooks"].get(event) {
            assert!(
                !entries.to_string().contains(COPILOT_HOOK_INSTALL_NAME),
                "expected zynk hooks.{event} entries to be removed"
            );
        }
    }
    assert!(settings["hooks"].get("sessionStart").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn copilot_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let copilot_hooks_dir = home.join(".copilot").join("hooks");
    fs::create_dir_all(&copilot_hooks_dir).unwrap();
    let hook_path = copilot_hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=copilot\n# ZYNK_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let copilot = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Copilot)
        .unwrap();

    assert_eq!(copilot.path, hook_path);
    assert_eq!(copilot.installed_version, Some(1));
    assert_eq!(copilot.expected_version, COPILOT_INTEGRATION_VERSION);
    assert_eq!(copilot.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

fn write_copilot_hook(home: &Path, contents: &str) -> PathBuf {
    let copilot_hooks_dir = home.join(".copilot").join("hooks");
    fs::create_dir_all(&copilot_hooks_dir).unwrap();
    let hook_path = copilot_hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(&hook_path, contents).unwrap();
    hook_path
}

fn copilot_status() -> IntegrationStatus {
    installed_integration_statuses()
        .into_iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Copilot)
        .expect("copilot integration status")
}

#[test]
fn stale_herdr_copilot_v2_hook_is_outdated_not_current() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    // Mirrors the incident: a pre-rebrand Herdr hook whose version marker was
    // bumped to v2 but which still identifies as `herdr:copilot` and carries
    // HERDR_* residue. It must NOT be reported as a current native hook.
    write_copilot_hook(
        &home,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=copilot\n# ZYNK_INTEGRATION_VERSION=2\nsource=\"herdr:copilot\"\nexport HERDR_SOCKET_PATH=/tmp/herdr.sock\n",
    );
    std::env::set_var("HOME", &home);

    let copilot = copilot_status();
    assert_eq!(copilot.installed_version, Some(2));
    assert_eq!(
        copilot.state,
        IntegrationStatusKind::Outdated,
        "stale Herdr-era copilot hook must be Outdated, not Current"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn copilot_v2_hook_missing_integration_id_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    write_copilot_hook(&home, "#!/bin/sh\n# ZYNK_INTEGRATION_VERSION=2\n");
    std::env::set_var("HOME", &home);

    let copilot = copilot_status();
    assert_eq!(copilot.installed_version, Some(2));
    assert_eq!(copilot.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn copilot_v2_hook_with_foreign_integration_id_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    write_copilot_hook(
        &home,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=cursor\n# ZYNK_INTEGRATION_VERSION=2\n",
    );
    std::env::set_var("HOME", &home);

    let copilot = copilot_status();
    assert_eq!(copilot.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn native_copilot_v2_hook_is_current() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    write_copilot_hook(
        &home,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=copilot\n# ZYNK_INTEGRATION_VERSION=2\nsource=\"zynk:copilot\"\n",
    );
    std::env::set_var("HOME", &home);

    let copilot = copilot_status();
    assert_eq!(copilot.installed_version, Some(2));
    assert_eq!(copilot.state, IntegrationStatusKind::Current);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn hook_has_herdr_residue_flags_legacy_tokens() {
    assert!(hook_has_herdr_residue("source=\"herdr:copilot\""));
    assert!(hook_has_herdr_residue("export HERDR_SOCKET_PATH=/tmp/x"));
    assert!(!hook_has_herdr_residue(
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=copilot\nsource=\"zynk:copilot\"\n"
    ));
}

#[test]
fn hook_is_native_requires_matching_id_and_no_residue() {
    let native = "#!/bin/sh\n# ZYNK_INTEGRATION_ID=copilot\n# ZYNK_INTEGRATION_VERSION=2\n";
    assert!(hook_is_native(native, "copilot"));
    // Correct marker shape but a different target's id.
    assert!(!hook_is_native(native, "cursor"));
    // No id marker at all.
    assert!(!hook_is_native(
        "#!/bin/sh\n# ZYNK_INTEGRATION_VERSION=2\n",
        "copilot"
    ));
    // Matching id but Herdr residue present.
    assert!(!hook_is_native(
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=copilot\nsource=\"herdr:copilot\"\n",
        "copilot"
    ));
}

#[test]
fn all_native_status_assets_pass_identity_gate() {
    use crate::api::schema::IntegrationTarget;
    // Every integration whose status path embeds the id marker, paired with the
    // asset installed to that path. Guards against a legitimate current integration
    // being false-failed by the identity gate, and forces this list to grow in
    // lockstep with `integration_specs()` when a new integration is added.
    let assets: &[(IntegrationTarget, &str)] = &[
        (IntegrationTarget::Pi, PI_EXTENSION_ASSET),
        (IntegrationTarget::Omp, OMP_EXTENSION_ASSET),
        (IntegrationTarget::Claude, CLAUDE_HOOK_ASSET),
        (IntegrationTarget::Codex, CODEX_HOOK_ASSET),
        (IntegrationTarget::Copilot, COPILOT_HOOK_ASSET),
        (IntegrationTarget::Devin, DEVIN_HOOK_ASSET),
        (IntegrationTarget::Droid, DROID_HOOK_ASSET),
        (IntegrationTarget::Kimi, KIMI_HOOK_ASSET),
        (IntegrationTarget::Opencode, OPENCODE_PLUGIN_ASSET),
        (IntegrationTarget::Kilo, KILO_PLUGIN_ASSET),
        (IntegrationTarget::Hermes, HERMES_PLUGIN_INIT_ASSET),
        (IntegrationTarget::Qodercli, QODERCLI_HOOK_ASSET),
        (IntegrationTarget::Cursor, CURSOR_HOOK_ASSET),
        (IntegrationTarget::Mastracode, MASTRACODE_HOOK_ASSET),
        (
            IntegrationTarget::AntigravityCli,
            ANTIGRAVITY_CLI_HOOK_ASSET,
        ),
        (IntegrationTarget::Grok, GROK_HOOK_ASSET),
    ];
    assert_eq!(
        assets.len(),
        integration_specs().len(),
        "native-asset identity coverage must enumerate every integration spec"
    );
    for (target, asset) in assets {
        let id = expected_integration_id(*target);
        assert!(
            asset.contains(&format!("{INTEGRATION_ID_MARKER}{id}")),
            "{id} status asset is missing its ZYNK_INTEGRATION_ID marker"
        );
        assert!(
            hook_is_native(asset, id),
            "{id} native status asset must pass the identity gate"
        );
    }
}

#[test]
fn install_copilot_uses_copilot_home_env_and_is_idempotent() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let copilot_dir = base.join("custom-copilot");
    fs::create_dir_all(&copilot_dir).unwrap();
    std::env::set_var(COPILOT_HOME_ENV_VAR, &copilot_dir);

    let installed = install_copilot().unwrap();
    install_copilot().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(copilot_dir.join("settings.json")).unwrap())
            .unwrap();

    assert_eq!(
        installed.hook_path,
        copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME)
    );
    assert_eq!(
        settings["hooks"]["SessionStart"].as_array().unwrap().len(),
        1
    );
    for event in COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS {
        assert!(
            settings["hooks"].get(event).is_none(),
            "expected hooks.{event} to be absent"
        );
    }
    assert!(settings["hooks"].get("sessionStart").is_none());

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_copilot_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let copilot_dir = home.join(".copilot");
    let hooks_dir = copilot_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(&hook_path, COPILOT_HOOK_ASSET).unwrap();
    let command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {"type": "command", direct_command_field(): command, "timeoutSec": 10},
                {"type": "command", "command": "echo keep", "timeoutSec": 10}
            ],
            "PostToolUse": [{"type": "command", direct_command_field(): command, "timeoutSec": 10}],
            "notification": [{
                "type": "command",
                "matcher": "permission_prompt|elicitation_dialog|agent_idle",
                direct_command_field(): command,
                "timeoutSec": 10
            }]
        }
    });
    fs::write(
        copilot_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_copilot().unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(copilot_dir.join("settings.json")).unwrap())
            .unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_settings);
    assert!(!result.hook_path.exists());
    assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(settings["hooks"]["PreToolUse"][0]["command"], "echo keep");
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("notification").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_copilot_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_copilot().unwrap_err().to_string();

    assert!(err.contains("copilot config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    fs::write(
        devin_dir.join("config.json"),
        r#"{"theme_mode":"dark","hooks":{}}"#,
    )
    .unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    let installed = install_devin().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();

    assert_eq!(installed.hook_path, devin_dir.join(DEVIN_HOOK_INSTALL_NAME));
    assert_eq!(installed.settings_path, devin_dir.join("config.json"));
    assert_eq!(hook_content, DEVIN_HOOK_ASSET);
    assert_eq!(settings["theme_mode"], "dark");
    for (event, action) in DEVIN_HOOK_EVENTS {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            command.contains(DEVIN_HOOK_INSTALL_NAME) && command.ends_with(action),
            "expected devin {event} hook command to end with {action}, got {command}"
        );
    }

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    install_devin().unwrap();
    install_devin().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();
    for (event, _) in DEVIN_HOOK_EVENTS {
        assert_eq!(
            settings["hooks"][event].as_array().unwrap().len(),
            1,
            "expected hooks.{event} to be idempotent"
        );
    }

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_removes_legacy_lifecycle_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    let hook_path = devin_dir.join(DEVIN_HOOK_INSTALL_NAME);
    let mut hooks = Map::new();
    for (event, action) in DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS {
        hooks.insert(
            event.to_string(),
            json!([
                {
                    "hooks": [{
                        "type": "command",
                        "command": hook_command(&hook_path, Some(action)),
                        "timeout": 10
                    }]
                }
            ]),
        );
    }
    fs::write(
        devin_dir.join("config.json"),
        serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap(),
    )
    .unwrap();

    install_devin().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();
    for (event, action) in DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS {
        let legacy_command = hook_command(&hook_path, Some(action));
        let entries = settings["hooks"][event].as_array();
        assert!(
            entries.is_none_or(|entries| {
                entries.iter().all(|entry| {
                    entry
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_none_or(|hooks| {
                            hooks.iter().all(|hook| {
                                hook.get("command").and_then(Value::as_str)
                                    != Some(legacy_command.as_str())
                            })
                        })
                })
            }),
            "expected legacy devin {event} -> {action} hook to be removed"
        );

        if !DEVIN_HOOK_EVENTS
            .iter()
            .any(|(installed_event, _)| installed_event == &event)
        {
            continue;
        }

        let session_command = hook_command(&hook_path, Some("session"));
        let entries = entries.unwrap();
        assert!(
            entries.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command").and_then(Value::as_str)
                                == Some(session_command.as_str())
                        })
                    })
            }),
            "expected devin {event} session hook to be installed"
        );
    }

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_devin_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    install_devin().unwrap();

    let hook_path = devin_dir.join(DEVIN_HOOK_INSTALL_NAME);
    let mut settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();
    settings["hooks"]["UserPromptSubmit"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": "echo keep",
                "timeout": 10
            }]
        }));
    fs::write(
        devin_dir.join("config.json"),
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    let result = uninstall_devin().unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_settings);
    assert!(!hook_path.exists());
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert!(settings["hooks"].get("SessionStart").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    fs::create_dir_all(&xdg_config).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    let err = install_devin().unwrap_err().to_string();
    assert!(err.contains("devin config directory not found"));

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_droid_writes_hook_to_settings_and_cleans_legacy_hooks_json() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_dir = home.join(".factory");
    let legacy_hook_path = droid_dir.join("hooks").join(DROID_HOOK_INSTALL_NAME);
    fs::create_dir_all(legacy_hook_path.parent().unwrap()).unwrap();
    fs::create_dir_all(&droid_dir).unwrap();
    let legacy_command = format!(
        "bash {}",
        shell_single_quote(&legacy_hook_path.display().to_string())
    );
    fs::write(
        droid_dir.join("hooks.json"),
        format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}","timeout":10}}]}}],"PreToolUse":[{{"matcher":"Read","hooks":[{{"type":"command","command":"echo keep","timeout":10}}]}}]}}}}"#,
            legacy_command,
        ),
    )
    .unwrap();
    fs::write(
        droid_dir.join("settings.json"),
        r#"{"theme":"factory-dark"}"#,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_droid().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();
    let legacy_hooks: Value =
        serde_json::from_str(&fs::read_to_string(&installed.hooks_path).unwrap()).unwrap();

    assert_eq!(
        installed.hook_path,
        droid_dir.join("hooks").join(DROID_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, droid_dir.join("hooks.json"));
    assert_eq!(installed.settings_path, droid_dir.join("settings.json"));
    assert!(installed.updated_legacy_hooks);
    assert_eq!(hook_content, DROID_HOOK_ASSET);
    assert_eq!(settings["theme"], "factory-dark");
    assert!(settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(DROID_HOOK_INSTALL_NAME));
    assert!(settings["hooks"]["SessionStart"][0]
        .get("matcher")
        .is_none());
    for (event, action) in DROID_HOOK_EVENTS {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            command.contains(DROID_HOOK_INSTALL_NAME) && command.ends_with(action),
            "expected droid {event} hook command to end with {action}, got {command}"
        );
    }
    assert_eq!(legacy_hooks["hooks"]["PreToolUse"][0]["matcher"], "Read");
    assert!(legacy_hooks["hooks"].get("SessionStart").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_droid_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_dir = home.join(".factory");
    fs::create_dir_all(&droid_dir).unwrap();
    std::env::set_var("HOME", &home);

    install_droid().unwrap();
    install_droid().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(droid_dir.join("settings.json")).unwrap())
            .unwrap();
    for (event, _) in DROID_HOOK_EVENTS {
        assert_eq!(
            settings["hooks"][event].as_array().unwrap().len(),
            1,
            "expected hooks.{event} to be idempotent"
        );
    }

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn droid_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_hooks_dir = home.join(".factory").join("hooks");
    fs::create_dir_all(&droid_hooks_dir).unwrap();
    let hook_path = droid_hooks_dir.join(DROID_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=droid\n# ZYNK_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let droid = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Droid)
        .unwrap();

    assert_eq!(droid.path, hook_path);
    assert_eq!(droid.installed_version, Some(1));
    assert_eq!(droid.expected_version, DROID_INTEGRATION_VERSION);
    assert_eq!(droid.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_droid_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_dir = home.join(".factory");
    let hooks_dir = droid_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(DROID_HOOK_INSTALL_NAME);
    fs::write(&hook_path, DROID_HOOK_ASSET).unwrap();
    let command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    fs::write(
        droid_dir.join("hooks.json"),
        format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}","timeout":10}},{{"type":"command","command":"echo keep","timeout":10}}]}}],"PreToolUse":[{{"matcher":"Read","hooks":[{{"type":"command","command":"echo read","timeout":10}}]}}]}}}}"#,
            command,
        ),
    )
    .unwrap();
    fs::write(
        droid_dir.join("settings.json"),
        format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}","timeout":10}}]}}],"PostToolUse":[{{"matcher":"Edit","hooks":[{{"type":"command","command":"echo post","timeout":10}}]}}]}}}}"#,
            command,
        ),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_droid().unwrap();
    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(droid_dir.join("hooks.json")).unwrap()).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(droid_dir.join("settings.json")).unwrap())
            .unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(result.updated_settings);
    assert!(!result.hook_path.exists());
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert_eq!(hooks["hooks"]["PreToolUse"][0]["matcher"], "Read");
    assert!(settings["hooks"].get("SessionStart").is_none());
    assert_eq!(settings["hooks"]["PostToolUse"][0]["matcher"], "Edit");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_droid_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_droid().unwrap_err().to_string();

    assert!(err.contains("droid config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_opencode_writes_plugin_to_plugins_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let opencode_dir = home.join(".config/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_opencode().unwrap();

    assert_eq!(
        installed.plugin_path,
        opencode_dir
            .join("plugins")
            .join(OPENCODE_PLUGIN_INSTALL_NAME)
    );
    assert_eq!(
        fs::read_to_string(&installed.plugin_path).unwrap(),
        OPENCODE_PLUGIN_ASSET
    );
    assert_eq!(
        installed.tui_plugin_path,
        opencode_dir.join(OPENCODE_TUI_PLUGIN_INSTALL_NAME)
    );
    assert_eq!(
        fs::read_to_string(&installed.tui_plugin_path).unwrap(),
        OPENCODE_TUI_PLUGIN_ASSET
    );
    assert_eq!(installed.tui_config_path, opencode_dir.join("tui.jsonc"));
    let tui_config: Value =
        serde_json::from_str(&fs::read_to_string(&installed.tui_config_path).unwrap()).unwrap();
    assert_eq!(tui_config["plugin"], json!([OPENCODE_TUI_PLUGIN_SPEC]));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn opencode_status_requires_the_tui_plugin_and_config_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let opencode_dir = home.join(".config/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    std::env::set_var("HOME", &home);
    let installed = install_opencode().unwrap();
    let status = || {
        integration_status_at(
            crate::api::schema::IntegrationTarget::Opencode,
            installed.plugin_path.clone(),
            OPENCODE_INTEGRATION_VERSION,
        )
        .state
    };

    assert_eq!(status(), IntegrationStatusKind::Current);
    fs::remove_file(&installed.tui_plugin_path).unwrap();
    assert_eq!(status(), IntegrationStatusKind::Outdated);
    fs::write(&installed.tui_plugin_path, OPENCODE_TUI_PLUGIN_ASSET).unwrap();
    super::opencode_config::remove_tui_plugin(&opencode_dir, OPENCODE_TUI_PLUGIN_SPEC).unwrap();
    assert_eq!(status(), IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_opencode_removes_plugins_and_managed_tui_config_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let opencode_dir = home.join(".config/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    std::env::set_var("HOME", &home);
    let installed = install_opencode().unwrap();

    let result = uninstall_opencode().unwrap();

    assert!(result.removed_plugin);
    assert!(result.removed_tui_plugin);
    assert!(result.updated_tui_config);
    assert!(!result.plugin_path.exists());
    assert!(!result.tui_plugin_path.exists());
    assert!(result.tui_config_path.exists());
    let tui_config: Value =
        serde_json::from_str(&fs::read_to_string(&result.tui_config_path).unwrap()).unwrap();
    assert_eq!(tui_config, json!({}));
    assert_eq!(installed.plugin_path, result.plugin_path);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_opencode_invalid_tui_config_does_not_write_plugins() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let opencode_dir = home.join(".config/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("tui.jsonc"), r#"{"plugin":{}}"#).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_opencode().unwrap_err().to_string();

    assert!(err.contains("plugin list"));
    assert!(!opencode_dir
        .join("plugins")
        .join(OPENCODE_PLUGIN_INSTALL_NAME)
        .exists());
    assert!(!opencode_dir.join(OPENCODE_TUI_PLUGIN_INSTALL_NAME).exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_opencode_removes_plugins_when_tui_config_is_invalid() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let opencode_dir = home.join(".config/opencode");
    let plugins_dir = opencode_dir.join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    let plugin_path = plugins_dir.join(OPENCODE_PLUGIN_INSTALL_NAME);
    let tui_plugin_path = opencode_dir.join(OPENCODE_TUI_PLUGIN_INSTALL_NAME);
    fs::write(&plugin_path, OPENCODE_PLUGIN_ASSET).unwrap();
    fs::write(&tui_plugin_path, OPENCODE_TUI_PLUGIN_ASSET).unwrap();
    fs::write(opencode_dir.join("tui.jsonc"), "{\"plugin\":").unwrap();
    std::env::set_var("HOME", &home);

    let err = uninstall_opencode().unwrap_err().to_string();

    assert!(err.contains("failed to parse OpenCode TUI config"));
    assert!(!plugin_path.exists());
    assert!(!tui_plugin_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_opencode_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_opencode().unwrap_err().to_string();

    assert!(err.contains("opencode config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kilo_writes_plugin_to_plugin_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kilo_dir = home.join(".config/kilo");
    fs::create_dir_all(&kilo_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_kilo().unwrap();
    let plugin_content = fs::read_to_string(&installed.plugin_path).unwrap();

    assert_eq!(
        installed.plugin_path,
        kilo_dir.join("plugin").join(KILO_PLUGIN_INSTALL_NAME)
    );
    assert_eq!(plugin_content, KILO_PLUGIN_ASSET);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_kilo_removes_plugin_when_present() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kilo_plugin_dir = home.join(".config/kilo/plugin");
    fs::create_dir_all(&kilo_plugin_dir).unwrap();
    fs::write(
        kilo_plugin_dir.join(KILO_PLUGIN_INSTALL_NAME),
        KILO_PLUGIN_ASSET,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_kilo().unwrap();

    assert!(result.removed_plugin);
    assert!(!result.plugin_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kilo_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_kilo().unwrap_err().to_string();

    assert!(err.contains("kilo config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_writes_plugin_and_enables_it() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(hermes_dir.join("config.yaml"), "model:\n  provider: auto\n").unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_hermes().unwrap();
    let manifest = fs::read_to_string(
        installed
            .plugin_dir
            .join(HERMES_PLUGIN_MANIFEST_INSTALL_NAME),
    )
    .unwrap();
    let init =
        fs::read_to_string(installed.plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME)).unwrap();
    let config = fs::read_to_string(&installed.config_path).unwrap();

    assert_eq!(
        installed.plugin_dir,
        hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME)
    );
    assert_eq!(manifest, HERMES_PLUGIN_MANIFEST_ASSET);
    assert_eq!(init, HERMES_PLUGIN_INIT_ASSET);
    assert!(config.contains("plugins:\n  enabled:\n    - zynk-agent-state"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_is_idempotent_for_enabled_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  enabled:\n    - zynk-agent-state\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();
    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(config.matches("zynk-agent-state").count(), 1);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_preserves_flat_plugin_list() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - platforms/discord\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config,
        "plugins:\n  - zynk-agent-state\n  - platforms/discord\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_converts_flow_plugin_list_to_block_list() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins: [platforms/discord]\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config,
        "plugins:\n  - zynk-agent-state\n  - platforms/discord\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_is_idempotent_for_quoted_flat_plugin_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - \"zynk-agent-state\" # installed by zynk\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config,
        "plugins:\n  - \"zynk-agent-state\" # installed by zynk\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_removes_plugin_and_enabled_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  enabled:\n    - other-plugin\n    - zynk-agent-state\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert!(!plugin_dir.exists());
    assert!(config.contains("    - other-plugin"));
    assert!(!config.contains("zynk-agent-state"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_preserves_flat_plugin_list() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - other-plugin\n  - zynk-agent-state\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert_eq!(config, "plugins:\n  - other-plugin\n");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_removes_flow_plugin_list_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins: [other-plugin, zynk-agent-state]\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert_eq!(config, "plugins:\n  - other-plugin\n");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_removes_commented_flat_plugin_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - other-plugin\n  - zynk-agent-state # installed by zynk\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert_eq!(config, "plugins:\n  - other-plugin\n");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_hermes().unwrap_err().to_string();

    assert!(err.contains("hermes config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn pi_asset_is_state_only_no_receiver() {
    // zynk: the visible message HEADER superseded the receipt footer, so the pi
    // asset is STATE-ONLY — it reports agent session state and never registers an
    // input-side receiver. The wire-parsing receiver (input hook, body_hash verify,
    // message_received auto-record, transform-strip, receipt-footer markers) is GONE.
    assert!(!PI_EXTENSION_ASSET.contains("pi.on(\"input\""));
    assert!(!PI_EXTENSION_ASSET.contains("eligibleZynkReceipt(event)"));
    assert!(!PI_EXTENSION_ASSET.contains("verifyZynkBodyHash"));
    assert!(!PI_EXTENSION_ASSET.contains("zynk.message_received"));
    // no source gating — there is no input-side receiver to gate.
    assert!(!PI_EXTENSION_ASSET.contains("source === \"rpc\""));
    assert!(!PI_EXTENSION_ASSET.contains("source === \"interactive\""));
    // no transform-strip shape.
    assert!(!PI_EXTENSION_ASSET.contains("action: \"transform\""));
    // the old receipt-footer markers are gone.
    assert!(!PI_EXTENSION_ASSET.contains("--- zynk receipt footer v1 ---"));
    assert!(!PI_EXTENSION_ASSET.contains("--- end zynk receipt footer ---"));
    // no fire-and-forget receipt send.
    assert!(!PI_EXTENSION_ASSET.contains(".catch(() => {})"));

    // STILL state-only: it reports the agent session path/id, publishes state,
    // reports the agent session, and releases on a root-session shutdown.
    assert!(PI_EXTENSION_ASSET.contains("agent_session_path: currentAgentSessionPath"));
    assert!(PI_EXTENSION_ASSET.contains("agent_session_id: currentAgentSessionId"));
    assert!(PI_EXTENSION_ASSET.contains("publishState(true)"));
    assert!(PI_EXTENSION_ASSET.contains("pane.report_agent"));
    assert!(PI_EXTENSION_ASSET.contains("pane.report_agent_session"));
    assert!(PI_EXTENSION_ASSET.contains("pane.release_agent"));

    // the asset version marker is bumped to the tui-only session-gate revision.
    assert_eq!(parse_integration_version(PI_EXTENSION_ASSET), Some(9));
}

#[test]
fn pi_integration_version_marker_matches_const() {
    // Direct parity: the embedded asset's ZYNK_INTEGRATION_VERSION marker must
    // equal PI_INTEGRATION_VERSION. The indirect outdated_* tests only catch
    // const-ahead-of-asset; this catches drift in BOTH directions.
    assert_eq!(
        parse_integration_version(PI_EXTENSION_ASSET),
        Some(PI_INTEGRATION_VERSION)
    );
}

#[test]
fn bundled_integration_asset_versions_match_expected_versions() {
    // Asset-version parity for EVERY bundled integration, both directions: a bump
    // that moves the const without the asset marker (or the reverse) fails here.
    // The pi-only `pi_integration_version_marker_matches_const` stays as the
    // narrower guard. Every integration added later must gain a row here.
    for (name, asset, expected_version) in [
        ("pi", PI_EXTENSION_ASSET, PI_INTEGRATION_VERSION),
        ("omp", OMP_EXTENSION_ASSET, OMP_INTEGRATION_VERSION),
        ("claude", CLAUDE_HOOK_ASSET, CLAUDE_INTEGRATION_VERSION),
        ("codex", CODEX_HOOK_ASSET, CODEX_INTEGRATION_VERSION),
        ("kimi", KIMI_HOOK_ASSET, KIMI_INTEGRATION_VERSION),
        ("copilot", COPILOT_HOOK_ASSET, COPILOT_INTEGRATION_VERSION),
        ("devin", DEVIN_HOOK_ASSET, DEVIN_INTEGRATION_VERSION),
        ("droid", DROID_HOOK_ASSET, DROID_INTEGRATION_VERSION),
        (
            "opencode",
            OPENCODE_PLUGIN_ASSET,
            OPENCODE_INTEGRATION_VERSION,
        ),
        // opencode installs TWO artifacts and versions them together, so both
        // markers have to move with the one const.
        (
            "opencode_tui",
            OPENCODE_TUI_PLUGIN_ASSET,
            OPENCODE_INTEGRATION_VERSION,
        ),
        ("kilo", KILO_PLUGIN_ASSET, KILO_INTEGRATION_VERSION),
        (
            "hermes",
            HERMES_PLUGIN_INIT_ASSET,
            HERMES_INTEGRATION_VERSION,
        ),
        (
            "qodercli",
            QODERCLI_HOOK_ASSET,
            QODERCLI_INTEGRATION_VERSION,
        ),
        ("cursor", CURSOR_HOOK_ASSET, CURSOR_INTEGRATION_VERSION),
        (
            "mastracode",
            MASTRACODE_HOOK_ASSET,
            MASTRACODE_INTEGRATION_VERSION,
        ),
        (
            "antigravity_cli",
            ANTIGRAVITY_CLI_HOOK_ASSET,
            ANTIGRAVITY_CLI_INTEGRATION_VERSION,
        ),
        ("grok", GROK_HOOK_ASSET, GROK_INTEGRATION_VERSION),
    ] {
        assert_eq!(
            parse_integration_version(asset),
            Some(expected_version),
            "{name} asset version must match its integration version constant"
        );
    }
}

#[test]
fn bundled_integration_assets_report_session_refs() {
    assert!(PI_EXTENSION_ASSET.contains("agent_session_path: currentAgentSessionPath"));
    assert!(PI_EXTENSION_ASSET.contains("agent_session_id: currentAgentSessionId"));
    assert!(PI_EXTENSION_ASSET.contains("publishState(true)"));
    // Pi activates its root session on the TUI mode, not on `hasUI` — RPC sessions
    // also report `hasUI: true`. omp still gates on `hasUI` (see
    // `omp_root_session_guard_is_instance_scoped`), so this stays pi-scoped.
    assert!(PI_EXTENSION_ASSET.contains("ctx?.mode !== \"tui\""));
    assert!(!PI_EXTENSION_ASSET.contains("ctx?.hasUI !== true"));
    // Pi settles through its own `agent_settled` event; the hand-rolled idle
    // debounce behind `agent_end` is gone. omp still registers `agent_end`, so the
    // negative assertion has to stay pi-scoped.
    assert!(PI_EXTENSION_ASSET.contains("pi.on(\"agent_settled\""));
    assert!(!PI_EXTENSION_ASSET.contains("pi.on(\"agent_end\""));
    assert!(OMP_EXTENSION_ASSET.contains("pi.on(\"agent_end\""));
    assert!(OMP_EXTENSION_ASSET.contains("agent_session_path: currentAgentSessionPath"));
    assert!(OMP_EXTENSION_ASSET.contains("agent_session_id: currentAgentSessionId"));
    assert!(OMP_EXTENSION_ASSET.contains("publishState(true)"));
    assert!(CLAUDE_HOOK_ASSET.contains("agent_session_id"));
    assert!(CLAUDE_HOOK_ASSET.contains("agent_session_path"));
    assert!(CLAUDE_HOOK_ASSET.contains("session_start_source"));
    assert!(CLAUDE_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!CLAUDE_HOOK_ASSET.contains("\"state\": action"));
    assert!(!CLAUDE_HOOK_ASSET.contains("pane.release_agent"));
    assert!(CODEX_HOOK_ASSET.contains("ZYNK_HOOK_INPUT_FILE"));
    assert!(CODEX_HOOK_ASSET.contains("CODEX_THREAD_ID"));
    assert!(CODEX_HOOK_ASSET.contains("agent_session_id"));
    assert!(CODEX_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!CODEX_HOOK_ASSET.contains("\"state\": action"));
    assert!(!CODEX_HOOK_ASSET.contains("pane.release_agent"));
    assert!(KIMI_HOOK_ASSET.contains("source\": \"zynk:kimi"));
    assert!(KIMI_HOOK_ASSET.contains("agent_session_id"));
    assert!(KIMI_HOOK_ASSET.contains("method = \"pane.report_agent_session\""));
    assert!(KIMI_HOOK_ASSET.contains("method = \"pane.report_agent\""));
    assert!(KIMI_HOOK_ASSET.contains("params[\"state\"] = action"));
    assert!(!KIMI_HOOK_ASSET.contains("pane.release_agent"));
    assert!(COPILOT_HOOK_ASSET.contains("agent_session_id"));
    assert!(COPILOT_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!COPILOT_HOOK_ASSET.contains("\"state\":"));
    assert!(!COPILOT_HOOK_ASSET.contains("pane.release_agent"));
    assert!(DEVIN_HOOK_ASSET.contains("ZYNK_INTEGRATION_ID=devin"));
    assert!(DEVIN_HOOK_ASSET.contains("SOURCE = \"zynk:devin\""));
    assert!(DEVIN_HOOK_ASSET.contains("ZYNK_DEVIN_LIST_JSON"));
    assert!(DEVIN_HOOK_ASSET.contains("\"method\": \"pane.report_agent_session\""));
    assert!(!DEVIN_HOOK_ASSET.contains("\"method\": \"pane.report_agent\""));
    assert!(!DEVIN_HOOK_ASSET.contains("\"state\":"));
    assert!(!DEVIN_HOOK_ASSET.contains("pane.release_agent"));
    assert!(DEVIN_HOOK_ASSET.contains("agent_session_id"));
    assert!(DROID_HOOK_ASSET.contains("agent_session_id"));
    assert!(DROID_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!DROID_HOOK_ASSET.contains("\"state\": action"));
    assert!(!DROID_HOOK_ASSET.contains("pane.release_agent"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("properties?.sessionID"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("params.agent_session_id = sessionID"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("pane.report_agent_session"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("reportState"));
    assert!(!OPENCODE_PLUGIN_ASSET.contains("pane.release_agent"));
    assert!(KILO_PLUGIN_ASSET.contains("SOURCE = \"zynk:kilo\""));
    assert!(KILO_PLUGIN_ASSET.contains("AGENT = \"kilo\""));
    assert!(KILO_PLUGIN_ASSET.contains("pane.report_agent_session"));
    assert!(KILO_PLUGIN_ASSET.contains("reportState"));
    assert!(!KILO_PLUGIN_ASSET.contains("pane.release_agent"));
    assert!(QODERCLI_HOOK_ASSET.contains("ZYNK_PANE_ID"));
    assert!(QODERCLI_HOOK_ASSET.contains("session_id"));
    assert!(QODERCLI_HOOK_ASSET.contains("report-agent-session"));
    assert!(QODERCLI_HOOK_ASSET.contains("--agent-session-id"));
    assert!(!QODERCLI_HOOK_ASSET.contains("report-agent\""));
    assert!(!QODERCLI_HOOK_ASSET.contains("release-agent"));
    assert!(CURSOR_HOOK_ASSET.contains("ZYNK_INTEGRATION_ID=cursor"));
    assert!(CURSOR_HOOK_ASSET.contains("conversation_id"));
    assert!(CURSOR_HOOK_ASSET.contains("conversationId"));
    assert!(CURSOR_HOOK_ASSET.contains("sessionId"));
    assert!(CURSOR_HOOK_ASSET.contains("agent_session_id"));
    assert!(CURSOR_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(CURSOR_HOOK_ASSET.contains("hook_event_name"));
    assert!(CURSOR_HOOK_ASSET.contains("sessionStart"));
    assert!(!CURSOR_HOOK_ASSET.contains("\"state\":"));
    assert!(!CURSOR_HOOK_ASSET.contains("pane.release_agent"));
    assert!(MASTRACODE_HOOK_ASSET.contains("ZYNK_INTEGRATION_ID=mastracode"));
    assert!(MASTRACODE_HOOK_ASSET.contains("ZYNK_INTEGRATION_VERSION=1"));
    assert!(MASTRACODE_HOOK_ASSET.contains("session_id"));
    assert!(!MASTRACODE_HOOK_ASSET.contains("run_id"));
    assert!(MASTRACODE_HOOK_ASSET.contains("agent_session_id"));
    assert!(MASTRACODE_HOOK_ASSET.contains("pane.report_agent"));
    assert!(MASTRACODE_HOOK_ASSET.contains("pane.release_agent"));
    assert!(GROK_HOOK_ASSET.contains("ZYNK_INTEGRATION_ID=grok"));
    assert!(GROK_HOOK_ASSET.contains("GROK_SESSION_ID"));
    assert!(GROK_HOOK_ASSET.contains("sessionId"));
    assert!(GROK_HOOK_ASSET.contains("agent_session_id"));
    assert!(GROK_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(GROK_HOOK_ASSET.contains("zynk:grok"));
    assert!(!GROK_HOOK_ASSET.contains("\"state\":"));
    assert!(!GROK_HOOK_ASSET.contains("pane.release_agent"));
    assert!(ANTIGRAVITY_CLI_HOOK_ASSET.contains("ZYNK_INTEGRATION_ID=antigravity_cli"));
    assert!(ANTIGRAVITY_CLI_HOOK_ASSET.contains("conversationId"));
    assert!(ANTIGRAVITY_CLI_HOOK_ASSET.contains("agent_session_id"));
    assert!(ANTIGRAVITY_CLI_HOOK_ASSET.contains("agent_session_path"));
    assert!(ANTIGRAVITY_CLI_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(ANTIGRAVITY_CLI_HOOK_ASSET.contains("zynk:antigravity_cli"));
    // The hook reports the canonical `agy` label; the server normalizes
    // `antigravity-cli` to it before matching, so a raw product-name report would
    // never reach `is_official_agent_source` and resume would be unreachable.
    assert!(ANTIGRAVITY_CLI_HOOK_ASSET.contains("\"agent\": \"agy\""));
    assert!(!ANTIGRAVITY_CLI_HOOK_ASSET.contains("\"state\":"));
    assert!(!ANTIGRAVITY_CLI_HOOK_ASSET.contains("pane.release_agent"));
}

#[test]
fn pi_extension_releases_only_for_quit_session_shutdown() {
    let release_policy = PI_EXTENSION_ASSET
        .find("function shouldReleaseOnSessionShutdown")
        .expect("pi extension should centralize session shutdown release policy");
    let quit_check = PI_EXTENSION_ASSET
        .find("reason === \"quit\"")
        .expect("pi extension should release only for true quit shutdowns");
    let shutdown_handler = PI_EXTENSION_ASSET
        .find("pi.on(\"session_shutdown\", async (event)")
        .expect("pi extension should inspect the session_shutdown event");
    let guarded_release = PI_EXTENSION_ASSET[shutdown_handler..]
        .find("if (shouldReleaseOnSessionShutdown(event))")
        .expect("pi extension should guard releaseAgent by shutdown reason");

    assert!(release_policy < shutdown_handler);
    assert!(release_policy < quit_check);
    assert!(quit_check < shutdown_handler);
    assert!(guarded_release > 0);
}

#[test]
fn pi_extension_refreshes_session_ref_before_agent_start_state() {
    let agent_start = PI_EXTENSION_ASSET
        .find("pi.on(\"agent_start\", (_event, ctx)")
        .expect("pi extension should receive agent_start context");
    let handler = &PI_EXTENSION_ASSET[agent_start..];
    let update_session = handler
        .find("updateSessionRef(ctx);")
        .expect("pi extension should refresh the active session on agent_start");
    let report_session = handler
        .find("void reportSession();")
        .expect("pi extension should report the refreshed session before state");
    let publish_state = handler
        .find("publishState();")
        .expect("pi extension should publish working state after refreshing session");

    assert!(update_session < report_session);
    assert!(report_session < publish_state);
}

#[test]
fn pi_extension_retries_an_unanswered_state_report() {
    let attempt = PI_EXTENSION_ASSET
        .find("function sendRequestAttempt(request: unknown, timeoutMs: number): Promise<boolean>")
        .expect("pi extension should report a per-attempt delivery result");
    let sender = PI_EXTENSION_ASSET
        .find("async function sendRequest(request: unknown): Promise<void>")
        .expect("pi extension should wrap the attempt in a retrying sender");
    let first_attempt = PI_EXTENSION_ASSET[sender..]
        .find("if (await sendRequestAttempt(request, 500))")
        .expect("pi extension should return once the first attempt is delivered");
    let retry_attempt = PI_EXTENSION_ASSET[sender..]
        .find("await sendRequestAttempt(request, 1500);")
        .expect("pi extension should retry an undelivered report with a longer timeout");

    assert!(attempt < sender);
    assert!(first_attempt < retry_attempt);
}

#[test]
fn omp_extension_releases_only_for_quit_session_shutdown() {
    let release_policy = OMP_EXTENSION_ASSET
        .find("function shouldReleaseOnSessionShutdown")
        .expect("omp extension should centralize session shutdown release policy");
    let quit_check = OMP_EXTENSION_ASSET
        .find("reason === \"quit\"")
        .expect("omp extension should release only for true quit shutdowns");
    let shutdown_handler = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_shutdown\", async (event)")
        .expect("omp extension should inspect the session_shutdown event");
    let guarded_release = OMP_EXTENSION_ASSET[shutdown_handler..]
        .find("if (shouldReleaseOnSessionShutdown(event))")
        .expect("omp extension should guard releaseAgent by shutdown reason");

    assert!(release_policy < shutdown_handler);
    assert!(release_policy < quit_check);
    assert!(quit_check < shutdown_handler);
    assert!(guarded_release > 0);
}

#[test]
fn omp_extension_refreshes_session_ref_before_agent_start_state() {
    let agent_start = OMP_EXTENSION_ASSET
        .find("pi.on(\"agent_start\", (_event, ctx)")
        .expect("omp extension should receive agent_start context");
    let handler = &OMP_EXTENSION_ASSET[agent_start..];
    let update_session = handler
        .find("updateSessionRef(ctx);")
        .expect("omp extension should refresh the active session on agent_start");
    let report_session = handler
        .find("void reportSession();")
        .expect("omp extension should report the refreshed session before state");
    let publish_state = handler
        .find("publishState();")
        .expect("omp extension should publish working state after refreshing session");

    assert!(update_session < report_session);
    assert!(report_session < publish_state);
}

#[test]
fn pi_and_omp_extensions_restore_working_state_on_reload() {
    // The two assets no longer share a session_start signature: pi awaits its
    // session report (see `pi_extension_reports_the_session_start_source`) and so
    // takes the event, omp still ignores it. Anchor on the registration itself.
    for (name, asset) in [("pi", PI_EXTENSION_ASSET), ("omp", OMP_EXTENSION_ASSET)] {
        let session_start = asset
            .find("pi.on(\"session_start\",")
            .unwrap_or_else(|| panic!("{name} extension registers session_start handler"));
        let handler = &asset[session_start..];
        let restore = handler
            .find("agentActive = ctx?.isIdle?.() === false;")
            .unwrap_or_else(|| {
                panic!("{name} extension should restore working state across a reload")
            });
        let publish_state = handler
            .find("publishState(true);")
            .unwrap_or_else(|| panic!("{name} extension publishes the restored state"));

        assert!(restore < publish_state);
    }
}

#[test]
fn pi_extension_reports_the_session_start_source() {
    // A pi session replacement (/new, /resume, /fork) has to reach the pane as a
    // session report CARRYING its reason: `session_start_source` is what
    // `TerminalState::session_start_source_allows_session_replacement` gates the
    // re-anchor on, so a report without it can never replace the stale session.
    let report_session = PI_EXTENSION_ASSET
        .find("function reportSession(sessionStartSource?: string): Promise<void>")
        .expect("pi extension should take the session start source");
    assert!(
        PI_EXTENSION_ASSET[report_session..].contains("session_start_source: sessionStartSource,"),
        "pi extension should forward the session start source on the wire"
    );

    let session_start = PI_EXTENSION_ASSET
        .find("pi.on(\"session_start\", async (event, ctx)")
        .expect("pi extension should receive the session_start event");
    let handler = &PI_EXTENSION_ASSET[session_start..];
    let reported = handler
        .find("await reportSession(event?.reason);")
        .expect("pi extension should report the session with its start reason");
    let publish_state = handler
        .find("publishState(true);")
        .expect("pi extension publishes state after the session is reported");

    // Ordering is the point: state published before the replacement session is
    // acknowledged would still be attributed to the session it replaced.
    assert!(reported < publish_state);
}

#[test]
fn omp_extension_retries_an_unanswered_state_report() {
    // The omp mirror of `pi_extension_retries_an_unanswered_state_report`: a
    // first attempt the socket accepts but never answers is retried once with a
    // longer timeout before the queue moves on to the next report.
    let attempt = OMP_EXTENSION_ASSET
        .find("function sendRequestAttempt(request: unknown, timeoutMs: number): Promise<boolean>")
        .expect("omp extension should report a per-attempt delivery result");
    let sender = OMP_EXTENSION_ASSET
        .find("async function sendRequestNow(request: unknown): Promise<void>")
        .expect("omp extension should wrap the attempt in a retrying sender");
    let first_attempt = OMP_EXTENSION_ASSET[sender..]
        .find("if (await sendRequestAttempt(request, 500))")
        .expect("omp extension should return once the first attempt is delivered");
    let retry_attempt = OMP_EXTENSION_ASSET[sender..]
        .find("await sendRequestAttempt(request, 1500);")
        .expect("omp extension should retry an undelivered report with a longer timeout");

    assert!(attempt < sender);
    assert!(first_attempt < retry_attempt);
}

fn omp_handler(event: &str) -> &'static str {
    let start = OMP_EXTENSION_ASSET
        .find(&format!("pi.on(\"{event}\""))
        .unwrap_or_else(|| panic!("omp extension registers {event} handler"));
    let rest = &OMP_EXTENSION_ASSET[start..];
    let end = rest[1..]
        .find("\n\n  pi.")
        .map(|offset| offset + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn omp_root_activation_requires_ui_context() {
    let activator = OMP_EXTENSION_ASSET
        .find("function activateRootSession(ctx: any, sessionStartSource = \"startup\"): boolean")
        .expect("omp extension should centralize root session activation");
    let helper = &OMP_EXTENSION_ASSET[activator..];
    let non_ui_guard = helper
        .find("ctx?.hasUI !== true")
        .expect("omp extension checks UI context before activating");
    let root_session = helper
        .find("rootSession = true;")
        .expect("omp extension activates root session after UI guard");
    let session_report = helper
        .find("void reportSession(sessionStartSource);")
        .expect("omp extension reports root session");

    assert!(non_ui_guard < root_session);
    assert!(root_session < session_report);
}

#[test]
fn omp_session_start_and_switch_use_root_activation() {
    let session_start = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_start\", (_event, ctx)")
        .expect("omp extension registers session_start handler");
    let session_start_handler = &OMP_EXTENSION_ASSET[session_start..];
    session_start_handler
        .find("if (!activateRootSession(ctx))")
        .expect("omp session_start handler should activate root session");

    let session_switch = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_switch\", (event, ctx)")
        .expect("omp extension registers session_switch handler");
    let session_switch_handler = &OMP_EXTENSION_ASSET[session_switch..];
    session_switch_handler
        .find("if (!activateRootSession(ctx, event?.reason || \"resume\"))")
        .expect("omp session_switch handler should activate root session with switch reason");
}

#[test]
fn omp_session_reports_include_start_source() {
    let report_session = OMP_EXTENSION_ASSET
        .find("function reportSession(sessionStartSource = \"startup\"): Promise<void>")
        .expect("omp extension should label session reports with a lifecycle source");
    let helper = &OMP_EXTENSION_ASSET[report_session..];
    let session_source = helper
        .find("session_start_source: sessionStartSource")
        .expect("omp session reports should include the lifecycle source");
    let session_ref = helper
        .find("...sessionRef")
        .expect("omp session reports should include the native session ref");

    assert!(session_source < session_ref);
}

#[test]
fn omp_socket_requests_are_serialized() {
    let queue = OMP_EXTENSION_ASSET
        .find("let requestQueue = Promise.resolve();")
        .expect("omp extension should keep socket reports ordered");
    let send_request = OMP_EXTENSION_ASSET[queue..]
        .find("function sendRequest(request: unknown): Promise<void>")
        .expect("omp extension should wrap socket sends in an ordered queue");
    let queued_send = OMP_EXTENSION_ASSET[queue + send_request..]
        .find("requestQueue = requestQueue.then(")
        .expect("omp extension should serialize socket requests through the queue");
    let raw_send = OMP_EXTENSION_ASSET[queue + send_request..]
        .find("sendRequestNow(request)")
        .expect("omp extension should enqueue the raw socket send");

    assert!(queued_send < raw_send);
}

#[test]
fn omp_runtime_events_can_activate_root_session_after_resume() {
    for event in [
        "agent_start",
        "tool_approval_requested",
        "tool_approval_resolved",
        "tool_execution_start",
        "tool_execution_end",
    ] {
        let handler = omp_handler(event);
        handler
            .find("!rootSession && !activateRootSession(ctx)")
            .unwrap_or_else(|| panic!("omp {event} handler should recover missing root session"));
    }
}

#[test]
fn omp_ask_and_approval_events_report_blocked_state() {
    let approval_handler = omp_handler("tool_approval_requested");
    approval_handler
        .find("activateBlocked(label);")
        .expect("approval requests should block the pane");

    let approval_resolved = omp_handler("tool_approval_resolved");
    approval_resolved
        .find("deactivateBlocked();")
        .expect("approval resolution should unblock the pane");

    let ask_handler = omp_handler("tool_execution_start");
    ask_handler
        .find("event?.toolName !== \"ask\"")
        .expect("tool execution handler should only treat Ask as blocked");
    ask_handler
        .find("activateBlocked(askBlockedMessage(event.args));")
        .expect("Ask start should block the pane");

    let ask_end_handler = omp_handler("tool_execution_end");
    ask_end_handler
        .find("event?.toolName !== \"ask\"")
        .expect("tool execution end should only treat Ask as blocked");
    ask_end_handler
        .find("deactivateBlocked();")
        .expect("Ask end should unblock the pane");
}

#[test]
fn omp_root_session_guard_is_instance_scoped() {
    let export_start = OMP_EXTENSION_ASSET
        .find("export default function (pi)")
        .expect("omp extension exports a function");
    let root_session_decl = OMP_EXTENSION_ASSET
        .find("let rootSession = false")
        .expect("omp extension declares root session guard");
    let session_start_handler = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_start\"")
        .expect("omp extension registers session_start handler");

    assert_eq!(
        OMP_EXTENSION_ASSET
            .matches("let rootSession = false")
            .count(),
        1
    );
    assert!(OMP_EXTENSION_ASSET.contains("ctx?.hasUI !== true"));
    assert!(OMP_EXTENSION_ASSET.contains("rootSession = true"));
    assert!(export_start < root_session_decl);
    assert!(root_session_decl < session_start_handler);
}

#[test]
fn install_qodercli_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let qoder_dir = base.join(".qoder");
    fs::create_dir_all(&qoder_dir).unwrap();
    fs::write(
        qoder_dir.join("settings.json"),
        r#"{"permissions":{"allow":["Read"]},"hooks":{}}"#,
    )
    .unwrap();
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &qoder_dir);

    let installed = install_qodercli().unwrap();

    assert_eq!(
        installed.hook_path,
        qoder_dir.join("hooks").join(QODERCLI_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.settings_path, qoder_dir.join("settings.json"));
    assert!(installed.hook_path.is_file());

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();
    let hooks = settings
        .get("hooks")
        .and_then(Value::as_object)
        .expect("hooks should be present");
    for (event, action) in QODERCLI_HOOK_EVENTS {
        assert!(
            hooks.contains_key(event),
            "expected hooks.{event} to be registered"
        );
        let command = hooks[event][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            command.contains(QODERCLI_HOOK_INSTALL_NAME) && command.ends_with(action),
            "expected qodercli {event} hook command to end with {action}, got {command}"
        );
    }
    // Pre-existing settings keys must be preserved.
    assert!(settings.get("permissions").is_some());

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_qodercli_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let qoder_dir = base.join(".qoder");
    fs::create_dir_all(&qoder_dir).unwrap();
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &qoder_dir);

    install_qodercli().unwrap();
    install_qodercli().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(qoder_dir.join("settings.json")).unwrap())
            .unwrap();
    let hooks = settings.get("hooks").and_then(Value::as_object).unwrap();
    for (event, _) in QODERCLI_HOOK_EVENTS {
        let entries = hooks.get(event).and_then(Value::as_array).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "expected hooks.{event} to contain exactly one entry, got {entries:?}"
        );
    }

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_qodercli_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let qoder_dir = base.join(".qoder");
    fs::create_dir_all(&qoder_dir).unwrap();
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &qoder_dir);

    install_qodercli().unwrap();
    // Inject a foreign hook entry the user might have configured by hand.
    let mut settings: Value =
        serde_json::from_str(&fs::read_to_string(qoder_dir.join("settings.json")).unwrap())
            .unwrap();
    settings["hooks"]["SessionStart"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "matcher": "*",
            "hooks": [{"type": "command", "command": "echo user-defined"}],
        }));
    fs::write(
        qoder_dir.join("settings.json"),
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    let result = uninstall_qodercli().unwrap();
    assert!(result.removed_hook_file);
    assert!(result.updated_settings);

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(qoder_dir.join("settings.json")).unwrap())
            .unwrap();
    let hooks = settings.get("hooks").and_then(Value::as_object).unwrap();
    let remaining = hooks.get("SessionStart").and_then(Value::as_array).unwrap();
    assert_eq!(remaining.len(), 1);
    let cmd = remaining[0]["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(cmd, "echo user-defined");

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_qodercli_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let missing = base.join(".qoder");
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &missing);

    let err = install_qodercli().unwrap_err().to_string();
    assert!(
        err.contains("qodercli config directory not found"),
        "unexpected error: {err}"
    );

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_writes_hook_and_updates_hooks_json() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    fs::write(
        cursor_dir.join("hooks.json"),
        r#"{"version":1,"hooks":{"stop":[{"command":"echo keep-me"}]}}"#,
    )
    .unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    let installed = install_cursor().unwrap();

    assert_eq!(
        installed.hook_path,
        cursor_dir.join(CURSOR_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, cursor_dir.join("hooks.json"));
    assert_eq!(
        fs::read_to_string(&installed.hook_path).unwrap(),
        CURSOR_HOOK_ASSET
    );

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.get("hooks").and_then(Value::as_object).unwrap();
    let session_start = hooks.get("sessionStart").and_then(Value::as_array).unwrap();
    assert_eq!(session_start.len(), 1);
    assert_eq!(
        session_start[0].get("command").and_then(Value::as_str),
        Some(hook_command(&installed.hook_path, Some("session")).as_str())
    );
    assert!(hooks.get("beforeSubmitPrompt").is_none());
    assert!(hooks.get("beforeShellExecution").is_none());
    let stop = hooks.get("stop").and_then(Value::as_array).unwrap();
    assert_eq!(stop.len(), 1);
    assert_eq!(
        stop[0].get("command").and_then(Value::as_str),
        Some("echo keep-me")
    );

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    install_cursor().unwrap();
    install_cursor().unwrap();

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.get("hooks").and_then(Value::as_object).unwrap();
    let session_start = hooks.get("sessionStart").and_then(Value::as_array).unwrap();
    assert_eq!(session_start.len(), 1);

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_cursor_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    install_cursor().unwrap();
    let mut hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    hooks_file["hooks"]["beforeSubmitPrompt"] = json!([{ "command": "echo user-defined" }]);
    fs::write(
        cursor_dir.join("hooks.json"),
        serde_json::to_string_pretty(&hooks_file).unwrap(),
    )
    .unwrap();

    let result = uninstall_cursor().unwrap();
    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(!cursor_dir.join(CURSOR_HOOK_INSTALL_NAME).is_file());

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.get("hooks").and_then(Value::as_object).unwrap();
    assert!(!hooks.contains_key("sessionStart"));
    assert!(hooks.contains_key("beforeSubmitPrompt"));

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_uses_cursor_config_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join("custom-cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    let installed = install_cursor().unwrap();

    assert_eq!(
        installed.hook_path,
        cursor_dir.join(CURSOR_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, cursor_dir.join("hooks.json"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn cursor_v1_integration_status_is_current() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    let hook_path = cursor_dir.join(CURSOR_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# ZYNK_INTEGRATION_ID=cursor\n# ZYNK_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    let statuses = installed_integration_statuses();
    let cursor = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Cursor)
        .expect("cursor integration status");
    assert_eq!(cursor.state, IntegrationStatusKind::Current);
    assert_eq!(cursor.installed_version, Some(CURSOR_INTEGRATION_VERSION));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let missing = base.join(".cursor");
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &missing);

    let err = install_cursor().unwrap_err().to_string();
    assert!(
        err.contains("cursor config directory not found"),
        "unexpected error: {err}"
    );

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_mastracode_writes_hook_and_updates_hooks_json() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    let mastracode_dir = base.join(".mastracode");
    fs::create_dir_all(&mastracode_dir).unwrap();
    fs::write(
        mastracode_dir.join("hooks.json"),
        r#"{"PostToolUse":[{"type":"command","command":"echo keep-me"}]}"#,
    )
    .unwrap();
    std::env::set_var("HOME", &base);

    let installed = install_mastracode().unwrap();

    assert_eq!(
        installed.hook_path,
        mastracode_dir
            .join("hooks")
            .join(MASTRACODE_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, mastracode_dir.join("hooks.json"));
    assert_eq!(
        fs::read_to_string(&installed.hook_path).unwrap(),
        MASTRACODE_HOOK_ASSET
    );

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(mastracode_dir.join("hooks.json")).unwrap())
            .unwrap();
    let hooks = hooks_file.as_object().unwrap();
    for (event, action) in MASTRACODE_HOOK_EVENTS {
        let entries = hooks.get(event).and_then(Value::as_array).unwrap();
        assert_eq!(entries.len(), 1, "{event} should have one zynk hook");
        let command = entries[0].get("command").and_then(Value::as_str).unwrap();
        assert_eq!(command, hook_command(&installed.hook_path, Some(action)));
        assert_eq!(
            entries[0].get("type").and_then(Value::as_str),
            Some("command")
        );
        assert_eq!(
            entries[0].get("timeout").and_then(Value::as_u64),
            Some(MASTRACODE_HOOK_TIMEOUT_MS)
        );
    }
    assert_eq!(
        hooks["PostToolUse"][0]
            .get("command")
            .and_then(Value::as_str),
        Some("echo keep-me")
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_mastracode_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &base);

    install_mastracode().unwrap();
    install_mastracode().unwrap();

    let hooks_file: Value = serde_json::from_str(
        &fs::read_to_string(base.join(".mastracode").join("hooks.json")).unwrap(),
    )
    .unwrap();
    let hooks = hooks_file.as_object().unwrap();
    for (event, _) in MASTRACODE_HOOK_EVENTS {
        assert_eq!(hooks.get(event).and_then(Value::as_array).unwrap().len(), 1);
    }

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_mastracode_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &base);

    install_mastracode().unwrap();
    let hooks_path = base.join(".mastracode").join("hooks.json");
    let mut hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    hooks_file["UserPromptSubmit"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "type": "command", "command": "echo user-defined" }));
    fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&hooks_file).unwrap(),
    )
    .unwrap();

    let result = uninstall_mastracode().unwrap();
    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(!base
        .join(".mastracode")
        .join("hooks")
        .join(MASTRACODE_HOOK_INSTALL_NAME)
        .is_file());

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    let hooks = hooks_file.as_object().unwrap();
    for (event, _) in MASTRACODE_HOOK_EVENTS {
        if event == "UserPromptSubmit" {
            continue;
        }
        assert!(!hooks.contains_key(event), "{event} should be removed");
    }
    let user_prompt_submit = hooks
        .get("UserPromptSubmit")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(user_prompt_submit.len(), 1);
    assert_eq!(
        user_prompt_submit[0].get("command").and_then(Value::as_str),
        Some("echo user-defined")
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_mastracode_errors_when_event_value_not_array() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    let mastracode_dir = base.join(".mastracode");
    fs::create_dir_all(&mastracode_dir).unwrap();
    fs::write(mastracode_dir.join("hooks.json"), r#"{"SessionStart":{}}"#).unwrap();
    std::env::set_var("HOME", &base);

    let err = install_mastracode().unwrap_err().to_string();
    assert!(
        err.contains("hook entries for SessionStart must be an array"),
        "unexpected error: {err}"
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_mastracode_errors_when_event_value_not_array() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    let mastracode_dir = base.join(".mastracode");
    fs::create_dir_all(&mastracode_dir).unwrap();
    fs::write(mastracode_dir.join("hooks.json"), r#"{"SessionStart":{}}"#).unwrap();
    std::env::set_var("HOME", &base);

    let err = uninstall_mastracode().unwrap_err().to_string();
    assert!(
        err.contains("hook entries for SessionStart must be an array"),
        "unexpected error: {err}"
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

fn grok_session_command(config: &Value) -> String {
    config["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .expect("grok SessionStart command")
        .to_string()
}

#[test]
fn install_grok_writes_hook_and_config() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let grok_dir = base.join(".grok");
    fs::create_dir_all(&grok_dir).unwrap();
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &grok_dir);

    let installed = install_grok().unwrap();

    let hooks_dir = grok_dir.join("hooks");
    assert_eq!(installed.hook_path, hooks_dir.join(GROK_HOOK_INSTALL_NAME));
    assert_eq!(
        installed.config_path,
        hooks_dir.join(GROK_HOOK_CONFIG_INSTALL_NAME)
    );
    assert_eq!(
        fs::read_to_string(&installed.hook_path).unwrap(),
        GROK_HOOK_ASSET
    );

    let config: Value =
        serde_json::from_str(&fs::read_to_string(&installed.config_path).unwrap()).unwrap();
    assert_eq!(config, grok_hook_config(&installed.hook_path));
    let session_start = config["hooks"]["SessionStart"].as_array().unwrap();
    assert_eq!(session_start.len(), 1);
    let command = grok_session_command(&config);
    assert!(command.starts_with("sh "));
    assert!(command.contains("zynk-agent-state.sh"));
    assert!(command.ends_with(" session"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_grok_is_idempotent() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let grok_dir = base.join(".grok");
    fs::create_dir_all(&grok_dir).unwrap();
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &grok_dir);

    install_grok().unwrap();
    let first =
        fs::read_to_string(grok_dir.join("hooks").join(GROK_HOOK_CONFIG_INSTALL_NAME)).unwrap();
    install_grok().unwrap();
    let second =
        fs::read_to_string(grok_dir.join("hooks").join(GROK_HOOK_CONFIG_INSTALL_NAME)).unwrap();
    assert_eq!(first, second);

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_grok_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    // Deliberately do not create the ~/.grok directory ahead of time: the
    // installer must refuse instead of conjuring a config dir for an agent
    // that is not installed.
    let missing = base.join(".grok");
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &missing);

    let err = install_grok().unwrap_err().to_string();
    assert!(
        err.contains("grok config directory not found"),
        "unexpected error: {err}"
    );
    assert!(!missing.exists(), "install must not create the config dir");

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_grok_removes_files() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let grok_dir = base.join(".grok");
    fs::create_dir_all(&grok_dir).unwrap();
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &grok_dir);

    install_grok().unwrap();
    let result = uninstall_grok().unwrap();
    assert!(result.removed_hook_file);
    assert!(result.removed_config_file);
    assert!(!result.hook_path.is_file());
    assert!(!result.config_path.is_file());

    // Uninstalling again is a no-op.
    let again = uninstall_grok().unwrap();
    assert!(!again.removed_hook_file);
    assert!(!again.removed_config_file);

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_grok_uses_grok_config_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let grok_dir = base.join("custom-grok");
    fs::create_dir_all(&grok_dir).unwrap();
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &grok_dir);

    let installed = install_grok().unwrap();

    let hooks_dir = grok_dir.join("hooks");
    assert_eq!(installed.hook_path, hooks_dir.join(GROK_HOOK_INSTALL_NAME));
    assert_eq!(
        installed.config_path,
        hooks_dir.join(GROK_HOOK_CONFIG_INSTALL_NAME)
    );

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn grok_dir_honors_grok_home_after_config_dir_seam() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home_dir = base.join("grok-home");
    fs::create_dir_all(&home_dir).unwrap();
    std::env::remove_var(GROK_CONFIG_DIR_ENV_VAR);
    std::env::set_var(GROK_HOME_ENV_VAR, &home_dir);

    // The grok CLI reads its config (and hooks/) from $GROK_HOME, so the
    // integration must install there too.
    let installed = install_grok().unwrap();
    assert_eq!(
        installed.hook_path,
        home_dir.join("hooks").join(GROK_HOOK_INSTALL_NAME)
    );

    // The zynk-level test seam still wins over GROK_HOME when set.
    let seam_dir = base.join("seam");
    fs::create_dir_all(&seam_dir).unwrap();
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &seam_dir);
    let installed = install_grok().unwrap();
    assert_eq!(
        installed.hook_path,
        seam_dir.join("hooks").join(GROK_HOOK_INSTALL_NAME)
    );

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn grok_v1_integration_status_is_current() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let grok_dir = base.join(".grok");
    fs::create_dir_all(&grok_dir).unwrap();
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &grok_dir);
    // A real install writes both the hook script and hooks/zynk.json.
    install_grok().unwrap();

    let statuses = installed_integration_statuses();
    let grok = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Grok)
        .expect("grok integration status");
    assert_eq!(grok.state, IntegrationStatusKind::Current);
    assert_eq!(grok.installed_version, Some(GROK_INTEGRATION_VERSION));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn grok_status_reports_outdated_when_hook_config_missing_or_broken() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let grok_dir = base.join(".grok");
    fs::create_dir_all(&grok_dir).unwrap();
    std::env::set_var(GROK_CONFIG_DIR_ENV_VAR, &grok_dir);
    install_grok().unwrap();
    let config_path = grok_dir.join("hooks").join(GROK_HOOK_CONFIG_INSTALL_NAME);

    let grok_state = || {
        installed_integration_statuses()
            .into_iter()
            .find(|status| status.target == crate::api::schema::IntegrationTarget::Grok)
            .expect("grok integration status")
            .state
    };

    // Missing config: grok never runs the hook, so the install is not current.
    fs::remove_file(&config_path).unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);

    // Corrupt config.
    fs::write(&config_path, "{not json").unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);

    // Config that no longer references the hook script.
    fs::write(
        &config_path,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo other"}]}]}}"#,
    )
    .unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);

    // Config that mentions the script name without invoking it, and one that
    // invokes it without the required `session` action: both are
    // nonfunctional, so neither may report current.
    fs::write(
        &config_path,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo zynk-agent-state.sh"}]}]}}"#,
    )
    .unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);
    let hook_path = grok_dir.join("hooks").join(GROK_HOOK_INSTALL_NAME);
    fs::write(
        &config_path,
        format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"sh '{}'"}}]}}]}}}}"#,
            hook_path.display()
        ),
    )
    .unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);

    // Correct command but not a command-type hook: grok will not execute it.
    let session_command = grok_session_command(&grok_hook_config(&hook_path));
    fs::write(
        &config_path,
        format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"http","command":{}}}]}}]}}}}"#,
            serde_json::to_string(&session_command).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);

    // A matcher can prevent the expected hook from running.
    let mut config = grok_hook_config(&hook_path);
    config["hooks"]["SessionStart"][0]["matcher"] = json!("(");
    fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);

    // A malformed sibling group makes grok reject the event's hook groups.
    let mut config = grok_hook_config(&hook_path);
    config["hooks"]["SessionStart"]
        .as_array_mut()
        .unwrap()
        .push(json!({}));
    fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Outdated);

    // Reinstall repairs both files.
    install_grok().unwrap();
    assert_eq!(grok_state(), IntegrationStatusKind::Current);

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_antigravity_cli_writes_hook_and_updates_hooks_json() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agy_dir = base.join(".gemini").join("config");
    fs::create_dir_all(&agy_dir).unwrap();
    fs::write(
        agy_dir.join("hooks.json"),
        r#"{"lint-checker":{"PreInvocation":[{"type":"command","command":"echo keep-me"}]}}"#,
    )
    .unwrap();
    std::env::set_var(ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR, &agy_dir);

    let installed = install_antigravity_cli().unwrap();

    assert_eq!(
        installed.hook_path,
        agy_dir
            .join("hooks")
            .join(ANTIGRAVITY_CLI_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, agy_dir.join("hooks.json"));
    assert_eq!(
        fs::read_to_string(&installed.hook_path).unwrap(),
        ANTIGRAVITY_CLI_HOOK_ASSET
    );

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(agy_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.as_object().unwrap();

    // zynk entries live under a named hook block; Antigravity CLI rejects a
    // file whose top level maps event names straight to arrays.
    let block = hooks
        .get(ANTIGRAVITY_CLI_HOOK_BLOCK_NAME)
        .and_then(Value::as_object)
        .unwrap();

    for (event, action) in ANTIGRAVITY_CLI_HOOK_EVENTS {
        let entries = block.get(event).and_then(Value::as_array).unwrap();
        assert_eq!(entries.len(), 1, "{event} should hold one zynk entry");
        let handler = &entries[0];

        // Handlers must be a flat list; the matcher/hooks wrapper is only
        // valid for tool events and invalidates the whole file here.
        assert!(
            handler.get("matcher").is_none() && handler.get("hooks").is_none(),
            "{event} must be a flat handler, got {handler}"
        );

        assert_eq!(handler.get("type").and_then(Value::as_str), Some("command"));
        assert_eq!(
            handler.get("timeout").and_then(Value::as_u64),
            Some(ANTIGRAVITY_CLI_HOOK_TIMEOUT_SEC)
        );
        let command = handler.get("command").and_then(Value::as_str).unwrap();
        assert!(command.contains("zynk-agent-state"));
        assert!(command.ends_with(action));
    }

    // The integration is session-only. Antigravity CLI cannot express blocked
    // state, skips PostInvocation on interruption, and fires Stop at end of
    // turn rather than process exit, so zynk never claims lifecycle authority
    // here and screen detection owns agent state.
    for event in ["PreToolUse", "PostToolUse", "PostInvocation", "Stop"] {
        assert!(
            block.get(event).is_none(),
            "{event} must not be registered; lifecycle stays with screen detection"
        );
    }

    // Other named hooks are left untouched.
    assert_eq!(
        hooks
            .get("lint-checker")
            .and_then(|block| block.get("PreInvocation"))
            .and_then(Value::as_array)
            .and_then(|entries| entries.first())
            .and_then(|entry| entry.get("command"))
            .and_then(Value::as_str),
        Some("echo keep-me")
    );

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_antigravity_cli_rewrites_stale_zynk_block() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agy_dir = base.join(".gemini").join("config");
    fs::create_dir_all(&agy_dir).unwrap();
    // An older zynk install claimed lifecycle authority, wrapped events in
    // matcher/hooks, and left entries Antigravity CLI now rejects.
    fs::write(
        agy_dir.join("hooks.json"),
        r#"{"zynk":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"stale"}]}],"PostInvocation":[{"type":"command","command":"stale idle"}],"Legacy":[]}}"#,
    )
    .unwrap();
    std::env::set_var(ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR, &agy_dir);

    install_antigravity_cli().unwrap();

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(agy_dir.join("hooks.json")).unwrap()).unwrap();
    let block = hooks_file
        .get(ANTIGRAVITY_CLI_HOOK_BLOCK_NAME)
        .and_then(Value::as_object)
        .unwrap();

    // The block is zynk-owned and rewritten wholesale, so a stale lifecycle
    // install is migrated to session-only rather than merged with.
    assert_eq!(
        block.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["PreInvocation"],
        "stale lifecycle events should be gone"
    );
    let entries = block
        .get("PreInvocation")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].get("hooks").is_none());
    assert!(entries[0]
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains("zynk-agent-state")));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_antigravity_cli_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agy_dir = base.join(".gemini").join("config");
    std::env::set_var(ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR, &agy_dir);

    let err = install_antigravity_cli().unwrap_err();
    assert!(err.to_string().contains("install antigravity cli first"));
    assert!(!agy_dir.exists(), "install must not create the config dir");

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_antigravity_cli_removes_zynk_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agy_dir = base.join(".gemini").join("config");
    fs::create_dir_all(&agy_dir).unwrap();
    fs::write(
        agy_dir.join("hooks.json"),
        r#"{"lint-checker":{"PreInvocation":[{"type":"command","command":"echo keep-me"}]}}"#,
    )
    .unwrap();
    std::env::set_var(ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR, &agy_dir);

    let installed = install_antigravity_cli().unwrap();
    assert!(installed.hook_path.is_file());

    let result = uninstall_antigravity_cli().unwrap();
    assert!(result.removed_hook_file);
    assert!(!installed.hook_path.is_file());
    assert!(result.updated_hooks);

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(agy_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.as_object().unwrap();

    // The zynk block is gone and unrelated named hooks survive.
    assert!(hooks.get(ANTIGRAVITY_CLI_HOOK_BLOCK_NAME).is_none());
    assert!(hooks.contains_key("lint-checker"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn antigravity_cli_v1_integration_status_is_current() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agy_dir = base.join(".gemini").join("config");
    fs::create_dir_all(&agy_dir).unwrap();
    std::env::set_var(ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR, &agy_dir);
    install_antigravity_cli().unwrap();

    let statuses = installed_integration_statuses();
    let agy = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::AntigravityCli)
        .expect("antigravity-cli integration status");
    assert_eq!(agy.state, IntegrationStatusKind::Current);
    assert_eq!(
        agy.installed_version,
        Some(ANTIGRAVITY_CLI_INTEGRATION_VERSION)
    );

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}
