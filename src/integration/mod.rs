use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::config_dir::{config_dir_from_env_or_home, expand_tilde_path, home_dir};
use portable_pty::CommandBuilder;
use serde_json::{json, Map, Value};

/// Zynk host-protocol pane-id env var (ADR 0010 — full rebrand): the env var name
/// exported to spawned panes/hooks so they know which pane they belong to.
pub(crate) const ZYNK_PANE_ID_ENV_VAR: &str = "ZYNK_PANE_ID";
/// Zynk host-protocol tab-id env var: identifies the public tab a pane belongs to.
pub(crate) const ZYNK_TAB_ID_ENV_VAR: &str = "ZYNK_TAB_ID";
/// Zynk host-protocol workspace-id env var: identifies the public workspace a pane belongs to.
pub(crate) const ZYNK_WORKSPACE_ID_ENV_VAR: &str = "ZYNK_WORKSPACE_ID";
const PI_EXTENSION_INSTALL_NAME: &str = "zynk-agent-state.ts";
const PI_EXTENSION_ASSET: &str = include_str!("assets/pi/zynk-agent-state.ts");
const PI_INTEGRATION_VERSION: u32 = 5;
const OMP_EXTENSION_INSTALL_NAME: &str = "zynk-omp-agent-state.ts";
// Pre-rebrand on-disk name of the omp extension; uninstall strips it too.
const OMP_EXTENSION_ASSET: &str = include_str!("assets/omp/zynk-agent-state.ts");
const OMP_INTEGRATION_VERSION: u32 = 3;
const PI_CODING_AGENT_DIR_ENV_VAR: &str = "PI_CODING_AGENT_DIR";
const CLAUDE_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "zynk-agent-state.ps1"
} else {
    "zynk-agent-state.sh"
};
const CLAUDE_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/claude/zynk-agent-state.ps1")
} else {
    include_str!("assets/claude/zynk-agent-state.sh")
};
const CLAUDE_INTEGRATION_VERSION: u32 = 7;
const CLAUDE_CONFIG_DIR_ENV_VAR: &str = "CLAUDE_CONFIG_DIR";
const CODEX_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "zynk-agent-state.ps1"
} else {
    "zynk-agent-state.sh"
};
const CODEX_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/codex/zynk-agent-state.ps1")
} else {
    include_str!("assets/codex/zynk-agent-state.sh")
};
const CODEX_INTEGRATION_VERSION: u32 = 6;
const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";
const KIMI_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "zynk-agent-state.ps1"
} else {
    "zynk-agent-state.sh"
};
const KIMI_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/kimi/zynk-agent-state.ps1")
} else {
    include_str!("assets/kimi/zynk-agent-state.sh")
};
const KIMI_INTEGRATION_VERSION: u32 = 4;
const KIMI_CODE_HOME_ENV_VAR: &str = "KIMI_CODE_HOME";
const KIMI_CONFIG_BLOCK_BEGIN: &str = "# >>> zynk kimi integration";
const KIMI_CONFIG_BLOCK_END: &str = "# <<< zynk kimi integration";
// Pre-rebrand kimi config-block fences; removal strips them too (migration compat).
const KIMI_MIN_VERSION: &str = "0.14.0";
const KIMI_HOOK_EVENTS: [(&str, &str); 9] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("SubagentStart", "working"),
    ("PreCompact", "working"),
    ("PermissionRequest", "blocked"),
    ("PermissionResult", "working"),
    ("Stop", "idle"),
    ("Interrupt", "idle"),
];
const COPILOT_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "zynk-agent-state.ps1"
} else {
    "zynk-agent-state.sh"
};
const COPILOT_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/copilot/zynk-agent-state.ps1")
} else {
    include_str!("assets/copilot/zynk-agent-state.sh")
};
const COPILOT_INTEGRATION_VERSION: u32 = 2;
const COPILOT_HOME_ENV_VAR: &str = "COPILOT_HOME";
const COPILOT_HOOK_EVENTS: [&str; 1] = ["SessionStart"];
const COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS: [&str; 9] = [
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "agentStop",
    "SessionEnd",
    "notification",
    "sessionStart",
];
const DEVIN_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
const DEVIN_HOOK_ASSET: &str = include_str!("assets/devin/zynk-agent-state.sh");
const DEVIN_INTEGRATION_VERSION: u32 = 1;
const DEVIN_HOOK_EVENTS: [(&str, &str); 6] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "session"),
    ("PreToolUse", "session"),
    ("PostToolUse", "session"),
    ("PermissionRequest", "session"),
    ("Stop", "session"),
];
const DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
const DROID_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "zynk-agent-state.ps1"
} else {
    "zynk-agent-state.sh"
};
const DROID_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/droid/zynk-agent-state.ps1")
} else {
    include_str!("assets/droid/zynk-agent-state.sh")
};
const DROID_INTEGRATION_VERSION: u32 = 2;
const DROID_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
const DROID_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 9] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("Notification", "blocked"),
    ("Stop", "idle"),
    ("SubagentStop", "working"),
    ("PreCompact", "working"),
    ("SessionEnd", "release"),
];
const OPENCODE_PLUGIN_INSTALL_NAME: &str = "zynk-agent-state.js";
const OPENCODE_PLUGIN_ASSET: &str = include_str!("assets/opencode/zynk-agent-state.js");
const OPENCODE_INTEGRATION_VERSION: u32 = 6;
const KILO_PLUGIN_INSTALL_NAME: &str = "zynk-agent-state.js";
const KILO_PLUGIN_ASSET: &str = include_str!("assets/kilo/zynk-agent-state.js");
const KILO_INTEGRATION_VERSION: u32 = 2;
const HERMES_PLUGIN_INSTALL_NAME: &str = "zynk-agent-state";
// Legacy hermes plugin name written by pre-rebrand installs; uninstall strips it
// from the user's config + removes its plugin dir (bounded migration cleanup).
const HERMES_PLUGIN_MANIFEST_INSTALL_NAME: &str = "plugin.yaml";
const HERMES_PLUGIN_INIT_INSTALL_NAME: &str = "__init__.py";
const HERMES_PLUGIN_MANIFEST_ASSET: &str = include_str!("assets/hermes/plugin.yaml");
const HERMES_PLUGIN_INIT_ASSET: &str = include_str!("assets/hermes/__init__.py");
const HERMES_INTEGRATION_VERSION: u32 = 3;
const QODERCLI_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "zynk-agent-state.ps1"
} else {
    "zynk-agent-state.sh"
};
const QODERCLI_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/qodercli/zynk-agent-state.ps1")
} else {
    include_str!("assets/qodercli/zynk-agent-state.sh")
};
const QODERCLI_INTEGRATION_VERSION: u32 = 2;
const QODERCLI_CONFIG_DIR_ENV_VAR: &str = "QODER_CONFIG_DIR";
const QODERCLI_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
const QODERCLI_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 12] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PostToolUseFailure", "working"),
    ("SubagentStart", "working"),
    ("SubagentStop", "working"),
    ("PreCompact", "working"),
    ("Notification", "blocked"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
const CURSOR_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
const CURSOR_HOOK_ASSET: &str = include_str!("assets/cursor/zynk-agent-state.sh");
const CURSOR_INTEGRATION_VERSION: u32 = 1;
const CURSOR_CONFIG_DIR_ENV_VAR: &str = "CURSOR_CONFIG_DIR";
const INTEGRATION_VERSION_MARKER: &str = "ZYNK_INTEGRATION_VERSION=";
// Pre-rebrand installs embedded `ZYNK_INTEGRATION_VERSION=`. status() still
// recognizes it (legacy installs surface as Outdated → prompt reinstall) and
// uninstall keys legacy hook cleanup off the matching ID marker below.
const INTEGRATION_ID_MARKER: &str = "ZYNK_INTEGRATION_ID=";

#[derive(Debug)]
pub(crate) struct ClaudeInstallPaths {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CodexInstallPaths {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct KimiInstallPaths {
    pub hook_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CopilotInstallPaths {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct DevinInstallPaths {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct DroidInstallPaths {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
    pub settings_path: PathBuf,
    pub updated_legacy_hooks: bool,
}

#[derive(Debug)]
pub(crate) struct OpenCodeInstallPaths {
    pub plugin_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct KiloInstallPaths {
    pub plugin_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct OmpInstallPaths {
    pub extension_path: PathBuf,
    pub removed_legacy_pi_extension: bool,
}

#[derive(Debug)]
pub(crate) struct HermesInstallPaths {
    pub plugin_dir: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct QodercliInstallPaths {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CursorInstallPaths {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CursorUninstallResult {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_hooks: bool,
}

#[derive(Debug)]
pub(crate) struct QodercliUninstallResult {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_settings: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationStatus {
    pub target: crate::api::schema::IntegrationTarget,
    pub path: PathBuf,
    pub state: IntegrationStatusKind,
    pub installed_version: Option<u32>,
    pub expected_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrationStatusKind {
    NotInstalled,
    Current,
    Outdated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationRecommendation {
    pub target: crate::api::schema::IntegrationTarget,
    pub label: &'static str,
    pub command: &'static str,
    pub available: bool,
    pub path: PathBuf,
    pub state: IntegrationStatusKind,
}

impl IntegrationRecommendation {
    pub fn needs_install(&self) -> bool {
        self.state == IntegrationStatusKind::Outdated
            || (self.available && self.state == IntegrationStatusKind::NotInstalled)
    }

    pub fn status_label(&self) -> &'static str {
        match (self.available, self.state) {
            (_, IntegrationStatusKind::Current) => "installed",
            (_, IntegrationStatusKind::Outdated) => "update available",
            (true, IntegrationStatusKind::NotInstalled) => "available",
            (false, IntegrationStatusKind::NotInstalled) => "not found",
        }
    }
}

#[derive(Debug)]
pub(crate) struct PiUninstallResult {
    pub extension_path: PathBuf,
    pub removed_extension: bool,
}

#[derive(Debug)]
pub(crate) struct OmpUninstallResult {
    pub extension_path: PathBuf,
    pub removed_extension: bool,
}

#[derive(Debug)]
pub(crate) struct ClaudeUninstallResult {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_settings: bool,
}

#[derive(Debug)]
pub(crate) struct CodexUninstallResult {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
    pub config_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_hooks: bool,
}

#[derive(Debug)]
pub(crate) struct KimiUninstallResult {
    pub hook_path: PathBuf,
    pub config_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_config: bool,
}

#[derive(Debug)]
pub(crate) struct CopilotUninstallResult {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_settings: bool,
}

#[derive(Debug)]
pub(crate) struct DevinUninstallResult {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_settings: bool,
}

#[derive(Debug)]
pub(crate) struct DroidUninstallResult {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_hooks: bool,
    pub updated_settings: bool,
}

#[derive(Debug)]
pub(crate) struct OpenCodeUninstallResult {
    pub plugin_path: PathBuf,
    pub removed_plugin: bool,
}

#[derive(Debug)]
pub(crate) struct KiloUninstallResult {
    pub plugin_path: PathBuf,
    pub removed_plugin: bool,
}

#[derive(Debug)]
pub(crate) struct HermesUninstallResult {
    pub plugin_dir: PathBuf,
    pub config_path: PathBuf,
    pub removed_plugin_dir: bool,
    pub updated_config: bool,
}

/// Export the Zynk-branded base env (`ZYNK_SOCKET_PATH`) that every spawned pane
/// receives regardless of whether it carries a pane/tab/workspace identity. The
/// pane/tab/workspace identity is layered on top by `apply_pane_launch_env`
/// (see `src/pane.rs`) when a `PaneLaunchEnv` carries one.
pub(crate) fn apply_pane_base_env(cmd: &mut CommandBuilder) {
    cmd.env(
        crate::api::ZYNK_SOCKET_PATH_ENV_VAR,
        crate::api::socket_path(),
    );
}

pub(crate) const INSTALL_WARNING_PREFIX: &str = "warning:";

struct AgentVersionRequirement {
    label: &'static str,
    binary: &'static str,
    args: &'static [&'static str],
    min_version: &'static str,
}

fn agent_version_requirement(
    target: crate::api::schema::IntegrationTarget,
) -> Option<AgentVersionRequirement> {
    match target {
        crate::api::schema::IntegrationTarget::Kimi => Some(AgentVersionRequirement {
            label: "kimi code",
            binary: "kimi",
            args: &["--version"],
            min_version: KIMI_MIN_VERSION,
        }),
        _ => None,
    }
}

fn extract_version_triple(text: &str) -> Option<(u64, u64, u64)> {
    text.split_whitespace().find_map(|token| {
        let token = token.trim_start_matches('v');
        let mut parts = token.splitn(3, '.');
        let major: u64 = parts.next()?.parse().ok()?;
        let minor: u64 = parts.next()?.parse().ok()?;
        let patch: u64 = parts
            .next()
            .map(|rest| {
                rest.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .and_then(|digits| digits.parse().ok())
            .unwrap_or(0);
        Some((major, minor, patch))
    })
}

/// Returns `Ok(None)` when the installed agent satisfies the requirement,
/// `Ok(Some(warning))` when the version cannot be determined (install
/// proceeds), and `Err` when the installed agent is too old.
fn enforce_agent_version(requirement: &AgentVersionRequirement) -> io::Result<Option<String>> {
    let probe = format!("{} {}", requirement.binary, requirement.args.join(" "));
    let output = match std::process::Command::new(requirement.binary)
        .args(requirement.args)
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => {
            return Ok(Some(format!(
                "{INSTALL_WARNING_PREFIX} could not run `{probe}` to verify the installed version; hooks require {} {} or newer",
                requirement.label, requirement.min_version
            )));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(found) = extract_version_triple(&stdout) else {
        return Ok(Some(format!(
            "{INSTALL_WARNING_PREFIX} could not parse the {} version from `{probe}` output; hooks require {} {} or newer",
            requirement.label, requirement.label, requirement.min_version
        )));
    };
    let required = extract_version_triple(requirement.min_version)
        .expect("static min version must be a valid version triple");

    if found < required {
        return Err(io::Error::other(format!(
            "{label} {}.{}.{} is too old: zynk hooks require {label} {min} or newer. upgrade {label}, then re-run install",
            found.0,
            found.1,
            found.2,
            label = requirement.label,
            min = requirement.min_version
        )));
    }
    Ok(None)
}

pub(crate) fn install_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let result = install_target_inner(target);
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::logging::integration_action("install", integration_target_label(target), outcome);
    result
}

fn install_target_inner(target: crate::api::schema::IntegrationTarget) -> io::Result<Vec<String>> {
    if !integration_target_supported(target) {
        return Err(io::Error::other(format!(
            "{} integration is not supported on Windows",
            integration_target_label(target)
        )));
    }

    let version_warning = match agent_version_requirement(target) {
        Some(requirement) => enforce_agent_version(&requirement)?,
        None => None,
    };

    let mut messages = match target {
        crate::api::schema::IntegrationTarget::Pi => {
            let path = install_pi()?;
            vec![format!("installed pi integration to {}", path.display())]
        }
        crate::api::schema::IntegrationTarget::Omp => {
            let installed = install_omp()?;
            let mut messages = Vec::new();
            if installed.removed_legacy_pi_extension {
                messages.push(format!(
                    "removed legacy pi integration from omp extension directory at {}",
                    installed
                        .extension_path
                        .with_file_name(PI_EXTENSION_INSTALL_NAME)
                        .display()
                ));
            }
            messages.push(format!(
                "installed omp integration to {}",
                installed.extension_path.display()
            ));
            messages
        }
        crate::api::schema::IntegrationTarget::Claude => {
            let installed = install_claude()?;
            vec![
                format!(
                    "installed claude integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured claude settings at {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Codex => {
            let installed = install_codex()?;
            vec![
                format!(
                    "installed codex integration hook to {}",
                    installed.hook_path.display()
                ),
                format!("ensured codex hooks at {}", installed.hooks_path.display()),
                format!(
                    "ensured codex config at {}",
                    installed.config_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Copilot => {
            let installed = install_copilot()?;
            vec![
                format!(
                    "installed copilot integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured copilot settings at {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Devin => {
            let installed = install_devin()?;
            vec![
                format!(
                    "installed devin integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured devin settings at {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Kimi => {
            let installed = install_kimi()?;
            vec![
                format!(
                    "installed kimi integration hook to {}",
                    installed.hook_path.display()
                ),
                format!("ensured kimi config at {}", installed.config_path.display()),
                format!("requires kimi code {KIMI_MIN_VERSION} or newer"),
            ]
        }
        crate::api::schema::IntegrationTarget::Droid => {
            let installed = install_droid()?;
            let mut messages = vec![
                format!(
                    "installed droid integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured droid hooks at {}",
                    installed.settings_path.display()
                ),
            ];
            if installed.updated_legacy_hooks {
                messages.push(format!(
                    "removed legacy zynk droid hook entries from {}",
                    installed.hooks_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let installed = install_opencode()?;
            vec![format!(
                "installed opencode integration plugin to {}",
                installed.plugin_path.display()
            )]
        }
        crate::api::schema::IntegrationTarget::Kilo => {
            let installed = install_kilo()?;
            vec![format!(
                "installed kilo integration plugin to {}",
                installed.plugin_path.display()
            )]
        }
        crate::api::schema::IntegrationTarget::Hermes => {
            let installed = install_hermes()?;
            vec![
                format!(
                    "installed hermes integration plugin to {}",
                    installed.plugin_dir.display()
                ),
                format!(
                    "enabled hermes plugin in {}",
                    installed.config_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Qodercli => {
            let installed = install_qodercli()?;
            vec![
                format!(
                    "installed qodercli integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured qodercli settings at {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Cursor => {
            let installed = install_cursor()?;
            vec![
                format!(
                    "installed cursor integration hook to {}",
                    installed.hook_path.display()
                ),
                format!("updated cursor hooks at {}", installed.hooks_path.display()),
            ]
        }
    };

    if let Some(warning) = version_warning {
        messages.push(warning);
    }

    Ok(messages)
}

pub(crate) fn uninstall_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let messages = match target {
        crate::api::schema::IntegrationTarget::Pi => {
            let result = uninstall_pi()?;
            if result.removed_extension {
                vec![format!(
                    "removed pi integration extension at {}",
                    result.extension_path.display()
                )]
            } else {
                vec![format!(
                    "no pi integration extension found at {}",
                    result.extension_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Omp => {
            let result = uninstall_omp()?;
            if result.removed_extension {
                vec![format!(
                    "removed omp integration extension at {}",
                    result.extension_path.display()
                )]
            } else {
                vec![format!(
                    "no omp integration extension found at {}",
                    result.extension_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Claude => {
            let result = uninstall_claude()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed claude hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no claude hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed zynk claude hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk claude hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Codex => {
            let result = uninstall_codex()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed codex hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no codex hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "removed zynk codex hook entries from {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk codex hook entries found in {}",
                    result.hooks_path.display()
                ));
            }
            messages.push(format!(
                "left codex config unchanged at {}",
                result.config_path.display()
            ));
            messages
        }
        crate::api::schema::IntegrationTarget::Copilot => {
            let result = uninstall_copilot()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed copilot hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no copilot hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed zynk copilot hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk copilot hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Devin => {
            let result = uninstall_devin()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed devin hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no devin hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed zynk devin hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk devin hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Kimi => {
            let result = uninstall_kimi()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed kimi hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no kimi hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_config {
                messages.push(format!(
                    "removed zynk kimi hook entries from {}",
                    result.config_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk kimi hook entries found in {}",
                    result.config_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Droid => {
            let result = uninstall_droid()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed droid hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no droid hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "removed legacy zynk droid hook entries from {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "no legacy zynk droid hook entries found in {}",
                    result.hooks_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed zynk droid hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk droid hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let result = uninstall_opencode()?;
            if result.removed_plugin {
                vec![format!(
                    "removed opencode integration plugin at {}",
                    result.plugin_path.display()
                )]
            } else {
                vec![format!(
                    "no opencode integration plugin found at {}",
                    result.plugin_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Kilo => {
            let result = uninstall_kilo()?;
            if result.removed_plugin {
                vec![format!(
                    "removed kilo integration plugin at {}",
                    result.plugin_path.display()
                )]
            } else {
                vec![format!(
                    "no kilo integration plugin found at {}",
                    result.plugin_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Hermes => {
            let result = uninstall_hermes()?;
            let mut messages = Vec::new();
            if result.removed_plugin_dir {
                messages.push(format!(
                    "removed hermes integration plugin at {}",
                    result.plugin_dir.display()
                ));
            } else {
                messages.push(format!(
                    "no hermes integration plugin found at {}",
                    result.plugin_dir.display()
                ));
            }
            if result.updated_config {
                messages.push(format!(
                    "disabled hermes plugin in {}",
                    result.config_path.display()
                ));
            } else {
                messages.push(format!(
                    "no hermes plugin entry found in {}",
                    result.config_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Qodercli => {
            let result = uninstall_qodercli()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed qodercli hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no qodercli hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed zynk qodercli hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk qodercli hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Cursor => {
            let result = uninstall_cursor()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed cursor hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no cursor hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "removed zynk cursor hook entries from {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "no zynk cursor hook entries found in {}",
                    result.hooks_path.display()
                ));
            }
            messages
        }
    };

    crate::logging::integration_action("uninstall", integration_target_label(target), "ok");
    Ok(messages)
}

pub(crate) fn integration_target_label(
    target: crate::api::schema::IntegrationTarget,
) -> &'static str {
    match target {
        crate::api::schema::IntegrationTarget::Pi => "pi",
        crate::api::schema::IntegrationTarget::Omp => "omp",
        crate::api::schema::IntegrationTarget::Claude => "claude",
        crate::api::schema::IntegrationTarget::Codex => "codex",
        crate::api::schema::IntegrationTarget::Copilot => "copilot",
        crate::api::schema::IntegrationTarget::Devin => "devin",
        crate::api::schema::IntegrationTarget::Droid => "droid",
        crate::api::schema::IntegrationTarget::Kimi => "kimi",
        crate::api::schema::IntegrationTarget::Opencode => "opencode",
        crate::api::schema::IntegrationTarget::Kilo => "kilo",
        crate::api::schema::IntegrationTarget::Hermes => "hermes",
        crate::api::schema::IntegrationTarget::Qodercli => "qodercli",
        crate::api::schema::IntegrationTarget::Cursor => "cursor",
    }
}

fn integration_target_command(target: crate::api::schema::IntegrationTarget) -> &'static str {
    integration_target_command_names(target)[0]
}

fn integration_target_command_names(
    target: crate::api::schema::IntegrationTarget,
) -> &'static [&'static str] {
    match target {
        crate::api::schema::IntegrationTarget::Pi => &["pi"],
        crate::api::schema::IntegrationTarget::Omp => &["omp"],
        crate::api::schema::IntegrationTarget::Claude => &["claude"],
        crate::api::schema::IntegrationTarget::Codex => &["codex"],
        crate::api::schema::IntegrationTarget::Copilot => &["copilot"],
        crate::api::schema::IntegrationTarget::Devin => &["devin"],
        crate::api::schema::IntegrationTarget::Droid => &["droid"],
        crate::api::schema::IntegrationTarget::Kimi => &["kimi"],
        crate::api::schema::IntegrationTarget::Opencode => &["opencode"],
        crate::api::schema::IntegrationTarget::Kilo => &["kilo", "kilo-code"],
        crate::api::schema::IntegrationTarget::Hermes => &["hermes"],
        crate::api::schema::IntegrationTarget::Qodercli => qodercli_command_names(),
        crate::api::schema::IntegrationTarget::Cursor => cursor_command_names(),
    }
}

fn cursor_command_names() -> &'static [&'static str] {
    &["cursor-agent"]
}

fn integration_target_supported(target: crate::api::schema::IntegrationTarget) -> bool {
    #[cfg(windows)]
    {
        matches!(
            target,
            crate::api::schema::IntegrationTarget::Claude
                | crate::api::schema::IntegrationTarget::Codex
                | crate::api::schema::IntegrationTarget::Copilot
                | crate::api::schema::IntegrationTarget::Droid
                | crate::api::schema::IntegrationTarget::Kimi
                | crate::api::schema::IntegrationTarget::Qodercli
        )
    }

    #[cfg(not(windows))]
    {
        let _ = target;
        true
    }
}

fn integration_target_available(target: crate::api::schema::IntegrationTarget) -> bool {
    if !integration_target_supported(target) {
        return false;
    }

    integration_target_command_names(target)
        .iter()
        .any(|command| command_available(command))
        || integration_target_install_layout_available(target)
}

#[cfg(windows)]
fn qodercli_command_names() -> &'static [&'static str] {
    &["qodercli", "qoder", "qoderclicn", "qodercn"]
}

#[cfg(not(windows))]
fn qodercli_command_names() -> &'static [&'static str] {
    &["qodercli"]
}

fn integration_target_install_layout_available(
    target: crate::api::schema::IntegrationTarget,
) -> bool {
    match target {
        crate::api::schema::IntegrationTarget::Codex => codex_standalone_binary_available(),
        crate::api::schema::IntegrationTarget::Hermes => hermes_install_layout_available(),
        _ => false,
    }
}

fn command_available(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        command_path_candidates(&dir, command)
            .into_iter()
            .any(|path| executable_file_exists(&path))
    })
}

fn command_path_candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    let base = dir.join(command);

    #[cfg(not(windows))]
    {
        vec![base]
    }

    #[cfg(windows)]
    {
        if Path::new(command).extension().is_some() {
            return vec![base];
        }

        let mut candidates = vec![base];
        for extension in [".exe", ".cmd", ".bat", ".ps1"] {
            candidates.push(dir.join(format!("{command}{extension}")));
        }
        candidates
    }
}

fn executable_file_exists(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn codex_standalone_binary_available() -> bool {
    let Ok(releases_dir) =
        codex_dir().map(|dir| dir.join("packages").join("standalone").join("releases"))
    else {
        return false;
    };
    let Ok(entries) = fs::read_dir(releases_dir) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        executable_file_exists(&entry.path().join("bin").join(codex_executable_name()))
    })
}

fn codex_executable_name() -> &'static str {
    if cfg!(windows) {
        "codex.exe"
    } else {
        "codex"
    }
}

fn hermes_install_layout_available() -> bool {
    #[cfg(windows)]
    {
        let Some(local_app_data) =
            std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty())
        else {
            return false;
        };
        let dir = PathBuf::from(local_app_data).join("hermes");
        [
            dir.join("hermes.exe"),
            dir.join("bin").join("hermes.exe"),
            dir.join("Scripts").join("hermes.exe"),
        ]
        .into_iter()
        .any(|path| executable_file_exists(&path))
    }

    #[cfg(not(windows))]
    {
        false
    }
}

pub(crate) fn installed_integration_statuses() -> Vec<IntegrationStatus> {
    integration_specs()
        .into_iter()
        .filter_map(|(target, path, expected_version)| {
            if !integration_target_supported(target) {
                return None;
            }
            Some(integration_status_at(target, path.ok()?, expected_version))
        })
        .collect()
}

pub(crate) fn integration_recommendations() -> Vec<IntegrationRecommendation> {
    integration_specs()
        .into_iter()
        .filter_map(|(target, path, expected_version)| {
            if !integration_target_supported(target) {
                return None;
            }
            let path = path.ok()?;
            let status = integration_status_at(target, path.clone(), expected_version);
            Some(IntegrationRecommendation {
                target,
                label: integration_target_label(target),
                command: integration_target_command(target),
                available: integration_target_available(target)
                    || status.state != IntegrationStatusKind::NotInstalled,
                path,
                state: status.state,
            })
        })
        .collect()
}

fn outdated_installed_integrations() -> Vec<IntegrationStatus> {
    installed_integration_statuses()
        .into_iter()
        .filter(|status| status.state == IntegrationStatusKind::Outdated)
        .collect()
}

fn integration_specs() -> [(
    crate::api::schema::IntegrationTarget,
    io::Result<PathBuf>,
    u32,
); 13] {
    [
        (
            crate::api::schema::IntegrationTarget::Pi,
            pi_extension_dir().map(|dir| dir.join(PI_EXTENSION_INSTALL_NAME)),
            PI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Omp,
            omp_extension_dir().map(|dir| dir.join(OMP_EXTENSION_INSTALL_NAME)),
            OMP_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Claude,
            claude_dir().map(|dir| dir.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME)),
            CLAUDE_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Codex,
            codex_dir().map(|dir| dir.join(CODEX_HOOK_INSTALL_NAME)),
            CODEX_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Copilot,
            copilot_dir().map(|dir| dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME)),
            COPILOT_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Devin,
            devin_dir().map(|dir| dir.join(DEVIN_HOOK_INSTALL_NAME)),
            DEVIN_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Droid,
            droid_dir().map(|dir| dir.join("hooks").join(DROID_HOOK_INSTALL_NAME)),
            DROID_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Kimi,
            kimi_dir().map(|dir| dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME)),
            KIMI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Opencode,
            opencode_dir().map(|dir| dir.join("plugins").join(OPENCODE_PLUGIN_INSTALL_NAME)),
            OPENCODE_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Kilo,
            kilo_dir().map(|dir| dir.join("plugin").join(KILO_PLUGIN_INSTALL_NAME)),
            KILO_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Hermes,
            hermes_plugin_dir().map(|dir| dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME)),
            HERMES_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Qodercli,
            qodercli_dir().map(|dir| dir.join("hooks").join(QODERCLI_HOOK_INSTALL_NAME)),
            QODERCLI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Cursor,
            cursor_dir().map(|dir| dir.join(CURSOR_HOOK_INSTALL_NAME)),
            CURSOR_INTEGRATION_VERSION,
        ),
    ]
}

pub(crate) fn integration_update_instructions(
    targets: &[crate::api::schema::IntegrationTarget],
) -> String {
    let commands: Vec<String> = targets
        .iter()
        .map(|target| {
            format!(
                "`zynk integration install {}`",
                integration_target_label(*target)
            )
        })
        .collect();

    match commands.as_slice() {
        [] => String::new(),
        [command] => format!("run {command}"),
        [rest @ .., last] => format!("run {} and {last}", rest.join(", ")),
    }
}

pub(crate) fn print_outdated_update_notice() -> bool {
    let outdated = outdated_installed_integrations();
    if outdated.is_empty() {
        return false;
    }

    let targets = outdated
        .iter()
        .map(|integration| integration.target)
        .collect::<Vec<_>>();
    eprintln!(
        "installed zynk integrations need updating; {}.",
        integration_update_instructions(&targets).replace('`', "")
    );
    true
}

fn integration_status_at(
    target: crate::api::schema::IntegrationTarget,
    path: PathBuf,
    expected_version: u32,
) -> IntegrationStatus {
    let resolved = path.is_file().then(|| path.clone());

    let Some(resolved) = resolved else {
        return IntegrationStatus {
            target,
            path,
            state: IntegrationStatusKind::NotInstalled,
            installed_version: None,
            expected_version,
        };
    };

    let content = fs::read_to_string(&resolved).ok();
    let installed_version = content.as_deref().and_then(parse_integration_version);
    // Current requires three conjuncts, not just a version match: the hook must be at
    // or above the expected version AND prove it is a genuine zynk-native hook for this
    // target (correct `ZYNK_INTEGRATION_ID`, no Herdr residue). A present-but-non-native
    // hook (stale Herdr-era, foreign id, or missing id) is Outdated so the CLI prompts a
    // reinstall, which overwrites it with the native hook.
    let is_native = content
        .as_deref()
        .is_some_and(|content| hook_is_native(content, expected_integration_id(target)));
    let state = if is_native && installed_version.is_some_and(|version| version >= expected_version)
    {
        IntegrationStatusKind::Current
    } else {
        IntegrationStatusKind::Outdated
    };

    IntegrationStatus {
        target,
        path: resolved,
        state,
        installed_version,
        expected_version,
    }
}

fn parse_integration_version(content: &str) -> Option<u32> {
    content.lines().find_map(|line| {
        let marker_line = line
            .trim()
            .trim_start_matches('/')
            .trim_start_matches('#')
            .trim();
        // Recognize the native `ZYNK_INTEGRATION_VERSION=` marker first; fall back
        // to the legacy `ZYNK_INTEGRATION_VERSION=` so a pre-rebrand install is
        // surfaced as Outdated (which prompts a reinstall) rather than missing.
        marker_line
            .strip_prefix(INTEGRATION_VERSION_MARKER)?
            .trim()
            .parse()
            .ok()
    })
}

/// True when the hook content carries pre-rebrand Herdr residue tokens. A version
/// marker alone is not enough to trust a hook as native: a stale Herdr-era hook can
/// carry `ZYNK_INTEGRATION_VERSION=` yet still identify as `herdr:<agent>` and export
/// `HERDR_*` env. Such a hook must never be reported as a current native integration.
fn hook_has_herdr_residue(content: &str) -> bool {
    content.contains("HERDR_") || content.contains("herdr:")
}

/// True when the hook content is a genuine zynk-native hook for `expected_id`: it
/// declares the matching `ZYNK_INTEGRATION_ID=<expected_id>` marker and carries no
/// Herdr residue. Marker parsing mirrors `parse_integration_version` (comment-prefix
/// stripping) so it works uniformly across `.sh`/`.ps1`/`.ts`/`.js`/`.py` hooks.
fn hook_is_native(content: &str, expected_id: &str) -> bool {
    if hook_has_herdr_residue(content) {
        return false;
    }
    content.lines().any(|line| {
        let marker_line = line
            .trim()
            .trim_start_matches('/')
            .trim_start_matches('#')
            .trim();
        marker_line
            .strip_prefix(INTEGRATION_ID_MARKER)
            .is_some_and(|id| id.trim() == expected_id)
    })
}

/// The `ZYNK_INTEGRATION_ID=<id>` value embedded in this target's native hook asset.
/// Declared explicitly per target (not derived from the display label) so a label
/// rename can never silently weaken the native-identity gate in `integration_status_at`.
fn expected_integration_id(target: crate::api::schema::IntegrationTarget) -> &'static str {
    use crate::api::schema::IntegrationTarget;
    match target {
        IntegrationTarget::Pi => "pi",
        IntegrationTarget::Omp => "omp",
        IntegrationTarget::Claude => "claude",
        IntegrationTarget::Codex => "codex",
        IntegrationTarget::Copilot => "copilot",
        IntegrationTarget::Devin => "devin",
        IntegrationTarget::Droid => "droid",
        IntegrationTarget::Kimi => "kimi",
        IntegrationTarget::Opencode => "opencode",
        IntegrationTarget::Kilo => "kilo",
        IntegrationTarget::Hermes => "hermes",
        IntegrationTarget::Qodercli => "qodercli",
        IntegrationTarget::Cursor => "cursor",
    }
}

pub(crate) fn install_pi() -> io::Result<PathBuf> {
    let dir = pi_extension_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "pi extension directory not found at {}. install pi and create the extensions directory first",
            dir.display()
        )));
    }

    let path = dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&path, PI_EXTENSION_ASSET)?;
    Ok(path)
}

pub(crate) fn install_omp() -> io::Result<OmpInstallPaths> {
    let dir = omp_extension_dir()?;
    if !dir.is_dir() {
        if dir.parent().is_some_and(|parent| parent.is_dir()) {
            fs::create_dir_all(&dir)?;
        } else {
            return Err(io::Error::other(format!(
                "omp extension directory not found at {}. install omp and create the extensions directory first",
                dir.display()
            )));
        }
    }

    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "omp extension directory not found at {}. install omp and create the extensions directory first",
            dir.display()
        )));
    }

    let removed_legacy_pi_extension = remove_legacy_pi_extension_from_omp_dir(&dir)?;
    let extension_path = dir.join(OMP_EXTENSION_INSTALL_NAME);
    fs::write(&extension_path, OMP_EXTENSION_ASSET)?;
    Ok(OmpInstallPaths {
        extension_path,
        removed_legacy_pi_extension,
    })
}

fn remove_legacy_pi_extension_from_omp_dir(dir: &Path) -> io::Result<bool> {
    // A pi extension can be mistakenly dropped into the omp dir under either the
    // native `zynk-agent-state.ts` name or the pre-rebrand `zynk-agent-state.ts`
    // name. Strip both (gated on a pi integration-ID marker so we never delete an
    // unrelated omp extension).
    let mut removed = false;
    for name in [PI_EXTENSION_INSTALL_NAME, "zynk-agent-state.ts"] {
        let legacy_path = dir.join(name);
        if !legacy_path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&legacy_path)?;
        if content.contains("ZYNK_INTEGRATION_ID=pi") {
            fs::remove_file(&legacy_path)?;
            removed = true;
        }
    }

    Ok(removed)
}

pub(crate) fn install_claude() -> io::Result<ClaudeInstallPaths> {
    let dir = claude_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "claude directory not found at {}. install claude code first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CLAUDE_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let settings_path = dir.join("settings.json");
    let mut settings = if settings_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?).map_err(|err| {
            io::Error::other(format!(
                "failed to parse {}: {err}",
                settings_path.display()
            ))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut settings,
        &settings_path,
        "claude settings",
        "claude settings hooks",
    )?;
    remove_hook_commands(hooks, "PostToolUse", &hook_path, Some("working"))?;
    remove_hook_commands(hooks, "PostToolUseFailure", &hook_path, Some("working"))?;
    remove_hook_commands(hooks, "SubagentStop", &hook_path, Some("working"))?;
    remove_hook_commands(hooks, "PermissionRequest", &hook_path, Some("blocked"))?;
    remove_hook_commands(hooks, "SessionStart", &hook_path, Some("idle"))?;
    remove_hook_commands(hooks, "UserPromptSubmit", &hook_path, Some("working"))?;
    remove_hook_commands(hooks, "PreToolUse", &hook_path, Some("working"))?;
    remove_hook_commands(hooks, "Stop", &hook_path, Some("idle"))?;
    remove_hook_commands(hooks, "SessionEnd", &hook_path, Some("release"))?;
    remove_hook_commands(hooks, "SessionStart", &hook_path, Some("session"))?;
    ensure_command_hook(
        hooks,
        "SessionStart",
        hook_command(&hook_path, Some("session")),
        10,
        Some("*"),
    )?;
    remove_legacy_bash_hook_file(&hook_path)?;

    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    Ok(ClaudeInstallPaths {
        hook_path,
        settings_path,
    })
}

pub(crate) fn install_codex() -> io::Result<CodexInstallPaths> {
    let dir = codex_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "codex config directory not found at {}. install codex first",
            dir.display()
        )));
    }

    let hook_path = dir.join(CODEX_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CODEX_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let hooks_path = dir.join("hooks.json");
    let mut hooks_file = if hooks_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?).map_err(|err| {
            io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut hooks_file,
        &hooks_path,
        "codex hooks file",
        "codex hooks file hooks",
    )?;
    remove_hook_commands(hooks, "PermissionRequest", &hook_path, Some("blocked"))?;
    remove_hook_commands(hooks, "SessionStart", &hook_path, Some("idle"))?;
    remove_hook_commands(hooks, "UserPromptSubmit", &hook_path, Some("working"))?;
    remove_hook_commands(hooks, "PreToolUse", &hook_path, Some("working"))?;
    remove_hook_commands(hooks, "Stop", &hook_path, Some("idle"))?;
    remove_hook_commands(hooks, "SessionStart", &hook_path, Some("session"))?;
    ensure_command_hook(
        hooks,
        "SessionStart",
        hook_command(&hook_path, Some("session")),
        10,
        None,
    )?;
    remove_legacy_bash_hook_file(&hook_path)?;

    fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;

    let config_path = dir.join("config.toml");
    let existing_config = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    let new_config = build_codex_config_with_hooks(&existing_config);
    if new_config != existing_config {
        fs::write(&config_path, new_config)?;
    }

    Ok(CodexInstallPaths {
        hook_path,
        hooks_path,
        config_path,
    })
}

pub(crate) fn install_kimi() -> io::Result<KimiInstallPaths> {
    let dir = kimi_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "kimi code config directory not found at {}. install kimi code first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(KIMI_HOOK_INSTALL_NAME);
    fs::write(&hook_path, KIMI_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let config_path = dir.join("config.toml");
    let existing_config = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    let new_config = build_kimi_config_with_hooks(&existing_config, &hook_path);
    if new_config != existing_config {
        fs::write(&config_path, new_config)?;
    }
    remove_legacy_bash_hook_file(&hook_path)?;

    Ok(KimiInstallPaths {
        hook_path,
        config_path,
    })
}

pub(crate) fn install_copilot() -> io::Result<CopilotInstallPaths> {
    let dir = copilot_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "copilot config directory not found at {}. install github copilot cli first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(&hook_path, COPILOT_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let settings_path = dir.join("settings.json");
    let mut settings = if settings_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?).map_err(|err| {
            io::Error::other(format!(
                "failed to parse {}: {err}",
                settings_path.display()
            ))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut settings,
        &settings_path,
        "copilot settings",
        "copilot settings hooks",
    )?;
    let command = hook_command(&hook_path, None);
    for event in COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS {
        remove_direct_hook_commands(hooks, event, &hook_path, None)?;
    }
    for event in COPILOT_HOOK_EVENTS {
        remove_direct_hook_commands(hooks, event, &hook_path, None)?;
    }
    for event in COPILOT_HOOK_EVENTS {
        ensure_direct_command_hook(hooks, event, command.clone(), 10, None)?;
    }
    remove_legacy_bash_hook_file(&hook_path)?;

    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    Ok(CopilotInstallPaths {
        hook_path,
        settings_path,
    })
}

pub(crate) fn install_devin() -> io::Result<DevinInstallPaths> {
    let dir = devin_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "devin config directory not found at {}. install devin cli first",
            dir.display()
        )));
    }

    let hook_path = dir.join(DEVIN_HOOK_INSTALL_NAME);
    fs::write(&hook_path, DEVIN_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let settings_path = dir.join("config.json");
    let mut settings = if settings_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?).map_err(|err| {
            io::Error::other(format!(
                "failed to parse {}: {err}",
                settings_path.display()
            ))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut settings,
        &settings_path,
        "devin settings",
        "devin settings hooks",
    )?;
    for (event, action) in DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS {
        remove_hook_commands(hooks, event, &hook_path, Some(action))?;
    }
    for (event, action) in DEVIN_HOOK_EVENTS {
        remove_hook_commands(hooks, event, &hook_path, Some(action))?;
    }
    for (event, action) in DEVIN_HOOK_EVENTS {
        ensure_command_hook(
            hooks,
            event,
            hook_command(&hook_path, Some(action)),
            10,
            None,
        )?;
    }
    remove_legacy_bash_hook_file(&hook_path)?;

    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    Ok(DevinInstallPaths {
        hook_path,
        settings_path,
    })
}

pub(crate) fn install_droid() -> io::Result<DroidInstallPaths> {
    let dir = droid_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "droid config directory not found at {}. install droid first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(DROID_HOOK_INSTALL_NAME);
    fs::write(&hook_path, DROID_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let settings_path = dir.join("settings.json");
    let mut settings = if settings_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?).map_err(|err| {
            io::Error::other(format!(
                "failed to parse {}: {err}",
                settings_path.display()
            ))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut settings,
        &settings_path,
        "droid settings",
        "droid settings hooks",
    )?;
    remove_hook_commands(hooks, "SessionStart", &hook_path, None)?;
    for (event, action) in DROID_REMOVED_LIFECYCLE_HOOK_EVENTS {
        remove_hook_commands(hooks, event, &hook_path, Some(action))?;
    }
    for (event, action) in DROID_HOOK_EVENTS {
        remove_hook_commands(hooks, event, &hook_path, Some(action))?;
    }
    for (event, action) in DROID_HOOK_EVENTS {
        ensure_command_hook(
            hooks,
            event,
            hook_command(&hook_path, Some(action)),
            10,
            None,
        )?;
    }
    remove_legacy_bash_hook_file(&hook_path)?;

    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    let hooks_path = dir.join("hooks.json");
    let mut updated_legacy_hooks = false;
    if hooks_path.is_file() {
        let mut hooks_file = serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?)
            .map_err(|err| {
                io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
            })?;
        if let Some(hooks) = hooks_object_if_present(
            &mut hooks_file,
            &hooks_path,
            "droid hooks file",
            "droid hooks file hooks",
        )? {
            updated_legacy_hooks = remove_hook_commands(hooks, "SessionStart", &hook_path, None)?;
            for (event, action) in DROID_REMOVED_LIFECYCLE_HOOK_EVENTS {
                updated_legacy_hooks |=
                    remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
            for (event, action) in DROID_HOOK_EVENTS {
                updated_legacy_hooks |=
                    remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
        }
        if updated_legacy_hooks {
            fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;
        }
    }

    Ok(DroidInstallPaths {
        hook_path,
        hooks_path,
        settings_path,
        updated_legacy_hooks,
    })
}

pub(crate) fn install_opencode() -> io::Result<OpenCodeInstallPaths> {
    let dir = opencode_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "opencode config directory not found at {}. install opencode first",
            dir.display()
        )));
    }

    let plugins_dir = dir.join("plugins");
    fs::create_dir_all(&plugins_dir)?;

    let plugin_path = plugins_dir.join(OPENCODE_PLUGIN_INSTALL_NAME);
    fs::write(&plugin_path, OPENCODE_PLUGIN_ASSET)?;

    Ok(OpenCodeInstallPaths { plugin_path })
}

pub(crate) fn install_kilo() -> io::Result<KiloInstallPaths> {
    let dir = kilo_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "kilo config directory not found at {}. install kilo first",
            dir.display()
        )));
    }

    let plugins_dir = dir.join("plugin");
    fs::create_dir_all(&plugins_dir)?;

    let plugin_path = plugins_dir.join(KILO_PLUGIN_INSTALL_NAME);
    fs::write(&plugin_path, KILO_PLUGIN_ASSET)?;

    Ok(KiloInstallPaths { plugin_path })
}

pub(crate) fn install_hermes() -> io::Result<HermesInstallPaths> {
    let dir = hermes_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "hermes config directory not found at {}. install hermes agent first",
            dir.display()
        )));
    }

    let plugin_dir = hermes_plugin_dir()?;
    fs::create_dir_all(&plugin_dir)?;
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_MANIFEST_INSTALL_NAME),
        HERMES_PLUGIN_MANIFEST_ASSET,
    )?;
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )?;

    let config_path = dir.join("config.yaml");
    let existing_config = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    let new_config = ensure_hermes_plugin_enabled(&existing_config);
    if new_config != existing_config {
        fs::write(&config_path, new_config)?;
    }

    Ok(HermesInstallPaths {
        plugin_dir,
        config_path,
    })
}

pub(crate) fn uninstall_pi() -> io::Result<PiUninstallResult> {
    let extension_path = pi_extension_dir()?.join(PI_EXTENSION_INSTALL_NAME);
    let removed_extension = remove_file_if_exists(&extension_path)?;

    Ok(PiUninstallResult {
        extension_path,
        removed_extension,
    })
}

pub(crate) fn uninstall_omp() -> io::Result<OmpUninstallResult> {
    let dir = omp_extension_dir()?;
    let extension_path = dir.join(OMP_EXTENSION_INSTALL_NAME);
    let removed_extension = remove_file_if_exists(&extension_path)?;

    Ok(OmpUninstallResult {
        extension_path,
        removed_extension,
    })
}

pub(crate) fn uninstall_claude() -> io::Result<ClaudeUninstallResult> {
    let hook_path = claude_dir()?.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME);
    let settings_path = claude_dir()?.join("settings.json");
    let mut updated_settings = false;

    if settings_path.is_file() {
        let mut settings = serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?)
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to parse {}: {err}",
                    settings_path.display()
                ))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut settings,
            &settings_path,
            "claude settings",
            "claude settings hooks",
        )? {
            updated_settings |=
                remove_hook_commands(hooks, "SessionStart", &hook_path, Some("idle"))?;
            updated_settings |=
                remove_hook_commands(hooks, "SessionStart", &hook_path, Some("session"))?;
            updated_settings |=
                remove_hook_commands(hooks, "UserPromptSubmit", &hook_path, Some("working"))?;
            updated_settings |=
                remove_hook_commands(hooks, "PreToolUse", &hook_path, Some("working"))?;
            updated_settings |=
                remove_hook_commands(hooks, "PermissionRequest", &hook_path, Some("blocked"))?;
            updated_settings |=
                remove_hook_commands(hooks, "PostToolUse", &hook_path, Some("working"))?;
            updated_settings |=
                remove_hook_commands(hooks, "PostToolUseFailure", &hook_path, Some("working"))?;
            updated_settings |=
                remove_hook_commands(hooks, "SubagentStop", &hook_path, Some("working"))?;
            updated_settings |= remove_hook_commands(hooks, "Stop", &hook_path, Some("idle"))?;
            updated_settings |=
                remove_hook_commands(hooks, "SessionEnd", &hook_path, Some("release"))?;
        }

        if updated_settings {
            fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    let removed_hook_file =
        remove_file_if_exists(&hook_path)? | remove_legacy_bash_hook_file(&hook_path)?;

    Ok(ClaudeUninstallResult {
        hook_path,
        settings_path,
        removed_hook_file,
        updated_settings,
    })
}

pub(crate) fn uninstall_codex() -> io::Result<CodexUninstallResult> {
    let codex_dir = codex_dir()?;
    let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
    let hooks_path = codex_dir.join("hooks.json");
    let config_path = codex_dir.join("config.toml");
    let mut updated_hooks = false;

    if hooks_path.is_file() {
        let mut hooks_file = serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?)
            .map_err(|err| {
                io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut hooks_file,
            &hooks_path,
            "codex hooks file",
            "codex hooks file hooks",
        )? {
            updated_hooks |= remove_hook_commands(hooks, "SessionStart", &hook_path, Some("idle"))?;
            updated_hooks |=
                remove_hook_commands(hooks, "SessionStart", &hook_path, Some("session"))?;
            updated_hooks |=
                remove_hook_commands(hooks, "UserPromptSubmit", &hook_path, Some("working"))?;
            updated_hooks |=
                remove_hook_commands(hooks, "PreToolUse", &hook_path, Some("working"))?;
            updated_hooks |=
                remove_hook_commands(hooks, "PermissionRequest", &hook_path, Some("blocked"))?;
            updated_hooks |= remove_hook_commands(hooks, "Stop", &hook_path, Some("idle"))?;
        }

        if updated_hooks {
            fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;
        }
    }

    let removed_hook_file =
        remove_file_if_exists(&hook_path)? | remove_legacy_bash_hook_file(&hook_path)?;

    Ok(CodexUninstallResult {
        hook_path,
        hooks_path,
        config_path,
        removed_hook_file,
        updated_hooks,
    })
}

pub(crate) fn uninstall_kimi() -> io::Result<KimiUninstallResult> {
    let kimi_dir = kimi_dir()?;
    let hook_path = kimi_dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME);
    let config_path = kimi_dir.join("config.toml");
    let mut updated_config = false;

    if config_path.is_file() {
        let existing_config = fs::read_to_string(&config_path)?;
        let new_config = remove_kimi_config_block(&existing_config);
        if new_config != existing_config {
            fs::write(&config_path, new_config)?;
            updated_config = true;
        }
    }

    let removed_hook_file =
        remove_file_if_exists(&hook_path)? | remove_legacy_bash_hook_file(&hook_path)?;

    Ok(KimiUninstallResult {
        hook_path,
        config_path,
        removed_hook_file,
        updated_config,
    })
}

pub(crate) fn uninstall_copilot() -> io::Result<CopilotUninstallResult> {
    let copilot_dir = copilot_dir()?;
    let hook_path = copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME);
    let settings_path = copilot_dir.join("settings.json");
    let mut updated_settings = false;

    if settings_path.is_file() {
        let mut settings = serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?)
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to parse {}: {err}",
                    settings_path.display()
                ))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut settings,
            &settings_path,
            "copilot settings",
            "copilot settings hooks",
        )? {
            for event in COPILOT_HOOK_EVENTS {
                updated_settings |= remove_direct_hook_commands(hooks, event, &hook_path, None)?;
            }
            for event in COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS {
                updated_settings |= remove_direct_hook_commands(hooks, event, &hook_path, None)?;
            }
        }

        if updated_settings {
            fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    let removed_hook_file =
        remove_file_if_exists(&hook_path)? | remove_legacy_bash_hook_file(&hook_path)?;

    Ok(CopilotUninstallResult {
        hook_path,
        settings_path,
        removed_hook_file,
        updated_settings,
    })
}

