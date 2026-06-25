---
name: documentation-and-adrs
description: Records decisions and documentation. Use when making architectural decisions, changing public APIs, shipping features, or when you need to record context that future engineers and agents will need to understand the codebase.
---

# Documentation and ADRs

## Overview

Document decisions, not just code. The most valuable documentation captures the *why* — the context, constraints, and trade-offs that led to a decision. Code shows *what* was built; documentation explains *why it was built this way* and *what alternatives were considered*. This context is essential for future humans and agents working in the codebase.

## When to Use

- Making a significant architectural decision
- Choosing between competing approaches
- Adding or changing a public API (socket protocol, CLI surface, library interface)
- Shipping a feature that changes user-facing behavior
- Onboarding new team members (or agents) to the project
- When you find yourself explaining the same thing repeatedly

**When NOT to use:** Don't document obvious code. Don't add comments that restate what the code already says. Don't write docs for throwaway prototypes.

## Architecture Decision Records (ADRs)

ADRs capture the reasoning behind significant technical decisions. They're the highest-value documentation you can write.

### When to Write an ADR

- Choosing a crate, library, or major dependency
- Designing a data model, DB schema, or migration strategy
- Selecting a wire/message protocol or delivery-receipt model
- Deciding on an API surface (socket commands, CLI shape, env-var contract)
- Choosing between build tools, runtimes, or platform abstractions
- Any decision that would be expensive to reverse

### ADR Template

Store ADRs in `docs/zynk/decisions/` with sequential numbering (`NNNN-kebab-title.md`), matching the existing house style:

```markdown
# ADR 0011 — Use sqlite-vec for embedding storage

**Status:** Proposed | Accepted | Superseded by ADR-NNNN | Deprecated
**Date:** 2026-06-24
**Spec:** `docs/zynk/SPEC.md` (relevant section)

## Context

We need persistent storage for conversation embeddings to support retrieval. Key requirements:
- Embedded, zero external service (zynk ships as a single binary)
- ACID writes for message persistence
- Vector similarity search over conversation history
- Runs inside the existing SQLite database we already use for the conversation layer

## Decision

Use the `sqlite-vec` extension loaded into the existing SQLite database.

## Alternatives considered

- **Standalone vector DB (e.g. a separate service)** — Pros: purpose-built ANN indexes.
  Cons: breaks the single-binary, zero-ops model; adds a network dependency.
  Rejected: disproportionate operational cost for a terminal-native tool.
- **In-memory brute-force search** — Pros: no extra dependency, trivial to implement.
  Cons: does not persist; rebuild cost grows with history.
  Rejected: history must survive detach/restart.
- **A second embedded engine alongside SQLite** — Pros: specialized.
  Cons: two storage engines, two migration paths, doubled failure surface.
  Rejected: one engine keeps the cutover and fail-closed behavior simple.

## Consequences

- One database, one migration path — keeps fail-closed DB behavior simple.
- Extension loading must be gated and version-pinned (build + runtime).
- Team needs SQLite + extension knowledge (standard, low risk).
- Embedding runtime choice is captured in a companion ADR.
```

### ADR Lifecycle

```
PROPOSED → ACCEPTED → (SUPERSEDED or DEPRECATED)
```

- **Don't delete old ADRs.** They capture historical context. Accepted ADRs are binding — amend via a new ADR, never rewrite.
- When a decision changes, write a new ADR that references and supersedes the old one.

## Inline Documentation

### When to Comment

Comment the *why*, not the *what*:

```rust
// BAD: Restates the code
// Increment counter by 1
counter += 1;

// GOOD: Explains non-obvious intent
// Rate limit uses a sliding window — reset counter at the window boundary,
// not on a fixed schedule, to prevent burst attacks at window edges.
if now - window_start > WINDOW_SIZE_MS {
    counter = 0;
    window_start = now;
}
```

### When NOT to Comment

```rust
// Don't comment self-explanatory code
fn calculate_total(items: &[CartItem]) -> u64 {
    items.iter().map(|item| item.price * item.quantity).sum()
}

// Don't leave TODO comments for things you should just do now
// TODO: add error handling  ← Just add it

// Don't leave commented-out code
// let old_implementation = || { ... };  ← Delete it, git has history
```

### Document Known Gotchas

```rust
/// IMPORTANT: Detection reads a screen *snapshot* only — never the live
/// parser or the scrollable user viewport. Matching against the viewport
/// produces false positives because user-typed text can mimic agent status.
///
/// See ADR 0009 for the full design rationale.
pub fn detect_agent_state(snapshot: &ScreenSnapshot) -> AgentState {
    // ...
}
```

