---
name: code-review-and-quality
description: Conducts multi-axis code review. Use before merging any change. Use when reviewing code written by yourself, another agent, or a human. Use when you need to assess code quality across multiple dimensions before it enters the main branch.
---

# Code Review and Quality

## Overview

Multi-dimensional code review with quality gates. Every change gets reviewed before merge — no exceptions. Review covers five axes: correctness, readability, architecture, security, and performance.

**The approval standard:** Approve a change when it definitely improves overall code health, even if it isn't perfect. Perfect code doesn't exist — the goal is continuous improvement. Don't block a change because it isn't exactly how you would have written it. If it improves the codebase and follows the project's conventions, approve it.

## When to Use

- Before merging any PR or change
- After completing a feature implementation
- When another agent or model produced code you need to evaluate
- When refactoring existing code
- After any bug fix (review both the fix and the regression test)

## The Five-Axis Review

Every review evaluates code across these dimensions:

### 1. Correctness

Does the code do what it claims to do?

- Does it match the spec or task requirements?
- Are edge cases handled (`None`, empty collections, boundary values)?
- Are error paths handled (`Result`/`Option` propagated, not just the happy path)?
- Does it pass all tests? Are the tests actually testing the right things?
- Are there off-by-one errors, race conditions in async/IPC, or state inconsistencies?

### 2. Readability & Simplicity

Can another engineer (or agent) understand this code without the author explaining it?

