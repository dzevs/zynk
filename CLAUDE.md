# CLAUDE.md

Build needs Rust (stable) + **Zig 0.15.2**; the TS asset test needs Bun. Co-author/reviewer agents (Codex,
Pi, swarm) read `AGENTS.md`; the gated dev/release flow is `WORKFLOW.md`. Deep design law: `docs/zynk/` (SPEC +
ADRs `decisions/`); **ADR 0010 (full fork, no upstream-merge survivability) is binding** — disregard older
ADR/ledger passages about minimal-rebrand or upstream-merge survivability.

When another agent sends you a message via zynk, **reply through zynk** (`zynk reply` / `zynk send`) — never
in the chat; a chat reply never reaches them.

## Commands (`just`)

- **Test:** `just test` (nextest + 6 maintenance unittests). One: `just test-one <filter>`. TS: `just test-ts` (Bun).
- **Lint:** `just lint` = `cargo fmt --check` + `cargo clippy --all-targets --locked -- -D warnings` (dead code fails the lint gate).
- **Check:** `just check` (= `ci` + maintenance unittests) — run before committing; never bypass a failing check.
- **Gates:** `just gate` (tracked-path + scrub + gitleaks). **Build:** `just build`. **Hooks:** `just install-hooks` (once per checkout).

## Build

- `build.rs` runs `zig build` against `vendor/libghostty-vt/` (static VT lib, linked `extern "C"`). `ZIG` env overrides the binary; `LIBGHOSTTY_VT_OPTIMIZE` (default `ReleaseFast`) etc. tune it. Zig emits `zig-out`/`.zig-cache` under **`OUT_DIR`** — never into `vendor/`, because `cargo publish` rejects a build that mutates package source. `build.rs` early-returns when `DOCS_RS` is set (no Zig/network).
- **Vendor patches:** `vendor/libghostty-vt.vendor.json` pins the source commit; `vendor/libghostty-vt.patches.md` is the patch index. `scripts.test_vendor_libghostty_vt` enforces index ↔ files. Drop a patch only when the vendored commit has the upstream fix AND `zig build test-lib-vt` passes.
- **sqlite-vec (ADR 0006):** `vec0` is statically compiled in + registered via `sqlite3_auto_extension` before any sqlx connection. This works ONLY because Cargo unifies to exactly one `libsqlite3-sys` node — a second one breaks it with no error (`no such module: vec0`). vec0 tables are created **on first use at runtime, never in a migration**.

## Architecture invariants (what · file · why)

- **State ≠ runtime.** `AppState` (`src/app/state.rs`) / `PaneState` (`src/pane/state.rs`) are pure data — no PTYs/async/channels; the live terminal is `PaneRuntime` (`src/pane.rs`). WHY: workspace/conversation logic is unit-testable via `AppState::test_new()` / `Workspace::test_new()` (no channels/PTYs). Push runtime concerns into `*State` and tests need real terminals.
- **Render is pure; `compute_view` mutates.** `compute_view(&mut AppState)` (`src/ui.rs`) does geometry + mutation; `render(&AppState, frame)` only draws — the shared borrow forbids mutating during draw. Keep it that way.
- **Detection reads a screen SNAPSHOT only.** Entry `detect_agent_with_osc` (`src/detect/mod.rs`) → `manifest::detect_with_osc`; input is a bottom-of-buffer tail string + OSC title/progress (`src/detect/manifest.rs`). NEVER the parser, viewport, or scrollable user viewport (it holds stale/replayed agent text → false states). Manifests (`src/detect/manifests/*.toml`) match **bounded regions** (`osc_title`, `prompt_box_body`, `bottom_non_empty_lines(N)`, …) with explicit `all`/`any`/`not` gates + `priority` — never incidental whole-pane text.
- **Identity is hook-authoritative, never detection-derived.** Resolve from `terminal.hook_authority` only, never `effective_agent_label()`'s `detected_agent` fallback (`src/app/creation.rs`). Pane-list `agent_session.source` is ephemeral hook state — verify identity against DB `conversation_participants` + `who --json`, not the pane list.
- **Platform code is isolated** behind a typed boundary (`src/platform/mod.rs`: `capabilities()`, `ForegroundJob`, `Signal`); impls in `linux.rs`/`macos.rs`/`windows.rs`/`fallback.rs`. Add OS behavior there, not via scattered `#[cfg(target_os)]`.
- **Conversation layer is fork-owned + additive** (`src/zynk/`: `db`, `db_cutover`, `message`, `header`, `receipt`, `inbox`, `retrieval/`, `embed/`, `runtime`, `skill`). `src/persist/` is session/layout/snapshot state, **NOT messages**. Touch upstream files minimally — only at API/CLI dispatch + integration-hook points — and log every touch in `docs/zynk/fork-patch-ledger.md` (append-only).