pub(crate) fn uninstall_devin() -> io::Result<DevinUninstallResult> {
    let devin_dir = devin_dir()?;
    let hook_path = devin_dir.join(DEVIN_HOOK_INSTALL_NAME);
    let settings_path = devin_dir.join("config.json");
    let mut updated_settings = false;

    if settings_path.is_file() {
        let mut settings = serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?)
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to parse {}: {err}",
                    settings_path.display()
                ))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut settings,
            &settings_path,
            "devin settings",
            "devin settings hooks",
        )? {
            for (event, action) in DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS {
                updated_settings |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
            for (event, action) in DEVIN_HOOK_EVENTS {
                updated_settings |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
        }

        if updated_settings {
            fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    let removed_hook_file =
        remove_file_if_exists(&hook_path)? | remove_legacy_bash_hook_file(&hook_path)?;

    Ok(DevinUninstallResult {
        hook_path,
        settings_path,
        removed_hook_file,
        updated_settings,
    })
}

pub(crate) fn uninstall_droid() -> io::Result<DroidUninstallResult> {
    let droid_dir = droid_dir()?;
    let hook_path = droid_dir.join("hooks").join(DROID_HOOK_INSTALL_NAME);
    let hooks_path = droid_dir.join("hooks.json");
    let settings_path = droid_dir.join("settings.json");
    let mut updated_hooks = false;
    let mut updated_settings = false;
    if hooks_path.is_file() {
        let mut hooks_file = serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?)
            .map_err(|err| {
                io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut hooks_file,
            &hooks_path,
            "droid hooks file",
            "droid hooks file hooks",
        )? {
            updated_hooks |= remove_hook_commands(hooks, "SessionStart", &hook_path, None)?;
            for (event, action) in DROID_REMOVED_LIFECYCLE_HOOK_EVENTS {
                updated_hooks |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
            for (event, action) in DROID_HOOK_EVENTS {
                updated_hooks |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
        }

        if updated_hooks {
            fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;
        }
    }

    if settings_path.is_file() {
        let mut settings = serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?)
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to parse {}: {err}",
                    settings_path.display()
                ))
            })?;
        if let Some(hooks) = hooks_object_if_present(
            &mut settings,
            &settings_path,
            "droid settings",
            "droid settings hooks",
        )? {
            updated_settings = remove_hook_commands(hooks, "SessionStart", &hook_path, None)?;
            for (event, action) in DROID_REMOVED_LIFECYCLE_HOOK_EVENTS {
                updated_settings |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
            for (event, action) in DROID_HOOK_EVENTS {
                updated_settings |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
        }

        if updated_settings {
            fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    let removed_hook_file =
        remove_file_if_exists(&hook_path)? | remove_legacy_bash_hook_file(&hook_path)?;

    Ok(DroidUninstallResult {
        hook_path,
        hooks_path,
        settings_path,
        removed_hook_file,
        updated_hooks,
        updated_settings,
    })
}

pub(crate) fn uninstall_opencode() -> io::Result<OpenCodeUninstallResult> {
    let plugin_path = opencode_dir()?
        .join("plugins")
        .join(OPENCODE_PLUGIN_INSTALL_NAME);
    let removed_plugin = remove_file_if_exists(&plugin_path)?;

    Ok(OpenCodeUninstallResult {
        plugin_path,
        removed_plugin,
    })
}

pub(crate) fn uninstall_kilo() -> io::Result<KiloUninstallResult> {
    let plugin_path = kilo_dir()?.join("plugin").join(KILO_PLUGIN_INSTALL_NAME);
    let removed_plugin = remove_file_if_exists(&plugin_path)?;

    Ok(KiloUninstallResult {
        plugin_path,
        removed_plugin,
    })
}

pub(crate) fn uninstall_hermes() -> io::Result<HermesUninstallResult> {
    let dir = hermes_dir()?;
    let plugin_dir = hermes_plugin_dir()?;
    let config_path = dir.join("config.yaml");

    let removed_plugin_dir = remove_dir_all_if_exists(&plugin_dir)?;
    let mut updated_config = false;
    if config_path.is_file() {
        let existing_config = fs::read_to_string(&config_path)?;
        let new_config = remove_hermes_plugin_enabled(&existing_config);
        if new_config != existing_config {
            fs::write(&config_path, new_config)?;
            updated_config = true;
        }
    }

    Ok(HermesUninstallResult {
        plugin_dir,
        config_path,
        removed_plugin_dir,
        updated_config,
    })
}

pub(crate) fn install_qodercli() -> io::Result<QodercliInstallPaths> {
    let dir = qodercli_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "qodercli config directory not found at {}. install qodercli first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(QODERCLI_HOOK_INSTALL_NAME);
    fs::write(&hook_path, QODERCLI_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    // Register the hook in ~/.qoder/settings.json. The schema mirrors claude
    // settings.json (per https://docs.qoder.com/zh/cli/hooks): a top-level
    // `hooks` object keyed by event name, each entry holding a matcher + a
    // list of `{type: "command", command, timeout?}` invocations. The hook
    // script reads the event payload from stdin via `hook_event_name`.
    let settings_path = dir.join("settings.json");
    let mut settings = if settings_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?).map_err(|err| {
            io::Error::other(format!(
                "failed to parse {}: {err}",
                settings_path.display()
            ))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut settings,
        &settings_path,
        "qodercli settings",
        "qodercli settings hooks",
    )?;
    for (event, action) in QODERCLI_REMOVED_LIFECYCLE_HOOK_EVENTS {
        remove_hook_commands(hooks, event, &hook_path, Some(action))?;
    }
    for (event, action) in QODERCLI_HOOK_EVENTS {
        remove_hook_commands(hooks, event, &hook_path, Some(action))?;
    }
    for (event, action) in QODERCLI_HOOK_EVENTS {
        ensure_command_hook(
            hooks,
            event,
            hook_command(&hook_path, Some(action)),
            10,
            Some("*"),
        )?;
    }
    remove_legacy_bash_hook_file(&hook_path)?;

    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    Ok(QodercliInstallPaths {
        hook_path,
        settings_path,
    })
}

pub(crate) fn install_cursor() -> io::Result<CursorInstallPaths> {
    let dir = cursor_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "cursor config directory not found at {}. install cursor agent cli first",
            dir.display()
        )));
    }

    let hook_path = dir.join(CURSOR_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CURSOR_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let hooks_path = dir.join("hooks.json");
    let mut hooks_file = if hooks_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?).map_err(|err| {
            io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
        })?
    } else {
        json!({ "version": 1 })
    };

    if hooks_file.get("version").is_none() {
        hooks_file
            .as_object_mut()
            .ok_or_else(|| {
                io::Error::other(format!(
                    "cursor hooks file at {} must be a JSON object",
                    hooks_path.display()
                ))
            })?
            .insert("version".to_string(), json!(1));
    }

    let hooks = ensure_hooks_object(
        &mut hooks_file,
        &hooks_path,
        "cursor hooks file",
        "cursor hooks file hooks",
    )?;
    let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());
    let session_command = format!("bash {quoted_hook_path} session");
    remove_simple_command_hook(hooks, "beforeSubmitPrompt", &session_command)?;
    remove_simple_command_hook(hooks, "beforeShellExecution", &session_command)?;
    remove_simple_command_hook(hooks, "beforeMCPExecution", &session_command)?;
    remove_simple_command_hook(hooks, "stop", &session_command)?;
    remove_simple_command_hook(hooks, "sessionEnd", &session_command)?;
    ensure_simple_command_hook(hooks, "sessionStart", session_command)?;

    fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;

    Ok(CursorInstallPaths {
        hook_path,
        hooks_path,
    })
}

