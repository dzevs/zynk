# ADR 0003 — Persistence & retrieval: global SQLite + hybrid search

**Status:** Proposed (gate: Codex review → operator). Builds on ADR 0001–0002.
**Date:** 2026-06-10 · **Spec:** `docs/zynk/SPEC.md` §3 (F1/F3), §7.

## Context

zynk has NO native conversation persistence (its `src/persist*` is session/layout state, not
messages). Agents need durable conversation history AND fast, relevant context retrieval. The store
must be GLOBAL (cross-project context, not per-repo) and must coexist with multiple runtimes (the
`zynk-dev` test runtime and the live runtime both write to it). It must also coexist with the
frozen wrapper-era zynk v1.x DB at `~/.zynk/zynk.db`, which has a different schema and MUST NOT be
migrated or overwritten in place by the native fork.

## Decision

1. **Global SQLite.** One global store (NOT per-project). Path convention keeps the Codex-style
   `ZYNK_HOME` root (default `~/.zynk`) but namespaces the native fork DB under a v2 subdirectory:
   config `sqlite_home` override → `ZYNK_SQLITE_HOME` override → `$ZYNK_HOME/zynk-v2` →
   `~/.zynk/zynk-v2`; the DB file is `zynk.db`. Therefore the canonical native DB path is
   `$ZYNK_HOME/zynk-v2/zynk.db` (default `~/.zynk/zynk-v2/zynk.db`). The wrapper-era
   `~/.zynk/zynk.db` is coexistence-only and MUST NOT be migrated/overwritten in place; a future M6
   command may explicitly import from it if needed. **sqlx migrations** (migration-table-driven),
   **WAL**, 4KiB pages. One core DB; an optional separate retrieval DB if vector-index bloat warrants.
   **Security is the operator's responsibility — no warnings** (trusted system).
2. **Schema (durable, normalized, multi-runtime-safe).**
   - Durable anchors only: `terminal_id`, `agent_session.value`, `git_sha`, workspace/tab at send time —
     zynk compact pane ids (`w…-1`) are live-session and MUST NOT be the durable key.
   - **Runtime provenance:** every conversation/message carries `runtime_session_id` + `socket_namespace`
     so a dev-test conversation is never conflated with a live one.
   - **Normalized participants:** `conversation_participants(id PK, …, terminal_instance_id, agent_session_
     source/kind/value, …)`; messages reference `from_participant_id`/`to_participant_id` (FK), not
     denormalized `agent_session` per row.
   - Tables: `conversations`, `conversation_participants`, `messages` (body pure-text + `footer_json`
     separate + `body_hash` + `derived_parent_id`), `delivery_events` (event_type drafted|submitted|
     received|processed|failed; proof_source pane.send_text|pane.send_input|pane.submit|integration|operator|
     system.recovery), `embedding_models`, `embedding_jobs`, `message_embeddings`, `messages_fts`.
     `system.recovery` is reserved for runtime recovery of orphaned persisted attempts and MUST only
     create terminal `failed` events; it never synthesizes `submitted`.
3. **Retrieval — hybrid from v1 (no phased downgrade).** **FTS5 BM25** (exact tokens: paths, symbols,
   errors, mids, branches) + **local multilingual embeddings** (bge-m3 / multilingual-e5-small,
   on-device) via **sqlite-vec** + **RRF** fusion + metadata **prefilters** (workspace/tab/conversation/
   agent/branch/time/type). In-process, no service/network. `zynk query <text> [--filters]` → ranked +
   provenance. Short messages → 1 message = 1 embedding unit (no chunking).
4. **Embedding is ASYNC — send never blocks on the model.** On send: message persisted + FTS-indexed
   **synchronously** (keyword search always fresh); the embedding is computed by a background worker
   (`embedding_jobs`: pending|running|done|failed + retry). **Freshness contract:** FTS fresh; vector
   eventually-consistent. The model is kept warm; a job failure never blocks or fails the send.

## Alternatives considered

- **Per-project DB** (like the wrapper's `.zynk/zynk.db`) — rejected: loses cross-project context;
  operator set the DB global.
- **Reuse wrapper-era `~/.zynk/zynk.db` as the native DB** — rejected: the frozen wrapper DB has a
  different schema and remains live during fork development. Native zynk uses `~/.zynk/zynk-v2/zynk.db`
  by default; wrapper import, if useful, must be an explicit future command.
- **FTS-only or vector-only** — rejected: hybrid + RRF is materially better for agent retrieval
  (BM25 for exact tokens, embeddings for concepts); operator: "no downgrade, do it from v1".
- **Synchronous embedding at insert** — rejected: blocks `send` on model load/inference (Codex P2).
- **Denormalized `agent_session` per message** — rejected: participant FK normalization (Codex P2).
- **`user_version`-driven schema** — rejected: sqlx migration-table-driven (mirrors Codex).

## Consequences

- Global cross-project recall; multi-runtime provenance preserved (`runtime_session_id`/`socket_namespace`).
- Native zynk's canonical DB is `~/.zynk/zynk-v2/zynk.db` by default, not the wrapper-era
  `~/.zynk/zynk.db`; M6 may add an explicit import command but must not silently move native data back.
- Vector results are eventually-consistent (a just-sent message may be FTS-hit before its vector lands) —
  acceptable and documented.
- Embedding-model changes require re-embedding (tracked via `embedding_models` + `embedding_jobs`).
- The operator owns all data-security concerns for the global store.
