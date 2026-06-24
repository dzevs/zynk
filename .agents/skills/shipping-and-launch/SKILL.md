---
name: shipping-and-launch
description: Prepares production launches. Use when preparing to cut a release. Use when you need a pre-launch checklist, when setting up monitoring, when planning a staged rollout, or when you need a rollback strategy.
---

# Shipping and Launch

## Overview

Ship with confidence. The goal is not just to release — it's to release safely, with monitoring in place, a rollback plan ready, and a clear understanding of what success looks like. Every launch should be reversible, observable, and incremental.

For this repo, "launch" means cutting a versioned release of the `zynk` binary: tagging a version, publishing a GitHub binary release, pushing the crate to crates.io, and updating the Homebrew tap. The same discipline applies whether you're shipping to package registries or rolling out a new agent-detection behavior to live runtimes.

## When to Use

- Cutting a release of the binary to users for the first time
- Releasing a significant change to behavior (agent detection, IPC protocol, conversation layer)
- Migrating the on-disk DB schema or config format
- Opening a beta or early-access build
- Any release that carries risk (all of them)

## The Pre-Launch Checklist

### Code Quality

- [ ] All tests pass (`just test` / `just check` — cargo nextest + maintenance-script tests)
- [ ] Release build succeeds (`just build` = `cargo build --release --locked`) with no warnings
- [ ] Lint and format pass (`just lint` = `cargo fmt --check` + `cargo clippy --all-targets --locked -- -D warnings`)
- [ ] Code reviewed and approved
- [ ] No TODO comments that should be resolved before launch
- [ ] No leftover `dbg!`, `eprintln!`, or stray `println!` debugging statements in production code (use `tracing` instead)
- [ ] No `unwrap()`/`expect()` in production paths where a recoverable error is possible
- [ ] Error handling covers expected failure modes (PTY spawn failure, socket unavailable, DB migration mismatch)

### Security

- [ ] No secrets in code or version control (gitleaks / private-content gate clean)
- [ ] `cargo audit` shows no critical or high advisories; `cargo deny check` passes
- [ ] Input validation on all socket/IPC command boundaries
- [ ] No untrusted input reaches a shell, path, or process spawn without validation
- [ ] The private-content gate (`scripts/check_public_tree.py` + `.gitleaks.toml`) passes — no maintainer-private paths leak into the published tree
- [ ] Fail-closed defaults preserved (e.g. updater, DB cutover) — no fail-open regressions
- [ ] AGPL `NOTICE` / `LICENSE` upstream attribution preserved in the build (legally required)

### Performance

- [ ] Startup time / time-to-first-render within acceptable bounds on a cold start
- [ ] No accidental O(n²) or blocking work on the render/event hot path
- [ ] Render stays pure (`render()` takes `&AppState` and only draws — never mutates state)
- [ ] No unbounded growth in retained terminal/scrollback buffers
- [ ] Release binary size hasn't regressed unexpectedly (check `cargo build --release` artifact size)
- [ ] Async work stays off the UI thread; no blocking calls inside the tokio reactor

### Robustness

- [ ] PTY teardown is clean on pane close (no leaked master FDs)
- [ ] Platform-specific behavior stays isolated in `src/platform/`; core modules build on every target
- [ ] Detection stays evidence-based (reads a screen snapshot only, never the parser/viewport)
- [ ] Graceful degradation when the socket server is unreachable
- [ ] Error messages surfaced to the user are descriptive and actionable, not internal panics

### Infrastructure

- [ ] Version bumped in `Cargo.toml` (and lockfile updated)
- [ ] DB migrations applied / forward-compatible (or a documented migration path exists)
- [ ] `CHANGELOG.md` updated for the release
- [ ] Release artifacts build reproducibly (`--locked`, pinned Zig 0.15.2 for `libghostty-vt`)
- [ ] Logging via `tracing` is configured and readable
- [ ] Health/version surface works (`zynk --version`, socket reachable)

### Documentation

