# zynk — specification (zynk fork)

**Status:** operator-gated design → spec (Phase 1). Converged Claude↔Codex (decorrelated),
operator-gated. Implementation is IN PROGRESS (see the current-state note below).
**Repo:** this is the zynk terminal workspace manager (`upstream` = `dzevs/zynk`).
**Supersedes:** the design draft at `agent-collab-protocol:outputs/sessions/zynk-fork-architecture/design-draft.md`.

> **CURRENT STATE (2026-06-15, M6):** This spec's requirements are binding and unchanged; the notes below
> only record build progress and the two path/binary refinements made by ADR 0007/0008 (additive, not a
> rewrite). Implemented: F1 global SQLite persistence (M2), F2 protocol metadata + visible header (M3b/M4, ADR 0009),
> dormant server-authoritative receipt API (M3a), F4 structured responses (CLI-wide), and F3 hybrid retrieval
> (M5: FTS5/BM25 + sqlite-vec + RRF). M6 makes the native
> command surface (`zynk send/reply/thread/inbox/whoami/who/query`), the user-facing rebrand (binary `zynk`;
> crate/package stays `zynk`), the native config/data layout, and a safe wrapper→native DB cutover
> **repo-ready** — no live install/dogfood/wrapper-replacement/cutover (those are gated to later milestones;
> see `docs/zynk/cutover-readiness.md`). Two current-state path refinements vs the original prose, both via
> accepted ADRs: the final DB path drops the `zynk-v2` subdir (ADR 0008 — see §3 F1 / §7 notes), and config
> lives at `~/.config/zynk/config.toml` separate from data (ADR 0007 §5 — see §8 note). The user-facing
> override env vars are now `ZYNK_*`-primary with `ZYNK_*` retained as transitional compat (ADR 0007 §5).
> The M3b/M4 hidden, pi-only receipt **footer** is superseded by **ADR 0009**: a uniform **visible awareness
> header** is prepended to every agent-targeted message, and footer-driven auto-receipt is removed
> (`delivery_status` stays `submitted`; the server-authoritative receipt API stays dormant) — see the §3 F2
> and §6 STATUS notes.

---

## 0. Hard rules (binding)

1. **NO publish / push before full LOCAL testing + operator gate.** No `cargo publish`,
   no `git push` of the fork, until everything is tested locally and the operator approves.
2. **Local testing ALWAYS via an ISOLATED dev runtime.** zynk (+ the frozen zynk wrapper) is
   LIVE on this machine and the agents (claude/codex/pi — including the dev session itself) run
   INSIDE zynk. The fork MUST NEVER touch the live zynk socket/config. **Isolation mechanism
   (source-grounded):** rebrand zynk's `app_dir_name()` -> `zynk`/`zynk-dev`, so `config_dir()` and
   the derived data dir + both sockets relocate to `~/.config/zynk[-dev]` by construction
   (`ZYNK_CONFIG_PATH` is a config FILE, not a dir); plus an isolated `CARGO_TARGET_DIR`
   (e.g. `/tmp/zynk-target`). Broad `ZYNK_*` explicit-override aliasing is a later complete rebrand task.
   **ENFORCEABLE preflight (not prose — code):** the dev binary/test harness MUST, before any
   socket connect, print and assert the active `session_name`, `config_dir`, `config_path` (the config
   FILE), `api_socket`, `client_socket`, and `target_dir`, and **ABORT (nonzero) if ANY resolves to the
   live zynk default** (catching `ZYNK_SOCKET_PATH`/`ZYNK_CLIENT_SOCKET_PATH`/`ZYNK_CONFIG_PATH`
   overrides) or if `CARGO_TARGET_DIR` is unset/default; dev runs scrub those override vars. The `db_path` assertion joins in **M2** (when the
   DB exists). No test runs against an un-asserted runtime.
3. **Accepted decisions are binding** — amend via a new ADR in `docs/zynk/decisions/`, do not
   rewrite. The wrapper's old `decisions/` (ADR 024–041) do NOT carry; they bound only the
   frozen `zynk` v1.5.1 wrapper.

## 1. Goal & non-goals

**Goal:** zynk stops being a zynk *wrapper* and BECOMES a zynk *fork* — owning the
terminal-multiplexer layer — and adds a native conversation protocol with persistence,
auto-metadata, retrieval, and legible responses, so multi-agent collaboration is first-class
and observable without text-scraping.

**Non-goals / explicitly deleted:** the wrapper-era delivery machinery (marker verification,
`classify_input`, `--source visible` scraping, marker-poll, preflight, the whole ADR 038–041
saga). Those were artifacts of observing zynk from outside. Owning the terminal makes submit +
state native, so they are **deleted, not ported**. D8 ("authoritative input-state") is not
"solved" — it **evaporates**.

## 2. Licensing & repo

- zynk is **AGPL-3.0-or-later** (dual w/ commercial). zynk = a derivative fork → **AGPL permanent**
  (operator-accepted; no proprietary zynk without a future commercial license from the zynk author).
- **Model = oh-my-pi**: `git clone` zynk (history KEPT) → repo `zynk` → `upstream` remote → minimal
  rebrand → preserve AGPL LICENSE + attribution (`© ogulcancelik`, `© Zevs`).
- **Identity:** the fork **IS `zynk`**. crates.io `zynk` 0.x–1.5.1 stay MIT (frozen, immutable);
  the fork ships as a **major bump (2.0.0)** with an explicit relicense note (not silent) — one
  package, one version line. NO second package, no "zynk-terminal".
- Operator's own NEW modules MAY also be released as standalone MIT crates (reusable), but the
  shipped zynk binary (= zynk + zynk code) is AGPL.
- **Upstream relicensed after the fork** (upstream `cd5ea1be`, 2026-07-22), so the bullets above
  describe only the pre-relicense provenance. Upstream code taken from that commit onward arrives
  under **Apache-2.0**, not AGPL, and is redistributed inside zynk's AGPL combined work under
  Apache-2.0 §4 — text in `LICENSE-APACHE-2.0.upstream`; the modified-file index in `NOTICE` tracks the
  change notices §4(b) requires in modified files (a convention, not compliance proof by itself).
  zynk's own license is unchanged. `NOTICE` is authoritative on provenance.

