mod actions;
mod command;
mod config_edit;
mod env;
mod file_ops;
mod registry;
mod targets;
mod types;
mod version;

pub(crate) use actions::{install_target, uninstall_target};
#[cfg(test)]
pub(crate) use env::integration_env_lock;
pub(crate) use env::{
    apply_pane_base_env, ZYNK_PANE_ID_ENV_VAR, ZYNK_TAB_ID_ENV_VAR, ZYNK_WORKSPACE_ID_ENV_VAR,
};
pub(crate) use registry::{
    installed_integration_statuses, integration_recommendations, integration_target_label,
    print_outdated_update_notice,
};
pub(crate) use types::{IntegrationRecommendation, IntegrationStatus, IntegrationStatusKind};

pub(crate) const PI_EXTENSION_INSTALL_NAME: &str = "zynk-agent-state.ts";
pub(crate) const PI_EXTENSION_ASSET: &str = include_str!("assets/pi/zynk-agent-state.ts");
pub(crate) const PI_INTEGRATION_VERSION: u32 = 8;
pub(crate) const OMP_EXTENSION_INSTALL_NAME: &str = "zynk-omp-agent-state.ts";
// Pre-rebrand on-disk name of the omp extension; uninstall strips it too.
pub(crate) const OMP_EXTENSION_ASSET: &str = include_str!("assets/omp/zynk-agent-state.ts");
pub(crate) const OMP_INTEGRATION_VERSION: u32 = 6;
pub(crate) const CLAUDE_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const CLAUDE_HOOK_ASSET: &str = include_str!("assets/claude/zynk-agent-state.sh");
pub(crate) const CLAUDE_INTEGRATION_VERSION: u32 = 7;
pub(crate) const CODEX_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const CODEX_HOOK_ASSET: &str = include_str!("assets/codex/zynk-agent-state.sh");
pub(crate) const CODEX_INTEGRATION_VERSION: u32 = 6;
pub(crate) const KIMI_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const KIMI_HOOK_ASSET: &str = include_str!("assets/kimi/zynk-agent-state.sh");
pub(crate) const KIMI_INTEGRATION_VERSION: u32 = 5;
pub(crate) const KIMI_CONFIG_BLOCK_BEGIN: &str = "# >>> zynk kimi integration";
pub(crate) const KIMI_CONFIG_BLOCK_END: &str = "# <<< zynk kimi integration";
// Pre-rebrand kimi config-block fences; removal strips them too (migration compat).
pub(crate) const KIMI_MIN_VERSION: &str = "0.14.0";
pub(crate) const KIMI_ASK_USER_QUESTION_MATCHER: &str = "^AskUserQuestion$";
pub(crate) const KIMI_OTHER_TOOL_MATCHER: &str = "^(?!AskUserQuestion$).*$";
// (event, tool matcher, reported state). A `None` matcher fires on every tool.
pub(crate) const KIMI_HOOK_EVENTS: [(&str, Option<&str>, &str); 12] = [
    ("SessionStart", None, "session"),
    ("UserPromptSubmit", None, "working"),
    ("PreToolUse", Some(KIMI_OTHER_TOOL_MATCHER), "working"),
    (
        "PreToolUse",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "blocked",
    ),
    (
        "PostToolUse",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "working",
    ),
    (
        "PostToolUseFailure",
        Some(KIMI_ASK_USER_QUESTION_MATCHER),
        "working",
    ),
    ("SubagentStart", None, "working"),
    ("PreCompact", None, "working"),
    ("PermissionRequest", None, "blocked"),
    ("PermissionResult", None, "working"),
    ("Stop", None, "idle"),
    ("Interrupt", None, "idle"),
];
pub(crate) const COPILOT_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const COPILOT_HOOK_ASSET: &str = include_str!("assets/copilot/zynk-agent-state.sh");
pub(crate) const COPILOT_INTEGRATION_VERSION: u32 = 2;
pub(crate) const COPILOT_HOOK_EVENTS: [&str; 1] = ["SessionStart"];
pub(crate) const COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS: [&str; 9] = [
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
pub(crate) const DEVIN_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const DEVIN_HOOK_ASSET: &str = include_str!("assets/devin/zynk-agent-state.sh");
pub(crate) const DEVIN_INTEGRATION_VERSION: u32 = 2;
pub(crate) const DEVIN_HOOK_EVENTS: [(&str, &str); 6] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "session"),
    ("PreToolUse", "session"),
    ("PostToolUse", "session"),
    ("PermissionRequest", "session"),
    ("Stop", "session"),
];
pub(crate) const DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
pub(crate) const DROID_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const DROID_HOOK_ASSET: &str = include_str!("assets/droid/zynk-agent-state.sh");
pub(crate) const DROID_INTEGRATION_VERSION: u32 = 2;
pub(crate) const DROID_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
pub(crate) const DROID_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 9] = [
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
pub(crate) const OPENCODE_PLUGIN_INSTALL_NAME: &str = "zynk-agent-state.js";
pub(crate) const OPENCODE_PLUGIN_ASSET: &str = include_str!("assets/opencode/zynk-agent-state.js");
pub(crate) const OPENCODE_INTEGRATION_VERSION: u32 = 9;
pub(crate) const KILO_PLUGIN_INSTALL_NAME: &str = "zynk-agent-state.js";
pub(crate) const KILO_PLUGIN_ASSET: &str = include_str!("assets/kilo/zynk-agent-state.js");
pub(crate) const KILO_INTEGRATION_VERSION: u32 = 3;
pub(crate) const HERMES_PLUGIN_INSTALL_NAME: &str = "zynk-agent-state";
// Legacy hermes plugin name written by pre-rebrand installs; uninstall strips it
// from the user's config + removes its plugin dir (bounded migration cleanup).
pub(crate) const HERMES_PLUGIN_MANIFEST_INSTALL_NAME: &str = "plugin.yaml";
pub(crate) const HERMES_PLUGIN_INIT_INSTALL_NAME: &str = "__init__.py";
pub(crate) const HERMES_PLUGIN_MANIFEST_ASSET: &str = include_str!("assets/hermes/plugin.yaml");
pub(crate) const HERMES_PLUGIN_INIT_ASSET: &str = include_str!("assets/hermes/__init__.py");
pub(crate) const HERMES_INTEGRATION_VERSION: u32 = 3;
pub(crate) const QODERCLI_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const QODERCLI_HOOK_ASSET: &str = include_str!("assets/qodercli/zynk-agent-state.sh");
pub(crate) const QODERCLI_INTEGRATION_VERSION: u32 = 2;
pub(crate) const QODERCLI_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
pub(crate) const QODERCLI_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 12] = [
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
pub(crate) const CURSOR_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const CURSOR_HOOK_ASSET: &str = include_str!("assets/cursor/zynk-agent-state.sh");
pub(crate) const CURSOR_INTEGRATION_VERSION: u32 = 1;
pub(crate) const MASTRACODE_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const MASTRACODE_HOOK_ASSET: &str =
    include_str!("assets/mastracode/zynk-agent-state.sh");
pub(crate) const MASTRACODE_INTEGRATION_VERSION: u32 = 1;
pub(crate) const MASTRACODE_HOOK_TIMEOUT_MS: u64 = 10_000;
pub(crate) const MASTRACODE_HOOK_EVENTS: [(&str, &str); 12] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("AgentStart", "working"),
    ("PreToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("PermissionResult", "working"),
    ("SubagentStart", "working"),
    ("SubagentEnd", "working"),
    ("Interrupt", "idle"),
    ("AgentEnd", "idle"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
pub(crate) const ANTIGRAVITY_CLI_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const ANTIGRAVITY_CLI_HOOK_ASSET: &str =
    include_str!("assets/antigravity_cli/zynk-agent-state.sh");
pub(crate) const ANTIGRAVITY_CLI_INTEGRATION_VERSION: u32 = 1;
/// Antigravity CLI keys `hooks.json` by hook name, so every zynk entry lives
/// under one zynk-owned block that install rewrites and uninstall removes.
pub(crate) const ANTIGRAVITY_CLI_HOOK_BLOCK_NAME: &str = "zynk";
pub(crate) const ANTIGRAVITY_CLI_HOOK_TIMEOUT_SEC: u64 = 10;
/// `(event, reported action)`. Session-only: `PreInvocation` is the only event
/// we need because it carries `conversationId`. The others cannot express
/// lifecycle safely — Antigravity CLI has no blocked event, `PostInvocation` is
/// skipped on interruption, and `Stop` is end-of-turn rather than process exit.
/// Screen detection owns agent state instead.
///
/// `PreInvocation` takes a flat handler list; only the `PreToolUse`/`PostToolUse`
/// events accept a `matcher`/`hooks` wrapper, and sending one here would
/// invalidate the whole file.
pub(crate) const ANTIGRAVITY_CLI_HOOK_EVENTS: [(&str, &str); 1] = [("PreInvocation", "session")];
pub(crate) const GROK_HOOK_INSTALL_NAME: &str = "zynk-agent-state.sh";
pub(crate) const GROK_HOOK_CONFIG_INSTALL_NAME: &str = "zynk.json";
pub(crate) const GROK_HOOK_ASSET: &str = include_str!("assets/grok/zynk-agent-state.sh");
pub(crate) const GROK_INTEGRATION_VERSION: u32 = 1;
pub(crate) const INTEGRATION_VERSION_MARKER: &str = "ZYNK_INTEGRATION_VERSION=";
// Pre-rebrand installs embedded `ZYNK_INTEGRATION_VERSION=`. status() still
// recognizes it (legacy installs surface as Outdated → prompt reinstall) and
// uninstall keys legacy hook cleanup off the matching ID marker below.
pub(crate) const INTEGRATION_ID_MARKER: &str = "ZYNK_INTEGRATION_ID=";

pub(crate) const INSTALL_WARNING_PREFIX: &str = "warning:";

#[cfg(test)]
mod tests;