## API Documentation

For public APIs (socket protocol, CLI commands, library interfaces):

### Inline with Rustdoc (preferred for Rust)

```rust
/// Sends a message to a target pane over the native bus.
///
/// # Arguments
/// * `target` - The pane identifier to deliver to.
/// * `body` - Pure-text message body; correlation goes in `trace`.
///
/// # Returns
/// A `DeliveryReceipt` with the honest delivery state.
///
/// # Errors
/// Returns `ProtocolError::PaneNotFound` if the target pane no longer exists.
/// Returns `ProtocolError::Busy` if the receiver is not accepting input.
///
/// # Examples
/// ```
/// let receipt = send_message(target, "ready for review")?;
/// assert!(receipt.delivered);
/// ```
pub fn send_message(target: PaneId, body: &str) -> Result<DeliveryReceipt, ProtocolError> {
    // ...
}
```

### Documenting the socket protocol / CLI surface

For the socket command layer the CLI drives, document each command's request/response shape (most commands return JSON) in the protocol docs, with a worked example:

```text
Command: message.send
Request:  { "target": "<pane-id>", "body": "<text>", "trace": "<id>" }
Response: { "delivered": true, "receipt_id": "<id>" }      # 200-equivalent
          { "error": "pane_not_found", "target": "<id>" }  # delivery error
```

Keep `zynk <group> <leaf> --help` accurate — it is the canonical CLI reference.

## README Structure

Every project should have a README that covers:

```markdown
# Project Name

One-paragraph description of what this project does.

## Quick Start
1. Clone the repo
2. Install prerequisites: Rust (stable) and Zig 0.15.2 (Bun for the TS asset test)
3. Build: `just build`
4. Run: `cargo run --release --locked -- --help`

## Commands
| Command | Description |
|---------|-------------|
| `just build` | Production release build |
| `just test` | Run tests (cargo nextest + script tests) |
| `just lint` | `cargo fmt --check` + clippy with `-D warnings` |
| `just ci` / `just check` | Full check |

## Architecture
Brief overview of the project structure and key design decisions.
Link to ADRs in `docs/zynk/decisions/` for details.

## Contributing
How to contribute, coding standards, PR process.
```

## Changelog Maintenance

For shipped features:

```markdown
# Changelog

## [3.0.0] - 2026-06-24
### Added
- Native message bus: agents address each other by pane (#123)
- Searchable, persisted conversation history (#124)

### Fixed
- Duplicate panes appearing on rapid agent start (#125)

### Changed
- `agent read` now defaults to the recent source for clearer snapshots (#126)
```

## Documentation for Agents

Special consideration for AI agent context:

- **CLAUDE.md / AGENTS.md / rules files** — Document project conventions so agents follow them
- **Spec files** — Keep `docs/zynk/SPEC.md` updated so agents build the right thing
- **ADRs** — Help agents understand why past decisions were made (prevents re-deciding); accepted ADRs are binding
- **Inline gotchas** — Prevent agents from falling into known traps (e.g. detection invariants)

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "The code is self-documenting" | Code shows what. It doesn't show why, what alternatives were rejected, or what constraints apply. |
| "We'll write docs when the API stabilizes" | APIs stabilize faster when you document them. The doc is the first test of the design. |
| "Nobody reads docs" | Agents do. Future engineers do. Your 3-months-later self does. |
| "ADRs are overhead" | A 10-minute ADR prevents a 2-hour debate about the same decision six months later. |
| "Comments get outdated" | Comments on *why* are stable. Comments on *what* get outdated — that's why you only write the former. |

## Red Flags

- Architectural decisions with no written rationale
- Public APIs (socket commands, CLI surface) with no documentation or types
- README that doesn't explain how to build/run the project
- Commented-out code instead of deletion
- TODO comments that have been there for weeks
- No ADRs in a project with significant architectural choices
- Documentation that restates the code instead of explaining intent

## Verification

After documenting:

- [ ] ADRs exist for all significant architectural decisions (in `docs/zynk/decisions/`)
- [ ] README covers quick start, commands, and architecture overview
- [ ] Public functions have Rustdoc with parameters, returns, and errors
- [ ] Known gotchas are documented inline where they matter
- [ ] No commented-out code remains
- [ ] Rules files (CLAUDE.md / AGENTS.md) are current and accurate