## 3. Feature set (FINAL)

zynk = **zynk's full command surface** (workspace/tab/pane/agent/wait/worktree/integration/…),
rebranded `zynk X` → `zynk X`, **plus** four zynk-layer features:

- **F1 — Global conversation DB.** Every zynk message auto-persists to a **global SQLite** store
  (NOT per-project). Path convention keeps the Codex-style `ZYNK_HOME` root (default `~/.zynk`) while
  namespacing the native fork DB under `zynk-v2`: config `sqlite_home` override → `ZYNK_SQLITE_HOME`
  override → `$ZYNK_HOME/zynk-v2` → `~/.zynk/zynk-v2`; DB file `zynk.db`. The wrapper-era
  `~/.zynk/zynk.db` is never migrated/overwritten in place. **sqlx migrations**
  (migration-table-driven), **WAL**, 4KiB pages. One core DB; optional separate retrieval DB if
  vector-index bloat warrants. **Security is the operator's responsibility — no warnings.**
  > **STATUS (ADR 0008, supersedes the DB-path default only):** the `zynk-v2` subdir was a temporary
  > coexistence workaround; the **final native default path is the clean `$ZYNK_HOME/zynk.db`** (default
  > `~/.zynk/zynk.db`). Native zynk **fails closed** (no overwrite, no in-place migration) if a foreign or
  > wrapper-era DB occupies that path, and provides an explicit, non-destructive `zynk db` adopt/backup/import
  > cutover action; the legacy `…/zynk-v2/zynk.db` is recognized only for transitional detection/import. All
  > other F1 decisions (schema, WAL, async embedding, multi-runtime provenance) stand.
- **F2 — Auto protocol metadata + visible header.** The agent sends a **plain message**; zynk auto-attaches
  **two field classes** (see ADR 0002 §Decision 1): (a) **provenance/environment fields — native zynk
  only, never invented:** agent identity + `agent_session{source,kind,value}`, workspace/tab (topology),
  branch/`git_sha`/`cwd`/`foreground_cwd`, optional `report_metadata` annotations; (b) **zynk-generated
  protocol IDs — `message_id`/`conversation_id`/`conversation_seq` — which ARE zynk's own.** Both classes are
  persisted structured in the `protocol_json` DB column (indexed separately from body) so receiver
  integrations can correlate; the protocol IDs + sender identity ALSO render in the visible header (see the
  STATUS note below). Replaces the hand-crafted `[zynk from=… mid=…]` header the wrapper forced agents to
  build. **Body is pure text; the structured metadata (`protocol_json`) is separate; neither pollutes
  `body`/`body_hash`/FTS.**
  > **STATUS (ADR 0009, supersedes the rendered *wire* footer only):** the M3b/M4 hidden, pi-only wire
  > footer (parsed + stripped by a custom pi receiver, invisible to the model) is **deleted**. Every native
  > message to an agent target (claude/codex/pi alike) now carries a **VISIBLE awareness HEADER prepended
  > before the body** (`HEADER + "\n\n" + PURE_BODY`) — uniform, never stripped, showing sender identity,
  > `message_id`, `conversation_id#conversation_seq`, the type (when present), and an exact `zynk reply`
  > instruction; `body_hash` is not shown. The structured protocol-metadata persistence (this bullet's two field
  > classes, ADR 0002 §1 / ADR 0005) is preserved, though its DB column is renamed `footer_json` → `protocol_json` (ADR 0009); only the *rendered wire footer* is replaced. Body +
  > `body_hash` + FTS stay pure (the header is wire-only, never persisted/indexed as content).
- **F3 — Retrieval (powerful + fast, for agents).** Full **hybrid from v1** (no phased downgrade):
  **FTS5 BM25** (exact tokens — paths, symbols, errors, mids, branches) + **local multilingual
  embeddings** (bge-m3 / multilingual-e5-small, on-device) via **sqlite-vec** + **RRF** fusion.
  Metadata **prefilters** (workspace/tab/conversation/agent/branch/time/type) before ranking.
  In-process SQLite, no service, no network. `zynk query <text> [--workspace|--conversation|--agent|
  --since|--limit]` → ranked results + provenance. (Short messages → 1 message = 1 embedding unit,
  no chunking.)
  **Embedding is ASYNC — send NEVER blocks on the model.** On send, the message is persisted and
  FTS-indexed **immediately** (so it is keyword-searchable at once); the embedding is computed by a
  background worker (`embedding_jobs` with `pending|running|done|failed` + retry). **Freshness
  contract:** FTS results are always fresh; vector results are eventually-consistent (a just-sent
  message may be FTS-hit before its vector lands). The model is kept warm; a job failure never blocks
  or fails the send.
- **F4 — Clear structured responses (CLI-WIDE).** NO silent success, no bare `ok`. Every command
  returns a clear structured response. **Response contract (agent-facing):** `result` (ok|failed),
  `command`, relevant `ids`, `target_resolution`, `status`, `proof`/`receipt` state, `next` (what the
  agent can/should do next). **Dual-mode:** stable JSON for automation + concise explicit human text.
  Failure → `{code, message, context}` with `result:"failed"`. Rationale: a new agent must NOT infer
  zynk semantics from empty stdout + exit code.

## 4. Command surface & the message-layer

> **STATUS (ADR 0007 §1–§2, M6):** the produced binary is now `zynk` (the crate/package stays `zynk`
> internally for upstream-merge survivability). In addition to the message-layer send commands below, M6
> promotes a native top-level verb surface — `zynk send` / `zynk reply` (transport reuse; `reply` has **no**
> `--reply-to`, parent auto-derived) and the read-only `zynk thread` / `zynk inbox` / `zynk query` plus the
> live-socket `zynk whoami` / `zynk who`. All return the F4 envelope; no invariant is weakened (receipt
> server-authoritative only; read paths write zero delivery events). The legacy `zynk query` /
> `zynk message-received` subcommand group is retained transitionally for back-compat.

