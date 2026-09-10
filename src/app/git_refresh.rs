// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use super::{App, GIT_REMOTE_STATUS_REFRESH_INTERVAL, GIT_REPO_DISCOVERY_REFRESH_INTERVAL};
use crate::events::AppEvent;
use crate::workspace::{GitStatusCacheEntry, GitStatusRefreshDemand, WorkspaceGitStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshItem {
    workspace_id: String,
    resolved_identity_cwd: PathBuf,
    cache_key_hint: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshTarget {
    workspace_id: String,
    resolved_identity_cwd: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshJob {
    cache_key: PathBuf,
    cached: Option<GitStatusCacheEntry>,
    targets: Vec<WorkspaceGitRefreshTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshOutput {
    results: Vec<WorkspaceGitStatus>,
    cache_updates: Vec<(PathBuf, GitStatusCacheEntry)>,
}

impl App {
    pub(crate) fn start_git_status_refresh_if_due(&mut self, now: Instant) {
        let Some(deadline) = self.git_refresh_deadline() else {
            return;
        };

        if now < deadline {
            return;
        }

        let refresh_repo_discovery = self.git_identity_refresh_requested
            || now.saturating_duration_since(self.last_git_repo_discovery_refresh)
                >= GIT_REPO_DISCOVERY_REFRESH_INTERVAL;
        let workspaces = self.workspace_git_refresh_items(refresh_repo_discovery);

        if workspaces.is_empty() {
            self.last_git_remote_status_refresh = now;
            self.git_identity_refresh_requested = false;
            return;
        }

        self.git_refresh_in_flight = true;
        let event_tx = self.event_tx.clone();
        let cache = self.git_status_cache.clone();
        let mut demand = self.git_refresh_demand();
        if self.git_identity_refresh_requested {
            demand.branch = true;
        }
        self.git_identity_refresh_requested = false;
        if refresh_repo_discovery {
            self.last_git_repo_discovery_refresh = now;
        }
        std::thread::spawn(move || {
            let output =
                refresh_workspace_git_statuses_with_cache_and_demand(workspaces, &cache, demand);
            let _ = event_tx.blocking_send(AppEvent::GitStatusRefreshed {
                results: output.results,
                cache_updates: output.cache_updates,
            });
        });
    }

    pub(crate) fn request_git_identity_refresh(&mut self, now: Instant) {
        self.git_identity_refresh_requested = true;
        self.mark_git_status_refresh_due(now);
    }

    pub(crate) fn mark_git_status_refresh_due(&mut self, now: Instant) {
        self.git_status_cache
            .retain(|_, entry| entry.fingerprint.is_some());
        if self.git_refresh_in_flight {
            self.git_refresh_due_after_in_flight = true;
            return;
        }
        self.last_git_remote_status_refresh = now
            .checked_sub(GIT_REMOTE_STATUS_REFRESH_INTERVAL)
            .unwrap_or(now);
        self.git_refresh_due_after_in_flight = false;
    }

    pub(crate) fn git_refresh_deadline(&self) -> Option<Instant> {
        (!self.git_refresh_in_flight
            && !self.state.workspaces.is_empty()
            && (self.git_identity_refresh_requested || !self.git_refresh_demand().is_empty()))
        .then_some(self.last_git_remote_status_refresh + GIT_REMOTE_STATUS_REFRESH_INTERVAL)
    }

    fn git_refresh_demand(&self) -> GitStatusRefreshDemand {
        // The fixed sidebar branch row consumes both until configurable rows are ported.
        GitStatusRefreshDemand {
            branch: true,
            ahead_behind: true,
        }
    }

    fn workspace_git_refresh_items(
        &self,
        refresh_repo_discovery: bool,
    ) -> Vec<WorkspaceGitRefreshItem> {
        self.state
            .workspaces
            .iter()
            .filter_map(|ws| {
                let cwd =
                    ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)?;
                let cache_key_hint = (!refresh_repo_discovery && ws.cached_identity_cwd == cwd)
                    .then(|| ws.cached_git_status_key.clone());
                Some(WorkspaceGitRefreshItem {
                    workspace_id: ws.id.clone(),
                    resolved_identity_cwd: cwd,
                    cache_key_hint,
                })
            })
            .collect()
    }
}

fn deduplicate_git_refresh_items(
    items: Vec<WorkspaceGitRefreshItem>,
    cache: &HashMap<PathBuf, GitStatusCacheEntry>,
) -> Vec<WorkspaceGitRefreshJob> {
    let mut indexes = HashMap::<PathBuf, usize>::new();
    let mut jobs = Vec::<WorkspaceGitRefreshJob>::new();

    for item in items {
        let cache_key = item.cache_key_hint.unwrap_or_else(|| {
            crate::workspace::git_status_cache_key(&item.resolved_identity_cwd)
                .unwrap_or_else(|| item.resolved_identity_cwd.clone())
        });
        let target = WorkspaceGitRefreshTarget {
            workspace_id: item.workspace_id,
            resolved_identity_cwd: item.resolved_identity_cwd,
        };
        if let Some(&index) = indexes.get(&cache_key) {
            jobs[index].targets.push(target);
            continue;
        }

        let cached = cache.get(&cache_key).cloned();
        indexes.insert(cache_key.clone(), jobs.len());
        jobs.push(WorkspaceGitRefreshJob {
            cache_key,
            cached,
            targets: vec![target],
        });
    }

    jobs
}

fn refresh_workspace_git_statuses_with_cache_and_demand(
    items: Vec<WorkspaceGitRefreshItem>,
    cache: &HashMap<PathBuf, GitStatusCacheEntry>,
    demand: GitStatusRefreshDemand,
) -> WorkspaceGitRefreshOutput {
    let mut results = Vec::new();
    let mut cache_updates = Vec::new();

    for job in deduplicate_git_refresh_items(items, cache) {
        let (snapshot, cache_entry) = crate::workspace::git_status_snapshot_for_cwd_with_demand(
            &job.cache_key,
            job.cached.as_ref(),
            demand,
        );
        if let Some(cache_entry) = cache_entry {
            cache_updates.push((job.cache_key.clone(), cache_entry));
        }
        results.extend(job.targets.into_iter().map(move |target| {
            snapshot.clone().into_workspace_status(
                target.workspace_id,
                target.resolved_identity_cwd,
                job.cache_key.clone(),
                demand,
            )
        }));
    }

    WorkspaceGitRefreshOutput {
        results,
        cache_updates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn git_refresh_deduplicates_workspaces_with_same_cache_key() {
        let repo =
            std::env::temp_dir().join(format!("zynk-git-refresh-dedupe-{}", std::process::id()));
        let nested = repo.join("nested");
        let other = repo.join("other");
        std::fs::create_dir_all(&nested).expect("create nested dir");
        std::fs::create_dir_all(&other).expect("create other dir");
        let mut command = std::process::Command::new("git");
        crate::workspace::scrub_git_env(&mut command);
        let output = command
            .arg("-C")
            .arg(&repo)
            .arg("init")
            .output()
            .expect("run git init");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            repo.join(".git").is_dir(),
            "fixture init left no .git: {}",
            repo.display()
        );

        let output = refresh_workspace_git_statuses_with_cache_and_demand(
            vec![
                WorkspaceGitRefreshItem {
                    workspace_id: "one".into(),
                    resolved_identity_cwd: nested.clone(),
                    cache_key_hint: None,
                },
                WorkspaceGitRefreshItem {
                    workspace_id: "two".into(),
                    resolved_identity_cwd: other.clone(),
                    cache_key_hint: None,
                },
            ],
            &HashMap::new(),
            GitStatusRefreshDemand::ALL,
        );

        assert_eq!(output.cache_updates.len(), 1);
        assert_eq!(
            output.cache_updates[0].0,
            std::fs::canonicalize(&repo).expect("canonical repo path")
        );
        assert_eq!(output.results.len(), 2);
        assert_eq!(output.results[0].workspace_id, "one");
        assert_eq!(output.results[0].resolved_identity_cwd, nested);
        assert_eq!(output.results[1].workspace_id, "two");
        assert_eq!(output.results[1].resolved_identity_cwd, other);

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn shared_root_repo_refresh_keeps_workspace_specific_fallback_labels() {
        let cache_key = PathBuf::from("/");
        let cached = GitStatusCacheEntry {
            fingerprint: None,
            retry_after: Some(Instant::now() + std::time::Duration::from_secs(30)),
            snapshot: crate::workspace::WorkspaceGitStatusSnapshot {
                auto_label: "/".into(),
                branch: Some("main".into()),
                ahead_behind: None,
                space: Some(crate::workspace::GitSpaceMetadata {
                    key: "/.git".into(),
                    checkout_key: "/".into(),
                    label: "repo".into(),
                    repo_root: cache_key.clone(),
                    is_linked_worktree: false,
                }),
            },
        };
        let items = ["alpha", "beta"]
            .into_iter()
            .map(|name| WorkspaceGitRefreshItem {
                workspace_id: name.into(),
                resolved_identity_cwd: cache_key.join(name),
                cache_key_hint: Some(cache_key.clone()),
            })
            .collect();

        let output = refresh_workspace_git_statuses_with_cache_and_demand(
            items,
            &HashMap::from([(cache_key, cached)]),
            GitStatusRefreshDemand::ALL,
        );

        assert_eq!(output.cache_updates.len(), 1);
        assert_eq!(output.results.len(), 2);
        assert_eq!(output.results[0].auto_label, "alpha");
        assert_eq!(output.results[1].auto_label, "beta");
        assert_eq!(output.results[0].branch.as_deref(), Some("main"));
        assert_eq!(output.results[1].branch.as_deref(), Some("main"));
    }

    #[test]
    fn git_refresh_item_collection_does_not_discover_uncached_cwd() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = std::env::temp_dir().join(format!("zynk-uncached-cwd-{}", std::process::id()));
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resolved_identity_cwd, cwd);
        assert_eq!(items[0].cache_key_hint, None);
    }

    #[test]
    fn git_refresh_item_collection_reuses_matching_cached_key() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = PathBuf::from("/repo/deep/nested");
        let cache_key = PathBuf::from("/repo");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.cached_identity_cwd = cwd;
        ws.cached_git_status_key = cache_key.clone();
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cache_key_hint, Some(cache_key));
    }

    #[test]
    fn periodic_repo_discovery_ignores_cached_key_hints() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = PathBuf::from("/repo/deep/nested");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.cached_identity_cwd = cwd;
        ws.cached_git_status_key = PathBuf::from("/repo");
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(true);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cache_key_hint, None);
    }

    #[test]
    fn headless_deadline_can_suppress_git_refresh_timer() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("test"));
        let now = Instant::now();
        app.last_git_remote_status_refresh = now - GIT_REMOTE_STATUS_REFRESH_INTERVAL;

        assert_eq!(
            app.next_headless_loop_deadline_with_git_refresh(now, false, false),
            None
        );
        assert_eq!(
            app.next_headless_loop_deadline_with_git_refresh(now, false, true),
            Some(now)
        );
    }

    #[test]
    fn explicit_git_refresh_invalidates_cached_non_git_results() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = std::env::temp_dir().join(format!("zynk-git-miss-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let (_, entry) = crate::workspace::git_status_snapshot_for_cwd_with_demand(
            &cwd,
            None,
            GitStatusRefreshDemand::ALL,
        );
        app.git_status_cache
            .insert(cwd.clone(), entry.expect("non-Git cache entry"));

        app.mark_git_status_refresh_due(Instant::now());

        assert!(app.git_status_cache.is_empty());
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn git_refresh_due_request_survives_in_flight_refresh() {
        let mut app = test_app(&crate::config::Config::default());
        let now = Instant::now();
        app.git_refresh_in_flight = true;

        app.mark_git_status_refresh_due(now);
        assert!(app.git_refresh_due_after_in_flight);

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            results: Vec::new(),
            cache_updates: Vec::new(),
        });

        assert!(!app.git_refresh_in_flight);
        assert!(!app.git_refresh_due_after_in_flight);
        assert_eq!(app.git_refresh_deadline(), None);

        app.state.workspaces.push(Workspace::test_new("test"));
        let deadline = app
            .git_refresh_deadline()
            .expect("refresh should be due once a workspace exists");
        assert!(deadline <= Instant::now());
    }

    #[test]
    fn git_refresh_items_use_cwd_cache_key_for_non_git_cwd() {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let cwd = std::env::temp_dir().join(format!("zynk-non-git-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("create temp cwd");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resolved_identity_cwd, cwd);
        assert_eq!(items[0].cache_key_hint, None);
        let jobs = deduplicate_git_refresh_items(items, &HashMap::new());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].cache_key, cwd);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn fixed_sidebar_refresh_keeps_branch_and_ahead_behind() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("test"));

        assert_eq!(app.git_refresh_demand(), GitStatusRefreshDemand::ALL);
        assert!(app.git_refresh_deadline().is_some());
        app.request_git_identity_refresh(Instant::now());
        assert!(app.git_identity_refresh_requested);
    }

    fn test_app(config: &crate::config::Config) -> super::super::App {
        super::super::App::new(
            config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }
}
