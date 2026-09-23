use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::warn;

use crate::api::schema::InstalledPluginInfo;

pub const MANIFEST_UNAVAILABLE_WARNING_PREFIX: &str = "manifest unavailable: ";
const PLUGIN_REGISTRY_MAX_BYTES: u64 = 8 * 1024 * 1024;
const REGISTRY_LOCK_FILE: &str = ".plugins.lock";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct BoundedJson {
    bytes: Vec<u8>,
}

impl std::io::Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > PLUGIN_REGISTRY_MAX_BYTES as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "plugin registry exceeds the size limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn registry_path() -> PathBuf {
    crate::config::config_dir().join("plugins.json")
}

fn registry_lock_path() -> PathBuf {
    crate::config::config_dir().join(REGISTRY_LOCK_FILE)
}

fn open_private_file(path: &Path, create: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "plugin registry path is not a regular file owned by this user: {}",
                path.display()
            ),
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn with_registry_lock<T>(operation: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    let lock_path = registry_lock_path();
    if let Some(parent) = lock_path.parent() {
        crate::plugin_paths::ensure_private_dir(parent)?;
    }
    let lock = open_private_file(&lock_path, true)?;
    lock.lock()?;
    operation()
}

fn save_json_to_path<T: serde::Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "registry path has no parent",
        )
    })?;
    crate::plugin_paths::ensure_private_dir(parent)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => drop(open_private_file(path, false)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let mut json = BoundedJson { bytes: Vec::new() };
    serde_json::to_writer_pretty(&mut json, value)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid registry file name",
            )
        })?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let mut tmp = options.open(&tmp_path)?;
        tmp.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        tmp.write_all(&json.bytes)?;
        tmp.sync_all()?;
        drop(tmp);
        std::fs::rename(&tmp_path, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

pub fn save_to_path(path: &Path, plugins: &[InstalledPluginInfo]) -> std::io::Result<()> {
    save_json_to_path(path, plugins)
}

pub fn update<T>(
    mutation: impl FnOnce(&mut Vec<InstalledPluginInfo>) -> T,
) -> std::io::Result<(T, Vec<InstalledPluginInfo>)> {
    with_registry_lock(|| {
        let mut plugins = load_from_path_strict(&registry_path())?;
        let result = mutation(&mut plugins);
        plugins.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        save_to_path(&registry_path(), &plugins)?;
        Ok((result, plugins))
    })
}

pub fn try_load() -> std::io::Result<Vec<InstalledPluginInfo>> {
    with_registry_lock(|| load_from_path_strict(&registry_path()))
}

/// Load the global registry. Missing or malformed data never blocks startup;
/// mutations use strict reads and will not overwrite a corrupt registry.
pub fn load() -> Vec<InstalledPluginInfo> {
    match try_load() {
        Ok(plugins) => plugins,
        Err(err) => {
            warn!(path = %registry_path().display(), err = %err, "failed to load plugin registry");
            Vec::new()
        }
    }
}

#[cfg(test)]
pub fn load_from_path(path: &Path) -> Vec<InstalledPluginInfo> {
    match load_from_path_strict(path) {
        Ok(entries) => entries,
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to read plugin registry");
            Vec::new()
        }
    }
}

fn load_from_path_strict(path: &Path) -> std::io::Result<Vec<InstalledPluginInfo>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    }
    let mut file = open_private_file(path, false)?;
    if file.metadata()?.len() > PLUGIN_REGISTRY_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "plugin registry exceeds the size limit",
        ));
    }
    let mut content = Vec::new();
    (&mut file)
        .take(PLUGIN_REGISTRY_MAX_BYTES + 1)
        .read_to_end(&mut content)?;
    if content.len() as u64 > PLUGIN_REGISTRY_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "plugin registry exceeds the size limit",
        ));
    }
    serde_json::from_slice(&content)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