zynk inherits ALL zynk commands (rebranded). The zynk **message-layer** (`--type` + auto-header +
auto-persist + delivery-tracking) is applied **UNIFORMLY across every text-send command**, not
special to one:

```
zynk pane run       <pane>   <text> [--type <t>]
zynk pane send-text <pane>   <text> [--type <t>]
zynk agent send     <target> <text> [--type <t>]
zynk agent prompt   <name>   <text> [--type <t>] [--trace <id|inherit>] [--wait] [--timeout <ms>]
```

- Agent supplies the positional `text` (+ optional `--type`). NO `--reply-to` (parent derived from
  target + `conversation_seq`).
- System auto-fills + persists: from/to, workspace, tab, branch, cwd, timestamps, protocol metadata, delivery —
  uniform regardless of which send command.
- **Submit semantics per command — `delivery_status` reflects them honestly (resolves the P1 ambiguity):**
  - `zynk agent send` → a **message**: zynk resolves `target` → its pane and **SUBMITS** via
    `pane.send_input` (atomic). It does NOT inherit zynk's raw `agent send` literal-no-Enter behavior
    (that was the stuck-draft bug). → `delivery_status = submitted`.
  - `zynk pane run` → atomic submit (`pane.send_input`). → `delivery_status = submitted`.
  - `zynk agent prompt` → persist the pure body and resolved Party, require the same terminal at
    dispatch, then submit only when the named agent is ready and is still the pane's foreground
    process. A verified response records `delivery_status = submitted` with
    `proof_source = agent.prompt` before an optional wait. A wait failure exits 3 with the original
    `message_id` and submitted state, and MUST NOT resubmit or append a compensating Failed event.
    A missing or contradictory response is explicitly unverified and advises against automatic
    resubmission; it is not proof that no terminal effect occurred. Prompt readiness, status and
    sequence observations never create receiver or receipt authority.
  - `zynk pane send-text` → explicit NO Enter (deliberately staging text). → `delivery_status = drafted`
    (message persisted + protocol metadata/type, but NOT submitted; a future submit transitions it to `submitted`).
  So `submitted` (§6) is consistent: only `agent send`/`pane run`/a future submit produce it; `send-text`
  produces `drafted`. M2/F1 persists drafts but defers `pane submit` per ADR 0004.
- **Draft → submitted transition (explicit, no implicit Enter-binding):** deferred by ADR 0004 until
  zynk has exact raw-input proof or a reviewed fail-closed draft guard. The intended future command is
  `zynk pane submit <pane> [--message-id <id>]`, sending one Enter and transitioning the targeted draft
  (default: latest `drafted` message on that pane) to `submitted`. **Collision rule:** if the pane's
  current input no longer matches the draft's `body_hash`, ABORT with a structured error
  (`result:"failed"`, `code:"draft_mismatch"`) — never submit a mutated draft. (We do NOT implicitly
  bind "next Enter from the pane" to the latest draft — that races user input.)
- `pane send-keys` (raw key codes, not text) is **excluded** from the message-layer — not a message.
- New retrieval command: `zynk query …` (F3).

### Managed agents and submission boundaries

`zynk agent start <name> --kind <kind> --pane <id> [--timeout <ms>] [-- <args...>]`
launches in an existing pane and waits for `interactive_ready`. A name is reserved in both Pending
and Active phases; duplicate attempts report `agent_name_taken` with the holder terminal, while a
rename during Pending reports `agent_launch_pending`. A timeout releases the managed reservation
but keeps the launch name as the pane's manual label and does not prove that the command did not run.
The server accepts `sh`, `bash`, `dash`, `zsh`, `ksh`, `mksh`, and `fish` foreground shells on Linux.
It refuses `pwsh`, `powershell`, `csh`, `tcsh`, `elvish`, `xonsh`, `nu`, and `cmd` as unsupported
dialects; absent child PID, unavailable foreground job, and a non-shell foreground process are
separate reason-bearing refusals in the same busy admission class. PATH is the server's environment.
PID reuse, concurrent typing, and a nonempty shell edit buffer are not observable admission facts.

`AgentInfo.launch_pending`, `interactive_ready`, `agent_status`, and `state_change_seq` are
observations, not identity or receipt authority. Managed metadata never manufactures
`agent_session`; receipt admission remains the confirmed owner-coherent hook path, including its
pending-exit fence. Restored Active metadata remains Active, Pending metadata is never persisted,
and invalid managed kinds are ignored and logged. A restored presentation seed deliberately has no
detector-observation timestamp: a visible blocker cannot override restored hook state until a real
detector observation supplies both state and time. `agent.get` can reconcile a due managed transition
and mark session state dirty; `agent.list` only projects the state already reconciled by the scheduler.
Neither read method creates conversation DB rows or delivery events. Both interactive and headless
schedulers reconcile due managed transitions; the headless call is a fork-owned correction absent
from upstream `e0758c32` and upstream `v0.8.2`.

`zynk agent wait <name> [--timeout <ms>]` resolves once, pins the terminal, and completes on observed
Idle, Done, or Blocked. It does not require launch readiness or a sequence advance, so a Pending
managed agent whose detector already reports Idle can satisfy it; use `agent start` when interactive
launch readiness is required. Prompt `--wait` is stricter: completion must be later than the prompt
baseline sequence. Timeouts are checked between requests and do not bound a blocked in-flight IPC
read. A changed terminal or name fails instead of retargeting.

`pane.send_input`, and therefore `agent send`, `agent prompt`, and `pane run`, validates all keys
before mutation and enqueues one byte vector containing optional bracketed text plus encoded keys.
The empty-text/empty-keys request now enqueues one empty item; this is a behavior change from the
previous zero-item path. One queue item is not a promise of one kernel write or external consumption.
Legacy and Kitty disambiguation-only Enter encode CR; Kitty report-all Enter encodes `ESC[13u`.

