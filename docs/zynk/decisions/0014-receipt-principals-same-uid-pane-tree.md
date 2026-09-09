# ADR 0014 — Receipt principals are the target pane's same-UID process tree

- **Status:** Accepted 2026-09-09 (operator decision, Gate-3 B1)
- **Refines:** ADR 0002 Decision 4 (receipt acceptance invariants) — adds a principal to the existing
  id/session invariants, changes none of them. **Consistent with:** ADR 0009 (no wire observation is a
  receipt), ADR 0013 (Linux-only APIs).
- **Review date:** none. Weakening the principal requires a new ADR.

## Context

ADR 0002 Decision 4 makes `received` depend on the receiver integration's message-specific
`zynk.message_received`, with matching `message_id`/`conversation_id`/`conversation_seq`, the same
`runtime_session_id`/`socket_namespace`, and a receiver session that matches the message's resolved target.
Identity itself is hook-reported over the same API socket (`pane.report_agent`,
`pane.report_agent_session`).

The socket is `0o600` and owned by the user, so the server already knows every caller is the same UID. What
it did NOT know is *which* process called. Gate-3 B1 reproduced the consequence: a passive process sitting in
a pane — a `cat`, or anything else the user starts — can report an agent identity for **any** pane and then
receipt messages addressed to it. The identity did not have to belong to the pane the caller was in, and the
receipt did not have to come from the receiver at all.

A cryptographic or process-identity proof that "the integration ran" is not available across same-UID
processes:

- `argv` is forgeable, so naming the binary proves nothing.
- Hook scripts are separate processes from the agent (a `bash` child, a `python3` grandchild), so "the agent's
  own process reported it" is not a rule any real integration satisfies.
- The user's own processes are the adversary model, and they can read any secret the pane holds.

So the honest question is not "did the integration run" — it is "**which pane** does the caller belong to".
That one the kernel can answer.

## Decision

1. **Principals.** The receipt principal is the **target pane's process tree under the same UID**. Identity
   reports (`pane.report_agent`, `pane.report_agent_session`) and receipts (`zynk.message_received`) are
   accepted only from a socket peer whose PID ancestry reaches the target pane's PTY child PID. The peer PID
   comes from `SO_PEERCRED` on the accepted connection — never from a request field — and the ancestry is
   walked through `/proc/<pid>/stat`'s PPid chain, bounded. A report from another pane, from outside any
   pane, from a different UID, or one whose ancestry cannot be established (a reparented or daemonized hook)
   is refused **fail-closed** with the F4 error `caller_outside_pane`; for a receipt the message stays
   `submitted` and no delivery event is written.

2. **Semantics.** `received` means: *a process inside the receiver pane's tree, holding that pane's
   environment, reported ingestion of exactly these ids.* It is the integration's report under the same-UID
   model — **not** a proof that the integration binary ran. The pane's own processes are trusted principals,
   deliberately. The persisted provenance value is renamed from `integration` to **`pane_tree`** so the
   record says what was actually proven, and no CLI or API wording may call a receipt a proof that an
   integration ran or was verified. (ADR 0003's `proof_source` enumeration is amended by this ADR:
   `pane.send_text | pane.send_input | pane.submit | pane_tree | operator | system.recovery`.)

3. **Unbound methods.** `pane.release_agent`, `pane.clear_agent_authority` and `pane.report_metadata` stay
   unbound. Release and clear only ever RETIRE an identity — refusing them would let an out-of-pane caller
   keep a dead session alive, which is the wrong direction to fail — and operator tooling releases panes from
   outside them by design. `pane.report_metadata` writes presentation only (title, display agent, custom
   status, state labels); it never touches `hook_authority`, `hook_identity` or the persisted session, so it
   cannot grant an identity.

## Consequences

- **Cross-pane and out-of-pane impersonation is refused**, and tested: another pane's process, the test
  harness process, and a double-forked child whose parent became PID 1 all fail closed.
- **In-pane self-assertion remains possible and is accepted by design.** A passive process in a pane may
  still claim that pane's identity and receipt its messages. That is the same-UID model stated plainly, not a
  gap left open by accident.
- **Detached hooks are refused.** An integration must keep its hook inside the pane tree. Every bundled asset
  already does:
  - **In-process plugins** (`pi`, `omp`, `opencode`, `kilo` — Node/Bun `net.createConnection`): the agent
    process itself opens the socket.
  - **In-process plugin that shells out** (`hermes` — `subprocess.run` of the zynk CLI): a direct child of
    the agent.
  - **Shell hooks** (`claude`, `codex`, `copilot`, `cursor`, `devin`, `droid`, `kimi`, `qodercli`): the agent
    runs `bash <hook>.sh <action>`, which runs `python3` on a heredoc, which opens the socket — a
    child-of-a-child, still in the tree. None of the thirteen backgrounds, `setsid`s, `nohup`s or double-forks
    its reporter.
- **A live handoff keeps working.** `server.live_handoff` preserves pane child PIDs, so the ancestry the
  check walks is the same before and after.
- **Linux-only, per ADR 0013.** `SO_PEERCRED` and `/proc` are the platform; both live behind
  `src/platform/`.