pub(crate) fn uninstall_qodercli() -> io::Result<QodercliUninstallResult> {
    let hook_path = qodercli_dir()?
        .join("hooks")
        .join(QODERCLI_HOOK_INSTALL_NAME);
    let settings_path = qodercli_dir()?.join("settings.json");
    let mut updated_settings = false;

    if settings_path.is_file() {
        let mut settings = serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?)
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to parse {}: {err}",
                    settings_path.display()
                ))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut settings,
            &settings_path,
            "qodercli settings",
            "qodercli settings hooks",
        )? {
            for (event, action) in QODERCLI_REMOVED_LIFECYCLE_HOOK_EVENTS {
                updated_settings |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
            for (event, action) in QODERCLI_HOOK_EVENTS {
                updated_settings |= remove_hook_commands(hooks, event, &hook_path, Some(action))?;
            }
        }

        if updated_settings {
            fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    let removed_hook_file =
        remove_file_if_exists(&hook_path)? | remove_legacy_bash_hook_file(&hook_path)?;

    Ok(QodercliUninstallResult {
        hook_path,
        settings_path,
        removed_hook_file,
        updated_settings,
    })
}

pub(crate) fn uninstall_cursor() -> io::Result<CursorUninstallResult> {
    let cursor_home = cursor_dir()?;
    let hook_path = cursor_home.join(CURSOR_HOOK_INSTALL_NAME);
    let hooks_path = cursor_home.join("hooks.json");
    let mut updated_hooks = false;

    if hooks_path.is_file() {
        let mut hooks_file = serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?)
            .map_err(|err| {
                io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut hooks_file,
            &hooks_path,
            "cursor hooks file",
            "cursor hooks file hooks",
        )? {
            let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());
            let session_command = format!("bash {quoted_hook_path} session");
            updated_hooks |= remove_simple_command_hook(hooks, "sessionStart", &session_command)?;
            updated_hooks |=
                remove_simple_command_hook(hooks, "beforeSubmitPrompt", &session_command)?;
            updated_hooks |=
                remove_simple_command_hook(hooks, "beforeShellExecution", &session_command)?;
            updated_hooks |=
                remove_simple_command_hook(hooks, "beforeMCPExecution", &session_command)?;
            updated_hooks |= remove_simple_command_hook(hooks, "stop", &session_command)?;
            updated_hooks |= remove_simple_command_hook(hooks, "sessionEnd", &session_command)?;
        }

        if updated_hooks {
            fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;
        }
    }

    let removed_hook_file = remove_file_if_exists(&hook_path)?;

    Ok(CursorUninstallResult {
        hook_path,
        hooks_path,
        removed_hook_file,
        updated_hooks,
    })
}

