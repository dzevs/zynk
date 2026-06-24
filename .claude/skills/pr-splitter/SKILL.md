---
name: pr-splitter
description: Use when breaking a large, complex, messy, or hard-to-review pull request into multiple smaller PRs; planning stacked PRs; extracting independent changes from a branch; splitting mixed refactor and behavior changes; managing drift after review feedback (operator gate, Codex/swarm second opinion); rebasing follow-up PRs as earlier PRs change; or preserving original branch intent while shipping incrementally.
---

# PR Splitter

Treat the original PR as raw material to mine, assemble each smaller reviewable PR on purpose, and keep
a local record of how the stack diverges as review feedback lands. Inside zynk, "review feedback" spans
both the operator gate and the decorrelated Codex/swarm second opinion — once they sign off on a
direction, that direction becomes the stack's new source of truth.

## Required workflow

1. **Snapshot before touching history**
   - Run `git status` to see where you stand.
   - Pin an immutable local pointer to the starting branch: `git branch backup/original-large-pr`.
   - Leave the original branch intact — no deletes, no rewrites — until every slice has shipped.

2. **Inventory the original PR**
   - Survey the diff with `git diff --stat <base>...HEAD`, `git diff --name-only <base>...HEAD`, and
     `git log --oneline <base>..HEAD`.
   - Sort the changes into reviewable buckets: prep/refactor, API/protocol/type changes, behavior,
     tests, docs, cleanup, and generated/lock files (`Cargo.lock`, generated snapshots).

3. **Create a local scratchpad**
   - Capture your split plan in a throwaway local file — `.notes/pr-split.md` is the preferred home.
   - Either gitignore `.notes/` or just keep it untracked. Never commit the scratchpad unless the
     user specifically requests it. (`.notes/` must stay out of every commit — the private-content
     gate trips on stray local artifacts.)
   - Record: original branch, base branch, the PRs you plan, which files/hunks each one pulls, how
     each PR is verified, what original diff is still left over, and where review feedback has pushed
     the stack off the original intent.

4. **Choose the split shape**
   - Reach for stacked PRs whenever later work sits on top of earlier work.
   - Reserve parallel PRs for changes that are genuinely independent of one another.
   - Pick foundation + parallel follow-ups when a single shared prep change opens the door to
     otherwise-independent work (for instance, a protocol/IPC type change that several handlers then
     build on).

5. **Extract changes safely**
   - Favor cutting fresh branches from the right base and selectively restoring into them over
     untangling messy history in place.
   - For whole files that belong cleanly to one slice, extract by path:
     `git checkout backup/original-large-pr -- src/path/to/file.rs`.
   - For files that mix concerns, extract the relevant hunks:
     `git restore -p --source backup/original-large-pr -- src/path/to/file.rs`.
   - Every PR has to stand on its own — buildable and reviewable in isolation. A Rust slice that
     won't compile by itself isn't a legitimate split unit; the crate must build.

6. **Verify each PR independently**
   - For each slice run the tightest relevant build, format, lint, and test pass for its scope:
     `cargo build --release --locked`, `cargo nextest run --locked <filter>`, `just lint`, and
     `just check` before you call the stack green.
   - Keep tests, docs, and generated files attached to the code they back unless the plan
     deliberately decouples them.

7. **Manage drift deliberately**
   - Once a change is approved (operator gate + Codex/swarm verdict), treat it as the stack's new
     source of truth.
   - When an earlier PR shifts, rebase its dependents onto the new version and settle conflicts
     toward the reviewed direction rather than reflexively toward the original branch.
   - Diff the evolving stack against `backup/original-large-pr` to surface intent you still owe —
     not to chase byte-for-byte equality.
   - Log every deliberate divergence in `.notes/pr-split.md`.

8. **Use range-diff for rewritten stacks**
   - After rebases, conflict resolution, or force-pushes, reach for `git range-diff` to see exactly
     what moved. (Force-push stays operator-gated — never run one without an explicit gate.)
   - When you refresh a stacked PR, distill the meaningful range-diff output into a note for
     reviewers.

## PR description pattern

Keep each PR description short and aimed at the reviewer:

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

Don't dump the whole split ledger into the PR body. The granular extraction notes and drift log
belong in `.notes/pr-split.md`.

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

A split PR should document and validate only its own changes — never inherit the original branch's
combined release/changelog wording into a single slice. That branch accounts for the whole combined
change, so its release note has no place in any one piece of it.

Once you've pulled changes into a split branch:

1. **Drop any combined-change release note or version bump that rode along from the original branch.**
   Written for the full diff, they leave misleading history on a slice.
2. **Compose a summary scoped strictly to this PR's changes.** Cover what this particular slice does,
   not the entire original feature.
3. **Name only the modules this PR actually touches.** If the original change spanned the server,
   protocol, and detection layers but this slice only touches `src/detect/`, keep the summary on
   detection.
4. **Size the framing to the slice.** A prep/refactor slice is a small, low-risk PR; a slice that
   adds new protocol/IPC surface is the one worth flagging for the Codex/swarm second opinion.
5. **Keep version bumps, tags, and releases out of split PRs entirely.** Those are separate,
   explicitly operator-gated actions — never bundle one into a routine slice.

Append a "summary + verification scoped to this slice" line under each planned PR in the scratchpad
template so it doesn't slip.

## Common failure modes

Watch out for: splitting along file boundaries when the behavior crosses files, pulling tests apart
from the code they exercise, shipping follow-up PRs that won't compile, force-pushing with no
range-diff summary for reviewers (and no operator gate), retiring the original branch too soon,
undoing reviewed feedback while you untangle stack conflicts, and dragging the original branch's
combined release wording or version bump into every slice instead of scoping each one to its own
changes.

## Default output

A split request should yield:

1. proposed PR sequence,
2. branch strategy,
3. scratchpad path and initial contents,
4. extraction commands,
5. verification plan for each PR (`just check` + scoped tests),
6. drift-management plan.
