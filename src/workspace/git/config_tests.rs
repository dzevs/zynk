// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use super::config::*;
use crate::workspace::git::{
    discovery::git_worktree_info,
    status::git_status_fingerprint,
    test_support::{temp_test_dir, write_fake_tracked_repo},
};

#[test]
fn config_symlink_retarget_invalidates_context() {
    use std::os::unix::fs::symlink;

    let root = temp_test_dir("config-symlink-retarget");
    write_fake_tracked_repo(&root);
    let alias = root.join("branch.cfg");
    let first = root.join("first.cfg");
    let second = root.join("second.cfg");
    std::fs::write(&first, "").unwrap();
    std::fs::write(&second, "").unwrap();
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    for path in [&first, &second] {
        std::fs::File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }
    assert_eq!(stamp(first.clone(), None).1, stamp(second.clone(), None).1);
    symlink(&first, &alias).unwrap();
    let context = read_config_with_user_paths(
        &git_worktree_info(&root).unwrap(),
        "main",
        vec![alias.clone()],
    );
    std::fs::remove_file(&alias).unwrap();
    symlink(&second, &alias).unwrap();

    assert!(!deps_current(&context.2));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn config_read_error_retries_next_refresh() {
    let root = temp_test_dir("config-read-error");
    write_fake_tracked_repo(&root);
    std::fs::write(root.join(".git/config"), [0xff]).unwrap();
    let context = read_config(&git_worktree_info(&root).unwrap(), "main");
    assert!(!deps_current(&context.2));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn config_reader_reads_each_canonical_file_once() {
    let root = temp_test_dir("config-read-count");
    write_fake_tracked_repo(&root);
    let included = root.join(".git/included.cfg");
    std::fs::write(&included, "[branch \"main\"]\nremote = fork\n").unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[branch \"main\"]\nremote = origin\nmerge = refs/heads/main\n[include]\npath = included.cfg\npath = included.cfg\n",
    )
    .unwrap();
    let alias = root.join("alias.cfg");
    std::os::unix::fs::symlink(&included, &alias).unwrap();
    CONFIG_READ_COUNT.set(0);
    let context =
        read_config_with_user_paths(&git_worktree_info(&root).unwrap(), "main", vec![alias]);
    let reads = CONFIG_READ_COUNT.get();
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(context.1.unwrap().remote, "fork");
    assert_eq!(
        reads, 2,
        "three parse passes and repeated aliases reread files"
    );
}

#[test]
fn config_missing_file_is_reusable_until_it_appears() {
    let root = temp_test_dir("config-missing-dep");
    write_fake_tracked_repo(&root);
    let missing = root.join("missing.cfg");
    let context = read_config_with_user_paths(
        &git_worktree_info(&root).unwrap(),
        "main",
        vec![missing.clone()],
    );
    assert!(deps_current(&context.2));
    std::fs::write(missing, "").unwrap();
    let current = deps_current(&context.2);
    std::fs::remove_dir_all(root).unwrap();
    assert!(!current);
}

#[test]
fn config_metadata_permission_error_is_not_reusable() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_test_dir("config-metadata-permission");
    let parent = root.join("private");
    std::fs::create_dir(&parent).unwrap();
    let path = parent.join("config");
    std::fs::write(&path, "").unwrap();
    let permissions = std::fs::metadata(&parent).unwrap().permissions();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
    let inaccessible = std::fs::metadata(&path);
    let dep = stamp(path, None);
    let current = deps_current(std::slice::from_ref(&dep));
    std::fs::set_permissions(&parent, permissions).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    if inaccessible.is_ok() {
        eprintln!("UNEXERCISED: privileged process can traverse mode-000 ancestor");
        return;
    }
    assert_eq!(
        inaccessible.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(!dep.2);
    assert!(!current);
}

#[test]
fn git_status_fingerprint_honors_remote_fetch_refspec() {
    let root = temp_test_dir("custom-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/upstream")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/upstream/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/upstream/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.full_ref, "refs/remotes/upstream/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_included_config() {
    let root = temp_test_dir("included-config");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[include]\n\tpath = included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_branch_config_reads_user_config_before_repo_config() {
    let root = temp_test_dir("user-config");
    write_fake_tracked_repo(&root);
    let user_config = root.join("user.gitconfig");
    std::fs::write(root.join(".git/config"), "").unwrap();
    std::fs::write(
            &user_config,
            "[remote \"global\"]\n\tfetch = +refs/heads/*:refs/remotes/global/*\n[branch \"main\"]\n\tremote = global\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_config_with_user_paths(&info, "main", vec![user_config])
        .1
        .unwrap();

    assert_eq!(config.remote, "global");
    assert_eq!(
        upstream_full_ref(&config).as_deref(),
        Some("refs/remotes/global/main")
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_branch_config_repo_config_overrides_user_config() {
    let root = temp_test_dir("repo-overrides-user-config");
    write_fake_tracked_repo(&root);
    let user_config = root.join("user.gitconfig");
    std::fs::write(
            &user_config,
            "[remote \"global\"]\n\tfetch = +refs/heads/*:refs/remotes/global/*\n[branch \"main\"]\n\tremote = global\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_config_with_user_paths(&info, "main", vec![user_config])
        .1
        .unwrap();

    assert_eq!(config.remote, "origin");
    assert_eq!(
        upstream_full_ref(&config).as_deref(),
        Some("refs/remotes/origin/main")
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_applies_repeated_includes_in_order() {
    let root = temp_test_dir("repeated-include");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[include]\n\tpath = included.cfg\n[branch \"main\"]\n\tremote = middle\n[include]\n\tpath = included.cfg\n",
        )
        .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_matching_include_if_config() {
    let root = temp_test_dir("include-if-config");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        format!(
            "[includeIf \"gitdir:{}\"]\n\tpath = included.cfg\n",
            root.join(".git").display()
        ),
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_matches_gitdir_include_if_directory_pattern() {
    let base = temp_test_dir("include-if-dir");
    let root = base.join("work/repo");
    std::fs::create_dir_all(&root).unwrap();
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        format!(
            "[includeIf \"gitdir:{}/\"]\n\tpath = included.cfg\n",
            base.join("work").display()
        ),
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn git_status_fingerprint_reads_case_insensitive_config_keys() {
    let root = temp_test_dir("case-insensitive-config");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[Remote \"origin\"] # remote section\n\tFetch = +refs/heads/*:refs/remotes/origin/*\n[Branch \"main\"] ; branch section\n\tRemote = origin\n\tMerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_keeps_refspecs_for_later_remote_override() {
    let root = temp_test_dir("worktree-remote-override");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig = true\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_worktree_config_when_extension_disabled() {
    let root = temp_test_dir("worktree-config-disabled");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_accepts_git_boolean_worktree_config() {
    let root = temp_test_dir("worktree-config-boolean");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_uses_last_worktree_config_boolean() {
    let root = temp_test_dir("worktree-config-duplicate-boolean");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig = false\n\tworktreeConfig = true\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_included_worktree_config_extension() {
    let root = temp_test_dir("worktree-config-included-extension");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/fork")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/fork/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[include]\n\tpath = extension.cfg\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/extension.cfg"),
        "[extensions]\n\tworktreeConfig = true\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_onbranch_include_if_config() {
    let root = temp_test_dir("include-if-onbranch");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[includeIf \"onbranch:main\"]\n\tpath = included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_hasconfig_include_if_config() {
    let root = temp_test_dir("include-if-hasconfig");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = included.cfg\n",
        )
        .unwrap();
    std::fs::write(
            root.join(".git/included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_matches_user_hasconfig_against_repo_remote_url() {
    let root = temp_test_dir("include-if-hasconfig-user-repo-url");
    let user_config = root.join("user.gitconfig");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[remote \"fork\"]\n\turl = https://example.test/fork.git\n",
    )
    .unwrap();
    std::fs::write(
        &user_config,
        "[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = user-included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join("user-included.cfg"),
            "[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_config_with_user_paths(&info, "main", vec![user_config])
        .1
        .unwrap();

    assert_eq!(config.remote, "included");
    assert_eq!(config.merge_ref, "refs/heads/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_skips_hasconfig_include_that_defines_remote_url() {
    let root = temp_test_dir("include-if-hasconfig-rejects-remote-url");
    let user_config = root.join("user.gitconfig");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        &user_config,
        "[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = user-included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join("user-included.cfg"),
            "[remote \"included\"]\n\turl = https://example.test/included.git\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_config_with_user_paths(&info, "main", vec![user_config])
        .1
        .unwrap();

    assert_eq!(config.remote, "origin");
    assert_eq!(config.merge_ref, "refs/heads/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_skips_hasconfig_include_chain_that_defines_remote_url() {
    let root = temp_test_dir("include-if-hasconfig-rejects-nested-remote-url");
    let user_config = root.join("user.gitconfig");
    write_fake_tracked_repo(&root);
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "3333333333333333333333333333333333333333\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        &user_config,
        "[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = user-included.cfg\n",
    )
    .unwrap();
    std::fs::write(
            root.join("user-included.cfg"),
            "[include]\n\tpath = nested-remote.cfg\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
            root.join("nested-remote.cfg"),
            "[remote \"included\"]\n\turl = https://example.test/included.git\n\tfetch = +refs/heads/*:refs/remotes/included/*\n",
        )
        .unwrap();

    let info = git_worktree_info(&root).unwrap();
    let config = read_config_with_user_paths(&info, "main", vec![user_config])
        .1
        .unwrap();

    assert_eq!(config.remote, "origin");
    assert_eq!(config.merge_ref, "refs/heads/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_worktree_urls_for_hasconfig() {
    let root = temp_test_dir("include-if-hasconfig-worktree-url");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[extensions]\n\tworktreeConfig = true\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
            root.join(".git/config.worktree"),
            "[remote \"fork\"]\n\turl = https://example.test/fork.git\n[includeIf \"hasconfig:remote.*.url:*fork.git\"]\n\tpath = included.cfg\n",
        )
        .unwrap();
    std::fs::write(
        root.join(".git/included.cfg"),
        "[branch \"main\"]\n\tremote = included\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_stops_recursive_include_cycles() {
    let root = temp_test_dir("include-cycle");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/included")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/included/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(root.join(".git/config"), "[include]\n\tpath = a.cfg\n").unwrap();
    std::fs::write(root.join(".git/a.cfg"), "[include]\n\tpath = b.cfg\n").unwrap();
    std::fs::write(
            root.join(".git/b.cfg"),
            "[include]\n\tpath = a.cfg\n[remote \"included\"]\n\tfetch = +refs/heads/*:refs/remotes/included/*\n[branch \"main\"]\n\tremote = included\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "included");
    assert_eq!(upstream.full_ref, "refs/remotes/included/main");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_reads_linked_worktree_config() {
    let base = temp_test_dir("linked-worktree-config");
    let common_dir = base.join("repo/.git");
    let worktree = base.join("linked");
    let git_dir = common_dir.join("worktrees/linked");
    std::fs::create_dir_all(common_dir.join("refs/heads")).unwrap();
    std::fs::create_dir_all(common_dir.join("refs/remotes/fork")).unwrap();
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
    std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(
        common_dir.join("refs/heads/main"),
        "1111111111111111111111111111111111111111\n",
    )
    .unwrap();
    std::fs::write(
        common_dir.join("refs/remotes/fork/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            common_dir.join("config"),
            "[extensions]\n\tworktreeConfig = TRUE\n[remote \"fork\"]\n\tfetch = +refs/heads/*:refs/remotes/fork/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();
    std::fs::write(
        git_dir.join("config.worktree"),
        "[branch \"main\"]\n\tremote = fork\n",
    )
    .unwrap();

    let fingerprint = git_status_fingerprint(&worktree).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.remote, "fork");
    assert_eq!(upstream.full_ref, "refs/remotes/fork/main");

    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn git_status_fingerprint_ignores_inline_fetch_refspec_comment() {
    let root = temp_test_dir("commented-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::remove_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/upstream")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/upstream/main"),
        "2222222222222222222222222222222222222222\n",
    )
    .unwrap();
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/upstream/* # custom map\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.full_ref, "refs/remotes/upstream/main");
    assert_eq!(
        upstream.oid.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_clears_upstream_for_unmapped_refspec() {
    let root = temp_test_dir("unmapped-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/pull/*:refs/remotes/origin/pr/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    assert_eq!(fingerprint.upstream, None);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_status_fingerprint_honors_negative_fetch_refspec() {
    let root = temp_test_dir("negative-fetch-refspec");
    write_fake_tracked_repo(&root);
    std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\tfetch = ^refs/heads/main\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

    let fingerprint = git_status_fingerprint(&root).unwrap();

    let upstream = fingerprint.upstream.unwrap();
    assert_eq!(upstream.full_ref, "refs/remotes/origin/main");
    assert_eq!(
        upstream.oid.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );

    std::fs::remove_dir_all(root).unwrap();
}
