---
name: pr-splitter
description: Use when breaking a large, complex, messy, or hard-to-review pull request into multiple smaller PRs; planning stacked PRs; extracting independent changes from a branch; splitting mixed refactor and behavior changes; managing drift after review feedback (operator gate, Codex/swarm second opinion); rebasing follow-up PRs as earlier PRs change; or preserving original branch intent while shipping incrementally.
---

# PR Splitter

Preserve the original PR as source material, build smaller reviewable PRs intentionally, and track
drift locally as review feedback changes the stack. In zynk, "review feedback" includes the operator
gate and the decorrelated Codex/swarm second opinion — treat their approved direction as the new
source of truth for the stack.

## Required workflow

1. **Snapshot before touching history**
   - Check `git status`.
   - Create an immutable local reference to the original branch: `git branch backup/original-large-pr`.
   - Do not delete or rewrite the original branch until the split is complete.

2. **Inventory the original PR**
   - Inspect `git diff --stat <base>...HEAD`, `git diff --name-only <base>...HEAD`, and
     `git log --oneline <base>..HEAD`.
   - Classify changes by review unit: prep/refactor, API/protocol/type changes, behavior, tests,
     docs, cleanup, generated/lock files (`Cargo.lock`, generated snapshots).

3. **Create a local scratchpad**
   - Write split notes to an uncommitted local file, preferably `.notes/pr-split.md`.
   - Ensure `.notes/` is ignored or leave it untracked. Do not commit scratchpad notes unless the
     user explicitly asks. (`.notes/` must never reach a commit — the private-content gate will
     flag stray local artifacts.)
   - Track: original branch, base branch, planned PRs, files/hunks extracted, verification per PR,
     remaining original diff, and intentional drift from review feedback.

4. **Choose the split shape**
   - Use stacked PRs when later work depends on earlier work.
   - Use parallel PRs only when changes are truly independent.
   - Use foundation + parallel follow-ups when one shared prep change unlocks independent work
     (e.g. a protocol/IPC type change that several handlers then build on).

5. **Extract changes safely**
   - Prefer fresh branches from the correct base plus selective restore over rewriting messy history.
   - Use path-level extraction for clean file ownership:
     `git checkout backup/original-large-pr -- src/path/to/file.rs`.
   - Use hunk-level extraction for mixed files:
     `git restore -p --source backup/original-large-pr -- src/path/to/file.rs`.
   - Keep each PR independently buildable and reviewable. A Rust PR that does not compile on its own
     is not a valid split unit — the crate must build.

6. **Verify each PR independently**
   - Run the narrowest relevant build, format, lint, and tests for that PR's scope:
     `cargo build --release --locked`, `cargo nextest run --locked <filter>`, `just lint`, and
     `just check` before the stack is considered green.
   - Do not leave tests, docs, or generated files separated from the code they validate unless the
     split plan explicitly calls for it.

7. **Manage drift deliberately**
   - Treat reviewer-approved changes (operator gate + Codex/swarm verdict) as the new source of
     truth for the stack.
   - After changing an earlier PR, rebase dependent PRs onto it and resolve conflicts in favor of
     the reviewed direction, not blindly in favor of the original branch.
   - Compare the evolving stack against `backup/original-large-pr` to find remaining intent, not to
     force byte-for-byte equality.
   - Record intentional differences in `.notes/pr-split.md`.

8. **Use range-diff for rewritten stacks**
   - Use `git range-diff` after rebases, conflict resolution, or force-pushes to understand what
     changed. (Force-push remains operator-gated — never force-push without an explicit gate.)
   - Summarize meaningful range-diff results for reviewers when updating a stacked PR.

## PR description pattern

Keep PR descriptions concise and reviewer-facing:

```markdown
## Summary

This is PR N of M split from a larger change.

## Scope

- ...

## Intentionally excluded

- Follow-up PR will handle ...

## Verification

- just check clean
- cargo nextest run --locked <filter>
```

Do not put the full split ledger in PR descriptions. Keep detailed extraction notes and drift
tracking in `.notes/pr-split.md`.

## Scratchpad template

```markdown
# PR split scratchpad

Original branch: backup/original-large-pr
Base branch: main

## Planned PRs

1. branch-name
   - Scope:
   - Files/hunks extracted:
   - Verification: (just check / cargo nextest filter / manual TUI repro)
   - Status:

## Remaining original intent

- ...

## Drift notes

- Date / branch / reason (e.g. Codex verdict changed the protocol shape):
```

## Per-PR scope, not a combined release note

Each split PR must describe and verify only the changes in that PR — never carry the original
branch's combined release/changelog wording into a single split PR. The original branch covers the
full combined change and its release note does not belong in any one slice.

After extracting changes into a split branch:

1. **Strip any combined-change release note or version bump carried over from the original branch.**
   These were written for the full diff and produce misleading history on a slice.
2. **Write a per-PR summary scoped to that PR's changes only.** Describe what this specific PR does,
   not the full original feature.
3. **Reference only the modules actually changed in this PR.** If the original change touched the
   server, protocol, and detection layers but this PR only touches `src/detect/`, scope the summary
   to detection.
4. **Match the change's apparent significance to the slice.** A prep/refactor slice is a small,
   low-risk PR; a slice that introduces new protocol/IPC surface is the one to flag for the
   Codex/swarm second opinion.
5. **Version bumps, tags, and releases stay out of split PRs.** Those are separate, explicitly
   operator-gated actions — never fold one into a routine split slice.

Add a "summary + verification scoped to this slice" line to the scratchpad template under each
planned PR so it is not forgotten.

## Common failure modes

Avoid splitting by file when behavior spans files, extracting tests without the code they validate,
leaving follow-up PRs that do not compile, force-pushing without a reviewer summary (and without the
operator gate), deleting the original branch early, reverting reviewed feedback while resolving stack
conflicts, and carrying the original branch's combined release wording or version bump into every
split PR instead of scoping each slice to its own changes.

## Default output

When asked to split a PR, produce:

1. proposed PR sequence,
2. branch strategy,
3. scratchpad path and initial contents,
4. extraction commands,
5. verification plan for each PR (`just check` + scoped tests),
6. drift-management plan.
