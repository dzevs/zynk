---
name: code-simplification
description: Simplifies code for clarity. Use when refactoring code for clarity without changing behavior. Use when code works but is harder to read, maintain, or extend than it should be. Use when reviewing code that has accumulated unnecessary complexity.
---

# Code Simplification

> Inspired by the [Claude Code Simplifier plugin](https://github.com/anthropics/claude-plugins-official/blob/main/plugins/code-simplifier/agents/code-simplifier.md). Adapted here as a model-agnostic, process-driven skill for any AI coding agent.

## Overview

Simplify code by reducing complexity while preserving exact behavior. The goal is not fewer lines — it's code that is easier to read, understand, modify, and debug. Every simplification must pass a simple test: "Would a new team member understand this faster than the original?"

## When to Use

- After a feature is working and tests pass, but the implementation feels heavier than it needs to be
- During code review when readability or complexity issues are flagged
- When you encounter deeply nested logic, long functions, or unclear names
- When refactoring code written under time pressure
- When consolidating related logic scattered across modules
- After merging changes that introduced duplication or inconsistency

**When NOT to use:**

- Code is already clean and readable — don't simplify for the sake of it
- You don't understand what the code does yet — comprehend before you simplify
- The code is performance-critical (e.g. a per-cell render or per-byte parser hot path) and the "simpler" version would be measurably slower
- You're about to rewrite the module entirely — simplifying throwaway code wastes effort

## The Five Principles

### 1. Preserve Behavior Exactly

Don't change what the code does — only how it expresses it. All inputs, outputs, side effects, error behavior, and edge cases must remain identical. If you're not sure a simplification preserves behavior, don't make it.

```
ASK BEFORE EVERY CHANGE:
→ Does this produce the same output for every input?
→ Does this maintain the same error behavior (same Result/Option, same panic vs. not)?
→ Does this preserve the same side effects and ordering?
→ Do all existing tests still pass without modification?
```

### 2. Follow Project Conventions

Simplification means making code more consistent with the codebase, not imposing external preferences. Before simplifying:

```
1. Read CLAUDE.md / project conventions
2. Study how neighboring code handles similar patterns
3. Match the project's style for:
   - Module layout and import/use ordering
   - Function declaration style
   - Naming conventions
   - Error handling patterns (?, Result, anyhow-style vs. typed errors)
   - Type annotation depth
```

In this codebase that means: no `unwrap()` in production code, `tracing` for logging, OS-specific behavior isolated in `src/platform/`, and the state/runtime + pure-render invariants intact. `rustfmt` and `clippy -D warnings` are the style authority. Simplification that breaks project consistency is not simplification — it's churn.

### 3. Prefer Clarity Over Cleverness

Explicit code is better than compact code when the compact version requires a mental pause to parse.

```rust
// UNCLEAR: Dense nested ternary-style chain
let label = if is_new { "New" } else if is_updated { "Updated" } else if is_archived { "Archived" } else { "Active" };

// CLEAR: Readable function with early returns
fn status_label(item: &Item) -> &'static str {
    if item.is_new { return "New"; }
    if item.is_updated { return "Updated"; }
    if item.is_archived { return "Archived"; }
    "Active"
}
```

```rust
// UNCLEAR: Chained fold with inline mutation logic
let counts = items.iter().fold(HashMap::new(), |mut acc, item| {
    *acc.entry(item.id.clone()).or_insert(0) += 1;
    acc
});

// CLEAR: Named intermediate step
let mut count_by_id: HashMap<String, usize> = HashMap::new();
for item in &items {
    *count_by_id.entry(item.id.clone()).or_insert(0) += 1;
}
```

### 4. Maintain Balance

Simplification has a failure mode: over-simplification. Watch for these traps:

- **Inlining too aggressively** — removing a helper that gave a concept a name makes the call site harder to read
- **Combining unrelated logic** — two simple functions merged into one complex function is not simpler
- **Removing "unnecessary" abstraction** — some abstractions exist for extensibility or testability, not complexity
- **Optimizing for line count** — fewer lines is not the goal; easier comprehension is

### 5. Scope to What Changed

Default to simplifying recently modified code. Avoid drive-by refactors of unrelated code unless explicitly asked to broaden scope. Unscoped simplification creates noise in diffs and risks unintended regressions.

## The Simplification Process

### Step 1: Understand Before Touching (Chesterton's Fence)

Before changing or removing anything, understand why it exists. This is Chesterton's Fence: if you see a fence across a road and don't understand why it's there, don't tear it down. First understand the reason, then decide if the reason still applies.

```
BEFORE SIMPLIFYING, ANSWER:
- What is this code's responsibility?
- What calls it? What does it call?
- What are the edge cases and error paths?
- Are there tests that define the expected behavior?
- Why might it have been written this way? (Performance? Platform constraint? Historical reason?)
- Check git blame: what was the original context for this code?
```

If you can't answer these, you're not ready to simplify. Read more context first. In this codebase, an extra check: a value or branch may exist to keep `AppState`/`PaneState` pure or to keep `render()` side-effect-free — don't collapse it in a way that smuggles mutation into render or couples state to the runtime.

### Step 2: Identify Simplification Opportunities

Scan for these patterns — each one is a concrete signal, not a vague smell:

**Structural complexity:**

| Pattern | Signal | Simplification |
|---------|--------|----------------|
| Deep nesting (3+ levels) | Hard to follow control flow | Extract conditions into guard clauses or helper functions; use `?` for error propagation |
| Long functions (50+ lines) | Multiple responsibilities | Split into focused functions with descriptive names |
| Nested `if`/`else` ladders | Requires mental stack to parse | Replace with early returns, `match`, or a lookup table |
| Boolean parameter flags | `do_thing(true, false, true)` | Replace with an options struct, an enum, or separate functions |
| Repeated conditionals | Same `if` check in multiple places | Extract to a well-named predicate function |

**Naming and readability:**

| Pattern | Signal | Simplification |
|---------|--------|----------------|
| Generic names | `data`, `result`, `tmp`, `val`, `item` | Rename to describe the content: `pane_state`, `validation_errors` |
| Abbreviated names | `usr`, `cfg`, `btn`, `evt` | Use full words unless the abbreviation is universal (`id`, `url`, `pty`) |
| Misleading names | Function named `get_` that also mutates state | Rename to reflect actual behavior |
| Comments explaining "what" | `// increment counter` above `count += 1` | Delete the comment — the code is clear enough |
| Comments explaining "why" | `// Retry because the PTY may not be ready immediately` | Keep these — they carry intent the code can't express |

**Redundancy:**

| Pattern | Signal | Simplification |
|---------|--------|----------------|
| Duplicated logic | Same 5+ lines in multiple places | Extract to a shared function |
| Dead code | Unreachable branches, unused fields, commented-out blocks | Remove (after confirming it's truly dead — beware `#[cfg(test)]`-only items clippy flags) |
| Unnecessary abstractions | Wrapper that adds no value | Inline the wrapper, call the underlying function directly |
| Over-engineered patterns | Trait-with-one-impl, builder-for-two-fields | Replace with the simple direct approach |
| Redundant conversions | `.clone()` where a borrow works; `.into()` to a type already held | Borrow instead of clone; drop the conversion |

### Step 3: Apply Changes Incrementally

Make one simplification at a time. Run tests after each change. **Submit refactoring changes separately from feature or bug fix changes.** A PR that refactors and adds a feature is two PRs — split them.

```
FOR EACH SIMPLIFICATION:
1. Make the change
2. Run the test suite (just test / cargo nextest run --locked)
3. If tests pass → commit (or continue to next simplification)
4. If tests fail → revert and reconsider
```

Avoid batching multiple simplifications into a single untested change. If something breaks, you need to know which simplification caused it.

**The Rule of 500:** If a refactoring would touch more than 500 lines, invest in automation (`cargo fix`, `cargo clippy --fix`, AST transforms, scripted edits) rather than making the changes by hand. Manual edits at that scale are error-prone and exhausting to review.

### Step 4: Verify the Result

After all simplifications, step back and evaluate the whole:

```
COMPARE BEFORE AND AFTER:
- Is the simplified version genuinely easier to understand?
- Did you introduce any new patterns inconsistent with the codebase?
- Is the diff clean and reviewable?
- Would a teammate approve this change?
```

If the "simplified" version is harder to understand or review, revert. Not every simplification attempt succeeds.

## Language-Specific Guidance

### Rust idioms

```rust
// SIMPLIFY: Unnecessary async wrapper that only forwards
// Before
async fn get_pane(&self, id: &str) -> Result<Pane> {
    self.store.find_by_id(id).await
}
// After — return the future directly when no extra await work is done
fn get_pane(&self, id: &str) -> impl Future<Output = Result<Pane>> + '_ {
    self.store.find_by_id(id)
}

// SIMPLIFY: Verbose conditional assignment
// Before
let display_name: String;
if let Some(nick) = &user.nickname {
    display_name = nick.clone();
} else {
    display_name = user.full_name.clone();
}
// After
let display_name = user.nickname.clone().unwrap_or_else(|| user.full_name.clone());

// SIMPLIFY: Manual collection building
// Before
let mut active = Vec::new();
for user in &users {
    if user.is_active {
        active.push(user.clone());
    }
}
// After
let active: Vec<_> = users.iter().filter(|u| u.is_active).cloned().collect();

// SIMPLIFY: Redundant boolean return
// Before
fn is_valid(input: &str) -> bool {
    if !input.is_empty() && input.len() < 100 {
        return true;
    }
    false
}
// After
fn is_valid(input: &str) -> bool {
    !input.is_empty() && input.len() < 100
}
```

### Control-flow and error handling

```rust
// SIMPLIFY: Nested conditionals → early returns with `?`
// Before
fn process(data: Option<&Data>) -> Result<Output> {
    if let Some(data) = data {
        if data.is_valid() {
            if data.has_permission() {
                Ok(do_work(data))
            } else {
                Err(Error::Permission)
            }
        } else {
            Err(Error::Invalid)
        }
    } else {
        Err(Error::Missing)
    }
}
// After
fn process(data: Option<&Data>) -> Result<Output> {
    let data = data.ok_or(Error::Missing)?;
    if !data.is_valid() {
        return Err(Error::Invalid);
    }
    if !data.has_permission() {
        return Err(Error::Permission);
    }
    Ok(do_work(data))
}

// SIMPLIFY: match on Option/Result → combinator when it reads clearer
// Before
let label = match maybe_title {
    Some(title) => title,
    None => "untitled".to_string(),
};
// After
let label = maybe_title.unwrap_or_else(|| "untitled".to_string());
```

### TUI / state code — judgment calls (flag, don't auto-refactor)

```rust
// JUDGMENT: A field threaded through several render helpers might be better
// held on PaneState and read in compute_view(). But moving state around can
// quietly break the state/runtime split or the pure-render invariant.
// This is a judgment call — flag it for review, don't auto-refactor.
```

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "It's working, no need to touch it" | Working code that's hard to read will be hard to fix when it breaks. Simplifying now saves time on every future change. |
| "Fewer lines is always simpler" | A 1-line nested expression is not simpler than a 5-line `match`. Simplicity is about comprehension speed, not line count. |
| "I'll just quickly simplify this unrelated code too" | Unscoped simplification creates noisy diffs and risks regressions in code you didn't intend to change. Stay focused. |
| "The types make it self-documenting" | Types document structure, not intent. A well-named function explains *why* better than a type signature explains *what*. |
| "This abstraction might be useful later" | Don't preserve speculative abstractions. If it's not used now, it's complexity without value. Remove it and re-add when needed. |
| "The original author must have had a reason" | Maybe. Check git blame — apply Chesterton's Fence. But accumulated complexity often has no reason; it's just the residue of iteration under pressure. |
| "I'll refactor while adding this feature" | Separate refactoring from feature work. Mixed changes are harder to review, revert, and understand in history. |

## Red Flags

- Simplification that requires modifying tests to pass (you likely changed behavior)
- "Simplified" code that is longer and harder to follow than the original
- Renaming things to match your preferences rather than project conventions
- Removing error handling because "it makes the code cleaner" (or swapping `?` for `unwrap()`)
- Simplifying code you don't fully understand
- Batching many simplifications into one large, hard-to-review commit
- Refactoring code outside the scope of the current task without being asked
- Collapsing a branch in a way that mutates state during render or couples `AppState` to the runtime

## Verification

After completing a simplification pass:

- [ ] All existing tests pass without modification (`just test`)
- [ ] Build succeeds with no new warnings (`cargo build --release --locked`)
- [ ] Formatter/linter passes — no style regressions (`just lint`: `cargo fmt --check` + `cargo clippy --all-targets --locked -- -D warnings`)
- [ ] Each simplification is a reviewable, incremental change
- [ ] The diff is clean — no unrelated changes mixed in
- [ ] Simplified code follows project conventions (checked against CLAUDE.md)
- [ ] No error handling was removed or weakened (no new `unwrap()` in production code)
- [ ] No dead code was left behind (unused imports, unreachable branches)
- [ ] A teammate or review agent would approve the change as a net improvement
