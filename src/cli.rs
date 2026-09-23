// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::api::client::{ApiClient, ApiClientError};
use crate::api::schema::{
    AgentStatus, EmptyParams, ErrorResponse, EventData, EventKind, EventMatch, EventsWaitParams,
    Method, OutputMatch, PaneAgentState, PaneWaitForOutputParams, ReadFormat, ReadSource, Request,
    ResponseResult, SplitDirection, SubscriptionEventData, SubscriptionEventEnvelope,
    SubscriptionEventKind,
};

mod agent;
mod api;
mod completion;
mod integration;
mod native;
mod notification;
mod pane;
mod plugin;
mod protocol_guard;
mod runtime;
mod server;
mod server_not_running;
mod skill;
mod spec;
mod status;
mod tab;
mod workspace;
mod worktree;

const TERMINAL_SESSION_OBSERVE_USAGE: &str =
    "usage: zynk terminal session observe <target> [--cols N] [--rows N]";
const TERMINAL_SESSION_CONTROL_USAGE: &str =
    "usage: zynk terminal session control <target> [--takeover] [--cols N] [--rows N]";
mod zynk;

pub(crate) const AGENT_HELP_FOOTER: &str = concat!(
    "Are you an AI? Use these resources only when the task calls for them:\n",
    "  Control Zynk panes, agents, workspaces, or messages:\n",
    "    Skip this if the zynk skill is already in context; otherwise run `zynk --skill`.\n",
    "  Confirm this build's exact command surface with `zynk <command> --help`.\n",
    "  Before release preparation, use the repository's `zynk-pre-release-audit` skill."
);

pub enum CommandOutcome {
    Handled(i32),
    NotCli,
}

pub(crate) fn parse_token_assignment(raw: &str) -> Result<(String, Option<String>), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err("token must use NAME=VALUE".into());
    };
    if key.is_empty() {
        return Err("token name must not be empty".into());
    }
    Ok((key.to_string(), Some(value.to_string())))
}

/// Parse a `KEY=VALUE` env assignment from CLI args (used by `zynk plugin pane open --env`).
pub(crate) fn parse_env_assignment(raw: &str) -> Result<(String, String), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err("env must use KEY=VALUE".into());
    };
    if key.is_empty() {
        return Err("env key must not be empty".into());
    }
    if key.contains('\0') || value.contains('\0') {
        return Err("env must not contain NUL bytes".into());
    }
    Ok((key.to_string(), value.to_string()))
}

/// A help FLAG (never the bare word `help`, which can be a legitimate value).
pub(crate) fn is_help_flag(arg: &str) -> bool {
    matches!(arg, "--help" | "-h")
}

/// True only for the exact leaf-help position `<group> <leaf> --help` — i.e. the
/// dispatcher's args are exactly `[leaf, "--help"|"-h"]`. This is delimiter/position
/// safe: it never fires for a help flag that is a value or a `--`-delimited payload
/// (`pane run w1:p1 -- --help`, `workspace create --label --help`, etc.), because
/// those have more than two args.
pub(crate) fn leaf_help_requested(args: &[String]) -> bool {
    args.len() == 2 && is_help_flag(&args[1])
}

