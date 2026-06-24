# ADR 0002 — Message protocol: command surface, delivery & receipt

**Status:** Proposed (gate: Codex review → operator). Builds on ADR 0001.
**Date:** 2026-06-10 · **Spec:** `docs/zynk/SPEC.md` §4–§6.

## Context

Agents collaborating in zynk panes must message each other. The wrapper forced agents to
hand-craft a verbose `[zynk from=… mid=…]` header and proved delivery by scraping/marker-poll —
both wrapper artifacts. Owning the terminal, zynk knows context natively and owns submit. A
decorrelated review (Codex) established the key boundary: **submit ≠ receipt** — `pane.send_input`
proves the bytes were submitted to the PTY, NOT that the receiver application ingested the message.

## Decision

1. **Command surface.** zynk inherits ALL zynk commands (rebranded). The zynk **message-layer**
   (`--type` + auto-footer + auto-persist + delivery-tracking) is applied **UNIFORMLY** across every
   text-send command — `zynk pane run`, `zynk pane send-text`, `zynk agent send` — not special to one.
   `pane send-keys` (raw keys) is excluded. The agent supplies only `text` (+ optional `--type`);
   the system auto-fills/persists from/to/workspace/tab/branch/cwd/timestamps/footer.
   **Footer contract (F2 — binding):** the body stays **pure user/agent text**. The footer has two
   field classes: (a) **provenance/environment fields — native zynk only**, never invented by zynk
   (agent identity + `agent_session{source,kind,value}`, workspace/tab topology, branch/`git_sha`/`cwd`/
   `foreground_cwd`, `report_metadata` annotations); (b) **zynk-generated protocol IDs** — `message_id`/
   `conversation_id`/`conversation_seq` — which ARE zynk's own and MUST be rendered so receiver
   integrations can correlate and emit `zynk.message_received`. The footer is stored structured
   (`footer_json`) and **rendered on output, indexed separately from the body** (retrieval distinguishes
   content from provenance).
2. **Honest submit semantics.** `zynk agent send` (a message) resolves `target`→pane and **SUBMITS**
   via `pane.send_input` (it does NOT inherit zynk's raw literal-no-Enter `agent send` — that was the
   stuck-draft bug); `pane run` submits atomically; both → `submitted`. `pane send-text` deliberately
   does NOT submit → `drafted`. Explicit transition: `zynk pane submit <pane> [--message-id]` sends one
   Enter and moves the targeted/latest draft to `submitted`, with a `draft_mismatch` collision abort if
   the pane input no longer matches the draft `body_hash`. NO implicit "next-Enter" binding (races user input).
3. **No `--reply-to`.** The target encodes reply-to; `derived_parent_id` = latest message in the
   conversation from the same logical party, keyed by `agent_label` (stable across restart/pane churn).
4. **Delivery states — four, never collapsed:** `drafted` (send-text, not submitted) → `submitted`
   (EITHER native `pane.send_input` from `agent send`/`pane run`, OR an explicit `zynk pane submit`
   — one Enter — after a `body_hash` match; the latter records `proof_source=pane.submit`, NOT
   `pane.send_input`) → `received` → `processed`. **`received` requires a message-specific native
   event `zynk.message_received`** (message_id/conversation_id/conversation_seq/receiver agent_session/
   status/seq/timestamp) reported by the RECEIVING zynk integration — SEPARATE from `report-agent`
   (lifecycle/footer). `pane.agent_status_changed` is **corroboration only, never receipt by itself**.
   **Honest fallback:** a receiver without the zynk integration stays at `submitted` (never falsely
   `received`). **NO marker, NO scraping.**
   **Receipt acceptance invariants (BINDING — not deferred to impl):** a `zynk.message_received` event
   advances delivery to `received` ONLY when (a) its `message_id`/`conversation_id`/`conversation_seq`
   match an existing message in the SAME `runtime_session_id`/`socket_namespace`; (b) the reporting
   receiver's `agent_session`/participant matches the message's resolved target; (c) its `seq` is
   monotonic/idempotent per message — duplicates are IGNORED (delivery never advances twice). Invalid,
   mismatched, or cross-runtime events MUST NOT advance delivery. (Agent-specific hook mechanics — how
   each integration emits the event — stay implementation detail.)
5. **Clear responses (F4) — CLI-WIDE, not just sends.** EVERY zynk command returns a clear structured
   response (`result`/`command`/`ids`/`target_resolution`/`status`/`proof`-or-`receipt`/`next`), JSON-stable
   + concise human text. **Raw inherited zynk commands MUST NOT keep empty-stdout + exit-code semantics.**
   Send commands additionally return the persisted message record + `delivery_status`. No silent success,
   no bare `ok` — a new agent must not infer zynk semantics from empty stdout + exit code.

## Alternatives considered

- **Keep the rendered `[zynk …]` marker as receipt** (even a native in-buffer marker-observation) —
  rejected: still text-observation; superseded by the integration-reported `zynk.message_received` event.
- **Accept `submitted` as `received`** (PTY submit = delivered) — rejected: overclaims (Codex P1);
  violates the submit≠receipt boundary.
- **Message-layer only on `agent send`** — rejected: the operator requires consistency across all
  text-send commands.
- **Implicit next-Enter binds the draft** — rejected: races real user input; explicit `pane submit`.

## Consequences

- Supersedes the wrapper's ADR 038 (multiline transport) and ADR 041 (marker verification) entirely —
  deleted, not ported.
- ADR-024-style honesty (no overclaiming `delivery_status`) is preserved natively.
- `received` depends on the receiver running the zynk integration; otherwise delivery is truthfully
  `submitted`-only. Only the agent-specific hook MECHANICS (how each integration emits the event) are
  implementation detail; the receipt **acceptance invariants (Decision 4) are BINDING** in this ADR.
- The `drafted` state + explicit `pane submit` add a small, well-bounded surface.
