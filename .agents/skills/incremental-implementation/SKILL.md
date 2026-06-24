---
name: incremental-implementation
description: Delivers changes incrementally. Use when implementing any feature or change that touches more than one file. Use when you're about to write a large amount of code at once, or when a task feels too big to land in one step.
---

# Incremental Implementation

## Overview

Build in thin vertical slices — implement one piece, test it, verify it, then expand. Avoid implementing an entire feature in one pass. Each increment should leave the system in a working, testable state. This is the execution discipline that makes large features manageable.

## When to Use

- Implementing any multi-file change
- Building a new feature from a task breakdown
- Refactoring existing code
- Any time you're tempted to write more than ~100 lines before testing

**When NOT to use:** Single-file, single-function changes where the scope is already minimal.

## The Increment Cycle

```
┌──────────────────────────────────────┐
│                                      │
│   Implement ──→ Test ──→ Verify ──┐  │
│       ▲                           │  │
│       └───── Commit ◄─────────────┘  │
│              │                       │
│              ▼                       │
│          Next slice                  │
│                                      │
└──────────────────────────────────────┘
```

For each slice:

1. **Implement** the smallest complete piece of functionality
2. **Test** — run the test suite (or write a test if none exists)
3. **Verify** — confirm the slice works as expected (tests pass, build succeeds, manual check)
4. **Commit** -- save your progress with a descriptive message (lowercase conventional commits, no emojis, no AI co-author lines)
5. **Move to the next slice** — carry forward, don't restart

## Slicing Strategies

### Vertical Slices (Preferred)

Build one complete path through the stack:

```
Slice 1: Create a pane (state + protocol command + basic CLI)
    → Tests pass, user can create a pane via the CLI

Slice 2: List panes (query + protocol command + CLI output)
    → Tests pass, user can see their panes

Slice 3: Rename a pane (mutation + protocol command + render)
    → Tests pass, user can modify panes

Slice 4: Close a pane (state + protocol command + CLI + confirmation)
    → Tests pass, full lifecycle complete
```

Each slice delivers working end-to-end functionality.

### Contract-First Slicing

When the server handler and the CLI client need to develop in parallel:

```
Slice 0: Define the protocol/IPC contract (request/response types in src/protocol)
Slice 1a: Implement the server-side handler against the contract + integration tests
Slice 1b: Implement the CLI client against the same contract types
Slice 2: Integrate and test end-to-end over the socket
```

### Risk-First Slicing

Tackle the riskiest or most uncertain piece first:

```
Slice 1: Prove the PTY/emulator round-trip works (highest risk)
Slice 2: Build the feature on the proven terminal pipeline
Slice 3: Add edge-case handling (resize, reattach, scrollback)
```

If Slice 1 fails, you discover it before investing in Slices 2 and 3.

## Implementation Rules

### Rule 0: Simplicity First

Before writing any code, ask: "What is the simplest thing that could work?"

After writing code, review it against these checks:
- Can this be done in fewer lines?
- Are these abstractions earning their complexity?
- Would a staff engineer look at this and say "why didn't you just..."?
- Am I building for hypothetical future requirements, or the current task?

```
SIMPLICITY CHECK:
✗ A generic trait + dyn dispatch layer for one detection rule
✓ A plain function

✗ An abstract builder for two similar protocol messages
✓ Two straightforward structs with a shared helper

✗ A config-driven layout engine for three fixed layouts
✓ Three explicit layout functions
```

Three similar lines of code is better than a premature abstraction. Implement the naive, obviously-correct version first. Optimize only after correctness is proven with tests.

### Rule 0.5: Scope Discipline

Touch only what the task requires.

Do NOT:
- "Clean up" code adjacent to your change
- Reorder `use` imports in files you're not modifying
- Remove comments you don't fully understand
- Add features not in the spec because they "seem useful"
- Modernize syntax in files you're only reading

If you notice something worth improving outside your task scope, note it — don't fix it:

```
NOTICED BUT NOT TOUCHING:
- src/terminal/screen.rs has an unused import (unrelated to this task)
- The detect module could use clearer error messages (separate task)
→ Want me to create tasks for these?
```

