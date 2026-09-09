//! What `build.rs` promises, checked by running the script and cargo. The two expensive cargo
//! cycles are ignored and run explicitly; the isolated Git-query regression runs in `just check`.
//!
//! ADR 0013 promises one thing to anyone who builds zynk on another platform: a `compile_error!`
//! naming the ADR (`src/main.rs`). `build.rs` runs BEFORE rustc compiles the crate, so a build
//! script that failed on an unsupported target would replace that message with a Zig-target panic
//! and the promise would be false. The first test asks cargo to check the crate for a non-Linux
//! target and asserts the ADR message is what comes back.
//!
//! The second promise is ADR 0013 custody: `ZYNK_BUILD_SHA` names the source this binary was built
//! from, `-dirty` included. `build_sha_attestation_does_not_survive_a_source_edit` runs the real
//! clean -> edit -> rebuild -> revert cycle in a scratch clone, because cargo's fingerprinting is
//! the thing under test and no unit test can observe it. Its scratch directory goes under
//! `ZYNK_BUILD_SHA_CHECK_ROOT`, else `$CARGO_TARGET_DIR`, else `target/`:
//!
//! ```bash
//! cargo nextest run --locked --test build_script_targets --run-ignored all -E 'test(build_sha)'
//! ```
//!
//! `#[ignore]` because it is not hermetic and not fast: it needs the cross target's std
//! (`rustup target add x86_64-pc-windows-gnu`) and it checks the whole dependency graph for that
//! target in a cold, throwaway target directory. Run it explicitly:
//!
//! ```bash
//! cargo test --locked --test build_script_targets -- --ignored --nocapture
//! # or, under nextest:
//! cargo nextest run --locked --run-ignored all -E 'test(adr_0013)'
//! ```
//!
//! The throwaway target directory is created under `$CARGO_TARGET_DIR` (or `target/`) and removed
//! when the test ends; set `ZYNK_CROSS_CHECK_TARGET_ROOT` to put it somewhere else, e.g. on a disk
//! that is not the tmpfs `/tmp`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The target the cross-check builds for. mingw is the cheapest non-Linux target to install: one
/// `rustup target add`, no platform SDK.
const CROSS_TARGET: &str = "x86_64-pc-windows-gnu";

/// The exact text `src/main.rs` raises on any non-Linux `target_os`.
const ADR_MESSAGE: &str =
    "zynk supports Linux only (docs/zynk/decisions/0013-linux-only-platform-scope.md)";

/// What cargo prints when a build script fails — the failure mode that used to hide `ADR_MESSAGE`.
const BUILD_SCRIPT_FAILURE: &str = "failed to run custom build command";

fn cross_target_std_installed() -> bool {
    let Ok(output) = Command::new("rustc")
        .args(["--print", "target-libdir", "--target", CROSS_TARGET])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let libdir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    !libdir.is_empty() && Path::new(&libdir).is_dir()
}

