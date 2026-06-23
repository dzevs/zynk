//! zynk fork: global SQLite path resolution (ADR 0003, finalized by ADR 0008).
//!
//! ADR 0008 makes the native default `$ZYNK_HOME/zynk.db` (default
//! `~/.zynk/zynk.db`). The wrapper-era `zynk-v2/` subdir is NO LONGER the
//! default — it is only RECOGNIZED for transitional adopt/detection
//! (`legacy_native_db_path*`), never created as the default sink.

use std::path::{Path, PathBuf};

pub const ZYNK_HOME_ENV: &str = "ZYNK_HOME";
pub const ZYNK_SQLITE_HOME_ENV: &str = "ZYNK_SQLITE_HOME";
/// Transitional only: the wrapper-era subdir under `~/.zynk`. Recognized for
/// adopt/detection (ADR 0008), NEVER the native default.
pub const LEGACY_NATIVE_DB_SUBDIR: &str = "zynk-v2";
pub const DB_FILE_NAME: &str = "zynk.db";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbPathSource {
    ConfigSqliteHome,
    ZynkSqliteHomeEnv,
    ZynkHomeEnv,
    DefaultHome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbPathResolution {
    pub sqlite_home: PathBuf,
    pub db_path: PathBuf,
    pub source: DbPathSource,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("zynk-home"))
}

fn config_relative_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    crate::config::config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(path)
}

/// Transitional: the legacy `~/.zynk/zynk-v2/zynk.db` location (the pre-ADR-0008
/// default). Recognized only so `zynk db adopt` can relocate/back it up. Never
/// created as the default.
pub fn legacy_native_db_path_with_home(home: &Path) -> PathBuf {
    home.join(".zynk")
        .join(LEGACY_NATIVE_DB_SUBDIR)
        .join(DB_FILE_NAME)
}

pub fn legacy_native_db_path() -> PathBuf {
    legacy_native_db_path_with_home(&home_dir())
}

pub fn resolve_db_path_with(
    home: &Path,
    config_sqlite_home: Option<&str>,
    env_zynk_sqlite_home: Option<&str>,
    env_zynk_home: Option<&str>,
) -> DbPathResolution {
    // Precedence (ADR 0008): config sqlite_home > ZYNK_SQLITE_HOME (exact dir) >
    // ZYNK_HOME (+ zynk.db, NO zynk-v2) > ~/.zynk/zynk.db.
    if let Some(path) = config_sqlite_home.filter(|s| !s.trim().is_empty()) {
        let sqlite_home = config_relative_path(Path::new(path.trim()));
        return DbPathResolution {
            db_path: sqlite_home.join(DB_FILE_NAME),
            sqlite_home,
            source: DbPathSource::ConfigSqliteHome,
        };
    }
    if let Some(path) = env_zynk_sqlite_home.filter(|s| !s.trim().is_empty()) {
        let sqlite_home = PathBuf::from(path.trim());
        return DbPathResolution {
            db_path: sqlite_home.join(DB_FILE_NAME),
            sqlite_home,
            source: DbPathSource::ZynkSqliteHomeEnv,
        };
    }
    if let Some(path) = env_zynk_home.filter(|s| !s.trim().is_empty()) {
        // ADR 0008: ZYNK_HOME holds zynk.db DIRECTLY — no zynk-v2 subdir.
        let sqlite_home = PathBuf::from(path.trim());
        return DbPathResolution {
            db_path: sqlite_home.join(DB_FILE_NAME),
            sqlite_home,
            source: DbPathSource::ZynkHomeEnv,
        };
    }
    // ADR 0008 native default: ~/.zynk/zynk.db (NOT ~/.zynk/zynk-v2/zynk.db).
    let sqlite_home = home.join(".zynk");
    DbPathResolution {
        db_path: sqlite_home.join(DB_FILE_NAME),
        sqlite_home,
        source: DbPathSource::DefaultHome,
    }
}

pub fn resolve_db_path() -> DbPathResolution {
    let loaded = crate::config::Config::load();
    resolve_db_path_with(
        &resolved_home_dir(
            loaded.config.zynk.sqlite_home.as_deref(),
            std::env::var(ZYNK_SQLITE_HOME_ENV).ok().as_deref(),
            std::env::var(ZYNK_HOME_ENV).ok().as_deref(),
        ),
        loaded.config.zynk.sqlite_home.as_deref(),
        std::env::var(ZYNK_SQLITE_HOME_ENV).ok().as_deref(),
        std::env::var(ZYNK_HOME_ENV).ok().as_deref(),
    )
}

