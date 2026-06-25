---
name: ci-cd-and-automation
description: Automates CI/CD pipeline setup. Use when setting up or modifying build and deployment pipelines. Use when you need to automate quality gates, configure test runners in CI, or establish deployment strategies.
---

# CI/CD and Automation

## Overview

Automate quality gates so that no change reaches production without passing tests, lint, formatting, and build. CI/CD is the enforcement mechanism for every other skill — it catches what humans and agents miss, and it does so consistently on every single change.

**Shift Left:** Catch problems as early in the pipeline as possible. A bug caught in linting costs minutes; the same bug caught in production costs hours. Move checks upstream — static analysis before tests, tests before staging, staging before production.

**Faster is Safer:** Smaller batches and more frequent releases reduce risk, not increase it. A deployment with 3 changes is easier to debug than one with 30. Frequent releases build confidence in the release process itself.

## When to Use

- Setting up a new project's CI pipeline
- Adding or modifying automated checks
- Configuring release pipelines
- When a change should trigger automated verification
- Debugging CI failures

## The Quality Gate Pipeline

Every change goes through these gates before merge:

```
Pull Request Opened
    │
    ▼
┌──────────────────────┐
│   FORMAT CHECK        │  cargo fmt --check
│   ↓ pass              │
│   LINT (clippy)       │  cargo clippy --all-targets -- -D warnings
│   ↓ pass              │
│   UNIT TESTS          │  cargo nextest run --locked
│   ↓ pass              │
│   ASSET / TS TESTS    │  just test-ts (bun)
│   ↓ pass              │
│   BUILD               │  cargo build --locked
│   ↓ pass              │
│   SCRIPT/MAINT TESTS  │  python3 -m unittest scripts.*
│   ↓ pass              │
│   PRIVATE-CONTENT     │  check_public_tree + scrub_check + gitleaks
│   GATE                │
│   ↓ pass              │
│   COMMIT-MSG GATE     │  conventional_commits.py
└──────────────────────┘
    │
    ▼
  Ready for review
```

**No gate can be skipped.** If clippy fails, fix the warning — don't `#[allow(...)]` it away. If a test fails, fix the code — don't ignore the test. On any private-content gate failure: STOP, fix the root cause, never bypass.

Locally the same gates run through `just`:

```bash
just lint     # cargo fmt --check + cargo clippy --all-targets --locked -- -D warnings
just test     # cargo nextest + maintenance-script tests
just ci       # lint + test-ts + nextest
just check    # ci + maintenance-script tests (the full gate)
just gate     # private-content gates (check_public_tree + scrub + gitleaks)
```

## GitHub Actions Configuration

### Basic CI Pipeline

```yaml
# .github/workflows/ci.yml
name: CI

on:
  pull_request:
    types: [opened, synchronize, reopened]
  push:
    branches: [main]

permissions:
  contents: read

concurrency:
  group: ci-${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: true

jobs:
  check:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v6
        with:
          persist-credentials: false

      - name: Install Rust (stable)
        uses: dtolnay/rust-toolchain@stable

      - name: Install Rust tools
        uses: taiki-e/install-action@v2
        with:
          tool: just,cargo-nextest

      - name: Install Zig
        uses: mlugg/setup-zig@v2
        with:
          version: 0.15.2

      - name: Install Bun
        uses: oven-sh/setup-bun@v2
        with:
          bun-version: latest

      - name: Restore cargo cache
        uses: Swatinem/rust-cache@v2

      - name: Run checks
        run: just ci 'all()'
```

> **Note:** The bundled `libghostty-vt` is built with Zig, so CI must install Zig 0.15.2; the TS asset test needs Bun. Pin action versions (ideally by commit SHA) for supply-chain safety.

### Cross-Platform Matrix

zynk targets Unix and Windows, so the real pipeline runs a matrix:

```yaml
  check:
    name: check (${{ matrix.os }})
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: ubuntu-latest
            kind: unix
            nextest_filter: all()
          - os: macos-latest
            kind: unix
            nextest_filter: not binary(live_handoff)   # env-sensitive PTY test
          - os: windows-latest
            kind: windows
    runs-on: ${{ matrix.os }}
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v6
      # ... toolchain setup ...
      - name: Run checks (unix)
        if: matrix.kind == 'unix'
        run: just ci '${{ matrix.nextest_filter }}'
      - name: Run checks (windows)
        if: matrix.kind == 'windows'
        shell: pwsh
        run: |
          cargo fmt --check
          cargo clippy --bin zynk --locked --target x86_64-pc-windows-msvc -- -D warnings
          cargo test --locked --target x86_64-pc-windows-msvc --bin zynk
          cargo build --locked --target x86_64-pc-windows-msvc
```

