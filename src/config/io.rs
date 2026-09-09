use std::path::{Path, PathBuf};

use tracing::warn;

use super::{model::LoadedConfig, Config, CONFIG_PATH_ENV_VAR, ZYNK_CONFIG_PATH_ENV_VAR};

const KNOWN_TOP_LEVEL_CONFIG_KEYS: &[&str] = &[
    "advanced",
    "experimental",
    // Feature #107 (IM3): the `[header]` section is CLIENT-SIDE — it is read per-send by
    // `resolve_header_options()` at header-render time, not applied to the running server.
    // Registered here so `Config::load` and `load_live_config_from_str` do NOT emit a
    // bogus "unknown top-level section" warning for it.
    "header",
    "keys",
    "onboarding",
    "remote",
    "session",
    "terminal",
    "theme",
    "ui",
    "update",
    "worktrees",
    "zynk",
];

pub fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "zynk-dev"
    } else {
        "zynk"
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_config_dir()
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_state_dir()
}

fn platform_config_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".config/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(app_dir_name())
    }
}

fn platform_state_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".local/state/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(format!("{}-state", app_dir_name()))
    }
}

/// Reads the config file, distinguishing "there is no config" from "there is a config we
/// could not read". A prior `path.exists()` guard answered both with `false` whenever the
/// path could not be stat'ed at all — an unreadable parent directory or a symlink loop —
/// so a real config silently fell back to defaults with no diagnostic at all.
fn read_optional_config(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

impl Config {
    pub fn load() -> LoadedConfig {
        let path = config_path();
        let content = match read_optional_config(&path) {
            Ok(Some(content)) => content,
            Ok(None) => {
                return LoadedConfig {
                    config: Self::default(),
                    diagnostics: Vec::new(),
                    invalid_sections: Vec::new(),
                };
            }
            Err(err) => {
                warn!(err = %err, "config read error, using defaults");
                return LoadedConfig {
                    config: Self::default(),
                    diagnostics: vec![format!("config read error: {err}; using defaults")],
                    invalid_sections: Vec::new(),
                };
            }
        };
        load_config_from_str(&content)
    }
}

/// Startup path: parse the whole file through the typed model and report every key the
/// model ignored, alongside the existing unknown-section and removed-key diagnostics.
fn load_config_from_str(content: &str) -> LoadedConfig {
    match deserialize_with_ignored::<Config, _>(toml::Deserializer::new(content)) {
        Ok((config, ignored_keys)) => {
            let (unknown_sections, mut diagnostics) = unknown_top_level_sections_from_str(content);
            diagnostics.extend(removed_config_key_diagnostics_from_str(content));
            // A whole unknown top-level table is already reported as a section; do not
            // report it a second time as a key.
            diagnostics.extend(unknown_config_key_diagnostics(
                ignored_keys
                    .into_iter()
                    .filter(|path| {
                        !matches!(
                            path.as_slice(),
                            [ConfigKeyPathSegment::Key(key)] if unknown_sections.contains(key)
                        )
                    })
                    .collect(),
                None,
            ));
            diagnostics.extend(config.collect_diagnostics());
            LoadedConfig {
                config,
                diagnostics,
                invalid_sections: Vec::new(),
            }
        }
        Err(err) => {
            warn!(err = %err, "config parse error, using defaults");
            LoadedConfig {
                config: Config::default(),
                diagnostics: vec![format!("config parse error: {err}; using defaults")],
                invalid_sections: Vec::new(),
            }
        }
    }
}

pub(super) fn resolve_config_relative_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(path)
}

pub fn config_path() -> PathBuf {
    // ADR 0007 §5: ZYNK_CONFIG_PATH is primary; ZYNK_CONFIG_PATH is the retained
    // transitional compat alias. ZYNK wins when both are set.
    if let Some(path) = super::env_first(&[ZYNK_CONFIG_PATH_ENV_VAR, CONFIG_PATH_ENV_VAR]) {
        return PathBuf::from(path);
    }
    config_dir().join("config.toml")
}