/// The home directory the ambient-env resolver feeds into [`resolve_db_path_with`].
///
/// In a NORMAL (production) build this is always the real `$HOME`. Under the CRATE'S
/// OWN test build (`cfg(test)`), the DEFAULT-home branch (no config sqlite_home, no
/// `ZYNK_SQLITE_HOME`, no `ZYNK_HOME`) is a SAFETY HAZARD: it would otherwise resolve to
/// the real `~/.zynk` and an in-process `open_migrated()` could migrate the live DB. So
/// when no explicit override is set, we substitute a unique per-process temp home — an
/// in-process unit test can never reach the live `~/.zynk`. When ANY override is present
/// the real `$HOME` is irrelevant (the override wins in `resolve_db_path_with`), so the
/// substitution does not change behavior. Production builds are entirely unaffected.
fn resolved_home_dir(
    config_sqlite_home: Option<&str>,
    env_zynk_sqlite_home: Option<&str>,
    env_zynk_home: Option<&str>,
) -> PathBuf {
    #[cfg(test)]
    {
        let has_override = config_sqlite_home.is_some_and(|s| !s.trim().is_empty())
            || env_zynk_sqlite_home.is_some_and(|s| !s.trim().is_empty())
            || env_zynk_home.is_some_and(|s| !s.trim().is_empty());
        if !has_override {
            return std::env::temp_dir().join(format!("zynk-unit-db-{}", std::process::id()));
        }
    }
    #[cfg(not(test))]
    let _ = (config_sqlite_home, env_zynk_sqlite_home, env_zynk_home);
    home_dir()
}

pub fn db_path() -> PathBuf {
    resolve_db_path().db_path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_zynk_db_under_home_no_v2() {
        let home = PathBuf::from("/tmp/home");
        let r = resolve_db_path_with(&home, None, None, None);
        assert_eq!(r.source, DbPathSource::DefaultHome);
        // ADR 0008: NO zynk-v2 in the default.
        assert_eq!(r.db_path, PathBuf::from("/tmp/home/.zynk/zynk.db"));
        assert!(!r.db_path.to_string_lossy().contains("zynk-v2"));
    }

    #[test]
    fn zynk_home_holds_db_directly_no_v2() {
        let home = PathBuf::from("/tmp/home");
        let r = resolve_db_path_with(&home, None, None, Some("/tmp/custom-zynk"));
        assert_eq!(r.source, DbPathSource::ZynkHomeEnv);
        // ADR 0008: ZYNK_HOME + zynk.db, NO zynk-v2.
        assert_eq!(r.db_path, PathBuf::from("/tmp/custom-zynk/zynk.db"));
        assert!(!r.db_path.to_string_lossy().contains("zynk-v2"));
    }

    #[test]
    fn zynk_sqlite_home_is_exact_sqlite_home() {
        let home = PathBuf::from("/tmp/home");
        let r = resolve_db_path_with(&home, None, Some("/tmp/sqlite-home"), Some("/tmp/ignored"));
        assert_eq!(r.source, DbPathSource::ZynkSqliteHomeEnv);
        assert_eq!(r.db_path, PathBuf::from("/tmp/sqlite-home/zynk.db"));
    }

    #[test]
    fn config_sqlite_home_wins() {
        let home = PathBuf::from("/tmp/home");
        let r = resolve_db_path_with(
            &home,
            Some("/tmp/config-home"),
            Some("/tmp/sqlite-home"),
            Some("/tmp/ignored"),
        );
        assert_eq!(r.source, DbPathSource::ConfigSqliteHome);
        assert_eq!(r.db_path, PathBuf::from("/tmp/config-home/zynk.db"));
    }

    #[test]
    fn resolved_home_dir_redirects_default_branch_under_test_build() {
        // #117 review fix C.1: under the crate's own test build, the DEFAULT-home branch
        // (no overrides) must NEVER resolve to the real `~/.zynk` — it is redirected to a
        // per-process temp home so an in-process unit test can never touch the live DB.
        let redirected = resolved_home_dir(None, None, None);
        assert!(
            redirected.starts_with(std::env::temp_dir()),
            "default-home must redirect to temp under cfg(test): {redirected:?}"
        );
        assert!(
            redirected
                .to_string_lossy()
                .contains(&format!("zynk-unit-db-{}", std::process::id())),
            "redirect must be per-process: {redirected:?}"
        );
    }

    #[test]
    fn resolved_home_dir_honors_explicit_overrides_under_test_build() {
        // An explicit override means the real `$HOME` is irrelevant to the resolved DB
        // path (the override wins in `resolve_db_path_with`), so the temp substitution is
        // a no-op effect: the resolution still lands on the override path.
        let home = resolved_home_dir(None, Some("/tmp/some-sqlite-home"), None);
        let r = resolve_db_path_with(&home, None, Some("/tmp/some-sqlite-home"), None);
        assert_eq!(r.source, DbPathSource::ZynkSqliteHomeEnv);
        assert_eq!(r.db_path, PathBuf::from("/tmp/some-sqlite-home/zynk.db"));
    }

    #[test]
    fn legacy_native_db_path_recognized_for_adopt() {
        // Transitional detection only — never the default.
        assert_eq!(
            legacy_native_db_path_with_home(Path::new("/tmp/home")),
            PathBuf::from("/tmp/home/.zynk/zynk-v2/zynk.db")
        );
    }
}
