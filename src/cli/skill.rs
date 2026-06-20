//! `zynk skill` CLI: install/status for the native agent skill.
//!
//! Thin dispatch over `crate::zynk::skill`. Only first-class targets
//! (claude/pi/codex) install; every other known agent is reported `unsupported`.

use serde_json::{json, Value};

use crate::zynk::skill::{
    expected_hash, expected_version, install_skill, skill_status, skill_statuses, SkillAgent,
    SkillInstallOutcome, SkillStatus, SkillStatusKind,
};

pub(super) fn run_skill_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        eprintln!("{}", skill_help_text());
        return Ok(2);
    };
    // `zynk skill install --help` / `status --help` (exact position) -> skill help.
    if crate::cli::leaf_help_requested(args) {
        println!("{}", skill_help_text());
        return Ok(0);
    }
    match subcommand {
        "install" => skill_install(&args[1..]),
        "status" => skill_status_command(&args[1..]),
        "help" | "--help" | "-h" => {
            println!("{}", skill_help_text());
            Ok(0)
        }
        other => {
            eprintln!("unknown `zynk skill` subcommand: {other}");
            eprintln!("{}", skill_help_text());
            Ok(2)
        }
    }
}

/// Split args into positionals and known flags; collect unknown flags.
struct ParsedArgs {
    positionals: Vec<String>,
    all: bool,
    force: bool,
    json: bool,
    unknown: Vec<String>,
}

fn parse_args(args: &[String]) -> ParsedArgs {
    let mut parsed = ParsedArgs {
        positionals: Vec::new(),
        all: false,
        force: false,
        json: false,
        unknown: Vec::new(),
    };
    for arg in args {
        match arg.as_str() {
            "--all" => parsed.all = true,
            "--force" => parsed.force = true,
            "--json" => parsed.json = true,
            other if other.starts_with('-') => parsed.unknown.push(other.to_string()),
            other => parsed.positionals.push(other.to_string()),
        }
    }
    parsed
}

fn skill_install(args: &[String]) -> std::io::Result<i32> {
    let parsed = parse_args(args);

    if let Some(flag) = parsed.unknown.first() {
        eprintln!("unknown option for `zynk skill install`: {flag}");
        eprintln!("usage: zynk skill install <claude|pi|codex> [--force] | --all [--force]");
        return Ok(2);
    }
    if parsed.json {
        eprintln!("`zynk skill install` does not support --json");
        return Ok(2);
    }
    if parsed.all && !parsed.positionals.is_empty() {
        eprintln!("`zynk skill install` takes either a target or --all, not both");
        return Ok(2);
    }

    if parsed.all {
        return install_all(parsed.force);
    }

    if parsed.positionals.len() != 1 {
        eprintln!("usage: zynk skill install <claude|pi|codex> [--force] | --all [--force]");
        return Ok(2);
    }

    let label = &parsed.positionals[0];
    let Some(agent) = SkillAgent::from_label(label) else {
        eprintln!("unknown agent: {label}");
        eprintln!("known agents: {}", known_agent_labels());
        return Ok(2);
    };
    if !agent.is_supported() {
        eprintln!(
            "{} is not a supported install target ({})",
            agent.label(),
            agent
                .unsupported_reason()
                .unwrap_or("no known skill directory")
        );
        eprintln!("supported install targets: claude, pi, codex");
        return Ok(2);
    }

    match install_skill(agent, parsed.force) {
        Ok(outcome) => {
            println!("{}", describe_outcome(agent, &outcome));
            Ok(0)
        }
        Err(error) => {
            eprintln!("{error}");
            Ok(1)
        }
    }
}

fn install_all(force: bool) -> std::io::Result<i32> {
    let mut had_error = false;
    for agent in SkillAgent::ALL {
        if !agent.is_supported() {
            println!(
                "skipped {} (unsupported: {})",
                agent.label(),
                agent.unsupported_reason().unwrap_or("no skill directory")
            );
            continue;
        }
        match install_skill(agent, force) {
            Ok(outcome) => println!("{}", describe_outcome(agent, &outcome)),
            Err(error) => {
                eprintln!("{}: {error}", agent.label());
                had_error = true;
            }
        }
    }
    Ok(if had_error { 1 } else { 0 })
}