pub fn config_diagnostic_summary(diagnostics: &[String]) -> Option<String> {
    const MAX_VISIBLE_DIAGNOSTICS: usize = 4;

    if diagnostics.is_empty() {
        return None;
    }

    let mut lines: Vec<String> = diagnostics
        .iter()
        .take(MAX_VISIBLE_DIAGNOSTICS)
        .map(|diagnostic| diagnostic.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let hidden = diagnostics.len().saturating_sub(MAX_VISIBLE_DIAGNOSTICS);
    if hidden > 0 {
        lines.push(format!("and {hidden} more config warnings"));
    }
    Some(lines.join("\n"))
}

pub fn load_live_config() -> Result<LoadedConfig, Vec<String>> {
    let path = config_path();
    let content = match read_optional_config(&path) {
        Ok(Some(content)) => content,
        Ok(None) => {
            return Ok(LoadedConfig {
                config: Config::default(),
                diagnostics: Vec::new(),
                invalid_sections: Vec::new(),
            });
        }
        Err(err) => {
            return Err(vec![format!(
                "config read error: {err}; keeping current config"
            )]);
        }
    };
    load_live_config_from_str(&content)
}

fn load_live_config_from_str(content: &str) -> Result<LoadedConfig, Vec<String>> {
    let value = content
        .parse::<toml::Value>()
        .map_err(|err| vec![format!("config parse error: {err}; keeping current config")])?;
    let table = value.as_table().ok_or_else(|| {
        vec![
            "config parse error: top-level config must be a table; keeping current config"
                .to_string(),
        ]
    })?;

    let mut config = Config::default();
    let mut diagnostics = unknown_top_level_section_diagnostics(table);
    diagnostics.extend(removed_config_key_diagnostics(table));
    diagnostics.extend(unknown_top_level_config_key_diagnostics(table));
    let mut invalid_sections = Vec::new();

    if let Some(value) = table.get("onboarding") {
        match value.clone().try_into::<Option<bool>>() {
            Ok(onboarding) => config.onboarding = onboarding,
            Err(err) => diagnostics.push(format!(
                "invalid onboarding setting: {err}; keeping current onboarding state"
            )),
        }
    }

    load_live_section(
        table,
        "theme",
        "theme config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.theme = section,
    );
    load_live_section(
        table,
        "keys",
        "keybinding config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.keys = section,
    );
    load_live_section(
        table,
        "terminal",
        "terminal config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.terminal = section,
    );
    load_live_section(
        table,
        "session",
        "session config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.session = section,
    );
    load_live_section(
        table,
        "update",
        "update config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.update = section,
    );
    load_live_section(
        table,
        "ui",
        "ui config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.ui = section,
    );
    load_live_section(
        table,
        "advanced",
        "advanced config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.advanced = section,
    );
    load_live_section(
        table,
        "worktrees",
        "worktree config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.worktrees = section,
    );
    load_live_section(
        table,
        "experimental",
        "experimental config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.experimental = section,
    );
    load_live_section(
        table,
        "remote",
        "remote config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.remote = section,
    );
    load_live_section(
        table,
        "zynk",
        "zynk config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.zynk = section,
    );
    // Feature #107 (IM3): carry `[header]` into the returned config so the parsed
    // section is NOT silently dropped on a live reload. The header box is APPLIED
    // PER-SEND-INVOCATION client-side — the sending CLI loads `Config` + env at
    // header-render time via `resolve_header_options()`, so a server `reload-config`
    // is NOT required for a `[header]` change to take effect on subsequent sends.
    // Carrying it here keeps the loaded config faithful and warning-free; the running
    // server does not consume `config.header` itself.
    load_live_section(
        table,
        "header",
        "header config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.header = section,
    );

    Ok(LoadedConfig {
        config,
        diagnostics,
        invalid_sections,
    })
}

/// Removed config keys that are still likely to be present in older config files. Serde ignores
/// unknown keys, so without this the removal would be silent; the diagnostic makes the migration
/// visible (3.1.0 policy: documented keys may be removed in a minor with a changelog note + this).
const REMOVED_CONFIG_KEYS: &[(&str, &str, &str)] = &[
    (
        "ui",
        "agent_panel_scope",
        "ui.agent_panel_scope is no longer supported (removed in 3.1.0); the agent panel shows all \
         workspaces. ui.agent_panel_sort controls ordering only and does not restore \
         current-workspace filtering; ignoring key",
    ),
    (
        "experimental",
        "switch_ascii_input_source_in_prefix",
        "experimental.switch_ascii_input_source_in_prefix is no longer supported (removed in \
         3.1.0); it switched the macOS host input source during prefix mode and zynk targets \
         Linux only; ignoring key",
    ),
];

fn removed_config_key_diagnostics_from_str(content: &str) -> Vec<String> {
    content
        .parse::<toml::Value>()
        .ok()
        .and_then(|value| value.as_table().map(removed_config_key_diagnostics))
        .unwrap_or_default()
}

fn removed_config_key_diagnostics(table: &toml::map::Map<String, toml::Value>) -> Vec<String> {
    REMOVED_CONFIG_KEYS
        .iter()
        .filter(|(section, key, _)| {
            table
                .get(*section)
                .and_then(toml::Value::as_table)
                .is_some_and(|section| section.contains_key(*key))
        })
        .map(|(_, _, message)| (*message).to_string())
        .collect()
}

/// Returns the unknown top-level section names alongside their diagnostics, so the
/// unknown-key reporter can skip keys it would otherwise double-report.
fn unknown_top_level_sections_from_str(content: &str) -> (Vec<String>, Vec<String>) {
    let Ok(value) = content.parse::<toml::Value>() else {
        return (Vec::new(), Vec::new());
    };
    let Some(table) = value.as_table() else {
        return (Vec::new(), Vec::new());
    };

    let mut keys = Vec::new();
    let mut diagnostics = Vec::new();
    for (key, value) in table {
        if let Some(diagnostic) = unknown_top_level_section_diagnostic(key, value) {
            keys.push(key.clone());
            diagnostics.push(diagnostic);
        }
    }
    (keys, diagnostics)
}

fn unknown_top_level_section_diagnostics(
    table: &toml::map::Map<String, toml::Value>,
) -> Vec<String> {
    table
        .iter()
        .filter_map(|(key, value)| unknown_top_level_section_diagnostic(key, value))
        .collect()
}

fn unknown_top_level_section_diagnostic(key: &str, value: &toml::Value) -> Option<String> {
    if KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(&key) {
        return None;
    }

    let header = if value.is_table() {
        format!("[{key}]")
    } else if value
        .as_array()
        .is_some_and(|items| !items.is_empty() && items.iter().all(toml::Value::is_table))
    {
        format!("[[{key}]]")
    } else {
        return None;
    };

    if key == "toast" {
        Some(format!(
            "unknown config section {header}; did you mean [ui.toast]? ignoring section"
        ))
    } else {
        Some(format!("unknown config section {header}; ignoring section"))
    }
}

/// Top-level scalar/array keys the typed model does not know. `load_live_config_from_str`
/// applies the config section by section, so nothing type-checks the top level for it; this
/// reproduces what `Config::load`'s whole-file typed parse reports via `serde_ignored`.
fn unknown_top_level_config_key_diagnostics(
    table: &toml::map::Map<String, toml::Value>,
) -> Vec<String> {
    let paths = table
        .iter()
        .filter(|(key, value)| {
            !KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(&key.as_str())
                && unknown_top_level_section_diagnostic(key, value).is_none()
        })
        .map(|(key, _)| vec![ConfigKeyPathSegment::Key(key.clone())])
        .collect();
    unknown_config_key_diagnostics(paths, None)
}

/// One step of the path to an ignored key, so it can be reported by its full dotted
/// location (`ui.toast.delivry`, `keys.command.0.descrption`) instead of its leaf name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ConfigKeyPathSegment {
    Key(String),
    Index(usize),
}

