# ADR 0009 — Visible awareness header replaces the hidden receipt footer

**Status:** Proposed (gate: operator). **Supersedes the wire-visibility + auto-receipt behavior only** of
ADR 0002 §Decision 4 (receipt) + ADR 0005 (rendered draft/submit wire footer) + the M3b/M4 footer and
pi-receiver mechanics — for the **wire-visible-text + footer-driven receipt parts**, nothing else. All other
ADR 0002 decisions (command surface, honest submit semantics, four delivery states, F4 envelope) and the
server-authoritative receipt **API** stand.
**Date:** 2026-06-15 · **Spec:** `docs/zynk/SPEC.md` §3 F2, §6 · **Milestone:** M6/M7-adjacent.

## Context

M3b/M4 shipped a **hidden receipt footer**: a minimal protocol-ID footer rendered into the *delivered text*
of `submitted` sends and parsed back out by a receiver hook. As built it had three defects that converge into
one bug:

- **Pi-only.** The wire footer was gated to a single agent via an allowlist (`["pi"]`); only Pi was
  receipt-capable. Claude and Codex were never receipt-capable on the wire.
- **Asymmetric + invisible.** Pi ran a **custom receiver** that parsed the footer IDs from its structured hook
  input and **stripped** the footer before the model saw it. So Pi's model saw a clean prompt with the
  metadata removed; Claude and Codex received the message as a **plain prompt** with no zynk framing at all.
  Every agent therefore looked at what reads as an ordinary direct-chat message — none of them was made aware
  that it was a zynk message, who sent it, or how to reply through zynk. Agents replied in direct chat,
  unaware a delivery/conversation record existed.
- **Footer-driven auto-receipt.** Observing/parsing the delivered footer was used to fire
  `zynk.message_received` for Pi. This conflated "the wire text arrived (and could be parsed)" with "the
  receiver application ingested the message" — a screen/marker-style observation dressed up as receipt, which
  is exactly the submit≠receipt boundary ADR 0002 drew.

The asymmetry (pi-only), the invisibility (stripped from the model / absent for others), and the
observation-as-receipt are the bug. The operator decision is to make the framing **uniform and visible** and
to **stop** treating any wire observation as receipt.

## Decision

1. **A VISIBLE awareness HEADER replaces the hidden footer — uniform for every agent target.** For **every**
   native zynk message delivered to an agent target (claude/codex/pi alike), an agent-readable header box is
   **PREPENDED** before the body. The delivered wire text is `HEADER + "\n\n" + PURE_BODY` (header first, then
   a blank line, then the unmodified body). The header is uniform across all agents — there is no per-agent
   allowlist, no per-agent variant — and it **replaces** the old hidden wire footer / zynk-wrapper header. The
   header is for **agent awareness** (who sent this, through which pane, what type, how to reply) and is
   **NEVER stripped** by any receiver. Exact box template (operator-decided — keep readable + stable):

   ```
   ╭─ Zynk message ─────────────────────────
   │ from: <from_agent> <from_pane>  cwd: <from_cwd>
   │ to:   <to_agent> <to_pane>  cwd: <to_cwd>
   │ type: <type>            (omit this line when no message type)
   │ id:   <message_id>
   │ conv: <conversation_id>#<conversation_seq>
   │ reply: zynk reply <from_pane> -- "<your response>"
   │ note: header is for agent awareness; not receipt proof
   ╰────────────────────────────────────────
   ```

   Missing/unknown optional fields (`cwd`, agent label) render as `-`, or — for the wholly-optional `type`
   line — the line is **omitted gracefully**. Never panic on a missing field. `body_hash` is **not** shown in
   the header: the header is human/agent awareness, not a machine-parse surface. The `reply:` line gives the
   agent the exact zynk reply command targeting the sender's pane.

2. **The hidden receipt footer is DELETED; Pi returns to Zynk state-only.** The rendered protocol-ID wire
   footer is removed. Pi's **custom receiver/parser/strip** path is removed: Pi no longer parses footer IDs
   from its hook input, no longer strips anything, and no longer fires receipt. Pi becomes **Zynk state-only**
   — detected/tracked exactly like every other agent, with no zynk-specific receiver extension behavior. There
   is no longer any agent with a footer-parsing receiver.