pub fn maybe_run(args: &[String]) -> std::io::Result<CommandOutcome> {
    let Some(command) = args.get(1).map(|arg| arg.as_str()) else {
        return Ok(CommandOutcome::NotCli);
    };

    let exit_code = match command {
        "completion" | "completions" => completion::run_completion_command(&args[2..])?,
        "api" => api::run_api_command(&args[2..])?,
        "server" => {
            let Some(exit_code) = server::run_server_command(&args[2..])? else {
                return Ok(CommandOutcome::NotCli);
            };
            exit_code
        }
        "status" => status::run_status_command(&args[2..])?,
        "config" => run_config_command(&args[2..])?,
        "channel" => run_channel_command(&args[2..])?,
        "workspace" => workspace::run_workspace_command(&args[2..])?,
        "worktree" => worktree::run_worktree_command(&args[2..])?,
        "tab" => tab::run_tab_command(&args[2..])?,
        "notification" => notification::run_notification_command(&args[2..])?,
        "agent" => agent::run_agent_command(&args[2..])?,
        "terminal" => run_terminal_command(&args[2..])?,
        "pane" => pane::run_pane_command(&args[2..])?,
        "popup" => run_popup_command(&args[2..])?,
        "wait" => run_wait_command(&args[2..])?,
        "integration" => integration::run_integration_command(&args[2..])?,
        "skill" => skill::run_skill_command(&args[2..])?,
        "plugin" => plugin::run_plugin_command(&args[2..])?,
        "session" => run_session_command(&args[2..])?,
        "zynk" => zynk::run_zynk_command(&args[2..])?,
        // zynk fork (M6 / ADR 0008): native DB cutover surface (`zynk db …`). Returns
        // its own i32 exit code directly (no socket round-trip).
        "db" => {
            return Ok(CommandOutcome::Handled(
                crate::zynk::db_cutover::run_db_command_code(&args[2..]),
            ))
        }
        // zynk fork (M6 / ADR 0007 §2): native top-level command surface.
        "send" => native::run_send_command(&args[2..])?,
        "reply" => native::run_reply_command(&args[2..])?,
        "thread" => native::run_thread_command(&args[2..])?,
        "inbox" => native::run_inbox_command(&args[2..])?,
        "trace" => native::run_trace_command(&args[2..])?,
        "whoami" => native::run_whoami_command(&args[2..])?,
        "who" => native::run_who_command(&args[2..])?,
        "query" => native::run_query_command(&args[2..])?,
        _ => return Ok(CommandOutcome::NotCli),
    };

    if exit_code == 0
        && args.len() == 3
        && is_help_flag(&args[2])
        && matches!(
            command,
            "api"
                | "server"
                | "status"
                | "config"
                | "channel"
                | "workspace"
                | "worktree"
                | "tab"
                | "notification"
                | "agent"
                | "terminal"
                | "pane"
                | "popup"
                | "wait"
                | "integration"
                | "skill"
                | "plugin"
                | "session"
                | "zynk"
                | "db"
        )
    {
        eprintln!();
        eprintln!("{AGENT_HELP_FOOTER}");
    }

    Ok(CommandOutcome::Handled(exit_code))
}

fn run_popup_command(args: &[String]) -> std::io::Result<i32> {
    match args {
        [command] if command == "close" => {
            send_ok_request(Method::PopupClose(EmptyParams::default()))
        }
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            eprintln!("usage: zynk popup close");
            Ok(0)
        }
        [command, flag] if command == "close" && is_help_flag(flag) => {
            eprintln!("usage: zynk popup close");
            Ok(0)
        }
        _ => {
            eprintln!("usage: zynk popup close");
            Ok(2)
        }
    }
}

fn run_channel_command(args: &[String]) -> std::io::Result<i32> {
    if leaf_help_requested(args) {
        print_channel_help();
        return Ok(0);
    }
    match args.first().map(|arg| arg.as_str()) {
        Some("set") => channel_set(&args[1..]),
        Some("show") if args.len() == 1 => {
            let config = crate::config::Config::load().config;
            println!("{}", config.update.channel.as_str());
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            print_channel_help();
            Ok(0)
        }
        _ => {
            print_channel_help();
            Ok(2)
        }
    }
}

fn channel_set(args: &[String]) -> std::io::Result<i32> {
    let Some(channel) = parse_channel_set_arg(args) else {
        eprintln!("usage: zynk channel set <stable|preview>");
        return Ok(2);
    };

    // ADR 0007/0013: update channels select a release manifest, and zynk is source-only — there are
    // no releases to select between. Fail closed (refuse, no config write).
    if !crate::update::release_infra_open() {
        eprintln!(
            "update channels are unavailable: zynk is built from source only, so there are no release channels to choose between. Rebuild from the source you want — cargo install zynk --locked, or cargo install --path . --locked from a reviewed checkout."
        );
        return Ok(1);
    }

    if let Some(reason) = channel_set_rejection(
        channel,
        crate::update::preview_channel_rejection_for_current_install(),
    ) {
        eprintln!("{reason}.");
        return Ok(1);
    }

    let path = crate::config::config_path();
    let content = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if let Err(err) = content.parse::<toml::Value>() {
        eprintln!(
            "config file at {} is invalid TOML: {err}. Fix it before changing the update channel.",
            path.display()
        );
        return Ok(1);
    }

    let updated = crate::config::upsert_section_value(
        &content,
        "update",
        "channel",
        &format!("\"{channel}\""),
    );
    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "changing the update channel would make {} invalid TOML: {err}; leaving config unchanged",
            path.display()
        );
        return Ok(1);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)?;
    println!(
        "Zynk update channel set to {channel} in {}.",
        path.display()
    );

    match channel_set_install_action(
        crate::update::package_manager_channel_update_guidance_for_current_install(),
    ) {
        ChannelSetInstallAction::PrintGuidance(guidance) => {
            println!("{guidance}");
            return Ok(0);
        }
        ChannelSetInstallAction::RunSelfUpdate => {}
    }

    if let Err(err) = crate::update::self_update(crate::update::SelfUpdateOptions::default()) {
        eprintln!("update failed: {err}");
        eprintln!("Run `zynk update` to retry.");
        return Ok(1);
    }

    Ok(0)
}

