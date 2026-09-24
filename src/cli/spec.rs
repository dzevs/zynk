// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use clap::{Arg, ArgAction, Command, ValueHint};

// Completion data only. Manual dispatchers remain the CLI execution authority.
pub(super) fn command() -> Command {
    let command = Command::new("zynk")
        .about("terminal workspace manager for AI coding agents")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(help_flag())
        .arg(flag("no-session").help("Run monolithically without server/client session mode"))
        .arg(option("session", "NAME").help("Use or create a named persistent session"))
        .arg(option("remote", "TARGET").help("Attach through SSH to a remote Zynk server"))
        .arg(
            option("remote-keybindings", "MODE")
                .value_parser(["local", "server"])
                .help("Choose local or server keybindings for remote attach"),
        )
        .arg(flag("handoff").help("Opt into live handoff for update or remote attach"))
        .arg(flag("default-config").help("Print default configuration and exit"))
        .arg(flag("skill").help("Print the bundled agent skill and exit"))
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::SetTrue)
                .help("Print version and exit"),
        )
        .subcommand(completion_command())
        .subcommand(update_command())
        .subcommand(status_command())
        .subcommand(config_command())
        .subcommand(channel_command())
        .subcommand(server_command())
        .subcommand(api_command())
        .subcommand(workspace_command())
        .subcommand(worktree_command())
        .subcommand(tab_command())
        .subcommand(notification_command())
        .subcommand(agent_command())
        .subcommand(pane_command())
        .subcommand(wait_command())
        .subcommand(terminal_command())
        .subcommand(session_command())
        .subcommand(integration_command())
        .subcommand(plugin_command())
        .subcommand(skill_command())
        .subcommand(db_command())
        .subcommand(zynk_command())
        .subcommand(message_command("send", "Send a native message"))
        .subcommand(message_command(
            "reply",
            "Reply with a derived conversation parent",
        ))
        .subcommand(id_command("thread", "id", "Read a conversation").arg(json_flag()))
        .subcommand(
            Command::new("inbox")
                .about("Read messages addressed to an agent")
                .arg(option("agent", "LABEL"))
                .arg(option("limit", "N"))
                .arg(json_flag()),
        )
        .subcommand(id_command("trace", "id", "Read messages by trace").arg(json_flag()))
        .subcommand(
            Command::new("whoami")
                .about("Show caller identity")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("who")
                .about("Show participant topology")
                .arg(json_flag()),
        )
        .subcommand(query_command());
    disable_auto_help(command)
}

fn disable_auto_help(command: Command) -> Command {
    command
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .mut_subcommands(disable_auto_help)
}

fn completion_command() -> Command {
    Command::new("completion")
        .visible_alias("completions")
        .about("Generate shell completion scripts")
        .arg(
            Arg::new("shell")
                .value_name("SHELL")
                .required(true)
                .value_parser(super::completion::SUPPORTED_SHELLS)
                .help("Shell to generate completions for"),
        )
}

fn update_command() -> Command {
    Command::new("update")
        .about("Explain source-only update policy (no downloader)")
        .arg(flag("handoff").help("Request handoff; source-only update remains unavailable"))
}

fn status_command() -> Command {
    Command::new("status")
        .about("Show local client and running server status")
        .arg(json_flag())
        .subcommand(
            Command::new("server")
                .about("Show running server status")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("client")
                .about("Show local client status")
                .arg(json_flag()),
        )
}

fn config_command() -> Command {
    Command::new("config")
        .about("Manage local configuration")
        .subcommand(Command::new("check").about("Validate config.toml and print diagnostics"))
        .subcommand(Command::new("reset-keys").about("Reset custom keybindings"))
}

fn channel_command() -> Command {
    Command::new("channel")
        .about("Inspect update channels (changes unavailable for source-only builds)")
        .subcommand(Command::new("show").about("Print the configured update channel"))
        .subcommand(
            Command::new("set")
                .about("Request a channel change (source-only builds refuse)")
                .arg(
                    Arg::new("channel")
                        .value_name("CHANNEL")
                        .required(true)
                        .value_parser(["stable", "preview"]),
                ),
        )
}

fn server_command() -> Command {
    Command::new("server")
        .about("Run or control the headless server")
        .subcommand(Command::new("stop").about("Stop the running server"))
        .subcommand(Command::new("reload-config").about("Reload config in the running server"))
        .subcommand(
            Command::new("agent-manifests")
                .about("Show active agent detection manifests")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("update-agent-manifests")
                .about("Fetch and reload agent detection manifests")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("reload-agent-manifests")
                .about("Reload local agent detection manifest overrides"),
        )
}

fn api_command() -> Command {
    Command::new("api")
        .about("Inspect socket API metadata and live runtime state")
        .subcommand(Command::new("snapshot").about("Print the live session snapshot as JSON"))
        .subcommand(
            Command::new("schema")
                .about("Print or write the bundled API schema")
                .arg(json_flag())
                .arg(path_option("output", "PATH")),
        )
}