### Conventional-Commit Gate

```yaml
  conventional-commits:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
          persist-credentials: false
      - name: Validate commit subjects (push)
        if: github.event_name == 'push'
        run: python3 scripts/conventional_commits.py --range "${{ github.event.before }}..${{ github.event.after }}"
      - name: Validate PR title
        if: github.event_name == 'pull_request'
        run: python3 scripts/conventional_commits.py "${{ github.event.pull_request.title }}"
```

### Private-Content Gates

A dedicated workflow keeps maintainer-private paths and secrets out of the public tree:

```yaml
# .github/workflows/gates.yml
name: Gates

on:
  pull_request:
  push:
    branches: [main]

permissions:
  contents: read

jobs:
  gates:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          persist-credentials: false
      - name: Tracked-path gate (no forbidden private path is tracked)
        run: python3 scripts/check_public_tree.py
      - name: Scrub gate (no product-specific reference terms)
        run: python3 scripts/scrub_check.py
      - name: Install gitleaks
        run: |
          curl -sSfL https://github.com/gitleaks/gitleaks/releases/download/v8.30.0/gitleaks_8.30.0_linux_x64.tar.gz -o /tmp/gitleaks.tgz
          tar -xzf /tmp/gitleaks.tgz -C /tmp gitleaks
          sudo install -m 0755 /tmp/gitleaks /usr/local/bin/gitleaks
      - name: Content gate (no private strings)
        run: gitleaks detect --no-git --config .gitleaks.toml --source . --redact
```

## Feeding CI Failures Back to Agents

The power of CI with AI agents is the feedback loop. When CI fails:

```
CI fails
    │
    ▼
Copy the failure output
    │
    ▼
Feed it to the agent:
"The CI pipeline failed with this error:
[paste specific error]
Fix the issue and verify locally before pushing again."
    │
    ▼
Agent fixes → pushes → CI runs again
```

**Key patterns:**

```
Format failure → Agent runs `cargo fmt` and commits
Clippy warning → Agent reads the lint location and fixes (no blanket #[allow])
Test failure   → Agent reproduces with `just test-one <filter>`, then debugs
Build error    → Agent checks Cargo.toml / target / Zig toolchain version
Gate failure   → Agent fixes the root cause (never bypass the gate)
```

## Release and Verification Strategies

### Dry-Run / Build-Artifact Workflows

Validate the release pipeline before cutting a real release. A manual `workflow_dispatch` build-artifacts job and a release dry-run job let you exercise packaging on every target without publishing:

```yaml
# Build artifacts on demand for verification
on:
  workflow_dispatch:

jobs:
  build:
    strategy:
      matrix:
        target: [x86_64-unknown-linux-gnu, x86_64-pc-windows-msvc, aarch64-apple-darwin]
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v6
      - name: Build release
        run: cargo build --release --locked --target ${{ matrix.target }}
      - uses: actions/upload-artifact@v4
        with:
          name: zynk-${{ matrix.target }}
          path: target/${{ matrix.target }}/release/zynk*
```

### Feature Flags

Feature flags decouple landing code from enabling behavior. Land incomplete or risky features behind a flag (a runtime config toggle or a Cargo feature) so you can:

- **Land code without enabling it.** Merge to main early, enable when ready.
- **Roll back without redeploying.** Disable the flag instead of reverting code.
- **Canary new behavior.** Enable for a subset before everyone.

```rust
// Simple runtime feature-flag pattern
if config.feature_enabled("new-delivery-receipt") {
    return build_receipt_v2(&message);
}
build_receipt_v1(&message)
```

**Flag lifecycle:** Create → Enable for testing → Canary → Full rollout → Remove the flag and dead code. Flags that live forever become technical debt — set a cleanup date when you create them.

### Staged Rollouts

```
PR merged to main
    │
    ▼
  Build + full check (CI, auto)
    │ Manual verification (dogfood the built binary in an isolated runtime)
    ▼
  Tagged release / artifact publish (gated, manual trigger)
    │
    ▼
  Monitor for errors (post-install smoke + first-run window)
    │
    ├── Errors detected → Roll back to previous binary/version
    └── Clean → Done
```

### Rollback Plan

Every release should be reversible. Because zynk ships as a single binary, rollback is "reinstall the previous version":