Protocol remains 19 in this intermediate range. It therefore spans two incompatible `agent.start`
request shapes: the equality-only guard cannot detect that difference, and an old-shape request is
typed-refused rather than launched. The later protocol-20 port must absorb this contract change.
`AgentPromptParams.expected_terminal_id` is an optional fork wire precondition; the fork CLI always
supplies it from the same resolution that produced the persisted Party.

### B1 agent automation refinements

`agent.send_keys` is the raw automation counterpart to the fork's persisted `agent.send`: it resolves the
agent target, validates every key before mutation, sends no text, and creates no message or delivery event.
`agent.wait` accepts repeated `--until` states and otherwise defaults to Idle, Done, or Blocked. Prompt
`--wait` uses the same status vocabulary but still requires the post-submission sequence gate described above.
These four B1 wire additions (`agent.send_keys`, `agent.wait`, `agent.view.set`, and `agent.view.clear`) remain
on protocol 19 only as an uninstalled port intermediate; protocol 20 absorbs them in B2.

Prompt submission separates the encoded text from Enter and schedules Enter 300 ms later through one ordered
PTY actor command. Input queued after that command remains behind the delayed suffix. GitHub Copilot receives
a focus-gained sequence before the prompt text on that same submission. A Working target returns
`agent_working`; a Blocked target returns `agent_blocked`; neither path writes input or records Submitted.
The Codex `trust_directory` detector rule is a priority-950 `visible_blocker` over the top 20
nonempty lines. It is status evidence only: it may gate prompt admission but cannot mint identity,
`agent_session`, managed ownership, proof, or receipt authority.

Newly launched panes are polled until the same terminal exposes one of the accepted Linux foreground shells
before `agent.start` is submitted. The client never retargets during that wait. A missing local server produces
one typed `server_not_running` diagnostic with a concrete start command; protocol mismatches and typed API
errors retain their own diagnostics. `zynk --skill` prints the bundled root `SKILL.md`; release preparation
continues to use the repository's `zynk-pre-release-audit` skill.

**Send response (F4):** every send returns the persisted record + delivery state:
```json
{ "result": "ok", "command": "agent send", "message_id": "...", "conversation_id": "...",
  "conversation_seq": N,
  "from": {"agent":"claude","pane":"...","terminal_id":"...","agent_session":{...}},
  "to":   {"agent":"codex","pane":"...","terminal_id":"...","agent_session":{...}},
  "type": "review|null", "delivery_status": "submitted", "submitted_at": "<rfc3339>",
  "body_hash": "...", "next": "delivered (submitted); agent targets receive a visible Zynk header and can reply via zynk reply" }
```

> **STATUS (ADR 0009):** no `footer_rendered` (or header) field is returned. The delivered WIRE text to an
> agent target is `HEADER + "\n\n" + body` (the visible header is prepended); the persisted `body` +
> `protocol_json` are separate — the header is **wire-only**, never part of `body`. There is no auto-receipt,
> so `next` does not promise `received`.

## 5. Conversation model

- Granularity **per-tab**. Participants derived from zynk **live topology** (pane/agent list),
  not from messages.
- **Threading:** the target already encodes reply-to. `derived_parent_id` is computed at insert =
  **the latest message in this conversation whose sender is the same logical party as the resolved
  target, keyed by `agent_label`** (the stable conversation-scoped identity) — NOT by pane/terminal/
  agent_session, which rotate on agent restart or pane churn. (Participant rows may rotate; the
  threading identity is `agent_label` within the conversation.) No user-facing flag. `type=approve/
  review` targets the latest relevant item by that rule (precise because the target disambiguates).
  Replying to a non-latest old message is rare → quote in body.

## 6. Delivery / receipt model (honesty preserved, native)

> **STATUS (ADR 0009, supersedes the auto-receipt path only):** the M3b/M4 footer-driven auto-receipt is
> **removed**. A delivered/visible awareness **header is NOT receipt proof** — `delivery_status` stays
> **`submitted`** for every agent and is **never auto-promoted** to `received` by any header/footer/marker/
> screen/status observation. The server-authoritative `zynk.message_received` API + `receipt.rs` + the binding
> acceptance invariants below are **retained as a DORMANT capability** (callable, but nothing auto-fires them),
> so a future **uniform** receipt (all agents, each requiring hook-authority) stays possible. The proof
> invariant is **unchanged** — `received` is reachable only via the validated server event. Consequence: with
> pi-only footer receipt gone and no uniform receipt yet, **no agent reaches `received`** until that future
> ADR lands. (Pi's custom receiver/parser/strip is removed; Pi is Zynk state-only like every other agent —
> its live extension must be reinstalled state-only.)

Four explicit states; never collapse:
- **`drafted`** — message persisted + protocol metadata/type, text written to the pane but NOT submitted
  (`pane send-text`, no Enter). A durable, typed-only state; a future exact submit transitions it to
  `submitted` (deferred by ADR 0004). Raw input commands are not protocol "deliveries" until submitted.
- **`submitted`** — native `pane.send_input` ok (`agent send`/`pane run`; `proof_source=pane.send_input`),
  plus future explicit `zynk pane submit` once exact proof exists (`proof_source=pane.submit`). Native
  submit proof is authoritative — zynk owns the PTY.
- **`received`** — the **receiving zynk integration** reports a **message-specific** receipt via a
  native event **`zynk.message_received`** (`message_id`, `conversation_id`, `conversation_seq`,
  receiver `agent_session`, `status`, `seq`, `timestamp`). SEPARATE from `report-agent` (lifecycle/state).
- **`processed`** — optional stronger receiver/operator confirmation. (`observed` is a DEFERRED alias,
  not a distinct `event_type` yet — §7 uses `processed`.)

`pane.agent_status_changed` is **corroboration only, never receipt by itself** — a generic status
change does not identify which message caused it. **Honest fallback:** a receiver without the zynk
integration stays at `submitted` (never falsely `received`). NO marker, NO scraping.

## 7. Storage schema (global SQLite)

