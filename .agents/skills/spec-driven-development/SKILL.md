---
name: spec-driven-development
description: Creates specs before coding. Use when starting a new project, feature, or significant change and no specification exists yet. Use when requirements are unclear, ambiguous, or only exist as a vague idea.
---

# Spec-Driven Development

## Overview

Write a structured specification before writing any code. The spec is the shared source of truth between you and the human engineer — it defines what we're building, why, and how we'll know it's done. Code without a spec is guessing.

## When to Use

- Starting a new project or feature
- Requirements are ambiguous or incomplete
- The change touches multiple files or modules
- You're about to make an architectural decision
- The task would take more than 30 minutes to implement

**When NOT to use:** Single-line fixes, typo corrections, or changes where requirements are unambiguous and self-contained.

## The Gated Workflow

Spec-driven development has four phases. Do not advance to the next phase until the current one is validated.

```
SPECIFY ──→ PLAN ──→ TASKS ──→ IMPLEMENT
   │          │        │          │
   ▼          ▼        ▼          ▼
 Human      Human    Human      Human
 reviews    reviews  reviews    reviews
```

### Phase 1: Specify

Start with a high-level vision. Ask the human clarifying questions until requirements are concrete.

**Surface assumptions immediately.** Before writing any spec content, list what you're assuming:

```
ASSUMPTIONS I'M MAKING:
1. This is a terminal TUI feature (rendered via ratatui), not a headless server-only change
2. State lives in AppState/PaneState (pure data), not in PaneRuntime (live terminal)
3. The CLI drives this over the existing Unix-socket IPC layer (server/api/protocol)
4. We target the stable Rust toolchain and the existing tokio runtime
→ Correct me now or I'll proceed with these.
```

Don't silently fill in ambiguous requirements. The spec's entire purpose is to surface misunderstandings *before* code gets written — assumptions are the most dangerous form of misunderstanding.

**Write a spec document covering these six core areas:**

1. **Objective** — What are we building and why? Who is the user? What does success look like?

2. **Commands** — Full executable commands with flags, not just task names.
   ```
   Build: just build              # cargo build --release --locked
   Test:  just test               # cargo nextest run --locked …
   One:   just test-one <filter>  # cargo nextest run --locked <filter>
   Lint:  just lint               # cargo fmt --check + cargo clippy --all-targets --locked -- -D warnings
   CI:    just ci                 # full PR check
   ```

3. **Project Structure** — Where source code lives, where tests go, where docs belong.
   ```
   src/                → Crate source code
   src/app/            → AppState/PaneState (pure, testable data) + actions/input
   src/server/, api/, protocol/, ipc.rs → the socket command layer the CLI drives
   src/pty/, terminal/ → PTY + emulator/screen state
   src/detect/         → evidence-based agent detection (reads a screen snapshot only)
   src/platform/       → OS-specific code (kept out of core modules)
   tests/              → integration tests (each spawns its own temp config/socket)
   docs/zynk/decisions/ → ADRs (binding once accepted)
   ```

4. **Code Style** — One real code snippet showing your style beats three paragraphs describing it. Include naming conventions, formatting rules, and examples of good output. (For zynk: no `unwrap()` in production code, `tracing` for logging, OS-specific behavior isolated in `src/platform/`.)

5. **Testing Strategy** — What framework, where tests live, coverage expectations, which test levels for which concerns. (For zynk: `cargo nextest`; unit tests live next to the code in `#[cfg(test)]` modules, integration tests in `tests/`; tests are hermetic — each spawns its own temp config/socket.)

6. **Boundaries** — Three-tier system:
   - **Always do:** Run `just check` before commits, follow naming conventions, validate inputs, keep render pure
   - **Ask first:** DB/migration changes, adding dependencies, changing CI config, anything touching `src/platform/`
   - **Never do:** Commit secrets, use `unwrap()` in production code, drop the upstream `NOTICE`/`LICENSE` attribution, remove failing tests without approval

**Spec template:**

