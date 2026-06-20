//! Native `zynk skill` install/status.
//!
//! Writes the embedded root `SKILL.md` (source of truth) into per-agent skill
//! directories. Status is decided by two independent signals: ownership via the
//! `<!-- zynk-skill-version: N -->` marker, and freshness via sha256 byte
//! identity against the embedded asset (the version number is display-only).
//!
//! Only agents with an FS-verified, clearly-pathed skill directory are
//! first-class install/status targets (claude, pi, codex). Every other known
//! agent is reported `unsupported` with a reason and is never written to.

use std::io;
use std::path::{Path, PathBuf};

use crate::config_dir::config_dir_from_env_or_home;
use crate::zynk::message::lowercase_hex_sha256;

/// The embedded source-of-truth skill (repo root `SKILL.md`).
const SKILL_ASSET: &str = include_str!("../../SKILL.md");
/// Ownership marker prefix; full form is `<!-- zynk-skill-version: N -->`.
const SKILL_VERSION_MARKER: &str = "<!-- zynk-skill-version:";
/// Installed file name and the managed subdirectory under each agent's config dir.
const SKILL_FILE_NAME: &str = "SKILL.md";
const SKILL_SUBDIR: &str = "skills";
const SKILL_NAME: &str = "zynk";

// Per-agent config-dir env overrides (external contracts; declared locally so
// this module does not depend on `integration` internals).
const CLAUDE_CONFIG_DIR_ENV_VAR: &str = "CLAUDE_CONFIG_DIR";
const PI_CODING_AGENT_DIR_ENV_VAR: &str = "PI_CODING_AGENT_DIR";
const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";

/// Every agent zynk knows about, whether or not it supports a reusable skill dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillAgent {
    Claude,
    Pi,
    Codex,
    Omp,
    Copilot,
    Devin,
    Droid,
    Kimi,
    Opencode,
    Kilo,
    Hermes,
    Qodercli,
    Cursor,
}