- [ ] README updated with any new setup requirements
- [ ] CLI `--help` / command docs current
- [ ] ADRs written under `docs/zynk/decisions/` for any architectural decisions (binding once accepted — amend via a new ADR, never rewrite)
- [ ] Changelog updated
- [ ] User-facing documentation updated (if applicable)

## Feature Flag Strategy

Ship behind feature flags / config gates to decouple release from activation:

```rust
// Config/flag check before activating new behavior
if config.flags.task_sharing {
    // New feature: task sharing
    return Some(render_task_sharing_panel(task));
}

// Default: existing behavior
None
```

**Feature flag lifecycle:**

```
1. RELEASE with flag OFF     → Code is shipped but inactive
2. ENABLE for team/beta     → Internal testing on real runtimes
3. GRADUAL ROLLOUT          → opt-in flag → default-on for beta → default-on for all
4. MONITOR at each stage    → Watch error rates, crash reports, user feedback
5. CLEAN UP                 → Remove flag and dead code path after full rollout
```

**Rules:**
- Every feature flag has an owner and an expiration date
- Clean up flags within 2 weeks of full rollout
- Don't nest feature flags (creates exponential combinations)
- Test both flag states (on and off) in CI

## Staged Rollout

### The Rollout Sequence

```
1. VALIDATE locally (isolated dev runtime)
   └── Full test suite (`just check`) on an isolated CARGO_TARGET_DIR
   └── Manual smoke test of critical flows against a dev socket/config (never the live one)

2. TAG + BUILD release artifacts (flag OFF)
   └── Verify the release binary boots and reports the right version
   └── Check the build is clean (no warnings, locked deps)

3. ENABLE for team (flag ON for internal/dogfood install)
   └── Team runs the build as their live runtime
   └── 24-hour monitoring window

4. CANARY rollout (publish to a beta channel / pre-release tag)
   └── Monitor crash reports, error logs, regression reports
   └── Compare: canary vs. previous stable
   └── 24-48 hour monitoring window
   └── Advance only if all thresholds pass (see table below)

5. PUBLISH widely (crates.io + GitHub release + Homebrew tap)
   └── Same monitoring after publish
   └── Ability to yank / point users back to the previous version

6. FULL rollout (default-on for all users)
   └── Monitor for 1 week
   └── Clean up feature flag
```

### Rollout Decision Thresholds

Use these thresholds to decide whether to advance, hold, or roll back at each stage:

| Metric | Advance (green) | Hold and investigate (yellow) | Roll back (red) |
|--------|-----------------|-------------------------------|-----------------|
| Crash / panic rate | Within 10% of baseline | 10-100% above baseline | >2x baseline |
| Startup / hot-path latency | Within 20% of baseline | 20-50% above baseline | >50% above baseline |
| New error-log types | No new error types | New errors in <0.1% of sessions | New errors in >0.1% of sessions |
| User-facing regressions | None reported | Minor cosmetic only | Functional / data-loss reports |

### When to Roll Back

Roll back immediately if:
- Crash/panic rate increases by more than 2x baseline
- Startup or hot-path latency increases by more than 50%
- User-reported issues spike
- Data integrity issues detected (corrupted DB, lost conversation state)
- Security vulnerability discovered

## Monitoring and Observability

### What to Monitor

```
Application signals:
├── Crash / panic rate (total and by code path)
├── Command latency (p50, p95, p99) for socket/IPC commands
├── Active panes / sessions
├── Error-log volume (by module via tracing targets)
└── Key behavior metrics (agent detection accuracy, message delivery)

Runtime / host signals:
├── CPU and memory utilization of the server process
├── PTY master FD count (leak detection)
├── Socket connection count
├── DB size growth
└── Background task / event-loop backlog

Client signals:
├── Render frame timing
├── Input-to-effect latency
└── User-reported error rates
```

### Error Reporting

```rust
// Centralize error reporting so failures are observable, not silently swallowed.
use tracing::error;

fn handle_command(cmd: Command, ctx: &Ctx) -> Result<Response, AppError> {
    match dispatch(cmd, ctx) {
        Ok(resp) => Ok(resp),
        Err(err) => {
            // Report with structured context; don't leak internals to the client.
            error!(
                command = %cmd.name(),
                session = ctx.session_id(),
                error = %err,
                "command failed"
            );
            // Return a stable, non-internal error shape over the wire.
            Err(AppError::internal("something went wrong"))
        }
    }
}
```

