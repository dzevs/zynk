use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn zig_target(target: &str) -> &str {
    match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("unsupported target for libghostty-vt build: {other}"),
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

/// Names of the archive members whose data does not start on an 8-byte boundary (the `__.SYMDEF*` table of
/// contents is not a Mach-O member and is skipped). Understands the BSD layout Zig writes (`#1/<len>` names
/// stored in front of the member data, counted in the member size) and the plain 16-byte names.
pub fn archive_misaligned_members(bytes: &[u8]) -> Result<Vec<String>, String> {
    const MAGIC: &[u8] = b"!<arch>\n";
    const HEADER: usize = 60;
    if !bytes.starts_with(MAGIC) {
        return Err("not an ar archive (missing !<arch> magic)".into());
    }
    let mut offset = MAGIC.len();
    let mut misaligned = Vec::new();
    while offset + HEADER <= bytes.len() {
        let header = &bytes[offset..offset + HEADER];
        if &header[58..60] != b"`\n" {
            return Err(format!("bad member header at offset {offset}"));
        }
        let name = String::from_utf8_lossy(&header[..16])
            .trim_end()
            .to_string();
        let size: usize = String::from_utf8_lossy(&header[48..58])
            .trim()
            .parse()
            .map_err(|e| format!("bad member size at offset {offset}: {e}"))?;
        let mut data = offset + HEADER;
        let mut member = name.clone();
        if let Some(len) = name.strip_prefix("#1/") {
            let len: usize = len
                .trim()
                .parse()
                .map_err(|e| format!("bad BSD name length at offset {offset}: {e}"))?;
            let raw = bytes
                .get(data..data + len)
                .ok_or_else(|| format!("truncated BSD member name at offset {offset}"))?;
            member = String::from_utf8_lossy(raw)
                .trim_end_matches('\0')
                .to_string();
            data += len;
        }
        if !member.starts_with("__.SYMDEF") && !data.is_multiple_of(8) {
            misaligned.push(member);
        }
        offset += HEADER + size + (size & 1);
    }
    Ok(misaligned)
}

/// The static archive to hand to the Apple linker. Zig 0.15's archive writer pads members to 2 bytes, but
/// Apple's linker (ld-prime, Xcode 15+) refuses a 64-bit Mach-O member whose data is not 8-byte aligned
/// ("ld: 64-bit mach-o member 'compiler_rt.o' not 8-byte aligned"). Whether a member lands aligned depends
/// on the sizes of the members before it: with `LIBGHOSTTY_VT_SIMD=false` the aarch64 archive misaligns
/// `compiler_rt.o` and the x86_64 archive misaligns the main object, while the SIMD build aligns both by
/// chance. A misaligned archive is rewritten on a macOS host with Apple's `libtool -static`, which lays
/// members out on 8-byte boundaries and regenerates the table of contents; the result is verified before it
/// is used. `LIBGHOSTTY_VT_LIBTOOL` names the tool to run (default: `xcrun libtool`, then `libtool`). On any
/// other host the archive is left as is (nothing there links it with Apple's linker).
pub fn darwin_link_archive(static_lib: &Path, lib_dir: &Path) -> PathBuf {
    let bytes = fs::read(static_lib)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", static_lib.display()));
    let misaligned = archive_misaligned_members(&bytes)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", static_lib.display()));
    if misaligned.is_empty() {
        return static_lib.to_path_buf();
    }
    let members = misaligned.join(", ");
    if !cfg!(target_os = "macos") {
        println!(
            "cargo:warning=libghostty-vt.a has members Apple's linker rejects as not 8-byte aligned ({members}); \
             it is normalized only when built on macOS"
        );
        return static_lib.to_path_buf();
    }
    let aligned = lib_dir.join("libghostty-vt-aligned.a");
    let candidates: Vec<Vec<String>> = match env::var("LIBGHOSTTY_VT_LIBTOOL") {
        Ok(tool) => vec![tool.split_whitespace().map(str::to_string).collect()],
        Err(_) => vec![
            vec!["xcrun".into(), "libtool".into()],
            vec!["libtool".into()],
        ],
    };
    let mut attempts = Vec::new();
    for candidate in candidates {
        let (program, args) = match candidate.split_first() {
            Some((program, args)) if !program.is_empty() => (program.clone(), args.to_vec()),
            _ => continue,
        };
        let _ = fs::remove_file(&aligned);
        let mut command = Command::new(&program);
        command
            .args(&args)
            .arg("-static")
            .arg("-o")
            .arg(&aligned)
            .arg(static_lib);
        match command.output() {
            Ok(output) if output.status.success() => {
                let rewritten = fs::read(&aligned)
                    .unwrap_or_else(|e| panic!("failed to read {}: {e}", aligned.display()));
                match archive_misaligned_members(&rewritten) {
                    Ok(still) if still.is_empty() => return aligned,
                    Ok(still) => attempts.push(format!(
                        "{}: output still has misaligned members ({})",
                        candidate.join(" "),
                        still.join(", ")
                    )),
                    Err(e) => {
                        attempts.push(format!("{}: unreadable output: {e}", candidate.join(" ")))
                    }
                }
            }
            Ok(output) => attempts.push(format!(
                "{}: {} ({})",
                candidate.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(e) => attempts.push(format!("{}: {e}", candidate.join(" "))),
        }
    }
    panic!(
        "libghostty-vt.a has members Apple's linker rejects as not 8-byte aligned ({members}) and no \
         `libtool -static` rewrite succeeded ({}); install the Xcode Command Line Tools or point \
         LIBGHOSTTY_VT_LIBTOOL at Apple's libtool",
        attempts.join("; ")
    );
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
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SIMD");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_LIBTOOL");
    println!("cargo:rerun-if-env-changed=ZYNK_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=ZYNK_BUILD_ID");
    println!("cargo:rerun-if-env-changed=ZYNK_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=ZIG");

    // docs.rs builds with no network and no Zig toolchain. rustdoc does not link, and the libghostty-vt API
    // is consumed via `extern "C"` declarations, so skip the native Zig build + all link directives there.
    if env::var_os("DOCS_RS").is_some() {
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
    let zig_target = zig_target(&target);
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
    if target.contains("apple-darwin") {
        let static_lib = lib_dir.join("libghostty-vt.a");
        let link_lib = darwin_link_archive(&static_lib, &lib_dir);
        println!("cargo:rustc-link-arg={}", link_lib.display());
    } else if target.contains("windows-msvc") {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}
