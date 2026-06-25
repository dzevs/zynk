use std::path::Path;

use serde::{Deserialize, Serialize};

const MAX_SESSION_ID_LEN: usize = 512;
const MAX_SESSION_PATH_LEN: usize = 4096;
/// Upper bound on the number of argv tokens we will scan when preserving original launch
/// flags. Bounds the work and rejects pathological argv before any rewrite.
const MAX_ARGV_TOKENS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRef {
    pub kind: AgentSessionRefKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRefKind {
    Id,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResumePlan {
    pub agent: String,
    pub argv: Vec<String>,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedAgentSession {
    pub source: String,
    pub agent: String,
    pub session_ref: AgentSessionRef,
}

impl AgentSessionRef {
    pub fn id(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_id(&value).then_some(Self {
            kind: AgentSessionRefKind::Id,
            value,
        })
    }

    pub fn path(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_path(&value).then_some(Self {
            kind: AgentSessionRefKind::Path,
            value,
        })
    }
}

pub fn session_ref_from_report(
    source: &str,
    agent: &str,
    agent_session_id: Option<String>,
    _agent_session_path: Option<String>,
) -> Option<AgentSessionRef> {
    if !is_official_agent_source(source, agent) {
        return None;
    }

    if agent == "pi" || agent == "omp" {
        return _agent_session_path
            .and_then(AgentSessionRef::path)
            .or_else(|| agent_session_id.and_then(AgentSessionRef::id));
    }

    agent_session_id.and_then(AgentSessionRef::id)
}

pub fn normalize_claude_session_start_source(value: Option<String>) -> Option<String> {
    match value.as_deref().map(str::trim) {
        Some(source @ ("startup" | "resume" | "clear" | "compact")) => Some(source.to_string()),
        _ => None,
    }
}

pub fn is_reserved_native_state_source(source: &str, agent: &str) -> bool {
    matches!(
        (source, agent),
        ("zynk:claude", "claude")
            | ("zynk:codex", "codex")
            | ("zynk:copilot", "copilot")
            | ("zynk:devin", "devin")
            | ("zynk:droid", "droid")
            | ("zynk:qodercli", "qodercli")
            | ("zynk:cursor", "cursor")
    )
}

pub fn session_ref_from_snapshot(
    source: &str,
    agent: &str,
    kind: AgentSessionRefKind,
    value: &str,
) -> Option<PersistedAgentSession> {
    if !is_official_agent_source(source, agent) {
        return None;
    }
    let session_ref = match (agent, kind) {
        ("pi" | "omp", AgentSessionRefKind::Path) => AgentSessionRef::path(value)?,
        (_, AgentSessionRefKind::Id) => AgentSessionRef::id(value)?,
        _ => return None,
    };
    Some(PersistedAgentSession {
        source: source.to_string(),
        agent: agent.to_string(),
        session_ref,
    })
}

pub fn plan(
    source: &str,
    agent: &str,
    session_ref: &AgentSessionRef,
    original_argv: Option<&[String]>,
) -> Option<AgentResumePlan> {
    if !is_official_agent_source(source, agent) {
        return None;
    }

    let canonical = canonical_resume_argv(source, agent, session_ref)?;
    let argv = match original_argv {
        Some(orig) => rewrite_preserving_flags(agent, orig, session_ref).unwrap_or(canonical),
        None => canonical,
    };

    Some(AgentResumePlan {
        agent: agent.to_string(),
        argv,
        dedupe_key: dedupe_key(source, agent, session_ref),
    })
}

/// Decide the launch argv to persist for an agent pane snapshot so its resume preserves the operator's
/// flags. Prefers a resume argv sanitized from the live foreground command (Tier B — covers agents
/// launched manually in a shell, which never populate `launch_argv`); falls back to a sanitized
/// existing launch argv (Tier A — zynk-started or recorded-back panes).
///
/// Returns `Some` ONLY when the sanitized result preserves at least one flag beyond the canonical
/// resume; otherwise `None`. `None` means no sanitized argv is persisted: for an OFFICIAL native
/// agent snapshot, restore falls back to the canonical resume rebuilt from the session ref — it must
/// NOT persist or replay the raw existing/launch argv (TB-001). The result is never raw process argv:
/// every candidate passes through `plan()`'s default-deny adapter, so secret-bearing (`pi --api-key`),
/// unknown, `--` payload, variadic, and non-resume forms collapse to canonical and are filtered out
/// here — they are never persisted or replayed.
pub fn persisted_resume_argv(
    source: &str,
    agent: &str,
    session_ref: &AgentSessionRef,
    foreground_argv: Option<&[String]>,
    existing_launch_argv: Option<&[String]>,
) -> Option<Vec<String>> {
    let canonical = canonical_resume_argv(source, agent, session_ref)?;
    let sanitized = |argv: Option<&[String]>| -> Option<Vec<String>> {
        argv.and_then(|a| plan(source, agent, session_ref, Some(a)).map(|p| p.argv))
            .filter(|out| out.len() > canonical.len())
    };
    sanitized(foreground_argv).or_else(|| sanitized(existing_launch_argv))
}

/// Today's fixed-minimal resume argv, built from agent identity + session ref only.
/// This is the canonical fallback when no original argv is available or it cannot be
/// safely rewritten.
fn canonical_resume_argv(
    source: &str,
    agent: &str,
    session_ref: &AgentSessionRef,
) -> Option<Vec<String>> {
    let argv = match (source, agent, session_ref.kind) {
        ("zynk:claude", "claude", AgentSessionRefKind::Id) => {
            vec![
                "claude".into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        ("zynk:codex", "codex", AgentSessionRefKind::Id) => {
            vec!["codex".into(), "resume".into(), session_ref.value.clone()]
        }
        ("zynk:copilot", "copilot", AgentSessionRefKind::Id) => {
            vec!["copilot".into(), format!("--resume={}", session_ref.value)]
        }
        ("zynk:devin", "devin", AgentSessionRefKind::Id) => {
            vec!["devin".into(), "--resume".into(), session_ref.value.clone()]
        }
        ("zynk:droid", "droid", AgentSessionRefKind::Id) => {
            vec!["droid".into(), "--resume".into(), session_ref.value.clone()]
        }
        ("zynk:kimi", "kimi", AgentSessionRefKind::Id) => {
            vec!["kimi".into(), "--session".into(), session_ref.value.clone()]
        }
        ("zynk:pi", "pi", AgentSessionRefKind::Path | AgentSessionRefKind::Id) => {
            vec!["pi".into(), "--session".into(), session_ref.value.clone()]
        }
        ("zynk:omp", "omp", AgentSessionRefKind::Path | AgentSessionRefKind::Id) => {
            // omp resume is `-r, --resume=<value>` (ID prefix or path); it has no
            // `--session` flag, unlike pi.
            vec!["omp".into(), format!("--resume={}", session_ref.value)]
        }
        ("zynk:hermes", "hermes", AgentSessionRefKind::Id) => {
            vec![
                "hermes".into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        ("zynk:opencode", "opencode", AgentSessionRefKind::Id) => {
            vec![
                "opencode".into(),
                "--session".into(),
                session_ref.value.clone(),
            ]
        }
        ("zynk:qodercli", "qodercli", AgentSessionRefKind::Id) => {
            vec![
                "qodercli".into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        ("zynk:kilo", "kilo", AgentSessionRefKind::Id) => {
            vec!["kilo".into(), "--session".into(), session_ref.value.clone()]
        }
        ("zynk:cursor", "cursor", AgentSessionRefKind::Id) => {
            vec![
                "cursor-agent".into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        _ => return None,
    };

    Some(argv)
}

pub fn dedupe_key(source: &str, agent: &str, session_ref: &AgentSessionRef) -> String {
    format!(
        "{source}\u{0}{agent}\u{0}{:?}\u{0}{}",
        session_ref.kind, session_ref.value
    )
}

fn is_official_agent_source(source: &str, agent: &str) -> bool {
    matches!(
        (source, agent),
        ("zynk:claude", "claude")
            | ("zynk:codex", "codex")
            | ("zynk:copilot", "copilot")
            | ("zynk:devin", "devin")
            | ("zynk:droid", "droid")
            | ("zynk:kimi", "kimi")
            | ("zynk:omp", "omp")
            | ("zynk:pi", "pi")
            | ("zynk:hermes", "hermes")
            | ("zynk:opencode", "opencode")
            | ("zynk:qodercli", "qodercli")
            | ("zynk:kilo", "kilo")
            | ("zynk:cursor", "cursor")
    )
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_LEN && !value.chars().any(char::is_control)
}

fn valid_session_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_PATH_LEN
        && !value.chars().any(char::is_control)
        && Path::new(value).is_absolute()
}

fn valid_argv_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_SESSION_PATH_LEN && !token.chars().any(char::is_control)
}

fn valid_argv(argv: &[String]) -> bool {
    !argv.is_empty() && argv.len() <= MAX_ARGV_TOKENS && argv.iter().all(|t| valid_argv_token(t))
}

/// True when the basename of `argv0` is exactly the agent's binary name. The three agents with
/// rewrite adapters (claude/codex/pi) launch under their own name, so a bare-name or absolute-path
/// argv0 matches, while a wrapped launcher (e.g. `node …/claude.js`) does not — and falls back to
/// the canonical resume, which is the safe behavior for a command we cannot faithfully rebuild.
fn argv0_matches_agent(argv0: &str, agent: &str) -> bool {
    Path::new(argv0).file_name().and_then(|name| name.to_str()) == Some(agent)
}

/// Per-agent rewrite spec (default-deny). Anything not described here forces a canonical fallback.
///
/// Allowlists are finalized against the installed CLI `--help` (claude 2.x, codex CLI, pi). They
/// cover documented runtime/policy options only; flags that take multiple values (variadic), select
/// an exit-and-quit mode, carry a secret (`--api-key`), or take arbitrary config (`codex -c/--config`)
/// are deliberately excluded so they fall back to the canonical resume instead of being replayed.
struct AgentRewriteSpec {
    /// Boolean session selectors (no value), stripped before re-injection.
    bool_selectors: &'static [&'static str],
    /// Value-bearing session selectors (consume a following value or `=value`), stripped.
    value_selectors: &'static [&'static str],
    /// The selector flag to inject for the resumed session.
    canonical_selector: &'static str,
    /// Recognized boolean policy/runtime flags to preserve verbatim.
    bool_allow: &'static [&'static str],
    /// Recognized value-bearing flags to preserve (`--flag value` and `--flag=value`).
    value_allow: &'static [&'static str],
}

// Claude Code (`claude --help`): selectors `-c/--continue`, `-r/--resume [value]`,
// `--session-id <uuid>`. `--session-id` pins a specific session id, so on resume it is stripped and
// replaced by the canonical `--resume <new-id>` (preserving any allowlisted runtime/policy flags).
const CLAUDE_SPEC: AgentRewriteSpec = AgentRewriteSpec {
    bool_selectors: &["--continue", "-c"],
    value_selectors: &["--resume", "-r", "--session-id"],
    canonical_selector: "--resume",
    bool_allow: &[
        "--dangerously-skip-permissions",
        "--allow-dangerously-skip-permissions",
        "--fork-session",
        "--safe-mode",
    ],
    value_allow: &[
        "--model",
        "--fallback-model",
        "--permission-mode",
        "--agent",
        "--effort",
    ],
};

// Pi (`pi --help`): selectors `-c/--continue`, `-r/--resume` (picker, no value),
// `--session <path|id>`, `--session-id <id>`. `--api-key` is intentionally NOT preserved.
const PI_SPEC: AgentRewriteSpec = AgentRewriteSpec {
    bool_selectors: &["--continue", "-c", "--resume", "-r"],
    value_selectors: &["--session", "--session-id"],
    canonical_selector: "--session",
    bool_allow: &[
        "--no-tools",
        "-nt",
        "--no-builtin-tools",
        "-nbt",
        "--approve",
        "-a",
        "--no-approve",
        "-na",
        "--offline",
        "--no-extensions",
        "-ne",
        "--no-skills",
        "-ns",
        "--no-context-files",
        "-nc",
    ],
    value_allow: &[
        "--provider",
        "--model",
        "--thinking",
        "--tools",
        "-t",
        "--exclude-tools",
        "-xt",
        "--models",
        "--mode",
    ],
};

// Codex (`codex --help` + `codex resume --help`): `resume` is a positional subcommand and accepts
// the runtime/policy options below. `-c/--config` (arbitrary key=value, secret risk),
// `--remote-auth-token-env`, and `-i/--image` are excluded.
const CODEX_BOOL_ALLOW: &[&str] = &[
    "--dangerously-bypass-approvals-and-sandbox",
    "--dangerously-bypass-hook-trust",
    "--oss",
    "--search",
    "--yolo",
];
const CODEX_VALUE_ALLOW: &[&str] = &[
    "-m",
    "--model",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "-C",
    "--cd",
    "--add-dir",
    "-p",
    "--profile",
    "--local-provider",
    "--enable",
    "--disable",
    "--remote",
];
// Picker/selection flags that are redundant once a concrete resumed id is injected — dropped.
const CODEX_SELECTOR_DROP: &[&str] = &["--last", "--all", "--include-non-interactive"];

/// True when `tok` is the `--flag=value` long form of a flag whose name is in `names`.
fn flag_eq_form_in(tok: &str, names: &[&str]) -> bool {
    tok.starts_with("--")
        && tok.contains('=')
        && names.contains(&tok.split('=').next().unwrap_or(""))
}

/// Rewrite the original launch argv to resume the given session while preserving the operator's
/// recognized policy/runtime flags. Returns `None` (caller falls back to canonical) when the agent
/// has no adapter or any token is not understood.
fn rewrite_preserving_flags(
    agent: &str,
    original_argv: &[String],
    session_ref: &AgentSessionRef,
) -> Option<Vec<String>> {
    if !valid_argv(original_argv) || !argv0_matches_agent(&original_argv[0], agent) {
        return None;
    }
    let argv0 = original_argv[0].clone();
    let rest = &original_argv[1..];
    match agent {
        "claude" => rewrite_flag_selector_agent(argv0, rest, &CLAUDE_SPEC, &session_ref.value),
        "pi" => rewrite_flag_selector_agent(argv0, rest, &PI_SPEC, &session_ref.value),
        "codex" => rewrite_codex(argv0, rest, &session_ref.value),
        _ => None,
    }
}

/// Claude/Pi: default-deny scan. Keep only allowlisted flags, replace the session selector at its
/// original index, and return `None` on `--` payloads, bare positionals, unknown flags, or an
/// ambiguous (missing/flag-like) value-flag value.
fn rewrite_flag_selector_agent(
    argv0: String,
    rest: &[String],
    spec: &AgentRewriteSpec,
    value: &str,
) -> Option<Vec<String>> {
    let mut kept: Vec<String> = Vec::new();
    let mut selector_pos: Option<usize> = None;
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i].as_str();
        if tok == "--" {
            return None; // payload/tail that could replay user input
        } else if spec.bool_selectors.contains(&tok) {
            selector_pos.get_or_insert(kept.len());
        } else if spec.value_selectors.contains(&tok) {
            selector_pos.get_or_insert(kept.len());
            // consume a following non-flag selector value (old id/path) when present
            if rest.get(i + 1).is_some_and(|n| !n.starts_with('-')) {
                i += 1;
            }
        } else if flag_eq_form_in(tok, spec.value_selectors) {
            selector_pos.get_or_insert(kept.len()); // `selector=value` form
        } else if spec.bool_allow.contains(&tok) {
            kept.push(rest[i].clone());
        } else if spec.value_allow.contains(&tok) {
            // space form: consume exactly one non-flag value, else ambiguous grammar -> fallback
            let val = rest.get(i + 1).filter(|n| !n.starts_with('-'))?;
            kept.push(rest[i].clone());
            kept.push(val.clone());
            i += 1;
        } else if flag_eq_form_in(tok, spec.value_allow) {
            kept.push(rest[i].clone()); // recognized `--flag=value` form
        } else {
            return None; // unknown flag or bare positional
        }
        i += 1;
    }
    let at = selector_pos.unwrap_or(kept.len());
    let mut out = vec![argv0];
    out.extend_from_slice(&kept[..at]);
    out.push(spec.canonical_selector.to_string());
    out.push(value.to_string());
    out.extend_from_slice(&kept[at..]);
    Some(out)
}

/// Codex: `resume` is a positional subcommand. Accept the interactive (global-flags-only) shape or
/// the `resume [old-id]` shape, preserve recognized runtime/policy flags (which `codex resume`
/// accepts), drop redundant picker flags, reject any other subcommand; normalize to
/// `argv0 resume <new-id> <kept flags>`.
fn rewrite_codex(argv0: String, rest: &[String], value: &str) -> Option<Vec<String>> {
    let mut kept: Vec<String> = Vec::new();
    let mut subcommand_seen = false;
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i].as_str();
        if tok == "--" {
            return None;
        } else if CODEX_SELECTOR_DROP.contains(&tok) {
            // redundant with a concrete resumed id — drop
        } else if tok == "resume" && !subcommand_seen {
            subcommand_seen = true;
            if rest.get(i + 1).is_some_and(|n| !n.starts_with('-')) {
                i += 1; // consume the old session id
            }
        } else if CODEX_BOOL_ALLOW.contains(&tok) {
            kept.push(rest[i].clone());
        } else if CODEX_VALUE_ALLOW.contains(&tok) {
            let val = rest.get(i + 1).filter(|n| !n.starts_with('-'))?;
            kept.push(rest[i].clone());
            kept.push(val.clone());
            i += 1;
        } else if flag_eq_form_in(tok, CODEX_VALUE_ALLOW) {
            kept.push(rest[i].clone());
        } else {
            return None; // unknown flag, non-resume subcommand, or bare positional payload
        }
        i += 1;
    }
    let mut out = vec![argv0, "resume".to_string(), value.to_string()];
    out.extend(kept);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_test_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    #[test]
    fn native_state_reservation_excludes_full_lifecycle_sources() {
        assert!(is_reserved_native_state_source("zynk:claude", "claude"));
        assert!(is_reserved_native_state_source("zynk:codex", "codex"));
        assert!(is_reserved_native_state_source("zynk:devin", "devin"));
        assert!(!is_reserved_native_state_source("zynk:kimi", "kimi"));
        assert!(!is_reserved_native_state_source(
            "zynk:opencode",
            "opencode"
        ));
    }

    fn pi_target_ref() -> AgentSessionRef {
        AgentSessionRef::path(absolute_test_path("s.jsonl")).unwrap()
    }

    #[test]
    fn argv_validation_rejects_control_chars_and_overlong_tokens() {
        assert!(valid_argv(&["claude".to_string(), "--resume".to_string()]));
        assert!(!valid_argv(&[
            "claude".to_string(),
            "bad\nflag".to_string()
        ]));
        assert!(!valid_argv(&[
            "claude".to_string(),
            "x".repeat(MAX_SESSION_PATH_LEN + 1)
        ]));
        assert!(!valid_argv(&[]));
    }

    #[test]
    fn argv0_matches_agent_accepts_paths_and_rejects_mismatch() {
        assert!(argv0_matches_agent("/usr/local/bin/claude", "claude"));
        assert!(argv0_matches_agent("claude", "claude"));
        assert!(!argv0_matches_agent("/usr/bin/python", "claude"));
        assert!(!argv0_matches_agent("node", "claude"));
    }

    // --- Happy path: policy flags preserved, selector replaced at its original index ---
    #[test]
    fn claude_resume_preserves_policy_flags_and_swaps_selector() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "--continue".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        let out = rewrite_preserving_flags("claude", &orig, &id).unwrap();
        assert_eq!(
            out,
            vec![
                "claude",
                "--resume",
                "new-id",
                "--dangerously-skip-permissions"
            ]
        );
    }

    #[test]
    fn claude_resume_strips_existing_resume_selector_before_reinjecting() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "--resume".to_string(),
            "OLD".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        let out = rewrite_preserving_flags("claude", &orig, &id).unwrap();
        assert_eq!(
            out,
            vec![
                "claude",
                "--resume",
                "new-id",
                "--dangerously-skip-permissions"
            ]
        );
    }

    #[test]
    fn claude_resume_inserts_selector_when_original_had_none() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        let out = rewrite_preserving_flags("claude", &orig, &id).unwrap();
        assert_eq!(
            out,
            vec![
                "claude",
                "--dangerously-skip-permissions",
                "--resume",
                "new-id"
            ]
        );
    }

    #[test]
    fn pi_resume_preserves_flags_and_swaps_continue_for_session() {
        let target = pi_target_ref();
        let orig = vec!["pi".to_string(), "-c".to_string()];
        let out = rewrite_preserving_flags("pi", &orig, &target).unwrap();
        assert_eq!(
            out,
            vec![
                "pi".to_string(),
                "--session".to_string(),
                target.value.clone()
            ]
        );
    }

    #[test]
    fn pi_resume_replaces_session_old_value_and_eq_forms() {
        let target = pi_target_ref();
        for orig in [
            vec![
                "pi".to_string(),
                "--session".to_string(),
                "/old.jsonl".to_string(),
            ],
            vec!["pi".to_string(), "--session=/old.jsonl".to_string()],
        ] {
            let out = rewrite_preserving_flags("pi", &orig, &target).unwrap();
            assert_eq!(
                out,
                vec![
                    "pi".to_string(),
                    "--session".to_string(),
                    target.value.clone()
                ]
            );
        }
    }

    #[test]
    fn codex_resume_replaces_existing_session_id_preserving_yolo() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "codex".to_string(),
            "resume".to_string(),
            "OLD".to_string(),
            "--yolo".to_string(),
        ];
        let out = rewrite_preserving_flags("codex", &orig, &id).unwrap();
        assert_eq!(out, vec!["codex", "resume", "new-id", "--yolo"]);
    }

    #[test]
    fn codex_resume_preserves_yolo_from_interactive_shape() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec!["codex".to_string(), "--yolo".to_string()];
        let out = rewrite_preserving_flags("codex", &orig, &id).unwrap();
        assert_eq!(out, vec!["codex", "resume", "new-id", "--yolo"]);
    }

    // --- Finalized allowlist coverage (flags taken from claude/codex/pi --help) ---
    #[test]
    fn claude_resume_replaces_short_resume_alias_and_preserves_permission_mode() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "-r".to_string(),
            "OLD".to_string(),
            "--permission-mode".to_string(),
            "plan".to_string(),
            "--allow-dangerously-skip-permissions".to_string(),
        ];
        let out = rewrite_preserving_flags("claude", &orig, &id).unwrap();
        assert_eq!(
            out,
            vec![
                "claude",
                "--resume",
                "new-id",
                "--permission-mode",
                "plan",
                "--allow-dangerously-skip-permissions",
            ]
        );
    }

    #[test]
    fn claude_resume_replaces_session_id_selector_space_and_eq_forms() {
        let id = AgentSessionRef::id("new-id").unwrap();
        // space form: `claude --session-id OLD --model sonnet --permission-mode plan`
        let space = vec![
            "claude".to_string(),
            "--session-id".to_string(),
            "OLD".to_string(),
            "--model".to_string(),
            "sonnet".to_string(),
            "--permission-mode".to_string(),
            "plan".to_string(),
        ];
        assert_eq!(
            rewrite_preserving_flags("claude", &space, &id).unwrap(),
            vec![
                "claude",
                "--resume",
                "new-id",
                "--model",
                "sonnet",
                "--permission-mode",
                "plan",
            ]
        );
        // `--session-id=OLD` long form, with a preserved policy flag
        let eq = vec![
            "claude".to_string(),
            "--session-id=OLD".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        assert_eq!(
            rewrite_preserving_flags("claude", &eq, &id).unwrap(),
            vec![
                "claude",
                "--resume",
                "new-id",
                "--dangerously-skip-permissions",
            ]
        );
    }

    #[test]
    fn codex_resume_preserves_model_alias_and_eq_value_forms() {
        let id = AgentSessionRef::id("new-id").unwrap();
        assert_eq!(
            rewrite_preserving_flags(
                "codex",
                &[
                    "codex".to_string(),
                    "resume".to_string(),
                    "OLD".to_string(),
                    "--model".to_string(),
                    "gpt-5".to_string(),
                ],
                &id,
            )
            .unwrap(),
            vec!["codex", "resume", "new-id", "--model", "gpt-5"]
        );
        assert_eq!(
            rewrite_preserving_flags(
                "codex",
                &[
                    "codex".to_string(),
                    "-m".to_string(),
                    "gpt-5".to_string(),
                    "resume".to_string(),
                ],
                &id,
            )
            .unwrap(),
            vec!["codex", "resume", "new-id", "-m", "gpt-5"]
        );
        assert_eq!(
            rewrite_preserving_flags(
                "codex",
                &[
                    "codex".to_string(),
                    "resume".to_string(),
                    "--model=gpt-5".to_string(),
                ],
                &id,
            )
            .unwrap(),
            vec!["codex", "resume", "new-id", "--model=gpt-5"]
        );
    }

    #[test]
    fn codex_resume_preserves_sandbox_approval_and_bypass_flags() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "codex".to_string(),
            "--sandbox".to_string(),
            "workspace-write".to_string(),
            "--ask-for-approval".to_string(),
            "on-request".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
            "--dangerously-bypass-hook-trust".to_string(),
            "resume".to_string(),
            "OLD".to_string(),
        ];
        let out = rewrite_preserving_flags("codex", &orig, &id).unwrap();
        assert_eq!(
            out,
            vec![
                "codex",
                "resume",
                "new-id",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
            ]
        );
    }

    #[test]
    fn codex_resume_drops_redundant_picker_flags() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "codex".to_string(),
            "resume".to_string(),
            "--last".to_string(),
            "--yolo".to_string(),
        ];
        let out = rewrite_preserving_flags("codex", &orig, &id).unwrap();
        assert_eq!(out, vec!["codex", "resume", "new-id", "--yolo"]);
    }

    #[test]
    fn pi_resume_preserves_provider_model_thinking_and_tools() {
        let target = pi_target_ref();
        let orig = vec![
            "pi".to_string(),
            "--provider".to_string(),
            "openai".to_string(),
            "--model".to_string(),
            "gpt-5".to_string(),
            "--thinking".to_string(),
            "high".to_string(),
            "--tools".to_string(),
            "read,grep".to_string(),
            "-c".to_string(),
        ];
        let out = rewrite_preserving_flags("pi", &orig, &target).unwrap();
        assert_eq!(
            out,
            vec![
                "pi".to_string(),
                "--provider".to_string(),
                "openai".to_string(),
                "--model".to_string(),
                "gpt-5".to_string(),
                "--thinking".to_string(),
                "high".to_string(),
                "--tools".to_string(),
                "read,grep".to_string(),
                "--session".to_string(),
                target.value.clone(),
            ]
        );
    }

    #[test]
    fn pi_resume_rejects_secret_bearing_api_key() {
        // --api-key is not allowlisted; a command carrying a secret falls back to canonical and is
        // never partially preserved.
        let target = pi_target_ref();
        let orig = vec![
            "pi".to_string(),
            "--api-key".to_string(),
            "sk-secret".to_string(),
            "--model".to_string(),
            "gpt-5".to_string(),
        ];
        assert!(rewrite_preserving_flags("pi", &orig, &target).is_none());
    }

    // --- Tier B: persisted_resume_argv sanitizes a live foreground command (manual shell launch) ---
    #[test]
    fn manual_foreground_claude_preserves_dangerously_skip_permissions() {
        let id = AgentSessionRef::id("X").unwrap();
        let fg = vec![
            "claude".to_string(),
            "--continue".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        assert_eq!(
            persisted_resume_argv("zynk:claude", "claude", &id, Some(&fg), None).unwrap(),
            vec!["claude", "--resume", "X", "--dangerously-skip-permissions"]
        );
    }

    #[test]
    fn manual_foreground_codex_preserves_yolo_and_model() {
        let id = AgentSessionRef::id("X").unwrap();
        let fg = vec![
            "codex".to_string(),
            "resume".to_string(),
            "--yolo".to_string(),
            "--model".to_string(),
            "gpt-5".to_string(),
        ];
        assert_eq!(
            persisted_resume_argv("zynk:codex", "codex", &id, Some(&fg), None).unwrap(),
            vec!["codex", "resume", "X", "--yolo", "--model", "gpt-5"]
        );
    }

    #[test]
    fn manual_foreground_pi_preserves_model_and_thinking() {
        let target = pi_target_ref();
        let fg = vec![
            "pi".to_string(),
            "--continue".to_string(),
            "--model".to_string(),
            "sonnet".to_string(),
            "--thinking".to_string(),
            "high".to_string(),
        ];
        // --continue was at the front, so --session replaces it there (selector-index semantics).
        assert_eq!(
            persisted_resume_argv("zynk:pi", "pi", &target, Some(&fg), None).unwrap(),
            vec![
                "pi".to_string(),
                "--session".to_string(),
                target.value.clone(),
                "--model".to_string(),
                "sonnet".to_string(),
                "--thinking".to_string(),
                "high".to_string(),
            ]
        );
    }

    #[test]
    fn manual_foreground_secret_argv_is_not_persisted() {
        // pi --api-key … -> plan falls back to canonical -> filtered as no-preservation -> None.
        let target = pi_target_ref();
        let fg = vec![
            "pi".to_string(),
            "--api-key".to_string(),
            "sk-secret".to_string(),
            "--model".to_string(),
            "x".to_string(),
        ];
        assert!(persisted_resume_argv("zynk:pi", "pi", &target, Some(&fg), None).is_none());
    }

    #[test]
    fn persisted_resume_argv_prefers_foreground_then_existing_then_none() {
        let id = AgentSessionRef::id("X").unwrap();
        let fg = vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        let canonical_existing = vec![
            "claude".to_string(),
            "--resume".to_string(),
            "OLD".to_string(),
        ];
        // foreground preserves a flag -> foreground wins over a canonical-only existing argv.
        // No `--continue` in fg, so the selector appends at the end (selector-index semantics).
        assert_eq!(
            persisted_resume_argv(
                "zynk:claude",
                "claude",
                &id,
                Some(&fg),
                Some(&canonical_existing)
            )
            .unwrap(),
            vec!["claude", "--dangerously-skip-permissions", "--resume", "X"]
        );
        // no foreground, but existing preserves a flag (Tier A) -> existing used
        let existing_flagged = vec!["claude".to_string(), "--safe-mode".to_string()];
        assert_eq!(
            persisted_resume_argv("zynk:claude", "claude", &id, None, Some(&existing_flagged))
                .unwrap(),
            vec!["claude", "--safe-mode", "--resume", "X"]
        );
        // nothing preserves a flag -> None (snapshot stores nothing; restore rebuilds canonical)
        assert!(persisted_resume_argv(
            "zynk:claude",
            "claude",
            &id,
            None,
            Some(&canonical_existing)
        )
        .is_none());
        assert!(persisted_resume_argv("zynk:claude", "claude", &id, None, None).is_none());
    }

    // --- Adversarial: anything unrecognized falls back to canonical (returns None) ---
    #[test]
    fn claude_resume_inserts_selector_before_double_dash() {
        // A `--` payload could replay user input -> fall back to canonical.
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--".to_string(),
            "write".to_string(),
            "a poem".to_string(),
        ];
        assert!(rewrite_preserving_flags("claude", &orig, &id).is_none());
    }

    #[test]
    fn pi_resume_inserts_session_before_double_dash() {
        let target = pi_target_ref();
        let orig = vec!["pi".to_string(), "--".to_string(), "hello".to_string()];
        assert!(rewrite_preserving_flags("pi", &orig, &target).is_none());
    }

    #[test]
    fn claude_resume_rejects_positional_prompt_payload() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "--continue".to_string(),
            "do-the-thing".to_string(),
        ];
        assert!(rewrite_preserving_flags("claude", &orig, &id).is_none());
    }

    #[test]
    fn pi_resume_rejects_bare_positional_payload() {
        let target = pi_target_ref();
        let orig = vec!["pi".to_string(), "hello".to_string(), "there".to_string()];
        assert!(rewrite_preserving_flags("pi", &orig, &target).is_none());
    }

    #[test]
    fn codex_resume_rejects_non_resume_subcommands() {
        let id = AgentSessionRef::id("new-id").unwrap();
        for sub in ["exec", "completion", "login"] {
            let orig = vec!["codex".to_string(), sub.to_string()];
            assert!(
                rewrite_preserving_flags("codex", &orig, &id).is_none(),
                "codex {sub} must fall back to canonical"
            );
        }
    }

    #[test]
    fn rewrite_falls_back_to_canonical_on_unknown_flag() {
        // Default-deny: an unrecognized flag is NOT blindly preserved.
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "--continue".to_string(),
            "--some-unknown-flag".to_string(),
        ];
        assert!(rewrite_preserving_flags("claude", &orig, &id).is_none());
    }

    #[test]
    fn rewrite_falls_back_to_canonical_on_argv0_mismatch() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec!["python".to_string(), "--continue".to_string()];
        assert!(rewrite_preserving_flags("claude", &orig, &id).is_none());
    }

    #[test]
    fn rewrite_falls_back_to_canonical_on_invalid_token() {
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec!["claude".to_string(), "bad\nflag".to_string()];
        assert!(rewrite_preserving_flags("claude", &orig, &id).is_none());
    }

    #[test]
    fn preserved_value_flags_are_data_not_shell_text() {
        // A recognized value flag is preserved verbatim; shell quoting happens later in
        // shell_command_from_argv, so injection-looking values stay inert.
        let id = AgentSessionRef::id("new-id").unwrap();
        let orig = vec![
            "claude".to_string(),
            "--model".to_string(),
            "weird; rm -rf /".to_string(),
            "-c".to_string(),
        ];
        let out = rewrite_preserving_flags("claude", &orig, &id).unwrap();
        assert_eq!(
            out,
            vec!["claude", "--model", "weird; rm -rf /", "--resume", "new-id"]
        );
    }

    #[test]
    fn planner_allows_supported_agents() {
        let pi_session = absolute_test_path("pi-session.jsonl");
        let omp_session = absolute_test_path("omp-session.jsonl");
        assert_eq!(
            plan(
                "zynk:claude",
                "claude",
                &AgentSessionRef::id("claude-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["claude", "--resume", "claude-session"]
        );
        assert_eq!(
            plan(
                "zynk:codex",
                "codex",
                &AgentSessionRef::id("codex-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["codex", "resume", "codex-session"]
        );
        assert_eq!(
            plan(
                "zynk:copilot",
                "copilot",
                &AgentSessionRef::id("copilot-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["copilot", "--resume=copilot-session"]
        );
        assert_eq!(
            plan(
                "zynk:devin",
                "devin",
                &AgentSessionRef::id("devin-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["devin", "--resume", "devin-session"]
        );
        assert_eq!(
            plan(
                "zynk:droid",
                "droid",
                &AgentSessionRef::id("droid-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["droid", "--resume", "droid-session"]
        );
        assert_eq!(
            plan(
                "zynk:kimi",
                "kimi",
                &AgentSessionRef::id("kimi-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["kimi", "--session", "kimi-session"]
        );
        assert_eq!(
            plan(
                "zynk:pi",
                "pi",
                &AgentSessionRef::path(&pi_session).unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["pi", "--session", pi_session.as_str()]
        );
        assert_eq!(
            plan(
                "zynk:omp",
                "omp",
                &AgentSessionRef::path(&omp_session).unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["omp", format!("--resume={omp_session}").as_str()]
        );
        assert_eq!(
            plan(
                "zynk:hermes",
                "hermes",
                &AgentSessionRef::id("hermes-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["hermes", "--resume", "hermes-session"]
        );
        assert_eq!(
            plan(
                "zynk:opencode",
                "opencode",
                &AgentSessionRef::id("opencode-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["opencode", "--session", "opencode-session"]
        );
        assert_eq!(
            plan(
                "zynk:qodercli",
                "qodercli",
                &AgentSessionRef::id("qoder-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["qodercli", "--resume", "qoder-session"]
        );
        assert_eq!(
            plan(
                "zynk:kilo",
                "kilo",
                &AgentSessionRef::id("kilo-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["kilo", "--session", "kilo-session"]
        );
        assert_eq!(
            plan(
                "zynk:cursor",
                "cursor",
                &AgentSessionRef::id("cursor-session").unwrap(),
                None,
            )
            .unwrap()
            .argv,
            vec!["cursor-agent", "--resume", "cursor-session"]
        );
    }

    #[test]
    fn planner_rejects_custom_and_unsupported_path_refs() {
        let claude_session = absolute_test_path("claude-session");
        assert!(plan(
            "custom:claude",
            "claude",
            &AgentSessionRef::id("session").unwrap(),
            None,
        )
        .is_none());
        assert!(plan(
            "zynk:claude",
            "claude",
            &AgentSessionRef::path(&claude_session).unwrap(),
            None,
        )
        .is_none());
    }

    #[test]
    fn report_ref_prefers_pi_and_omp_paths_and_validates_values() {
        let pi_session = absolute_test_path("pi-session.jsonl");
        let omp_session = absolute_test_path("omp-session.jsonl");
        let claude_session = absolute_test_path("claude-session");
        let copilot_session = absolute_test_path("copilot-session");
        let session_ref = session_ref_from_report(
            "zynk:pi",
            "pi",
            Some("pi-id".into()),
            Some(pi_session.clone()),
        )
        .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Path);
        assert_eq!(session_ref.value, pi_session);

        assert!(session_ref_from_report("zynk:pi", "pi", Some("bad\nid".into()), None).is_none());
        assert!(
            session_ref_from_report("zynk:pi", "pi", None, Some("relative.jsonl".into())).is_none()
        );
        assert!(session_ref_from_report("custom:pi", "pi", Some("pi-id".into()), None).is_none());

        let session_ref = session_ref_from_report(
            "zynk:omp",
            "omp",
            Some("omp-id".into()),
            Some(omp_session.clone()),
        )
        .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Path);
        assert_eq!(session_ref.value, omp_session);

        let session_ref =
            session_ref_from_report("zynk:omp", "omp", Some("omp-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "omp-id");
        let session_ref = session_ref_from_report(
            "zynk:omp",
            "omp",
            Some("omp-id".into()),
            Some("relative.jsonl".into()),
        )
        .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "omp-id");
        assert!(
            session_ref_from_report("zynk:omp", "omp", None, Some("relative.jsonl".into()))
                .is_none()
        );

        assert!(
            session_ref_from_report("zynk:claude", "claude", None, Some(claude_session)).is_none()
        );

        let session_ref =
            session_ref_from_report("zynk:copilot", "copilot", Some("copilot-id".into()), None)
                .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "copilot-id");
        assert!(
            session_ref_from_report("zynk:copilot", "copilot", None, Some(copilot_session))
                .is_none()
        );

        let session_ref =
            session_ref_from_report("zynk:devin", "devin", Some("devin-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "devin-id");

        let session_ref =
            session_ref_from_report("zynk:droid", "droid", Some("droid-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "droid-id");
        assert!(session_ref_from_report(
            "zynk:droid",
            "droid",
            None,
            Some("/tmp/droid-session".into())
        )
        .is_none());

        let session_ref =
            session_ref_from_report("zynk:kimi", "kimi", Some("kimi-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "kimi-id");

        let session_ref =
            session_ref_from_report("zynk:kilo", "kilo", Some("kilo-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "kilo-id");

        let session_ref =
            session_ref_from_report("zynk:qodercli", "qodercli", Some("qoder-id".into()), None)
                .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "qoder-id");
    }

    #[test]
    fn normalize_claude_session_start_source_allows_known_claude_values() {
        assert_eq!(
            normalize_claude_session_start_source(Some("startup".into())),
            Some("startup".into())
        );
        assert_eq!(
            normalize_claude_session_start_source(Some("resume".into())),
            Some("resume".into())
        );
        assert_eq!(
            normalize_claude_session_start_source(Some("clear".into())),
            Some("clear".into())
        );
        assert_eq!(
            normalize_claude_session_start_source(Some("compact".into())),
            Some("compact".into())
        );
        assert_eq!(
            normalize_claude_session_start_source(Some(" resume ".into())),
            Some("resume".into())
        );
        assert_eq!(
            normalize_claude_session_start_source(Some("other".into())),
            None
        );
        assert_eq!(normalize_claude_session_start_source(None), None);
    }

    #[test]
    fn ids_are_data_not_shell_text() {
        let id = "abc; rm -rf /";
        let codex_plan = plan(
            "zynk:codex",
            "codex",
            &AgentSessionRef::id(id).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(codex_plan.argv, vec!["codex", "resume", id]);

        let copilot_plan = plan(
            "zynk:copilot",
            "copilot",
            &AgentSessionRef::id(id).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(copilot_plan.argv, vec!["copilot", "--resume=abc; rm -rf /"]);

        let devin_plan = plan(
            "zynk:devin",
            "devin",
            &AgentSessionRef::id(id).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(devin_plan.argv, vec!["devin", "--resume", id]);
    }

    #[test]
    fn planner_rejects_path_refs_for_id_only_agents() {
        let hermes_session = absolute_test_path("hermes-session");
        let opencode_session = absolute_test_path("opencode-session");
        let kilo_session = absolute_test_path("kilo-session");
        let copilot_session = absolute_test_path("copilot-session");
        let devin_session = absolute_test_path("devin-session");
        assert!(plan(
            "zynk:hermes",
            "hermes",
            &AgentSessionRef::path(&hermes_session).unwrap(),
            None,
        )
        .is_none());
        assert!(plan(
            "zynk:opencode",
            "opencode",
            &AgentSessionRef::path(&opencode_session).unwrap(),
            None,
        )
        .is_none());
        assert!(plan(
            "zynk:kilo",
            "kilo",
            &AgentSessionRef::path(&kilo_session).unwrap(),
            None,
        )
        .is_none());
        assert!(plan(
            "zynk:copilot",
            "copilot",
            &AgentSessionRef::path(&copilot_session).unwrap(),
            None,
        )
        .is_none());
        assert!(plan(
            "zynk:devin",
            "devin",
            &AgentSessionRef::path(&devin_session).unwrap(),
            None,
        )
        .is_none());
        assert!(session_ref_from_snapshot(
            "zynk:hermes",
            "hermes",
            AgentSessionRefKind::Id,
            "hermes-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "zynk:opencode",
            "opencode",
            AgentSessionRefKind::Id,
            "opencode-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "zynk:kilo",
            "kilo",
            AgentSessionRefKind::Id,
            "kilo-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "zynk:copilot",
            "copilot",
            AgentSessionRefKind::Id,
            "copilot-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "zynk:devin",
            "devin",
            AgentSessionRefKind::Id,
            "devin-session"
        )
        .is_some());
    }
}
