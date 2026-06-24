# ADR 0004 — Defer `pane submit` until exact draft proof exists

**Status:** Proposed (M2 implementation addendum). Builds on ADR 0002–0003.
**Date:** 2026-06-14 · **Spec:** `docs/zynk/SPEC.md` §4, §6.

## Context

ADR 0002 defines `zynk pane submit <pane> [--message-id]` as the explicit transition from a
persisted `drafted` message to `submitted`: zynk sends one Enter only after proving the pane's current
input still matches the draft's `body_hash`. The same ADR rejects implicit next-Enter binding because
it races user input, and the fork deletes wrapper-era screen scraping/marker machinery.

During the M2 feasibility spike, the current zynk API/state surface was inspected:

- `PaneInfo` exposes pane metadata (`pane_id`, `terminal_id`, workspace/tab, cwd/foreground_cwd,
  agent/session/status, revision), but no raw input buffer.
- `pane.read`/detection reads rendered terminal output, not the raw unsubmitted input line.
- `PaneSendText`, `PaneSendInput`, and `PaneSendKeys` write bytes to the PTY but do not expose a durable
  current-input snapshot.
- A small server-side draft guard is possible only with new cross-cutting server state: record zynk
  `send-text` drafts, pass message/body hashes into the server, invalidate on API writes and raw user
  input, clean up on pane close/restart, and fail closed after any unknown input. That is larger than
  the M2 F1 persistence slice and would still need careful review to avoid racing real user input.

## Decision

M2 does **not** ship `zynk pane submit`.

- `pane send-text` messages are persisted with `delivery_status = drafted` and a `drafted` delivery event.
- Drafted messages remain `drafted` in M2. There is no M2 transition to `submitted` for drafts.
- No visible-buffer, detection-buffer, screen-scraping, marker, or prompt-text hash is acceptable proof.
- `proof_source = pane.submit` remains reserved for the future exact implementation.
- Implementing `pane submit` later requires one of:
  1. a native raw input-buffer API/state that can be compared to the stored `body_hash`; or
  2. a reviewed server-side draft guard that records zynk drafts and invalidates them on every possible
     subsequent input path, failing closed across restarts/unknown state.

## Consequences

- M2 can complete F1 persistence without adding broad server-side draft state.
- The delivery model remains honest: zynk never sends Enter for a draft unless it can prove the draft
  text was not mutated.
- Users/agents can still use `pane run` or `agent send` for submitted messages; `pane send-text` remains
  a durable draft-only operation until a later milestone implements exact submit proof.
- ADR 0002's final-state design is preserved, but the `drafted → submitted` command is implementation-
  deferred from M2.