fn workspace_command() -> Command {
    Command::new("workspace")
        .about("Manage workspaces over the socket API")
        .subcommand(Command::new("list").about("List workspaces"))
        .subcommand(
            Command::new("create")
                .about("Create a workspace")
                .arg(path_option("cwd", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("get", "workspace_id", "Show a workspace"))
        .subcommand(id_command("focus", "workspace_id", "Focus a workspace"))
        .subcommand(
            Command::new("rename")
                .about("Rename a workspace")
                .arg(required("workspace_id", "WORKSPACE_ID"))
                .arg(required("label", "LABEL").num_args(1..)),
        )
        .subcommand(
            Command::new("report-metadata")
                .about("Report display-only workspace metadata")
                .arg(required("workspace_id", "WORKSPACE_ID"))
                .arg(option("source", "ID"))
                .arg(repeatable_option("token", "NAME=VALUE"))
                .arg(repeatable_option("clear-token", "NAME"))
                .arg(option("seq", "N"))
                .arg(option("ttl-ms", "N")),
        )
        .subcommand(id_command("close", "workspace_id", "Close a workspace"))
}

fn worktree_command() -> Command {
    Command::new("worktree")
        .about("Manage Git worktree-backed workspaces")
        .subcommand(
            Command::new("list")
                .about("List worktree workspaces")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH")),
        )
        .subcommand(
            Command::new("create")
                .about("Create and open a Git worktree")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(option("branch", "NAME"))
                .arg(option("base", "REF"))
                .arg(path_option("path", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(
            Command::new("open")
                .about("Open an existing Git worktree")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(path_option("path", "PATH"))
                .arg(option("branch", "NAME"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove a worktree checkout")
                .arg(option("workspace", "ID"))
                .arg(flag("force")),
        )
}

fn tab_command() -> Command {
    Command::new("tab")
        .about("Manage tabs over the socket API")
        .subcommand(
            Command::new("list")
                .about("List tabs")
                .arg(option("workspace", "WORKSPACE_ID")),
        )
        .subcommand(
            Command::new("create")
                .about("Create a tab")
                .arg(option("workspace", "WORKSPACE_ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("get", "tab_id", "Show a tab"))
        .subcommand(id_command("focus", "tab_id", "Focus a tab"))
        .subcommand(
            Command::new("rename")
                .about("Rename a tab")
                .arg(required("tab_id", "TAB_ID"))
                .arg(required("label", "LABEL").num_args(1..)),
        )
        .subcommand(id_command("close", "tab_id", "Close a tab"))
}

fn notification_command() -> Command {
    Command::new("notification")
        .about("Show Zynk notifications")
        .subcommand(
            Command::new("show")
                .about("Show a notification")
                .arg(required("title", "TITLE"))
                .arg(option("body", "TEXT"))
                .arg(option("position", "POSITION").value_parser([
                    "top-left",
                    "top-right",
                    "bottom-left",
                    "bottom-right",
                ]))
                .arg(option("sound", "SOUND").value_parser(["none", "done", "request"])),
        )
}

fn agent_command() -> Command {
    Command::new("agent")
        .about("Control and inspect agent panes")
        .subcommand(Command::new("list").about("List agents"))
        .subcommand(id_command("get", "target", "Show an agent"))
        .subcommand(
            Command::new("read")
                .about("Read agent terminal output")
                .override_usage("zynk agent read <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(read_source_option(true))
                .arg(option("lines", "N"))
                .arg(text_ansi_format_option())
                .arg(flag("ansi")),
        )
        .subcommand(
            Command::new("send")
                .about("Send text to an agent")
                .arg(required("target", "TARGET"))
                .arg(option("type", "TYPE"))
                .arg(required("text", "TEXT").num_args(1..).last(true)),
        )
        .subcommand(
            Command::new("send-keys")
                .about("Send key presses to an agent")
                .arg(required("target", "TARGET"))
                .arg(required("key", "KEY").num_args(1..))
                .after_help("Use esc as the canonical Escape key name; escape is also accepted."),
        )
        .subcommand(
            Command::new("rename")
                .about("Rename an agent")
                .arg(required("target", "TARGET"))
                .arg(Arg::new("name").value_name("NAME"))
                .arg(flag("clear")),
        )
        .subcommand(
            Command::new("prompt")
                .about("Submit to a ready named agent; optional wait observes later idle/done/blocked, with exit 3 on wait failure after submission")
                .override_usage("zynk agent prompt <NAME> [OPTIONS] -- <TEXT>")
                .arg(required("name", "NAME"))
                .arg(option("type", "TYPE"))
                .arg(option("trace", "ID|inherit"))
                .arg(flag("wait"))
                .arg(
                    option("until", "STATUS")
                        .action(ArgAction::Append)
                        .requires("wait")
                        .value_parser(["idle", "working", "blocked", "done", "unknown"])
                        .help("State to match after --wait; repeat for more than one state"),
                )
                .arg(option("timeout", "MS").requires("wait"))
                .arg(required("text", "TEXT").num_args(1..).last(true)),
        )
        .subcommand(id_command("focus", "target", "Focus an agent"))
        .subcommand(
            Command::new("wait")
                .about("Wait for a named agent to be idle, done or blocked; use agent start for launch readiness")
                .override_usage("zynk agent wait <NAME> [OPTIONS]")
                .arg(required("name", "NAME"))
                .arg(
                    option("until", "STATUS")
                        .action(ArgAction::Append)
                        .value_parser(["idle", "working", "blocked", "done", "unknown"])
                        .help("State to match; repeat for more than one state"),
                )
                .arg(option("timeout", "MS")),
        )
        .subcommand(
            Command::new("attach")
                .about("Attach directly to an agent terminal")
                .override_usage("zynk agent attach <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(flag("takeover")),
        )
        .subcommand(
            Command::new("start")
                .about("Launch in an existing pane and wait for interactive readiness; timeout keeps the label and does not prove the command did not run")
                .override_usage("zynk agent start <NAME> --kind <KIND> --pane <ID> [OPTIONS] [-- [AGENT_ARG]...]")
                .arg(required("name", "NAME"))
                .arg(option("kind", "KIND").required(true).value_parser(crate::detect::Agent::ALL.map(crate::detect::agent_label)))
                .arg(option("pane", "ID").required(true))
                .arg(option("timeout", "MS")),
        )
        .subcommand(
            Command::new("explain")
                .about("Explain agent detection state")
                .arg(Arg::new("target").value_name("TARGET"))
                .arg(path_option("file", "PATH"))
                .arg(option("agent", "LABEL"))
                .arg(json_flag())
                .arg(text_json_format_option())
                .arg(
                    Arg::new("verbose")
                        .short('v')
                        .long("verbose")
                        .action(ArgAction::SetTrue),
                ),
        )
}

fn pane_command() -> Command {
    Command::new("pane")
        .about("Control terminal panes")
        .subcommand(
            Command::new("list")
                .about("List panes")
                .arg(option("workspace", "WORKSPACE_ID")),
        )
        .subcommand(id_command("get", "pane_id", "Show a pane"))
        .subcommand(
            Command::new("layout")
                .about("Show pane layout information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("neighbor")
                .about("Find a pane neighbor")
                .arg(direction_option())
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("edges")
                .about("Show pane edge information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("focus")
                .about("Focus a neighboring pane")
                .arg(direction_option())
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("resize")
                .about("Resize a pane split")
                .arg(direction_option())
                .arg(option("amount", "FLOAT"))
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("zoom")
                .about("Toggle or set pane zoom")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(flag("toggle"))
                .arg(flag("on"))
                .arg(flag("off")),
        )
        .subcommand(
            Command::new("read")
                .about("Read pane terminal output")
                .arg(required("pane_id", "PANE_ID"))
                .arg(read_source_option(true))
                .arg(option("lines", "N"))
                .arg(text_ansi_format_option())
                .arg(flag("ansi"))
                .arg(flag("raw")),
        )
        .subcommand(
            Command::new("rename")
                .about("Rename a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(Arg::new("label").value_name("LABEL").num_args(1..))
                .arg(flag("clear")),
        )
        .subcommand(
            Command::new("input")
                .about("Set pane input routing")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(
                    option("right-click", "TARGET")
                        .value_parser(["zynk", "pane"])
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("split")
                .about("Split a pane")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(split_direction_option())
                .arg(option("ratio", "FLOAT"))
                .arg(path_option("cwd", "PATH"))
                .arg(option("right-click", "TARGET").value_parser(["zynk", "pane"]))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(
            Command::new("swap")
                .about("Swap panes")
                .arg(direction_option())
                .args(current_pane_args())
                .arg(option("source-pane", "ID"))
                .arg(option("target-pane", "ID")),
        )
        .subcommand(
            Command::new("move")
                .about("Move a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(option("tab", "TAB_ID"))
                .arg(option("split", "DIRECTION").value_parser(["right", "down"]))
                .arg(option("target-pane", "ID"))
                .arg(option("ratio", "FLOAT"))
                .arg(flag("new-tab"))
                .arg(option("workspace", "ID"))
                .arg(flag("new-workspace"))
                .arg(option("label", "TEXT"))
                .arg(option("tab-label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("close", "pane_id", "Close a pane"))
        .subcommand(
            Command::new("send-text")
                .about("Send literal text to a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(option("type", "TYPE"))
                .arg(option("trace", "ID|inherit"))
                .arg(required("text", "TEXT").num_args(1..).last(true)),
        )
        .subcommand(
            Command::new("send-keys")
                .about("Send key presses to a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(required("key", "KEY").num_args(1..)),
        )
        .subcommand(
            Command::new("run")
                .about("Run a command in a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(option("type", "TYPE"))
                .arg(option("trace", "ID|inherit"))
                .arg(required("command", "COMMAND").num_args(1..).last(true)),
        )
        .subcommand(report_agent_command())
        .subcommand(report_agent_session_command())
        .subcommand(release_agent_command())
        .subcommand(report_metadata_command())
}

fn report_agent_command() -> Command {
    Command::new("report-agent")
        .about("Report pane agent lifecycle state")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID"))
        .arg(option("agent", "LABEL"))
        .arg(pane_agent_state_option("state"))
        .arg(option("message", "TEXT"))
        .arg(option("seq", "N"))
        .arg(option("agent-session-id", "ID"))
        .arg(path_option("agent-session-path", "PATH"))
}

fn report_agent_session_command() -> Command {
    Command::new("report-agent-session")
        .about("Report pane agent session identity")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID"))
        .arg(option("agent", "LABEL"))
        .arg(option("seq", "N"))
        .arg(option("agent-session-id", "ID"))
        .arg(path_option("agent-session-path", "PATH"))
        .arg(option("session-start-source", "SOURCE"))
}

fn release_agent_command() -> Command {
    Command::new("release-agent")
        .about("Release pane agent lifecycle authority")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID"))
        .arg(option("agent", "LABEL"))
        .arg(option("seq", "N"))
}

fn report_metadata_command() -> Command {
    Command::new("report-metadata")
        .about("Report display-only pane metadata")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID"))
        .arg(option("agent", "LABEL"))
        .arg(option("applies-to-source", "ID"))
        .arg(option("title", "TEXT"))
        .arg(flag("clear-title"))
        .arg(option("display-agent", "TEXT"))
        .arg(flag("clear-display-agent"))
        .arg(option("state-label", "STATUS=TEXT"))
        .arg(flag("clear-state-labels"))
        .arg(repeatable_option("token", "NAME=VALUE"))
        .arg(repeatable_option("clear-token", "NAME"))
        .arg(option("seq", "N"))
        .arg(option("ttl-ms", "N"))
}

fn wait_command() -> Command {
    Command::new("wait")
        .about("Wait for pane output or agent state")
        .subcommand(
            Command::new("output")
                .about("Wait for matching pane output")
                .arg(required("pane_id", "PANE_ID"))
                .arg(option("match", "TEXT"))
                .arg(read_source_option(true))
                .arg(option("lines", "N"))
                .arg(option("timeout", "MS"))
                .arg(flag("regex"))
                .arg(flag("raw")),
        )
        .subcommand(
            Command::new("agent-status")
                .about("Wait for pane agent status")
                .arg(required("pane_id", "PANE_ID"))
                .arg(status_option("status", true))
                .arg(option("timeout", "MS")),
        )
}

fn terminal_command() -> Command {
    Command::new("terminal")
        .about("Attach to or observe raw terminal streams")
        .subcommand(
            Command::new("attach")
                .about("Attach directly to a terminal stream")
                .arg(required("terminal_id", "TERMINAL_ID"))
                .arg(flag("takeover")),
        )
        .subcommand(
            Command::new("session")
                .about("Work with terminal sessions")
                .subcommand(
                    Command::new("observe")
                        .about("Observe a terminal stream")
                        .arg(required("target", "TARGET"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                )
                .subcommand(
                    Command::new("control")
                        .about("Control a terminal stream with raw input")
                        .arg(required("target", "TARGET"))
                        .arg(flag("takeover"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                ),
        )
}

fn session_command() -> Command {
    Command::new("session")
        .about("Manage named persistent sessions")
        .subcommand(Command::new("list").about("List sessions").arg(json_flag()))
        .subcommand(
            Command::new("attach")
                .about("Attach to a session")
                .arg(required("name", "NAME")),
        )
        .subcommand(
            Command::new("stop")
                .about("Stop a session")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("delete")
                .about("Delete a stopped session")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
}

fn integration_command() -> Command {
    Command::new("integration")
        .about("Manage built-in agent integrations")
        .subcommand(
            Command::new("install")
                .about("Install an integration")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Uninstall an integration")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("status")
                .about("Show integration status")
                .arg(flag("outdated-only")),
        )
}

fn plugin_command() -> Command {
    Command::new("plugin")
        .about("Install and run workflow plugins")
        .subcommand(
            Command::new("install")
                .about("Install a plugin from GitHub")
                .arg(required("source", "OWNER/REPO[/SUBDIR]"))
                .arg(option("ref", "REF"))
                .arg(
                    Arg::new("yes")
                        .short('y')
                        .long("yes")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Uninstall a plugin")
                .arg(required("plugin", "PLUGIN")),
        )
        .subcommand(
            Command::new("link")
                .about("Link a local plugin")
                .arg(path_arg("path", "PATH"))
                .arg(flag("disabled"))
                .arg(flag("enabled")),
        )
        .subcommand(
            Command::new("unlink")
                .about("Unlink a local plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("enable")
                .about("Enable a plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("disable")
                .about("Disable a plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("list")
                .about("List installed plugins")
                .arg(option("plugin", "ID"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("config-dir")
                .about("Print a plugin config directory")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("action")
                .about("List or invoke plugin actions")
                .subcommand(
                    Command::new("list")
                        .about("List plugin actions")
                        .arg(option("plugin", "ID")),
                )
                .subcommand(
                    Command::new("invoke")
                        .about("Invoke a plugin action")
                        .arg(required("action_id", "ACTION_ID"))
                        .arg(option("plugin", "ID")),
                ),
        )
        .subcommand(
            Command::new("log")
                .about("Inspect plugin command logs")
                .visible_alias("logs")
                .arg(option("plugin", "ID"))
                .arg(option("limit", "N"))
                .subcommand(
                    Command::new("list")
                        .about("List plugin command logs")
                        .arg(option("plugin", "ID"))
                        .arg(option("limit", "N")),
                ),
        )
        .subcommand(
            Command::new("pane")
                .about("Manage plugin-owned panes")
                .subcommand(
                    Command::new("open")
                        .about("Open a plugin pane")
                        .arg(option("plugin", "ID"))
                        .arg(option("entrypoint", "ID"))
                        .arg(
                            option("placement", "PLACEMENT")
                                .value_parser(["overlay", "split", "tab", "zoomed"]),
                        )
                        .arg(option("workspace", "ID"))
                        .arg(option("target-pane", "PANE"))
                        .arg(split_direction_option())
                        .arg(path_option("cwd", "PATH"))
                        .arg(env_option())
                        .arg(flag("focus"))
                        .arg(flag("no-focus")),
                )
                .subcommand(
                    Command::new("focus")
                        .about("Focus a plugin pane")
                        .arg(required("pane_id", "PANE_ID")),
                )
                .subcommand(
                    Command::new("close")
                        .about("Close a plugin pane")
                        .arg(required("pane_id", "PANE_ID")),
                ),
        )
}

fn message_command(name: &'static str, about: &'static str) -> Command {
    Command::new(name)
        .about(about)
        .arg(required("target", "TARGET"))
        .arg(option("type", "TYPE"))
        .arg(option("trace", "ID|inherit"))
        .arg(required("text", "TEXT").num_args(1..).last(true))
}

fn query_command() -> Command {
    Command::new("query")
        .about("Search stored conversations")
        .arg(required("text", "TEXT").num_args(1..))
        .arg(option("workspace", "ID"))
        .arg(option("conversation", "ID"))
        .arg(option("agent", "LABEL"))
        .arg(option("since", "RFC3339"))
        .arg(option("type", "TYPE"))
        .arg(option("branch", "BRANCH"))
        .arg(path_option("cwd", "PATH"))
        .arg(option("trace", "ID"))
        .arg(option("limit", "N"))
        .arg(flag("exact"))
        .arg(json_flag())
}

fn zynk_command() -> Command {
    Command::new("zynk")
        .about("Native receipt and retrieval commands")
        .subcommand(query_command())
        .subcommand(
            Command::new("message-received")
                .about("Submit a hook receipt for server validation")
                .arg(option("pane-id", "ID"))
                .arg(option("message-id", "ID"))
                .arg(option("conversation-id", "ID"))
                .arg(option("conversation-seq", "N"))
                .arg(option("runtime-session-id", "ID"))
                .arg(path_option("socket-namespace", "PATH"))
                .arg(option("receiver-seq", "N"))
                .arg(option("status", "STATUS"))
                .arg(json_flag()),
        )
}

fn db_command() -> Command {
    Command::new("db")
        .about("Inspect or explicitly adopt the native conversation database")
        .subcommand(Command::new("status").about("Inspect database provenance"))
        .subcommand(Command::new("adopt").about("Back up a foreign database and start fresh"))
        .subcommand(Command::new("backup").about("Alias of database adoption"))
        .subcommand(
            Command::new("import")
                .about("Back up foreign data and start fresh, without importing content"),
        )
}

fn skill_command() -> Command {
    Command::new("skill")
        .about("Manage the native agent skill")
        .subcommand(
            Command::new("install")
                .about("Install for a supported agent")
                .arg(
                    Arg::new("target")
                        .value_name("AGENT")
                        .value_parser(["claude", "pi", "codex"]),
                )
                .arg(flag("all"))
                .arg(flag("force")),
        )
        .subcommand(
            Command::new("status")
                .about("Inspect skill status, including unsupported agents")
                .arg(
                    Arg::new("target").value_name("AGENT").value_parser(
                        crate::zynk::skill::SkillAgent::ALL.map(|agent| agent.label()),
                    ),
                )
                .arg(json_flag()),
        )
}

fn current_pane_args() -> [Arg; 2] {
    [option("pane", "ID"), flag("current")]
}

fn integration_target_arg() -> Arg {
    Arg::new("target")
        .value_name("TARGET")
        .required(true)
        .value_parser([
            "pi",
            "omp",
            "claude",
            "codex",
            "copilot",
            "devin",
            "droid",
            "kimi",
            "opencode",
            "kilo",
            "hermes",
            "qodercli",
            "qwen",
            "cursor",
            "mastracode",
            "antigravity-cli",
            "antigravity_cli",
            "grok",
        ])
}

fn id_command(name: &'static str, id: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).arg(required(id, id))
}

fn direction_option() -> Arg {
    option("direction", "DIRECTION").value_parser(["left", "right", "up", "down"])
}

fn split_direction_option() -> Arg {
    option("direction", "DIRECTION").value_parser(["right", "down"])
}

fn status_option(name: &'static str, required: bool) -> Arg {
    option(name, "STATUS")
        .required(required)
        .value_parser(["idle", "working", "blocked", "done", "unknown"])
}

fn pane_agent_state_option(name: &'static str) -> Arg {
    option(name, "STATUS")
        .required(true)
        .value_parser(["idle", "working", "blocked", "unknown"])
}

fn read_source_option(include_detection: bool) -> Arg {
    let values = if include_detection {
        vec!["visible", "recent", "recent-unwrapped", "detection"]
    } else {
        vec!["visible", "recent", "recent-unwrapped"]
    };
    option("source", "SOURCE").value_parser(values)
}

fn text_ansi_format_option() -> Arg {
    option("format", "FORMAT").value_parser(["text", "ansi"])
}

fn text_json_format_option() -> Arg {
    option("format", "FORMAT").value_parser(["text", "json"])
}

fn json_flag() -> Arg {
    flag("json")
}

fn help_flag() -> Arg {
    Arg::new("help")
        .short('h')
        .long("help")
        .action(ArgAction::SetTrue)
        .help("Show help")
}

fn env_option() -> Arg {
    option("env", "KEY=VALUE")
        .action(ArgAction::Append)
        .help("Set an environment variable for the launched process")
}

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}

fn option(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .value_name(value_name)
        .action(ArgAction::Set)
}

fn repeatable_option(name: &'static str, value_name: &'static str) -> Arg {
    option(name, value_name).action(ArgAction::Append)
}

fn path_option(name: &'static str, value_name: &'static str) -> Arg {
    option(name, value_name).value_hint(ValueHint::AnyPath)
}

fn required(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name).value_name(value_name).required(true)
}

fn path_arg(name: &'static str, value_name: &'static str) -> Arg {
    required(name, value_name).value_hint(ValueHint::AnyPath)
}

#[cfg(test)]
mod tests {
    use clap::Command;

    fn sorted<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        let mut items = items.into_iter().collect::<Vec<_>>();
        items.sort_unstable();
        items
    }

    #[test]
    fn m828b_pane_token_completion_preserves_legacy_flags() {
        let mut cmd = super::command();
        cmd.build();
        let report = command_path(&cmd, &["pane", "report-metadata"]);
        assert_eq!(
            sorted(report.get_arguments().filter_map(|arg| arg.get_long())),
            [
                "agent",
                "applies-to-source",
                "clear-display-agent",
                "clear-state-labels",
                "clear-title",
                "clear-token",
                "display-agent",
                "seq",
                "source",
                "state-label",
                "title",
                "token",
                "ttl-ms"
            ]
        );
        let positional = report.get_positionals().collect::<Vec<_>>();
        for name in ["custom-status", "clear-custom-status"] {
            assert!(!report
                .get_arguments()
                .any(|arg| arg.get_long() == Some(name)));
        }
        assert_eq!(positional.len(), 1);
        assert!(positional[0].is_required_set());
        for name in ["token", "clear-token"] {
            let arg = report
                .get_arguments()
                .find(|arg| arg.get_long() == Some(name))
                .unwrap();
            assert!(
                matches!(arg.get_action(), clap::ArgAction::Append),
                "{name}"
            );
        }
    }

    #[test]
    fn m828a_workspace_metadata_completion_matches_manual_flags() {
        let mut cmd = super::command();
        cmd.build();
        let workspace = command_path(&cmd, &["workspace"]);
        let report = workspace.find_subcommand("report-metadata");
        assert!(
            report.is_some(),
            "workspace report-metadata completion is absent"
        );
        let report = report.unwrap();
        assert_eq!(
            sorted(report.get_arguments().filter_map(|arg| arg.get_long())),
            ["clear-token", "seq", "source", "token", "ttl-ms"]
        );
        let positional = report.get_positionals().collect::<Vec<_>>();
        assert_eq!(positional.len(), 1);
        assert!(positional[0].is_required_set());
        for name in ["token", "clear-token"] {
            let arg = report
                .get_arguments()
                .find(|arg| arg.get_long() == Some(name))
                .unwrap();
            assert!(
                matches!(arg.get_action(), clap::ArgAction::Append),
                "{name}"
            );
        }
    }

    #[test]
    fn spec_matches_manual_command_population() {
        let mut cmd = super::command();
        cmd.build();
        for (path, children) in [
            ("", "api server status config channel workspace worktree tab notification agent terminal pane wait integration skill plugin session zynk db send reply thread inbox trace whoami who query update completion"),
            ("api", "schema snapshot"),
            ("server", "stop reload-config agent-manifests update-agent-manifests reload-agent-manifests"),
            ("status", "server client"),
            ("config", "check reset-keys"),
            ("channel", "show set"),
            ("workspace", "list create get focus rename report-metadata close"),
            ("worktree", "list create open remove"),
            ("tab", "list create get focus rename close"),
            ("notification", "show"),
            ("agent", "list get read send send-keys prompt rename focus wait attach start explain"),
            ("pane", "list get layout neighbor edges focus resize zoom read input rename split swap move close send-text send-keys report-agent report-agent-session release-agent report-metadata run"),
            ("wait", "output agent-status"),
            ("terminal", "attach session"),
            ("terminal session", "observe control"),
            ("session", "list attach stop delete"),
            ("integration", "install uninstall status"),
            ("skill", "install status"),
            ("plugin", "install uninstall link list config-dir unlink enable disable action log pane"),
            ("plugin action", "list invoke"),
            ("plugin log", "list"),
            ("plugin pane", "open focus close"),
            ("zynk", "message-received query"),
            ("db", "status adopt backup import"),
        ] {
            let path_parts = path.split_whitespace().collect::<Vec<_>>();
            let node = command_path(&cmd, &path_parts);
            assert_eq!(
                sorted(node.get_subcommands().map(Command::get_name)),
                sorted(children.split_whitespace()),
                "command population at {path}"
            );
        }
        assert_eq!(
            command_path(&cmd, &["plugin", "log"])
                .get_all_aliases()
                .collect::<Vec<_>>(),
            ["logs"]
        );
    }

    #[test]
    fn spec_applies_env_only_to_existing_plugin_surface() {
        let cmd = super::command();
        for path in [
            ["workspace", "create"],
            ["tab", "create"],
            ["agent", "start"],
            ["pane", "split"],
        ] {
            assert!(
                !has_option(command_path(&cmd, &path), "env"),
                "future --env at {path:?}"
            );
        }
        assert!(has_option(
            command_path(&cmd, &["plugin", "pane", "open"]),
            "env"
        ));
    }

    #[test]
    fn spec_preserves_fork_flags_and_contextual_values() {
        let cmd = super::command();
        for (path, flags) in [
            ("send", "type trace"), ("reply", "type trace"),
            ("pane send-text", "type trace"), ("pane run", "type trace"),
            ("agent send", "type"),
            ("agent prompt", "type trace wait until timeout"),
            ("agent wait", "until timeout"),
            ("thread", "json"), ("trace", "json"), ("whoami", "json"), ("who", "json"),
            ("inbox", "agent limit json"),
            ("query", "workspace conversation agent since type branch cwd trace limit exact json"),
            ("zynk query", "workspace conversation agent since type branch cwd trace limit exact json"),
            ("zynk message-received", "pane-id message-id conversation-id conversation-seq runtime-session-id socket-namespace receiver-seq status json"),
            ("skill install", "all force"), ("skill status", "json"),
            ("terminal session observe", "cols rows"),
            ("terminal session control", "takeover cols rows"),
            ("plugin log", "plugin limit"), ("plugin log list", "plugin limit"),
            ("plugin pane open", "plugin entrypoint placement workspace target-pane direction cwd env focus no-focus"),
            ("db status", ""), ("db adopt", ""), ("db backup", ""), ("db import", ""),
        ] {
            let path_parts = path.split_whitespace().collect::<Vec<_>>();
            let node = command_path(&cmd, &path_parts);
            assert_eq!(
                sorted(node.get_arguments().filter_map(|arg| arg.get_long())),
                sorted(flags.split_whitespace()),
                "flags at {path}"
            );
        }
        for (path, option, values) in [
            ("pane report-agent", "state", "idle working blocked unknown"),
            (
                "wait agent-status",
                "status",
                "idle working blocked done unknown",
            ),
            (
                "wait output",
                "source",
                "visible recent recent-unwrapped detection",
            ),
            (
                "pane read",
                "source",
                "visible recent recent-unwrapped detection",
            ),
            (
                "agent read",
                "source",
                "visible recent recent-unwrapped detection",
            ),
            ("agent prompt", "until", "idle working blocked done unknown"),
            ("agent wait", "until", "idle working blocked done unknown"),
            ("plugin pane open", "placement", "overlay split tab zoomed"),
        ] {
            assert_eq!(
                option_values(
                    command_path(&cmd, &path.split_whitespace().collect::<Vec<_>>()),
                    option
                ),
                values.split_whitespace().collect::<Vec<_>>(),
                "values at {path} --{option}"
            );
        }
        for (path, values) in [
            ("integration install", "pi omp claude codex copilot devin droid kimi opencode kilo hermes qodercli qwen cursor mastracode antigravity-cli antigravity_cli grok"),
            ("integration uninstall", "pi omp claude codex copilot devin droid kimi opencode kilo hermes qodercli qwen cursor mastracode antigravity-cli antigravity_cli grok"),
            ("skill install", "claude pi codex"),
            ("skill status", "claude pi codex omp copilot devin droid kimi opencode kilo hermes qodercli cursor"),
        ] {
            let parts = path.split_whitespace().collect::<Vec<_>>();
            let target = command_path(&cmd, &parts).get_arguments().find(|arg| arg.get_id() == "target").unwrap();
            let actual = target.get_value_parser().possible_values().unwrap()
                .map(|value| value.get_name().to_string()).collect::<Vec<_>>();
            assert_eq!(actual, values.split_whitespace().collect::<Vec<_>>(), "{path}");
        }
    }

    fn command_path<'a>(cmd: &'a Command, path: &[&str]) -> &'a Command {
        let mut current = cmd;
        for name in path {
            current = current
                .get_subcommands()
                .find(|subcommand| subcommand.get_name() == *name)
                .unwrap_or_else(|| panic!("missing command path segment {name}"));
        }
        current
    }

    fn option_values(cmd: &Command, option: &str) -> Vec<String> {
        let arg = cmd
            .get_arguments()
            .find(|arg| arg.get_long() == Some(option))
            .unwrap_or_else(|| panic!("missing --{option}"));
        arg.get_value_parser()
            .possible_values()
            .into_iter()
            .flatten()
            .map(|value| value.get_name().to_string())
            .collect()
    }

    fn has_option(cmd: &Command, option: &str) -> bool {
        cmd.get_arguments()
            .any(|arg| arg.get_long() == Some(option))
    }

    fn assert_command_descriptions(cmd: &Command, path: &mut Vec<String>) {
        if !path.is_empty() {
            assert!(
                cmd.get_about().is_some(),
                "missing completion description for {}",
                path.join(" ")
            );
        }
        for subcommand in cmd.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            assert_command_descriptions(subcommand, path);
            path.pop();
        }
    }

    #[test]
    fn spec_describes_all_completion_commands() {
        let cmd = super::command();
        assert_command_descriptions(&cmd, &mut Vec::new());
    }

    #[test]
    fn spec_includes_completion_alias_and_shells() {
        let cmd = super::command();
        let completion = command_path(&cmd, &["completion"]);
        assert!(completion
            .get_all_aliases()
            .any(|alias| alias == "completions"));
        let shells = completion
            .get_arguments()
            .find(|arg| arg.get_id() == "shell")
            .unwrap()
            .get_value_parser()
            .possible_values()
            .unwrap()
            .map(|value| value.get_name().to_string())
            .collect::<Vec<_>>();
        assert!(shells.contains(&"zsh".to_string()));
        assert!(shells.contains(&"fish".to_string()));
    }

    #[test]
    fn spec_includes_nested_plugin_pane_open_options() {
        let cmd = super::command();
        let open = command_path(&cmd, &["plugin", "pane", "open"]);
        assert!(open
            .get_arguments()
            .any(|arg| arg.get_long() == Some("entrypoint")));
        assert!(option_values(open, "placement").contains(&"zoomed".to_string()));
    }

    #[test]
    fn m839c_spec_wait_has_no_status_and_names_completion_states() {
        let cmd = super::command();
        let wait = command_path(&cmd, &["agent", "wait"]);
        assert!(!has_option(wait, "status"));
        assert!(has_option(wait, "timeout"));
        assert_eq!(
            option_values(wait, "until"),
            ["idle", "working", "blocked", "done", "unknown"]
        );
        let help = wait
            .get_about()
            .expect("agent wait completion description")
            .to_string();
        for state in ["idle", "done", "blocked"] {
            assert!(
                help.contains(state),
                "completion state {state} absent: {help}"
            );
        }
    }

    #[test]
    fn m875_agent_usages_put_the_target_before_options() {
        let cmd = super::command();
        assert!(has_option(&cmd, "skill"));
        for (name, target) in [
            ("read", "<TARGET>"),
            ("prompt", "<NAME>"),
            ("wait", "<NAME>"),
            ("attach", "<TARGET>"),
            ("start", "<NAME>"),
        ] {
            let command = command_path(&cmd, &["agent", name]);
            let usage = command
                .get_overridden_usage()
                .expect("agent command override usage")
                .to_string();
            assert!(
                usage.find(target) < usage.find("[OPTIONS]"),
                "target must precede options: {usage}"
            );
        }
    }

    #[test]
    fn spec_includes_pane_read_raw_flag() {
        let cmd = super::command();
        let pane_read = command_path(&cmd, &["pane", "read"]);
        assert!(has_option(pane_read, "raw"));
    }

    #[test]
    fn worktree_json_compatibility_flag_stays_out_of_public_spec() {
        let cmd = super::command();
        for subcommand in ["list", "create", "open", "remove"] {
            let worktree_command = command_path(&cmd, &["worktree", subcommand]);
            assert!(
                !has_option(worktree_command, "json"),
                "zynk worktree {subcommand} should not advertise --json"
            );
        }
    }

    #[test]
    fn spec_matches_pane_split_direction_flag() {
        let cmd = super::command();
        let pane_split = command_path(&cmd, &["pane", "split"]);
        assert!(has_option(pane_split, "direction"));
        assert!(!has_option(pane_split, "split"));
        assert_eq!(option_values(pane_split, "direction"), ["right", "down"]);
    }

    #[test]
    fn spec_does_not_complete_agent_start_argv_without_separator() {
        let cmd = super::command();
        let agent_start = command_path(&cmd, &["agent", "start"]);
        assert!(!agent_start
            .get_arguments()
            .any(|arg| arg.get_id() == "argv"));
    }

    #[test]
    fn zsh_completion_contains_public_commands_and_values() {
        let mut cmd = super::command();
        let mut output = Vec::new();
        clap_complete::generate(clap_complete::Shell::Zsh, &mut cmd, "zynk", &mut output);
        let script = String::from_utf8(output).unwrap();
        assert!(script.contains("#compdef zynk"));
        assert!(script.contains("--help"));
        assert!(script.contains("'completion:Generate shell completion scripts'"));
        assert!(script.contains("bash elvish fish powershell zsh"));
        assert!(script.contains("'pane:Control terminal panes'"));
        assert!(script.contains("idle working blocked done unknown"));
        assert!(!script.contains("live-handoff"));
    }
}