fn parse_channel_set_arg(args: &[String]) -> Option<&str> {
    let channel = args.first().map(|arg| arg.as_str())?;
    if args.len() == 1 && matches!(channel, "stable" | "preview") {
        Some(channel)
    } else {
        None
    }
}

fn channel_set_rejection(
    channel: &str,
    install_rejection: Option<&'static str>,
) -> Option<&'static str> {
    if channel == "preview" {
        return install_rejection;
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelSetInstallAction {
    RunSelfUpdate,
    PrintGuidance(&'static str),
}

fn channel_set_install_action(
    package_manager_guidance: Option<&'static str>,
) -> ChannelSetInstallAction {
    match package_manager_guidance {
        Some(guidance) => ChannelSetInstallAction::PrintGuidance(guidance),
        None => ChannelSetInstallAction::RunSelfUpdate,
    }
}

fn print_channel_help() {
    eprintln!("zynk channel commands:");
    eprintln!("  zynk channel show                  print the configured update channel");
    eprintln!("  zynk channel set <stable|preview>  choose the update channel");
}

fn run_config_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_config_help();
        return Ok(2);
    };

    if leaf_help_requested(args) {
        print_config_help();
        return Ok(0);
    }

    match subcommand {
        "check" => config_check(&args[1..]),
        "reset-keys" => config_reset_keys(&args[1..]),
        "help" | "--help" | "-h" => {
            print_config_help();
            Ok(0)
        }
        _ => {
            print_config_help();
            Ok(2)
        }
    }
}

fn config_check(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => {}
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            eprintln!("usage: zynk config check");
            return Ok(0);
        }
        _ => {
            eprintln!("usage: zynk config check");
            return Ok(2);
        }
    }

    let diagnostics = crate::config::Config::load().diagnostics;
    if diagnostics.is_empty() {
        println!("config: ok");
    } else {
        println!("config: issues found");
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
    }

    Ok(i32::from(!diagnostics.is_empty()))
}

fn config_reset_keys(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: zynk config reset-keys");
        return Ok(2);
    }

    let path = crate::config::config_path();
    if !path.exists() {
        println!(
            "No config file found at {}. Built-in v2 keybindings already apply.",
            path.display()
        );
        return Ok(0);
    }

    let content = std::fs::read_to_string(&path)?;
    let parsed = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "config file at {} is invalid TOML: {err}. Fix it manually or move it aside to use defaults.",
                path.display()
            );
            return Ok(1);
        }
    };
    let Some(table) = parsed.as_table() else {
        eprintln!(
            "config file at {} is invalid TOML: top-level config must be a table.",
            path.display()
        );
        return Ok(1);
    };

    if !table.contains_key("keys") {
        println!(
            "No [keys] config found in {}. Built-in v2 keybindings already apply.",
            path.display()
        );
        return Ok(0);
    }

    let (updated, removed) = crate::config::remove_keybinding_config_sections(&content);
    if !removed {
        eprintln!(
            "could not safely remove keybinding config from {} without rewriting comments; edit the file manually or remove the top-level keys setting.",
            path.display()
        );
        return Ok(1);
    }
    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "removing keybinding config would make {} invalid TOML: {err}; leaving config unchanged",
            path.display()
        );
        return Ok(1);
    }

    let backup_path = key_config_backup_path(&path);
    std::fs::copy(&path, &backup_path)?;
    std::fs::write(&path, updated)?;

    println!("Created backup: {}", backup_path.display());
    println!(
        "Removed [keys], [keys.indexed], and [[keys.command]] from {}.",
        path.display()
    );
    println!("Built-in v2 keybindings will apply after Zynk restarts or reloads config.");
    println!("If a Zynk server is running, run `zynk server reload-config` to apply this now.");
    println!(
        "To restore: cp {} {}",
        backup_path.display(),
        path.display()
    );
    Ok(0)
}

fn key_config_backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    path.with_file_name(format!("{file_name}.bak-keybind-v2-{timestamp}"))
}

fn run_terminal_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_terminal_help();
        return Ok(2);
    };

    if leaf_help_requested(args) {
        print_terminal_help();
        return Ok(0);
    }

    match subcommand {
        "attach" => terminal_attach(&args[1..]),
        "session" => terminal_session(&args[1..]),
        "help" | "--help" | "-h" => {
            print_terminal_help();
            Ok(0)
        }
        _ => {
            print_terminal_help();
            Ok(2)
        }
    }
}

fn run_wait_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_wait_help();
        return Ok(2);
    };

    if leaf_help_requested(args) {
        print_wait_help();
        return Ok(0);
    }

    match subcommand {
        "output" => wait_output(&args[1..]),
        "agent-status" => wait_agent_status(&args[1..]),
        "help" | "--help" | "-h" => {
            print_wait_help();
            Ok(0)
        }
        _ => {
            print_wait_help();
            Ok(2)
        }
    }
}