fn skill_status_command(args: &[String]) -> std::io::Result<i32> {
    let parsed = parse_args(args);

    if let Some(flag) = parsed.unknown.first() {
        eprintln!("unknown option for `zynk skill status`: {flag}");
        eprintln!("usage: zynk skill status [<agent>] [--json]");
        return Ok(2);
    }
    if parsed.all || parsed.force {
        eprintln!("`zynk skill status` does not take --all or --force (no target => all agents)");
        return Ok(2);
    }
    if parsed.positionals.len() > 1 {
        eprintln!("usage: zynk skill status [<agent>] [--json]");
        return Ok(2);
    }

    let statuses = if let Some(label) = parsed.positionals.first() {
        let Some(agent) = SkillAgent::from_label(label) else {
            eprintln!("unknown agent: {label}");
            eprintln!("known agents: {}", known_agent_labels());
            return Ok(2);
        };
        vec![skill_status(agent)]
    } else {
        skill_statuses()
    };

    if parsed.json {
        println!("{}", status_json(&statuses));
    } else {
        for status in &statuses {
            println!("{}", describe_status(status));
        }
    }
    Ok(0)
}

fn known_agent_labels() -> String {
    SkillAgent::ALL
        .iter()
        .map(|agent| agent.label())
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_outcome(agent: SkillAgent, outcome: &SkillInstallOutcome) -> String {
    match outcome {
        SkillInstallOutcome::AlreadyCurrent(path) => {
            format!(
                "{} skill already current -> {}",
                agent.label(),
                path.display()
            )
        }
        SkillInstallOutcome::Installed(path) => {
            format!("installed {} skill -> {}", agent.label(), path.display())
        }
        SkillInstallOutcome::Updated(path) => {
            format!("updated {} skill -> {}", agent.label(), path.display())
        }
        SkillInstallOutcome::BackedUpAndReplaced { path, backup } => format!(
            "replaced non-zynk {} file (backed up to {}) -> {}",
            agent.label(),
            backup.display(),
            path.display()
        ),
    }
}

fn describe_status(status: &SkillStatus) -> String {
    let label = status.agent.label();
    match status.state {
        SkillStatusKind::Unsupported => format!(
            "{label}: unsupported ({})",
            status.reason.unwrap_or("no known skill directory")
        ),
        SkillStatusKind::NotInstalled => format!("{label}: not installed ({})", display_path(status)),
        SkillStatusKind::Current => format!(
            "{label}: current ({}) {}",
            version_str(status),
            display_path(status)
        ),
        SkillStatusKind::Outdated => format!(
            "{label}: outdated ({}) {}",
            version_str(status),
            display_path(status)
        ),
        SkillStatusKind::ConflictCustom => format!(
            "{label}: conflict-custom (non-zynk file present; use --force to back up and replace) {}",
            display_path(status)
        ),
    }
}

fn display_path(status: &SkillStatus) -> String {
    status
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn version_str(status: &SkillStatus) -> String {
    match status.installed_version {
        Some(version) => format!("v{version}"),
        None => "unversioned".to_string(),
    }
}

fn status_json(statuses: &[SkillStatus]) -> Value {
    let skills: Vec<Value> = statuses
        .iter()
        .map(|status| {
            json!({
                "agent": status.agent.label(),
                "supported": status.supported,
                "path": status.path.as_ref().map(|path| path.display().to_string()),
                "state": status.state.as_str(),
                "managed": status.managed,
                "installed_version": status.installed_version,
                "installed_hash": status.installed_hash,
                "reason": status.reason,
            })
        })
        .collect();
    json!({
        "command": "zynk skill status",
        "result": "ok",
        "type": "zynk_skill_status",
        "expected_version": expected_version(),
        "expected_hash": expected_hash(),
        "skills": skills,
    })
}

fn skill_help_text() -> String {
    "\
zynk skill — install and check the native zynk agent skill

Usage:
  zynk skill install <claude|pi|codex> [--force]   Install/update the managed skill for one agent
  zynk skill install --all [--force]               Install for all supported agents; skip unsupported
  zynk skill status [<agent>] [--json]             Show skill status; no agent => all known agents
  zynk skill help                                  Show this help

Supported install targets (have a reusable skill directory):
  claude  -> ~/.claude/skills/zynk/SKILL.md         (override: CLAUDE_CONFIG_DIR)
  pi      -> ~/.pi/agent/skills/zynk/SKILL.md       (override: PI_CODING_AGENT_DIR)
  codex   -> ~/.codex/skills/zynk/SKILL.md          (override: CODEX_HOME)

Other known agents (omp, copilot, devin, droid, kimi, opencode, kilo, hermes, qodercli, cursor)
have no verified reusable skill directory; `status` reports them `unsupported` and `install` refuses.

Status states:
  current          installed and byte-identical to the shipped skill
  outdated         a managed zynk skill, but an older/edited version (install updates it)
  not-installed    no skill file at the target path
  conflict-custom  a non-zynk file exists; install refuses unless --force
  unsupported      the agent has no known reusable skill directory

Safety:
  - Only ever writes <base>/skills/zynk/SKILL.md (atomic write).
  - --force backs up a conflicting custom file to a unique SKILL.md.bak-<hash> first;
    an existing backup is never overwritten with different content.

Examples:
  zynk skill status --json
  zynk skill install codex
  zynk skill install codex --force
  zynk skill install --all"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_lists_supported_targets_and_unsupported_behavior() {
        let help = skill_help_text();
        for token in [
            "claude",
            "pi",
            "codex",
            "unsupported",
            "conflict-custom",
            "--force",
        ] {
            assert!(help.contains(token), "help must mention `{token}`");
        }
        // Bare/`help` invocation exits cleanly (2 for no subcommand, 0 for help).
        assert_eq!(run_skill_command(&args(&[])).unwrap(), 2);
        assert_eq!(run_skill_command(&args(&["help"])).unwrap(), 0);
    }

    #[test]
    fn status_json_shape_includes_registry_fields() {
        let statuses = vec![
            SkillStatus {
                agent: SkillAgent::Claude,
                supported: true,
                path: Some(std::path::PathBuf::from("/tmp/claude/skills/zynk/SKILL.md")),
                state: SkillStatusKind::NotInstalled,
                managed: false,
                installed_version: None,
                installed_hash: None,
                reason: None,
            },
            SkillStatus {
                agent: SkillAgent::Cursor,
                supported: false,
                path: None,
                state: SkillStatusKind::Unsupported,
                managed: false,
                installed_version: None,
                installed_hash: None,
                reason: Some("no verified reusable skill directory"),
            },
        ];

        let value = status_json(&statuses);
        assert_eq!(value["command"], "zynk skill status");
        assert_eq!(value["type"], "zynk_skill_status");
        assert_eq!(value["result"], "ok");
        assert!(value["expected_hash"].as_str().unwrap().len() >= 64);
        assert_eq!(value["skills"].as_array().unwrap().len(), 2);
        assert_eq!(value["skills"][0]["agent"], "claude");
        assert_eq!(value["skills"][0]["state"], "not-installed");
        assert_eq!(value["skills"][1]["agent"], "cursor");
        assert_eq!(value["skills"][1]["state"], "unsupported");
        assert_eq!(value["skills"][1]["supported"], false);
    }

    #[test]
    fn install_rejects_mixed_target_and_all() {
        assert_eq!(
            run_skill_command(&args(&["install", "codex", "--all"])).unwrap(),
            2
        );
    }

    #[test]
    fn parser_rejects_unknown_agent_and_unknown_flag() {
        // Both fail at parse/registry time, before any filesystem access.
        assert_eq!(run_skill_command(&args(&["status", "bogus"])).unwrap(), 2);
        assert_eq!(
            run_skill_command(&args(&["install", "claude", "--nope"])).unwrap(),
            2
        );
        assert_eq!(run_skill_command(&args(&["wat"])).unwrap(), 2);
    }

    #[test]
    fn install_rejects_unsupported_target_with_exit_2() {
        assert_eq!(run_skill_command(&args(&["install", "cursor"])).unwrap(), 2);
        assert_eq!(run_skill_command(&args(&["install", "omp"])).unwrap(), 2);
    }
}