- Are names descriptive and consistent with project conventions? (No `tmp`, `data`, `result` without context)
- Is the control flow straightforward (avoid deep nesting, prefer guard clauses / `?`)?
- Is the code organized logically (related code grouped, clear module boundaries)?
- Are there any "clever" tricks that should be simplified?
- **Could this be done in fewer lines?** (1000 lines where 100 suffice is a failure)
- **Are abstractions earning their complexity?** (Don't generalize until the third use case)
- Would comments help clarify non-obvious intent? (But don't comment obvious code.)
- Are there dead code artifacts: no-op variables (`_unused`), backwards-compat shims, or `// removed` comments?

### 3. Architecture

Does the change fit the system's design?

- Does it follow existing patterns or introduce a new one? If new, is it justified?
- Does it maintain clean module boundaries? (`src/app/`, `server/`, `pty/`, `terminal/`, `detect/`, ...)
- Does it respect the core invariants: **state separated from runtime** (`AppState`/`PaneState` pure, no PTYs/async), **render is pure** (`render()` only draws, `compute_view()` does geometry/mutations — never mutate during render), **platform code isolated** in `src/platform/` (core modules avoid `#[cfg(target_os)]`)?
- Is detection **decoupled + evidence-based** (reads a screen snapshot only, never the parser/viewport; explicit AND/OR gates; no incidental whole-pane text)?
- Is there code duplication that should be shared?
- Are dependencies flowing in the right direction (no circular module dependencies)?
- Is the abstraction level appropriate (not over-engineered, not too coupled)?

### 4. Security

For detailed security guidance, see `security-and-hardening`. Does the change introduce vulnerabilities?

- Is external input validated and sanitized?
- Are secrets kept out of code, logs, and version control?
- Is authorization checked where needed (e.g. socket command surface)?
- Is `unsafe` avoided, or — where unavoidable — justified, minimal, and documented?
- Is data from external sources (a pane's terminal output, IPC payloads, config files, env) treated as **untrusted**?
- Are external data flows validated at system boundaries before use in logic or rendering?

### 5. Performance

For detailed profiling and optimization, see `performance-optimization`. Does the change introduce performance problems?

- Any work done per-frame in the render path that should be cached or precomputed?
- Any unbounded loops, unbounded scrollback growth, or unconstrained reads?
- Any blocking/synchronous operations on the async runtime that should be spawned/awaited?
- Any unnecessary re-renders or full redraws where a partial update suffices?
- Any per-byte or per-cell hot paths doing avoidable allocations or clones?
- Any large objects cloned in hot paths instead of borrowed?

## Change Sizing

Small, focused changes are easier to review, faster to merge, and safer to deploy. Target these sizes:

```
~100 lines changed   → Good. Reviewable in one sitting.
~300 lines changed   → Acceptable if it's a single logical change.
~1000 lines changed  → Too large. Split it.
```

**What counts as "one change":** A single self-contained modification that addresses one thing, includes related tests, and keeps the system functional after submission. One part of a feature — not the whole feature.

**Splitting strategies when a change is too large:**

| Strategy | How | When |
|----------|-----|------|
| **Stack** | Submit a small change, start the next one based on it | Sequential dependencies |
| **By module group** | Separate changes for groups needing different reviewers | Cross-cutting concerns |
| **Horizontal** | Create shared code/stubs first, then consumers | Layered architecture |
| **Vertical** | Break into smaller end-to-end slices of the feature | Feature work |

**When large changes are acceptable:** Complete file deletions and automated refactoring where the reviewer only needs to verify intent, not every line.

**Separate refactoring from feature work.** A change that refactors existing code and adds new behavior is two changes — submit them separately. Small cleanups (variable renaming) can be included at reviewer discretion.

## Change Descriptions

Every change needs a description that stands alone in version control history. Use lowercase conventional commits, no emojis, no AI co-author lines.

**First line:** Short, imperative, standalone. "delete the unused pane cache" not "deleting the unused pane cache." Must be informative enough that someone searching history can understand the change without reading the diff.

**Body:** What is changing and why. Include context, decisions, and reasoning not visible in the code itself. Link to issue numbers, benchmark results, or ADRs (`docs/zynk/decisions/`) where relevant. Acknowledge approach shortcomings when they exist.

**Anti-patterns:** "fix bug," "fix build," "add patch," "moving code from A to B," "phase 1," "add convenience functions."

## Review Process

### Step 1: Understand the Context

Before looking at code, understand the intent:

```
- What is this change trying to accomplish?
- What spec or task does it implement?
- What is the expected behavior change?
```

### Step 2: Review the Tests First

Tests reveal intent and coverage:

```
- Do tests exist for the change?
- Do they test behavior (not implementation details)?
- Are edge cases covered?
- Do tests have descriptive names?
- Would the tests catch a regression if the code changed?
```

### Step 3: Review the Implementation

Walk through the code with the five axes in mind:

```
For each file changed:
1. Correctness: Does this code do what the test says it should?
2. Readability: Can I understand this without help?
3. Architecture: Does this fit the system (state/runtime split, pure render, isolated platform code)?
4. Security: Any vulnerabilities? Is external/pane data treated as untrusted?
5. Performance: Any bottlenecks in the render or hot paths?
```

### Step 4: Categorize Findings

Label every comment with its severity so the author knows what's required vs optional:

| Prefix | Meaning | Author Action |
|--------|---------|---------------|
| *(no prefix)* | Required change | Must address before merge |
| **Critical:** | Blocks merge | Security vulnerability, data loss, broken functionality, panic in production path |
| **Nit:** | Minor, optional | Author may ignore — formatting, style preferences |
| **Optional:** / **Consider:** | Suggestion | Worth considering but not required |
| **FYI** | Informational only | No action needed — context for future reference |

This prevents authors from treating all feedback as mandatory and wasting time on optional suggestions.

### Step 5: Verify the Verification

Check the author's verification story:

```
- What tests were run? (just test / cargo nextest)
- Did lint and the build pass? (just lint, cargo build --release --locked)
- Was the change tested manually in a running instance?
- Are there before/after notes for TUI/render changes?
```

## Multi-Model Review Pattern

Use different models for different review perspectives:

```
Model A writes the code
    │
    ▼
Model B reviews for correctness and architecture
    │
    ▼
Model A addresses the feedback
    │
    ▼
Human makes the final call
```

This catches issues that a single model might miss — different models have different blind spots.

**Example prompt for a review agent:**
```
Review this code change for correctness, security, and adherence to
our project conventions. The spec says [X]. The change should [Y].
Flag any issues as Critical, Important, or Suggestion.
```

## Dead Code Hygiene

After any refactoring or implementation change, check for orphaned code:

1. Identify code that is now unreachable or unused
2. List it explicitly
3. **Ask before deleting:** "Should I remove these now-unused elements: [list]?"

Don't leave dead code lying around — it confuses future readers and agents. Clippy's `dead_code` lint helps, but be careful: compat constants used only in `#[cfg(test)]` can look "never used" to clippy while still being load-bearing for tests — verify before deleting. Don't silently delete things you're not sure about. When in doubt, ask.

```
DEAD CODE IDENTIFIED:
- format_legacy_date() in src/util/date.rs — replaced by format_date()
- OldPaneCard renderer in src/ui/ — replaced by PaneCard
- LEGACY_SOCKET_PATH const in src/config/mod.rs — no remaining references
→ Safe to remove these?
```

## Review Speed

Slow reviews block entire teams. The cost of context-switching to review is less than the waiting cost imposed on others.

- **Respond within one business day** — this is the maximum, not the target
- **Ideal cadence:** Respond shortly after a review request arrives, unless deep in focused coding. A typical change should complete multiple review rounds in a single day
- **Prioritize fast individual responses** over quick final approval. Quick feedback reduces frustration even if multiple rounds are needed
- **Large changes:** Ask the author to split them rather than reviewing one massive change

## Handling Disagreements

When resolving review disputes, apply this hierarchy:

1. **Technical facts and data** override opinions and preferences
2. **Style guides** are the absolute authority on style matters (here: `rustfmt` + `clippy -D warnings`)
3. **Software design** must be evaluated on engineering principles, not personal preference
4. **Codebase consistency** is acceptable if it doesn't degrade overall health

**Don't accept "I'll clean it up later."** Experience shows deferred cleanup rarely happens. Require cleanup before submission unless it's a genuine emergency. If surrounding issues can't be addressed in this change, require filing an issue with self-assignment.

## Honesty in Review

When reviewing code — whether written by you, another agent, or a human:

- **Don't rubber-stamp.** "LGTM" without evidence of review helps no one.
- **Don't soften real issues.** "This might be a minor concern" when it's a bug that will hit production is dishonest.
- **Quantify problems when possible.** "This redraws every cell on each keystroke, ~N allocations per frame" is better than "this could be slow."
- **Push back on approaches with clear problems.** Sycophancy is a failure mode in reviews. If the implementation has issues, say so directly and propose alternatives.
- **Accept override gracefully.** If the author has full context and disagrees, defer to their judgment. Comment on code, not people — reframe personal critiques to focus on the code itself.

## Dependency Discipline

Part of code review is dependency review:

**Before adding any dependency:**
1. Does the existing stack solve this? (Often it does — std, ratatui, tokio, portable-pty, interprocess are already here.)
2. How heavy is the dependency? (Compile time, binary size, transitive deps via `cargo tree`.)
3. Is it actively maintained? (Check last release, open issues.)
4. Does it have known vulnerabilities? (`cargo audit`)
5. What's the license? (Must be compatible with **AGPL-3.0-or-later** — run `cargo deny` and preserve the `NOTICE`/`LICENSE` attribution.)

**Rule:** Prefer the standard library and existing utilities over new dependencies. Every dependency is a liability.

## The Review Checklist

```markdown
## Review: [PR/Change title]

### Context
- [ ] I understand what this change does and why

### Correctness
- [ ] Change matches spec/task requirements
- [ ] Edge cases handled (None, empty, boundaries)
- [ ] Error paths handled (Result/Option, no unwrap() in production)
- [ ] Tests cover the change adequately

### Readability
- [ ] Names are clear and consistent
- [ ] Logic is straightforward
- [ ] No unnecessary complexity

### Architecture
- [ ] Follows existing patterns and module boundaries
- [ ] State/runtime split respected; render stays pure
- [ ] Platform code isolated; detection reads only the snapshot
- [ ] Appropriate abstraction level

### Security
- [ ] No secrets in code
- [ ] Input validated at boundaries
- [ ] No unjustified `unsafe`
- [ ] Pane/IPC/external data treated as untrusted

### Performance
- [ ] No avoidable per-frame work
- [ ] No unbounded operations
- [ ] No blocking calls on the async runtime

### Verification
- [ ] Tests pass (just test)
- [ ] Lint clean + build succeeds (just lint, cargo build --release --locked)
- [ ] Manual verification done (if applicable)

### Verdict
- [ ] **Approve** — Ready to merge
- [ ] **Request changes** — Issues must be addressed
```
## See Also

- For detailed security review guidance, see `references/security-checklist.md`
- For performance review checks, see `references/performance-checklist.md`

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "It works, that's good enough" | Working code that's unreadable, insecure, or architecturally wrong creates debt that compounds. |
| "I wrote it, so I know it's correct" | Authors are blind to their own assumptions. Every change benefits from another set of eyes. |
| "We'll clean it up later" | Later never comes. The review is the quality gate — use it. Require cleanup before merge, not after. |
| "AI-generated code is probably fine" | AI code needs more scrutiny, not less. It's confident and plausible, even when wrong. |
| "The tests pass, so it's good" | Tests are necessary but not sufficient. They don't catch architecture problems, security issues, or readability concerns. |

## Red Flags

- PRs merged without any review
- Review that only checks if tests pass (ignoring other axes)
- "LGTM" without evidence of actual review
- Security-sensitive changes without security-focused review
- Large PRs that are "too big to review properly" (split them)
- No regression tests with bug fix PRs
- Review comments without severity labels — makes it unclear what's required vs optional
- Accepting "I'll fix it later" — it never happens
- `unwrap()` / blanket `#[allow]` / new `unsafe` slipping through unreviewed

## Verification

After review is complete:

- [ ] All Critical issues are resolved
- [ ] All Important issues are resolved or explicitly deferred with justification
- [ ] Tests pass (`just test`)
- [ ] Lint clean and build succeeds (`just lint` + `cargo build --release --locked`)
- [ ] The verification story is documented (what changed, how it was verified)
