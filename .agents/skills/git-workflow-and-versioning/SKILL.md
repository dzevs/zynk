---
name: git-workflow-and-versioning
description: Structures git workflow practices. Use when making any code change. Use when committing, branching, resolving conflicts, or when you need to organize work across multiple parallel streams.
---

# Git Workflow and Versioning

## Overview

Git is your safety net. Treat commits as save points, branches as sandboxes, and history as documentation. With AI agents generating code at high speed, disciplined version control is the mechanism that keeps changes manageable, reviewable, and reversible.

## When to Use

Always. Every code change flows through git.

## Core Principles

### Trunk-Based Development (Recommended)

Keep `main` always deployable. Work in short-lived feature branches that merge back within 1-3 days. Long-lived development branches are hidden costs — they diverge, create merge conflicts, and delay integration. DORA research consistently shows trunk-based development correlates with high-performing engineering teams.

```
main ──●──●──●──●──●──●──●──●──●──  (always deployable)
        ╲      ╱  ╲    ╱
         ●──●─╱    ●──╱    ← short-lived feature branches (1-3 days)
```

This is the recommended default. Teams using gitflow or long-lived branches can adapt the principles (atomic commits, small changes, descriptive messages) to their branching model — the commit discipline matters more than the specific branching strategy.

- **Dev branches are costs.** Every day a branch lives, it accumulates merge risk.
- **Release branches are acceptable.** When you need to stabilize a release while main moves forward.
- **Feature flags > long branches.** Prefer deploying incomplete work behind flags rather than keeping it on a branch for weeks.

### 1. Commit Early, Commit Often

Each successful increment gets its own commit. Don't accumulate large uncommitted changes.

```
Work pattern:
  Implement slice → Test → Verify → Commit → Next slice

Not this:
  Implement everything → Hope it works → Giant commit
```

Commits are save points. If the next change breaks something, you can revert to the last known-good state instantly.

### 2. Atomic Commits

Each commit does one logical thing:

```
# Good: Each commit is self-contained
git log --oneline
a1b2c3d feat(api): add message.send socket command with validation
d4e5f6g feat(detect): add codex working-state gate
h7i8j9k fix(pty): release master fd on pane close
m1n2o3p test: cover message delivery to a missing pane

# Bad: Everything mixed together
x1y2z3a add message feature, fix sidebar, bump deps, refactor detect
```

### 3. Descriptive Messages

Commit messages explain the *why*, not just the *what*:

```
# Good: Explains intent
feat(api): add validation to the message.send handler

Prevents malformed protocol fields from reaching the conversation DB.
Validates the target/body/trace shape at the handler level, consistent
with existing validation in the api layer.

# Bad: Describes what's obvious from the diff
update api.rs
```

**Format (lowercase conventional commits — enforced by `scripts/conventional_commits.py`):**
```
<type>(<optional-scope>): <short description>

<optional body explaining why, not what>
```

**Types (allowed by the gate):**
- `feat` — New feature
- `fix` — Bug fix
- `refactor` — Code change that neither fixes a bug nor adds a feature
- `test` — Adding or updating tests
- `docs` — Documentation only
- `chore` — Tooling, dependencies, config

No emojis, no AI co-author lines. Scopes are optional but useful (e.g. `fix(update):`, `chore(gate):`).

### 4. Keep Concerns Separate

Don't combine formatting changes with behavior changes. Don't combine refactors with features. Each type of change should be a separate commit — and ideally a separate PR:

```
# Good: Separate concerns
git commit -m "refactor: extract delivery-receipt builder into a helper"
git commit -m "feat: add trace-id correlation to message.send"

# Bad: Mixed concerns
git commit -m "refactor receipt and add trace-id field"
```

**Separate refactoring from feature work.** A refactoring change and a feature change are two different changes — submit them separately. This makes each change easier to review, revert, and understand in history. Small cleanups (renaming a variable) can be included in a feature commit at reviewer discretion.

### 5. Size Your Changes

Target ~100 lines per commit/PR. Changes over ~1000 lines should be split.

```
~100 lines  → Easy to review, easy to revert
~300 lines  → Acceptable for a single logical change
~1000 lines → Split into smaller changes
```

## Branching Strategy

### Feature Branches

```
main (always deployable)
  │
  ├── feat/message-send-validation  ← One feature per branch
  ├── feat/codex-working-gate       ← Parallel work
  └── fix/pty-master-fd-leak        ← Bug fixes
```

