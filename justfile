# Modified by the zynk project: this file differs from the upstream version it was derived from.
# See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
# zynk task runner. Tests are hermetic (each spawns its own temp config/socket), so plain
# `cargo nextest` / `just test` is safe to run directly.

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    just ui-hot-path-architecture-test
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_vendor_libghostty_vt scripts.test_conventional_commits scripts.test_check_public_tree scripts.test_gitleaks_config scripts.test_scrub_check scripts.test_skills_catalog scripts.test_release_audit_refs scripts.test_gitleaks_tracked scripts.test_hermes_integration_asset scripts.test_license_docs scripts.test_release_binary_audit

# Run one nextest filter, e.g. `just test-one codex_stale_working`
test-one filter:
    cargo nextest run --locked "{{filter}}" --status-level fail --final-status-level fail --failure-output final --success-output never

# Enforce deterministic UI hot-path architecture boundaries
ui-hot-path-architecture-test:
    python3 -m unittest scripts.test_ui_hot_path_architecture

# Run the bundled agent-integration asset TypeScript tests (bun). Wired into `ci`/`check`.
test-ts:
    bun test src/integration/assets/zynk-agent-state.test.ts
    bun test src/integration/assets/pi/zynk-agent-state.test.ts
    bun test src/integration/assets/opencode/zynk-agent-state.test.ts
    bun test src/integration/assets/opencode/zynk-tui-session.test.ts

# Run fast local lint checks
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

# Run PR CI checks
ci filter='all()': lint test-ts
    cargo nextest run --locked -E "{{filter}}" --status-level fail --final-status-level slow --failure-output final --success-output never
    just ui-hot-path-architecture-test

# Check formatting + run unit tests + maintenance script tests
check: ci
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_vendor_libghostty_vt scripts.test_conventional_commits scripts.test_check_public_tree scripts.test_gitleaks_config scripts.test_scrub_check scripts.test_skills_catalog scripts.test_release_audit_refs scripts.test_gitleaks_tracked scripts.test_hermes_integration_asset scripts.test_license_docs scripts.test_release_binary_audit

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    chmod +x .githooks/commit-msg
    @echo "installed git hooks from .githooks"

# Build release binary
build:
    cargo build --release --locked

# Supporting full-render scaling profile; review ratios rather than absolute CI timings.
bench-render-scale:
    cargo test --release --locked --bin zynk render_scale_profile -- --ignored --nocapture --test-threads=1

# Release verification (NOT part of `just check`): build the release binary and audit the ARTIFACT —
# no debug-only env seam, no update URL constant, `zynk update` fails closed without reaching a
# downloader. `just check` only runs the hermetic fixture-based unittest for the same script.
release-audit:
    cargo build --release --locked
    python3 scripts/release_binary_audit.py "${CARGO_TARGET_DIR:-target}/release/zynk"

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
