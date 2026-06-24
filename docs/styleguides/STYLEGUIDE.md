# zynk documentation styleguide

The default writing guide for zynk docs (the `zynk-docs` skill routes here).

## Core rules

- Write clearly and directly: short sentences, short paragraphs, simple words, low jargon.
- Break dense text with headings and bullet lists. Refer to the reader as `you` when needed.
- Write for readers who are tired, rushed, reading in a non-native language, or new to terminals/Rust.
- Lead with the actionable point; cut filler. Every line should earn its place.

## Terse + actionable — no marketing

- Agent-facing docs (`CLAUDE.md`, `AGENTS.md`, `WORKFLOW.md`) load into context every session, so noise wastes
  tokens and *reduces* adherence. Include only facts an agent needs and cannot quickly derive: build/commands,
  conventions, architecture invariants + file paths + the WHY, gotchas.
- Be specific and verifiable: "`no unwrap()` in production", not "write clean code". Structure with headers +
  bullets. Target under 200 lines.
- The "what zynk is" intro belongs in `README.md` ONLY — never repeat it in `CLAUDE.md`/`AGENTS.md`.

## Tone

- Neutral and factual. Not funny, whimsical, or story-driven. Keep each page self-contained.

## zynk specifics

- Keep the AGPL attribution in `NOTICE`/`LICENSE` (it credits upstream herdr). Don't reintroduce `herdr` in
  active source/docs beyond that attribution (ADR 0010).
- `ZYNK_*` is the env surface; source/event labels are `zynk:<agent>`.

## Linting

Run `just docs-lint` (Vale: write-good + the `zynk` prose style) before committing docs. Optional, not a hard CI gate.