fn ensure_hooks_object<'a>(
    settings: &'a mut Value,
    settings_path: &Path,
    root_description: &str,
    hooks_description: &str,
) -> io::Result<&'a mut Map<String, Value>> {
    let root = settings.as_object_mut().ok_or_else(|| {
        io::Error::other(format!(
            "{root_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })?;

    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    hooks.as_object_mut().ok_or_else(|| {
        io::Error::other(format!(
            "{hooks_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })
}

fn hooks_object_if_present<'a>(
    settings: &'a mut Value,
    settings_path: &Path,
    root_description: &str,
    hooks_description: &str,
) -> io::Result<Option<&'a mut Map<String, Value>>> {
    let root = settings.as_object_mut().ok_or_else(|| {
        io::Error::other(format!(
            "{root_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })?;

    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(None);
    };

    hooks.as_object_mut().map(Some).ok_or_else(|| {
        io::Error::other(format!(
            "{hooks_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })
}

fn ensure_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: String,
    timeout: u64,
    matcher: Option<&str>,
) -> io::Result<()> {
    let entries = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    let already_installed = entries.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hook_entries| {
                hook_entries.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook.get("command").and_then(Value::as_str) == Some(command.as_str())
                })
            })
    });
    if already_installed {
        return Ok(());
    }

    let mut entry = Map::new();
    if let Some(matcher) = matcher {
        entry.insert("matcher".to_string(), Value::String(matcher.to_string()));
    }
    entry.insert(
        "hooks".to_string(),
        json!([
            {
                "type": "command",
                "command": command,
                "timeout": timeout,
            }
        ]),
    );

    entries.push(Value::Object(entry));
    Ok(())
}

// Claude and Codex use nested hook groups:
//   { "matcher": "...", "hooks": [{ "type": "command", ... }] }
// Copilot uses the flatter settings shape:
//   { "type": "command", "matcher": "...", "bash": "...", ... }
// Keep the helpers separate so install/uninstall preserves unrelated hooks in
// each agent's native format instead of normalizing user configuration.
fn ensure_direct_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: String,
    timeout_sec: u64,
    matcher: Option<&str>,
) -> io::Result<()> {
    let entries = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    let command_field = direct_command_field();
    if let Some(entry) = entries.iter_mut().find(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("command")
            && is_matching_direct_command_entry(entry, command.as_str())
    }) {
        let Some(entry_object) = entry.as_object_mut() else {
            return Ok(());
        };
        entry_object.remove("command");
        entry_object.remove("bash");
        entry_object.remove("powershell");
        entry_object.insert(command_field.to_string(), Value::String(command.clone()));
        entry_object.insert("timeoutSec".to_string(), Value::Number(timeout_sec.into()));
        match matcher {
            Some(matcher) => {
                entry_object.insert("matcher".to_string(), Value::String(matcher.to_string()));
            }
            None => {
                entry_object.remove("matcher");
            }
        }
        return Ok(());
    }

    let mut entry = Map::new();
    entry.insert("type".to_string(), Value::String("command".to_string()));
    if let Some(matcher) = matcher {
        entry.insert("matcher".to_string(), Value::String(matcher.to_string()));
    }
    entry.insert(command_field.to_string(), Value::String(command));
    entry.insert("timeoutSec".to_string(), Value::Number(timeout_sec.into()));
    entries.push(Value::Object(entry));
    Ok(())
}

fn direct_command_field() -> &'static str {
    if cfg!(windows) {
        "powershell"
    } else {
        "bash"
    }
}

fn is_matching_direct_command_entry(entry: &Value, command: &str) -> bool {
    entry.get("command").and_then(Value::as_str) == Some(command)
        || entry.get("bash").and_then(Value::as_str) == Some(command)
        || entry.get("powershell").and_then(Value::as_str) == Some(command)
}

fn remove_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: &str,
) -> io::Result<bool> {
    let Some(entries_value) = hooks.get_mut(event) else {
        return Ok(false);
    };

    let entries = entries_value
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    let mut removed = false;
    entries.retain_mut(|entry| {
        let Some(entry_object) = entry.as_object_mut() else {
            return true;
        };
        let Some(hook_entries) = entry_object.get_mut("hooks") else {
            return true;
        };
        let Some(hook_entries) = hook_entries.as_array_mut() else {
            return true;
        };

        let before = hook_entries.len();
        hook_entries.retain(|hook| !is_matching_command_hook(hook, command));
        if hook_entries.len() != before {
            removed = true;
        }

        !hook_entries.is_empty()
    });

    let remove_event = entries.is_empty();
    if remove_event {
        hooks.remove(event);
    }

    Ok(removed)
}

fn remove_direct_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: &str,
) -> io::Result<bool> {
    let Some(entries_value) = hooks.get_mut(event) else {
        return Ok(false);
    };

    let entries = entries_value
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    let before = entries.len();
    entries.retain(|entry| {
        !(entry.get("type").and_then(Value::as_str) == Some("command")
            && is_matching_direct_command_entry(entry, command))
    });
    let removed = entries.len() != before;
    if entries.is_empty() {
        hooks.remove(event);
    }
    Ok(removed)
}

// Cursor hooks.json uses the minimal shape `{ "command": "..." }` documented at
// https://cursor.com/docs/hooks. Keep this separate from the nested codex and
// flat copilot helpers so install/uninstall does not rewrite unrelated hooks.
fn ensure_simple_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: String,
) -> io::Result<()> {
    let entries = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    if entries
        .iter()
        .any(|entry| entry.get("command").and_then(Value::as_str) == Some(command.as_str()))
    {
        return Ok(());
    }

    entries.push(json!({ "command": command }));
    Ok(())
}