fn config_key_path(path: &serde_ignored::Path<'_>) -> Vec<ConfigKeyPathSegment> {
    fn visit(path: &serde_ignored::Path<'_>, segments: &mut Vec<ConfigKeyPathSegment>) {
        match path {
            serde_ignored::Path::Root => {}
            serde_ignored::Path::Seq { parent, index } => {
                visit(parent, segments);
                segments.push(ConfigKeyPathSegment::Index(*index));
            }
            serde_ignored::Path::Map { parent, key } => {
                visit(parent, segments);
                segments.push(ConfigKeyPathSegment::Key(key.clone()));
            }
            serde_ignored::Path::Some { parent }
            | serde_ignored::Path::NewtypeStruct { parent }
            | serde_ignored::Path::NewtypeVariant { parent } => visit(parent, segments),
        }
    }

    let mut segments = Vec::new();
    visit(path, &mut segments);
    segments
}

/// Render a path as the user would write it in TOML: bare when the key is a bare TOML key,
/// quoted otherwise, so `"foo.bar"` is not mistaken for a nested `foo` table.
fn format_config_key_path(path: &[ConfigKeyPathSegment]) -> String {
    path.iter()
        .map(|segment| match segment {
            ConfigKeyPathSegment::Key(key)
                if !key.is_empty()
                    && key.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                    }) =>
            {
                key.clone()
            }
            ConfigKeyPathSegment::Key(key) => toml::Value::String(key.clone()).to_string(),
            ConfigKeyPathSegment::Index(index) => index.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// True when `REMOVED_CONFIG_KEYS` already explains this key. Serde ignores a removed key
/// like any other unknown one, so without this the user would get the migration note AND a
/// generic unknown-key warning for the same line.
fn path_is_removed_config_key(path: &[ConfigKeyPathSegment]) -> bool {
    let [ConfigKeyPathSegment::Key(section), ConfigKeyPathSegment::Key(key)] = path else {
        return false;
    };
    REMOVED_CONFIG_KEYS
        .iter()
        .any(|(removed_section, removed_key, _)| {
            section.as_str() == *removed_section && key.as_str() == *removed_key
        })
}

fn unknown_config_key_diagnostics(
    paths: Vec<Vec<ConfigKeyPathSegment>>,
    section: Option<&str>,
) -> Vec<String> {
    let mut paths: Vec<Vec<ConfigKeyPathSegment>> = paths
        .into_iter()
        .map(|mut path| {
            if let Some(section) = section {
                path.insert(0, ConfigKeyPathSegment::Key(section.to_string()));
            }
            path
        })
        .filter(|path| !path_is_removed_config_key(path))
        .collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            format!(
                "unknown config key {}; ignoring key",
                format_config_key_path(&path)
            )
        })
        .collect()
}

