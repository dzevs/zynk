---
name: pr-triage
description: Triage the open zynk PR queue into a decision-first table, prioritizing PRs to merge or close
---

# PR Triage

Survey the open zynk PRs (`gh pr list`) and produce a concise, decision-first table to help merge or close as
many as possible. For each PR: number/title, area, CI status, a one-line assessment, and a recommendation
(merge / changes-needed / close / needs-maintainer), prioritizing the ones closest to mergeable.

Read-only: do NOT merge, close, comment, or push — those are operator gates (`WORKFLOW.md`). Produce the table
and the recommended order; the operator decides.