fn remove_simple_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: &str,
) -> io::Result<bool> {
    let Some(entries_value) = hooks.get_mut(event) else {
        return Ok(false);
    };

    let entries = entries_value
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    let before = entries.len();
    entries.retain(|entry| entry.get("command").and_then(Value::as_str) != Some(command));
    let removed = entries.len() != before;
    if entries.is_empty() {
        hooks.remove(event);
    }
    Ok(removed)
}

fn remove_hook_commands(
    hooks: &mut Map<String, Value>,
    event: &str,
    hook_path: &Path,
    action: Option<&str>,
) -> io::Result<bool> {
    let mut removed = false;
    for command in hook_command_variants(hook_path, action) {
        removed |= remove_command_hook(hooks, event, &command)?;
    }
    Ok(removed)
}

fn remove_direct_hook_commands(
    hooks: &mut Map<String, Value>,
    event: &str,
    hook_path: &Path,
    action: Option<&str>,
) -> io::Result<bool> {
    let mut removed = false;
    for command in hook_command_variants(hook_path, action) {
        removed |= remove_direct_command_hook(hooks, event, &command)?;
    }
    Ok(removed)
}

fn hook_command_variants(hook_path: &Path, action: Option<&str>) -> Vec<String> {
    let mut commands = vec![hook_command(hook_path, action)];
    push_unique_command(&mut commands, legacy_bash_hook_command(hook_path, action));

    #[cfg(windows)]
    {
        push_unique_command(
            &mut commands,
            legacy_bash_hook_command(&legacy_bash_hook_path(hook_path), action),
        );
    }

    commands
}