fn deserialize_with_ignored<'de, T, D>(
    deserializer: D,
) -> Result<(T, Vec<Vec<ConfigKeyPathSegment>>), D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    let mut ignored = Vec::new();
    let value = serde_ignored::deserialize(deserializer, |path| {
        ignored.push(config_key_path(&path));
    })?;
    Ok((value, ignored))
}

fn load_live_section<T>(
    table: &toml::map::Map<String, toml::Value>,
    section: &'static str,
    label: &str,
    diagnostics: &mut Vec<String>,
    invalid_sections: &mut Vec<String>,
    apply: impl FnOnce(T),
) where
    T: serde::de::DeserializeOwned,
{
    let Some(value) = table.get(section) else {
        return;
    };

    // An invalid section keeps its current settings, so its ignored keys are discarded
    // with it — reporting them would point at a section the user must fix anyway.
    match deserialize_with_ignored(value.clone()) {
        Ok((section_config, ignored_keys)) => {
            diagnostics.extend(unknown_config_key_diagnostics(ignored_keys, Some(section)));
            apply(section_config);
        }
        Err(err) => {
            diagnostics.push(format!(
                "invalid {label}: {err}; keeping current {section} settings"
            ));
            invalid_sections.push(section.to_string());
        }
    }
}

pub(crate) fn upsert_top_level_bool(content: &str, key: &str, value: bool) -> String {
    let replacement = format!("{key} = {value}");
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    let mut in_section = false;

    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            continue;
        }
        if in_section {
            continue;
        }
        if trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")) {
            *line = replacement.clone();
            return lines.join("\n") + "\n";
        }
    }

    if lines.is_empty() {
        format!("{replacement}\n")
    } else {
        format!("{replacement}\n{}\n", lines.join("\n").trim_end())
    }
}

/// Write a key = value pair in a TOML section (creates section if missing).
pub fn upsert_section_value(content: &str, section: &str, key: &str, value: &str) -> String {
    upsert_section_raw(content, section, key, value)
}

pub fn upsert_section_bool(content: &str, section: &str, key: &str, value: bool) -> String {
    upsert_section_raw(content, section, key, &value.to_string())
}

pub fn remove_section_key(content: &str, section: &str, key: &str) -> String {
    let header = format!("[{section}]");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut in_section = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == header;
            result.push(line.to_string());
            i += 1;
            continue;
        }

        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            i += 1;
            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    result.join("\n") + "\n"
}

pub fn remove_keybinding_config_sections(content: &str) -> (String, bool) {
    let mut result = Vec::new();
    let mut removed = false;
    let mut skipping_key_section = false;
    let mut in_table = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(table_name) = toml_table_header_name(trimmed) {
            in_table = true;
            skipping_key_section = is_keys_table_name(table_name);
            if skipping_key_section {
                removed = true;
                continue;
            }
        } else if skipping_key_section || (!in_table && is_top_level_keys_assignment(trimmed)) {
            removed = true;
            continue;
        }

        result.push(line.to_string());
    }

    let mut updated = result.join("\n");
    if content.ends_with('\n') || !updated.is_empty() {
        updated.push('\n');
    }
    (updated, removed)
}

