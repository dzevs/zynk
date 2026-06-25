# Handle PR Review Comments

View ALL review comments for the current branch's PR (`gh pr view --comments`; include inline review comments,
not just top-level). For each: understand the request, make the change locally (TDD where behavior changes),
run `just check`, then commit gate-safe (`/commit`).

**Replying/commenting on the PR and pushing are operator gates** (`WORKFLOW.md`) — do not comment or push
without explicit approval. Prepare the responses and wait.