fn push_unique_command(commands: &mut Vec<String>, command: String) {
    if !commands.iter().any(|existing| existing == &command) {
        commands.push(command);
    }
}

fn is_matching_command_hook(hook: &Value, command: &str) -> bool {
    hook.get("type").and_then(Value::as_str) == Some("command")
        && hook.get("command").and_then(Value::as_str) == Some(command)
}

fn remove_file_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
fn legacy_bash_hook_path(hook_path: &Path) -> PathBuf {
    hook_path.with_file_name("zynk-agent-state.sh")
}

#[cfg(windows)]
fn remove_legacy_bash_hook_file(hook_path: &Path) -> io::Result<bool> {
    let legacy_path = legacy_bash_hook_path(hook_path);
    let content = match fs::read_to_string(&legacy_path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    if content.contains("ZYNK_INTEGRATION_ID=") {
        fs::remove_file(legacy_path)?;
        return Ok(true);
    }

    Ok(false)
}

#[cfg(not(windows))]
fn remove_legacy_bash_hook_file(_hook_path: &Path) -> io::Result<bool> {
    Ok(false)
}

fn remove_dir_all_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn ensure_hermes_plugin_enabled(content: &str) -> String {
    update_hermes_enabled_plugin(content, true, HERMES_PLUGIN_INSTALL_NAME)
}

fn remove_hermes_plugin_enabled(content: &str) -> String {
    update_hermes_enabled_plugin(content, false, HERMES_PLUGIN_INSTALL_NAME)
}

fn update_hermes_enabled_plugin(content: &str, enabled: bool, plugin_name: &str) -> String {
    let trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let Some(plugins_index) = top_level_yaml_key_index(&lines, "plugins") else {
        if !enabled {
            return content.to_string();
        }
        let mut result = content.trim_end_matches('\n').to_string();
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&format!("plugins:\n  enabled:\n    - {plugin_name}\n"));
        return result;
    };

    let plugins_end =
        next_top_level_yaml_key_index(&lines, plugins_index + 1).unwrap_or(lines.len());
    let plugins_inline_items = yaml_key_value_at_indent(&lines[plugins_index], 0, "plugins")
        .and_then(yaml_flow_sequence_items);
    let enabled_index = lines[plugins_index + 1..plugins_end]
        .iter()
        .position(|line| yaml_key_at_indent(line, 2) == Some("enabled"))
        .map(|offset| plugins_index + 1 + offset);
    let flat_list_start = lines[plugins_index + 1..plugins_end]
        .iter()
        .position(|line| yaml_list_item_value_at_indent(line, 2).is_some())
        .map(|offset| plugins_index + 1 + offset);

    if let Some(enabled_index) = enabled_index {
        let line = lines[enabled_index].trim();
        if line == "enabled: []" || line == "enabled: [] # zynk" {
            if enabled {
                lines[enabled_index] = "  enabled:".to_string();
                lines.insert(enabled_index + 1, format!("    - {plugin_name}"));
            }
            return join_yaml_lines(lines, trailing_newline);
        }

        let list_start = enabled_index + 1;
        let list_end = lines[list_start..plugins_end]
            .iter()
            .position(|line| {
                yaml_indent(line).is_some_and(|indent| indent <= 2) && yaml_key_name(line).is_some()
            })
            .map(|offset| list_start + offset)
            .unwrap_or(plugins_end);
        let existing_item_index = lines[list_start..list_end]
            .iter()
            .position(|line| yaml_list_item_matches(line, plugin_name))
            .map(|offset| list_start + offset);

        match (enabled, existing_item_index) {
            (true, Some(_)) | (false, None) => return content.to_string(),
            (true, None) => lines.insert(list_start, format!("    - {plugin_name}")),
            (false, Some(index)) => {
                lines.remove(index);
            }
        }
        return join_yaml_lines(lines, trailing_newline);
    }

    if let Some(mut items) = plugins_inline_items {
        let existing_item_index = items.iter().position(|item| item == plugin_name);

        match (enabled, existing_item_index) {
            (true, Some(_)) | (false, None) => return content.to_string(),
            (true, None) => items.insert(0, plugin_name.to_string()),
            (false, Some(index)) => {
                items.remove(index);
            }
        }

        let replacement = hermes_flat_plugin_lines(&items);
        lines.splice(plugins_index..plugins_end, replacement);
        return join_yaml_lines(lines, trailing_newline);
    }

    if let Some(flat_list_start) = flat_list_start {
        let existing_item_index = lines[plugins_index + 1..plugins_end]
            .iter()
            .position(|line| yaml_list_item_matches_at_indent(line, 2, plugin_name))
            .map(|offset| plugins_index + 1 + offset);

        match (enabled, existing_item_index) {
            (true, Some(_)) | (false, None) => return content.to_string(),
            (true, None) => lines.insert(flat_list_start, format!("  - {plugin_name}")),
            (false, Some(index)) => {
                lines.remove(index);
            }
        }
        return join_yaml_lines(lines, trailing_newline);
    }

    if enabled {
        lines.insert(plugins_index + 1, "  enabled:".to_string());
        lines.insert(plugins_index + 2, format!("    - {plugin_name}"));
        return join_yaml_lines(lines, trailing_newline);
    }

    content.to_string()
}

