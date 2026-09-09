//! ADR 0013 promises one thing to anyone who builds zynk on another platform: a `compile_error!`
//! naming the ADR (`src/main.rs`). `build.rs` runs BEFORE rustc compiles the crate, so a build
//! script that failed on an unsupported target would replace that message with a Zig-target panic
//! and the promise would be false. This test asks cargo to check the crate for a non-Linux target
//! and asserts the ADR message is what comes back.
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
