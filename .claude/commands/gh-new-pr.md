# Create New Pull Request

Open a PR for the current branch. **This pushes the branch — an operator gate.** Do not push or create the PR
without explicit operator approval (`WORKFLOW.md`).

Once gated: use `gh pr create --web` (opens in the browser so the operator can edit the title/description).
Title = conventional commits (`fix: …` / `feat(pkg): …`); description = concise, humble, and to the point, with
a before/after snippet for fixes or an after snippet for features.
