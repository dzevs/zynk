use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The diagnostic ADR 0013 promises on an unsupported platform. `src/main.rs` raises it as a
/// `compile_error!`; this build script repeats it as a warning before skipping the native build,
/// because a build script runs before rustc ever compiles the crate.
const LINUX_ONLY_DIAGNOSTIC: &str =
    "zynk supports Linux only (docs/zynk/decisions/0013-linux-only-platform-scope.md)";

/// The Zig target for a Rust target zynk builds for, or `None` for a Linux architecture the
/// vendored libghostty-vt build has no mapping for.
fn zig_target(target: &str) -> Option<&'static str> {
    match target {
        "x86_64-unknown-linux-gnu" => Some("x86_64-linux-gnu"),
        "x86_64-unknown-linux-musl" => Some("x86_64-linux-musl"),
        _ => None,
    }
}

fn env_bool(name: &str) -> Option<bool> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            other => panic!("invalid boolean value for {name}: {other}"),
        },
        Err(env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read {name}: {err}"),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig.zon");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/include");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/pkg");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/src");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/VERSION");
    // DOCS_RS decides whether this script builds and links the native library at all, so a change to
    // it has to invalidate the cached result. Without this, a docs-mode run's no-Zig/no-link output
    // stays cached for a later normal build in the same target directory.
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SIMD");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed=ZYNK_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=ZYNK_BUILD_ID");
    println!("cargo:rerun-if-env-changed=ZYNK_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=ZIG");

    // docs.rs builds with no network and no Zig toolchain. rustdoc does not link, and the libghostty-vt API
    // is consumed via `extern "C"` declarations, so skip the native Zig build + all link directives there.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

    // zynk builds for Linux only (ADR 0013). The failure a user on another platform is promised is the
    // `compile_error!` in `src/main.rs` naming that ADR — and rustc only reaches it if this script does
    // not fail first, so an unsupported OS skips the Zig build and the link directives rather than
    // panicking over a missing Zig target mapping.
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    if target_os != "linux" {
        println!("cargo:warning={LINUX_ONLY_DIAGNOSTIC}");
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored_dir = manifest_dir.join("vendor/libghostty-vt");
    // Emit Zig's install prefix (zig-out) + local cache (.zig-cache) UNDER OUT_DIR so the `zig build` below
    // never writes into the vendored source tree. `cargo package`/`cargo publish` verification rejects a build
    // that modifies the package source; this keeps vendor/libghostty-vt/ untouched. (Zig's global dep cache
    // stays at its default, outside the source tree.)
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let zig_prefix = out_dir.join("ghostty");
    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET");
    // A Linux target with no Zig mapping compiles past the `compile_error!` in `src/main.rs` (its
    // `target_os` IS linux), so this panic is the only diagnostic it gets.
    let Some(zig_target) = zig_target(&target) else {
        panic!("{LINUX_ONLY_DIAGNOSTIC}: no vendored libghostty-vt build for target {target}");
    };
    let version_string = fs::read_to_string(vendored_dir.join("VERSION"))
        .expect("failed to read vendored libghostty-vt VERSION")
        .trim()
        .to_string();

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let mut command = Command::new(zig);
    command
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dsimd={simd}"))
        .arg(format!("-Dtarget={zig_target}"))
        .arg(format!("-Dversion-string={version_string}"))
        .arg("-Demit-xcframework=false")
        .arg("--prefix")
        .arg(&zig_prefix)
        .arg("--cache-dir")
        .arg(out_dir.join("zig-cache"));
    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {
        command.arg("--system").arg(system_dir);
    }

    let status = command
        .current_dir(&vendored_dir)
        .status()
        .expect("failed to execute zig build for vendored libghostty-vt");
    assert!(
        status.success(),
        "zig build for vendored libghostty-vt failed: {status}"
    );

    let lib_dir = zig_prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");
}