fn run_session_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_session_help();
        return Ok(2);
    };

    if leaf_help_requested(args) {
        print_session_help();
        return Ok(0);
    }

    match subcommand {
        "list" => session_list(&args[1..]),
        "attach" => session_attach_help(&args[1..]),
        "stop" => session_stop(&args[1..]),
        "delete" => session_delete(&args[1..]),
        "help" | "--help" | "-h" => {
            print_session_help();
            Ok(0)
        }
        _ => {
            print_session_help();
            Ok(2)
        }
    }
}

fn session_attach_help(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("usage: zynk session attach <name>");
        return Ok(0);
    }
    eprintln!("usage: zynk session attach <name>");
    Ok(2)
}

fn session_list(args: &[String]) -> std::io::Result<i32> {
    let json = match parse_session_json_only(args, "usage: zynk session list [--json]") {
        Ok(json) => json,
        Err(code) => return Ok(code),
    };

    let sessions = crate::session::list_sessions()?;
    if json {
        _print_json(&serde_json::json!({
            "sessions": sessions,
        }));
    } else {
        print_session_table(&sessions);
    }
    Ok(0)
}

fn session_stop(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: zynk session stop <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let target = match crate::session::parse_target_name(&name) {
        Ok(target) => target,
        Err(message) => {
            print_session_error("invalid_session_name", &message);
            return Ok(1);
        }
    };
    match crate::session::stop_session(target.as_deref()) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "stopped": true,
                    "session": session,
                }));
            } else {
                println!("stopped session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_stop_failed", &message);
            Ok(1)
        }
    }
}

fn session_delete(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: zynk session delete <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    match crate::session::delete_session(&name) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "deleted": true,
                    "session": session,
                }));
            } else {
                println!("deleted session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_delete_failed", &message);
            Ok(1)
        }
    }
}

fn terminal_attach(args: &[String]) -> std::io::Result<i32> {
    let (terminal_id, takeover) = match parse_attach_target(
        args,
        "usage: zynk terminal attach <terminal_id> [--takeover]",
    ) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(code),
    };
    crate::client::run_terminal_attach(terminal_id, takeover)?;
    Ok(0)
}

fn terminal_session(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("control") => terminal_session_control(&args[1..]),
        Some("observe") => terminal_session_observe(&args[1..]),
        Some("help" | "--help" | "-h") => {
            eprintln!("{TERMINAL_SESSION_CONTROL_USAGE}");
            eprintln!("{TERMINAL_SESSION_OBSERVE_USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{TERMINAL_SESSION_CONTROL_USAGE}");
            eprintln!("{TERMINAL_SESSION_OBSERVE_USAGE}");
            Ok(2)
        }
    }
}

fn terminal_session_control(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_terminal_session_options(
        args,
        TERMINAL_SESSION_CONTROL_USAGE,
        "control",
        true,
    )? {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };

    crate::client::run_terminal_session_control(
        options.target,
        options.takeover,
        options.cols,
        options.rows,
    )?;
    Ok(0)
}

fn terminal_session_observe(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_terminal_session_options(
        args,
        TERMINAL_SESSION_OBSERVE_USAGE,
        "observe",
        false,
    )? {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };

    crate::client::run_terminal_session_observe(options.target, options.cols, options.rows)?;
    Ok(0)
}

struct TerminalSessionOptions {
    target: String,
    cols: u16,
    rows: u16,
    takeover: bool,
}

fn parse_terminal_session_options(
    args: &[String],
    usage: &str,
    command: &str,
    allow_takeover: bool,
) -> std::io::Result<Result<TerminalSessionOptions, i32>> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{usage}");
        return Ok(Err(0));
    }
    let Some(target) = args.first() else {
        eprintln!("{usage}");
        return Ok(Err(2));
    };

    let mut cols = 120;
    let mut rows = 40;
    let mut takeover = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--takeover" if allow_takeover => {
                takeover = true;
                index += 1;
            }
            "--cols" | "--rows" => {
                let flag = args[index].as_str();
                let Some(value) = args.get(index + 1) else {
                    eprintln!("{usage}");
                    return Ok(Err(2));
                };
                let dimension = parse_terminal_dimension(value, flag)?;
                if flag == "--cols" {
                    cols = dimension;
                } else {
                    rows = dimension;
                }
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("{usage}");
                return Ok(Err(0));
            }
            other => {
                eprintln!("unknown terminal session {command} option: {other}");
                eprintln!("{usage}");
                return Ok(Err(2));
            }
        }
    }

    Ok(Ok(TerminalSessionOptions {
        target: target.clone(),
        cols,
        rows,
        takeover,
    }))
}