fn hermes_flat_plugin_lines(items: &[String]) -> Vec<String> {
    if items.is_empty() {
        return vec!["plugins: []".to_string()];
    }

    let mut lines = vec!["plugins:".to_string()];
    lines.extend(items.iter().map(|item| format!("  - {item}")));
    lines
}

fn top_level_yaml_key_index(lines: &[String], key: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| yaml_key_at_indent(line, 0) == Some(key))
}

fn next_top_level_yaml_key_index(lines: &[String], start: usize) -> Option<usize> {
    lines[start..]
        .iter()
        .position(|line| yaml_indent(line) == Some(0) && yaml_key_name(line).is_some())
        .map(|offset| start + offset)
}

fn yaml_key_at_indent(line: &str, indent: usize) -> Option<&str> {
    if yaml_indent(line)? != indent {
        return None;
    }
    yaml_key_name(line)
}

fn yaml_key_value_at_indent<'a>(line: &'a str, indent: usize, key: &str) -> Option<&'a str> {
    if yaml_indent(line)? != indent {
        return None;
    }
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
        return None;
    }
    let (line_key, value) = trimmed.split_once(':')?;
    (line_key.trim() == key).then_some(value.trim())
}

fn yaml_key_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
        return None;
    }
    let (key, _) = trimmed.split_once(':')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

fn yaml_indent(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    Some(line.len() - trimmed.len())
}

fn yaml_list_item_value(line: &str) -> Option<&str> {
    line.trim().strip_prefix("- ").map(str::trim)
}

fn yaml_list_item_matches(line: &str, value: &str) -> bool {
    yaml_list_item_value(line).is_some_and(|item| yaml_scalar_value(item) == value)
}

fn yaml_list_item_value_at_indent(line: &str, indent: usize) -> Option<&str> {
    if yaml_indent(line)? != indent {
        return None;
    }
    yaml_list_item_value(line)
}

fn yaml_list_item_matches_at_indent(line: &str, indent: usize, value: &str) -> bool {
    yaml_list_item_value_at_indent(line, indent)
        .is_some_and(|item| yaml_scalar_value(item) == value)
}

fn yaml_flow_sequence_items(value: &str) -> Option<Vec<String>> {
    let value = strip_yaml_inline_comment(value).trim();
    let inner = value.strip_prefix('[')?.strip_suffix(']')?.trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }

    let mut items = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in inner.chars() {
        if let Some(quote_char) = quote {
            current.push(ch);
            if quote_char == '"' && ch == '\\' && !escaped {
                escaped = true;
                continue;
            }
            if ch == quote_char && !escaped {
                quote = None;
            }
            escaped = false;
            continue;
        }

        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                current.push(ch);
            }
            ',' => {
                items.push(yaml_scalar_value(&current));
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if quote.is_some() {
        return None;
    }

    items.push(yaml_scalar_value(&current));
    Some(items)
}

fn yaml_scalar_value(value: &str) -> String {
    let value = strip_yaml_inline_comment(value).trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let quoted = (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'');
        if quoted {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn strip_yaml_inline_comment(value: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;

    for (index, ch) in value.char_indices() {
        if let Some(quote_char) = quote {
            if quote_char == '"' && ch == '\\' && !escaped {
                escaped = true;
                continue;
            }
            if ch == quote_char && !escaped {
                quote = None;
            }
            escaped = false;
            continue;
        }

        match ch {
            '"' | '\'' => quote = Some(ch),
            '#' if index == 0 || value[..index].ends_with(char::is_whitespace) => {
                return value[..index].trim_end();
            }
            _ => {}
        }
    }

    value
}

fn join_yaml_lines(lines: Vec<String>, trailing_newline: bool) -> String {
    let mut result = lines.join("\n");
    if trailing_newline || result.is_empty() {
        result.push('\n');
    }
    result
}

fn build_codex_config_with_hooks(content: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let trailing_newline = content.ends_with('\n');
    let mut in_top_level_features = false;
    let mut features_header_index = None;
    let mut hooks_index = None;
    let mut deprecated_hooks_indexes = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        if let Some(header) = toml_table_header(line) {
            in_top_level_features = header == "[features]";
            if in_top_level_features && features_header_index.is_none() {
                features_header_index = Some(index);
            }
            continue;
        }

        if !in_top_level_features {
            continue;
        }

        if is_toml_key(line, "codex_hooks") {
            deprecated_hooks_indexes.push(index);
        } else if is_toml_key(line, "hooks") {
            hooks_index = Some(index);
        }
    }

    if let Some(index) = hooks_index {
        lines[index] = "hooks = true".to_string();
    }

    for index in deprecated_hooks_indexes.into_iter().rev() {
        lines.remove(index);
    }

    if hooks_index.is_none() {
        if let Some(index) = features_header_index {
            lines.insert(index + 1, "hooks = true".to_string());
            return join_toml_lines(lines, trailing_newline);
        }

        let mut result = content.trim_end_matches('\n').to_string();
        if !result.is_empty() {
            result.push('\n');
            result.push('\n');
        }
        result.push_str("[features]\nhooks = true\n");
        return result;
    }

    join_toml_lines(lines, trailing_newline)
}

fn build_kimi_config_with_hooks(content: &str, hook_path: &Path) -> String {
    let mut result = remove_kimi_config_block(content)
        .trim_end_matches('\n')
        .to_string();
    if !result.is_empty() {
        result.push('\n');
        result.push('\n');
    }

    result.push_str(KIMI_CONFIG_BLOCK_BEGIN);
    result.push('\n');
    for (event, action) in KIMI_HOOK_EVENTS {
        result.push_str(&kimi_hook_table(event, hook_path, action));
    }
    result.push_str(KIMI_CONFIG_BLOCK_END);
    result.push('\n');
    result
}

fn kimi_hook_table(event: &str, hook_path: &Path, action: &str) -> String {
    let command = hook_command(hook_path, Some(action));
    format!(
        "[[hooks]]\nevent = {}\ncommand = {}\ntimeout = 10\n\n",
        toml_basic_string(event),
        toml_basic_string(&command)
    )
}

fn remove_kimi_config_block(content: &str) -> String {
    let trailing_newline = content.ends_with('\n');
    let mut lines = Vec::new();
    let mut in_block = false;
    let mut removed_block = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == KIMI_CONFIG_BLOCK_BEGIN {
            in_block = true;
            removed_block = true;
            continue;
        }
        if in_block {
            if trimmed == KIMI_CONFIG_BLOCK_END {
                in_block = false;
            }
            continue;
        }
        lines.push(line.to_string());
    }

    if !removed_block {
        return content.to_string();
    }

    let mut result = join_toml_lines(lines, trailing_newline);
    while result.ends_with("\n\n") {
        result.pop();
    }
    if result == "\n" {
        String::new()
    } else {
        result
    }
}

fn toml_basic_string(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for ch in value.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\u{08}' => result.push_str("\\b"),
            '\t' => result.push_str("\\t"),
            '\n' => result.push_str("\\n"),
            '\u{0c}' => result.push_str("\\f"),
            '\r' => result.push_str("\\r"),
            ch if ch <= '\u{1f}' || ch == '\u{7f}' => {
                result.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}

fn join_toml_lines(lines: Vec<String>, trailing_newline: bool) -> String {
    let mut result = lines.join("\n");
    if trailing_newline || result.is_empty() {
        result.push('\n');
    }
    result
}

fn toml_table_header(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || !trimmed.starts_with('[') {
        return None;
    }

    let header_end = if trimmed.starts_with("[[") {
        trimmed.find("]]").map(|index| index + 2)?
    } else {
        trimmed.find(']').map(|index| index + 1)?
    };
    let header = &trimmed[..header_end];
    let rest = trimmed[header_end..].trim_start();
    if !rest.is_empty() && !rest.starts_with('#') {
        return None;
    }

    Some(header)
}

fn is_toml_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') || !trimmed.starts_with(key) {
        return false;
    }

    trimmed[key.len()..].trim_start().starts_with('=')
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn hook_command(hook_path: &Path, action: Option<&str>) -> String {
    let path = hook_path.display().to_string();
    #[cfg(windows)]
    {
        let mut command = format!(
            "powershell -NoProfile -ExecutionPolicy Bypass -File {}",
            windows_command_quote(&path)
        );
        if let Some(action) = action {
            command.push(' ');
            command.push_str(action);
        }
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = format!("bash {}", shell_single_quote(&path));
        if let Some(action) = action {
            command.push(' ');
            command.push_str(action);
        }
        command
    }
}

fn legacy_bash_hook_command(hook_path: &Path, action: Option<&str>) -> String {
    let mut command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    if let Some(action) = action {
        command.push(' ');
        command.push_str(action);
    }
    command
}

#[cfg(windows)]
fn windows_command_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn make_executable(_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(_path, perms)?;
    }

    Ok(())
}

fn pi_extension_dir() -> io::Result<PathBuf> {
    Ok(
        config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".pi", "agent"])?
            .join("extensions"),
    )
}

fn omp_extension_dir() -> io::Result<PathBuf> {
    Ok(
        config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".omp", "agent"])?
            .join("extensions"),
    )
}

fn claude_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CLAUDE_CONFIG_DIR_ENV_VAR, &[".claude"])
}

fn codex_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CODEX_HOME_ENV_VAR, &[".codex"])
}

fn kimi_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(KIMI_CODE_HOME_ENV_VAR, &[".kimi-code"])
}

fn copilot_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(COPILOT_HOME_ENV_VAR, &[".copilot"])
}

fn devin_dir() -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value)).map(|path| path.join("devin"));
    }

    Ok(home_dir()?.join(".config").join("devin"))
}

fn droid_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".factory"))
}

fn opencode_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/opencode"))
}

fn kilo_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/kilo"))
}

fn hermes_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".hermes"))
}

fn hermes_plugin_dir() -> io::Result<PathBuf> {
    Ok(hermes_dir()?
        .join("plugins")
        .join(HERMES_PLUGIN_INSTALL_NAME))
}

fn qodercli_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(QODERCLI_CONFIG_DIR_ENV_VAR, &[".qoder"])
}

fn cursor_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CURSOR_CONFIG_DIR_ENV_VAR, &[".cursor"])
}