Durable identity: store STABLE anchors (`terminal_id`, `agent_session.value`, `git_sha`, workspace/tab
at send time). zynk compact pane ids (`w…-1`) are live-session only and MUST NOT be the durable key.
**Runtime namespace (because the DB is GLOBAL but multiple runtimes — `zynk-dev`, live, future — write
to it):** every conversation/message carries `runtime_session_id` + `socket_namespace` so a dev-test
conversation is never conflated with a live one. Participants are referenced by FK (normalized snapshot),
not denormalized onto every message.

- `conversations(id, runtime_session_id, socket_namespace, workspace_id, tab_id, topic, created_at,
  last_message_at, status, meta_json)`
- `conversation_participants(id PK, conversation_id, agent_label, pane_id, terminal_id, terminal_instance_id,
  agent_session_source, agent_session_kind, agent_session_value, joined_at, left_at)`
- `messages(id, conversation_id, conversation_seq, derived_parent_id NULL, runtime_session_id,
  socket_namespace, created_at, target_arg,
  from_participant_id FK, to_participant_id FK,
  type NULL, body, body_hash, workspace_id, tab_id, cwd, foreground_cwd, branch, git_sha,
  protocol_json, meta_json)`
  — `from_participant_id`/`to_participant_id` reference `conversation_participants` (the agent/session
  snapshot, with decomposed `source`/`kind`/`value`), instead of denormalizing `agent_session` per row.
- `delivery_events(id, message_id, event_type drafted|submitted|received|processed|failed,
  proof_source pane.send_text|pane.send_input|pane.submit|agent.prompt|integration|pane_tree|operator|system.recovery,
  zynk_event_id NULL, seq, timestamp, payload_json)`
  — migration 0005 adds `agent.prompt` by rebuilding the table and copying all eight columns
  verbatim. Migrations 0001-0004 remain immutable. A pre-0005 binary sees the resulting DB as
  `Newer`, never ready or Foreign; rollback requires a compatible binary or an operator-owned
  database backup/recovery decision, not migration-row/checksum edits or a reverse migration.
- `messages_fts` — FTS5 external-content over `body` + selected searchable metadata (written synchronously
  on insert — keyword search is always fresh)
- `embedding_models(id, local_model, dims, tokenizer_hash, created_at)`
- `embedding_jobs(id, message_id, model_id, status pending|running|done|failed, attempts, last_error,
  created_at, updated_at)` — async pipeline; send never blocks on the model (§3 F3)
- `message_embeddings(message_id, model_id, text_hash, vector)` (or sqlite-vec virtual table + mapping)

## 8. Native zynk primitives we build on

> **STATUS (ADR 0007 §5, M6):** the runtime tree is now Zynk-branded by construction. Config lives at
> `~/.config/zynk/config.toml` (separate from data); sockets/logs use `zynk.sock` / `zynk-client.sock` /
> `zynk*.log` under `~/.config/zynk[-dev]/`. User-facing override env vars are `ZYNK_*`-primary
> (`ZYNK_CONFIG_PATH` / `ZYNK_SOCKET_PATH` / `ZYNK_CLIENT_SOCKET_PATH` / `ZYNK_SESSION`) with the existing
> `ZYNK_*` retained as transitional compat aliases — when both are set, the `ZYNK_*` var wins. The data home
> is separate: `~/.zynk/` for the DB (§3 F1 / ADR 0008).

- Transport: newline-delimited JSON over `~/.config/zynk/zynk.sock` (zynk → `~/.config/zynk/…`).
- `pane.send_input` (atomic submit), `pane.send_text`/`send_keys` (no Enter).
- `pane.report_agent` (lifecycle), `pane.report_agent_session` → `agent_session{source,agent,kind:id|path,value}`,
  `pane.report_metadata` (display: title/display-agent/state-labels/tokens/ttl).