- **A debug-only test seam exists and must stay out of release builds.** Integration tests report identity
  from the harness process, which is outside every pane. The seam that lets a connection adopt the pane's
  child PID is compiled only under `#[cfg(debug_assertions)]`, so its env var name does not exist in a
  release binary. The release-binary string audit must assert that absence alongside its other checks.

## Rejected alternatives

- **Detector corroboration as proof** — require the pane's foreground process to look like the agent that is
  reporting. Rejected: an agent-shaped passive process defeats it (`exec -a claude cat` is enough), and it
  would break every real integration whose hook runs while the agent is mid-turn. Screen detection stays what
  it already is: evidence for resume rules, never a receipt principal.
- **Per-pane secret tokens** — mint a secret into each pane's environment and require it on every report.
  Rejected: every process in the pane inherits it, so it proves exactly what the process tree already proves,
  and it adds a secret to leak into logs, core dumps and `/proc/<pid>/environ`.
- **Cryptographic attestation of the integration** — sign reports with a key only the integration holds.
  Rejected: there is no key material on either side, no way to provision one that the user cannot read, and
  no trusted party to vouch for a same-UID process.

## Amendment 2026-09-09 (pre-merge; Codex Gate-2 msg_3e339000b75278a4)

Decision 2's rename applies to the receipts **this build records**. It does not reach back.

- A `delivery_events` row written before the origin check carries `integration`. That server matched the
  message ids and the receiver's hook-authoritative identity, but never established which process the
  caller was, so `pane_tree` — "a process inside the receiver pane's tree reported the ingestion" — is
  evidence it never held. The consequence was reproduced on a real upgrade of one database: a receipt taken
  by a process outside every pane, recorded by the previous binary, read back as `received`/`pane_tree`
  once the next binary opened the same file, with no new receipt.
- **No historical row is relabelled.** Migration 0004 WIDENS the `proof_source` CHECK instead of rewriting
  rows. `integration` stays in the enumeration as the LEGACY, origin-unverified value
  (`receipt::LEGACY_RECEIPT_PROOF_SOURCE`), and this build never writes it:
  `append_delivery_event_in_transaction` refuses that value outright.
- The ADR 0003 enumeration amendment stated in Decision 2 therefore **adds** `pane_tree` rather than
  replacing `integration`: `pane.send_text | pane.send_input | pane.submit | integration | pane_tree |
  operator | system.recovery`.
- No CLI, API or document may present an `integration` row as a pane-tree receipt. Its provenance is a
  claim about which check ran, and for those rows the origin check did not run.
- **Only a row recorded AFTER migration 0004 has run, by a build that performs the pane-tree origin
  check, may carry `pane_tree`; a row that predates it can only ever be `integration`.**

## Amendment 2026-09-09 (pre-merge; ARCH-E8-ADR14-PID-REUSE-001)

Decision 1 is stated in pids. **A pid is not an identity.** The kernel reuses pids, so a pid outlives
the process it named, and the check as first written treated whatever holds a number as the process
that used to hold it. Both ends of the comparison are therefore **`(pid, start time)` principals**, read
from `/proc/<pid>/stat` field 22.

- **The pane's principal** is its PTY child's pid together with the start time captured when that pid
  was published. If the pane's root has been reaped, its pid is free, and an unrelated same-UID process
  that receives it — or any child that process forks — reaches that pid by ordinary ancestry. The
  pid-only predicate placed such a caller *inside the pane*, which is the pane's whole standing:
  reporting its identity, and receipting its messages. The reviewer demonstrated acceptance at the
  predicate, not an end-to-end `received` event; a receipt additionally needs a stale identity and a
  delayed pane-died signal. The predicate is the principal, so the hole is in the principal.
- **The caller's principal** is the `SO_PEERCRED` pid together with the start time read at accept. The
  second window is between the peer's `connect()` and the server's `/proc` read: the peer could exit and
  its pid be handed to another process — possibly one genuinely inside the pane. The connection was
  never that process's, so a start time that no longer matches is a refusal.
- **Fail closed, by name.** A pane root that was published without a start time, a pane root that is
  gone from `/proc`, a pane root whose pid now holds a different process, a caller whose start time was
  unreadable at accept or has changed since, any failed `/proc` read, and the existing hop bound are all
  refusals. In particular, **a reaped pane root refuses every caller**: nothing can be inside a tree
  whose root no longer exists. They stay one F4 code, `caller_outside_pane`, with the reason in the
  message, because the socket `ErrorBody` carries only a code and a message.
- **A live handoff keeps the principal.** The handed-over pane keeps its child process, so it keeps that
  process's start time: the exporting server sends it, and a server that predates the field sends none,
  in which case the importer re-reads it from the still-live child rather than let an upgrade — the very
  thing a live handoff exists to perform — leave every pane unidentifiable.
- **`SO_PEERPIDFD` is the future strengthening, and is not used yet.** A pidfd taken by the kernel at
  connect would remove the caller-side window entirely and would let each hop of the ancestry walk be
  pinned rather than re-read. It needs Linux >= 6.5, and this fork's users include Ubuntu 22.04 on the
  5.15 kernel, so adopting it now would fail closed on a supported platform. Until that floor moves,
  `(pid, start time)` is the mechanism; the residual window is that the walk reads each hop at a
  different instant, with the two endpoints pinned.
