use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::env::*;

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
        crate::api::schema::IntegrationTarget::Mastracode => "mastracode",
        crate::api::schema::IntegrationTarget::AntigravityCli => "antigravity-cli",
        crate::api::schema::IntegrationTarget::Grok => "grok",
    }
}

pub(crate) fn integration_target_command(
    target: crate::api::schema::IntegrationTarget,
) -> &'static str {
    integration_target_command_names(target)[0]
}

pub(crate) fn integration_target_command_names(
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
        crate::api::schema::IntegrationTarget::Mastracode => &["mastracode"],
        crate::api::schema::IntegrationTarget::AntigravityCli => &["agy"],
        crate::api::schema::IntegrationTarget::Grok => &["grok"],
    }
}

pub(crate) fn cursor_command_names() -> &'static [&'static str] {
    &["cursor-agent"]
}

pub(crate) fn integration_target_available(target: crate::api::schema::IntegrationTarget) -> bool {
    integration_target_command_names(target)
        .iter()
        .any(|command| command_available(command))
        || integration_target_install_layout_available(target)
}

pub(crate) fn qodercli_command_names() -> &'static [&'static str] {
    &["qodercli"]
}

pub(crate) fn integration_target_install_layout_available(
    target: crate::api::schema::IntegrationTarget,
) -> bool {
    match target {
        crate::api::schema::IntegrationTarget::Codex => codex_standalone_binary_available(),
        crate::api::schema::IntegrationTarget::Hermes => hermes_install_layout_available(),
        _ => false,
    }
}

pub(crate) fn command_available(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        command_path_candidates(&dir, command)
            .into_iter()
            .any(|path| executable_file_exists(&path))
    })
}

pub(crate) fn command_path_candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    vec![dir.join(command)]
}

