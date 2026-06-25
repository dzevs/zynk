# Fix GitHub Actions CI Failures

Diagnose and fix CI failures for the current branch's PR.

1. `gh pr status` / `gh pr checks` to find the failing checks; `gh run view <id> --log-failed` for the logs.
2. Reproduce locally — `just ci` / `just check` / `just gate` — with an isolated `CARGO_TARGET_DIR` (never the
   live runtime). Find the root cause before fixing (use the `debugging-and-error-recovery` skill).
3. Fix, re-run the relevant check locally until green, then commit gate-safe (`/commit` — no auto-push).

**Pushing the fix is an operator gate** (`WORKFLOW.md`). Do not push without explicit approval.