fn fresh_target_dir() -> PathBuf {
    let root = env::var_os("ZYNK_CROSS_CHECK_TARGET_ROOT")
        .or_else(|| env::var_os("CARGO_TARGET_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    root.join(format!(
        "adr0013-cross-check-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
#[ignore = "cross-target: needs the x86_64-pc-windows-gnu std and a cold dependency check (minutes)"]
fn adr_0013_compile_error_is_what_a_non_linux_target_reports() {
    assert!(
        cross_target_std_installed(),
        "the {CROSS_TARGET} standard library is missing; run `rustup target add {CROSS_TARGET}`"
    );

    let target_dir = fresh_target_dir();
    fs::create_dir_all(&target_dir).expect("create the throwaway cross-check target directory");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["check", "--locked", "--target", CROSS_TARGET])
        .env("CARGO_TARGET_DIR", &target_dir)
        // The docs.rs path skips the native build for every target; this check must exercise the
        // normal one.
        .env_remove("DOCS_RS")
        .output()
        .expect("run cargo check for the cross target");
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Remove before asserting: a failure still reports `rendered`, and the directory holds a full
    // dependency check.
    let _ = fs::remove_dir_all(&target_dir);

    assert!(
        !output.status.success(),
        "checking zynk for {CROSS_TARGET} must fail:\n{rendered}"
    );
    assert!(
        rendered.contains(&format!("error: {ADR_MESSAGE}")),
        "checking zynk for {CROSS_TARGET} must report the ADR 0013 compile_error:\n{rendered}"
    );
    assert!(
        !rendered.contains(BUILD_SCRIPT_FAILURE),
        "build.rs must skip an unsupported target, not fail the build before rustc:\n{rendered}"
    );
}

/// The marker inserted into a tracked Rust constant to make the source genuinely different.
const DIRTY_MARKER: &str = "zynk-build-sha-cycle-marker";
/// The tracked source the cycle edits, and the constant inside it.
const EDITED_SOURCE: &str = "src/update.rs";
const EDITED_CONST: &str = "pub(crate) const ZYNK_UPDATE_UNAVAILABLE_MESSAGE: &str = \"";

fn scratch_root() -> PathBuf {
    env::var_os("ZYNK_BUILD_SHA_CHECK_ROOT")
        .or_else(|| env::var_os("CARGO_TARGET_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    scrub_git_env(&mut command);
    let output = command
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("run git {args:?} in {}: {err}", dir.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn scrub_git_env(command: &mut Command) {
    for (key, _) in env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_SYSTEM", "/dev/null");
}

/// `cargo build --locked --bin zynk` in `checkout`, with its target directory OUTSIDE the checkout
/// so the build never dirties the tree it is attesting. The same directory across calls, because a
/// COLD rebuild would re-run the build script for reasons that have nothing to do with this test.
fn build_zynk(checkout: &Path, target_dir: &Path) -> PathBuf {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    scrub_git_env(&mut command);
    let output = command
        .current_dir(checkout)
        .args(["build", "--locked", "--bin", "zynk"])
        .env("CARGO_TARGET_DIR", target_dir)
        .env_remove("DOCS_RS")
        .env_remove("ZYNK_BUILD_SHA")
        .output()
        .expect("run cargo build for the scratch checkout");
    assert!(
        output.status.success(),
        "cargo build in {} failed:\n{}{}",
        checkout.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    target_dir.join("debug").join("zynk")
}

fn version_line(binary: &Path) -> String {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("run the built zynk --version");
    assert!(
        output.status.success(),
        "{} --version failed: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn binary_contains(binary: &Path, needle: &str) -> bool {
    let bytes = fs::read(binary).expect("read the built binary");
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn build_attestation_distinguishes_clean_dirty_and_failed_git_queries() {
    use std::os::unix::fs::PermissionsExt;

    let root = fresh_target_dir();
    let checkout = root.join("checkout");
    let tools = root.join("tools");
    fs::create_dir_all(&checkout).unwrap();
    fs::create_dir_all(&tools).unwrap();
    let script = root.join("build-script");
    let compiled = Command::new("rustc")
        .args(["--edition=2021", "--crate-name", "build_script_probe"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"))
        .arg("-o")
        .arg(&script)
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");

    git(&checkout, &["init", "--quiet"]);
    assert!(checkout.join(".git").is_dir());
    fs::write(checkout.join("source"), "clean\n").unwrap();
    git(&checkout, &["add", "source"]);
    git(
        &checkout,
        &[
            "-c",
            "user.name=Build Test",
            "-c",
            "user.email=build@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    let head = git(&checkout, &["rev-parse", "HEAD"]);
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let wrapper = tools.join("git");
    fs::write(&wrapper, "#!/bin/sh\nif [ \"$1\" = \"$FAIL_GIT_QUERY\" ]; then exit 42; fi\nexec \"$REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = env::join_paths(
        std::iter::once(tools).chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )
    .unwrap();
    let attest = |fail: &str| {
        let mut command = Command::new(&script);
        scrub_git_env(&mut command);
        let output = command
            .env("CARGO_MANIFEST_DIR", &checkout)
            .env("DOCS_RS", "1")
            .env_remove("ZYNK_BUILD_SHA")
            .env("PATH", &path)
            .env("REAL_GIT", &real_git)
            .env("FAIL_GIT_QUERY", fail)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find_map(|line| {
                line.strip_prefix("cargo:rustc-env=ZYNK_BUILD_SHA=")
                    .map(str::to_owned)
            })
            .expect("the build script must explicitly export the attestation")
    };
    assert_eq!(attest(""), head, "a successful empty status is clean");
    fs::write(checkout.join("source"), "dirty\n").unwrap();
    assert_eq!(attest(""), format!("{head}-dirty"));
    assert_eq!(
        attest("status"),
        "",
        "a failed status query must not attest clean source"
    );
    assert_eq!(
        attest("rev-parse"),
        "",
        "a failed HEAD query cannot attest source"
    );
    fs::write(checkout.join("source"), "clean\n").unwrap();
    assert_eq!(
        attest(""),
        head,
        "healthy queries recover without an override"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "real build cycle: clones the checkout and builds zynk three times (minutes)"]
fn build_sha_attestation_does_not_survive_a_source_edit() {
    // Codex Gate-2 (`msg_3e339000b75278a4`): `build.rs` emitted `cargo:rerun-if-changed` only for
    // the git HEAD/ref, and an explicit list REPLACES cargo's default file watching. So after a
    // clean build, editing a tracked Rust source and rebuilding produced a CHANGED executable that
    // still reported `zynk <version> (<clean sha>)` with no `-dirty` — and `src/remote/unix.rs`
    // consumes exactly that line as ADR 0013 install custody. Only the real cycle proves the fix:
    // a unit test cannot observe cargo's fingerprinting.
    //
    // The checkout under test is a CLONE at the source checkout's HEAD, so what is exercised is the
    // committed `build.rs`, and the clone's own `git rev-parse HEAD` is the expected attestation.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let head = git(&manifest, &["rev-parse", "HEAD"]);

    let root = scratch_root();
    fs::create_dir_all(&root).expect("create the scratch root");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let scratch = root.join(format!("build-sha-cycle-{}-{nanos}", std::process::id()));
    let checkout = scratch.join("checkout");
    let target_dir = scratch.join("target");
    fs::create_dir_all(&scratch).expect("create the scratch directory");

    let cycle = std::panic::catch_unwind(|| {
        let mut command = Command::new("git");
        scrub_git_env(&mut command);
        let clone = command
            .args(["clone", "--quiet"])
            .arg(&manifest)
            .arg(&checkout)
            .output()
            .expect("clone the checkout under test");
        assert!(
            clone.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&clone.stderr)
        );
        git(&checkout, &["checkout", "--quiet", "--detach", &head]);
        assert!(
            checkout.join(".git").is_dir(),
            "the build fixture must own its repository"
        );
        assert_eq!(
            git(
                &checkout,
                &["status", "--porcelain", "--untracked-files=no"]
            ),
            "",
            "the scratch checkout must start clean"
        );

        // 1. Warm clean build: the attestation is the commit, with no `-dirty`.
        let binary = build_zynk(&checkout, &target_dir);
        let clean = version_line(&binary);
        assert!(
            clean.ends_with(&format!("({head})")),
            "a clean checkout must attest exactly its own commit: {clean:?}"
        );
        assert!(
            !clean.contains("-dirty"),
            "a clean checkout is not dirty: {clean:?}"
        );
        assert!(
            !binary_contains(&binary, DIRTY_MARKER),
            "the clean build already carries the marker"
        );

        // 2. Edit one tracked Rust constant and rebuild in the SAME target directory.
        let source = checkout.join(EDITED_SOURCE);
        let original = fs::read_to_string(&source).expect("read the source to edit");
        let anchor = original
            .find(EDITED_CONST)
            .unwrap_or_else(|| panic!("{EDITED_SOURCE} no longer declares {EDITED_CONST}"));
        let split = anchor + EDITED_CONST.len();
        let edited = format!(
            "{}{DIRTY_MARKER} {}",
            &original[..split],
            &original[split..]
        );
        fs::write(&source, &edited).expect("write the edited source");
        assert_ne!(
            git(
                &checkout,
                &["status", "--porcelain", "--untracked-files=no"]
            ),
            "",
            "the edit did not make the tree dirty"
        );

        let binary = build_zynk(&checkout, &target_dir);
        let dirty = version_line(&binary);
        assert!(
            binary_contains(&binary, DIRTY_MARKER),
            "the edited constant was not compiled, so the cycle proves nothing: {dirty:?}"
        );
        assert_eq!(
            dirty,
            clean.replace(&format!("({head})"), &format!("({head}-dirty)")),
            "a modified tracked source must be attested as dirty: {dirty:?}"
        );

        // 3. Revert and rebuild: the clean attestation comes back.
        fs::write(&source, &original).expect("restore the source");
        assert_eq!(
            git(
                &checkout,
                &["status", "--porcelain", "--untracked-files=no"]
            ),
            "",
            "the revert did not restore the tree"
        );
        let binary = build_zynk(&checkout, &target_dir);
        let restored = version_line(&binary);
        assert!(
            !binary_contains(&binary, DIRTY_MARKER),
            "the reverted source was not recompiled: {restored:?}"
        );
        assert_eq!(
            restored, clean,
            "reverting the edit must restore the clean attestation"
        );
    });

    let _ = fs::remove_dir_all(&scratch);
    if let Err(panic) = cycle {
        std::panic::resume_unwind(panic);
    }
}
