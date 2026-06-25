# zynk task runner. Tests are hermetic (each spawns its own temp config/socket), so plain
# `cargo nextest` / `just test` is safe to run directly.

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty scripts.test_conventional_commits scripts.test_check_public_tree scripts.test_gitleaks_config scripts.test_scrub_check scripts.test_skills_catalog scripts.test_release_audit_refs scripts.test_gitleaks_tracked

# Run one nextest filter, e.g. `just test-one codex_stale_working`
test-one filter:
    cargo nextest run --locked "{{filter}}" --status-level fail --final-status-level fail --failure-output final --success-output never

# Run the pi state-only integration TypeScript tests (bun). Wired into `ci`/`check`.
test-ts:
    bun test src/integration/assets/pi/zynk-agent-state.test.ts

# Run fast local lint checks
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

# Run the Windows target clippy from Unix/macOS to catch cfg(windows) compile + clippy
# failures before the windows-latest CI job. NOTE: zynk's bundled C deps (sqlite-vec /
# libsqlite3-sys, ADR 0006) need an MSVC-compatible archiver (`lib.exe`) to cross-compile
# the build scripts, so this requires an MSVC C toolchain (clang-cl/llvm-lib or a Windows
# host). It is intentionally NOT wired into `check` — the authoritative Windows runtime
# check is the ci.yml windows-latest job. (mirrors upstream b7a504b)
windows-lint:
    rustup target add x86_64-pc-windows-msvc
    LIBGHOSTTY_VT_SIMD=false cargo clippy --bin zynk --locked --target x86_64-pc-windows-msvc -- -D warnings

# Run PR CI checks
ci filter='all()': lint test-ts
    cargo nextest run --locked -E "{{filter}}" --status-level fail --final-status-level slow --failure-output final --success-output never

# Check formatting + run unit tests + maintenance script tests
check: ci
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty scripts.test_conventional_commits scripts.test_check_public_tree scripts.test_gitleaks_config scripts.test_scrub_check scripts.test_skills_catalog scripts.test_release_audit_refs scripts.test_gitleaks_tracked

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    chmod +x .githooks/commit-msg
    @echo "installed git hooks from .githooks"

# Build release binary
build:
    cargo build --release --locked

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Print default config
default-config:
    cargo run --release --locked -- --default-config

# Private-content gates over the TRACKED tree (structural tracked-path + scoped scrub + gitleaks
# content on a clean `git archive` export, so ignored/local artifacts can't false-fail it).
gate:
    python3 scripts/check_public_tree.py
    python3 scripts/scrub_check.py
    bash scripts/gitleaks_tracked.sh

# Optional docs prose lint (Vale: write-good + the custom zynk style). NOT a hard CI gate; needs `vale` installed.
docs-lint:
    vale README.md CLAUDE.md AGENTS.md WORKFLOW.md CONTRIBUTING.md CODE_OF_CONDUCT.md DEVELOPMENT.md SECURITY.md docs/styleguides/