fn toml_table_header_name(trimmed: &str) -> Option<&str> {
    if let Some(name) = trimmed
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
    {
        return Some(name.trim());
    }
    trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .map(str::trim)
}

fn is_keys_table_name(name: &str) -> bool {
    name == "keys" || name.starts_with("keys.")
}

fn is_top_level_keys_assignment(trimmed: &str) -> bool {
    trimmed.starts_with("keys ") || trimmed.starts_with("keys=") || trimmed.starts_with("keys.")
}

fn upsert_section_raw(content: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{section}]");
    let assignment = format!("{key} = {value}");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut found_section = false;
    let mut inserted = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed == header {
            found_section = true;
            result.push(line.to_string());
            i += 1;

            while i < lines.len() {
                let current = lines[i];
                let current_trimmed = current.trim();
                if current_trimmed.starts_with('[') && current_trimmed.ends_with(']') {
                    if !inserted {
                        result.push(assignment.clone());
                        inserted = true;
                    }
                    break;
                }

                if current_trimmed.starts_with(&format!("{key} "))
                    || current_trimmed.starts_with(&format!("{key}="))
                {
                    result.push(assignment.clone());
                    inserted = true;
                } else {
                    result.push(current.to_string());
                }
                i += 1;
            }

            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    if !found_section {
        if !result.is_empty() && !result.last().is_some_and(|line| line.trim().is_empty()) {
            result.push(String::new());
        }
        result.push(header);
        result.push(assignment);
    } else if !inserted {
        result.push(assignment);
    }

    result.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_uses_zynk_app_name() {
        let _g = crate::config::test_config_env_lock().lock().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/zynk-xdg-test");
        let dir = config_dir();
        std::env::remove_var("XDG_CONFIG_HOME");
        // debug build -> "zynk-dev"; release -> "zynk"
        assert!(
            dir.ends_with("zynk-dev") || dir.ends_with("zynk"),
            "{dir:?}"
        );
    }

    #[test]
    fn upsert_top_level_bool_replaces_existing_value() {
        let content = "onboarding = true\n[keys]\nprefix = \"ctrl+b\"\n";
        let updated = upsert_top_level_bool(content, "onboarding", false);
        assert!(updated.contains("onboarding = false"));
        assert!(!updated.contains("onboarding = true"));
    }

    #[test]
    fn upsert_section_bool_adds_missing_section() {
        let updated = upsert_section_bool("", "ui.toast", "enabled", true);
        assert!(updated.contains("[ui.toast]"));
        assert!(updated.contains("enabled = true"));
    }

    #[test]
    fn remove_section_key_removes_matching_key_from_section() {
        let content =
            "[ui.toast]\nenabled = true\ndelivery = \"zynk\"\n[ui.sound]\nenabled = true\n";
        let updated = remove_section_key(content, "ui.toast", "enabled");
        assert!(!updated.contains("[ui.toast]\nenabled = true"));
        assert!(updated.contains("delivery = \"zynk\""));
        assert!(updated.contains("[ui.sound]\nenabled = true"));
    }

    #[test]
    fn config_diagnostic_summary_keeps_multiple_warnings_visible() {
        let diagnostics = vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
            "five".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("one\ntwo\nthree\nfour\nand 1 more config warnings")
        );
    }

    #[test]
    fn load_live_config_parses_session_section() {
        let loaded = load_live_config_from_str(
            r#"
[session]
resume_agents_on_restore = true
"#,
        )
        .unwrap();

        assert!(loaded.config.session.resume_agents_on_restore);
        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_carries_header_section_without_warning() {
        // Feature #107 (IM3) review fix B: `[header]` is a KNOWN top-level section, so a
        // live reload must NOT warn about it, and the parsed values must survive into the
        // returned config (not be silently dropped).
        let loaded = load_live_config_from_str(
            r#"
[header]
verbose = true
max_width = 72
"#,
        )
        .unwrap();

        assert!(
            loaded.diagnostics.is_empty(),
            "no unknown-section warning for [header]: {:?}",
            loaded.diagnostics
        );
        assert!(loaded.invalid_sections.is_empty());
        assert!(loaded.config.header.verbose);
        assert_eq!(loaded.config.header.max_width, 72);
    }

    #[test]
    fn load_live_config_warns_about_unknown_top_level_sections() {
        let loaded = load_live_config_from_str(
            r#"
[toast]
delivery = "system"

[ui.toast]
delivery = "zynk"
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [toast]; did you mean [ui.toast]? ignoring section"]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Zynk
        );
    }

    #[test]
    fn load_live_config_warns_about_unknown_keys_and_applies_known_siblings() {
        let loaded = load_live_config_from_str(
            r##"
plugin = []

[theme.custom]
accentt = "#ffffff"

[advanced]
scrollback_lines = 42

[keys]
fullscreen = "prefix+z"
new_tabb = "prefix+t"

[[keys.command]]
key = "prefix+g"
command = "git status"
descrption = "status"

[ui]
mouse_capture = false
mouse_captur = true
"foo.bar" = true
"foo.?.bar" = false

[ui.toast]
delivery = "zynk"
delivry = "system"
"##,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec![
                "unknown config key plugin; ignoring key",
                "unknown config key theme.custom.accentt; ignoring key",
                "unknown config key keys.command.0.descrption; ignoring key",
                "unknown config key keys.new_tabb; ignoring key",
                "unknown config key ui.\"foo.?.bar\"; ignoring key",
                "unknown config key ui.\"foo.bar\"; ignoring key",
                "unknown config key ui.mouse_captur; ignoring key",
                "unknown config key ui.toast.delivry; ignoring key",
            ]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(loaded.config.advanced.scrollback_limit_bytes, 42);
        assert!(!loaded.config.ui.mouse_capture);
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Zynk
        );
        assert_eq!(
            loaded.config.keys.zoom,
            super::super::BindingConfig::one("prefix+z")
        );
    }

    #[test]
    fn load_live_config_discards_ignored_keys_from_an_invalid_section() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
