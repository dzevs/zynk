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

/// `git` stdout for a successful read-only query, including an empty result.
/// `None` means the query failed; in particular, it is not evidence of a clean checkout.
fn git_stdout(dir: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

/// Emit `cargo:rerun-if-changed` for every TRACKED file in `dir`, returning how many were watched.
///
/// An explicit `rerun-if-changed` list REPLACES cargo's default file watching, so watching only
/// the git HEAD/ref left the dirty computation skippable: after a clean build, editing a tracked
/// Rust source and rebuilding produced a CHANGED executable that still reported the clean SHA with
/// no `-dirty`, and `src/remote/unix.rs` consumes that attestation as ADR 0013 install custody
/// (Codex Gate-2 `msg_3e339000b75278a4`). Watching the tracked set makes the dirty computation
/// unskippable: any edit to a file `git status` would notice re-runs this script.
///
/// A path is emitted as recorded even though it is checked for existence first — cargo cannot stat
/// a path that later disappears, so a DELETED tracked file re-runs the script too.
///
/// Returns 0 when git is absent or says nothing — a crates.io `.crate` unpack, a source tarball —
/// where the SHA is empty anyway and the remote-copy path already refuses to seed a remote host.
fn watch_tracked_sources(dir: &str) -> usize {
    let Ok(output) = Command::new("git")
        .current_dir(dir)
        .args(["ls-files", "-z"])
        .output()
    else {
        return 0;
    };
    if !output.status.success() {
        return 0;
    }
    let root = PathBuf::from(dir);
    let mut watched = 0;
    for name in output.stdout.split(|byte| *byte == 0) {
        if name.is_empty() {
            continue;
        }
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        let path = root.join(name);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
            watched += 1;
        }
    }
    watched
}

/// What a usable source attestation looks like, shared verbatim with the binary (ADR 0013 custody)
/// so the build input and the custody boundary cannot disagree.
#[path = "src/build_sha.rs"]
mod build_sha;

/// Export `ZYNK_BUILD_SHA`: the source commit this binary is built from (ADR 0013 custody).
///
/// A remote-copy install has to prove the far end runs the exact reviewed source, and a version
/// string cannot do that. An explicit `ZYNK_BUILD_SHA` wins, for reproducible and CI builds;
/// otherwise the commit comes from `git rev-parse HEAD`, suffixed `-dirty` when tracked files
/// differ from it. A build with neither — a crates.io `.crate` unpack, a source tarball — exports
/// an empty value, and the remote-copy path then refuses to seed a remote host from that binary.
fn export_build_sha() {
    println!("cargo:rerun-if-env-changed=ZYNK_BUILD_SHA");
    if let Ok(value) = env::var("ZYNK_BUILD_SHA") {
        let value = value.trim();
        // An empty override keeps its meaning: nothing is attested, and the remote-copy path
        // refuses to seed a host from a binary that cannot name its source. A NON-empty one is a
        // claim about reviewed source, so it has to have the shape of a commit here rather than
        // compiling in an attestation the custody boundary would have to reject later.
        if let Some(problem) = (!value.is_empty())
            .then(|| build_sha::attested_sha_problem(value))
            .flatten()
        {
            panic!(
                "ZYNK_BUILD_SHA={value:?} is not a usable source attestation: {problem}. ADR 0013 custody needs the exact reviewed commit (docs/zynk/decisions/0013-linux-only-platform-scope.md); pass `git rev-parse HEAD` of the source being built, or leave it unset to attest from the checkout."
            );
        }
        println!("cargo:rustc-env=ZYNK_BUILD_SHA={value}");
        return;
    }

    let dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    // Rebuild when the checkout moves, so the attested commit cannot go stale. The explicit
    // `rerun-if-changed` list above replaces cargo's default file watching, so without these a
    // later commit would keep the previous SHA compiled in.
    let watch = |args: &[&str]| {
        if let Some(path) = git_stdout(&dir, args) {
            if PathBuf::from(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    };
    watch(&["rev-parse", "--path-format=absolute", "--git-path", "HEAD"]);
    if let Some(reference) = git_stdout(&dir, &["symbolic-ref", "-q", "HEAD"]) {
        watch(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            &reference,
        ]);
    }
    // And every tracked file, so an edit that makes the tree dirty cannot leave a stale clean
    // SHA compiled in. The commit alone is not the attestation; the commit PLUS the dirty flag is.
    // When the tracked set cannot be enumerated the dirty flag cannot be kept fresh, so nothing is
    // attested at all — ADR 0013 refuses a binary that cannot attest its source, and a stale
    // attestation is worse than none.
    let watched = watch_tracked_sources(&dir);

    let sha = match git_stdout(&dir, &["rev-parse", "HEAD"])
        .filter(|sha| watched > 0 && build_sha::attested_sha_problem(sha).is_none())
    {
        Some(sha) => {
            let dirty = git_stdout(&dir, &["status", "--porcelain", "--untracked-files=no"]);
            match dirty {
                Some(status) if status.is_empty() => sha,
                Some(_) => format!("{sha}-dirty"),
                None => {
                    println!("cargo:warning=git status failed; building without source attestation (ADR 0013 remote custody is unavailable)");
                    String::new()
                }
            }
        }
        None => String::new(),
    };
    println!("cargo:rustc-env=ZYNK_BUILD_SHA={sha}");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/build_sha.rs");
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

    export_build_sha();

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
