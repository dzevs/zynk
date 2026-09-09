use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn temp_test_dir(name: &str) -> PathBuf {
    let unique = format!(
        "zynk-workspace-tests-{}-{}-{}",
        name,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&path).unwrap();
    path
}

pub(super) fn write_fake_tracked_repo(root: &Path) {
    let head_oid = "1111111111111111111111111111111111111111";
    let upstream_oid = "2222222222222222222222222222222222222222";
    std::fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(root.join(".git/refs/heads/main"), format!("{head_oid}\n")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/origin/main"),
        format!("{upstream_oid}\n"),
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
    )
    .unwrap();
}

/// Fixture git commands must never inherit the caller's git environment. With `GIT_DIR`
/// exported, `git init` initialises the directory that variable names, leaves
/// `<fixture>/.git` absent and still exits 0; every later `git -C <fixture> ...` then
/// silently reads and writes the OUTER repository. Neutralising the global/system config
/// keeps the host's own git settings out of the fixture as well.
pub(super) fn fixture_git_command() -> std::process::Command {
    let mut command = std::process::Command::new("git");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(key);
    }
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_SYSTEM", "/dev/null");
    command
}

/// Path of `repo`'s own config file, asserting first that `repo` really is a repository.
/// `git config` WALKS UP to the nearest parent repository, so an identity written into a
/// fixture whose `git init` did not take lands in a real checkout's `.git/config`.
fn fixture_git_config_path(repo: &std::path::Path) -> std::path::PathBuf {
    assert!(
        repo.join(".git").exists(),
        "fixture repo has no .git, refusing to write a git config that would escape into a parent repository: {}",
        repo.display()
    );
    let output = fixture_git_command()
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git rev-parse --git-common-dir failed for {}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()).join("config")
}

/// Seed the fixture identity so the write cannot travel: `--file` names the repository's
/// own config file and never walks up to a parent repository.
pub(super) fn seed_fixture_identity(repo: &std::path::Path) {
    let config = fixture_git_config_path(repo);
    let config = config.to_string_lossy().into_owned();
    run_git(
        repo,
        &[
            "config",
            "--file",
            &config,
            "user.email",
            "zynk@example.invalid",
        ],
    );
    run_git(
        repo,
        &["config", "--file", &config, "user.name", "Zynk Test"],
    );
}

pub(super) fn run_git(cwd: &Path, args: &[&str]) {
    let output = fixture_git_command()
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