fn parse_terminal_dimension(raw: &str, flag: &str) -> std::io::Result<u16> {
    let parsed = raw.parse::<u16>().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{flag} must be an integer between 1 and {}", u16::MAX),
        )
    })?;
    if parsed == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{flag} must be greater than 0"),
        ));
    }
    Ok(parsed)
}

pub(super) fn parse_attach_target(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let Some(target) = args.first() else {
        eprintln!("{usage}");
        return Err(2);
    };
    let mut takeover = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--takeover" => takeover = true,
            "help" | "--help" | "-h" => {
                eprintln!("{usage}");
                return Err(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Err(2);
            }
        }
    }
    Ok((target.clone(), takeover))
}

fn wait_output(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk wait output <pane_id> --match <text> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--timeout MS] [--regex]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut timeout_ms = None;
    let mut strip_ansi = true;
    let mut regex = false;
    let mut match_value = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--match" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --match");
                    return Ok(2);
                };
                match_value = Some(value.clone());
                index += 2;
            }
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            "--regex" => {
                regex = true;
                index += 1;
            }
            "--raw" => {
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(match_value) = match_value else {
        eprintln!("missing required --match");
        return Ok(2);
    };

    let matcher = if regex {
        OutputMatch::Regex { value: match_value }
    } else {
        OutputMatch::Substring { value: match_value }
    };

    let response = send_request(&Request {
        id: "cli:wait:output".into(),
        method: Method::PaneWaitForOutput(PaneWaitForOutputParams {
            pane_id,
            source,
            lines,
            r#match: matcher,
            timeout_ms,
            strip_ansi,
        }),
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(&response).unwrap());
    Ok(0)
}

fn wait_agent_status(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk wait agent-status <pane_id> --status <idle|working|blocked|done|unknown> [--timeout MS]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut timeout_ms = None;
    let mut desired_status = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                desired_status = Some(parse_agent_status(value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(agent_status) = desired_status else {
        eprintln!("missing required --status");
        return Ok(2);
    };

    let request = Request {
        id: "cli:wait:agent-status".into(),
        method: Method::EventsWait(EventsWaitParams {
            match_event: EventMatch::PaneAgentStatusChanged {
                pane_id,
                agent_status,
            },
            timeout_ms,
        }),
    };
    let response = send_request(&request)?;
    match crate::api::client::parse_response_value(response) {
        Ok(success) => {
            let ResponseResult::WaitMatched { event } = success.result else {
                return Err(std::io::Error::other("unexpected wait response result"));
            };
            if event.event != EventKind::PaneAgentStatusChanged {
                return Err(std::io::Error::other("unexpected wait event kind"));
            }
            let EventData::PaneAgentStatusChanged {
                pane_id,
                workspace_id,
                agent_status,
                agent,
                title,
                display_agent,

                state_labels,
            } = event.data
            else {
                return Err(std::io::Error::other("unexpected wait event data"));
            };
            let event = SubscriptionEventEnvelope {
                event: SubscriptionEventKind::PaneAgentStatusChanged,
                data: SubscriptionEventData::PaneAgentStatusChanged(
                    crate::api::schema::PaneAgentStatusChangedEvent {
                        pane_id,
                        workspace_id,
                        agent_status,
                        agent,

                        title,
                        display_agent,
                        state_labels,
                    },
                ),
            };
            println!(
                "{}",
                serde_json::to_string(&event).map_err(std::io::Error::other)?
            );
            Ok(0)
        }
        Err(ApiClientError::ErrorResponse(response)) => {
            if response.error.code == "timeout" {
                eprintln!("timed out waiting for agent status change");
            } else {
                eprintln!(
                    "{}",
                    serde_json::to_string(&response).map_err(std::io::Error::other)?
                );
            }
            Ok(1)
        }
        Err(err) => Err(api_client_error_to_io(err)),
    }
}

pub(super) fn print_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(response).unwrap());
    Ok(0)
}

pub(super) fn send_ok_request(method: Method) -> std::io::Result<i32> {
    let response = send_request(&Request {
        id: "cli:request".into(),
        method,
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    Ok(0)
}

pub(super) fn send_request(request: &Request) -> std::io::Result<serde_json::Value> {
    let client = ApiClient::local();
    ensure_server_protocol_compatible(&client, &request.id)?;
    client
        .request_value(request)
        .map_err(|err| map_server_not_running_or_io(err, &request.id, &client))
}

pub(super) fn send_request_unchecked(request: &Request) -> std::io::Result<serde_json::Value> {
    let client = ApiClient::local();
    client
        .request_value(request)
        .map_err(|err| map_server_not_running_or_io(err, &request.id, &client))
}

fn ensure_server_protocol_compatible(client: &ApiClient, request_id: &str) -> std::io::Result<()> {
    let status = client
        .status()
        .map_err(|err| map_server_not_running_or_io(err, request_id, client))?;
    let server_protocol = status
        .protocol
        .ok_or_else(|| std::io::Error::other("server ping did not include a protocol version"))?;
    match protocol_guard::mismatch_response(
        request_id,
        server_protocol,
        &crate::session::active_restart_after_update_guidance(),
    ) {
        Some(response) => Err(protocol_guard::mismatch_error(response)),
        None => Ok(()),
    }
}

pub(crate) fn protocol_mismatch_response(err: &std::io::Error) -> Option<&ErrorResponse> {
    protocol_guard::error_response(err)
}

pub(crate) fn server_not_running_response(err: &std::io::Error) -> Option<&ErrorResponse> {
    server_not_running::reported_response(err)
}

pub(super) fn server_not_running_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

fn map_server_not_running_or_io(
    err: ApiClientError,
    request_id: &str,
    client: &ApiClient,
) -> std::io::Error {
    match err {
        ApiClientError::Io(err) if server_not_running_error(&err) => {
            server_not_running::reported_error(server_not_running::response(
                request_id,
                &client.socket_path(),
            ))
        }
        err => api_client_error_to_io(err),
    }
}

fn api_client_error_to_io(err: ApiClientError) -> std::io::Error {
    match err {
        ApiClientError::Io(err) => err,
        err => std::io::Error::other(err),
    }
}

pub(super) fn normalize_workspace_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn normalize_tab_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn normalize_pane_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn parse_split_direction(value: &str) -> std::io::Result<SplitDirection> {
    match value {
        "right" => Ok(SplitDirection::Right),
        "down" => Ok(SplitDirection::Down),
        _ => Err(std::io::Error::other(format!(
            "invalid split direction: {value}"
        ))),
    }
}

pub(super) fn parse_read_source(value: &str) -> std::io::Result<ReadSource> {
    match value {
        "visible" => Ok(ReadSource::Visible),
        "recent" => Ok(ReadSource::Recent),
        "recent-unwrapped" | "recent_unwrapped" => Ok(ReadSource::RecentUnwrapped),
        "detection" => Ok(ReadSource::Detection),
        _ => Err(std::io::Error::other(format!(
            "invalid read source: {value}"
        ))),
    }
}

pub(super) fn parse_read_format(value: &str) -> std::io::Result<ReadFormat> {
    match value {
        "text" => Ok(ReadFormat::Text),
        "ansi" => Ok(ReadFormat::Ansi),
        _ => Err(std::io::Error::other(format!(
            "invalid read format: {value}"
        ))),
    }
}

fn parse_agent_status(value: &str) -> std::io::Result<AgentStatus> {
    match value {
        "idle" => Ok(AgentStatus::Idle),
        "working" => Ok(AgentStatus::Working),
        "blocked" => Ok(AgentStatus::Blocked),
        "done" => Ok(AgentStatus::Done),
        "unknown" => Ok(AgentStatus::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid agent status: {value} (expected idle, working, blocked, done, or unknown)"
        ))),
    }
}

pub(super) fn parse_pane_agent_state(value: &str) -> std::io::Result<PaneAgentState> {
    match value {
        "idle" => Ok(PaneAgentState::Idle),
        "working" => Ok(PaneAgentState::Working),
        "blocked" => Ok(PaneAgentState::Blocked),
        "unknown" => Ok(PaneAgentState::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid pane agent state: {value} (expected idle, working, blocked, or unknown)"
        ))),
    }
}

