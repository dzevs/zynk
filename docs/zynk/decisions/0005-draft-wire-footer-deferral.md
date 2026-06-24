# ADR 0005 — Defer the rendered draft wire-footer (clarify ADR 0002/SPEC for drafts)

**Status:** Proposed (M3b planning addendum; operator-directed). Builds on ADR 0002–0004.
**Date:** 2026-06-14 · **Spec:** `docs/zynk/SPEC.md` §3 F2, §4, §6.

## Context

ADR 0002 §Decision 1 makes the zynk message-layer (`--type` + auto-footer + auto-persist + delivery-tracking)
apply **uniformly** across every text-send command — `zynk pane run`, `zynk pane send-text`, `zynk agent send`
— and requires that zynk-generated protocol IDs MUST be rendered so receiver integrations can correlate and
emit `zynk.message_received`. SPEC §4 likewise says `zynk pane send-text` is persisted with `footer/type` but
stays `drafted` (no Enter) until a future submit.

M3b introduces a **rendered wire footer**: the protocol IDs are appended into the *delivered text* (the bytes
sent to the receiver pane) so a receiver hook can extract them. This raises a question the original ADRs did
not resolve for the **draft** path:

- `pane send-text` stages text with **no Enter** for human review/edit; it is `drafted`, not delivered.
- ADR 0004 **deferred `pane submit`** until exact raw-input proof exists.
- ADR 0002's future `pane submit` carries a **collision guard**: it sends one Enter only after proving the
  pane's current input still matches the draft's `body_hash`, aborting with `draft_mismatch` otherwise.

If a rendered wire footer were appended into a `drafted` message's pane text, then (a) the human would review
and the future `pane submit` would submit `body + footer`, and (b) the `pane submit` `body_hash` guard
(`body_hash` is computed over the *pure body*) could **never match** a footered pane input → `draft_mismatch`
on every footered draft. A rendered draft footer also pollutes the human's editable draft with protocol noise
before any delivery occurs, and the footer's sole purpose — receiver correlation — does not apply to an
undelivered draft (there is no receiver yet).

## Decision

1. **Structured footer remains uniform.** The message-layer continues to persist `footer_json` (structured
   provenance + protocol IDs) and `type`, and to track delivery, **uniformly across all three send commands**
   including `pane send-text`. This satisfies ADR 0002 §Decision 1's auto-footer/auto-persist requirement at
   the structured layer.
2. **The rendered wire footer is limited to submitted send paths.** In M3b, the rendered protocol-ID footer
   is appended into the delivered text **only for `zynk agent send` and `zynk pane run`** (the `submitted`
   paths) **and only for receipt-capable targets** (hook-authoritative agent identity per the M3b plan).
3. **Drafts keep exact text.** `zynk pane send-text` drafts keep the **exact input text**; no rendered footer
   is appended while the text remains only a draft. The honest `drafted` state (ADR 0002 §Decision 4, ADR 0004)
   is unchanged.
4. **Rationale.** ADR 0004 deferred `pane submit`; a rendered draft footer would create exact-input /
   `body_hash` ambiguity against ADR 0002's own `pane submit` collision guard and would pollute the editable
   draft before an exact submit proof exists. Receiver correlation (the footer's purpose) applies only to
   delivered/submitted messages.
5. **Future `pane submit`** (when ADR 0004 is superseded) MUST explicitly design whether and how to render
   and verify a footer at submit time, including the `body_hash` collision guard. M3b does **not** solve or
   claim this.

This ADR **clarifies/supersedes** ADR 0002 §Decision 1 and SPEC §4 **for the rendered wire footer on drafts
specifically**; it does not change structured `footer_json` uniformity, the `drafted` state, or any receipt
invariant.

## Alternatives considered

- **Render the wire footer into drafts too (literal ADR 0002 uniformity).** Rejected: breaks ADR 0002's own
  future `pane submit` `body_hash` collision guard (a footered draft can never match the pure-body hash),
  pollutes the human-editable draft with protocol noise, and serves no receiver (drafts are not delivered).
- **Drop structured `footer_json` for drafts too.** Rejected: structured provenance is cheap, already
  persisted in M2, useful for retrieval/audit, and keeping it preserves ADR 0002 §Decision 1 at the
  structured layer.

## Consequences

- The M3b plan (`docs/zynk/plans/2026-06-14-m3b-m4-footer-live-receipt.md`) references this ADR for the
  draft no-wire-footer rule; the ADR 0002/SPEC ambiguity is removed.
- `pane send-text` drafts remain byte-exact, so a future exact `pane submit` (ADR 0004) can still match
  `body_hash` and submit honestly.
- Whether `pane submit` renders/verifies a footer at submit time is explicitly future work, gated by its own
  ADR/amendment.
