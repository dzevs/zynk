---
name: source-driven-development
description: Grounds every implementation decision in official documentation. Use when you want authoritative, source-cited code free from outdated patterns. Use when building with any framework or library where correctness matters.
---

# Source-Driven Development

## Overview

Every crate-specific code decision must be backed by official documentation. Don't implement from memory — verify, cite, and let the user see your sources. Training data goes stale, APIs get deprecated, best practices evolve. This skill ensures the user gets code they can trust because every pattern traces back to an authoritative source they can check.

## When to Use

- The user wants code that follows current best practices for a given crate or library
- Building boilerplate, starter code, or patterns that will be copied across the codebase
- The user explicitly asks for documented, verified, or "correct" implementation
- Implementing features where the library's recommended approach matters (async tasks, terminal rendering, PTY handling, IPC, serialization)
- Reviewing or improving code that uses crate-specific patterns
- Any time you are about to write crate-specific code from memory

**When NOT to use:**

- Correctness does not depend on a specific version (renaming variables, fixing typos, moving files)
- Pure logic that works the same across all versions (loops, conditionals, data structures)
- The user explicitly wants speed over verification ("just do it quickly")

## The Process

```
DETECT ──→ FETCH ──→ IMPLEMENT ──→ CITE
  │          │           │            │
  ▼          ▼           ▼            ▼
 What       Get the    Follow the   Show your
 stack?     relevant   documented   sources
            docs       patterns
```

### Step 1: Detect Stack and Versions

Read the project's dependency file to identify exact versions:

```
Cargo.toml / Cargo.lock → crate versions (ratatui, tokio, portable-pty, interprocess, serde, …)
rust-toolchain(.toml)   → pinned Rust toolchain, if present
build.rs / vendor dir   → native/vendored build deps (e.g. libghostty-vt, built with Zig 0.15.2)
package.json (assets)   → Bun/TS test assets, where present
```

State what you found explicitly:

```
STACK DETECTED:
- ratatui 0.x (from Cargo.lock)
- tokio 1.x with the "full" feature
- portable-pty 0.x
→ Fetching official docs for the relevant patterns.
```

If versions are missing or ambiguous, **ask the user**. Don't guess — the version determines which patterns are correct. Prefer the exact pinned version in `Cargo.lock` over the range in `Cargo.toml`.

### Step 2: Fetch Official Documentation

Fetch the specific documentation page for the feature you're implementing. Not the homepage, not the full docs — the relevant page.

**Source hierarchy (in order of authority):**

| Priority | Source | Example |
|----------|--------|---------|
| 1 | Official crate docs / API reference | docs.rs/<crate>/<version>, ratatui.rs, tokio.rs |
| 2 | Official guide / changelog / RFC | The Rust Book/Reference, tokio.rs/tokio/tutorial, crate CHANGELOG.md |
| 3 | Platform / OS references | man pages, POSIX/terminfo, console_codes |
| 4 | Toolchain compatibility | doc.rust-lang.org release notes, crate MSRV notes |

**Not authoritative — never cite as primary sources:**

- Stack Overflow answers
- Blog posts or tutorials (even popular ones)
- AI-generated documentation or summaries
- Your own training data (that is the whole point — verify it)

**Be precise with what you fetch:**

```
BAD:  Fetch the tokio homepage
GOOD: Fetch docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html

BAD:  Search "ratatui layout best practices"
GOOD: Fetch ratatui.rs/concepts/layout/ (and the docs.rs Layout API page for the pinned version)
```

After fetching, extract the key patterns and note any deprecation warnings or migration guidance.

When official sources conflict with each other (e.g. a migration guide contradicts the API reference), surface the discrepancy to the user and verify which pattern actually works against the detected version (a quick `cargo build` / `just build` against the pinned version is the tiebreaker).

### Step 3: Implement Following Documented Patterns

Write code that matches what the documentation shows:

- Use the API signatures from the docs for the pinned version, not from memory
- If the docs show a new way to do something, use the new way
- If the docs deprecate a pattern, don't use the deprecated version
- If the docs don't cover something, flag it as unverified

**When docs conflict with existing project code:**

```
CONFLICT DETECTED:
The existing code blocks on a sync filesystem call inside an async handler,
but the tokio docs recommend tokio::task::spawn_blocking for blocking work.
(Source: docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)

Options:
A) Use the documented pattern (spawn_blocking) — consistent with current docs
B) Match existing code (inline blocking call) — consistent with the surrounding module
→ Which approach do you prefer?
```

Surface the conflict. Don't silently pick one.

### Step 4: Cite Your Sources

Every crate-specific pattern gets a citation. The user must be able to verify every decision.

**In code comments:**

```rust
// tokio: run blocking work off the async runtime threads
// Source: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
let result = tokio::task::spawn_blocking(move || read_pty_snapshot(&path)).await?;
```

**In conversation:**

```
I'm using spawn_blocking instead of calling the blocking read directly
inside the async handler. tokio's docs are explicit that blocking calls
on the async runtime threads can starve other tasks.

Source: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
"In general, issuing a blocking call or performing a lot of compute in a
future without yielding is problematic [...] use spawn_blocking."
```

**Citation rules:**

- Full URLs, not shortened
- Pin docs.rs links to the version where it matters (e.g. `docs.rs/ratatui/0.x.y/...`) — version-pinned links survive API churn better than `/latest/`
- Prefer deep links with anchors where possible — anchors survive doc restructuring better than top-level pages
- Quote the relevant passage when it supports a non-obvious decision
- Include platform/OS support data when recommending OS-specific behavior (which in zynk belongs in `src/platform/`)
- If you cannot find documentation for a pattern, say so explicitly:

```
UNVERIFIED: I could not find official documentation for this
pattern. This is based on training data and may be outdated.
Verify before using in production.
```

Honesty about what you couldn't verify is more valuable than false confidence.

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "I'm confident about this API" | Confidence is not evidence. Training data contains outdated patterns that look correct but break against current crate versions. Verify. |
| "Fetching docs wastes tokens" | Hallucinating an API wastes more. The user debugs for an hour, then discovers the function signature changed between crate versions. One fetch prevents hours of rework. |
| "The docs won't have what I need" | If the docs don't cover it, that's valuable information — the pattern may not be officially recommended. |
| "I'll just mention it might be outdated" | A disclaimer doesn't help. Either verify and cite, or clearly flag it as unverified. Hedging is the worst option. |
| "This is a simple task, no need to check" | Simple tasks with wrong patterns become templates. The user copies your deprecated handler into ten call sites before discovering the modern approach exists. |

## Red Flags

- Writing crate-specific code without checking the docs for that version
- Using "I believe" or "I think" about an API instead of citing the source
- Implementing a pattern without knowing which crate version it applies to
- Citing Stack Overflow or blog posts instead of official docs.rs / crate docs
- Using deprecated APIs because they appear in training data
- Not reading `Cargo.toml` / `Cargo.lock` before implementing
- Delivering code without source citations for crate-specific decisions
- Fetching an entire docs site when only one page is relevant

## Verification

After implementing with source-driven development:

- [ ] Crate and library versions were identified from `Cargo.toml` / `Cargo.lock`
- [ ] Official documentation was fetched for crate-specific patterns
- [ ] All sources are official docs (docs.rs / crate guides), not blog posts or training data
- [ ] Code follows the patterns shown in the pinned version's documentation
- [ ] Non-trivial decisions include source citations with full URLs
- [ ] No deprecated APIs are used (checked against the crate CHANGELOG / migration notes)
- [ ] Conflicts between docs and existing code were surfaced to the user
- [ ] Anything that could not be verified is explicitly flagged as unverified