mouse_capture = "yes"
mouse_captur = true
"#,
        )
        .unwrap();

        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].contains("invalid ui config"));
        assert!(!loaded.diagnostics[0].starts_with("unknown config key"));
        assert_eq!(loaded.invalid_sections, vec!["ui"]);
    }

    #[test]
    fn load_live_config_warns_about_removed_agent_panel_scope_key() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
agent_panel_scope = "current"
agent_panel_sort = "priority"
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec![
                "ui.agent_panel_scope is no longer supported (removed in 3.1.0); the agent panel \
                 shows all workspaces. ui.agent_panel_sort controls ordering only and does not \
                 restore current-workspace filtering; ignoring key"
            ]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.ui.agent_panel_sort,
            super::super::AgentPanelSortConfig::Priority
        );
    }

    #[test]
    fn load_live_config_warns_about_removed_switch_ascii_input_source_key() {
        let loaded = load_live_config_from_str(
            r#"
[experimental]
switch_ascii_input_source_in_prefix = true
pane_history = true
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec![
                "experimental.switch_ascii_input_source_in_prefix is no longer supported (removed \
                 in 3.1.0); it switched the macOS host input source during prefix mode and zynk \
                 targets Linux only; ignoring key"
            ]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert!(loaded.config.experimental.pane_history);
    }

    #[test]
    fn load_live_config_does_not_warn_without_removed_keys() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
agent_panel_sort = "spaces"
"#,
        )
        .unwrap();

        assert!(loaded.diagnostics.is_empty());
    }

    #[test]
    fn startup_config_load_warns_about_removed_agent_panel_scope_key() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "zynk-config-removed-key-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
[ui]
agent_panel_scope = "all"
"#,
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(
            loaded.diagnostics[0].starts_with("ui.agent_panel_scope is no longer supported"),
            "{:?}",
            loaded.diagnostics
        );

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn startup_config_load_warns_about_unknown_top_level_sections() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "zynk-config-unknown-section-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
[[plugin]]
id = "example"