## Module map (roles that surprise)

- `src/api/` — the socket **command schema + JSON dispatch** layer. `schema.rs` `enum Method` is the wire contract (`#[serde(tag="method", content="params")]`; each `serde(rename="…")` **is the wire ID**). Defines + transports methods; **doesn't** run them.
- `src/app/api/` — the App-side handlers that mutate `AppState` per `Method` (`panes.rs`, `workspaces.rs`, …, fork-owned `zynk.rs`). Where socket commands become state changes.
- `src/server/` — headless lifecycle (`headless.rs::run_server` binds `zynk.sock` JSON + `zynk-client.sock` binary, inits state/PTYs, renders to an in-memory buffer, streams frames; installs the App-owned receipt worker). `handoff.rs` = live server replacement (Unix).
- `src/ipc.rs` — low-level Unix-socket primitives (`interprocess`): connect/bind, `SocketFileIdentity`, perms/cleanup. Transport plumbing, not the command layer.
- `src/protocol/` — the **binary** client frame protocol (`wire.rs`) + `render_ansi.rs`. Distinct from the `src/api/` JSON method protocol.
- `src/cli/` — hand-rolled positional dispatcher (NOT clap); `cli/zynk.rs` is the `zynk` subcommand group (transport shim, never the receipt authority). `src/pty/` + `src/terminal/` = PTY actor + emulator/screen state feeding `compute_view`/detection.

## Rules and antipatterns (NEVER/ALWAYS · why)

- **NEVER** feed detection the parser/viewport/scrollable viewport; **NEVER** match incidental whole-pane text (use the tightest region). **NEVER** use detection-tainted identity (`to.agent`/`effective_agent_label`/`detected_agent`) for receipt/awareness — gate on hook-authoritative `agent_session`/`hook_authority`.
- **No `unwrap()` in production**; `tracing` for logging. The header renderer must **never panic on a sparse party** — missing cwd/agent/pane render as `-` (`src/zynk/header.rs::or_dash`). Dead code fails `clippy -D warnings` → targeted `#![allow(dead_code)]` **with a justifying comment**.
- **Body purity is sacred.** `messages.body` + `body_hash` + FTS store the **pure body only**. The visible awareness header (ADR 0009), structured protocol IDs (`protocol_json`), and `trace_id` (`meta_json`) are **wire-only/sidecar — never in body/body_hash/FTS**. WHY: a polluted `body_hash ≠ sha256(body)` fails receipt correlation on every message.
- **Submit ≠ receipt.** `delivery_status ∈ {drafted, submitted, received, processed}` never auto-promotes; only the server-validated `zynk.message_received` event reaches `received` (ADR 0002/0009). `agent send`/`pane run` (atomic send + Enter) → `submitted`; `pane send-text` (no Enter) → `drafted`. A resolved-then-failed submit → `transport_failed`, never `submitted`.
- **F4 envelope on every command** — no silent `ok`, no bare exit code: `{result, command, ids, target_resolution, status, proof/receipt, next}`; failure → `{code, message, context}`.
- **Read paths write zero delivery events** — `thread`/`inbox`/`query` open via `db::open_query_readonly` (`PRAGMA query_only=1`); a query must never synthesize state.
- **Durable keys must be stable anchors** (`terminal_id`, `agent_session.value`, `git_sha`, `agent_label`), never live compact pane ids (`w…-1`, which rotate on restart). Threading `derived_parent_id` is keyed by `agent_label`.

