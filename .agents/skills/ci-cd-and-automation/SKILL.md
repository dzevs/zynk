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
  check-required:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v6
        with:
          persist-credentials: false

      # The repo pins its toolchain in rust-toolchain.toml; the real workflow reads the channel out of
      # that file and passes it here, so CI pre-installs the pinned version rather than `stable`.
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: ${{ steps.rust-toolchain.outputs.channel }}
          components: rustfmt,clippy

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

      # `just check` = lint + TS + nextest + the Python maintenance tests. `just ci` is only the fast subset
      # (lint + TS + nextest) — never use it as the CI gate.
      - name: Run the full check
        run: just check
```

> **Note:** The bundled `libghostty-vt` is built with Zig, so CI must install Zig 0.15.2; the TS asset test needs Bun; the gitleaks maintenance tests need the `gitleaks` binary (install it as `ci.yml` does, or those tests skip). Pin action versions (ideally by commit SHA) for supply-chain safety.

> **Single build target (ADR 0013).** zynk builds for Linux x86_64 only; every other target is a compile error. `check-required` on `ubuntu-latest` is therefore the whole functional gate — there is no build matrix, no optional tier and no per-target evidence to reconcile. Never add a runner for another OS.

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

### Verifying a release build

zynk publishes no binaries, so there is no packaging pipeline to dry-run. The release build is verified where it is made — locally, from the exact reviewed SHA:

```bash
git switch --detach <reviewed-sha>
just check                      # the full gate
just build                      # cargo build --release --locked
sha256sum target/release/zynk   # record the SHA + this hash in the gate
```

Record the commit SHA and that sha256 with the operator gate. A binary whose provenance is not recorded is not release evidence.

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

Every release should be reversible. Because zynk ships as a single binary built from source, rollback is "reinstall the previous version":

```bash
git switch --detach <previous-good-sha>
just build
cp target/release/zynk ~/.cargo/bin/zynk.new && mv ~/.cargo/bin/zynk.new ~/.cargo/bin/zynk
zynk server stop     # the new binary takes effect on the next server start
```

Keep the prior binary beside the live one so the atomic `cp → mv` swap can be reversed without a rebuild. On crates.io, `cargo yank --version X.Y.Z` withdraws a bad version (then publish a fixed patch) — an operator gate of its own.

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
- Releases published without a recorded build SHA and binary sha256
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
