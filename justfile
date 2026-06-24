# zynk task runner. Tests are hermetic (each spawns its own temp config/socket), so plain
# `cargo nextest` / `just test` is safe to run directly.

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_vendor_libghostty_vt scripts.test_conventional_commits scripts.test_check_public_tree scripts.test_gitleaks_config

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

# Run PR CI checks
ci filter='all()': lint test-ts
    cargo nextest run --locked -E "{{filter}}" --status-level fail --final-status-level slow --failure-output final --success-output never

# Check formatting + run unit tests + maintenance script tests
check: ci
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_vendor_libghostty_vt scripts.test_conventional_commits scripts.test_check_public_tree scripts.test_gitleaks_config

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

# Private-content gates over the whole repo (structural tracked-path + gitleaks content).
gate:
    python3 scripts/check_public_tree.py
    gitleaks detect --no-git --config .gitleaks.toml --source . --redact
