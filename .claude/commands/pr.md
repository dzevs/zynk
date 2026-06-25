# PR

Prepare a pull request for the current branch (zynk has no separate version-bump or release-note step to run).

1. Ensure the branch is committed and `just check` is green.
2. Self-review the full branch diff (`/selfreview`) and address what you find.
3. Draft a concise, humble PR title using conventional commits (e.g. `fix: …` / `feat(detect): …`) and a short
   description — what changed, why, and a before/after snippet for fixes or an after snippet for features. No
   flowery or verbose language.

**Pushing the branch and opening the PR is an operator gate** — do not push or `gh pr create` without explicit
operator approval (`WORKFLOW.md`). Present the prepared title/description and wait.