```yaml
# Manual rollback workflow (re-publish a known-good tag's artifacts)
name: Rollback
on:
  workflow_dispatch:
    inputs:
      version:
        description: 'Version/tag to roll back to'
        required: true

jobs:
  rollback:
    runs-on: ubuntu-latest
    steps:
      - name: Re-publish previous release artifacts
        run: |
          echo "Re-publishing ${{ inputs.version }} as the current release"
          # gh release / artifact re-promotion for the specified tag
```

For a local install, keep the prior binary so an atomic `cp → mv` swap can be reversed.

## Environment and Secrets

```
config defaults     → Committed (printed by `cargo run -- --default-config`)
local config        → NOT committed (~/.config/zynk, machine-local)
test fixtures       → Committed (no real secrets)
CI secrets          → Stored in GitHub Secrets
Release/signing keys → Stored in GitHub Secrets / a vault
```

CI should never carry production/signing secrets in plaintext. Use GitHub Secrets. The private-content gates (`gitleaks`, `check_public_tree`, `scrub_check`) are the backstop that keeps secrets and maintainer-private paths out of the tree.

## Automation Beyond CI

### Dependency Updates

```yaml
# .github/dependabot.yml
version: 2
updates:
  - package-ecosystem: cargo
    directory: /
    schedule:
      interval: weekly
    open-pull-requests-limit: 5
  - package-ecosystem: github-actions
    directory: /
    schedule:
      interval: weekly
```

Pair with `cargo audit` / `cargo deny` in CI to catch advisories and license/dup violations.

### Build Cop Role

Designate someone responsible for keeping CI green. When the build breaks, the Build Cop's job is to fix or revert — not the person whose change caused the break. This prevents broken builds from accumulating while everyone assumes someone else will fix it.

### PR Checks

- **Required reviews:** At least 1 approval before merge
- **Required status checks:** CI + Gates must pass before merge
- **Branch protection:** No force-pushes to main
- **Auto-merge:** If all checks pass and approved, merge automatically

## CI Optimization

When the pipeline exceeds the timeout budget, apply these strategies in order of impact:

```
Slow CI pipeline?
├── Cache the cargo registry + build artifacts
│   └── Use Swatinem/rust-cache to reuse the target dir across runs
├── Run jobs in parallel
│   └── Split fmt/clippy/test/build across the OS matrix and separate jobs
├── Only run what changed
│   └── Use paths-ignore / path filters (e.g. skip CI for website-only changes)
├── Shard the test suite
│   └── Partition nextest across runners for large suites
├── Optimize the test suite
│   └── Move slow/env-sensitive tests off the critical path (run on a schedule)
└── Use larger runners
    └── GitHub-hosted larger runners or self-hosted for CPU-heavy builds
```

**Example: caching and parallelism**
```yaml
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --all-targets --locked -- -D warnings

  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: taiki-e/install-action@v2
        with: { tool: cargo-nextest }
      - uses: Swatinem/rust-cache@v2
      - run: cargo nextest run --locked
```

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "CI is too slow" | Optimize the pipeline (see CI Optimization), don't skip it. Caching the target dir prevents hours of debugging. |
| "This change is trivial, skip CI" | Trivial changes break builds. CI is fast for trivial changes anyway. |
| "The test is flaky, just re-run" | Flaky tests mask real bugs and waste everyone's time. Fix the flakiness (or pin the env-sensitive test out of the critical path with a documented reason). |
| "We'll add CI later" | Projects without CI accumulate broken states. Set it up on day one. |
| "Manual testing is enough" | Manual testing doesn't scale and isn't repeatable. Automate what you can. |
| "I'll just `#[allow]` the clippy warning" | An allow is a silenced gate. Fix the warning or document why the allow is correct. |

## Red Flags

- No CI pipeline in the project
- CI failures ignored or silenced
- Tests disabled in CI to make the pipeline pass
- Releases published without a verified build / dry-run
- No rollback mechanism (no way to reinstall the previous binary)
- Secrets stored in code or CI config files (not GitHub Secrets / vault)
- Private-content gate bypassed instead of fixing the root cause
- Long CI times with no optimization effort

## Verification

After setting up or modifying CI:

- [ ] All quality gates are present (fmt, clippy, tests, build, script/maintenance tests)
- [ ] Private-content gates run (check_public_tree + scrub_check + gitleaks)
- [ ] Conventional-commit validation runs on PR title / pushed subjects
- [ ] Pipeline runs on every PR and push to main
- [ ] Failures block merge (branch protection + required checks configured)
- [ ] CI results feed back into the development loop
- [ ] Secrets are in GitHub Secrets, not in code
- [ ] Releases are reversible (previous binary/version can be reinstalled)
- [ ] Pipeline stays within its timeout budget (caching + matrix parallelism)