impl SkillAgent {
    /// The full known-agent registry, in display order (supported first).
    pub(crate) const ALL: [SkillAgent; 13] = [
        SkillAgent::Claude,
        SkillAgent::Pi,
        SkillAgent::Codex,
        SkillAgent::Omp,
        SkillAgent::Copilot,
        SkillAgent::Devin,
        SkillAgent::Droid,
        SkillAgent::Kimi,
        SkillAgent::Opencode,
        SkillAgent::Kilo,
        SkillAgent::Hermes,
        SkillAgent::Qodercli,
        SkillAgent::Cursor,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SkillAgent::Claude => "claude",
            SkillAgent::Pi => "pi",
            SkillAgent::Codex => "codex",
            SkillAgent::Omp => "omp",
            SkillAgent::Copilot => "copilot",
            SkillAgent::Devin => "devin",
            SkillAgent::Droid => "droid",
            SkillAgent::Kimi => "kimi",
            SkillAgent::Opencode => "opencode",
            SkillAgent::Kilo => "kilo",
            SkillAgent::Hermes => "hermes",
            SkillAgent::Qodercli => "qodercli",
            SkillAgent::Cursor => "cursor",
        }
    }

    pub(crate) fn from_label(label: &str) -> Option<SkillAgent> {
        SkillAgent::ALL
            .into_iter()
            .find(|agent| agent.label() == label)
    }

    pub(crate) fn is_supported(self) -> bool {
        self.supported_dir_spec().is_some()
    }

    /// `(env override var, home-relative segments)` for the agent's config dir,
    /// or `None` if the agent has no FS-verified reusable skill directory (v1).
    fn supported_dir_spec(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            SkillAgent::Claude => Some((CLAUDE_CONFIG_DIR_ENV_VAR, &[".claude"])),
            SkillAgent::Pi => Some((PI_CODING_AGENT_DIR_ENV_VAR, &[".pi", "agent"])),
            SkillAgent::Codex => Some((CODEX_HOME_ENV_VAR, &[".codex"])),
            _ => None,
        }
    }

    /// Why an unsupported agent is not a v1 target (None for supported agents).
    pub(crate) fn unsupported_reason(self) -> Option<&'static str> {
        let reason = match self {
            SkillAgent::Claude | SkillAgent::Pi | SkillAgent::Codex => return None,
            SkillAgent::Omp => "no verified reusable skill directory; config dir ~/.omp/agent",
            SkillAgent::Copilot => "no verified reusable skill directory; config dir ~/.copilot",
            SkillAgent::Devin => "no verified reusable skill directory; config dir ~/.config/devin",
            SkillAgent::Droid => "no verified reusable skill directory; config dir ~/.factory",
            SkillAgent::Kimi => "no verified reusable skill directory; config dir ~/.kimi-code",
            SkillAgent::Opencode => {
                "no verified reusable skill directory; config dir ~/.config/opencode"
            }
            SkillAgent::Kilo => "no verified reusable skill directory; config dir ~/.config/kilo",
            SkillAgent::Hermes => "no verified reusable skill directory; config dir ~/.hermes",
            SkillAgent::Qodercli => "no verified reusable skill directory; config dir ~/.qoder",
            SkillAgent::Cursor => {
                "no verified reusable skill directory; uses project .cursor/rules, not a global skill dir"
            }
        };
        Some(reason)
    }

    /// Resolve `<base>/skills/zynk/SKILL.md` for a supported agent. `None` =>
    /// unsupported; `Some(Err)` => supported but the base dir is unresolvable.
    fn skill_file_path(self) -> Option<io::Result<PathBuf>> {
        let (env_var, segments) = self.supported_dir_spec()?;
        Some(config_dir_from_env_or_home(env_var, segments).map(|base| {
            base.join(SKILL_SUBDIR)
                .join(SKILL_NAME)
                .join(SKILL_FILE_NAME)
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillStatusKind {
    Current,
    Outdated,
    NotInstalled,
    ConflictCustom,
    Unsupported,
}

impl SkillStatusKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SkillStatusKind::Current => "current",
            SkillStatusKind::Outdated => "outdated",
            SkillStatusKind::NotInstalled => "not-installed",
            SkillStatusKind::ConflictCustom => "conflict-custom",
            SkillStatusKind::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SkillStatus {
    pub agent: SkillAgent,
    pub supported: bool,
    pub path: Option<PathBuf>,
    pub state: SkillStatusKind,
    pub managed: bool,
    pub installed_version: Option<u32>,
    pub installed_hash: Option<String>,
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SkillInstallOutcome {
    AlreadyCurrent(PathBuf),
    Installed(PathBuf),
    Updated(PathBuf),
    BackedUpAndReplaced { path: PathBuf, backup: PathBuf },
}

/// The version the embedded asset declares (`None` if the marker is missing).
pub(crate) fn expected_version() -> Option<u32> {
    parse_skill_version(SKILL_ASSET)
}

/// sha256 of the embedded asset — the freshness reference.
pub(crate) fn expected_hash() -> String {
    lowercase_hex_sha256(SKILL_ASSET.as_bytes())
}

fn parse_skill_version(content: &str) -> Option<u32> {
    content.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(SKILL_VERSION_MARKER)?;
        rest.trim().strip_suffix("-->")?.trim().parse().ok()
    })
}

fn is_managed(content: &str) -> bool {
    content.contains(SKILL_VERSION_MARKER)
}

/// Status for every known agent (the registry view).
pub(crate) fn skill_statuses() -> Vec<SkillStatus> {
    SkillAgent::ALL.into_iter().map(skill_status).collect()
}

/// Status for one agent.
pub(crate) fn skill_status(agent: SkillAgent) -> SkillStatus {
    let unsupported = |reason: Option<&'static str>| SkillStatus {
        agent,
        supported: false,
        path: None,
        state: SkillStatusKind::Unsupported,
        managed: false,
        installed_version: None,
        installed_hash: None,
        reason,
    };

    let Some(path_result) = agent.skill_file_path() else {
        return unsupported(agent.unsupported_reason());
    };
    // Supported agent, but the base dir could not be resolved (e.g. no HOME).
    let Ok(path) = path_result else {
        return unsupported(Some("could not resolve the agent config directory"));
    };

    let base = SkillStatus {
        agent,
        supported: true,
        path: Some(path.clone()),
        state: SkillStatusKind::NotInstalled,
        managed: false,
        installed_version: None,
        installed_hash: None,
        reason: None,
    };

    if !path.is_file() {
        return base;
    }

    // An existing-but-unreadable file is treated as conflict-custom so install
    // refuses to clobber it.
    let Ok(content) = std::fs::read_to_string(&path) else {
        return SkillStatus {
            state: SkillStatusKind::ConflictCustom,
            reason: Some("existing file could not be read"),
            ..base
        };
    };

    let installed_hash = lowercase_hex_sha256(content.as_bytes());
    if !is_managed(&content) {
        return SkillStatus {
            state: SkillStatusKind::ConflictCustom,
            managed: false,
            installed_hash: Some(installed_hash),
            ..base
        };
    }

    let state = if installed_hash == expected_hash() {
        SkillStatusKind::Current
    } else {
        SkillStatusKind::Outdated
    };
    SkillStatus {
        state,
        managed: true,
        installed_version: parse_skill_version(&content),
        installed_hash: Some(installed_hash),
        ..base
    }
}

/// Install (or update) the managed skill for a supported agent.
///
/// No-clobber: a `conflict-custom` file is never overwritten without `force`,
/// and `force` backs it up to a unique, content-addressed name first.
pub(crate) fn install_skill(agent: SkillAgent, force: bool) -> io::Result<SkillInstallOutcome> {
    let Some(path_result) = agent.skill_file_path() else {
        return Err(io::Error::other(format!(
            "{} does not support `zynk skill install` (no known skill directory); supported: claude, pi, codex",
            agent.label()
        )));
    };
    let path = path_result?;
    let status = skill_status(agent);

    match status.state {
        SkillStatusKind::Current => Ok(SkillInstallOutcome::AlreadyCurrent(path)),
        SkillStatusKind::NotInstalled => {
            write_managed_skill(&path)?;
            Ok(SkillInstallOutcome::Installed(path))
        }
        SkillStatusKind::Outdated => {
            write_managed_skill(&path)?;
            Ok(SkillInstallOutcome::Updated(path))
        }
        SkillStatusKind::ConflictCustom => {
            if !force {
                return Err(io::Error::other(format!(
                    "{} already has a non-zynk file at {}; refusing to overwrite. re-run with --force to back it up and replace it",
                    agent.label(),
                    path.display()
                )));
            }
            let backup = backup_existing(&path)?;
            write_managed_skill(&path)?;
            Ok(SkillInstallOutcome::BackedUpAndReplaced { path, backup })
        }
        SkillStatusKind::Unsupported => Err(io::Error::other(format!(
            "{} does not support `zynk skill install`",
            agent.label()
        ))),
    }
}

/// Create the managed `skills/zynk/` directory and atomically write the asset.
fn write_managed_skill(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(path, SKILL_ASSET)
}

/// Write to a sibling temp file then rename over the target (no partial files).
fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    let tmp = sibling_name(path, &format!("{SKILL_FILE_NAME}.tmp"))?;
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Back up an existing file to a unique, content-addressed name that never
/// overwrites a different prior backup.
fn backup_existing(path: &Path) -> io::Result<PathBuf> {
    let existing = std::fs::read(path)?;
    let sha8 = &lowercase_hex_sha256(&existing)[..8];
    let base_name = format!("{SKILL_FILE_NAME}.bak-{sha8}");

    let candidate = sibling_name(path, &base_name)?;
    if backup_slot_is_free_or_identical(&candidate, &existing)? {
        std::fs::write(&candidate, &existing)?;
        return Ok(candidate);
    }
    for n in 1..1000 {
        let candidate = sibling_name(path, &format!("{base_name}-{n}"))?;
        if backup_slot_is_free_or_identical(&candidate, &existing)? {
            std::fs::write(&candidate, &existing)?;
            return Ok(candidate);
        }
    }
    Err(io::Error::other(
        "could not find a free backup name; refusing to overwrite an existing backup",
    ))
}

/// A backup slot is usable if it does not exist, or already holds the exact
/// same bytes (idempotent). A slot holding *different* bytes is never reused.
fn backup_slot_is_free_or_identical(candidate: &Path, bytes: &[u8]) -> io::Result<bool> {
    if !candidate.exists() {
        return Ok(true);
    }
    Ok(std::fs::read(candidate)? == bytes)
}

fn sibling_name(path: &Path, file_name: &str) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("skill path has no parent directory"))?;
    Ok(parent.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn unique_base() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("zynk-skill-test-{}-{n}", std::process::id()))
    }

    /// Point every supported agent's config-dir env override at `base/<dir>`.
    fn redirect_supported_dirs(base: &Path) {
        std::env::set_var(CLAUDE_CONFIG_DIR_ENV_VAR, base.join("claude"));
        std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, base.join("pi-agent"));
        std::env::set_var(CODEX_HOME_ENV_VAR, base.join("codex"));
    }

    fn clear_supported_dirs() {
        std::env::remove_var(CLAUDE_CONFIG_DIR_ENV_VAR);
        std::env::remove_var(PI_CODING_AGENT_DIR_ENV_VAR);
        std::env::remove_var(CODEX_HOME_ENV_VAR);
    }

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn claude_not_installed_when_target_missing() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);

        let status = skill_status(SkillAgent::Claude);
        assert!(status.supported);
        assert_eq!(status.state, SkillStatusKind::NotInstalled);
        assert_eq!(
            status.path.unwrap(),
            base.join("claude/skills/zynk/SKILL.md")
        );

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn install_writes_managed_file_and_is_current() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);

        let outcome = install_skill(SkillAgent::Claude, false).unwrap();
        let path = base.join("claude/skills/zynk/SKILL.md");
        assert_eq!(outcome, SkillInstallOutcome::Installed(path.clone()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_ASSET);

        let status = skill_status(SkillAgent::Claude);
        assert_eq!(status.state, SkillStatusKind::Current);
        assert_eq!(status.installed_version, expected_version());

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn install_is_idempotent() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);

        install_skill(SkillAgent::Pi, false).unwrap();
        let again = install_skill(SkillAgent::Pi, false).unwrap();
        let path = base.join("pi-agent/skills/zynk/SKILL.md");
        assert_eq!(again, SkillInstallOutcome::AlreadyCurrent(path));
        assert_eq!(skill_status(SkillAgent::Pi).state, SkillStatusKind::Current);

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn outdated_when_managed_but_content_differs() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);
        let path = base.join("codex/skills/zynk/SKILL.md");
        // Managed (has the marker) but stale content.
        write_file(&path, "<!-- zynk-skill-version: 1 -->\nold body\n");

        let status = skill_status(SkillAgent::Codex);
        assert_eq!(status.state, SkillStatusKind::Outdated);
        assert!(status.managed);
        assert_eq!(status.installed_version, Some(1));

        let outcome = install_skill(SkillAgent::Codex, false).unwrap();
        assert_eq!(outcome, SkillInstallOutcome::Updated(path.clone()));
        assert_eq!(
            skill_status(SkillAgent::Codex).state,
            SkillStatusKind::Current
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_ASSET);

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn conflict_custom_when_marker_absent_refuses_without_force() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);
        let path = base.join("codex/skills/zynk/SKILL.md");
        // Mirrors the real stale ~/.codex/skills/zynk: no marker + herdr text.
        let custom = "---\nname: zynk\n---\n# Zynk\nrun herdr send ...\n";
        write_file(&path, custom);

        let status = skill_status(SkillAgent::Codex);
        assert_eq!(status.state, SkillStatusKind::ConflictCustom);
        assert!(!status.managed);

        let err = install_skill(SkillAgent::Codex, false).unwrap_err();
        assert!(err.to_string().contains("--force"));
        // File is untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), custom);

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn force_backup_is_unique_and_never_overwrites_existing() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);
        let path = base.join("codex/skills/zynk/SKILL.md");
        let dir = path.parent().unwrap().to_path_buf();

        // First custom -> force -> backup A holds the first bytes.
        let custom_a = "custom A (no marker)\n";
        write_file(&path, custom_a);
        let outcome_a = install_skill(SkillAgent::Codex, true).unwrap();
        let backup_a = match outcome_a {
            SkillInstallOutcome::BackedUpAndReplaced { backup, .. } => backup,
            other => panic!("expected BackedUpAndReplaced, got {other:?}"),
        };
        assert_eq!(std::fs::read_to_string(&backup_a).unwrap(), custom_a);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_ASSET);

        // A DIFFERENT custom -> force again -> a distinct backup B; A is untouched.
        let custom_b = "custom B (different, no marker)\n";
        std::fs::write(&path, custom_b).unwrap();
        let outcome_b = install_skill(SkillAgent::Codex, true).unwrap();
        let backup_b = match outcome_b {
            SkillInstallOutcome::BackedUpAndReplaced { backup, .. } => backup,
            other => panic!("expected BackedUpAndReplaced, got {other:?}"),
        };
        assert_ne!(backup_a, backup_b);
        assert_eq!(std::fs::read_to_string(&backup_a).unwrap(), custom_a);
        assert_eq!(std::fs::read_to_string(&backup_b).unwrap(), custom_b);

        // No plain SKILL.md.bak was ever produced.
        assert!(!dir.join("SKILL.md.bak").exists());

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn install_does_not_clobber_unrelated_skill_dirs() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);
        let other = base.join("claude/skills/other/SKILL.md");
        write_file(&other, "other skill\n");

        install_skill(SkillAgent::Claude, false).unwrap();
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "other skill\n");

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn supported_target_paths_are_exact() {
        let _lock = env_lock();
        let base = unique_base();
        redirect_supported_dirs(&base);

        assert_eq!(
            skill_status(SkillAgent::Claude).path.unwrap(),
            base.join("claude/skills/zynk/SKILL.md")
        );
        assert_eq!(
            skill_status(SkillAgent::Pi).path.unwrap(),
            base.join("pi-agent/skills/zynk/SKILL.md")
        );
        assert_eq!(
            skill_status(SkillAgent::Codex).path.unwrap(),
            base.join("codex/skills/zynk/SKILL.md")
        );

        clear_supported_dirs();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unsupported_agents_report_reason_and_install_rejects() {
        let _lock = env_lock();
        let unsupported = [
            SkillAgent::Omp,
            SkillAgent::Copilot,
            SkillAgent::Devin,
            SkillAgent::Droid,
            SkillAgent::Kimi,
            SkillAgent::Opencode,
            SkillAgent::Kilo,
            SkillAgent::Hermes,
            SkillAgent::Qodercli,
            SkillAgent::Cursor,
        ];
        assert_eq!(unsupported.len(), 10);
        for agent in unsupported {
            let status = skill_status(agent);
            assert!(!status.supported, "{} should be unsupported", agent.label());
            assert_eq!(status.state, SkillStatusKind::Unsupported);
            assert!(status.path.is_none());
            assert!(status.reason.is_some());
            let err = install_skill(agent, true).unwrap_err();
            assert!(err.to_string().contains("does not support"));
        }
    }

    #[test]
    fn registry_has_three_supported_and_ten_unsupported() {
        let supported = SkillAgent::ALL
            .into_iter()
            .filter(|a| a.is_supported())
            .count();
        assert_eq!(supported, 3);
        assert_eq!(SkillAgent::ALL.len(), 13);
    }

    #[test]
    fn embedded_skill_asset_is_clean() {
        assert!(
            !SKILL_ASSET.to_lowercase().contains("herdr"),
            "embedded skill must not mention herdr"
        );
        assert!(!SKILL_ASSET.contains("HERDR_"));
        assert!(
            is_managed(SKILL_ASSET),
            "embedded skill must carry the marker"
        );
        assert!(expected_version().is_some());
        // Current ids/commands, not pre-rebrand.
        for token in ["w1:p", "ZYNK_ENV=1", "zynk send", "zynk reply"] {
            assert!(
                SKILL_ASSET.contains(token),
                "embedded skill must contain `{token}`"
            );
        }
    }

    #[test]
    fn parse_skill_version_reads_the_marker() {
        assert_eq!(
            parse_skill_version("<!-- zynk-skill-version: 2 -->"),
            Some(2)
        );
        assert_eq!(
            parse_skill_version("text\n  <!-- zynk-skill-version: 7 -->\nmore"),
            Some(7)
        );
        assert_eq!(parse_skill_version("no marker here"), None);
    }
}
