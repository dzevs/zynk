# Document

Document a feature, decision, or GitHub issue. For an issue: `gh issue view <number> --json title,body,comments,labels`.

Use the `documentation-and-adrs` skill. A durable architectural decision → a new ADR in `docs/zynk/decisions/`
(amend via a new ADR, never rewrite an accepted one). User-facing docs follow the `zynk-docs` styleguides. Keep
docs terse and actionable (see `CLAUDE.md`); no marketing prose.

Commit docs gate-safe (`/commit`); pushing is an operator gate (`WORKFLOW.md`).