#[cfg(test)]
pub(crate) fn integration_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(unix)]
    #[test]
    fn enforce_agent_version_rejects_old_version() {
        let requirement = AgentVersionRequirement {
            label: "kimi code",
            binary: "echo",
            args: &["0.12.1"],
            min_version: "0.14.0",
        };
        let err =
            enforce_agent_version(&requirement).expect_err("old version must fail the install");
        let message = err.to_string();
        assert!(message.contains("0.12.1"));
        assert!(message.contains("0.14.0"));
        assert!(message.contains("upgrade"));
    }

    #[cfg(unix)]
    #[test]
    fn enforce_agent_version_accepts_current_version() {
        let requirement = AgentVersionRequirement {
            label: "kimi code",
            binary: "echo",
            args: &["0.14.0"],
            min_version: "0.14.0",
        };
        let result = enforce_agent_version(&requirement)
            .expect("matching version must not fail the install");
        assert!(result.is_none(), "matching version must not warn");
    }

    fn clear_integration_path_env() {
        std::env::remove_var(PI_CODING_AGENT_DIR_ENV_VAR);
        std::env::remove_var(CLAUDE_CONFIG_DIR_ENV_VAR);
        std::env::remove_var(CODEX_HOME_ENV_VAR);
        std::env::remove_var(COPILOT_HOME_ENV_VAR);
        std::env::remove_var(KIMI_CODE_HOME_ENV_VAR);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
        std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
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

    fn assert_kimi_hook(config: &str, hook_path: &Path, event: &str, action: &str) {
        let command = kimi_hook_command(hook_path, action);
        let hooks = kimi_config_hooks(config);
        assert!(
            hooks.iter().any(|hook| {
                hook.get("event").and_then(toml::Value::as_str) == Some(event)
                    && hook.get("command").and_then(toml::Value::as_str) == Some(command.as_str())
                    && hook.get("timeout").and_then(toml::Value::as_integer) == Some(10)
            }),
            "missing kimi hook for {event} -> {action}"
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

    #[cfg(windows)]
    #[test]
    fn home_dir_uses_userprofile_when_home_is_missing() {
        let _lock = integration_env_lock();
        let base = unique_base();
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        std::env::remove_var("HOME");
        std::env::set_var("USERPROFILE", &base);

        assert_eq!(home_dir().unwrap(), base);

        if let Some(home) = previous_home {
            std::env::set_var("HOME", home);
        }
        if let Some(userprofile) = previous_userprofile {
            std::env::set_var("USERPROFILE", userprofile);
        } else {
            std::env::remove_var("USERPROFILE");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_supports_only_cli_hook_integrations() {
        use crate::api::schema::IntegrationTarget;

        assert!(!integration_target_supported(IntegrationTarget::Pi));
        assert!(!integration_target_supported(IntegrationTarget::Omp));
        assert!(!integration_target_supported(IntegrationTarget::Opencode));
        assert!(!integration_target_supported(IntegrationTarget::Kilo));
        assert!(!integration_target_supported(IntegrationTarget::Hermes));
        assert!(!integration_target_supported(IntegrationTarget::Cursor));
        assert!(!integration_target_supported(IntegrationTarget::Devin));

        assert!(integration_target_supported(IntegrationTarget::Claude));
        assert!(integration_target_supported(IntegrationTarget::Codex));
        assert!(integration_target_supported(IntegrationTarget::Copilot));
        assert!(integration_target_supported(IntegrationTarget::Droid));
        assert!(integration_target_supported(IntegrationTarget::Kimi));
        assert!(integration_target_supported(IntegrationTarget::Qodercli));
    }

    #[cfg(windows)]
    #[test]
    fn windows_does_not_offer_unsupported_integrations_even_when_commands_exist() {
        use crate::api::schema::IntegrationTarget;

        let _lock = integration_env_lock();
        let base = unique_base();
        let bin = base.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let original_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &bin);

        fs::write(bin.join("pi.cmd"), "@echo off\r\n").unwrap();
        fs::write(bin.join("omp.cmd"), "@echo off\r\n").unwrap();
        fs::write(bin.join("opencode.cmd"), "@echo off\r\n").unwrap();
        fs::write(bin.join("kilo.cmd"), "@echo off\r\n").unwrap();
        fs::write(bin.join("hermes.exe"), "").unwrap();
        fs::write(bin.join("cursor-agent.cmd"), "@echo off\r\n").unwrap();
        fs::write(bin.join("devin.cmd"), "@echo off\r\n").unwrap();

        assert!(!integration_target_available(IntegrationTarget::Pi));
        assert!(!integration_target_available(IntegrationTarget::Omp));
        assert!(!integration_target_available(IntegrationTarget::Opencode));
        assert!(!integration_target_available(IntegrationTarget::Kilo));
        assert!(!integration_target_available(IntegrationTarget::Hermes));
        assert!(!integration_target_available(IntegrationTarget::Cursor));
        assert!(!integration_target_available(IntegrationTarget::Devin));

        if let Some(path) = original_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_rejects_unsupported_integration_before_config_lookup() {
        use crate::api::schema::IntegrationTarget;

        let _lock = integration_env_lock();
        let original_home = std::env::var_os("HOME");
        let original_userprofile = std::env::var_os("USERPROFILE");
        let original_homedrive = std::env::var_os("HOMEDRIVE");
        let original_homepath = std::env::var_os("HOMEPATH");
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
        std::env::remove_var("HOMEDRIVE");
        std::env::remove_var("HOMEPATH");

        let err = install_target(IntegrationTarget::Pi).unwrap_err();
        assert_eq!(
            err.to_string(),
            "pi integration is not supported on Windows"
        );

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        }
        if let Some(userprofile) = original_userprofile {
            std::env::set_var("USERPROFILE", userprofile);
        }
        if let Some(homedrive) = original_homedrive {
            std::env::set_var("HOMEDRIVE", homedrive);
        }
        if let Some(homepath) = original_homepath {
            std::env::set_var("HOMEPATH", homepath);
        }
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(windows)]
    fn command_available_finds_windows_command_shims_on_path() {
        let _lock = integration_env_lock();
        let base = unique_base();
        let bin = base.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let original_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &bin);

        fs::write(bin.join("claude.cmd"), "@echo off\r\n").unwrap();
        assert!(command_available("claude"));

        fs::write(bin.join("codex.exe"), "").unwrap();
        assert!(command_available("codex"));

        assert!(!command_available("missing-agent"));

        if let Some(path) = original_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    #[cfg(windows)]
    fn qodercli_availability_checks_windows_aliases() {
        let _lock = integration_env_lock();
        let base = unique_base();
        let bin = base.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let original_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &bin);

        fs::write(bin.join("qoder.cmd"), "@echo off\r\n").unwrap();

        assert!(integration_target_available(
            crate::api::schema::IntegrationTarget::Qodercli
        ));

        if let Some(path) = original_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    #[cfg(windows)]
    fn hermes_layout_can_exist_without_making_unsupported_target_available() {
        let _lock = integration_env_lock();
        let base = unique_base();
        let local_app_data = base.join("local-app-data");
        let hermes_bin = local_app_data.join("hermes").join("bin");
        fs::create_dir_all(&hermes_bin).unwrap();
        fs::write(hermes_bin.join("hermes.exe"), "").unwrap();
        let original_local_app_data = std::env::var_os("LOCALAPPDATA");
        let original_path = std::env::var_os("PATH");
        std::env::set_var("LOCALAPPDATA", &local_app_data);
        std::env::set_var("PATH", "");

        assert!(hermes_install_layout_available());
        assert!(!integration_target_available(
            crate::api::schema::IntegrationTarget::Hermes
        ));

        if let Some(local_app_data) = original_local_app_data {
            std::env::set_var("LOCALAPPDATA", local_app_data);
        } else {
            std::env::remove_var("LOCALAPPDATA");
        }
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
    fn install_omp_uses_pi_coding_agent_dir_env() {
        let _lock = integration_env_lock();
        let base = unique_base();
        let agent_dir = base.join("custom-omp-agent");
        let ext_dir = agent_dir.join("extensions");
        fs::create_dir_all(&ext_dir).unwrap();
        std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, &agent_dir);

        let installed = install_omp().unwrap();

        assert_eq!(
            installed.extension_path,
            ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
        );
        assert!(!installed.removed_legacy_pi_extension);

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
        assert_eq!(codex.expected_version, 6);
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
            serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
                .unwrap();
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
            serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
                .unwrap();
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
        for (event, action) in KIMI_HOOK_EVENTS {
            assert_kimi_hook(&config, &installed.hook_path, event, action);
        }

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
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
            serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap())
                .unwrap();
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
            serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap())
                .unwrap();
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
            serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap())
                .unwrap();
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
            serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap())
                .unwrap();

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
            serde_json::from_str(&fs::read_to_string(droid_dir.join("hooks.json")).unwrap())
                .unwrap();
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
        let plugin_content = fs::read_to_string(&installed.plugin_path).unwrap();

        assert_eq!(
            installed.plugin_path,
            opencode_dir
                .join("plugins")
                .join(OPENCODE_PLUGIN_INSTALL_NAME)
        );
        assert_eq!(plugin_content, OPENCODE_PLUGIN_ASSET);

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_opencode_removes_plugin_when_present() {
        let _lock = integration_env_lock();
        let base = unique_base();
        let home = base.join("home");
        let opencode_dir = home.join(".config/opencode/plugins");
        fs::create_dir_all(&opencode_dir).unwrap();
        fs::write(
            opencode_dir.join(OPENCODE_PLUGIN_INSTALL_NAME),
            OPENCODE_PLUGIN_ASSET,
        )
        .unwrap();
        std::env::set_var("HOME", &home);

        let result = uninstall_opencode().unwrap();

        assert!(result.removed_plugin);
        assert!(!result.plugin_path.exists());

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

        // STILL state-only: it reports the agent session path/id, publishes state, and
        // reports the agent session control surface (root-session scoped).
        assert!(PI_EXTENSION_ASSET.contains("agent_session_path: currentAgentSessionPath"));
        assert!(PI_EXTENSION_ASSET.contains("agent_session_id: currentAgentSessionId"));
        assert!(PI_EXTENSION_ASSET.contains("publishState(true)"));
        assert!(PI_EXTENSION_ASSET.contains("pane.report_agent"));
        assert!(PI_EXTENSION_ASSET.contains("pane.report_agent_session"));

        // the asset version marker is bumped to the root-session-protected revision.
        assert_eq!(parse_integration_version(PI_EXTENSION_ASSET), Some(5));
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
    fn bundled_integration_assets_report_session_refs() {
        assert!(PI_EXTENSION_ASSET.contains("agent_session_path: currentAgentSessionPath"));
        assert!(PI_EXTENSION_ASSET.contains("agent_session_id: currentAgentSessionId"));
        assert!(PI_EXTENSION_ASSET.contains("publishState(true)"));
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
        assert!(CODEX_HOOK_ASSET.contains("agent_session_id"));
        assert!(CODEX_HOOK_ASSET.contains("pane.report_agent_session"));
        assert!(!CODEX_HOOK_ASSET.contains("\"state\": action"));
        assert!(!CODEX_HOOK_ASSET.contains("pane.release_agent"));
        assert!(KIMI_HOOK_ASSET.contains("source = \"zynk:kimi\""));
        assert!(KIMI_HOOK_ASSET.contains("agent_session_id"));
        assert!(KIMI_HOOK_ASSET.contains("pane.report_agent_session"));
        assert!(KIMI_HOOK_ASSET.contains("\"state\": action"));
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
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("session_id = _session_id(kwargs)"));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("agent_session_id"));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("pane.report_agent\","));
        assert!(!HERMES_PLUGIN_INIT_ASSET.contains("pane.release_agent"));
        assert!(QODERCLI_HOOK_ASSET.contains("ZYNK_HOOK_INPUT_FILE"));
        assert!(QODERCLI_HOOK_ASSET.contains("agent_session_id"));
        assert!(QODERCLI_HOOK_ASSET.contains("pane.report_agent_session"));
        assert!(!QODERCLI_HOOK_ASSET.contains("\"state\": action"));
        assert!(!QODERCLI_HOOK_ASSET.contains("pane.release_agent"));
        assert!(!QODERCLI_HOOK_ASSET.contains("QODER_HOOK_EVENT"));
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
            serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap())
                .unwrap();
        let hooks = hooks_file.get("hooks").and_then(Value::as_object).unwrap();
        let session_start = hooks.get("sessionStart").and_then(Value::as_array).unwrap();
        assert_eq!(session_start.len(), 1);
        assert!(session_start[0]
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| {
                command.starts_with("bash ")
                    && command.contains("zynk-agent-state.sh")
                    && command.ends_with(" session")
            }));
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
            serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap())
                .unwrap();
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
            serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap())
                .unwrap();
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
            serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap())
                .unwrap();
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
}