```markdown
# Spec: [Project/Feature Name]

## Objective
[What we're building and why. User stories or acceptance criteria.]

## Tech Stack
[Crates, Rust edition, key dependencies with versions — e.g. ratatui, tokio, portable-pty]

## Commands
[Build, test, lint, ci — full commands]

## Project Structure
[Directory layout with descriptions]

## Code Style
[Example snippet + key conventions]

## Testing Strategy
[Framework, test locations, coverage requirements, test levels]

## Boundaries
- Always: [...]
- Ask first: [...]
- Never: [...]

## Success Criteria
[How we'll know this is done — specific, testable conditions]

## Open Questions
[Anything unresolved that needs human input]
```

**Reframe instructions as success criteria.** When receiving vague requirements, translate them into concrete conditions:

```
REQUIREMENT: "Make the pane switcher faster"

REFRAMED SUCCESS CRITERIA:
- compute_view() for a 40-pane workspace completes in < 1ms
- No visible frame stutter when switching panes (render stays under one frame budget)
- A nextest benchmark/assertion guards the timing
→ Are these the right targets?
```

This lets you loop, retry, and problem-solve toward a clear goal rather than guessing what "faster" means.

### Phase 2: Plan

With the validated spec, generate a technical implementation plan:

1. Identify the major components and their dependencies
2. Determine the implementation order (what must be built first)
3. Note risks and mitigation strategies
4. Identify what can be built in parallel vs. what must be sequential
5. Define verification checkpoints between phases

The plan should be reviewable: the human should be able to read it and say "yes, that's the right approach" or "no, change X."

### Phase 3: Tasks

Break the plan into discrete, implementable tasks:

- Each task should be completable in a single focused session
- Each task has explicit acceptance criteria
- Each task includes a verification step (test, build, manual check)
- Tasks are ordered by dependency, not by perceived importance
- No task should require changing more than ~5 files

**Task template:**
```markdown
- [ ] Task: [Description]
  - Acceptance: [What must be true when done]
  - Verify: [How to confirm — test command, build, manual check]
  - Files: [Which files will be touched]
```

### Phase 4: Implement

Execute tasks one at a time following `skills/incremental-implementation/SKILL.md` (`incremental-implementation`) and `skills/test-driven-development/SKILL.md` (`test-driven-development`). Load the right spec sections and source files at each step rather than flooding the agent with the entire spec.

## Keeping the Spec Alive

The spec is a living document, not a one-time artifact:

- **Update when decisions change** — If you discover a data model (e.g. `PaneState`) needs to change, update the spec first, then implement.
- **Update when scope changes** — Features added or cut should be reflected in the spec.
- **Commit the spec** — The spec belongs in version control alongside the code.
- **Reference the spec in PRs** — Link back to the spec section that each PR implements.

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "This is simple, I don't need a spec" | Simple tasks don't need *long* specs, but they still need acceptance criteria. A two-line spec is fine. |
| "I'll write the spec after I code it" | That's documentation, not specification. The spec's value is in forcing clarity *before* code. |
| "The spec will slow us down" | A 15-minute spec prevents hours of rework. Waterfall in 15 minutes beats debugging in 15 hours. |
| "Requirements will change anyway" | That's why the spec is a living document. An outdated spec is still better than no spec. |
| "The user knows what they want" | Even clear requests have implicit assumptions. The spec surfaces those assumptions. |

## Red Flags

- Starting to write code without any written requirements
- Asking "should I just start building?" before clarifying what "done" means
- Implementing features not mentioned in any spec or task list
- Making architectural decisions without documenting them (in zynk, binding decisions become ADRs under `docs/zynk/decisions/`)
- Skipping the spec because "it's obvious what to build"

## Verification

Before proceeding to implementation, confirm:

- [ ] The spec covers all six core areas
- [ ] The human has reviewed and approved the spec
- [ ] Success criteria are specific and testable
- [ ] Boundaries (Always/Ask First/Never) are defined
- [ ] The spec is saved to a file in the repository
