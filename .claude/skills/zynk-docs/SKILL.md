---
name: zynk-docs
description: Guidelines for writing or editing zynk documentation (README, CLAUDE.md/AGENTS.md/WORKFLOW.md, ADRs, user docs). Use when creating or updating any doc in this repo.
---

# zynk Documentation Guidelines

Use this skill when you write or edit zynk docs. Start with `docs/styleguides/STYLEGUIDE.md` (the default
writing guide), then apply the doc-type guidance below.

## Doc types

- **`README.md`** — user-facing: what zynk is, install, quickstart, key usage. The ONLY place for the project
  intro/marketing.
- **`CLAUDE.md` / `AGENTS.md` / `WORKFLOW.md`** — agent guidance. Terse + actionable ONLY (build, commands,
  conventions, architecture invariants + file paths + the WHY, gotchas). NO marketing or "what the project is"
  prose — that belongs in README. These load into context every session, so noise wastes tokens and reduces
  adherence. Target under 200 lines.
- **`docs/zynk/decisions/NNNN-*.md` (ADRs)** — `# ADR NNNN — Title`, then `**Status:** … **Date:** …`,
  `## Context` / `## Decision` / `## Alternatives considered` / `## Consequences`. Accepted ADRs are binding —
  amend via a NEW ADR, never rewrite one.
- **`CONTRIBUTING.md` / `CHANGELOG.md` / `SECURITY.md`** — keep current and factual.

## Linting

`just docs-lint` runs Vale (write-good + the custom `zynk` prose style in `docs/styles/`) over the docs —
optional, not a hard CI gate. Run it before committing docs.