### Rule 1: One Thing at a Time

Each increment changes one logical thing. Don't mix concerns:

**Bad:** One commit that adds a new protocol command, refactors an existing handler, and updates the CI config.

**Good:** Three separate commits — one for each change.

### Rule 2: Keep It Compilable

After each increment, the project must build and existing tests must pass. Don't leave the codebase in a broken state between slices. `cargo build` / `just build` must succeed and `just lint` must stay clean (clippy runs with `-D warnings`).

### Rule 3: Feature Flags for Incomplete Features

If a feature isn't ready for users but you need to merge increments, gate it behind a `ZYNK_*` environment flag:

```rust
// Feature flag for work-in-progress
let enable_pane_sharing = std::env::var("ZYNK_FEATURE_PANE_SHARING")
    .map(|v| v == "1")
    .unwrap_or(false);

if enable_pane_sharing {
    // New sharing path
}
```

This lets you merge small increments to the main branch without exposing incomplete work.

### Rule 4: Safe Defaults

New code should default to safe, conservative behavior:

```rust
// Safe: disabled by default, opt-in
pub struct CreatePaneOptions {
    pub notify: bool, // defaults to false via Default
}

pub fn create_pane(input: PaneInput, options: CreatePaneOptions) -> Result<PaneId> {
    let should_notify = options.notify; // off unless explicitly requested
    // ...
}
```

### Rule 5: Rollback-Friendly

Each increment should be independently revertable:

- Additive changes (new files, new functions) are easy to revert
- Modifications to existing code should be minimal and focused
- DB migrations should have a corresponding rollback path
- Avoid deleting something in one commit and replacing it in the same commit — separate them

## Working with Agents

When directing an agent to implement incrementally:

```
"Let's implement Task 3 from the plan.

Start with just the state mutation and the protocol command.
Don't touch the rendering yet — we'll do that in the next increment.

After implementing, run `just test` and `just build` to verify
nothing is broken."
```

Be explicit about what's in scope and what's NOT in scope for each increment.

## Increment Checklist

After each increment, verify:

- [ ] The change does one thing and does it completely
- [ ] All existing tests still pass (`just test`)
- [ ] The build succeeds (`just build`)
- [ ] Formatting is clean (`cargo fmt --check`)
- [ ] Clippy passes with no warnings (`cargo clippy --all-targets --locked -- -D warnings`)
- [ ] The new functionality works as expected
- [ ] The change is committed with a descriptive message

**Note:** Run each verification command after a change that could affect it. After a successful run, don't repeat the same command unless the code has changed since — re-running on unchanged code adds no information.

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "I'll test it all at the end" | Bugs compound. A bug in Slice 1 makes Slices 2-5 wrong. Test each slice. |
| "It's faster to do it all at once" | It *feels* faster until something breaks and you can't find which of 500 changed lines caused it. |
| "These changes are too small to commit separately" | Small commits are free. Large commits hide bugs and make rollbacks painful. |
| "I'll add the feature flag later" | If the feature isn't complete, it shouldn't be user-visible. Add the flag now. |
| "This refactor is small enough to include" | Refactors mixed with features make both harder to review and debug. Separate them. |
| "Let me run the build command again just to be sure" | After a successful run, repeating the same command adds nothing unless the code has changed since. Run it again after subsequent edits, not as reassurance. |

## Red Flags

- More than 100 lines of code written without running tests
- Multiple unrelated changes in a single increment
- "Let me just quickly add this too" scope expansion
- Skipping the test/verify step to move faster
- Build or tests broken between increments
- Large uncommitted changes accumulating
- Building abstractions before the third use case demands it
- Touching files outside the task scope "while I'm here"
- Creating new utility files for one-time operations
- Running the same build/test command twice in a row without any intervening code change

## Verification

After completing all increments for a task:

- [ ] Each increment was individually tested and committed
- [ ] The full test suite passes (`just test`)
- [ ] The build is clean (`just build`) and lint passes (`just lint`)
- [ ] The feature works end-to-end as specified
- [ ] No uncommitted changes remain
