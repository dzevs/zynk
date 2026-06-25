---
name: pr-explainer
description: Use when creating an approachable, self-contained HTML review aid for a pull request; explaining what changed, why it matters, how it works, and how it fits into the broader system; turning PR diffs, commits, tests, and architecture context into a local `.pr-review/` HTML page for reviewers; or helping reviewers (including the Codex/swarm second-opinion pass) understand complex code changes without dumping the full diff.
---

# PR Explainer

Build a standalone HTML page, kept on disk, that walks a reviewer through the PR's narrative: the
change itself, the motivation, the mechanism, where it lands in zynk's architecture, and the evidence
that it works. Treat the page as a primer for the GitHub review and for the decorrelated Codex/swarm
second opinion — it supplements the final full-diff pass, never stands in for it.

## Required workflow

1. **Get the PR clear in your head before generating any HTML**
   - Gather the title and number, the branch, a link if there is one, the base branch, the commit
     range, the touched files, and whatever was already validated (`just check`, `cargo nextest`,
     hands-on TUI reproduction).
   - Read the PR metadata via `gh pr view <number>` (or `gh pr view` while on the branch).
   - Check the working tree with `git status --short`.
   - Look over the latest commits with `git log --oneline -n 10`.
   - Size up the change with `git diff <base>...HEAD --stat` and `git diff <base>...HEAD`.
   - When a single commit holds the bulk of the work, drill into it with `git show --stat <commit>`
     and `git show <commit>`.

2. **Choose the order that teaches it best**
   - Resist walking the files in the order the diff presents them.
   - Where it fits, lead the reviewer through these beats:
     1. the problem,
     2. the system context (its spot in zynk's module map),
     3. the before/after data or control flow,
     4. the substantive code changes,
     5. the proof from tests, builds, or manual TUI checks,
     6. the takeaway for the reviewer.
   - Sort the changed files into buckets: core behavior, plumbing/integration (IPC/protocol/server),
     tests, release metadata, or incidental noise.
   - Surface only the files that actually advance the explanation.

3. **Aim for accessibility**
   - Lean on plain language, compact sections, concrete before/after examples, tight focused
     snippets, diagrams, tables, and callouts.
   - Lay out the problem first; save implementation detail for after.
   - Spell out acronyms and zynk-internal terms the first time they appear (for example
     `PaneRuntime`, `AppState`, `compute_view()`, the socket command layer, trace id, header-v2).
   - Resist pasting the whole diff or presuming the reviewer already carries the internal context.

4. **Emit one self-contained HTML file locally**
   - Drop the generated output into `.pr-review/`.
   - Keep everything — CSS and content alike — inside a single HTML file.
   - Leave `.pr-review/` out of commits by default.
   - Lean on repo ignore rules or `.git/info/exclude` so these generated pages never land in a commit
     (and never trip the private-content gate). Nothing under `.pr-review/` should ever be committed.

## Recommended HTML structure

Default to this layout, swapping in a different teaching order only when the PR plainly calls for one:

1. **Hero**
   - The PR number and title, a single-sentence summary, branch/link/status.
   - A few quick metrics: files changed, tests added, modules affected.

2. **Problem**
   - How things behaved before.
   - What made that wrong, confusing, absent, or hazardous.

3. **System Context**
   - The change's home in zynk's architecture (`app/`, `server/` + `ipc.rs` + `api/` + `protocol/`,
     `pty/` + `terminal/`, `pane/`, `input/`, `detect/`, `persist/`, `remote/`, `client/`, the
     `zynk_*` conversation modules).
   - Who calls in from above (the CLI over the unix socket), what happens downstream, and why this
     layer is the correct one.
   - What was deliberately left untouched. Call out any state-vs-runtime or pure-render invariant the
     change has to honor.

4. **Before/After Flow**
   - A side-by-side of the old route versus the new one whenever the PR shifts flow, state,
     ownership, IPC request handling, PTY lifecycle, screen/parser state, detection gates,
     persistence, or pane relationships.

5. **Code Walkthrough**
   - The explanation path, taken step by step.
   - Targeted diffs limited to the files that matter.
   - For each snippet, say what it does and why the change had to happen.

6. **Tests / Verification**
   - Which tests were added or changed.
   - The commands that ran: `just check` / `just ci`, `cargo nextest run --locked <filter>`,
     `just lint`, `just build`, or the manual TUI reproduction steps.
   - Any unrelated warnings or known pre-existing flakes worth noting.

7. **Reviewer Takeaway**
   - The tightest mental model that still captures the PR.
   - Where the reviewer (and the Codex/swarm second opinion) should aim their attention during the
     real diff pass — for instance the single IPC boundary or the single detection gate holding the
     risk.

## Diagrams

Reach for a diagram whenever it lowers the reviewer's mental effort. Favor plain HTML/CSS diagrams
over pulling in outside dependencies.

Diagram types that tend to work:

- IPC flow: CLI → unix socket → server command → handler → `AppState` mutation → JSON response
- Before/after path: the broken route against the fixed route
- Ownership map: which module owns which responsibility (state vs. runtime)
- Data transformation: PTY bytes → terminal/screen state → snapshot → detection result
- State machine: the pane lifecycle (spawning → running → exited), or agent status transitions

Every diagram has to earn its place by answering: "What does this let the reviewer grasp sooner?"

## Focused diff snippets

Trace snippets along the explanation path rather than dumping wholesale patches. Each snippet that
earns inclusion should carry:

- the file path,
- only the added/removed lines that matter,
- styling that distinguishes additions from removals,
- a brief explanation,
- a link back to the PR's narrative.

Follow this shape:

```html
<div class="diff">
  <div class="diff-title">src/detect/mod.rs</div>
  <pre>
<span class="del">- // matched incidental whole-pane text</span>
<span class="add">+ // explicit AND/OR gate on invariant vs alternative controls</span>
  </pre>
</div>
```

The reviewer should be able to follow the PR without opening GitHub, yet the page is no substitute for
the final full-diff review.

## Verification requirements

Close on evidence. Quote the exact commands whenever you have them, for example:

```text
cargo nextest run --locked detect::agent_status
just check
just build
```

When nothing was verified, say so plainly and spell out the commands worth running (`just check` at a
minimum).

## Final checklist

Before declaring the page finished, make sure it carries:

- a crisp one-sentence summary,
- a statement of the problem,
- a before/after explanation,
- the wider system context (the zynk module map),
- a visual diagram where it helps,
- a step-by-step code walkthrough,
- focused diffs that name their file paths,
- the tests and the verification commands,
- the reviewer takeaway,
- self-contained HTML/CSS,
- placement under `.pr-review/`,
- no staging or committing unless that was explicitly asked for.

## Default output

On a request to produce a PR explainer, write or refresh a `.pr-review/*.html` file and report back:

1. the output path,
2. the PR narrative it covers,
3. the key sections it contains,
4. the verification evidence it embeds,
5. whether `.pr-review/` stays untracked or excluded.