## Testing

- **Hermetic** — each test spawns its own temp config/socket, so `just test` is safe to run directly. Pure state via `AppState::test_new()` / `Workspace::test_new()`; `PaneRuntime` has a `#[cfg(test)] TestChannel` so panes run without a PTY. All tests use the std-only deterministic `FakeEmbedder` and **must not touch the network** (real `fastembed` is behind a feature, absent from the default graph).
- **Isolated dev runtime is MANDATORY** — never the live socket/config (`~/.config/zynk/`) and never the default `CARGO_TARGET_DIR` (the machine runs `cargo-watch` on it). Set an isolated `CARGO_TARGET_DIR` + isolated `ZYNK_SOCKET_PATH`/`ZYNK_HOME`/`ZYNK_SQLITE_HOME` (and `XDG_*`); `zynk status` shows the resolved runtime paths — confirm none is a live default before running. DB tests plant a fake DB in a temp `ZYNK_SQLITE_HOME` — never touch `~/.zynk/zynk.db`.
- **Maintenance unittests (Python, in `just test`/`check`):** `test_agent_detection_manifest_check`, `test_vendor_libghostty_vt`, `test_conventional_commits`, `test_check_public_tree`, `test_gitleaks_config`, `test_scrub_check`.
- **Characterization/parity tests are REQUIRED for:** wire IDs (`Method` `serde(rename)` — breaks clients), protocol-ID field set (`header::protocol_id_fields` ↔ persisted `protocol_json`), the delivery-transition matrix (only `submitted→received`), receipt invariants, integration-asset version parity (`PI_INTEGRATION_VERSION` ↔ the `// ZYNK_INTEGRATION_VERSION=N` asset marker), and FTS/body purity.

## Gotchas

- **Foreign-DB fail-closed (ADR 0008).** Native DB = `$ZYNK_HOME/zynk.db` (default `~/.zynk/zynk.db`); config separate at `~/.config/zynk/config.toml`. `db::classify_db_at` classifies any existing file; a non-empty unrecognized DB is **`Foreign` → FAIL CLOSED**, never auto-migrate/overwrite. The only cutover is the explicit non-destructive `zynk db status|adopt|backup|import`.
- **Editing a migration in place re-checksums it and WIPES existing DBs at next init.** ADR 0010 accepted this (DB disposable, no external consumers). Don't expect data to survive a `migrations/zynk/0001_*` edit.
- **`trace_id` lives in `messages.meta_json` only** (indexed by `migrations/zynk/0003`), never in body/body_hash/protocol_json/FTS.
- **Global DB shared across runtimes** — every row carries `runtime_session_id` + `socket_namespace` so dev-test conversations never conflate with live; the runtime-id file sits beside the active API socket.
- **`Method` is exhaustively matched** in `api_method_name` + dispatch — a new variant is compiler-forced everywhere; changing a `serde(rename)` breaks clients.
- **Remote manifests gate on `min_engine_version`** vs `MANIFEST_ENGINE_VERSION` (currently 2, `src/detect/manifest_update.rs`) — bumping engine semantics requires bumping the const.
- **Env + labels:** `ZYNK_*` is the sole env surface; source/event labels are `zynk:<agent>`. Don't reintroduce the upstream name `herdr` in active source/docs (ADR 0010 §7 CI gate) — allowed ONLY in `NOTICE`/`LICENSE` attribution + frozen historical records (accepted ADRs, ledger).

## Skills (`.claude/skills/`)

Claude's implementer skills: `smoke-test`, `zynk-docs`, `pr-explainer`, `pr-splitter`, `debugging-difficult-bugs`. The co-authors' workflow set is `.agents/skills/` (separate; each agent uses its own dir).