pub(super) fn parse_u32_flag(flag: &str, value: &str) -> std::io::Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

pub(super) fn parse_u64_flag(flag: &str, value: &str) -> std::io::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

fn parse_session_json_only(args: &[String], usage: &str) -> Result<bool, i32> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn parse_session_name_and_json(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let mut name = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if name.is_none() {
            name = Some(arg.clone());
        } else {
            eprintln!("{usage}");
            return Err(2);
        }
    }

    let Some(name) = name else {
        eprintln!("{usage}");
        return Err(2);
    };
    Ok((name, json))
}

fn print_session_table(sessions: &[crate::session::SessionInfo]) {
    println!("{:<20} {:<8} {:<48} socket", "name", "status", "directory");
    for session in sessions {
        println!(
            "{:<20} {:<8} {:<48} {}",
            session.name,
            if session.running {
                "running"
            } else {
                "stopped"
            },
            session.session_dir,
            session.socket_path
        );
    }
}

fn print_session_error(code: &str, message: &str) {
    eprintln!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        }))
        .unwrap()
    );
}

fn print_config_help() {
    eprintln!("zynk config commands:");
    eprintln!("  zynk config check  validate config.toml and print diagnostics");
    eprintln!("  zynk config reset-keys  back up config.toml and remove custom keybindings");
}

