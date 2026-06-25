# Debug GitHub Issue

Investigate a zynk GitHub issue: `gh issue view <number> --json title,body,comments,labels,assignees`.

Reproduce the reported behavior in an isolated dev runtime (never the live socket/config/DB). Trace the root
cause (use `debugging-and-error-recovery`, or `debugging-difficult-bugs` for runtime/concurrency/persistence
bugs); confirm with a failing test before fixing. Summarize the root cause and a proposed fix.

Any fix follows the normal gates (TDD, `just check`, gate-safe `/commit`); pushing or commenting on the issue
is an operator gate (`WORKFLOW.md`).
