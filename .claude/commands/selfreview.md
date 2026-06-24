# Self-Review

Critically review your own branch work before handing it off for review. Diff the FULL branch (`gh pr diff` if a
PR exists, else against `main`) — not just the last commit. Read the entire diff carefully; do not skim; re-read
tricky sections and the surrounding code.

Look for: bugs / logic mistakes; missing tests (especially the characterization-required surfaces in
`CLAUDE.md`); violated zynk invariants; scope creep; leftover debug code or `unwrap()`; and anything the
Codex / swarm review would flag. Fix what you find (gate-safe), then run `just check`. Report the residual risks
honestly — do not claim done if a check is red or a step was skipped.

This command is review-only: never push, merge, or open a PR from here — those stay explicit operator-gated steps (see `/commit`, `/pr`, and `WORKFLOW.md`).
