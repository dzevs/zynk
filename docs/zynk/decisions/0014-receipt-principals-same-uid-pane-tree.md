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
