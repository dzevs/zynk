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

pub(super) fn run_git(cwd: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    scrub_git_env(&mut command);
    let output = command.arg("-C").arg(cwd).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Strip every inherited git environment variable that could point a fixture's git command at
/// ANOTHER repository, and pin the config files it may read.
///
/// The arbiter's reproduction: with `GIT_DIR` inherited, `git init` inside a child directory exits
/// 0 while leaving `child/.git` ABSENT — git initialises the directory `GIT_DIR` names — and the
/// `git -C child config …` that follows exits 0 too, writing into the OUTER repository, because
/// `git config` walks up to the nearest parent repository. Every status code is success, so nothing
/// in the fixture notices, and a real checkout is left authoring commits as
/// `Zynk Test <zynk@example.invalid>`. A sanitised caller environment hides this, so a fixture
/// cannot rely on having one.
pub(super) fn scrub_git_env(command: &mut std::process::Command) -> &mut std::process::Command {
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
}

/// Seed the fixture repository's commit identity, with the write unable to leave the fixture.
///
/// Containment is asserted BEFORE the write — a `git init` that did not take aborts the test rather
/// than escaping — and the write then names the config file explicitly, which cannot walk up at all.
pub(super) fn set_repo_identity(repo: &Path) {
    let dot_git = repo.join(".git");
    assert!(
        dot_git.exists(),
        "fixture repository was not initialised, so a config write would escape into a parent repository: {}",
        repo.display()
    );
    let config_path = if dot_git.is_dir() {
        dot_git.join("config")
    } else {
        // A linked worktree or a submodule: `.git` is a FILE naming the real gitdir.
        let mut command = std::process::Command::new("git");
        scrub_git_env(&mut command);
        let output = command
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "could not resolve the git dir of {}",
            repo.display()
        );
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("config")
    };
    let config_path = config_path.to_string_lossy().into_owned();
    for (key, value) in [
        ("user.email", "zynk@example.invalid"),
        ("user.name", "Zynk Test"),
    ] {
        run_git(repo, &["config", "--file", &config_path, key, value]);
    }
}

/// Sets `GIT_DIR` for this process for the duration of the containment probe and removes it again
/// even if an assertion fires, so a failure cannot leak the arbiter's trigger into the rest of the
/// run. `cargo nextest` gives each test its own process; the guard makes it safe regardless.
struct GitDirEnvGuard;

impl Drop for GitDirEnvGuard {
    fn drop(&mut self) {
        std::env::remove_var("GIT_DIR");
    }
}

#[test]
fn a_fixture_git_command_cannot_reach_an_outer_repository() {
    // The arbiter's exact trigger: with `GIT_DIR` inherited, `git init` inside a child directory
    // exits 0 while leaving `child/.git` ABSENT, and the `git -C child config …` that follows exits
    // 0 too, writing into the OUTER repository. Every status code is success, so nothing in a
    // fixture notices — which is how a real checkout twice ended up authoring commits as
    // `Zynk Test <zynk@example.invalid>`.
    let outer = temp_test_dir("fixture-containment-outer");
    let mut init = std::process::Command::new("git");
    scrub_git_env(&mut init);
    assert!(init
        .arg("-C")
        .arg(&outer)
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    let outer_config = outer.join(".git/config");
    let before = std::fs::read_to_string(&outer_config).unwrap();

    // From here every git command a fixture spawns inherits `GIT_DIR` naming the OUTER repository.
    std::env::set_var("GIT_DIR", outer.join(".git"));
    let _guard = GitDirEnvGuard;

    // The scrub: `git init` really initialises the child, rather than the directory `GIT_DIR` names.
    let child = outer.join("child");
    std::fs::create_dir_all(&child).unwrap();
    run_git(&child, &["init", "--quiet"]);
    assert!(
        child.join(".git").is_dir(),
        "the scrub must let `git init` initialise the child itself"
    );

    // The containment assert: a fixture whose init did NOT take aborts instead of escaping.
    let skipped = outer.join("skipped");
    std::fs::create_dir_all(&skipped).unwrap();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let escaped =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| set_repo_identity(&skipped)));
    std::panic::set_hook(previous_hook);
    let payload =
        escaped.expect_err("a config write into an uninitialised fixture must abort the test");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        message.contains("was not initialised"),
        "the panic must name the containment failure: {message}"
    );

    // The contained write lands in the child's own config, and the outer repository is untouched by
    // either the refused write or the accepted one.
    set_repo_identity(&child);
    assert!(std::fs::read_to_string(child.join(".git/config"))
        .unwrap()
        .contains("zynk@example.invalid"));
    assert_eq!(
        std::fs::read_to_string(&outer_config).unwrap(),
        before,
        "the outer repository's config must be byte-identical"
    );

    std::fs::remove_dir_all(&outer).unwrap();
}