[ui.toast]
delivery = "system"
"#,
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [[plugin]]; ignoring section"]
        );
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::System
        );

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remove_keybinding_config_sections_removes_keys_tables_only() {
        let content = r#"onboarding = false

[theme]
name = "catppuccin"

[keys]
prefix = "ctrl+a"
new_tab = "c"

[[keys.command]]
key = "g"
command = "lazygit"

[keys.indexed]
tabs = "ctrl"

[ui]
mouse_capture = false
"#;

        let (updated, removed) = remove_keybinding_config_sections(content);

        assert!(removed);
        assert!(updated.contains("onboarding = false"));
        assert!(updated.contains("[theme]\nname = \"catppuccin\""));
        assert!(updated.contains("[ui]\nmouse_capture = false"));
        assert!(!updated.contains("[keys]"));
        assert!(!updated.contains("[[keys.command]]"));
        assert!(!updated.contains("[keys.indexed]"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn remove_keybinding_config_sections_reports_noop_without_keys() {
        let content = "[ui]\nmouse_capture = true\n";
        let (updated, removed) = remove_keybinding_config_sections(content);
        assert!(!removed);
        assert_eq!(updated, content);
    }

    #[test]
    fn load_live_config_warns_about_unknown_top_level_keys_and_ignores_comments() {
        let loaded = load_live_config_from_str(
            r#"
made_up_toplevel = 1
"made.up" = 2
# commented_out_toplevel = true
onboarding = false
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec![
                "unknown config key \"made.up\"; ignoring key",
                "unknown config key made_up_toplevel; ignoring key",
            ]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(loaded.config.onboarding, Some(false));
    }

    #[test]
    fn load_live_config_warns_about_unknown_zynk_key_with_its_full_path() {
        // Decision P2: `[zynk]` is recognized because `ZynkConfig` is a real typed section,
        // NOT via a blanket `zynk.*` suppression — so a typo inside it must still warn.
        let loaded = load_live_config_from_str(
            r#"
[zynk]
sqlite_home = "/somewhere/zynk"
sqlite_hmoe = "/typo"
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config key zynk.sqlite_hmoe; ignoring key"]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.zynk.sqlite_home.as_deref(),
            Some("/somewhere/zynk")
        );
    }

    #[test]
    fn load_live_config_accepts_every_fork_owned_config_key_without_warning() {
        // The fork-owned sections (`[zynk]`, `[header]`, `[ui.toast.zynk]`) are real typed
        // fields, so a full config that uses them must produce no unknown-key diagnostics.
        let loaded = load_live_config_from_str(
            r##"
onboarding = false

[theme]
name = "catppuccin"
auto_switch = true
dark_name = "catppuccin"
light_name = "catppuccin-latte"

[theme.custom]
accent = "#f5c2e7"
panel_bg = "reset"

[terminal]
default_shell = "/bin/zsh"
shell_mode = "non_login"
new_cwd = "home"

[session]
resume_agents_on_restore = true

[update]
channel = "stable"
version_check = false
manifest_check = false

[keys]
prefix = "ctrl+a"
new_workspace = "prefix+m"
zoom = "prefix+z"

[keys.indexed]
tabs = "ctrl"
workspaces = "alt"
agents = "super"

[[keys.command]]
key = "prefix+g"
command = "git status"
type = "shell"
description = "status"

[ui]
sidebar_width = 24
mouse_capture = true
agent_panel_sort = "priority"
accent = "#89b4fa"

[ui.toast]
delivery = "zynk"
delay_seconds = 4

[ui.toast.zynk]
position = "bottom-right"

[ui.toast.clipboard]
enabled = true
position = "top-center"

[ui.sound]
enabled = false

[worktrees]
directory = "~/.zynk/worktrees"

[advanced]
scrollback_limit_bytes = 20000

[experimental]
allow_nested = true
pane_history = true

[remote]
manage_ssh_config = false

[zynk]
sqlite_home = "/somewhere/zynk"

[header]
verbose = true
max_width = 72
"##,
        )
        .unwrap();

        assert!(
            loaded.diagnostics.is_empty(),
            "expected no diagnostics: {:?}",
            loaded.diagnostics
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.zynk.sqlite_home.as_deref(),
            Some("/somewhere/zynk")
        );
        assert!(loaded.config.header.verbose);
        assert_eq!(loaded.config.header.max_width, 72);
        assert_eq!(
            loaded.config.ui.toast.zynk.position,
            super::super::ToastZynkPosition::BottomRight
        );
    }

    #[test]
    fn load_live_config_reports_removed_keys_without_duplicate_unknown_key_warnings() {
        // A key on the removed list is also an unknown key to serde; it must produce
        // exactly its removed-key diagnostic, never a second generic unknown-key warning.
        let loaded = load_live_config_from_str(
            r#"
[ui]
agent_panel_scope = "current"
agent_panel_sort = "priority"

[experimental]
switch_ascii_input_source_in_prefix = true
pane_history = true
"#,
        )
        .unwrap();

        assert_eq!(loaded.diagnostics.len(), 2, "{:?}", loaded.diagnostics);
        assert!(
            loaded.diagnostics[0].starts_with("ui.agent_panel_scope is no longer supported"),
            "{:?}",
            loaded.diagnostics
        );
        assert!(
            loaded.diagnostics[1]
                .starts_with("experimental.switch_ascii_input_source_in_prefix is no longer"),
            "{:?}",
            loaded.diagnostics
        );
        assert!(
            !loaded
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.starts_with("unknown config key")),
            "{:?}",
            loaded.diagnostics
        );
        assert!(loaded.invalid_sections.is_empty());
        assert!(loaded.config.experimental.pane_history);
    }

    #[test]
    fn startup_config_load_warns_about_unknown_keys_with_full_paths() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "zynk-config-unknown-key-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
made_up_toplevel = 1
# commented_out_toplevel = true

[zynk]
sqlite_hmoe = "/typo"

[ui]
mouse_captur = false
"#,
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            loaded.diagnostics,
            vec![
                "unknown config key made_up_toplevel; ignoring key",
                "unknown config key ui.mouse_captur; ignoring key",
                "unknown config key zynk.sqlite_hmoe; ignoring key",
            ]
        );
    }

    #[test]
    fn startup_config_load_reports_removed_key_without_duplicate_unknown_key_warning() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "zynk-config-removed-key-no-dup-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
[ui]
agent_panel_scope = "all"
"#,
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.diagnostics.len(), 1, "{:?}", loaded.diagnostics);
        assert!(
            loaded.diagnostics[0].starts_with("ui.agent_panel_scope is no longer supported"),
            "{:?}",
            loaded.diagnostics
        );
    }

    // The two keys M5-07 registers get the fork's standard new-key pair: the key itself
    // round-trips through the live loader, and a misspelled sibling in the same section is
    // reported with its FULL path so a typo is never mistaken for the real key.
    #[test]
    fn load_live_config_registers_copy_on_select_and_reports_misspelled_sibling() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