The rules that matter:
- **Never swallow an error silently** — at minimum log it via `tracing` with enough context to localize the failure.
- **Don't expose internals** to the user/client; map to a stable error shape.
- **Attach context** (command, session, target) so a single log line is enough to start debugging.

### Post-Launch Verification

In the first hour after launch:

```
1. Check `zynk --version` reports the released version and the binary boots
2. Check error logs / crash reports (no new error types)
3. Check latency on the hot commands (no regression)
4. Test the critical user flow manually (spawn pane, run agent, exchange a message)
5. Verify logs are flowing and readable
6. Confirm rollback mechanism works (dry run: can you reinstall the previous version?)
```

## Rollback Strategy

Every release needs a rollback plan before it happens:

```markdown
## Rollback Plan for [Feature/Release]

### Trigger Conditions
- Crash/panic rate > 2x baseline
- Hot-path latency > [X]ms
- User reports of [specific issue]

### Rollback Steps
1. Disable feature flag / config gate (if applicable)
   OR
1. Point users back to the previous version:
   - crates.io: `cargo yank --version X.Y.Z` (then publish a fixed patch)
   - GitHub release: mark the release as a draft / re-pin "latest" to the prior tag
   - Homebrew tap: revert the formula to the previous bottle/version
2. Revert the code: `git revert <commit> && git push` (on a branch; never force-push main)
3. Verify rollback: `zynk --version`, error logs clean
4. Communicate: notify team of rollback

### DB / On-Disk Considerations
- Migration [X] is forward-only — document the manual downgrade path (drop index, delete the `_sqlx_migrations` row)
- Data written by the new feature: [preserved / cleaned up]
- Never assume tests isolate the live DB — verify migrations don't touch the live runtime's on-disk DB

### Time to Rollback
- Feature flag / config gate: < 1 minute
- Reinstall previous binary: < 5 minutes
- DB downgrade: < 15 minutes
```

## See Also

- For security pre-launch checks, see `references/security-checklist.md`
- For performance pre-launch checklist, see `references/performance-checklist.md`
- For robustness/teardown verification before launch, see `references/robustness-checklist.md`

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "It works on my machine, it'll work for users" | Users have different terminals, agents, OSes, and edge cases. Monitor after release. |
| "We don't need a feature flag for this" | Every feature benefits from a kill switch. Even "simple" changes can break things. |
| "Monitoring is overhead" | Not having monitoring means you discover problems from user complaints instead of logs. |
| "We'll add monitoring later" | Add it before launch. You can't debug what you can't see. |
| "Rolling back is admitting failure" | Rolling back is responsible engineering. Shipping a broken release is the failure. |
| "Just publish to crates.io and tag it now" | Publish/tag/yank are separately gated, irreversible-ish operations. Validate locally first, then gate each one explicitly. |

## Red Flags

- Releasing without a rollback plan
- No monitoring or error reporting on the live runtime
- Big-bang releases (everything at once, no staged validation)
- Feature flags with no expiration or owner
- No one monitoring the release for the first hour
- Release/version configuration done by memory, not by the lockfile and `Cargo.toml`
- "It's Friday afternoon, let's tag and publish"
- Publishing to crates.io, tagging, or yanking without a separate explicit gate
- Tests that migrate the live on-disk DB instead of an isolated one

## Verification

Before releasing:

- [ ] Pre-launch checklist completed (all sections green)
- [ ] Feature flag / config gate configured (if applicable)
- [ ] Rollback plan documented
- [ ] Monitoring / log dashboards set up
- [ ] Team notified of the release
- [ ] `just check` clean on an isolated runtime

After releasing:

- [ ] `zynk --version` reports the released version and the binary boots
- [ ] Crash/panic rate is normal
- [ ] Latency is normal
- [ ] Critical user flow works
- [ ] Logs are flowing
- [ ] Rollback tested or verified ready