- `events.subscribe`/`events.wait` (`pane.agent_status_changed`, `workspace.*`).
- `integration install <pi|omp|claude|codex|…>` — registers agent hooks; zynk registers its own.
- zynk has NO native conversation persistence → F1/F2/F3/F4 + delivery records are 100% zynk-layer.
  (zynk's `src/persist*` is session/layout state, not messages — a pattern to learn from, not reuse.)

Public `events.wait` accepts agent-status event matches; other event matches
are refused before subscription setup. Setup errors return immediately with
the original wait request ID. During agent-status polling, only `pane_not_found`
ends the wait with an error; other poll errors continue toward a match or timeout.
If the event sequence changes during a pane snapshot, its result, including an
error, is discarded and re-derived on the next stable poll. Ordinary subscription
polling continues to suppress snapshot errors. The internal `pane_get` helper
decodes the existing `ErrorResponse` and preserves its remote ID; the wait path
then rebinds that ID to the original wait request. These are separate contracts,
not a new error envelope or wire method. Read-only waits create no new delivery
events. The binary client protocol remains 19.

Dedicated `custom_status` and `clear_custom_status` presentation are retired. JSON request decoding
keeps its existing unknown-field policy: retired keys are ignored, even in otherwise valid mixed
reports, with no alias or ignored-key signal in the legacy contentless `Ok {}` API response.
Retired-only metadata reports fail the existing missing-field check (`invalid_metadata_request`).
CLI `--custom-status` and `--clear-custom-status` fail as unknown arguments before a socket request.
JSON projections and agent-status events omit the retired member; the binary client-frame protocol
stays at version 19. This does not change F4's separate CLI command-envelope contract. Explicit
display tokens such as `task`, opted into as `$task` in desktop agent rows, replace user-chosen text;
they do not migrate old values or acquire lifecycle, hook, replay or receipt authority. Mobile and
navigator details and the agent switcher have no automatic token alias. See the README migration
section for the three caller experiences and token reporting limits.

Successful `server.live_handoff` sends its response, waits for completion of the
ordinary socket writer's existing write attempt, then sets the old server's final
exit flags. The internal completion carrier is neither a JSON field nor a native
message receipt: it proves no peer read or parse. A disconnected writer or the
six-second channel receive timeout permits shutdown; scheduling can extend the
observed duration, so this is not a six-second wall-clock service guarantee.
Internal dispatch without a socket waiter does not wait. Failed handoff retains
its existing error path, and caller identity, ownership transfer, rollback,
delivery status and `message_received` admission are unchanged.

Incomplete full SGR mouse prefixes (`ESC[<` followed by ASCII digits or
semicolons, including an empty suffix) receive a 150-ms first reassembly poll
when client mouse capture is active. The existing lone-Escape alternative is
unchanged. The legacy reader extends only full prefixes; lone Escape stays at
10 ms there. The new full-prefix alternative is ineligible inside an existing
discard family, including OSC/ST payloads. The second poll remains 10 ms;
these policies do not establish a real delayed-reader trace or latency SLA.

A timed-out full prefix is dropped and arms mouse-tail discard only when its
length is at most 128 bytes, counting that prefix in the budget. Exactly 128
arms until the next drain call, including an empty push; longer prefixes are
dropped without arming. The existing orphan path after an emitted Escape keeps
its zero-based continuation budget. Both use the fork's immediate budget
termination and inspected-only removal, retaining uninspected same-buffer
surplus. A further quiet flush clears mouse discard state for both origins;
later tails become ordinary input. This intentionally changes the old orphan
lifetime without discharging its carried origin. Host-reply CSI discard keeps
its separate behavior. The new full-prefix entry clears its spent Escape hold
without cancelling outstanding host replies; it does not repair the separate
parent generic-drop spent-hold condition recorded in the patch ledger.

### Experimental pane graphics

`pane.graphics.set`, `pane.graphics.clear`, `pane.graphics.info`, and
`pane.graphics.stream` are public socket methods gated by the existing
`experimental.kitty_graphics` flag, default false. Reusing an existing true value
widens its capability from painting to upload admission without a separate
consent step. The acceptance depends on owner-only socket permissions (currently
0600), not hostile-plugin isolation. The existing socket permission test checks
application of the constant, not a literal 0600 policy; the handoff listener
has its own hardcoded 0600. Any permission change must revisit both paths and
this capability acceptance.

After successful decoding, disabled public methods and internal open/set return
`feature_disabled` before target or payload processing. Malformed requests use
the existing earlier `invalid_request` decoder path. Internal close/cancellation
remain available after disable. No existing caller authority set is widened.

Set requires pane ID, format (`png`, `rgb`, `rgba`), nonzero width/height and
nonempty data. Public base64 is checked for encoded length before a bounded
decode, with at most 512 KiB decoded data. Over-limit data may return
`image_too_large` before malformed-encoding diagnosis; invalid dimensions,
encoding or RGB/RGBA length use `invalid_image`. PNG content remains opaque.
Set replaces one layer; clear removes it or succeeds when absent. Placement
defaults to viewport (0,0) and the pane's inner grid dimensions when either
grid dimension is zero, with clipping to the pane. Info returns
`pane_graphics_info` and positive cell pixel dimensions or
`cell_size_unavailable`; available foreground-client hints can be the existing
8x16 fallback, not physical measurement. Set/clear use the existing `ok` envelope.

Stream begins with an ordinary public Request and one open `ok`. Following frames
use an LF-terminated JSON header plus exactly `data_length` raw bytes. Headers
include format, image dimensions, length and optional placement. The header cap
is 64 KiB including LF; the nonempty body cap is 16 MiB, checked before allocation
and again at mutation. Body reads use 64 KiB chunks. Header timing starts with
its first byte; header/body idle and total limits are 5 and 30 seconds. An idle
connection awaiting its first header has no header deadline. These are configured
receive bounds, not scheduling-independent completion guarantees. Successful
frames do not receive individual `ok` replies. Frame errors end the stream.
Public Stream remains omitted from generated schema discovery (M832-G1-N1).

The dedicated stream's timed-read wrapper does not reset the socket mode when
the read returns its terminal no-data result (`None`). A successful value resets
the mode and propagates a reset error; a read error attempts reset while retaining
the original read error regardless of whether reset succeeds. These terminal streams
are not reused after `None`.
Linux setup errors, including `InvalidInput`, remain errors. The existing
`Unsupported` fallback uses nonblocking polling; this does not change framing,
payload caps, deadlines or cancellation policy.

Internal Open/Set/Close Method variants are skipped by both serde and schemars;
attempted serialization of such a typed variant intentionally fails. Transport
sends them only through the typed App request channel. Public owner/data fields
are ignored and default empty/None, independently of method exclusion. The
server supplies owner and raw bytes, preserves the accepted ApiCaller, resolves
the pane once, and validates the active matching owner on subsequent frames.
An active claim makes public set/clear and a second open return `stream_conflict`.
An abandoned queued open returns `stream_closed` rather than deleting a later
static layer. Unknown close is idempotent; stale close cannot remove another
owner. Runtime registrations are App-local weak tokens, never pure-state handles.

Pane/tab/workspace removal and feature disable clear relevant state. Finite
request/event synchronization and pre-wait/render synchronization invalidate
claims and propagate changed cleanup to Full rendering; re-enable resurrects
nothing. Layers, claims, revision and cell hints are session-local, absent from
persisted and handoff schemas. Successful handoff loses all streams/layers and
requires reconnect/resend, without a guaranteed final `stream_closed`. Early
handoff failure differs from listener rollback replacing ServerHandle; no stream
continuity guarantee applies after replacement. No graphics handoff experiment
or native receipt guarantee is implied by these source contracts.

Layer collection and encoding use the same B1 bounded collision resolver, with
tagged source/placement ownership, adoption before release and a retained
pre-update upload snapshot. Full UI work wins over graphics; visible PTY plus
graphics is Full, hidden/clean plus graphics can be Graphics. App-only targets
need compatible geometry and a valid semantic baseline; other cases retain
their full/raw paths. Speculative cache commits only after successful enqueue,
or for empty output with no send. A full writer lane retains the old cache and
strongest pending kind. Graphics-only work does not replace semantic baselines;
presentation-attempt cadence is not evidence of physical display.

M832-AGGREGATE-RESOURCE-BUDGET stays open: no global quota or connection cap.
With panes P, overlapping accepted/queued/retired bodies K, pending JSON requests
N and clients C, payload accounting is O(P*S + K*S + N*J + C*F + C*P), where
S=16 MiB, J=1 MiB and F=32 MiB. This is not an RSS maximum; encoding and copies
allocate before the outgoing frame check. Valid traffic can exhaust resources.
The accepted nonblocking status depends on the flag remaining default false
and experimental. Before changing either, resolve the budget or explicitly
document the limitation for the changed policy; the operator retains release
approval. Per-request tests do not establish aggregate memory safety.

### Global plugins, startup hooks, and agent views

The installed-plugin registry is global across named sessions under zynk's private config directory. Registry
and lock files are same-user regular files, capped and opened without following symlinks; updates serialize
through the lock, write a private temporary file, sync it, rename atomically, and sync the directory. A corrupt
registry is never overwritten by a mutation. `plugin link` can validate and persist a local manifest without a
running server, and live sessions refresh the shared registry before plugin operations. Tests must provide an
explicit isolated registry root; test code never falls back to the user's live config directory.

Manifest build, action, event, pane, and startup commands are nonempty argv vectors. Arguments are never
flattened into a shell string. A relative program containing `/` resolves from the plugin root while its
arguments and environment remain distinct. Startup commands are Linux-filtered, failure-isolated, capped to 32
concurrent plugin commands and 64 KiB per output stream, and bounded to 10 seconds. They run once per actual
server process after registry refresh, not during CLI-only linking, restore replay, or handoff import. Startup
logs redact command argv; one failed hook does not prevent another hook or server readiness.

`agent.view.set` installs one bounded, typed projection with recursive filters over status, workspace, tab,
pane, agent kind, seen state, state sequence, or metadata tokens, plus stable sort fields. `agent.view.clear`
is source-guarded. A view changes only sidebar/mobile projection, scroll selection, and the displayed label or
empty state; global counts and underlying agent state stay unchanged. Plugin disable/unlink clears that
plugin's view. Detection-labelled view data remains observation and never creates receiver or receipt authority.

### Modal terminal popups

`plugin.pane.open` accepts placement `popup`, optional `width` and `height`, and
the existing plugin trust/platform/entrypoint checks. A popup is session-modal,
not a tile or plugin-owned public pane. It leaves underlying focus, layout and
plugin context unchanged, has no public pane ID, and emits no pane lifecycle
events. Only one popup may exist; plugin opening requires Terminal mode and
rejects workspace, target-pane or split-direction options. `focus=false` does
not make it non-modal. Non-popup sizes are invalid rather than silently ignored.

Dimensions describe the outer rectangle: unsigned cell counts through 65535
(including zero) or canonical percent strings `"1%"` through `"100%"`. Signs and
leading zeros in percentages are rejected. Omitted dimensions default to half
the available area; small values clamp to a 6-column by 4-row minimum, then to
the available area. An area too small for that minimum is refused. Request
dimensions override manifest dimensions independently. For example:

```sh
zynk plugin pane open --plugin example.picker --entrypoint picker --placement popup --width 80% --height 20
zynk popup close
```

The plugin must already be installed and enabled with the named entrypoint.
Manifest `[[panes]]` entries may set `placement = "popup"`, `width` and `height`.
Custom commands use the same dimensions without changing the old `pane` action:

```toml
[[keys.command]]
key = "prefix+t"
type = "popup"
command = "exec \"${SHELL:-sh}\""
description = "open scratch terminal"
width = "80%"
height = "80%"
```

Popup commands receive host appearance and existing plugin/workspace context but
not `ZYNK_PANE_ID`, even when supplied in extra environment. Agent detection is
disabled. This is identity isolation, not a process or resource sandbox.
Terminal keys, text, paste and fresh mouse input go to the popup; Escape is input,
not a universal close shortcut. Existing key/mouse gestures retain their original
source and target. Closing releases forwarded keys before removing the runtime
and suppresses later repeats until the existing release/teardown policy clears
their ownership. A failed repeat does not erase its original forwarded lease.

Process exit or `{"id":"close","method":"popup.close","params":{}}` closes
the popup. Close returns `ok`, or `popup_not_open` when absent. The fork convenience
route `zynk popup close` uses that same API and compatibility guard; its help and
invalid arguments do not connect. Background removal of the opening workspace
does not itself close a popup. Popup state and runtime handles are excluded from
persisted/handoff snapshots. Successful handoff drops the popup through ordinary
runtime shutdown; early failure before that commit path does not. No guarantee
is made about arbitrary detached descendants or an untested live popup handoff.

View computation resizes popup runtimes; rendering remains read-only. Popup text
and cursor replace the tiled presentation within its rectangle. Hidden cursor,
scrollback and synchronized output suppress cursor intent without tile fallback.
Tiled OSC-8 link metadata inside the popup rectangle is removed, not merely hidden
by text clearing; outside links remain. Retained-PTY rendering declines an app
popup. App-surface graphics are suppressed through normal cache deletion and
writer acceptance, without cancelling streams. Valid hidden stream frames still
replace the latest layer; close reveals surviving current data, not frame history.
No per-frame acknowledgment, instantaneous erasure at a blocked writer, popup
image support, or aggregate resource bound is added. Direct-terminal clients
retain their separate text/frame behavior. The resource and handoff limitations
in the graphics section remain in force.

### CLI protocol compatibility

Operational CLI `send_request` and agent subscription paths obtain server status
on a separate connection and require protocol equality (currently 19), independent
of package version. Missing/malformed/unreachable ping is a transport error, not
assumed compatibility. Ordinary mismatch output is one JSON `protocol_mismatch`
error with the request ID and restart/upgrade guidance. The transport returns a
typed error and prints nothing itself, including during plugin rollback.

Native/agent/pane delivery routes retain F4: pre-resolution refusal yields
`transport_failed`, unknown target resolution, the existing generic message and
no structured mismatch context or recorded send attempt. Direct status provides
the detailed diagnostic route. A refusal after a recorded attempt reaches the
existing Failed append and F4 error; healthy-database controls observe Failed
and no Submitted/Received. The attempt itself creates no Submitted event.
Existing append failures are not made durably successful by this guard.

Direct status, explicit live handoff and direct server stop bypass the guard for
recovery; they are not invoked automatically. Low-level API and direct binary
clients remain separate. Ping and operation are not an atomic version lock:
ordinary live handoff can replace the server between them. The operation may
succeed or fail against the replacement, and a lost response is not proof of no
side effect. No retry, replay, recursion, new overall timeout or receipt authority
is introduced. Future low-level CLI routes require an explicit admission decision;
the present guard placement is not compiler-enforced route enumeration.

## 9. Fork engineering discipline

- zynk-native code in **NEW modules**: `zynk_db`, `zynk_messages`, `zynk_receipts`, `zynk_retrieval`,
  `zynk_header`, `zynk_response`. Touch upstream files ONLY at API/CLI dispatch + integration hook
  points. Maintain an explicit **fork-patch ledger** (`docs/zynk/fork-patch-ledger.md`) so
  `git merge upstream` stays survivable.
- **Rebrand: minimal** — binary/brand/docs/config/socket-path + `ZYNK_*` env (keep `ZYNK_*` compat
  during migration). Keep internal module/API names close to upstream to minimize merge cost. Avoid a
  global internal `zynk`→`zynk` rename.
- New socket methods (`zynk.message_received`, etc.) are additive and clearly fork-owned.
- **Runtime isolation for testing** (hard rule §0.2): isolation via the `app_dir_name()` rebrand
  (debug `zynk-dev` / release `zynk`) → config/state/socket tree relocates off `~/.config/zynk` by
  construction, + an isolated `CARGO_TARGET_DIR`, + the fail-closed preflight. (No binary rename in M0;
  broad `ZYNK_*` explicit-override env aliasing is a later complete rebrand task.)

### Test-only paint evidence

The monolithic paint regression may retain its evaluated outer-PTY capture when
`ZYNK_TEST_PAINT_EVIDENCE_ROOT` names an existing canonical, current-user-owned
mode-0700 directory. The isolated verification runner supplies a fresh on-disk
root explicitly after clearing inherited `ZYNK_*` variables. An unset input
means `NOT_REQUESTED`; invalid input or a returned storage error means
`RETENTION_FAILED`, with no temporary-directory fallback. This input is read
only in test code, not a production or debug-build configuration seam.

The reader retains ordered chunk endpoints, its state and a metadata-completeness
flag alongside the original bytes. It still appends before checking the two-MiB
cap, permitting one 8192-byte read of overshoot, and records at most 4096 chunk
endpoints. Metadata loss does not discard raw bytes. At either instrumented
failure site, the exact snapshot used by the assertion supplies the digest:
emit `MPD_EVALUATED`, attempt retention, emit `MPD_RETENTION`, then execute the
unchanged assertion or panic. There is no post-panic snapshot. Returned diagnostic
errors do not replace the original panic; blocked synchronous storage, abort and
OOM are outside that guarantee. Metadata bookkeeping may affect test timing.

A successful receipt binds private `raw.bin`, `manifest.json` and `complete.json`
files, including geometry 106x34, trigger, watermark and nullable clean length.
Files and directories are synced before success; the completion file alone does
not prove the final directory sync. The evidence root is outside fixture cleanup.
The cooperating single-writer assumption does not defend against hostile
same-UID path replacement. Raw evidence is binary terminal output, not text to
print directly into a terminal.

The test-only `ZYNK_TEST_PAINT_REPLAY` input names a private bounded JSON request
with `artifact_directory` and the independently supplied `evaluated_sha256`.
Explicit invalid input fails instead of falling back to the driver's always-run
synthetic control. An external replay requires `MPD_REPLAY` with `requested=true`
and matching input/artifact digests, not merely a passing driver test.

Replay uses the vendored parser and fallible viewport, active-screen and
synchronized-output queries. Zero scrollback is a declared outer-terminal
observer parameter, not the inner panes' configuration. Reader chunk endpoints
are samples, not displayed frames. A separate byte-prefix pass is capped at
65536 observations; legacy regex observations at chunk endpoints have a 64-MiB
cumulative prefix budget. Both passes record actual submitted-byte counts and
hashes. Geometry, ordering and artifact integrity are checked before accepting
completion; constructor/query errors, capture cap/error, missing metadata and
budget exhaustion retain positive observations but make results indeterminate.

Facts are independent: `RAW_LITERAL_ABSENT`,
`NO_CELL_MATCH_AT_OBSERVED_PREFIXES`, `CELL_MATCH_OBSERVED`,
`LATER_CELL_ABSENCE`, `CELL_REGEX_DISAGREEMENT` and `INDETERMINATE`.
Disagreement means cells match while the same-prefix legacy regex does not;
the watermark applies only to its cleaned-text predicate. Neither raw absence
nor sampled absence establishes never-painted output. Later absence does not
distinguish clearing, scrolling or a screen switch. `READING` and even `EOF` do
not establish that every intended producer byte was captured. These prospective
observations do not recover or explain the earlier unretained paint failure.

## 10. Open / deferred (named)

- Embedding model final pick (bge-m3 vs multilingual-e5-small) — bench on-device at impl.
- ANN (HNSW via usearch/hnsw_rs) only if brute-force sqlite-vec slows at scale (>~1M messages).
- `processed` stronger-confirmation state — define when an integration/operator supplies it.
  (`observed` is a DEFERRED alias, NOT a second `event_type` — see §6.)
- Exact `zynk.message_received` integration handshake per agent (claude/codex/pi) — impl detail.
- Production binary cutover: retire the frozen `zynk` v1.5.1 wrapper binary when the fork installs `zynk`.

## 11. Process / milestones (next)

Phase 1 (this doc) → ADRs in `docs/zynk/decisions/` (rebrand strategy; zynk-layer architecture;
message/DB/delivery model; runtime isolation) → writing-plans → subagent-driven implement (Opus) →
decorrelated review (Codex/Claude, peer-first) → operator gate per milestone. Roles assigned by the
operator per cycle. NO publish/push before full local test + operator gate (§0.1).