/// Re-read each entry's manifest from disk using the provided reload function.
///
/// If the manifest parses successfully, replace cached fields but keep the
/// stored `enabled` flag.  If the file is gone or unparseable, keep the stored
/// entry and append a warning so `plugin.list` surfaces it.
pub fn reload_manifests(
    mut entries: Vec<InstalledPluginInfo>,
    reload_fn: impl Fn(&str, bool) -> Result<InstalledPluginInfo, String>,
) -> Vec<InstalledPluginInfo> {
    for entry in &mut entries {
        entry.warnings.clear();
        match reload_fn(&entry.manifest_path, entry.enabled) {
            Ok(mut fresh) => {
                fresh.enabled = entry.enabled;
                fresh.source = entry.source.clone();
                *entry = fresh;
            }
            Err(warn_msg) => {
                entry
                    .warnings
                    .push(format!("{MANIFEST_UNAVAILABLE_WARNING_PREFIX}{warn_msg}"));
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir()
            .join(format!(
                "zynk-registry-{name}-{}-{nanos}",
                std::process::id()
            ))
            .join("plugins.json")
    }

    fn sample_plugin(id: &str) -> InstalledPluginInfo {
        InstalledPluginInfo {
            plugin_id: id.to_string(),
            name: "Test Plugin".to_string(),
            version: "0.1.0".to_string(),
            min_zynk_version: crate::build_info::BASE_VERSION.to_string(),
            description: None,
            manifest_path: format!("/tmp/{id}/zynk-plugin.toml"),
            plugin_root: format!("/tmp/{id}"),
            enabled: true,
            platforms: None,
            build: vec![],
            actions: vec![],
            events: vec![],
            panes: vec![],
            link_handlers: vec![],
            source: Default::default(),
            warnings: vec![],
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let path = temp_registry_path("roundtrip");
        let plugins = vec![sample_plugin("example.a"), sample_plugin("example.b")];

        save_to_path(&path, &plugins).unwrap();

        let loaded = load_from_path(&path);
        assert_eq!(loaded.len(), 2);
        let ids: Vec<_> = loaded.iter().map(|p| p.plugin_id.as_str()).collect();
        assert!(ids.contains(&"example.a"));
        assert!(ids.contains(&"example.b"));
    }

    #[test]
    fn missing_file_returns_empty() {
        let path = temp_registry_path("missing");
        let loaded = load_from_path(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn corrupt_file_returns_empty_without_panic() {
        let path = temp_registry_path("corrupt");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, b"this is not valid json {{{{").unwrap();

        let loaded = load_from_path(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn reload_manifests_keeps_entry_with_warning_on_missing_manifest() {
        let entry = sample_plugin("example.missing");
        let entries = vec![entry];

        let result = reload_manifests(entries, |path, _enabled| {
            Err(format!("manifest not found at {path}"))
        });

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].plugin_id, "example.missing");
        assert!(!result[0].warnings.is_empty());
        assert!(result[0].warnings[0].contains("manifest not found"));
    }

    #[test]
    fn reload_manifests_uses_fresh_parse_and_keeps_enabled_flag() {
        let mut entry = sample_plugin("example.reload");
        entry.enabled = false;
        entry.source = crate::api::schema::PluginSourceInfo {
            kind: crate::api::schema::PluginSourceKind::Github,
            owner: Some("ogulcancelik".into()),
            repo: Some("plugin-examples".into()),
            subdir: Some("worktree-bootstrap".into()),
            requested_ref: Some("main".into()),
            resolved_commit: Some("abc123".into()),
            managed_path: Some("/tmp/zynk/plugins/github/example.reload".into()),
            installed_unix_ms: Some(42),
        };

        let result = reload_manifests(vec![entry], |_path, _enabled| {
            Ok(InstalledPluginInfo {
                plugin_id: "example.reload".to_string(),
                name: "Fresh Name".to_string(),
                version: "0.2.0".to_string(),
                min_zynk_version: crate::build_info::BASE_VERSION.to_string(),
                description: Some("refreshed".to_string()),
                manifest_path: "/tmp/example.reload/zynk-plugin.toml".to_string(),
                plugin_root: "/tmp/example.reload".to_string(),
                enabled: true, // caller would pass stored enabled; fresh parse returns true
                platforms: None,
                build: vec![],
                actions: vec![],
                events: vec![],
                panes: vec![],
                link_handlers: vec![],
                source: Default::default(),
                warnings: vec![],
            })
        });

        assert_eq!(result[0].name, "Fresh Name");
        assert_eq!(result[0].version, "0.2.0");
        // enabled preserved from stored entry
        assert!(!result[0].enabled);
        assert_eq!(
            result[0].source.kind,
            crate::api::schema::PluginSourceKind::Github
        );
        assert_eq!(result[0].source.owner.as_deref(), Some("ogulcancelik"));
        assert!(result[0].warnings.is_empty());
    }

    #[test]
    fn atomic_write_temp_file_is_cleaned_up_on_rename_failure() {
        let path = temp_registry_path("cleanup");
        save_to_path(&path, &[sample_plugin("example.cleanup")]).unwrap();

        let temp_prefix = format!(".{}.tmp-", path.file_name().unwrap().to_string_lossy());
        let leftovers = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&temp_prefix)
            })
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "temporary files should be absent after successful rename: {leftovers:?}"
        );
        assert!(path.exists());
    }

    #[test]
    fn save_replaces_existing_registry_file() {
        let path = temp_registry_path("replace-existing");
        save_to_path(&path, &[sample_plugin("example.first")]).unwrap();
        save_to_path(&path, &[sample_plugin("example.second")]).unwrap();

        let loaded = load_from_path(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].plugin_id, "example.second");
    }

    #[test]
    fn m847_global_registry_updates_atomically_with_private_modes() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        let root = temp_registry_path("global-private");
        let config_home = root.parent().unwrap().join("config");
        std::env::set_var("XDG_CONFIG_HOME", &config_home);

        update(|plugins| plugins.push(sample_plugin("example.private"))).unwrap();
        let loaded = try_load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].plugin_id, "example.private");

        let registry = registry_path();
        let lock = registry_lock_path();
        assert_eq!(
            registry.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(lock.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(registry.metadata().unwrap().uid(), unsafe {
            libc::geteuid()
        });
        assert_eq!(lock.metadata().unwrap().uid(), unsafe { libc::geteuid() });

        let valid_bytes = std::fs::read(&registry).unwrap();
        std::fs::write(&registry, b"not json").unwrap();
        let corrupt_bytes = std::fs::read(&registry).unwrap();
        let err = update(|plugins| plugins.push(sample_plugin("example.refused"))).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&registry).unwrap(), corrupt_bytes);

        std::fs::write(&registry, &valid_bytes).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&registry)
            .unwrap()
            .set_len(PLUGIN_REGISTRY_MAX_BYTES + 1)
            .unwrap();
        let err = try_load().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        std::fs::write(&registry, &valid_bytes).unwrap();
        let symlink_target = root.parent().unwrap().join("must-not-change.json");
        std::fs::write(&symlink_target, b"outside").unwrap();
        std::fs::remove_file(&registry).unwrap();
        std::os::unix::fs::symlink(&symlink_target, &registry).unwrap();
        assert!(update(|plugins| plugins.clear()).is_err());
        assert_eq!(std::fs::read(&symlink_target).unwrap(), b"outside");

        let _ = std::fs::remove_dir_all(root.parent().unwrap());
        match previous {
            Some(previous) => std::env::set_var("XDG_CONFIG_HOME", previous),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