pub(crate) fn executable_file_exists(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

pub(crate) fn codex_standalone_binary_available() -> bool {
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

pub(crate) fn codex_executable_name() -> &'static str {
    "codex"
}

pub(crate) fn hermes_install_layout_available() -> bool {
    false
}

pub(crate) fn installed_integration_statuses() -> Vec<super::IntegrationStatus> {
    integration_specs()
        .into_iter()
        .filter_map(|(target, path, expected_version)| {
            Some(integration_status_at(target, path.ok()?, expected_version))
        })
        .collect()
}

pub(crate) fn integration_recommendations() -> Vec<super::IntegrationRecommendation> {
    integration_specs()
        .into_iter()
        .filter_map(|(target, path, expected_version)| {
            let path = path.ok()?;
            let status = integration_status_at(target, path.clone(), expected_version);
            Some(super::IntegrationRecommendation {
                target,
                label: integration_target_label(target),
                command: integration_target_command(target),
                available: integration_target_available(target)
                    || status.state != super::IntegrationStatusKind::NotInstalled,
                path,
                state: status.state,
            })
        })
        .collect()
}

pub(crate) fn outdated_installed_integrations() -> Vec<super::IntegrationStatus> {
    installed_integration_statuses()
        .into_iter()
        .filter(|status| status.state == super::IntegrationStatusKind::Outdated)
        .collect()
}

pub(crate) fn integration_specs() -> [(
    crate::api::schema::IntegrationTarget,
    io::Result<PathBuf>,
    u32,
); 16] {
    [
        (
            crate::api::schema::IntegrationTarget::Pi,
            pi_extension_dir().map(|dir| dir.join(super::PI_EXTENSION_INSTALL_NAME)),
            super::PI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Omp,
            omp_extension_dir().map(|dir| dir.join(super::OMP_EXTENSION_INSTALL_NAME)),
            super::OMP_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Claude,
            claude_dir().map(|dir| dir.join("hooks").join(super::CLAUDE_HOOK_INSTALL_NAME)),
            super::CLAUDE_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Codex,
            codex_dir().map(|dir| dir.join(super::CODEX_HOOK_INSTALL_NAME)),
            super::CODEX_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Copilot,
            copilot_dir().map(|dir| dir.join("hooks").join(super::COPILOT_HOOK_INSTALL_NAME)),
            super::COPILOT_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Devin,
            devin_dir().map(|dir| dir.join(super::DEVIN_HOOK_INSTALL_NAME)),
            super::DEVIN_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Droid,
            droid_dir().map(|dir| dir.join("hooks").join(super::DROID_HOOK_INSTALL_NAME)),
            super::DROID_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Kimi,
            kimi_dir().map(|dir| dir.join("hooks").join(super::KIMI_HOOK_INSTALL_NAME)),
            super::KIMI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Opencode,
            opencode_dir().map(|dir| {
                dir.join("plugins")
                    .join(super::OPENCODE_PLUGIN_INSTALL_NAME)
            }),
            super::OPENCODE_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Kilo,
            kilo_dir().map(|dir| dir.join("plugin").join(super::KILO_PLUGIN_INSTALL_NAME)),
            super::KILO_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Hermes,
            hermes_plugin_dir().map(|dir| dir.join(super::HERMES_PLUGIN_INIT_INSTALL_NAME)),
            super::HERMES_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Qodercli,
            qodercli_dir().map(|dir| dir.join("hooks").join(super::QODERCLI_HOOK_INSTALL_NAME)),
            super::QODERCLI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Cursor,
            cursor_dir().map(|dir| dir.join(super::CURSOR_HOOK_INSTALL_NAME)),
            super::CURSOR_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Mastracode,
            mastracode_dir().map(|dir| dir.join("hooks").join(super::MASTRACODE_HOOK_INSTALL_NAME)),
            super::MASTRACODE_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::AntigravityCli,
            antigravity_cli_dir().map(|dir| {
                dir.join("hooks")
                    .join(super::ANTIGRAVITY_CLI_HOOK_INSTALL_NAME)
            }),
            super::ANTIGRAVITY_CLI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Grok,
            grok_dir().map(|dir| dir.join("hooks").join(super::GROK_HOOK_INSTALL_NAME)),
            super::GROK_INTEGRATION_VERSION,
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

/// Whether the zynk-owned Grok hook config exactly matches the installed
/// integration. JSON formatting and object key order do not affect validity.
fn grok_hook_config_is_valid(hook_path: &Path) -> bool {
    let Some(hooks_dir) = hook_path.parent() else {
        return false;
    };
    let config_path = hooks_dir.join(super::GROK_HOOK_CONFIG_INSTALL_NAME);
    fs::read_to_string(config_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .is_some_and(|config| config == super::targets::grok_hook_config(hook_path))
}

/// Whether opencode's SECOND installed artifact is present, current and registered.
///
/// opencode loads the TUI selection plugin only when `tui.jsonc` lists it, so a
/// current server plugin beside a missing, stale or unregistered TUI plugin is a
/// half-install that silently stops reporting the locally selected session.
/// `integration_specs()` keys one path per target, so this reaches the sibling
/// artifact from the registered plugin path (`<config>/plugins/<name>` -> `<config>`).
fn opencode_tui_integration_is_valid(plugin_path: &Path, expected_version: u32) -> bool {
    let Some(config_dir) = plugin_path.parent().and_then(Path::parent) else {
        return false;
    };
    let tui_plugin_path = config_dir.join(super::OPENCODE_TUI_PLUGIN_INSTALL_NAME);
    let tui_plugin_current = fs::read_to_string(tui_plugin_path)
        .ok()
        .and_then(|content| parse_integration_version(&content))
        .is_some_and(|version| version >= expected_version);
    tui_plugin_current
        && super::opencode_config::tui_plugin_is_configured(
            config_dir,
            super::OPENCODE_TUI_PLUGIN_SPEC,
        )
}

pub(crate) fn integration_status_at(
    target: crate::api::schema::IntegrationTarget,
    path: PathBuf,
    expected_version: u32,
) -> super::IntegrationStatus {
    let resolved = path.is_file().then(|| path.clone());

    let Some(resolved) = resolved else {
        return super::IntegrationStatus {
            target,
            path,
            state: super::IntegrationStatusKind::NotInstalled,
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
    let mut state =
        if is_native && installed_version.is_some_and(|version| version >= expected_version) {
            super::IntegrationStatusKind::Current
        } else {
            super::IntegrationStatusKind::Outdated
        };

    // Grok only invokes the hook when the zynk-owned `hooks/zynk.json` registers
    // it, so a current hook script with a missing or broken config is a
    // nonfunctional install: report it as outdated so `zynk integration status`
    // flags it and a reinstall rewrites both files.
    if target == crate::api::schema::IntegrationTarget::Grok
        && state == super::IntegrationStatusKind::Current
        && !grok_hook_config_is_valid(&resolved)
    {
        state = super::IntegrationStatusKind::Outdated;
    }
    if target == crate::api::schema::IntegrationTarget::Opencode
        && state == super::IntegrationStatusKind::Current
        && !opencode_tui_integration_is_valid(&resolved, expected_version)
    {
        state = super::IntegrationStatusKind::Outdated;
    }

    super::IntegrationStatus {
        target,
        path: resolved,
        state,
        installed_version,
        expected_version,
    }
}

pub(crate) fn parse_integration_version(content: &str) -> Option<u32> {
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
            .strip_prefix(super::INTEGRATION_VERSION_MARKER)?
            .trim()
            .parse()
            .ok()
    })
}

/// True when the hook content carries pre-rebrand Herdr residue tokens. A version
/// marker alone is not enough to trust a hook as native: a stale Herdr-era hook can
/// carry `ZYNK_INTEGRATION_VERSION=` yet still identify as `herdr:<agent>` and export
/// `HERDR_*` env. Such a hook must never be reported as a current native integration.
pub(crate) fn hook_has_herdr_residue(content: &str) -> bool {
    content.contains("HERDR_") || content.contains("herdr:")
}

/// True when the hook content is a genuine zynk-native hook for `expected_id`: it
/// declares the matching `ZYNK_INTEGRATION_ID=<expected_id>` marker and carries no
/// Herdr residue. Marker parsing mirrors `parse_integration_version` (comment-prefix
/// stripping) so it works uniformly across `.sh`/`.ts`/`.js`/`.py` hooks.
pub(crate) fn hook_is_native(content: &str, expected_id: &str) -> bool {
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
            .strip_prefix(super::INTEGRATION_ID_MARKER)
            .is_some_and(|id| id.trim() == expected_id)
    })
}

/// The `ZYNK_INTEGRATION_ID=<id>` value embedded in this target's native hook asset.
/// Declared explicitly per target (not derived from the display label) so a label
/// rename can never silently weaken the native-identity gate in `integration_status_at`.
pub(crate) fn expected_integration_id(
    target: crate::api::schema::IntegrationTarget,
) -> &'static str {
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
        IntegrationTarget::Mastracode => "mastracode",
        IntegrationTarget::AntigravityCli => "antigravity_cli",
        IntegrationTarget::Grok => "grok",
    }
}