copy_on_select = false
copy_on_selectt = false
"#,
        )
        .unwrap();

        assert!(!loaded.config.ui.copy_on_select);
        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config key ui.copy_on_selectt; ignoring key"]
        );
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn config_loaders_report_unreadable_path() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path =
            std::env::temp_dir().join(format!("zynk-config-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let startup = Config::load();
        let reload = load_live_config();

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(&path);

        assert!(startup
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("config read error")
                && diagnostic.contains("using defaults")));
        assert!(reload.unwrap_err().iter().any(|diagnostic| {
            diagnostic.contains("config read error")
                && diagnostic.contains("keeping current config")
        }));
    }

    /// A config path that cannot be stat'ed at all is NOT a missing config. The old
    /// `path.exists()` guard answered `false` for both and dropped the user's real config
    /// on the floor without a single diagnostic.
    #[test]
    fn config_loaders_report_an_unstattable_path_instead_of_silently_using_defaults() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("zynk-config-unstattable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let loop_target = dir.join("config-loop.toml");
        std::os::unix::fs::symlink(&loop_target, &path).unwrap();
        std::os::unix::fs::symlink(&path, &loop_target).unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        assert!(!path.exists(), "the loop must defeat a stat-based guard");
        let startup = Config::load();
        let reload = load_live_config();

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            startup
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("config read error")
                    && diagnostic.contains("using defaults")),
            "{:?}",
            startup.diagnostics
        );
        assert!(reload.unwrap_err().iter().any(|diagnostic| {
            diagnostic.contains("config read error")
                && diagnostic.contains("keeping current config")
        }));
    }

    #[test]
    fn load_live_config_registers_sidebar_collapsed_mode_and_reports_misspelled_sibling() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
sidebar_collapsed_mode = "hidden"
sidebar_collapsed_mod = "hidden"
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.config.ui.sidebar_collapsed_mode,
            super::super::SidebarCollapsedModeConfig::Hidden
        );
        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config key ui.sidebar_collapsed_mod; ignoring key"]
        );
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_registers_hide_tab_bar_when_single_tab_and_reports_misspelled_sibling() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
hide_tab_bar_when_single_tab = true
hide_tab_bar_when_single_tabb = true
"#,
        )
        .unwrap();

        assert!(loaded.config.ui.hide_tab_bar_when_single_tab);
        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config key ui.hide_tab_bar_when_single_tabb; ignoring key"]
        );
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn startup_config_load_accepts_the_shipped_default_config() {
        // `zynk --default-config` is what users copy to config.toml; every key it ships
        // must be a real typed key, and its commented lines must not read as keys.
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "zynk-config-default-sample-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, crate::DEFAULT_CONFIG).unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(&path);

        assert!(
            loaded.diagnostics.is_empty(),
            "shipped default config must load clean: {:?}",
            loaded.diagnostics
        );
    }
}
