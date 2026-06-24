---
name: pr-explainer
description: Use when creating an approachable, self-contained HTML review aid for a pull request; explaining what changed, why it matters, how it works, and how it fits into the broader system; turning PR diffs, commits, tests, and architecture context into a local `.pr-review/` HTML page for reviewers; or helping reviewers (including the Codex/swarm second-opinion pass) understand complex code changes without dumping the full diff.
---

# PR Explainer

Create a local, self-contained HTML page that teaches a reviewer the PR story: what changed, why it
matters, how it works, how it fits into zynk's architecture, and how it was verified. The page is a
teaching aid for the GitHub PR review and for the decorrelated Codex/swarm second opinion — it never
replaces the final full-diff review.

## Required workflow

1. **Understand the PR before writing HTML**
   - Collect PR title/number, branch, link if available, base branch, commit range, changed files,
     and verification already performed (`just check`, `cargo nextest`, manual TUI reproduction).
   - Pull PR metadata with `gh pr view <number>` (or `gh pr view` on the current branch).
   - Inspect the current state with `git status --short`.
   - Inspect recent commits with `git log --oneline -n 10`.
   - Inspect scope with `git diff <base>...HEAD --stat` and `git diff <base>...HEAD`.
   - If one commit carries the main change, inspect it with `git show --stat <commit>` and
     `git show <commit>`.

2. **Find the explanation path**
   - Do not explain files in raw diff order.
   - Teach the change in this order when possible:
     1. problem,
     2. system context (where it sits in zynk's module map),
     3. before/after data or control flow,
     4. key code changes,
     5. proof from tests/builds/manual TUI checks,
     6. reviewer takeaway.
   - Classify changed files as core behavior, plumbing/integration (IPC/protocol/server),
     tests, release metadata, or incidental noise.
   - Highlight only files that help explain the PR.

3. **Write for approachability**
   - Use plain language, short sections, concrete before/after examples, small focused snippets,
     diagrams, tables, and callouts.
   - Explain the problem before implementation details.
   - Define acronyms or zynk-internal terms before using them (e.g. `PaneRuntime`, `AppState`,
     `compute_view()`, the socket command layer, trace id, header-v2).
   - Avoid dumping the full diff or assuming the reviewer already knows internal context.

4. **Create a local self-contained HTML file**
   - Put generated files in `.pr-review/`.
   - Use one HTML file containing all CSS and content.
   - Do not commit `.pr-review/` by default.
   - Prefer repo ignore rules or `.git/info/exclude` so generated review pages stay out of commits
     (and out of the private-content gate). `.pr-review/` should never reach a commit.

## Recommended HTML structure

Use this structure unless the PR clearly needs a different teaching order:

1. **Hero**
   - PR number/title, one-sentence summary, branch/link/status.
   - Small metrics: files changed, tests added, modules affected.

2. **Problem**
   - Previous behavior.
   - Why it was wrong, confusing, missing, or risky.

3. **System Context**
   - Where the change sits in zynk's architecture (`app/`, `server/` + `ipc.rs` + `api/` +
     `protocol/`, `pty/` + `terminal/`, `pane/`, `input/`, `detect/`, `persist/`, `remote/`,
     `client/`, the `zynk_*` conversation modules).
   - Upstream callers (CLI over the unix socket), downstream behavior, and why this is the right layer.
   - Behavior intentionally not changed. Note any state-vs-runtime or pure-render invariants the
     change must respect.

4. **Before/After Flow**
   - Visual old path vs. new path when the PR changes flow, state, ownership, IPC request handling,
     PTY lifecycle, screen/parser state, detection gates, persistence, or pane relationships.

5. **Code Walkthrough**
   - Step-by-step explanation path.
   - Focused diffs for important files only.
   - Explain what each snippet accomplishes and why it is necessary.

6. **Tests / Verification**
   - Tests added or updated.
   - Commands run: `just check` / `just ci`, `cargo nextest run --locked <filter>`, `just lint`,
     `just build`, or manual TUI reproduction steps.
   - Known unrelated warnings or pre-existing flakes, if any.

7. **Reviewer Takeaway**
   - The shortest useful mental model of the PR.
   - What the reviewer (and the Codex/swarm second opinion) should focus on while reviewing the
     actual diff — e.g. the one IPC boundary or the one detection gate that carries the risk.

## Diagrams

Add diagrams when they reduce cognitive load. Prefer simple HTML/CSS diagrams over external
dependencies.

Good diagram types:

- IPC flow: CLI → unix socket → server command → handler → `AppState` mutation → JSON response
- Before/after path: broken path vs. fixed path
- Ownership map: module responsibility boundaries (state vs. runtime)
- Data transformation: PTY bytes → terminal/screen state → snapshot → detection result
- State machine: pane lifecycle (spawning → running → exited), or agent status transitions

Each diagram must answer: "What does this help the reviewer understand faster?"

## Focused diff snippets

Show snippets along the explanation path, not giant patches. Each important snippet should include:

- file path,
- relevant added/removed lines only,
- visual styling for additions/removals,
- a short explanation,
- connection back to the PR story.

Use this pattern:

```html
<div class="diff">
  <div class="diff-title">src/detect/mod.rs</div>
  <pre>
<span class="del">- // matched incidental whole-pane text</span>
<span class="add">+ // explicit AND/OR gate on invariant vs alternative controls</span>
  </pre>
</div>
```

A reviewer should understand the PR without opening GitHub, but the page should not replace the final
full diff review.

## Verification requirements

End with proof. Include exact commands when available, for example:

```text
cargo nextest run --locked detect::agent_status
just check
just build
```

If verification was not run, say so clearly and list the recommended commands (at minimum
`just check`).

## Final checklist

Before calling the page done, confirm it has:

- clear one-sentence summary,
- problem statement,
- before/after explanation,
- broader system context (zynk module map),
- visual diagram where useful,
- step-by-step code walkthrough,
- focused diffs with file paths,
- tests and verification commands,
- reviewer takeaway,
- self-contained HTML/CSS,
- stored in `.pr-review/`,
- not staged or committed unless explicitly requested.

## Default output

When asked to create a PR explainer, produce or update a `.pr-review/*.html` file and summarize:

1. output path,
2. PR story covered,
3. key sections included,
4. verification evidence included,
5. whether `.pr-review/` remains untracked or excluded.