- Branch from `main` (or the team's default branch)
- Keep branches short-lived (merge within 1-3 days) — long-lived branches are hidden costs
- Delete branches after merge
- Prefer feature flags over long-lived branches for incomplete features

### Branch Naming

```
feat/<short-description>      → feat/message-send-validation
fix/<short-description>       → fix/pty-master-fd-leak
chore/<short-description>     → chore/update-deps
refactor/<short-description>  → refactor/detect-module
```

## Working with Worktrees

For parallel AI agent work, use git worktrees to run multiple branches simultaneously:

```bash
# Create a worktree for a feature branch
git worktree add ../zynk-feature-a feat/message-send-validation
git worktree add ../zynk-feature-b feat/codex-working-gate

# Each worktree is a separate directory with its own branch
# Agents can work in parallel without interfering
ls ../
  zynk/              ← main branch
  zynk-feature-a/    ← message-send-validation branch
  zynk-feature-b/    ← codex-working-gate branch

# When done, merge and clean up
git worktree remove ../zynk-feature-a
```

Benefits:
- Multiple agents can work on different features simultaneously
- No branch switching needed (each directory has its own branch)
- If one experiment fails, delete the worktree — nothing is lost
- Changes are isolated until explicitly merged

> Note: each worktree gets its own checkout, but they share the same `target/`-relative tooling assumptions — point dev builds at an isolated `CARGO_TARGET_DIR` so concurrent builds don't fight over the default target.

## The Save Point Pattern

```
Agent starts work
    │
    ├── Makes a change
    │   ├── Test passes? → Commit → Continue
    │   └── Test fails? → Revert to last commit → Investigate
    │
    ├── Makes another change
    │   ├── Test passes? → Commit → Continue
    │   └── Test fails? → Revert to last commit → Investigate
    │
    └── Feature complete → All commits form a clean history
```

This pattern means you never lose more than one increment of work. If an agent goes off the rails, `git reset --hard HEAD` takes you back to the last successful state (use deliberately — it discards uncommitted work).

## Change Summaries

After any modification, provide a structured summary. This makes review easier, documents scope discipline, and surfaces unintended changes:

```
CHANGES MADE:
- src/api/message.rs: Added validation to the message.send handler
- src/protocol/mod.rs: Added MessageSendInput shape + parse

THINGS I DIDN'T TOUCH (intentionally):
- src/api/status.rs: Has a similar validation gap but out of scope
- src/server/mod.rs: Error format could be improved (separate task)

POTENTIAL CONCERNS:
- Validation rejects unknown protocol fields — confirm this is desired.
- No new dependency added; uses the existing serde-based parse path.
```

This pattern catches wrong assumptions early and gives reviewers a clear map of the change. The "DIDN'T TOUCH" section is especially important — it shows you exercised scope discipline and didn't go on an unsolicited renovation.

## Pre-Commit Hygiene

Before every commit:

```bash
# 1. Check what you're about to commit
git diff --staged

# 2. Ensure no secrets
git diff --staged | grep -i "password\|secret\|api_key\|token"

# 3. Run the full check (fmt + clippy + tests + script gates)
just check

# 4. (faster inner loop) lint + a focused test while iterating
just lint
just test-one <filter>
```

Automate this with repo-local git hooks:

```bash
# One-time per checkout — installs the pre-commit + commit-msg hooks
# (lint + private-content gates + conventional-commit validation)
just install-hooks
```

The hooks enforce `cargo fmt`/`clippy`, the conventional-commit format, and the private-content gates (`scripts/check_public_tree.py` structural + `.gitleaks.toml` content). On any gate failure: STOP, fix the root cause, never bypass.

## Handling Generated Files

- **Commit lockfiles and vendored sources** the project expects (e.g., `Cargo.lock`, the vendored `libghostty-vt` source dist)
- **Don't commit** build output (`target/`, `result*`), local settings (`.claude/settings.local.json`), or maintainer-private paths
- **The `.gitignore`** already covers `target/`, `__pycache__/`, private paths, and local config — keep it current

## Using Git for Debugging

```bash
# Find which commit introduced a bug
git bisect start
git bisect bad HEAD
git bisect good <known-good-commit>
# Git checks out midpoints; run your test at each to narrow down

# View what changed recently
git log --oneline -20
git diff HEAD~5..HEAD -- src/

# Find who last changed a specific line
git blame src/detect/codex.rs

# Search commit messages for a keyword
git log --grep="delivery" --oneline
```

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "I'll commit when the feature is done" | One giant commit is impossible to review, debug, or revert. Commit each slice. |
| "The message doesn't matter" | Messages are documentation. Future you (and future agents) will need to understand what changed and why. |
| "I'll squash it all later" | Squashing destroys the development narrative. Prefer clean incremental commits from the start. |
| "Branches add overhead" | Short-lived branches are free and prevent conflicting work from colliding. Long-lived branches are the problem — merge within 1-3 days. |
| "I'll split this change later" | Large changes are harder to review, riskier to deploy, and harder to revert. Split before submitting, not after. |
| "I don't need the private-content gate" | Until a maintainer-private path or secret gets committed. The hooks catch it before push — don't bypass them. |

## Red Flags

- Large uncommitted changes accumulating
- Commit messages like "fix", "update", "misc" (the conventional-commit gate rejects these anyway)
- Formatting changes mixed with behavior changes
- Committing `target/`, secrets, or private paths
- Long-lived branches that diverge significantly from main
- Force-pushing to shared branches

## Verification

For every commit:

- [ ] Commit does one logical thing
- [ ] Message follows lowercase conventional-commit format (passes `scripts/conventional_commits.py`)
- [ ] `just check` is clean before committing
- [ ] No secrets or private paths in the diff (private-content gate passes)
- [ ] No formatting-only changes mixed with behavior changes
- [ ] Hooks installed (`just install-hooks`) so gates run automatically
