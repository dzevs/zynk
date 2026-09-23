# ADR 0015 - Agent prompt submission proof

- **Status:** Proposed for the M8-39 exact-tip review
- **Date:** 2026-09-23
- **Refines:** ADR 0003 (delivery-event provenance), ADR 0014 (receiver authority)

## Context

`agent prompt` persists a message, submits terminal bytes to a resolved managed agent, and can then
wait for a later observed state. The submit and wait are distinct effects. Recording the event as
`pane.send_input` would hide the command-level admission checks, while recording it only after the
wait would make a completed terminal write look unsubmitted when the later observation times out.

The `delivery_events.proof_source` values are enforced by a SQLite CHECK. Migration 0004 contains the
current seven-value vocabulary, so adding a value by editing that migration would rewrite migration
history. SQLite cannot alter the CHECK in place.

## Decision

1. A verified `agent.prompted` response appends a Submitted delivery event with
   `proof_source = agent.prompt` before any optional wait begins. The row binds the message and the
   terminal that accepted the single encoded input item. A later wait failure returns the same
   `message_id` and submitted status and never resubmits or appends a compensating Failed event.
2. Prompt readiness and managed-agent observations do not confer receiver identity. The persisted
   Party and `expected_terminal_id` come from the same target resolution used by agent send. Receipt
   authority remains `authoritative_receiver_identity` under ADR 0014.
3. Migration 0005 rebuilds `delivery_events`, admits `agent.prompt` in addition to all seven existing
   values, copies all eight columns verbatim, recreates the index, and leaves migrations 0001-0004
   immutable.
4. Protocol remains 19 for this intermediate port even though `agent.start` now has an incompatible
   request shape. The equality-only guard cannot detect that shape difference; old-shape requests are
   typed-refused and never launched. The later protocol-20 port must absorb this contract change.

## Upgrade and rollback

A post-0005 database is `Newer` to a pre-0005 binary. `zynk db status` reports the typed newer-lineage
diagnostic; the older binary does not become ready, classify the database as Foreign, or crash. There
is no reverse migration. Rollback requires a compatible binary or an operator-owned backup/recovery
decision. Editing migration rows or checksums is not a rollback mechanism.

## Consequences

- A Submitted row means the server verified the prompt response and recorded the terminal effect
  before waiting; it does not mean the receiver later became idle, done, or blocked.
- A malformed, contradictory, or lost response remains submission-unverified and carries
  no-resubmit guidance because absence of proof is not proof that no terminal effect occurred.
- Every runtime-emitted proof-source literal must remain a member of the migrated CHECK vocabulary.
- Database live handoff must use binaries that understand the same migration lineage.

## Rejected alternatives

- **Reuse `pane.send_input`.** Rejected because it erases the prompt admission boundary and the
  command that established the proof.
- **Record after waiting.** Rejected because wait failure is not submit failure and would invite an
  unsafe resend.
- **Edit migration 0004.** Rejected because released migration history is immutable and existing
  databases must retain their checksums.