fn print_terminal_help() {
    eprintln!("zynk terminal commands:");
    eprintln!("  zynk terminal attach <terminal_id> [--takeover]");
    eprintln!("  zynk terminal session control <target> [--takeover] [--cols N] [--rows N]");
    eprintln!("  zynk terminal session observe <target> [--cols N] [--rows N]");
    eprintln!("  detach from direct attach with ctrl+b q; send literal ctrl+b with ctrl+b ctrl+b");
}

fn print_wait_help() {
    eprintln!("zynk wait commands:");
    eprintln!("  zynk wait output <pane_id> --match <text> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--timeout MS] [--regex] [--raw]");
    eprintln!(
        "  zynk wait agent-status <pane_id> --status <idle|working|blocked|done|unknown> [--timeout MS]"
    );
}

fn print_session_help() {
    eprintln!("zynk session commands:");
    eprintln!("  zynk session list [--json]");
    eprintln!("  zynk session attach <name>");
    eprintln!("  zynk session stop <name> [--json]");
    eprintln!("  zynk session delete <name> [--json]");
    eprintln!("  use 'default' as <name> to target the default session for stop");
}

fn _print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).unwrap());
}

#[cfg(test)]
mod tests {
    #[test]
    fn m835_mismatch_response_pins_protocol_not_package_and_keeps_typed_error() {
        use super::protocol_guard::{error_response, mismatch_error, mismatch_response};
        let current = crate::protocol::PROTOCOL_VERSION;
        assert_eq!(current, 19);
        assert!(mismatch_response("same", current, "restart-fixture").is_none());
        for (server, guidance_present) in [(current - 1, true), (current + 1, false)] {
            let response = mismatch_response("original-id", server, "restart-fixture").unwrap();
            assert_eq!(response.id, "original-id");
            assert_eq!(response.error.code, "protocol_mismatch");
            assert!(response
                .error
                .message
                .contains(&format!("server protocol {server}")));
            assert_eq!(
                response.error.message.contains("restart-fixture"),
                guidance_present
            );
            assert!(response.error.message.contains(if guidance_present {
                "restart"
            } else {
                "upgrade"
            }));
            let expected = serde_json::to_value(&response).unwrap();
            let error = mismatch_error(response);
            assert_eq!(
                serde_json::to_value(error_response(&error).unwrap()).unwrap(),
                expected
            );
            assert!(error.to_string().contains("protocol"));
            assert!(error_response(&std::io::Error::other(error.to_string())).is_none());
        }
        assert!(error_response(&std::io::Error::other("ordinary failure")).is_none());
    }

    #[test]
    fn terminal_session_options_default_and_override_dimensions() {
        let defaults =
            super::parse_terminal_session_options(&["w1:p1".to_owned()], "usage", "observe", false)
                .expect("parse")
                .expect("options");
        assert_eq!(defaults.target, "w1:p1");
        assert_eq!((defaults.cols, defaults.rows), (120, 40));
        assert!(!defaults.takeover);

        let control = super::parse_terminal_session_options(
            &[
                "agent:codex".to_owned(),
                "--takeover".to_owned(),
                "--cols".to_owned(),
                "200".to_owned(),
                "--rows".to_owned(),
                "60".to_owned(),
            ],
            "usage",
            "control",
            true,
        )
        .expect("parse")
        .expect("options");
        assert_eq!(control.target, "agent:codex");
        assert_eq!((control.cols, control.rows), (200, 60));
        assert!(control.takeover);
    }

    #[test]
    fn terminal_session_observer_rejects_takeover_and_zero_dimensions() {
        let takeover = super::parse_terminal_session_options(
            &["w1:p1".to_owned(), "--takeover".to_owned()],
            "usage",
            "observe",
            false,
        )
        .expect("parse");
        assert!(matches!(takeover, Err(2)));
        assert!(super::parse_terminal_dimension("0", "--cols").is_err());
        assert!(super::parse_terminal_dimension("65536", "--rows").is_err());
    }

    #[test]
    fn parses_channel_set_argument() {
        assert_eq!(
            super::parse_channel_set_arg(&["preview".to_string()]),
            Some("preview")
        );
        assert_eq!(
            super::parse_channel_set_arg(&["stable".to_string()]),
            Some("stable")
        );
        assert_eq!(super::parse_channel_set_arg(&["nightly".to_string()]), None);
        assert_eq!(
            super::parse_channel_set_arg(&["preview".to_string(), "stable".to_string()]),
            None
        );
    }

