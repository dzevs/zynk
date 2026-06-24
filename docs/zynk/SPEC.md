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
  proof_source pane.send_text|pane.send_input|pane.submit|integration|operator|system.recovery,
  zynk_event_id NULL, seq, timestamp, payload_json)`
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
  `pane.report_metadata` (display: title/display-agent/custom-status/state-labels/ttl).
- `events.subscribe`/`events.wait` (`pane.agent_status_changed`, `workspace.*`).
- `integration install <pi|omp|claude|codex|…>` — registers agent hooks; zynk registers its own.
- zynk has NO native conversation persistence → F1/F2/F3/F4 + delivery records are 100% zynk-layer.
  (zynk's `src/persist*` is session/layout state, not messages — a pattern to learn from, not reuse.)

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
