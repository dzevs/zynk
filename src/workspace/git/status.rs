// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::workspace::{GitSpaceMetadata, WorkspaceGitStatusSnapshot};

use super::{
    config::{deps_current, read_config, stamp, upstream_full_ref, ConfigCtx, FileDep},
    discovery::{
        automatic_workspace_label, canonicalize_best_effort_path, fallback_label_from_cwd,
        git_ref_storage_is_reftable, git_rev_parse_verify, git_space_metadata_from_info,
        git_symbolic_head_full, git_worktree_info, read_ref_oid, GitWorktreeInfo,
    },
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GitStatusRefreshDemand {
    pub branch: bool,
    pub ahead_behind: bool,
    pub dirty_paths: bool,
    // Internal worker payload; it is never config, wire, or persisted state.
    pub(crate) dirty_paths_result: Option<usize>,
}

impl GitStatusRefreshDemand {
    #[cfg(test)]
    pub const ALL: Self = Self {
        branch: true,
        ahead_behind: true,
        dirty_paths: true,
        dirty_paths_result: None,
    };

    pub fn is_empty(self) -> bool {
        !self.branch && !self.ahead_behind && !self.dirty_paths
    }

    pub(crate) fn with_dirty_paths_result(mut self, result: Option<usize>) -> Self {
        self.dirty_paths_result = result;
        self
    }

    pub(crate) fn dirty_paths_result(self) -> Option<usize> {
        self.dirty_paths_result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusCacheEntry {
    pub fingerprint: Option<GitStatusFingerprint>,
    pub retry_after: Option<Instant>,
    pub dirty_refresh_after: Option<Instant>,
    pub snapshot: WorkspaceGitStatusSnapshot,
}

pub(crate) const GIT_DIRTY_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const GIT_DIRTY_STATUS_FAILURE_BACKOFF: Duration = Duration::from_secs(30);
pub(crate) const GIT_DIRTY_STATUS_TIMEOUT: Duration = Duration::from_millis(250);
pub(crate) const GIT_DIRTY_STATUS_CLEANUP_GRACE: Duration = Duration::from_millis(250);
pub(crate) const GIT_DIRTY_STATUS_OUTPUT_MAX_BYTES: usize = 4 * 1024 * 1024;
const GIT_DIRTY_STATUS_STDERR_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusFingerprint {
    pub head: GitHeadIdentity,
    pub upstream: Option<GitUpstreamIdentity>,
    repository_context: RepoContext,
}

// Discovery, ref storage, discovery dependencies, and optional branch config.
type RepoContext = (GitWorktreeInfo, bool, Vec<FileDep>, Option<ConfigCtx>);

fn repo_context(cwd: &Path) -> Option<RepoContext> {
    let info = git_worktree_info(cwd)?;
    let reftable = git_ref_storage_is_reftable(&info.git_common_dir);
    let mut paths = vec![info.repo_root.join(".git"), info.git_dir.join("commondir")];
    paths.push(info.git_dir.join("HEAD"));
    paths.push(info.git_common_dir.join("config"));
    paths.extend((info.git_dir != info.git_common_dir).then(|| info.git_dir.join("config")));
    let mut deps: Vec<_> = paths.into_iter().map(|path| stamp(path, None)).collect();
    deps[0].reusable &= git_worktree_info(cwd).as_ref() == Some(&info)
        && git_ref_storage_is_reftable(&info.git_common_dir) == reftable
        && deps_current(&deps);
    Some((info, reftable, deps, None))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHeadIdentity {
    Branch {
        full_ref: String,
        short_name: String,
        oid: Option<String>,
    },
    Detached {
        oid: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitUpstreamIdentity {
    pub remote: String,
    pub merge_ref: String,
    pub full_ref: String,
    pub oid: Option<String>,
}

pub fn git_status_cache_key(cwd: &Path) -> Option<PathBuf> {
    git_worktree_info(cwd).map(|info| canonicalize_best_effort_path(&info.repo_root))
}

pub fn git_status_cache_key_for_space(space: &GitSpaceMetadata) -> PathBuf {
    canonicalize_best_effort_path(&space.repo_root)
}

#[cfg(test)]
pub fn git_status_snapshot_for_cwd(
    cwd: &Path,
    cached: Option<&GitStatusCacheEntry>,
) -> (WorkspaceGitStatusSnapshot, Option<GitStatusCacheEntry>) {
    git_status_snapshot_for_cwd_with_demand(cwd, cached, GitStatusRefreshDemand::ALL)
}

pub fn git_status_snapshot_for_cwd_with_demand(
    cwd: &Path,
    cached: Option<&GitStatusCacheEntry>,
    demand: GitStatusRefreshDemand,
) -> (WorkspaceGitStatusSnapshot, Option<GitStatusCacheEntry>) {
    git_status_snapshot_for_cwd_with_demand_at(cwd, cached, demand, Instant::now(), dirty_paths)
}

fn git_status_snapshot_for_cwd_with_demand_at<F>(
    cwd: &Path,
    cached: Option<&GitStatusCacheEntry>,
    demand: GitStatusRefreshDemand,
    now: Instant,
    query_dirty_paths: F,
) -> (WorkspaceGitStatusSnapshot, Option<GitStatusCacheEntry>)
where
    F: FnOnce(&Path) -> Result<usize, DirtyStatusError>,
{
    if let Some(cached) = cached.filter(|entry| {
        entry.fingerprint.is_none()
            && entry
                .retry_after
                .is_some_and(|retry_after| retry_after > now)
    }) {
        return (cached.snapshot.clone(), Some(cached.clone()));
    }

    let repository_context = cached
        .and_then(|entry| entry.fingerprint.as_ref())
        .map(|fingerprint| fingerprint.repository_context.clone())
        .filter(|context| deps_current(&context.2))
        .or_else(|| repo_context(cwd));
    let Some(repository_context) = repository_context else {
        let snapshot = WorkspaceGitStatusSnapshot {
            auto_label: fallback_label_from_cwd(cwd),
            branch: None,
            ahead_behind: None,
            dirty_paths: None,
            space: None,
        };
        return (
            snapshot.clone(),
            Some(GitStatusCacheEntry {
                fingerprint: None,
                retry_after: Some(now + Duration::from_secs(30)),
                dirty_refresh_after: None,
                snapshot,
            }),
        );
    };
    let auto_label = automatic_workspace_label(cwd, &repository_context.0.repo_root);
    let space = git_space_metadata_from_info(&repository_context.0);

    let (mut snapshot, fingerprint) = if !demand.ahead_behind {
        let fingerprint = fingerprint(repository_context, false);
        let branch = demand
            .branch
            .then(|| fingerprint.as_ref()?.branch_name())
            .flatten()
            .map(str::to_string);
        let snapshot = WorkspaceGitStatusSnapshot {
            auto_label,
            branch,
            ahead_behind: None,
            dirty_paths: cached.and_then(|entry| entry.snapshot.dirty_paths),
            space: Some(space),
        };
        (snapshot, fingerprint)
    } else {
        let Some(fingerprint) = fingerprint(repository_context, true) else {
            return (
                WorkspaceGitStatusSnapshot {
                    auto_label,
                    branch: None,
                    ahead_behind: None,
                    dirty_paths: None,
                    space: Some(space),
                },
                None,
            );
        };
        let branch = fingerprint.branch_name().map(str::to_string);
        let ahead_behind = if let Some(cached) =
            cached.filter(|entry| entry.fingerprint.as_ref() == Some(&fingerprint))
        {
            cached.snapshot.ahead_behind
        } else {
            fingerprint
                .head_oid()
                .zip(fingerprint.upstream_oid())
                .and_then(|(head_oid, upstream_oid)| {
                    git_ahead_behind_between(cwd, head_oid, upstream_oid)
                })
        };
        let snapshot = WorkspaceGitStatusSnapshot {
            auto_label,
            branch,
            ahead_behind,
            dirty_paths: cached.and_then(|entry| entry.snapshot.dirty_paths),
            space: Some(space),
        };
        (snapshot, Some(fingerprint))
    };

    let mut dirty_refresh_after = cached.and_then(|entry| entry.dirty_refresh_after);
    if demand.dirty_paths && dirty_refresh_after.is_none_or(|refresh_after| refresh_after <= now) {
        match query_dirty_paths(cwd) {
            Ok(dirty_paths) => {
                snapshot.dirty_paths = Some(dirty_paths);
                dirty_refresh_after = Some(now + GIT_DIRTY_STATUS_REFRESH_INTERVAL);
            }
            Err(_) => {
                dirty_refresh_after = Some(now + GIT_DIRTY_STATUS_FAILURE_BACKOFF);
            }
        }
    }

    (
        snapshot.clone(),
        fingerprint.map(|fingerprint| GitStatusCacheEntry {
            fingerprint: Some(fingerprint),
            retry_after: None,
            dirty_refresh_after,
            snapshot,
        }),
    )
}

#[derive(Debug)]
enum DirtyStatusError {
    Spawn,
    Wait,
    Timeout,
    Output,
    Exit,
    Malformed,
}

fn dirty_status_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    scrub_git_status_env(&mut command);
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("--no-optional-locks")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(cwd)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=all",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::platform::configure_status_command(&mut command);
    command
}

fn scrub_git_status_env(command: &mut Command) {
    let names = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| name.as_encoded_bytes().starts_with(b"GIT_"))
        .collect::<Vec<_>>();
    for name in names {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
}

fn dirty_paths(cwd: &Path) -> Result<usize, DirtyStatusError> {
    run_dirty_status_command(dirty_status_command(cwd))
}

fn run_dirty_status_command(mut command: Command) -> Result<usize, DirtyStatusError> {
    let deadline = Instant::now() + GIT_DIRTY_STATUS_TIMEOUT;
    let mut child = command.spawn().map_err(|_| DirtyStatusError::Spawn)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    crate::platform::wait_for_bounded_child_output(
        &mut child,
        stdout,
        stderr,
        crate::platform::BoundedChildOutputOptions {
            deadline,
            cleanup_grace: GIT_DIRTY_STATUS_CLEANUP_GRACE,
            stdout_max_bytes: GIT_DIRTY_STATUS_OUTPUT_MAX_BYTES,
            stderr_max_bytes: GIT_DIRTY_STATUS_STDERR_MAX_BYTES,
        },
        validate_dirty_status_output,
    )
    .map_err(|error| match error {
        crate::platform::BoundedChildOutputError::Wait => DirtyStatusError::Wait,
        crate::platform::BoundedChildOutputError::Timeout => DirtyStatusError::Timeout,
        crate::platform::BoundedChildOutputError::Output => DirtyStatusError::Output,
        crate::platform::BoundedChildOutputError::Exit => DirtyStatusError::Exit,
        crate::platform::BoundedChildOutputError::Validation => DirtyStatusError::Malformed,
    })
}

fn validate_dirty_status_output(
    outcome: crate::platform::BoundedChildOutput,
) -> Result<usize, crate::platform::BoundedChildOutputError> {
    let crate::platform::BoundedChildOutput { stdout, stderr } = outcome;
    if matches!(&stderr, crate::platform::LimitedRead::Oversized)
        || matches!(&stdout, crate::platform::LimitedRead::Oversized)
    {
        return Err(crate::platform::BoundedChildOutputError::Output);
    }
    let bytes = match stdout {
        crate::platform::LimitedRead::Empty => Vec::new(),
        crate::platform::LimitedRead::Complete(bytes) => bytes,
        crate::platform::LimitedRead::Oversized => {
            return Err(crate::platform::BoundedChildOutputError::Output);
        }
    };
    parse_dirty_paths(&bytes).map_err(|_| crate::platform::BoundedChildOutputError::Validation)
}

fn parse_dirty_paths(bytes: &[u8]) -> Result<usize, DirtyStatusError> {
    #[cfg(test)]
    record_validation_direct_child_state();
    if bytes.is_empty() {
        return Ok(0);
    }
    if !bytes.ends_with(&[0]) {
        return Err(DirtyStatusError::Malformed);
    }
    let mut paths = HashSet::new();
    for record in bytes[..bytes.len() - 1].split(|byte| *byte == 0) {
        if record.len() < 4 || record[2] != b' ' || record[3..].is_empty() {
            return Err(DirtyStatusError::Malformed);
        }
        paths.insert(record[3..].to_vec());
    }
    Ok(paths.len())
}

#[cfg(test)]
fn record_validation_direct_child_state() {
    let Some(validation_sentinel) = std::env::var_os("ZYNK_DIRTY_PIPE_VALIDATION_SENTINEL") else {
        return;
    };
    let state = std::env::var_os("ZYNK_DIRTY_PIPE_DIRECT_SENTINEL")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|pid| std::fs::read_to_string(format!("/proc/{}/status", pid.trim())).ok())
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("State:"))?
                .split_whitespace()
                .next()?
                .chars()
                .next()
        })
        .unwrap_or('-');
    let _ = std::fs::write(validation_sentinel, format!("{state}\n"));
}

#[cfg(test)]
pub(super) fn git_status_fingerprint(cwd: &Path) -> Option<GitStatusFingerprint> {
    fingerprint(repo_context(cwd)?, true)
}

fn fingerprint(mut repo: RepoContext, include_upstream: bool) -> Option<GitStatusFingerprint> {
    let head = read_head_identity(&repo.0, repo.1)?;
    let upstream = match &head {
        GitHeadIdentity::Branch { short_name, .. } if include_upstream => {
            read_upstream(&mut repo, short_name)
        }
        _ => None,
    };

    Some(GitStatusFingerprint {
        head,
        upstream,
        repository_context: repo,
    })
}

impl GitStatusFingerprint {
    fn branch_name(&self) -> Option<&str> {
        match &self.head {
            GitHeadIdentity::Branch { short_name, .. } => Some(short_name.as_str()),
            GitHeadIdentity::Detached { .. } => None,
        }
    }

    fn head_oid(&self) -> Option<&str> {
        match &self.head {
            GitHeadIdentity::Branch { oid, .. } => oid.as_deref(),
            GitHeadIdentity::Detached { oid } => Some(oid.as_str()),
        }
    }

    fn upstream_oid(&self) -> Option<&str> {
        self.upstream
            .as_ref()
            .and_then(|upstream| upstream.oid.as_deref())
    }
}

fn read_head_identity(info: &GitWorktreeInfo, reftable: bool) -> Option<GitHeadIdentity> {
    if reftable {
        return read_head_identity_from_git(info);
    }

    read_head_identity_from_files(info)
}

fn read_head_identity_from_git(info: &GitWorktreeInfo) -> Option<GitHeadIdentity> {
    if let Some(full_ref) = git_symbolic_head_full(&info.repo_root) {
        let short_name = full_ref.strip_prefix("refs/heads/")?.to_string();
        let oid = git_rev_parse_verify(&info.repo_root, &full_ref);
        return Some(GitHeadIdentity::Branch {
            full_ref,
            short_name,
            oid,
        });
    }

    git_rev_parse_verify(&info.repo_root, "HEAD").map(|oid| GitHeadIdentity::Detached { oid })
}

fn read_head_identity_from_files(info: &GitWorktreeInfo) -> Option<GitHeadIdentity> {
    let head = std::fs::read_to_string(info.git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(full_ref) = head.strip_prefix("ref: ") {
        let short_name = full_ref.strip_prefix("refs/heads/")?.to_string();
        let oid = read_ref_oid(&info.git_common_dir, full_ref);
        return Some(GitHeadIdentity::Branch {
            full_ref: full_ref.to_string(),
            short_name,
            oid,
        });
    }

    (!head.is_empty()).then(|| GitHeadIdentity::Detached {
        oid: head.to_string(),
    })
}

fn read_upstream(repo: &mut RepoContext, branch: &str) -> Option<GitUpstreamIdentity> {
    if repo
        .3
        .as_ref()
        .is_none_or(|context| context.0 != branch || !deps_current(&context.2))
    {
        repo.3 = Some(read_config(&repo.0, branch));
    }
    let config = repo.3.as_ref()?.1.clone()?;
    let full_ref = upstream_full_ref(&config)?;
    let oid = if repo.1 {
        git_rev_parse_verify(&repo.0.repo_root, &full_ref)
    } else {
        read_ref_oid(&repo.0.git_common_dir, &full_ref)
    };
    Some(GitUpstreamIdentity {
        remote: config.remote,
        merge_ref: config.merge_ref,
        full_ref,
        oid,
    })
}

#[cfg(test)]
pub(crate) fn git_ahead_behind(cwd: &Path) -> Option<(usize, usize)> {
    super::discovery::git_repo_root(cwd)?;

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_git_ahead_behind_output(&stdout)
}

fn git_ahead_behind_between(
    cwd: &Path,
    head_oid: &str,
    upstream_oid: &str,
) -> Option<(usize, usize)> {
    let range = format!("{head_oid}...{upstream_oid}");
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-list", "--left-right", "--count", &range])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_git_ahead_behind_output(&stdout)
}

fn parse_git_ahead_behind_output(stdout: &str) -> Option<(usize, usize)> {
    let mut parts = stdout.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::git::git_space_metadata;
    use crate::workspace::git::test_support::{
        run_git, scrub_git_env, set_repo_identity, temp_test_dir, write_fake_tracked_repo,
    };

    #[test]
    fn non_git_refresh_reuses_cached_miss_without_rechecking_filesystem() {
        let root = temp_test_dir("cached-miss");
        let cwd = root.join("deep/nested");
        std::fs::create_dir_all(&cwd).unwrap();

        let (initial, cache_entry) = git_status_snapshot_for_cwd(&cwd, None);
        let cache_entry = cache_entry.expect("non-Git result should be cached");
        std::fs::remove_dir_all(&root).unwrap();

        let (cached, update) = git_status_snapshot_for_cwd(&cwd, Some(&cache_entry));

        assert_eq!(cached, initial);
        assert_eq!(update, Some(cache_entry));
    }

    #[test]
    fn cache_key_from_space_preserves_non_utf8_checkout_path() {
        use std::os::unix::ffi::OsStringExt;

        let base = temp_test_dir("non-utf8-key");
        let root = base.join(std::ffi::OsString::from_vec(vec![
            b'r', b'e', b'p', b'o', 0x80,
        ]));
        write_fake_tracked_repo(&root);
        let space = git_space_metadata(&root).expect("Git metadata");

        assert_eq!(
            git_status_cache_key_for_space(&space),
            std::fs::canonicalize(&root).unwrap()
        );

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn git_status_cache_key_ignores_invalid_git_marker() {
        let base = temp_test_dir("invalid-git-root");
        let cwd = base.join("workspace");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        assert_eq!(git_status_cache_key(&cwd), None);

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn expired_non_git_cache_detects_repository_created_in_place() {
        let root = temp_test_dir("expired-miss");
        let (_, cache_entry) = git_status_snapshot_for_cwd(&root, None);
        let mut cache_entry = cache_entry.expect("non-Git result should be cached");
        cache_entry.retry_after = Some(Instant::now() - Duration::from_secs(1));
        write_fake_tracked_repo(&root);

        let (snapshot, update) = git_status_snapshot_for_cwd(&root, Some(&cache_entry));

        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert!(update.is_some_and(|entry| entry.fingerprint.is_some()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_repo_identity_clears_when_head_disappears() {
        let root = temp_test_dir("missing-head");
        write_fake_tracked_repo(&root);
        let (_, cached) = git_status_snapshot_for_cwd(&root, None);
        std::fs::remove_file(root.join(".git/HEAD")).unwrap();

        let (snapshot, _) = git_status_snapshot_for_cwd(&root, cached.as_ref());

        assert_eq!(snapshot.space, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unchanged_refresh_does_not_reread_config() {
        use super::super::config::CONFIG_READ_COUNT;
        let root = temp_test_dir("config-cache-read-count");
        write_fake_tracked_repo(&root);
        CONFIG_READ_COUNT.set(0);
        let (initial, cached) = git_status_snapshot_for_cwd(&root, None);
        let cold_reads = CONFIG_READ_COUNT.replace(0);
        let (second, updated) = git_status_snapshot_for_cwd(&root, cached.as_ref());
        let warm_reads = CONFIG_READ_COUNT.get();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            cold_reads > 0,
            "cold control did not read any configuration"
        );
        assert_eq!(warm_reads, 0, "unchanged refresh reread configuration");
        assert_eq!(initial, second);
        assert_eq!(cached, updated);
        eprintln!("config file reads: cold={cold_reads}, unchanged={warm_reads}");
    }

    #[test]
    fn cached_config_rechecks_worktree_overrides() {
        let root = temp_test_dir("cached-worktree-config");
        write_fake_tracked_repo(&root);
        let config_path = root.join(".git/config");
        let mut config = std::fs::read_to_string(&config_path).unwrap();
        config.push_str("\n[extensions]\nworktreeConfig = true\n");
        std::fs::write(config_path, config).unwrap();
        let worktree_path = root.join(".git/config.worktree");
        std::fs::write(&worktree_path, "[branch \"main\"]\nremote = first\n").unwrap();
        let (_, cached) = git_status_snapshot_for_cwd(&root, None);
        assert_eq!(
            cached
                .as_ref()
                .unwrap()
                .fingerprint
                .as_ref()
                .unwrap()
                .upstream
                .as_ref()
                .unwrap()
                .remote,
            "first"
        );
        std::fs::write(worktree_path, "[branch \"main\"]\nremote = second\n").unwrap();
        let (_, updated) = git_status_snapshot_for_cwd(&root, cached.as_ref());
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(
            updated
                .unwrap()
                .fingerprint
                .unwrap()
                .upstream
                .unwrap()
                .remote,
            "second"
        );
    }

    #[test]
    fn cached_config_rechecks_onbranch_selection() {
        let root = temp_test_dir("cached-onbranch");
        write_fake_tracked_repo(&root);
        std::fs::write(root.join(".git/config"), "[branch \"main\"]\nremote = origin\nmerge = refs/heads/main\n[includeIf \"onbranch:feature\"]\npath = feature.cfg\n").unwrap();
        std::fs::write(
            root.join(".git/feature.cfg"),
            "[branch \"feature\"]\nremote = feature-remote\nmerge = refs/heads/feature\n",
        )
        .unwrap();
        let (_, cached) = git_status_snapshot_for_cwd(&root, None);
        assert_eq!(
            cached
                .as_ref()
                .unwrap()
                .fingerprint
                .as_ref()
                .unwrap()
                .upstream
                .as_ref()
                .unwrap()
                .remote,
            "origin"
        );
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
        let (snapshot, updated) = git_status_snapshot_for_cwd(&root, cached.as_ref());
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(snapshot.branch.as_deref(), Some("feature"));
        assert_eq!(
            updated
                .unwrap()
                .fingerprint
                .unwrap()
                .upstream
                .unwrap()
                .remote,
            "feature-remote"
        );
    }

    #[test]
    fn branch_only_refresh_skips_ahead_behind_cache_work() {
        let root = temp_test_dir("branch-only");
        write_fake_tracked_repo(&root);

        let (snapshot, update) = git_status_snapshot_for_cwd_with_demand(
            &root,
            None,
            GitStatusRefreshDemand {
                branch: true,
                ahead_behind: false,
                dirty_paths: false,
                dirty_paths_result: None,
            },
        );

        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.ahead_behind, None);
        assert!(update.is_some_and(|entry| entry.fingerprint.is_some()));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dirty_status_parser_counts_unique_nul_delimited_paths() {
        assert_eq!(
            parse_dirty_paths(b" M tracked\0?? untracked\nname\0M  tracked\0").unwrap(),
            2
        );
        assert!(matches!(
            parse_dirty_paths(b" M missing-terminator"),
            Err(DirtyStatusError::Malformed)
        ));
        assert!(matches!(
            parse_dirty_paths(b"broken\0"),
            Err(DirtyStatusError::Malformed)
        ));
    }

    #[test]
    fn dirty_status_command_disables_optional_locks_and_fsmonitor() {
        let mut scrub_probe = Command::new("git");
        scrub_probe
            .env("GIT_DIR", "/outside")
            .env("GIT_CONFIG_COUNT", "1");
        scrub_git_status_env(&mut scrub_probe);
        assert!(scrub_probe.get_envs().all(|(name, value)| {
            !matches!(name.to_str(), Some("GIT_DIR" | "GIT_CONFIG_COUNT")) || value.is_none()
        }));

        let command = dirty_status_command(Path::new("/repo"));
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
                "-C",
                "/repo",
                "status",
                "--porcelain=v1",
                "-z",
                "--no-renames",
                "--untracked-files=all",
            ]
        );
        assert!(command.get_envs().any(|(name, value)| {
            name == "GIT_OPTIONAL_LOCKS" && value == Some(std::ffi::OsStr::new("0"))
        }));
    }

    #[test]
    fn dirty_status_counts_paths_without_writing_the_index() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = temp_test_dir("dirty-count-index-custody");
        run_git(&root, &["init", "--quiet"]);
        set_repo_identity(&root);
        std::fs::write(root.join("tracked"), "initial\n").unwrap();
        std::fs::write(root.join("deleted"), "initial\n").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "--quiet", "-m", "initial"]);

        std::fs::write(root.join("tracked"), "changed\n").unwrap();
        run_git(&root, &["add", "tracked"]);
        std::fs::write(root.join("tracked"), "changed again\n").unwrap();
        std::fs::remove_file(root.join("deleted")).unwrap();
        std::fs::create_dir_all(root.join("untracked/nested")).unwrap();
        std::fs::write(root.join("untracked/one"), "one\n").unwrap();
        std::fs::write(root.join("untracked/nested/two"), "two\n").unwrap();
        std::fs::write(root.join("ignored"), "ignored\n").unwrap();

        let index = root.join(".git/index");
        let git_dir = root.join(".git");
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let index_before = std::fs::read(&index).unwrap();
        let metadata_before = std::fs::metadata(&index).unwrap();
        let digest_before = Sha256::digest(&index_before);
        assert_eq!(dirty_paths(&root).unwrap(), 4);
        let metadata_after = std::fs::metadata(&index).unwrap();
        let index_after = std::fs::read(&index).unwrap();
        assert_eq!(index_after, index_before);
        assert_eq!(Sha256::digest(&index_after), digest_before);
        assert_eq!(metadata_after.ino(), metadata_before.ino());
        assert_eq!(metadata_after.mode(), metadata_before.mode());
        assert_eq!(metadata_after.len(), metadata_before.len());
        assert_eq!(metadata_after.mtime(), metadata_before.mtime());
        assert_eq!(metadata_after.mtime_nsec(), metadata_before.mtime_nsec());
        assert_eq!(metadata_after.ctime(), metadata_before.ctime());
        assert_eq!(metadata_after.ctime_nsec(), metadata_before.ctime_nsec());
        assert!(!root.join(".git/index.lock").exists());

        std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pure_jj_checkout_hides_git_status_without_spawning_dirty_query() {
        let root = temp_test_dir("dirty-pure-jj");
        std::fs::create_dir_all(root.join(".jj/repo")).unwrap();

        let (snapshot, cache) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            None,
            GitStatusRefreshDemand::ALL,
            Instant::now(),
            |_| panic!("pure jj checkout must not launch git status"),
        );

        assert_eq!(snapshot.space, None);
        assert_eq!(snapshot.branch, None);
        assert_eq!(snapshot.ahead_behind, None);
        assert_eq!(snapshot.dirty_paths, None);
        assert!(cache.is_some_and(|entry| entry.fingerprint.is_none()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn colocated_git_and_jj_checkout_uses_git_dirty_status() {
        let root = temp_test_dir("dirty-git-jj");
        write_fake_tracked_repo(&root);
        std::fs::create_dir_all(root.join(".jj/repo")).unwrap();

        let (snapshot, cache) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            None,
            GitStatusRefreshDemand::ALL,
            Instant::now(),
            |_| Ok(2),
        );

        assert!(snapshot.space.is_some());
        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.dirty_paths, Some(2));
        assert!(cache.is_some_and(|entry| entry.fingerprint.is_some()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dirty_status_uses_five_second_success_cadence() {
        let root = temp_test_dir("dirty-success-cadence");
        write_fake_tracked_repo(&root);
        let demand = GitStatusRefreshDemand {
            branch: false,
            ahead_behind: false,
            dirty_paths: true,
            dirty_paths_result: None,
        };
        let now = Instant::now();
        let (first, cache) =
            git_status_snapshot_for_cwd_with_demand_at(&root, None, demand, now, |_| Ok(2));
        assert_eq!(first.dirty_paths, Some(2));
        let cache = cache.unwrap();

        let (cached, cache) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            Some(&cache),
            demand,
            now + Duration::from_millis(4_999),
            |_| panic!("dirty query ran before the five-second deadline"),
        );
        assert_eq!(cached.dirty_paths, Some(2));
        let (refreshed, _) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            cache.as_ref(),
            demand,
            now + Duration::from_secs(5),
            |_| Ok(3),
        );
        assert_eq!(refreshed.dirty_paths, Some(3));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dirty_status_is_not_queried_without_configured_demand() {
        let root = temp_test_dir("dirty-not-demanded");
        write_fake_tracked_repo(&root);
        let (snapshot, _) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            None,
            GitStatusRefreshDemand {
                branch: true,
                ahead_behind: false,
                dirty_paths: false,
                dirty_paths_result: None,
            },
            Instant::now(),
            |_| panic!("dirty query ran without a configured git_status token"),
        );
        assert_eq!(snapshot.dirty_paths, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unchanged_head_still_refreshes_due_dirty_paths() {
        let root = temp_test_dir("dirty-unchanged-head");
        run_git(&root, &["init", "--quiet"]);
        set_repo_identity(&root);
        run_git(
            &root,
            &["commit", "--quiet", "--allow-empty", "-m", "initial"],
        );
        let now = Instant::now();
        let (clean, cache) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            None,
            GitStatusRefreshDemand::ALL,
            now,
            dirty_paths,
        );
        assert_eq!(clean.dirty_paths, Some(0));
        let fingerprint = cache.as_ref().unwrap().fingerprint.clone();

        std::fs::write(root.join("new"), "dirty\n").unwrap();
        let (dirty, updated) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            cache.as_ref(),
            GitStatusRefreshDemand::ALL,
            now + GIT_DIRTY_STATUS_REFRESH_INTERVAL,
            dirty_paths,
        );
        assert_eq!(dirty.dirty_paths, Some(1));
        assert_eq!(updated.as_ref().unwrap().fingerprint, fingerprint);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dirty_status_counts_rename_sides_and_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;

        let root = temp_test_dir("dirty-nul-names");
        run_git(&root, &["init", "--quiet"]);
        set_repo_identity(&root);
        std::fs::write(root.join("old"), "tracked\n").unwrap();
        run_git(&root, &["add", "old"]);
        run_git(&root, &["commit", "--quiet", "-m", "initial"]);
        run_git(&root, &["mv", "old", "new"]);
        for name in ["with space", "with\ttab", "with\nnewline", "unicode-界"] {
            std::fs::write(root.join(name), "untracked\n").unwrap();
        }
        std::fs::write(
            root.join(std::ffi::OsString::from_vec(vec![
                b'n', b'o', b'n', b'-', 0x80,
            ])),
            "untracked\n",
        )
        .unwrap();

        assert_eq!(dirty_paths(&root).unwrap(), 7);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn run_dirty_pipe_fixture(mode: &str) -> (Result<usize, DirtyStatusError>, Duration, bool) {
        let fixture = crate::workspace::DirtyPipeFixture::new(mode);
        let started = Instant::now();
        let result = dirty_paths(&fixture.repo);
        let elapsed = started.elapsed();
        let descendant_alive = fixture.descendant_alive();
        (result, elapsed, descendant_alive)
    }

    fn assert_dirty_pipe_timeout(mode: &str, expect_descendant_alive: bool) {
        assert_eq!(GIT_DIRTY_STATUS_TIMEOUT, Duration::from_millis(250));
        assert_eq!(GIT_DIRTY_STATUS_CLEANUP_GRACE, Duration::from_millis(250));
        let (result, elapsed, descendant_alive) = run_dirty_pipe_fixture(mode);
        assert!(
            matches!(result, Err(DirtyStatusError::Timeout)),
            "direct-child exit without {mode} EOF must remain a Timeout: {result:?}"
        );
        assert!(
            elapsed
                <= GIT_DIRTY_STATUS_TIMEOUT
                    + GIT_DIRTY_STATUS_CLEANUP_GRACE
                    + Duration::from_millis(500),
            "dirty query exceeded deadline plus cleanup grace: {elapsed:?}"
        );
        assert_eq!(
            descendant_alive, expect_descendant_alive,
            "unexpected descendant state after {mode} custody"
        );
    }

    #[test]
    fn dirty_status_deadline_covers_stdout_eof_after_direct_child_exit() {
        assert_dirty_pipe_timeout("stdout", false);
    }

    #[test]
    fn dirty_status_deadline_covers_stderr_eof_after_direct_child_exit() {
        assert_dirty_pipe_timeout("stderr", false);
    }

    #[test]
    fn dirty_status_deadline_does_not_wait_for_escaped_descendant_eof() {
        assert_dirty_pipe_timeout("escaped", true);
    }

    #[test]
    fn dirty_status_keeps_direct_child_reserved_until_group_cleanup() {
        let fixture = crate::workspace::DirtyPipeFixture::new("escaped-custody");
        let repo = fixture.repo.clone();
        let started = Instant::now();
        let query = std::thread::spawn(move || dirty_paths(&repo));

        let sentinel_deadline = started + Duration::from_millis(100);
        while fixture.direct_pid().is_none() && Instant::now() < sentinel_deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let direct_pid = fixture
            .direct_pid()
            .expect("fixture must report the direct child before releasing it");
        fixture.release_direct_child();
        let zombie_deadline = started + Duration::from_millis(150);
        let mut state_while_pipe_is_held = fixture.direct_child_state();
        while state_while_pipe_is_held != Some('Z')
            && !query.is_finished()
            && Instant::now() < zombie_deadline
        {
            std::thread::sleep(Duration::from_millis(1));
            state_while_pipe_is_held = fixture.direct_child_state();
        }

        let result = query.join().expect("dirty query thread must not panic");
        let elapsed = started.elapsed();
        let direct_child_exists_after_return = fixture.direct_child_exists();

        assert!(
            matches!(result, Err(DirtyStatusError::Timeout)),
            "escaped descendant pipe must keep the query in Timeout: {result:?}"
        );
        assert_eq!(
            state_while_pipe_is_held,
            Some('Z'),
            "direct child {direct_pid} must remain a zombie until group cleanup"
        );
        assert!(
            !direct_child_exists_after_return,
            "direct child {direct_pid} must be reaped after group cleanup"
        );
        assert!(
            elapsed
                <= GIT_DIRTY_STATUS_TIMEOUT
                    + GIT_DIRTY_STATUS_CLEANUP_GRACE
                    + Duration::from_millis(500),
            "custody query exceeded deadline plus cleanup grace: {elapsed:?}"
        );
        assert!(
            fixture.descendant_alive(),
            "escaped descendant should remain outside the original group"
        );
    }

    #[test]
    fn dirty_status_malformed_output_signals_group_before_reaping() {
        let fixture = crate::workspace::DirtyPipeFixture::new("malformed-custody");
        let started = Instant::now();
        let result = dirty_paths(&fixture.repo);
        let elapsed = started.elapsed();
        let direct_pid = fixture
            .direct_pid()
            .expect("fixture must report the direct child");

        assert!(
            matches!(result, Err(DirtyStatusError::Malformed)),
            "status-zero malformed output must remain Malformed: {result:?}"
        );
        assert_eq!(
            fixture.validation_direct_child_state(),
            Some('Z'),
            "malformed output must be validated while direct child {direct_pid} is still a zombie"
        );
        assert!(
            !fixture.direct_child_exists(),
            "direct child {direct_pid} must be reaped exactly once after validation cleanup"
        );
        assert!(
            !fixture.descendant_alive(),
            "malformed-output cleanup must terminate the same-group descendant"
        );
        assert!(
            elapsed
                <= GIT_DIRTY_STATUS_TIMEOUT
                    + GIT_DIRTY_STATUS_CLEANUP_GRACE
                    + Duration::from_millis(500),
            "malformed-output cleanup exceeded deadline plus cleanup grace: {elapsed:?}"
        );
    }

    #[test]
    fn dirty_status_failure_keeps_last_good_and_backs_off_thirty_seconds() {
        let root = temp_test_dir("dirty-failure-backoff");
        write_fake_tracked_repo(&root);
        let demand = GitStatusRefreshDemand {
            branch: false,
            ahead_behind: false,
            dirty_paths: true,
            dirty_paths_result: None,
        };
        let now = Instant::now();
        let (_, cache) =
            git_status_snapshot_for_cwd_with_demand_at(&root, None, demand, now, |_| Ok(2));
        let cache = cache.unwrap();
        let (failed, cache) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            Some(&cache),
            demand,
            now + Duration::from_secs(5),
            |_| Err(DirtyStatusError::Timeout),
        );
        assert_eq!(failed.dirty_paths, Some(2));
        let cache = cache.unwrap();

        let (backed_off, cache) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            Some(&cache),
            demand,
            now + Duration::from_secs(34),
            |_| panic!("dirty query ran during the failure backoff"),
        );
        assert_eq!(backed_off.dirty_paths, Some(2));
        let (recovered, _) = git_status_snapshot_for_cwd_with_demand_at(
            &root,
            cache.as_ref(),
            demand,
            now + Duration::from_secs(35),
            |_| Ok(4),
        );
        assert_eq!(recovered.dirty_paths, Some(4));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_dirty_failure_class_hides_first_value_and_uses_failure_backoff() {
        for (index, error) in [
            DirtyStatusError::Spawn,
            DirtyStatusError::Wait,
            DirtyStatusError::Timeout,
            DirtyStatusError::Output,
            DirtyStatusError::Exit,
            DirtyStatusError::Malformed,
        ]
        .into_iter()
        .enumerate()
        {
            let root = temp_test_dir(&format!("dirty-first-failure-{index}"));
            write_fake_tracked_repo(&root);
            let now = Instant::now();
            let (snapshot, cache) = git_status_snapshot_for_cwd_with_demand_at(
                &root,
                None,
                GitStatusRefreshDemand::ALL,
                now,
                |_| Err(error),
            );
            assert_eq!(snapshot.dirty_paths, None);
            assert_eq!(
                cache.unwrap().dirty_refresh_after,
                Some(now + GIT_DIRTY_STATUS_FAILURE_BACKOFF)
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn git_status_reuses_cached_ahead_behind_when_fingerprint_matches() {
        let root = temp_test_dir("cache-hit");
        write_fake_tracked_repo(&root);
        let fingerprint = git_status_fingerprint(&root).unwrap();
        let cached = GitStatusCacheEntry {
            fingerprint: Some(fingerprint),
            retry_after: None,
            dirty_refresh_after: None,
            snapshot: WorkspaceGitStatusSnapshot {
                auto_label: "repo".into(),
                branch: Some("main".into()),
                ahead_behind: Some((2, 1)),
                dirty_paths: None,
                space: git_space_metadata(&root),
            },
        };

        let (snapshot, update) = git_status_snapshot_for_cwd(&root, Some(&cached));

        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.ahead_behind, Some((2, 1)));
        assert_eq!(update.unwrap().snapshot.ahead_behind, Some((2, 1)));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_does_not_reuse_cache_when_branch_changes_at_same_oid() {
        let root = temp_test_dir("branch-switch");
        write_fake_tracked_repo(&root);
        let fingerprint = git_status_fingerprint(&root).unwrap();
        let cached = GitStatusCacheEntry {
            fingerprint: Some(fingerprint),
            retry_after: None,
            dirty_refresh_after: None,
            snapshot: WorkspaceGitStatusSnapshot {
                auto_label: "repo".into(),
                branch: Some("main".into()),
                ahead_behind: Some((4, 0)),
                dirty_paths: None,
                space: git_space_metadata(&root),
            },
        };
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
        std::fs::write(
            root.join(".git/refs/heads/feature"),
            "1111111111111111111111111111111111111111\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".git/config"),
            "[branch \"feature\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

        let (snapshot, _) = git_status_snapshot_for_cwd(&root, Some(&cached));

        assert_eq!(snapshot.branch.as_deref(), Some("feature"));
        assert_eq!(snapshot.ahead_behind, None);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_clears_ahead_behind_when_upstream_is_unset() {
        let root = temp_test_dir("upstream-unset");
        write_fake_tracked_repo(&root);
        let fingerprint = git_status_fingerprint(&root).unwrap();
        let cached = GitStatusCacheEntry {
            fingerprint: Some(fingerprint),
            retry_after: None,
            dirty_refresh_after: None,
            snapshot: WorkspaceGitStatusSnapshot {
                auto_label: "repo".into(),
                branch: Some("main".into()),
                ahead_behind: Some((0, 3)),
                dirty_paths: None,
                space: git_space_metadata(&root),
            },
        };
        std::fs::write(root.join(".git/config"), "").unwrap();

        let (snapshot, _) = git_status_snapshot_for_cwd(&root, Some(&cached));

        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.ahead_behind, None);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_rebuilds_config_when_missing_include_appears() {
        let root = temp_test_dir("include-appears");
        write_fake_tracked_repo(&root);
        std::fs::write(
            root.join(".git/config"),
            "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[include]\n\tpath = branch.cfg\n",
        )
        .unwrap();
        let (_, cached) = git_status_snapshot_for_cwd(&root, None);
        std::fs::write(
            root.join(".git/branch.cfg"),
            "[branch \"main\"]\n\tremote = fork\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

        let (_, updated) = git_status_snapshot_for_cwd(&root, cached.as_ref());

        let upstream = updated.unwrap().fingerprint.unwrap().upstream.unwrap();
        assert_eq!(upstream.remote, "fork");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_fingerprint_reads_packed_refs() {
        let root = temp_test_dir("packed-refs");
        write_fake_tracked_repo(&root);
        std::fs::remove_file(root.join(".git/refs/remotes/origin/main")).unwrap();
        std::fs::write(
            root.join(".git/packed-refs"),
            "# pack-refs with: peeled fully-peeled sorted\n2222222222222222222222222222222222222222 refs/remotes/origin/main\n",
        )
        .unwrap();

        let fingerprint = git_status_fingerprint(&root).unwrap();

        assert_eq!(
            fingerprint.upstream.unwrap().oid.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_worktree_refresh_keeps_checkout_name_as_auto_label() {
        let (base, _, checkout) =
            crate::workspace::git::test_support::create_repo_with_linked_worktree(
                "linked-refresh-label",
            );
        let (snapshot, _) = git_status_snapshot_for_cwd(&checkout, None);
        assert_eq!(snapshot.auto_label, "topic");
        assert_eq!(snapshot.space.unwrap().repo_name, "repo");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn git_status_cache_key_is_per_linked_worktree_checkout() {
        let base = temp_test_dir("linked-worktree-keys");
        let common_dir = base.join("repo/.git");
        let worktree_one = base.join("one");
        let worktree_two = base.join("two");
        let git_dir_one = common_dir.join("worktrees/one");
        let git_dir_two = common_dir.join("worktrees/two");
        std::fs::create_dir_all(&git_dir_one).unwrap();
        std::fs::create_dir_all(&git_dir_two).unwrap();
        std::fs::create_dir_all(&worktree_one).unwrap();
        std::fs::create_dir_all(&worktree_two).unwrap();
        std::fs::write(
            worktree_one.join(".git"),
            format!("gitdir: {}\n", git_dir_one.display()),
        )
        .unwrap();
        std::fs::write(
            worktree_two.join(".git"),
            format!("gitdir: {}\n", git_dir_two.display()),
        )
        .unwrap();
        std::fs::write(git_dir_one.join("HEAD"), "ref: refs/heads/one\n").unwrap();
        std::fs::write(git_dir_two.join("HEAD"), "ref: refs/heads/two\n").unwrap();

        assert_ne!(
            git_status_cache_key(&worktree_one),
            git_status_cache_key(&worktree_two)
        );

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn git_status_fingerprint_reads_reftable_branch_identity() {
        let root = temp_test_dir("reftable-fingerprint");
        let root_arg = root.to_string_lossy().to_string();
        let mut command = std::process::Command::new("git");
        scrub_git_env(&mut command);
        let output = command
            .args(["init", "--ref-format=reftable", "-b", "main", &root_arg])
            .output()
            .unwrap();
        if !output.status.success() {
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
        set_repo_identity(&root);
        run_git(&root, &["commit", "--allow-empty", "-m", "initial"]);

        let fingerprint = git_status_fingerprint(&root).unwrap();

        assert_eq!(
            fingerprint.head,
            GitHeadIdentity::Branch {
                full_ref: "refs/heads/main".into(),
                short_name: "main".into(),
                oid: git_rev_parse_verify(&root, "HEAD"),
            }
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_recomputes_ahead_behind_when_head_moves() {
        let base = temp_test_dir("head-moves");
        let remote = base.join("remote.git");
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let remote_arg = remote.to_string_lossy().to_string();
        run_git(&base, &["init", "--bare", &remote_arg]);
        run_git(&repo, &["init"]);
        set_repo_identity(&repo);
        run_git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
        run_git(&repo, &["branch", "-M", "main"]);
        run_git(&repo, &["remote", "add", "origin", &remote_arg]);
        run_git(&repo, &["push", "-u", "origin", "main"]);

        let (initial, cache_entry) = git_status_snapshot_for_cwd(&repo, None);
        assert_eq!(initial.ahead_behind, Some((0, 0)));
        run_git(&repo, &["commit", "--allow-empty", "-m", "ahead"]);

        let (updated, _) = git_status_snapshot_for_cwd(&repo, cache_entry.as_ref());

        assert_eq!(updated.branch.as_deref(), Some("main"));
        assert_eq!(updated.ahead_behind, Some((1, 0)));

        std::fs::remove_dir_all(base).unwrap();
    }
}