    #[test]
    fn channel_set_rejects_package_managed_preview_before_config_write() {
        assert_eq!(
            super::channel_set_rejection("preview", Some("no preview")),
            Some("no preview")
        );
        assert_eq!(
            super::channel_set_rejection("stable", Some("no preview")),
            None
        );
        assert_eq!(super::channel_set_rejection("preview", None), None);
    }

    #[test]
    fn channel_set_rejects_stable_only_on_windows() {
        assert_eq!(super::channel_set_rejection("stable", None), None);
    }

    #[test]
    fn channel_set_skips_self_update_for_package_manager_guidance() {
        assert_eq!(
            super::channel_set_install_action(Some("use package manager")),
            super::ChannelSetInstallAction::PrintGuidance("use package manager")
        );
        assert_eq!(
            super::channel_set_install_action(None),
            super::ChannelSetInstallAction::RunSelfUpdate
        );
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn is_help_flag_matches_only_flag_forms() {
        assert!(super::is_help_flag("--help"));
        assert!(super::is_help_flag("-h"));
        // the bare word `help` is a legitimate value/label, NOT a help flag
        assert!(!super::is_help_flag("help"));
        assert!(!super::is_help_flag("w2:p2"));
        assert!(!super::is_help_flag("--label"));
    }

    #[test]
    fn leaf_help_requested_only_fires_in_exact_position() {
        // exact `<leaf> --help` / `-h`
        assert!(super::leaf_help_requested(&v(&["split", "--help"])));
        assert!(super::leaf_help_requested(&v(&["read", "-h"])));
        // SAFETY: never a `--`-delimited payload, a flag value, or a bare-word label
        assert!(!super::leaf_help_requested(&v(&[
            "run", "w1:p1", "--", "--help"
        ])));
        assert!(!super::leaf_help_requested(&v(&[
            "send-text",
            "w1:p1",
            "--",
            "--help"
        ])));
        assert!(!super::leaf_help_requested(&v(&[
            "create", "--label", "--help"
        ])));
        assert!(!super::leaf_help_requested(&v(&[
            "rename", "w1:p1", "help"
        ])));
        assert!(!super::leaf_help_requested(&v(&[
            "rename", "w1:p1", "--help"
        ])));
        // not a leaf-help position: bare group flag / lone leaf
        assert!(!super::leaf_help_requested(&v(&["--help"])));
        assert!(!super::leaf_help_requested(&v(&["split"])));
    }

    #[test]
    fn group_leaf_help_flag_exits_zero() {
        // P0 leaves (incl. the exit-1 OS-error and misleading-error cases) now -> Ok(0).
        // The leaf-help guard returns before any socket dispatch, so these are socket-free.
        assert_eq!(
            super::pane::run_pane_command(&v(&["read", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::pane::run_pane_command(&v(&["get", "-h"])).unwrap(),
            0
        );
        assert_eq!(
            super::pane::run_pane_command(&v(&["split", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::run_wait_command(&v(&["output", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::run_wait_command(&v(&["agent-status", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::integration::run_integration_command(&v(&["install", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::skill::run_skill_command(&v(&["install", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::skill::run_skill_command(&v(&["status", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::plugin::run_plugin_command(&v(&["list", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::plugin::run_plugin_command(&v(&["log", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::agent::run_agent_command(&v(&["get", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::agent::run_agent_command(&v(&["read", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::workspace::run_workspace_command(&v(&["focus", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::workspace::run_workspace_command(&v(&["close", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::tab::run_tab_command(&v(&["focus", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::status::run_status_command(&v(&["server", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::status::run_status_command(&v(&["client", "-h"])).unwrap(),
            0
        );
    }

    #[test]
    fn plugin_subgroups_help_preserved() {
        // `plugin action`/`plugin pane` keep their OWN sub-group help (exit 0),
        // never hijacked into the plugin group help.
        assert_eq!(
            super::plugin::run_plugin_command(&v(&["action", "--help"])).unwrap(),
            0
        );
        assert_eq!(
            super::plugin::run_plugin_command(&v(&["pane", "--help"])).unwrap(),
            0
        );
        // exact leaf-help inside a sub-group also exits 0
        assert_eq!(
            super::plugin::run_plugin_command(&v(&["action", "list", "--help"])).unwrap(),
            0
        );
    }

    #[test]
    fn unknown_leaf_in_help_position_shows_group_help_exit_zero() {
        // Intentional + tested: a mistyped leaf in the help position yields the group help
        // (which lists the valid leaves), not an error.
        assert_eq!(
            super::pane::run_pane_command(&v(&["bogus", "--help"])).unwrap(),
            0
        );
    }
}