3. **Auto-receipt-from-footer is REMOVED — a visible header is NOT receipt proof.** Delivering (and the
   receiver seeing) the visible header does **not** advance delivery. `delivery_status` stays **`submitted`**
   for every agent and is **never auto-promoted to `received`** by header/footer/marker/screen/status
   observation. The server-authoritative `zynk.message_received` API + `receipt.rs` are **retained as a
   DORMANT capability**: the validated server event and its binding acceptance invariants (ADR 0002 §Decision 4
   (a)–(c): id/runtime match, receiver-identity match, monotonic/idempotent `seq`) still exist and still work,
   but **nothing auto-fires them** post-M3b/M4 removal. This keeps a future **uniform** receipt possible (all
   agents, each requiring hook-authority) without re-introducing the pi-only asymmetry.

4. **Proof invariant UNCHANGED.** No header, footer, marker, screen text, or status change is ever receipt
   proof. `received` is reached **only** via the validated `zynk.message_received` server event (ADR 0002
   §Decision 4). This ADR does not weaken that invariant; it **removes** the one path (footer observation) that
   had quietly stretched it.

5. **Body purity preserved.** The header is **wire-only** (prepended into the delivered bytes). The stored
   `messages.body`, its `body_hash`, and the FTS index stay the **pure body** — the header is not persisted as
   body and is not indexed as content. (This mirrors the F2 rule that provenance is indexed separately from the
   body; the awareness header is a delivery-time wrapper, not content.)

## Alternatives considered

- **Keep the pi-only hidden footer** — rejected: it is unfair (one agent receipt-capable), asymmetric (custom
  receiver/strip for Pi only), and invisible (stripped from Pi's model, absent for Claude/Codex). That triad is
  the bug this ADR removes.
- **Make receipt uniform now** (every agent fires a validated `zynk.message_received`) — rejected for this
  cycle: it requires **hook-authority for all agents**, including the reserved-native Codex and Claude, which is
  a large architectural lift (per-agent authoritative receipt hooks). De-scope instead: keep the receipt API
  dormant and ship the uniform **awareness** header now; a future ADR can land uniform receipt on top of the
  dormant API.
- **Render a machine-parseable footer in addition to the visible header** — rejected: re-introduces the
  observation-as-receipt temptation and pollutes the wire; awareness needs only a readable header, and any
  future receipt must come from the server event, not wire text.
- **Keep the header but auto-promote `received` on delivery** — rejected: delivery/visibility is not ingestion;
  auto-promotion would violate the submit≠receipt invariant exactly as the footer auto-receipt did.

## Consequences

- Every agent (claude/codex/pi) now receives a **uniform, visible** awareness header with sender identity, the
  message/conversation IDs, the message type (when present), and an exact `zynk reply` instruction — replacing
  the pi-only hidden footer and the old zynk-wrapper header. No agent's model is fed a footer that was silently
  stripped or a plain prompt with no zynk framing.
- **No agent reaches `received` anymore.** With footer auto-receipt removed and no uniform receipt yet,
  `delivery_status` truthfully stays `submitted` for all agents until a future **uniform** receipt (built on
  the retained dormant `zynk.message_received` API) lands via its own ADR. This is honest, not a regression:
  the old `received` was pi-only and rested on an observation that should never have been proof.
- The server receipt **API** + `receipt.rs` + the binding acceptance invariants remain in the tree as a
  dormant, callable capability — no caller auto-fires them. The proof invariant is unchanged.
- Pi's live extension must be **reinstalled as state-only (v4)**: the custom receiver/parser/strip is gone, so
  the previously-installed receipt extension no longer matches the build and must be replaced at the operator's
  cutover (M7-adjacent). This is the only live-side follow-up.
- `messages.body` + `body_hash` + FTS stay pure; the header is wire-only and never persisted/indexed as content.
- ADR 0002's command surface, honest submit semantics, four delivery states, and F4 envelope are untouched;
  ADR 0005's structured protocol-metadata persistence is preserved, though its DB column is renamed `footer_json` → `protocol_json`; only its *rendered wire footer* is superseded (by the visible header).
