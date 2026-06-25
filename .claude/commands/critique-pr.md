---
name: critique-pr
description: Analyze and critique the current pull request and draft an optional review comment (posting is operator-gated)
---

# Review Pull Request

Review the PR for the current branch (`gh pr view`, `gh pr diff`, or diff against `main` if no PR exists yet).
Read the WHOLE diff carefully — re-read tricky sections; read the surrounding code when a change looks subtle or
risky.

Critique across: correctness/bugs; the zynk invariants (state≠runtime, render-pure, detection-snapshot-only,
identity hook-authoritative, body purity, submit≠receipt — see `CLAUDE.md`); tests (coverage + the
characterization-required surfaces); security (IPC/socket, untrusted pane output); performance (render/IPC/PTY
hot paths); and simplicity. Use the `code-review-and-quality` skill.

Produce a structured critique (findings by severity, each with `file:line`). Drafting a review comment is fine;
**posting it to the PR is an operator gate** (`WORKFLOW.md`).
