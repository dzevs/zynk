# zynk fork-patch ledger

Append-only record of every upstream (zynk) source touch made by the zynk
fork, with rationale, so the divergence from zynk stays auditable.

## M0 — Task 1: Relocate the config tree (`app_dir_name()` → zynk)

**Date:** 2026-06-10 · **Branch:** `zynk-fork`

### Core change

- `src/config/io.rs` `app_dir_name()` (fn at line 7): returned
  `"zynk-dev"` (debug) / `"zynk"` (release) → now returns `"zynk-dev"` /
  `"zynk"`.
  **Rationale (isolation):** `config_dir()`/`state_dir()` and both unix sockets
  derive from `app_dir_name()`. Rebranding it relocates the entire
  config/state/socket tree to `~/.config/zynk[-dev]` by construction, isolated
  from the live zynk at `~/.config/zynk`. No other behavior changes; socket
  and log *basenames* (`zynk.sock`, `zynk-client.sock`, `zynk.log`, …) and
  the binary name remain `zynk` (out of scope for this task).
- `src/config/io.rs` `#[cfg(test)]`: added RED-first test
  `config_dir_uses_zynk_app_name_not_zynk` (serialized via the existing
  `crate::config::test_config_env_lock()`); asserts `config_dir()` ends with
  `zynk-dev`/`zynk` and never contains `zynk`.

### Upstream test assertions updated for the rebrand

These integration tests hardcoded the old app-dir name (`zynk-dev`/`zynk`) in
the path where the running binary now writes (`zynk-dev`/`zynk`). Only the
**app-dir path component** was rebranded; socket/log/config *basenames* were
left as `zynk.*`.

- `tests/auto_detect.rs` — 3 inline `app_dir_name` helper blocks + 3
  `config_home.join("zynk")` dir/config-file literals → `zynk-dev`.
  (8 string occurrences across the socket-path + log-path assertions.)
- `tests/cli_wrapper.rs` — 1 `app_dir_name()` fn body (`zynk-dev`/`zynk` →
  `zynk-dev`/`zynk`); the fn feeds `named_session_socket` + session-dir
  assertions (2 string occurrences).
- `tests/client_mode.rs` — 1 inline `app_dir` block + 3
  `config_home.join("zynk")` dir/config literals → `zynk-dev`
  (5 string occurrences).
- `tests/multi_client.rs` — 1 `server_log_path` `app_dir` block → `zynk-dev`/
  `zynk` (2 string occurrences). See the pre-existing-interaction note below for
  the `spawn_server` config helper.
- `tests/live_handoff.rs` — `spawn_server_with_env` + two named-session spawn
  helpers + session/socket-path assertions: every **app-dir path component**
  `zynk`/`zynk-dev` → `zynk-dev` (9 path-component occurrences); `zynk.sock`
  / `zynk-client.sock` basenames preserved.

### Pre-existing interactions discovered (NOT caused by the rebrand)

1. **`multi_client::multi_client_broadcasts_frame_updates_to_all_clients` —
   FIXED (2026-06-10).**
   Pre-rebrand, `spawn_server` wrote config to `config_home/zynk/config.toml`,
   but the debug server reads the `*-dev` app dir — so that config was a **no-op**
   and the server always ran with DEFAULT config (onboarding-overlay path). The
   test passed only *incidentally*: with onboarding active, no default workspace
   is auto-created at startup, so when the test created its workspace via
   `workspace.create` the server's `active` pane was `None`, and
   `create_workspace_with_options` set it active + `Mode::Terminal` (via the
   `focus || active.is_none()` branch in `src/app/creation.rs`) — so client input
   reached the test's pane. With `onboarding=false`, `ensure_default_workspace`
   auto-creates a focused default workspace at startup, so `active.is_some()`;
   the test's `workspace.create` request (which omitted `focus`, defaulting to
   `false`) then did **not** switch focus to the test pane, and client input was
   routed to the default workspace's pane instead. The marker never appeared in
   the test's pane → the broadcast assertion broke.

   **Root cause:** the test relied on an incidental side effect (onboarding
   suppressing the default workspace) to make its created pane the active,
   input-routed one. **Fix (intentional, not a no-op):** `spawn_server` now
   writes a real `onboarding = false\n` config into the app dir the running
   binary actually reads (`zynk-dev` debug / `zynk` release, matching
   `server_log_path`), giving a deterministic non-onboarding startup; and
   `create_workspace_and_root_pane` now requests `"focus":true`, so the pane each
   test sends input to is always the active, input-routed pane regardless of any
   auto-created default workspace. All 7 `multi_client` tests pass (broadcast test
   confirmed deterministic over repeated runs). The inline preserve-hack NOTE was
   removed.

2. **`live_handoff::live_server_holds_one_pty_master_fd_per_pane` — FIXED
   (2026-06-10).**
   Fails with "replacement server … did not appear; last pids: []", and fails
   **identically on the pure zynk base `0facd81`** (last pre-zynk upstream code
   commit, before the AGPL fork attribution + zynk docs), confirmed in a throwaway
   worktree — so it is **independent of the rebrand and of our M0 changes**.

   The live handoff itself *works*: the server log shows `spawned handoff import
   server pid=…`, `handoff import ready panes=3`, `live handoff completed; old
   server exiting` — the replacement server is really spawned and adopts all 3
   panes. The failure is in the **test harness's process discovery**, not the
   product. `wait_for_replacement_server_pid` → `zynk_server_pids_for_runtime_dir`
   → `is_test_zynk_server_process` → `is_test_zynk_binary` (in
   `tests/support/mod.rs`) requires the server's `/proc/<pid>/exe` path to both
   `ends_with("target/debug/zynk")` **and** `starts_with(current_checkout_root())`
   where `current_checkout_root() = CARGO_MANIFEST_DIR`. Under the mandated
   isolated build (`CARGO_TARGET_DIR=/tmp/zynk-target`), the binary lives at
   `/tmp/zynk-target/debug/zynk`, which does **not** start with the checkout root
   `~/workspace/zynk` — so the matcher rejects **every** server process
   (including the live original answering on the socket), and discovery returns
   `[]`. With the default in-tree target (`<checkout>/target/debug/zynk`) the
   matcher would match; the test is coupled to building in the default target dir.

   **Environmental, not ours**, but blocking under the mandated isolated target.
   **Fix (`tests/support/mod.rs:607` `is_test_zynk_binary`, 2026-06-10):**
   relaxed the matcher to recognize the test server binary under the isolated
   `CARGO_TARGET_DIR` as well as the in-tree default target. The path check
   `ends_with("target/debug/zynk") && starts_with(current_checkout_root())`
   became: `ends_with("debug/zynk")` AND (`starts_with(current_checkout_root())`
   OR `starts_with($CARGO_TARGET_DIR)` when that env var is set and non-empty).
   The `ends_with` was loosened from `target/debug/zynk` to `debug/zynk`
   because under `CARGO_TARGET_DIR=/tmp/zynk-target` the binary is
   `/tmp/zynk-target/debug/zynk` (no `target` path component). This keeps the
   original in-tree behavior (that path satisfies both `ends_with("debug/zynk")`
   and `starts_with(checkout_root)`) and still rejects arbitrary/installed
   binaries — only a binary literally named `zynk` under `debug/` inside a
   target dir we control (the checkout's target or `$CARGO_TARGET_DIR`) matches.
   NOT `#[ignore]`d. **Rationale:** make zynk's server-discovery matcher work
   under the isolated `CARGO_TARGET_DIR` that zynk's hard build rule mandates.
   Target test now PASSES and the full suite is 100% green (2202 passed, 0
   failed) under the isolated target; fmt + clippy clean.

## M0 — Task 3: `zynk preflight` subcommand (wires the Task 2 guard into the CLI)

### Core change (upstream file touched: `src/main.rs`)

`src/main.rs fn main()` gains a `preflight` dispatch arm, inserted **after**
`remote::extract_remote_args(&args)` returns (so session + remote args are
already parsed) and **before** the `cli::maybe_run(&args)` call (so zynk's CLI
parser never sees `preflight` and never rejects it as an unknown command):

```rust
// zynk fork dev/test guard (ADR 0001 §6). After session+remote parse, before zynk CLI.
if args.get(1).map(String::as_str) == Some("preflight") {
    let paths = zynk::preflight::RuntimePaths::resolve();
    println!("{}", paths.render());
    if let Err(e) = paths.assert_isolated() {
        eprintln!("{e}");
        std::process::exit(2);
    }
    return Ok(());
}
```

It renders the resolved `RuntimePaths` to stdout, then `assert_isolated()`:
exit 0 when the runtime is isolated from the live zynk default and the build
target is non-default; exit 2 (with the error on stderr) when it would run
against `~/.config/zynk/*` or the default target dir.

### zynk-owned file touched: `src/zynk/preflight.rs`

Removed the two `#[allow(dead_code)]` attributes on `RuntimePaths` (struct +
impl) — it is now a live caller from `main.rs`, so clippy stays clean with no
dead-code allow.

### Test (zynk-owned: `tests/zynk_preflight.rs`)

Process-level integration test using `CARGO_BIN_EXE_zynk` (binary still named
`zynk`) with host/live env scrubbed: aborts (nonzero + "live zynk" on stderr)
when `ZYNK_SOCKET_PATH` or `ZYNK_CONFIG_PATH` point at `~/.config/zynk/*`;
succeeds with the "zynk runtime preflight" banner on an isolated debug build
(`app_dir_name` = `zynk-dev`, config dir not under `~/.config/zynk`).

Full suite 100% green (2205 passed, 0 failed) under the isolated target;
fmt + clippy clean (no dead-code allow).

## M1 — Task 4: hook the three cli send fns (honest submit + F4 response)

**Date:** 2026-06-10 · **Branch:** `zynk-fork`

The three zynk `cli` send functions now drive zynk's native submit and print
the fork-owned F4 `SendOutcome` JSON (ADR 0002 §5) on stdout instead of the bare
`ok`/`print_response`. They compose only the pure `crate::zynk::message` layer
(no new behavior in zynk's `api`/`app`): parse the optional leading `--type`,
resolve source via `ZYNK_PANE_ID`+`pane.get` and target via `agent.get`/`pane.get`,
generate a `message_id`, and on transport success record the honest
`delivery_status` (+ proof + `submitted_at`); on any failure they record the
F4 `error` and NEVER claim a delivery.

### Upstream files touched

- `src/cli/agent.rs` `agent_send` (fn previously ~line 311). **Rewrote the body**
  (the ADR 0002 honest-submit correction): instead of `Method::AgentSend` (zynk's
  literal no-Enter input), it now `resolve_target`s the agent and submits via
  `Method::PaneSendInput { pane_id, text, keys:["Enter"] }` (atomic). On `Resolved`
  + transport ok → `SendOutcome::submitted(AgentSend, …, Proof{proof_source:
  "pane.send_input"})` (exit 0); on a RESOLVED submit that fails →
  `failed(…, Resolved, SendError{code:"transport_failed"})` (exit 1).
  `NotFound` → `failed(…, NotFound, code:"target_not_found")` — zynk NORMALIZES
  zynk's server `agent_not_found` to the F4 `target_not_found`. `Ambiguous` →
  `failed(…, Ambiguous, code:"agent_target_ambiguous")`. `Unknown` (a transport
  IO error — dead/missing socket — so `resolve_target` never reached the server) →
  `failed(…, Unknown, code:"transport_failed")`; this is the honesty fix: a
  transport failure is NOT a `target_not_found` (we never asked the server) and is
  not a `resolved` (we proved nothing). The unused `AgentSendParams` import was
  dropped (the literal `agent.send` path is gone).

  **Send-something vs send-nothing (precise):** RESOLUTION failures
  (`NotFound`/`Ambiguous`) and a TRANSPORT-pre-submit failure (`Unknown`) send
  NOTHING — they abort before any submit request. A RESOLVED submit that fails is
  different: it means **NO CONFIRMED DELIVERY, not necessarily no bytes** —
  `PaneSendInput` writes the text bytes THEN the keys (`src/app/api/panes.rs`
  PaneSendInput), so a transport failure mid-submit can write the text without the
  trailing Enter. The F4 outcome (`failed`, `transport_failed`, no
  `delivery_status`/`proof`) correctly refuses to claim delivery; it does not assert
  zero bytes were written.
- `src/cli/pane.rs` `pane_send_text` (fn previously ~line 680) + `pane_run` (fn
  previously ~line 702). **Rewrote both bodies** to print the F4 `SendOutcome`
  (replacing `send_ok_request`, which printed nothing on success). `pane_send_text`
  → `Method::PaneSendText` + `proof_source:"pane.send_text"` + `Drafted`; `pane_run`
  → `Method::PaneSendInput{… keys:["Enter"]}` + `proof_source:"pane.send_input"` +
  `Submitted`. Both resolve the target party via the local helper
  `resolve_pane_target_party`, now returning `(Party, TargetResolution)` from a
  read-only `pane.get`: success → (`Party`, `Resolved`); a SERVER error code
  containing `pane_not_found` → (empty, `NotFound`); any other server error or a
  transport IO error → (empty, `Unknown`). The submit is GATED on resolution —
  `Unknown` → `failed(Unknown, code:"transport_failed")` and `NotFound`/`Ambiguous`
  → `failed(NotFound, code:"pane_not_found")`, BOTH sending NOTHING (no submit
  request issued, so no bytes reach the runtime). Only `Resolved` proceeds to
  submit; a RESOLVED submit that fails → `failed(…, Resolved,
  code:"transport_failed")` = **NO CONFIRMED DELIVERY, not necessarily no bytes**
  (text may be written without the Enter, per `PaneSendInput` above) — never
  `submitted`/`drafted`.

### Upstream test updated for the F4 request shape

- `tests/cli_wrapper.rs` `pane_run_sends_one_send_input_request_with_enter_key`
  (fn ~line 663). The old assertion required the FIRST request to be
  `pane.send_input` and FORBADE any second request. Under F4, `pane run` first
  issues a read-only `pane.get` (to populate the response's `to` party), then the
  single `pane.send_input` submit. **Updated** the mock server to collect ALL
  requests within the window and assert the honest-submit invariant directly:
  EXACTLY ONE `pane.send_input` with `pane_id=1-1` / `text="echo hello"` /
  `keys=["Enter"]` (no duplicate submit), and every other request is only the
  read-only resolution `pane.get` (never another input). Same product guarantee
  (one submit with Enter), expressed against the new request shape.

### zynk-owned files touched

- `src/zynk/message.rs`: removed the module `#![allow(dead_code)]` (the F4 surface
  is now a live caller from the three cli hooks). Added dependency-free
  `rfc3339_utc`/`now_rfc3339` (Howard Hinnant civil-from-days) for `submitted_at`,
  with unit tests. The single still-unused method `SendOutcome::human` carries a
  TARGETED `#[allow(dead_code)]` (it is part of the ADR 0002 §5 "stable JSON +
  human text" surface and unit-tested, but no non-test caller consumes it until a
  later human-readable mode wires it).
- `tests/zynk_message.rs` (new, zynk-owned): M1 integration tests — spawn an
  ISOLATED dev server under `/tmp` (own `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`/
  `ZYNK_SOCKET_PATH`; reuses `tests/support` registries/cleanup) and drive the
  `zynk` CLI binary as a subprocess: `agent send` → a real agent (registered via
  `pane.report_agent`, which sets the hook authority so `AgentGet` resolves it) is
  submitted (asserts the F4 ok JSON AND that the text rendered in the target pane);
  `agent send` to a nonexistent / ambiguous (two panes, same label) target fails
  with `target_not_found` / `agent_target_ambiguous`, exits nonzero, delivers
  nothing; `pane send-text` drafts; `pane run` submits with a sparser `to`; and a
  send with `ZYNK_PANE_ID` unset still succeeds with a sparse `from`.

Full suite 100% green (2246 passed, 0 failed) under the isolated target;
fmt + clippy clean (module dead-code allow removed; one targeted allow on `human`).

## Upstream merge d35c642 (~v0.6.9) — rebrand adaptation
- `tests/api_ping.rs:323` — upstream's new `server_reload_agent_manifests_reports_runtime_override`
  (from 36a1b7f "distribute agent detection manifests") hardcoded the override dir as
  `config_home/zynk-dev/agent-detection`. Our M0 rebrand makes `app_dir_name()` = `zynk-dev`
  (debug), so the server reads `config_home/zynk-dev/...` → test wrote to the wrong dir → reported
  `bundled` instead of `local override`. Changed the literal `"zynk-dev"` → `"zynk-dev"` to match
  the rebranded app dir. Removal/revisit: when the binary-only crate exposes `app_dir_name()` to
  integration tests (or a test helper), reference the function instead of the literal.

## Upstream merge d35c642 — Codex integration-review R1 fixes
- `AGENTS.md` "Agent Detection Updates" — upstream doc told agents to copy manifest overrides to the
  LIVE `~/.config/zynk/agent-detection/` + reload the live server, contradicting the isolation hard
  rule. Re-pointed to the isolated dev runtime (`~/.config/zynk-dev/...` via scripts/zynk-dev.sh) +
  explicit NEVER-touch-live-zynk. (Codex P1.)
- `tests/{api_ping,server_headless,detach_reattach,cross_area}.rs` — config-seed helpers wrote
  onboarding config to `config_home/zynk/config.toml` while debug app_dir_name()=zynk-dev, so the
  spawned servers never read it. Rebranded `zynk` -> `zynk-dev` (same class as the M0 multi_client
  fix). (Codex P2.)
- `scripts/zynk-dev.sh` — export GIT_CONFIG_GLOBAL=/dev/null for the test subprocess: the operator's
  global commit.gpgsign=true leaked into temp git repos created by tests (worktree/git_meta) and
  failed their commits in a clean reviewer env (Codex needed this manually). Fork commits stay signed
  (they run outside the wrapper). (Codex verification note.)
  - cross_area follow-up: rebranding the config path made the server actually READ onboarding=false,
    which (per the M0 multi_client root-cause) triggers auto-default-workspace creation that competes
    for focus. cross_area's `workspace_create` helper created its workspace WITHOUT `focus:true`, so
    input/agent-status routed to the wrong workspace (2 detach/reattach+shared-view tests failed).
    Added `"focus":true` to cross_area's `workspace_create` (the M0 multi_client pattern). 5/5 green.

## M2 — Global SQLite persistence / F1

**Date:** 2026-06-14 · **Branch:** `zynk-fork`

### Upstream files touched

- `Cargo.toml` / `Cargo.lock` — added `sqlx` (`runtime-tokio`, `sqlite`, `migrate`, `macros`) for the
  ADR 0003-approved SQLite implementation using direct `SqliteConnection` and sqlx migrations.
- `src/config/model.rs` + `src/config/io.rs` — added live `[zynk] sqlite_home` config section. This is
  the highest-precedence native DB home override before `ZYNK_SQLITE_HOME`, `ZYNK_HOME/zynk-v2`, and
  the default `~/.zynk/zynk-v2`.
- `src/server/headless.rs` — server startup now writes a per-server `runtime.id` and opens/migrates the
  native DB once, running orphan recovery at startup. This keeps `runtime_session_id` authoritative and
  avoids CLI-side recovery racing concurrent in-flight send rows.
- `src/cli/agent.rs` — `agent send` now persists a message row before native submit, appends
  `submitted` on `pane.send_input` success, appends `failed` for post-row transport failure when
  possible, and includes `conversation_id`, `conversation_seq`, `body_hash`, `runtime_session_id`, and
  `socket_namespace` in F4 responses.
- `src/cli/pane.rs` — `pane run` and `pane send-text` now use the same persistence path. `pane run`
  records `submitted`; `pane send-text` records durable `drafted`. No `pane submit` dispatch is added
  in M2 per ADR 0004.
- `scripts/zynk-dev.sh` — scrubs inherited `ZYNK_HOME`/`ZYNK_SQLITE_HOME` and live Zynk startup env
  (`ZYNK_STARTUP_CWD`), sets isolated `/tmp/zynk-sqlite-dev`, and keeps lockfile maintenance reachable
  through the wrapper while all normal commands still run locked preflight.
- `tests/zynk_message.rs` — extended the M1 send tests to assert DB rows/events/FTS freshness,
  pre-resolution no-row behavior, missing-runtime fail-closed behavior, monotonic/concurrent
  `conversation_seq`, and startup orphan recovery.
- `tests/zynk_preflight.rs` — updated process preflight tests to use an isolated `ZYNK_SQLITE_HOME`
  and added process-level DB-path rejection coverage.
- `tests/cli_wrapper.rs` — test harness commands now set per-test `ZYNK_SQLITE_HOME` derived from the
  test socket/config root, and the mock `pane run` test writes a socket-sibling `runtime.id`. This keeps
  upstream CLI tests isolated now that zynk send hooks require a runtime namespace and persistence DB.
- `tests/api_ping.rs` — `spawn_zynk_with_options` defensively removes inherited `ZYNK_STARTUP_CWD` so
  test headless servers do not create a Zynk startup workspace from the parent agent pane cwd.

### zynk-owned files added/touched

- `src/zynk/db_path.rs` — canonical DB path resolver and wrapper DB path helpers.
- `src/zynk/db.rs` — SQLite open/migrate PRAGMAs, wrapper-schema rejection, and `system.recovery`
  orphan repair.
- `src/zynk/persistence.rs` — short `BEGIN IMMEDIATE` message transactions, normalized participants,
  atomic `conversation_seq`, explicit same-transaction FTS insert, delivery event append, and M2
  transition validation.
- `src/zynk/runtime.rs` — runtime id file + socket namespace helpers. The runtime id is stored beside
  the active API socket so explicit `ZYNK_SOCKET_PATH` clients can find the matching server namespace.
- `src/zynk/message.rs` — response shape gains M2 persistence identifiers and `body_hash` helpers;
  message IDs now use `msg_` prefix, and the generic success constructor is named `ok` for both
  `submitted` and `drafted` outcomes.
- `src/zynk/preflight.rs` — `RuntimePaths` now prints/asserts `db_path`, rejects live wrapper/production
  DB paths in dev preflight, and verifies the DB parent is writable.
- `migrations/zynk/0001_global_persistence.sql` — M2 global SQLite schema with conversations,
  participants, messages, delivery events, and FTS5.
- `docs/zynk/decisions/0004-defer-pane-submit-until-exact-draft-proof.md` — records the feasibility
  spike: no exact raw-input proof/small guard in M2, so drafts remain `drafted` and `pane submit` is
  deferred.

### Rationale / removal condition

These touches are the minimal integration surface needed for ADR 0003 persistence while keeping new
logic in `src/zynk/*`. A future server-side DB actor may replace CLI-side writes, but must preserve the
same delivery-proof boundary and migration path. ADR 0004 can be superseded only by a later accepted
ADR/addendum that provides exact raw-input proof or a reviewed fail-closed draft guard for `pane submit`.

## M3a — Native receipt infrastructure (`zynk.message_received`)

**Date:** 2026-06-14 · **Branch:** `zynk-fork`

M3a adds the server-authoritative native receipt: a receiver-side integration calls
`zynk.message_received`, the server validates the ADR 0002 §Decision 4 invariants against the global
SQLite store + live App state, and exactly one valid `submitted` message advances to `received`
(`proof_source='integration'`). Per the approved M3a/M3b split, M3a injects NO footer and changes NO
send-hook text; real automatic receiver hooks + the minimal protocol-ID footer remain M3b/M4.

### Upstream files touched (edits to existing zynk files)

- `src/api/schema.rs` — added (a) `Method::ZynkMessageReceived(ZynkMessageReceivedParams)`
  (`#[serde(rename = "zynk.message_received")]`); (b) the `ZynkMessageReceivedParams` struct
  (`Eq` omitted because `receiver_agent_session: Option<serde_json::Value>`); (c)
  `ResponseResult::ZynkMessageReceived { message_id, conversation_id, conversation_seq, receipt_status,
  delivery_status, receiver_pane_id, receiver_agent_label, next }` — the structured F4 acknowledgement.
- `src/api/server.rs` — `api_method_name` gains `Method::ZynkMessageReceived(_) => "zynk.message_received"`
  (the match is exhaustive, so this is compiler-forced).
- `src/app/api.rs` — dispatch arm `Method::ZynkMessageReceived(params) => return
  self.handle_zynk_message_received(request.id, params)` (before the `not_implemented` catch-all), and
  `mod zynk;` to register the new fork-owned handler submodule.
- `src/app/creation.rs` — `terminal_agent_session_info` made `pub(crate)`; added
  `App::authoritative_receiver_identity(&self, public_pane_id)` which resolves the receiver identity
  from `terminal.hook_authority` ONLY (never `effective_agent_label()`'s `detected_agent` fallback),
  returning `None` for detection-only/non-agent panes. This is the F-CLAUDE-01 anti-detection guarantee.
- `src/app/mod.rs` — `App` gains `pub(crate) zynk_receipt_worker:
  Option<crate::zynk::receipt_worker::ReceiptWorkerHandle>` (init `None` in the single `App::new`
  struct literal; all other constructors delegate to `App::new`, so they inherit `None`).
- `src/server/headless.rs` — `run_server` spawns the App-owned receipt DB worker and installs the
  handle on `app` (`app.zynk_receipt_worker = Some(crate::zynk::receipt_worker::spawn())`) after
  `App::new`, before `HeadlessServer::new` takes ownership; `complete_shutdown` does
  `self.app.zynk_receipt_worker.take()` (ordered, before `cleanup_sockets`) to stop+join the worker.
  NOTE: only the primary `run_server` path installs the worker; `run_handoff_import_server` (a transient
  import mode) does not, so receipts on a handoff-import server return `receipt_worker_unavailable`
  until a future milestone wires it there too.
- `src/cli.rs` — `mod zynk;` + dispatch arm `"zynk" => zynk::run_zynk_command(&args[2..])?`.

### New fork-owned files added under upstream directories

- `src/app/api/zynk.rs` — the `handle_zynk_message_received` App handler: resolves the authoritative
  receiver identity, delegates the DB write to the App-owned worker with a bounded timeout, and shapes
  the F4 response / structured error codes. Includes a `request_changes_ui == false` regression test.
- `src/cli/zynk.rs` — the `zynk zynk message-received` thin socket client (always prints the JSON
  response; never writes SQLite). It is a transport shim, not the receipt authority.

### zynk-owned modules (NEW — not upstream divergence; listed for the milestone summary)

- `src/zynk/receipt.rs` — receipt request/result types, the ADR 0002 §4 acceptance invariants, and the
  idempotent `append_received_event` (one `BEGIN IMMEDIATE`; current-socket guard; message-triple +
  stored-runtime/socket match; hook-authoritative receiver-identity + self-receipt rejection;
  `already_received` no-op without a `delivery_seq` bump; routes the append through the shared validated
  path with `proof_source='integration'`). Post-restart receipts are accepted on the same socket
  namespace with a differing recording runtime (recorded in `payload_json`).
- `src/zynk/receipt_worker.rs` — the App-owned receipt DB worker: a `std::thread` owning one
  current-thread Tokio runtime + a reused native DB connection, fed by a bounded channel. Avoids the
  server-loop nested-runtime hazard and the `block_in_place` current-thread deadlock. **Enqueue is
  non-blocking** (`try_send`; a full queue → `receipt_worker_busy`, retryable) so the synchronous
  handler can never block on backpressure before the bounded `recv_timeout` even starts; `recv_timeout`
  → `receipt_result_unknown` (never a false failure). **Shutdown is non-blocking** (`try_send(Shutdown)`
  then drop the sender so the worker disconnects after draining in-flight work); dropping the handle
  joins the thread and never touches live Zynk state. (Non-blocking enqueue + shutdown = the
  **ARB-M3A-001** post-gate fix; covered by `submit_does_not_block_when_queue_is_full` +
  `submit_without_sender_is_unavailable`.) The handler also rejects a non-positive `receiver_seq` with
  `invalid_params` (plan D2).
- `src/zynk/persistence.rs` (zynk-owned, extended) — `DeliveryEventType::Received`;
  `validate_m2_transition` renamed to `validate_delivery_transition` and expanded to allow only
  `submitted → received` while defensively rejecting `none/drafted/failed/received → received`;
  `begin_send_attempt_async`, `append_delivery_event_async`, and `append_delivery_event_in_transaction`
  made `pub(crate)` so `receipt.rs` reuses the validated append path and its tests reuse the setup path.
- `src/zynk/mod.rs` — registers `pub mod receipt;` and `pub mod receipt_worker;`.

### Tests

- `tests/zynk_receipt.rs` (new, zynk-owned) — isolated-server integration tests: end-to-end receipt via
  raw socket and via the CLI shim (results match), duplicate → `already_received`, a non-hook-authority
  pane → `receiver_identity_unverified`, and `report_agent` + send alone never auto-creating a `received`
  event.
- Unit tests: `src/zynk/persistence.rs` (M3a transition matrix), `src/zynk/receipt.rs` (valid/duplicate/
  mismatch/self-receipt/draft/failed/post-restart), `src/app/api/zynk.rs` (`request_changes_ui` false).

### Rationale / removal condition

Receipt validation is server-authoritative because it needs live App state for hook-authoritative
receiver identity; the upstream touches are confined to API dispatch / handler / lifecycle boundary
points. Removing M3a code leaves M2 data and any `received` events intact (no schema migration: the M2
`delivery_events` CHECK already permits `received` + `integration`). The footer + real receiver hooks
are deferred to M3b/M4; the worker may later also be installed on the handoff-import server.

## M3b — Send-side rendered receipt footer (send hook only)

**Date:** 2026-06-14 · **Branch:** `zynk-fork`

M3b adds the SEND side of the protocol-ID footer. The structured `footer_json` is persisted uniformly
for every send (incl. `pane send-text` drafts); a rendered, delimited wire footer is appended to the
delivered text ONLY for submitted sends (`agent send`, `pane run`) AND only when the resolved target is
receipt-capable. The receipt-capable predicate gates EXCLUSIVELY on the IPC-sourced
`agent_session["agent"]` (never the detection-tainted `to.agent`), matched against the effective
allowlist = an **empty** default const ∪ `ZYNK_RECEIPT_CAPABLE_AGENTS` (env). Per ADR 0005, drafts keep
their byte-exact input text and never get a rendered footer. NO receiver-side parsing/hook and NO
`messages.body`/`body_hash`/FTS change (the footer is wire-only) — those remain M4. No M4 assets were
touched (`src/integration/assets/pi/zynk-agent-state.ts` untouched, `PI_INTEGRATION_VERSION` not
bumped, no agent added to the production const, `src/zynk/receipt_worker.rs` untouched).

### Upstream files touched (edits to existing zynk files)

- `src/cli/agent.rs` `agent_send` — added a single send-hook injection (additive, between the existing
  M2 persist/resolve block and the `Method::PaneSendInput { keys:["Enter"] }` submit): shadow `text`
  with `append_footer(text, render_footer(&record, message_type))` IFF
  `is_receipt_capable_target(&to, &effective_receipt_capable_agents())`. The persisted `body`/`body_hash`
  computed earlier stay pure; only the wire `text` changes. No other behavior touched.
- `src/cli/pane.rs` `pane_run` — the same additive injection before its `PaneSendInput` submit. `pane_run`
  is a SUBMITTED path, so it is footer-eligible; `pane_send_text` (a DRAFT) was deliberately left
  UNTOUCHED so drafts stay byte-exact (ADR 0005).

### zynk-owned files added/touched (NEW logic — not upstream divergence; listed for the milestone summary)

- `src/zynk/footer.rs` (NEW) — the generic, model-agnostic mechanism: `FOOTER_VERSION`/markers,
  `protocol_id_fields` (the shared ID set used by both the wire footer and `footer_json` so they cannot
  drift), `render_footer`, the binding D1 join `append_footer(body, footer) = body + "\n\n" + footer`
  (kept here so the receiver's future strip is the exact inverse and `body_hash = sha256(body)` can't
  fail-closed on every message), `effective_receipt_capable_agents` (empty const ∪ env, malformed env
  entries fail-closed), and `is_receipt_capable_target` (reads `agent_session["agent"]` ONLY).
- `src/zynk/persistence.rs` (extended) — `footer_json` now carries the `protocol_id_fields` (v,
  message_id, conversation_id, conversation_seq, runtime_session_id, socket_namespace, body_hash, +
  optional type) for ALL commands incl. drafts, with the existing `command` key preserved. `body`,
  `body_hash`, and the FTS row are unchanged (sentinel-tested: a body token MATCHes, the
  footer_json-only `body_hash` token does not).
- `src/zynk/mod.rs` — registers `pub mod footer;`.

### Tests

- `tests/zynk_footer.rs` (NEW, zynk-owned) — isolated-server integration tests: a receipt-capable
  `pane run` (target `agent_session.agent` in the env allowlist) renders the wire footer into the pane
  while `messages.body` stays pure and `footer_json` carries the IDs; a non-allowlisted agent and a plain
  shell pane get exact text (no footer); `pane send-text` is never rendered-footered but its `footer_json`
  still carries the IDs; and a RESERVED-NATIVE safety guard — `ZYNK_RECEIPT_CAPABLE_AGENTS=codex` injects
  the (inert) footer on send yet the receipt is still rejected `receiver_identity_unverified` (codex
  records a persisted session, never hook authority), so it is not practically receipt-capable.
  (AGENT-LABEL NOTE: a synthetic `test-agent` can never surface `agent_session` because
  `session_ref_from_report` rejects non-official source/agent pairs, so the tests use official agents
  `pi`/`kimi`/`codex` via the env override; the mechanism itself hard-codes no agent.)
- Unit tests: `src/zynk/footer.rs` (render markers/IDs, deterministic render, the byte-exact join, the
  predicate's agent_session-only gate vs detection taint, env allowlist parsing/union) and
  `src/zynk/persistence.rs` (footer_json protocol IDs uniform incl. drafts; body/FTS purity).

### Rationale / removal condition

The send hook is the minimal additive surface to attach protocol IDs to outgoing text; all new logic
lives in `src/zynk/footer.rs`. The production const allowlist is intentionally empty so NOTHING is
footered without an explicit operator `ZYNK_RECEIPT_CAPABLE_AGENTS` opt-in until M4 ships a receiver
hook. Removing M3b leaves M2/M3a data intact (no schema change; `footer_json` already existed). The
rendered wire footer for drafts and the receiver-side parse/receipt-from-footer remain deferred to M4
(ADR 0005); the empty const is widened only when M4 ships a real receiver hook for a given agent.

## M4 — pi receiver hook (live receipt-from-footer)

**Date:** 2026-06-14 · **Branch:** `zynk-fork`

M4 closes the loop: the pi integration asset now parses the M3b protocol-ID footer from its OWN structured
`input` event (`pi.on("input")`, never scraping), verifies `body_hash`, records the receipt via the
server-authoritative M3a `zynk.message_received` (the footer is correlation metadata, NOT proof — the
server validates), and strips the footer from the model's view via an input transform. The production
receipt-capable const flips to `["pi"]` so a pi pane is footered by default. pi qualifies because it is
NON-reserved-native (a stateful `pane.report_agent` routes to `set_hook_authority_with_session_ref`, so it
has BOTH `agent_session.agent=="pi"` for the send gate AND `hook_authority` for the receipt gate);
reserved-native agents stay off the allowlist. M3a (`zynk.message_received`, `receipt_worker.rs`) and the
M3b send path are reused UNCHANGED (no new server/API surface, no `PROTOCOL_VERSION`/`PI` protocol bump).

Pi API gate (installed pi `0.79.3`) re-confirmed at the type defs before coding: `pi.on("input",
ExtensionHandler<InputEvent,InputEventResult>)` (types.d.ts:838), `InputEvent.text:string` +
`InputSource = "interactive"|"rpc"|"extension"` (types.d.ts:588-600), `InputEventResult` transform variant
`{action:"transform",text,images?}` replacing text before the model sees it (agent-session.js:725-728).
`node:crypto` sha256 hex matches the Rust `lowercase_hex_sha256` byte-for-byte.

### Upstream files touched (edits to existing zynk files)

- `src/integration/assets/pi/zynk-agent-state.ts` — ADDED (additive, alongside the existing
  session_start/agent_start/agent_end/session_shutdown hooks): `import { createHash } from "node:crypto"`;
  module-level pure exports `parseZynkFooter` (extract the FINAL line-anchored footer block + the byte-exact
  inverse of `append_footer`'s `body + "\n\n" + block`), `isEligibleInputSource` (rpc/interactive only),
  `zynkSha256Hex`/`verifyZynkBodyHash`, `eligibleZynkReceipt` (source-gate ∧ parse ∧ body_hash), the
  response-aware `parseReceiptResponse`/`classifyReceiptOutcome`/`sendReceiptRequest` (reads + parses the
  framed JSON-RPC response — UNLIKE the existing `sendRequest` which discards it), and the bounded
  dedup/retry state machine `recordZynkReceipt`; plus the `pi.on("input")` hook that source-gates, parses,
  verifies, FIRE-AND-FORGETs the receipt (the model turn never awaits transport), and returns
  `{action:"transform", text: pureBody, images}` to strip the footer. Bumped the marker
  `// ZYNK_INTEGRATION_VERSION=2` → `=3`. Retry policy is env-overridable
  (`ZYNK_PI_RECEIPT_MAX_ATTEMPTS`/`_BACKOFF_MS`/`_BACKOFF_CAP_MS`).
- `src/integration/mod.rs` — bumped `const PI_INTEGRATION_VERSION: u32 = 2;` → `= 3;` (mirrors the asset
  marker). Added two `#[cfg(test)]` tests: `pi_integration_version_marker_matches_const` (the DIRECT parity
  test the plan flagged as missing — catches drift in BOTH directions, unlike the indirect `outdated_*`
  tests) and `pi_asset_registers_zynk_receiver_hook` (CI-enforced asset-shape guard: `pi.on("input"`,
  `zynk.message_received`, source-gating, transform/continue shapes, footer markers, fire-and-forget).
- `tests/cli_wrapper.rs` — `integration_commands_run_locally_when_server_is_missing` status assertion
  `pi: current (v2)` → `(v3)` (the version bump changes the installed-version display).
- `justfile` — added a narrow `test-ts` recipe (`bun test src/integration/assets/pi/zynk-agent-state.test.ts`)
  and made `ci` depend on it (`ci filter='all()': lint test-ts`), so the proof-boundary-critical TS
  parser/helper logic is CI-enforced via `just ci`/`just check` (operator decision A). bun is already a
  sanctioned tool here and used by `website-build`; the addition is intentionally limited to the M4 pi
  tests — no broader JS toolchain churn.

### zynk-owned files added/touched

- `src/zynk/footer.rs` — flipped `const RECEIPT_CAPABLE_AGENTS: &[&str] = &[]` → `&["pi"]` (the production
  behavior flip: pi panes are footered by default). Updated `effective_allowlist_default_is_empty` →
  `effective_allowlist_default_is_pi_only`; added `pi_is_receipt_capable_by_default_reserved_native_is_not`
  (pi capable by default; codex/claude NOT).
- `src/integration/assets/pi/zynk-agent-state.test.ts` (NEW, zynk-owned) — 48 bun tests covering the
  parser (last-block-wins incl. a full decoy block at end-of-body, fake-footer-earlier, byte-exact pure
  body incl. trailing-whitespace/multi-blank bodies, malformed/duplicate/missing markers + CRLF +
  single-newline-separator + unknown-version + non-integer/string `conversation_seq`/`v` fail-closed),
  source gating, body_hash verify/mismatch, the response parser/classifier, the bounded dedup/retry state
  machine (success→deduped, already-deduped→skip, busy→retry→received, terminal→stop, attempts
  exhausted→gaveup, throwing/rejecting transport→retryable, in-flight concurrent duplicate→skip, NaN
  maxAttempts→sane bound), and the response-aware socket helper (incl. a synchronous `connect()` throw →
  null). A decorrelated adversarial review (3 lenses + synthesis) confirmed the byte-exact inverse and
  proof boundary, and surfaced one major (a synchronous `createConnection()` throw rejected instead of
  resolving the retryable `null`) now fixed, plus hardening (stricter footer-version/`conversation_seq`
  typing, a bounded dedup set, an in-flight guard, CRLF documented as fail-closed).
- `tests/zynk_receipt.rs` (extended) — `pi_default_footer_send_receipts_via_footer_ids`: a deterministic
  e2e (no live pi) — a `pane run` to a pi pane receipt-capable BY DEFAULT injects the footer; a receipt with
  the footer-carried IDs advances the message to `received`/`integration` exactly once, idempotently
  (`already_received` on replay). Helpers `report_pi_agent_session` (stateful `pane.report_agent` + session
  ref → hook authority AND agent_session), `pane_recent_text`, `wait_for_pane_text`.

### Rationale / removal condition

M4 is the minimal additive receiver: all new TS logic is pure/testable; the hook reads only its own
structured payload and the server remains the sole receipt authority. The const flip + version bump + the
receiver hook land as ONE atomic unit (operator-gated) so a footered pi pane can always strip + receipt.
Reverting M4 returns the const to `&[]` (no pi footering), reverts the asset to v2, and drops the bun
target; M3a/M3b data stay intact (no schema/protocol change). Claude/Codex remain valid message TARGETS but
get NO receiver adapter in M4 (reserved-native — they lack hook_authority); automatic receipt adapters for
them are future work. The bun/`test-ts` CI wiring is the only fork CI-surface divergence and is logged here
per the operator's instruction.

## M5a — `zynk query` lexical/BM25 path

**Date:** 2026-06-14 · **Branch:** `zynk-fork`

M5a adds `zynk zynk query <text> [filters] [--json] [--exact]` — the **F3 lexical/BM25 path ONLY** (NOT
"F3 complete"): an in-process, read-only BM25 query over the existing M2 `messages_fts`, with metadata
prefilters applied before ranking and an F4-enveloped JSON/human response. NO embeddings / sqlite-vec / RRF
/ embedding worker / `embedding_jobs` / migration 0002 / `Cargo.toml` deps (all M5b/M5c). No upstream
(zynk) source files were touched — all edits are in zynk-owned modules.

### zynk-owned files added/touched

- `src/zynk/retrieval/mod.rs` (NEW) — `QueryFilters` (default limit 20, capped at 200), `QueryHit`
  (provenance row), the F4-enveloped `QueryResponse` (`result` ok/failed + `command:"zynk query"` +
  `type:"zynk_query_result"` + `next`; failures carry `code`/`message`/`context`), and `run_query` (validate
  → open read-only → BM25 → envelope). Empty/whitespace query → `invalid_query` before any DB access; an
  FTS5 syntax error → `invalid_query`; other DB errors → `db_unavailable`. + unit tests.
- `src/zynk/retrieval/fts.rs` (NEW) — `bm25_search`: the BM25 SQL over `messages_fts` joined to `messages`
  by `rowid` (+ `conversation_participants` for from/to labels, the latest `delivery_events` row for
  `delivery_status`, FTS5 `snippet()`). ALL user input is bound (sqlx `.bind`) — no string interpolation;
  prefilters bound in the `WHERE` (before ranking); `ORDER BY bm25(messages_fts) ASC` (lower-is-better).
- `src/zynk/db.rs` — added `open_query_readonly[_at]` = `open_migrated_for_append` (runs MIGRATOR, skips
  orphan recovery) + `PRAGMA query_only = 1`, so a query NEVER synthesizes a `failed`/`system.recovery`
  delivery event. + a unit test that the opener rejects writes.
- `src/cli/zynk.rs` (fork-owned `zynk zynk` namespace) — added the `query` dispatch arm + the `query()`
  handler (flag parse mirroring `message_received`; `--json` → JSON, else human; F4 `invalid_filter` for a
  bad `--since`/`--limit` before any DB access) + `QUERY_USAGE`.
- `src/zynk/mod.rs` — register `pub mod retrieval;`.
- `tests/zynk_query.rs` (NEW) — 11 isolated-server integration tests: FTS freshness (send → query → hit),
  `--type`/`--agent`/`--since` prefilters, no-match→ok-empty, `invalid_query` (empty), `invalid_filter`
  (bad `--since`/`--limit`), F4 JSON envelope stability, human output, **zero `delivery_events` writes**
  across queries (read-only invariant), and BM25 ordering.

### Rationale / removal condition

M5a is purely additive + read-only — it cannot affect any M2/M3/M4 invariant (send never blocks; body/
`body_hash`/FTS purity; receipts server-authoritative). The read path is write-incapable (`query_only`) and
the zero-`delivery_events` test guards the no-recovery invariant. F3 is NOT complete until M5b (embeddings +
sqlite-vec) and M5c (RRF) land (gated on the sqlite-vec spike + ADR 0006). Reverting M5a drops the new
modules + the `query` arm + the read opener; the M2 schema/FTS are unchanged.

## M5b — Embedding index (sqlite-vec + async embed worker + enqueue) / F3 vector index landed

**Date:** 2026-06-14 · **Branch:** `zynk-fork`

M5b lands the **F3 VECTOR INDEX** (NOT "F3 complete" — RRF fusion is M5c): an `Embedder` provider seam
(deterministic std-only `FakeEmbedder` default; opt-in real `fastembed`), a per-message `embedding_jobs`
queue enqueued INSIDE the existing send transaction (atomic, send never blocks), and an App-owned async
embedding worker that loads sqlite-vec statically and writes vectors into a lazily-created `vec0` table +
`message_embeddings`. Under ACCEPTED **ADR 0006** (commit `da37e3a`). No M2/M3/M4 invariant weakened
(send-path `body`/`body_hash`/FTS purity preserved; send never blocks on embedding). The default build/test
stays 100% network-free + hermetic (FakeEmbedder; no ort/ONNX/HF in the default graph). No RRF /
`ranking:"rrf"` / hybrid query (all M5c).

### Upstream files touched (edits to existing zynk files)

- `Cargo.toml` / `Cargo.lock` — (ADR 0006) added DIRECT `libsqlite3-sys = { version = "0.30.1", features =
  ["bundled"] }` (feature-unified onto the SINGLE `libsqlite3-sys` node `sqlx-sqlite` links — load-bearing for
  the static `sqlite3_auto_extension` registration) + `sqlite-vec = "0.1.9"` (vendors `sqlite-vec.c`,
  compiled `-DSQLITE_CORE`; exports `sqlite3_vec_init`). Added a `[features]` section (`default = []`,
  `fastembed = ["dep:fastembed"]`) + the OPTIONAL `fastembed = { version = "5", optional = true }`. Deps added
  via `cargo add` (minimal lock update): **ZERO existing-package version bumps** (verified vs `da37e3a`), the
  single-`libsqlite3-sys` node preserved. `fastembed` pulls ~180 transitive packages but ALL are behind the
  optional feature — ABSENT from the default compiled graph (default `cargo tree` has no `ort`/`ort-sys`/ONNX/
  HF), so `just test`/`just check` stay network-free. **(Operator note: the +180 optional lock packages are a
  notable Cargo.lock diff even though feature-gated; drop the `fastembed` dep if the FakeEmbedder default path
  alone is preferred until a provisioning milestone.)**
- `src/app/mod.rs` — `App` gains `pub(crate) zynk_embedding_worker:
  Option<crate::zynk::embedding_worker::EmbeddingWorkerHandle>` (init `None` in the single `App::new` struct
  literal; all other constructors delegate to `App::new`, so they inherit `None`) — mirrors the M3a
  `zynk_receipt_worker` field exactly.
- `src/server/headless.rs` — `run_server` spawns the App-owned embedding worker right after the receipt-worker
  spawn (`app.zynk_embedding_worker = Some(crate::zynk::embedding_worker::spawn())`), before `HeadlessServer::new`
  takes ownership; `complete_shutdown` does `self.app.zynk_embedding_worker.take()` (ordered, before
  `cleanup_sockets`, right after the receipt-worker take) to stop+join the worker. Same precedent + caveat as
  M3a: only the primary `run_server` installs it (not `run_handoff_import_server`).

### zynk-owned files added/touched (NEW logic — not upstream divergence; listed for the milestone summary)

- `migrations/zynk/0002_embedding_index.sql` (NEW) — the three PLAIN metadata tables (`embedding_models`,
  `embedding_jobs`, `message_embeddings`); all `CREATE ... IF NOT EXISTS`, additive, forward-only, no change
  to M2 tables. The sqlite-vec `vec0` virtual table is DELIBERATELY NOT here — the worker creates it lazily on
  an extension-loaded connection (ADR 0006), keeping the migrator extension-free.
- `src/zynk/embed/mod.rs` (NEW) — the `Embedder` trait + `EmbedError` + `embedder_from_env`
  (`ZYNK_EMBED_PROVIDER`, default `fake`; the `fastembed` arm `#[cfg(feature)]`-gated) + `active_model_id()`
  (the single source of truth shared by the enqueue + the worker).
- `src/zynk/embed/fake.rs` (NEW) — deterministic, std-only `FakeEmbedder` (fixed-key `DefaultHasher` →
  L2-normalized unit vector; `with_dim`/`failing_then_ok` seams). The default + the no-network test invariant;
  a golden-vector test pins the hash output against a toolchain-induced change.
- `src/zynk/embed/vec.rs` (NEW) — sqlite-vec loading: `register_sqlite_vec()` (process-global `Once` →
  `sqlite3_auto_extension(sqlite3_vec_init)` via an explicitly-typed `unsafe` transmute) + `ensure_vec0_table`
  (lazy `CREATE VIRTUAL TABLE IF NOT EXISTS … USING vec0(… float[dim] distance_metric=cosine)`, identifier-
  validated). Tests assert the SINGLE-`libsqlite3-sys` invariant (Cargo.lock count == 1 — the ADR 0006 CI
  guard) + registration-ordering (register → fresh sqlx conn → create vec0 → insert → KNN).
- `src/zynk/embed/fastembed.rs` (NEW, `#[cfg(feature = "fastembed")]`) — the real `RealEmbedder`
  (`multilingual-e5-small@1`, dim 384), loaded OFFLINE from a provisioned local dir (`ZYNK_EMBED_MODEL_DIR`)
  via `TextEmbedding::try_new_from_user_defined`; `Err(ModelUnavailable)` if unprovisioned — never panics,
  never downloads (ADR 0006 §D5/§D6). Absent from the default build.
- `src/zynk/embedding_worker.rs` (NEW) — the App-owned poll-driven embed worker (mirrors `receipt_worker.rs`):
  a `std::thread` + one current-thread Tokio runtime + one reused sqlite conn (vec0 registered before open) +
  a shutdown channel. At start it ensures the model row + lazy vec0 table, recovers crashed `running` jobs,
  bounded-backfills, then polls `embedding_jobs` and embeds each (FakeEmbedder) → vec0 row + `message_embeddings`
  + `done`, all in ONE transaction. Per-job failure is recorded (`failed`, retryable to `MAX_ATTEMPTS`) and
  NEVER propagated — a STRUCTURAL no-strand guarantee (everything past the running-mark funnels through one
  `mark_job_failed` via a `JobError` inner step); finite/dim guards; COMMIT-error rollback on the reused conn;
  bounded `Drop`-join shutdown; never touches live Zynk state.
- `src/zynk/persistence.rs` (extended) — `begin_send_attempt_in_transaction` now enqueues ONE `embedding_jobs`
  row (status `pending`, `active_model_id()`) AFTER the `messages_fts` insert, INSIDE the same `BEGIN IMMEDIATE`
  unit, so it commits/rolls back atomically with the message (never an orphan job, never a message without one).
  Purely additive — `body`/`body_hash`/`footer_json`/`messages`/`messages_fts` writes byte-unchanged. NEVER an
  inline embed (send never blocks; compute is the out-of-band worker).
- `src/zynk/mod.rs` — registers `pub mod embed;` + `pub mod embedding_worker;`.

### Tests

- `tests/zynk_embed.rs` (NEW, zynk-owned) — migration 0002 applies + the three tables exist + M2 tables intact
  + NO `USING vec0` entry in `sqlite_master` (the extension-free proof) + a static check that the migration FILE
  declares no `CREATE VIRTUAL TABLE` + the status CHECK constraint. **The migrated DB is obtained via the
  IN-PROCESS, worker-free `zynk zynk query` path (M5a) — NOT a spawned server — so the proof is DETERMINISTIC and
  never races the runtime worker's lazy vec0 (ARB-M5B-001 R1 fix; the worker's legitimate runtime vec0 creation is
  covered separately by the `embedding_worker.rs` unit tests).**
- Unit tests: `embed/fake.rs` (determinism/unit-norm/dim-0/failure-injection/golden-vector), `embed/mod.rs`
  (provider selection incl. the cfg-gated fastembed arm in BOTH configs, `active_model_id`), `embed/vec.rs`
  (single-`libsqlite3` guard, registration-ordering vec0 round-trip, identifier validation),
  `embedding_worker.rs` (pending→done + vec0/`message_embeddings` rows, retry, crash-recovery, backfill,
  no-job-left-`running`, vec_table identifier, spawn/Drop), `persistence.rs` (one pending job enqueued
  atomically; failed-send leaves no orphan).
- A decorrelated **5-lens adversarial review** (purity/atomicity, FFI/invariants, worker correctness,
  hermeticity/features, wiring/lifecycle) surfaced 1 major + 4 minor — ALL FIXED: the worker no-strand
  restructure (major — a transient DB fetch error used to strand a job in `running`), COMMIT-error rollback on
  the reused conn, non-finite/wrong-dim embedding guards, the Drop-comment accuracy, and the FakeEmbedder
  golden test + softened cross-release determinism claim.
- The Arbiter R1 gate (REQUEST_CHANGES) found ARB-M5B-001 — the migration extension-free test raced the
  server-spawned worker's lazy vec0; FIXED by migrating worker-free via the in-process `zynk zynk query` path
  (above), making the proof deterministic (operator-mandated; no worker disable, no runtime behavior change).
- Full `just check` GREEN on the final tree: **2269 nextest + 48 bun + 49 python**, fmt + clippy `-D warnings`
  clean; `git diff --check` clean.

### Rationale / removal condition

M5b is the minimal additive surface for the F3 vector index: all compute is out-of-band (send never blocks),
the write path stays pure, and the heavy real embedder is opt-in + offline-by-construction so the default
build/test never touches ort/ONNX/HF/network. The single-`libsqlite3-sys` invariant is the load-bearing
constraint (test-guarded). Reverting M5b drops the new zynk modules + migration 0002 + the three upstream
touches (the `App` field, the headless spawn/shutdown, the Cargo deps/feature); M2/M3/M4 data + schema stay
intact (0002 is additive `IF NOT EXISTS`). F3 is NOT complete until M5c (RRF fusion of BM25 + vector). The
`fastembed` feature stays default-OFF until a dedicated provisioning/cutover milestone stages the ORT cache +
the model dir.

## M5c — RRF hybrid query (F3 complete)

**Date:** 2026-06-14 · **Branch:** `zynk-fork`

M5c adds the RRF/hybrid layer on top of the M5a BM25 path + the M5b vector index — **F3 (full hybrid
retrieval) is now COMPLETE**. `zynk zynk query` fuses lexical (BM25 over `messages_fts`) + vector (vec0 KNN
over the M5b embedding index) results via Reciprocal Rank Fusion. NO upstream (zynk) source files touched, NO
new deps — all edits are in zynk-owned `src/zynk/retrieval/*` + the zynk-owned `tests/zynk_query.rs`. The query
stays in-process / no-socket / read-only (`open_query_readonly`, `query_only=1`, zero recovery/delivery
writes); the default path is FakeEmbedder / no-network; fastembed stays opt-in.

### zynk-owned files added/touched

- `src/zynk/retrieval/rrf.rs` (NEW) — pure `rrf_fuse(lists: &[(weight, ids)], k) -> Vec<(id, score)>` (Cormack
  et al.; `RRF_K=60`; score = Σ w/(k+rank), 1-based; per-list dedup; deterministic `score DESC, id ASC`).
- `src/zynk/retrieval/vector.rs` (NEW) — the vec0 KNN runner `knn_search(conn, query, filters) ->
  VectorOutcome`. Resolves the active model (`embedding_models` for `active_model_id()` → `vec_table`/`dim`);
  embeds the QUERY via `embedder_from_env` (default FakeEmbedder → the same space the worker stored); runs vec0
  KNN for a CANDIDATE POOL (`k=(limit*8).clamp(limit,512)`), then JOINS to `messages` + applies the SAME
  prefilters as `fts.rs` (the plan §6 fallback — the M5b vec0 table has no aux/partition columns),
  distance-ordered, emitting ALL survivors (the post-fusion truncate cuts to `limit`, preserving fusion recall).
  **Graceful degradation (operator hard requirement): returns `VectorOutcome` DIRECTLY — never a `Result`,
  never panics; EVERY vector-side problem (no model row, no vec0 table, embedder won't build, embed/dim/KNN/
  provenance error) swallows to `functional=false` + empty hits**, so the caller degrades to BM25-only with an
  honest `vector_index.ready=false`. Reports `pending_jobs` (non-`done` `embedding_jobs`).
- `src/zynk/retrieval/mod.rs` (extended) — `QueryHit`: `bm25_rank` → `Option<i64>` + new `vector_rank:
  Option<i64>`. `QueryResponse`: new top-level `vector_index` (`{ready, pending_jobs, model_id}`). `run_query`
  is the hybrid pipeline: validate → `register_sqlite_vec()` (before the read conn) → `open_query_readonly` →
  `bm25_search?` (ONLY error sources: `fts_query_error`→`invalid_query`, true open→`db_unavailable`) →
  `knn_search` (never errors) → the pure `fuse_results`. `fuse_results`: vector `functional` AND non-empty →
  `ranking="rrf"` + `rrf_fuse([(1.0,bm25_ids),(1.0,vec_ids)], 60)` (provenance prefers BM25 for the snippet;
  per-hit `matched_modes`/`bm25_rank`/`vector_rank`/fused score); else → `ranking="bm25"` passthrough.
  **HONESTY: never claims `rrf` without vector hits; `vector_index` always truthful; the `next` guidance is
  gated on the real `ranking` (never claims hybrid on a BM25 result); never a `vector_unavailable` failure.**
- `src/zynk/retrieval/fts.rs` (extended) — `bm25_rank: Some((i)+1)`, `vector_rank: None`.

### Tests

- Unit: `rrf.rs` (7), `vector.rs` (5 DB-backed — exact-match→`vector_rank=Some(1)`, prefilter restricts,
  no-model/no-table degrade w/o panic, `pending_jobs`), `mod.rs` `fuse_results` (4 + the `next`-honesty test).
- Integration `tests/zynk_query.rs`: `partial_freshness_honest_bm25_fallback` + the existing lexical tests run
  on a fixture configured `ZYNK_EMBED_PROVIDER=fastembed` — UNCOMPILED in the default build, so the worker's
  `embedder_from_env()` returns `ModelUnavailable` and it cannot embed → the vector index is DETERMINISTICALLY
  unavailable → honest BM25 fallback (a real "provider configured but not built" config, NOT a test-only
  worker-disable; this replaced an earlier poll-pin that raced the worker's boot sweep — the ARB-M5B-001 race
  class). `hybrid_rrf_end_to_end` runs on a default-FakeEmbedder fast-worker fixture + a BOUNDED poll until
  `ranking="rrf"` → a both-mode hit (both ranks) + `ready=true`/`pending=0`. `hybrid_prefilter_excludes_…`. The
  vector-only/lexical-only PARTITION is covered by the `vector.rs`/`fuse_results` units (documented). A `Drop`
  on the test `Fixture` guarantees temp-dir cleanup on a panicking test.
- A decorrelated 5-lens adversarial review (rrf correctness, vector degrade/SQL-safety, hybrid honesty, test
  determinism, invariant preservation) found 1 major + 3 minor — ALL FIXED: the boot-sweep race (major), the
  `next`-claims-hybrid-on-bm25 honesty drift, the vector-leg-pre-truncation recall loss, the Fixture temp-dir leak.
- Full `just check` GREEN: 2289 nextest + 48 bun + 49 python, fmt + clippy `-D warnings` clean; `git diff
  --check` clean.

### Rationale / removal condition

M5c is purely additive + read-only — it cannot affect any M2/M3/M4/M5a/M5b invariant (send never blocks;
body/body_hash/FTS purity; receipts server-authoritative; the query write-incapable). With this, **F3 (full
hybrid retrieval) is COMPLETE**. Reverting M5c drops `rrf.rs`/`vector.rs` + the hybrid wiring; the query falls
back to the M5a BM25 path. No schema change, no new deps, no ADR change. Native install/dogfood/cutover remain
out of scope (separate operator kickoffs).

## M5d — cleanup / stabilization (F3 follow-up: deferred test + docs tidy)

**Date:** 2026-06-15 · **Branch:** `zynk-fork`

M5d is post-F3 cleanup — closing one deferred test gap and a truthful current-state doc note. NO production
code change, NO upstream source touched, NO new deps, NO schema/ADR change. Test + docs only.

### zynk-owned files touched

- `tests/zynk_footer.rs` (extended) — closes the deferred **M3b-INS-001**: a DETERMINISTIC, DIRECT `agent send`
  rendered-footer integration pair (prior M3b footer coverage was `pane run` + indirect). A shared helper
  `report_hook_agent_session` reports the target pane via `pane.report_agent` (non-reserved source + session
  ref → `set_hook_authority_with_session_ref` = BOTH `agent_session` surfaces AND `agent.get` resolves; note
  `report_agent_session` alone is persisted-only, NOT `agent.get`-resolvable — the M3b routing gotcha).
  - `footer_injected_for_receipt_capable_agent_send`: pane reported as `pi`, `agent send pi -- zbodysentinel hi`
    under `ZYNK_RECEIPT_CAPABLE_AGENTS=pi` → asserts exit 0 + `message_id`, the rendered footer START/END
    markers land in the target pane (bounded `wait_for_pane_text`, no fixed sleep), `messages.body` stays PURE
    (`"zbodysentinel hi"`), and `footer_json` carries the protocol message_id.
  - `no_footer_for_non_allowlisted_agent_send`: pane reported as `kimi`, allowlist stays `pi` → the bare body is
    delivered (`wait_for_pane_text("zbodysentinel")`) with NO footer marker — the allowlist gate governs
    `agent send` too.
  - The agent-send footer injection itself is PRE-EXISTING (M3b `cli/agent.rs`); this is COVERAGE only, no code
    change. Not in the worker-race class — the footer is CLI-injected (send hook), independent of the embedding
    worker, so these tests don't depend on vector/ranking state. Deterministic (verified across repeated runs).
- `docs/zynk/plans/2026-06-14-m5-query-retrieval.md` (additive STATUS banner) — truthful current state: F3
  COMPLETE (M5a `58c6e37` / M5b `1b42b93` / M5c `8f3cfe0` all committed); the plan's "F3 NOT complete until…" /
  "NEVER 'F3 complete'" lines are noted as the *cadence* guardrails they were, now satisfied. The plan body is
  UNCHANGED; SPEC.md (the timeless F3 *requirement*) and the accepted ADRs are untouched (additive-only law).

### Readiness smoke (D3)

`zynk zynk query` was smoke-verified end-to-end (JSON + human modes) on a fresh DB: the F4 envelope carries the
M5c hybrid fields honestly (`ranking="bm25"`, `vector_index{ready=false, pending_jobs, model_id}`, count 0; the
human mode prints the no-matches guidance). The UX is already covered by the 25 `zynk_query` integration tests,
so no separate smoke script was added (per the operator's "only if clearly useful + non-invasive").

### Tests / validation

- Focused: `cargo nextest --test zynk_footer` 11/11 (incl. the 2 new agent-send tests), `--test zynk_message`
  17/17, `--test zynk_query` 25/25 — all `--test-threads=1`, deterministic across repeated runs.
- Full `just check` GREEN: 2292 nextest + 48 bun + 49 python, fmt + clippy `-D warnings` clean; `git diff
  --check` clean.

### Rationale / removal condition

M5d is test/docs-only — it adds coverage for the existing M3b agent-send footer behavior and a truthful
F3-complete status note; it changes no production code, schema, dep, or ADR, so it cannot affect any
M2/M3/M4/M5* invariant. Reverting M5d drops the two `zynk_footer.rs` tests + the plan banner. Native
install/dogfood/cutover/rebrand remain out of scope (separate operator kickoffs).

## M6 — Native Zynk UX + rebrand + cutover readiness (ADR 0007 + ADR 0008)

**Date:** 2026-06-15 · **Branch:** `zynk-fork` · **Baseline:** `6096c0c`. Binding decisions: **ADR 0007**
(native UX & rebrand), **ADR 0008** (native DB path & wrapper cutover; supersedes ADR 0003 path default).
One milestone, implemented by parallel worktree agents (native commands / string-env-socket rebrand / DB
cutover / 12-target integration rebrand / dev-UX / docs / website) + a serial integration pass. Repo-ready
ONLY — NO live install/dogfood/wrapper-replacement/master-cutover/push/tag/release. Package/crate name stays
`zynk` (no broad internal rename — upstream-merge survivability).

### Binary + dispatch
- `Cargo.toml` — added `[[bin]] name = "zynk" path = "src/main.rs"`. Produced binary is now `zynk`; package
  stays `zynk`. Repo-wide `CARGO_BIN_EXE_zynk` → `CARGO_BIN_EXE_zynk` test-locator sweep (19 test files).
  `target/release/zynk` → `…/zynk` in `remote/unix.rs`; program-name fixtures in `session.rs`. **Merge
  hot-spot.** `EnvFilter("zynk=info")` → `"zynk=info"` (the `[[bin]]` reroots `module_path!` to `zynk`).
- `src/cli.rs` — `mod native;` + new top-level dispatch arms `send/reply/thread/inbox/whoami/who/query`
  (native verbs) + `db` (ADR 0008), appended after the `zynk` arm; own usage/help/status strings → zynk.
  The flat dispatch match is the canonical upstream merge hot-spot (arms appended, no reorder).

### Native command surface (ADR 0007 §2) — net-new fork files
- `src/cli/native.rs` (NEW) — `send/reply` (reuse the existing `agent send`/`pane run` transport — resolve
  target→pane→`begin_send_attempt`→footer→`PaneSendInput`; `reply` = send, parent auto-derived, no
  `--reply-to`); `whoami`/`who` (live-socket compose, hook-authoritative identity, detection-only labels
  surfaced as `detected`); `query` (top-level over `retrieval::run_query`; legacy `zynk query` retained).
- `src/zynk/inbox.rs` (NEW) — `thread`/`inbox` read-only queries via `open_query_readonly` (PRAGMA
  query_only=1), runtime-scoped on `socket_namespace`, F4-enveloped, ZERO delivery-event writes.
- `src/zynk/mod.rs` — `pub mod inbox;` + `pub mod db_cutover;`.
- `tests/zynk_native_cli.rs` (NEW, 16 tests).

### Native DB path + wrapper-cutover safety (ADR 0008) — zynk-owned, no upstream surface
- `src/zynk/db_path.rs` — final native default `$ZYNK_HOME/zynk.db` (`~/.zynk/zynk.db`); `zynk-v2` dropped
  as default, recognized only for transitional adopt. `src/zynk/db.rs` — `classify_db_at()` (read-only,
  positive native recognition) + fail-closed `db_foreign_conflict` guard wired into the shared opener
  BEFORE the writable WAL connect → a foreign/wrapper DB is left byte-identical and refused. `src/zynk/
  db_cutover.rs` (NEW) — `zynk db status|adopt|backup|import` (deterministic, non-destructive relocate).
  `src/zynk/preflight.rs` — reconciled (the dev/test runtime still refuses the whole production `~/.zynk`).
  `tests/zynk_db_cutover.rs` (NEW) + `tests/zynk_preflight.rs` (adjusted).
- `src/server/headless.rs` — server startup **fail-closed**: a `db_foreign_conflict` from `open_migrated`
  aborts (`process::exit(1)`, branded stderr) instead of warn-and-continue (ADR 0008).

### User-facing rebrand + env/socket/log (ADR 0007 §3, §5)
- `src/main.rs` — `--help` banner/usage/version/DEFAULT_CONFIG (incl. fixing the stale `~/.config/zynk`
  hint → `~/.config/zynk`, `[ui.toast.zynk]`→`[ui.toast.zynk]`) → zynk; `ZYNK_ENV` primary.
- `src/config.rs`/`config/io.rs` (+`ZYNK_CONFIG_PATH` primary, `env_first()` ZYNK-first resolver), `src/api/
  mod.rs` (+`ZYNK_SOCKET_PATH`), `src/server/socket_paths.rs` (+`ZYNK_CLIENT_SOCKET_PATH`, basenames
  `zynk.sock`/`zynk-client.sock`), `src/session.rs` (+`ZYNK_SESSION`), `src/logging.rs` (`ZYNK_LOG` filter,
  `LOG_FILE_{MONOLITH,CLIENT,SERVER}` = `zynk*.log`) — **ZYNK_\* primary, ZYNK_\* transitional compat,
  ZYNK wins when both set (tested `tests/zynk_env_precedence.rs`).** Consumers `client/mod.rs` +
  `server/headless.rs` wired to the `LOG_FILE_*` consts.
- `src/cli/{pane,agent,workspace,tab,server,worktree,notification,status,zynk}.rs`, `src/remote/unix.rs`,
  `src/ui/dialogs.rs`, `src/ui/settings.rs`, `src/server/autodetect.rs`, `src/client/mod.rs`, `src/server/
  handoff.rs`, `src/app/worktrees.rs`, `src/app/api/worktrees.rs` — user-facing usage/help/error/prose
  strings → zynk. `src/config/model.rs` — `ToastDelivery::Zynk` `#[serde(rename="zynk", alias="zynk")]`,
  `[ui.toast.zynk]` canonical key (legacy `zynk` alias), worktree default `~/.zynk/worktrees` →
  `~/.zynk/worktrees`. Production runtime artifact basenames (`handoff`/`sound`/`clipboard`/`pty`/remote
  sockets) → `zynk-*`.
- **Retained (classified):** package/crate name `zynk` (cat-2 survivability); `ZYNK_*` env names (cat-3
  transitional compat); `zynk:<agent>` socket source labels + `RemoteZynk` symbol (cat-2 host-protocol);
  release-asset `zynk-{target}` + `update.rs` install/self-update infra (cat-2, M8-gated); `zynk-agent-
  state.*` legacy filename literals in the integration migration-cleanup path (cat-3); test-scaffolding
  `zynk-*-test` temp dirs (cat-2). **Zero category-4 (user-facing) residue at completion.**

### Integration adapters — all 12 targets (ADR 0007 §3)
- `src/integration/mod.rs` + `src/cli/integration.rs` + `src/integration/assets/*` — asset files renamed
  `zynk-agent-state.*` → `zynk-agent-state.*` (18 files); marker `ZYNK_INTEGRATION_VERSION` →
  `ZYNK_INTEGRATION_VERSION` (legacy marker recognized as fallback); hook binary spawn `zynk`→`zynk`; hook
  env reads `ZYNK_*` primary + `ZYNK_*` fallback; install writes new names, uninstall/status clean up BOTH
  new + legacy (bounded migration). Behavior classes preserved: **state-reporting** pi/omp/hermes;
  **session-identity** claude/codex/copilot/droid/qodercli/cursor; **both** kilo/kimi/opencode. User-facing
  install/status/uninstall/help text → zynk. `tests/zynk_integration_rebrand.rs` (NEW) + `tests/cli_wrapper.rs`
  (asset-rename consequence).

### Dev UX (ADR 0007 §4)
- `justfile` — `check/test/test-one/lint/ci/build/default-config` route cargo through `scripts/zynk-dev.sh`
  so a bare `just check` auto-enforces isolation (no operator-facing wrapper). `scripts/zynk-dev.sh` —
  internal guard; scrubs `ZYNK_*` + primary `ZYNK_*` overrides; default `CARGO_TARGET_DIR` moved to
  `/var/tmp` (disk, not tmpfs). `docs/zynk/dev-ux.md` (NEW). `-p zynk` retained (cat-2).

### Docs / website
- `README.md` + `docs/next/README.md` (rebranded, provenance preserved), `docs/zynk/SPEC.md` (additive
  current-state STATUS notes), `docs/zynk/cutover-readiness.md` (NEW deliverable), `docs/zynk/decisions/
  0007`+`0008` (NEW ADRs), `docs/zynk/plans/2026-06-15-m6-native-ux-rebrand-cutover.md` (NEW plan).
- `website/**` + `docs/next/website/**` (49 files; mirror gate kept byte-identical) + `SKILL.md` — brand
  copy → Zynk; upstream provenance (`dzevs/zynk`, LICENSE/NOTICE) preserved (cat-1). **Distribution
  layer retained as cat-1/3 (M8-gated):** `website/{latest.json,preview.json,install.sh,install.ps1,
  agent-guide.md}` describe the ACTUAL upstream `zynk` release/install (the fork has no `zynk.dev`/release
  endpoint yet — rebranding to a nonexistent zynk binary would be broken fiction; flagged for M8).

### Validation / invariants
- Full `just check` GREEN — **2341 nextest + 48 bun + 49 python; fmt + clippy `-D warnings` clean**;
  `git diff --check` clean. Invariants intact: F4 envelopes; receipt server-authoritative (ADR 0002 §4);
  `pane send-text` draft-only (ADR 0004); query read-only / zero delivery writes; body/footer purity;
  M2/M3/M4/M5 suites green. No live runtime mutation; all DB/integration tests use isolated temp homes; the
  operator's real `~/.zynk` / `~/.config/zynk` were never touched.

### Removal condition / scope
M6 makes the repo READY for native cutover; it performs none of it. Reverting M6 returns the binary to
`zynk`, removes the native verbs + `zynk db` + the foreign-DB guard, and restores upstream brand strings.
Live install/dogfood/wrapper-replacement gated to **M7**; push/tag/release gated to **M8**.

### M6 gate R1 fixes (Arbiter REQUEST_CHANGES → fixed; same gate continuation)
- **ENV-002:** `src/pane.rs` (`apply_host_protocol_env` exports `ZYNK_ENV` + `ZYNK_ENV`) + `src/integration/
  mod.rs` (`apply_pane_env` exports `ZYNK_SOCKET_PATH`/`ZYNK_PANE_ID` + ZYNK_* compat) — the runtime now
  EXPORTS the ZYNK_* host-protocol primaries (ADR 0007 §5), not just reads them. +3 tests.
- **NATIVE-003:** `src/zynk/message.rs` (`SendCommand::ZynkSend`/`ZynkReply` → `"zynk send"`/`"zynk reply"`) +
  `src/cli/native.rs` — native `send`/`reply` F4 `command` field is now the native label (was `agent send`);
  transport/delivery unchanged. +command-label success/failure tests.
- **WARDEN-001:** stable website/docs no longer advertise a nonexistent zynk release/install (install.mdx +
  index.html + AGENTS.md → build-from-source/"no release yet"; secondary pages cli-reference/session-state/
  windows-beta/configuration caveated; mirror byte-identical). Distribution layer + `update.rs` stay M8.
- **DOC-004:** `docs/zynk/dev-ux.md` proof re-captured to `zynk` binary/socket names.
- Re-validated: `just check` 2345 nextest + 48 bun + 49 python green; fmt + clippy clean; mirror EMPTY;
  `git diff --check` clean.

### M6 gate R2 fix (WARDEN-001-R2 — updater fails closed pre-M8; same gate continuation)
- **WARDEN-001-R2:** `src/update.rs` (`ZYNK_RELEASE_INFRA_AVAILABLE=false` + `release_infra_open()`;
  `self_update`/`auto_update` fail closed before consulting the `zynk.dev` manifests — test override =
  `ZYNK_FAKE_UPDATE_VERSION` so the release-machinery tests still run), `src/cli.rs` (`channel_set` fails
  closed), `src/main.rs` (help caveats `zynk update`/`channel set` as unavailable pre-release; branded
  fail-closed message prints clean), `README.md` + `docs/next/README.md` (rewritten: auto-update + channels
  not available until first Zynk release; update by rebuilding from source; mirror byte-identical). The
  manifest URL consts stay `zynk.dev` (cat-2, repointed at M8) but are never consulted while the gate is
  closed. Re-validated: `just check` 2345 nextest + 48 bun + 49 python green; `git diff --check` clean; the
  fail-closed `zynk update`/`channel set` outputs captured.

### M6 operator commit-gate fix (deterministic `just check`)
- **Flake hardening (test-only):** `tests/cli_wrapper.rs` + `tests/support/mod.rs` — floored both
  `wait_for_socket` helpers at a 30s minimum (`timeout.max(Duration::from_secs(30))`). The
  `named_sessions_use_separate_servers_and_workspace_state` test (spawns 2 named-session servers, 5s
  socket-appearance budget) flaked under bare `just check` saturation (~32-wide nextest); the generous floor
  removes the load-induced timeout without slowing the happy path (returns <1s when the socket binds). No
  `src/`/schema/ADR change. Proof: full `just check` ×3 consecutive → 2345/2345 green each; `git diff --check`
  clean.

## Visible message HEADER replaces the hidden pi-only receipt footer (ADR 0009)

**Date:** 2026-06-15 · **Branch:** `zynk-fork` · **Baseline:** `a68d3556`. Operator-decided fix: the M3b/M4
hidden receipt footer was **pi-only** (allowlist `["pi"]`), parsed+stripped by a custom pi receiver, invisible
to the model — unfair/asymmetric + agents replied in direct chat unaware it was a zynk message. Replaced with a
uniform **agent-visible HEADER** for every native message to an agent target. Binding: **ADR 0009**. Built by 3
parallel worktree lanes + a serial integration. Repo-ready; NO commit (operator gate).

### Header core (was footer.rs → header.rs)
- **`src/zynk/footer.rs` → `git mv` `src/zynk/header.rs`**: `render_header(from,to,record,type)` (the operator
  box template — from/to+cwd, optional type, id, conv#seq, `reply: zynk reply <from_pane> -- "…"`, "not receipt
  proof" note; missing fields → `-`; never panics) + `prepend_header(h,b)` = `h\n\n b`. Removed the receipt-capable
  allowlist (`is_receipt_capable_target`, `RECEIPT_CAPABLE_AGENTS` const/env, `effective_receipt_capable_agents`,
  the FOOTER markers); added `is_agent_target(to)` (`agent_session||agent`, awareness gate, not control-path).
  `FOOTER_VERSION` → `PROTOCOL_VERSION`. Module `pub mod footer;` → `pub mod header;`; all `footer::` → `header::`.
- **Inject sites** `src/cli/{native,agent,pane}.rs`: `if is_agent_target(&to) { prepend_header(render_header(from,
  to,record,type), text) } else { text }` — uniform for claude/codex/pi; `from` already in scope. No
  delivery/transport change.
- **`src/zynk/message.rs`**: F4 `next` made truthful (no auto-receipt) — "delivered (submitted); recipient sees a
  Zynk header and can reply via `zynk reply`", never promises received.
- **Receipt de-scoped (dormant, not deleted):** `src/zynk/receipt.rs` + `src/app/api/zynk.rs` + the
  `message-received` CLI kept — the server-authoritative `zynk.message_received` capability stays, but NOTHING
  auto-fires it on send. `delivery_status` stays `submitted`; proof invariant unchanged (header ≠ proof).

### DB column rename (direct schema change — DB disposable, no data to preserve)
- `migrations/zynk/0001_global_persistence.sql`: `footer_json` → **`protocol_json`** (edited in place; the live
  dev/prod DB must be **wiped** on redeploy — checksum changed). `src/zynk/persistence.rs` + tests updated;
  `footer_json` removed from the runtime schema/source/tests (historical docs/plan references to the old footer design remain as history); `protocol_json` is now the column. body/body_hash/FTS stay pure (header is wire-only).

### Pi extension → Zynk state-only (v4)
- `src/integration/assets/pi/zynk-agent-state.ts`: removed the `pi.on("input")` receiver + footer parser +
  receipt machinery + `ZYNK_FOOTER_*` + the transform-strip + `node:crypto`; kept lifecycle/state/session-ref
  reporting + `ZYNK_*`/`ZYNK_*` env + `source="zynk:pi"`. **`ZYNK_INTEGRATION_VERSION` 3 → 4.** TS test rewritten
  (state-only + receiver-absence guards; 9 bun tests). `src/integration/mod.rs`: `PI_INTEGRATION_VERSION` 3 → 4;
  the `pi_asset_registers_zynk_receiver_hook` test → `pi_asset_is_state_only_no_receiver` (CONTAINS→absence
  asserts). `tests/cli_wrapper.rs`: `pi: current (v3)` → `(v4)`.

### Test-isolation fix (orthogonal latent M6 bug, surfaced by running inside live zynk)
- `scripts/zynk-dev.sh`: the wrapper now also scrubs `ZYNK_ENV ZYNK_ENV ZYNK_PANE_ID ZYNK_PANE_ID`. When dev
  commands run from INSIDE a live zynk pane (which exports `ZYNK_ENV=1` per M6 ENV-002), the leaked flag reached
  test-spawned servers → nested-runtime guard → socket never appeared (14 server-spawn tests failed; fails on
  baseline `a68d3556` too inside zynk — NOT this change). Scrubbing keeps tests hermetic.

### Docs
- `docs/zynk/decisions/0009-visible-message-header-replaces-receipt-footer.md` (NEW); additive SPEC §3 F2 / §6
  STATUS notes; M3b/M4 plan marked superseded-in-part.
- **R3 active-doc footer purge** (operator: "pastikan semuanya clean tidak ada residu footer"). Every doc that
  describes the *current* architecture is now footer-clean or supersession-marked: `docs/zynk/SPEC.md` (§3 F2
  reworked "Auto footer metadata"→"Auto protocol metadata + visible header"; 7 single-line prose reworks —
  build-history/message-layer/persist-list/draft+send-text/report-agent/module-list — `footer`→`header`/`protocol
  metadata`); `docs/next/README.md` + root `README.md` (mirrored byte-identical, 6 reworks: "footer-tagged"→
  "header-tagged", "structured footer"→"protocol metadata + visible header", dropped the now-false "live receipt
  (M3/M4)" claim since ADR 0009 removed the only auto-receipt path); `docs/zynk/cutover-readiness.md` + this
  repo's `CLAUDE.md` ("footer metadata"/"DB/footer/…"→"protocol metadata + visible header"/"DB/header/…"). KEPT
  AS-IS by design: predecessor ADRs 0002/0003/0005 (governance: never rewrite — superseded *by* 0009), the
  append-only history sections of THIS ledger, dated `docs/zynk/plans/*.md` execution records, absence-assert
  literals, the proof-invariant `header/footer/marker/screen/status` enumeration, and unrelated UI/page-layout
  `footer` rects (`src/app/input/`, `src/ui/`, `website/` html/css) + upstream `amp.toml approval_footer`.

### Validation / redeploy
- Full nextest **2340/2340** (`--no-fail-fast`); fmt + clippy `-D warnings` clean; bun test-ts 9; python 49;
  `git diff --check` clean; **zero active message-footer residue** after the R3 doc purge — every remaining
  `footer` hit is in an audited acceptable bucket (UI/page-layout rects, absence-assert literals, the proof
  invariant enumeration, supersession STATUS notes, predecessor ADRs / append-only ledger history / dated plan
  records, retrieval test-fixtures, upstream `approval_footer`). Known pre-existing `named_sessions` load flake under bare fail-fast
  `just check` (passes in isolation + `--no-fail-fast`) — orthogonal.
- **Redeploy (operator-gated, M7-adjacent):** (1) `just build` release → install; (2) **wipe `~/.zynk/zynk.db*`**
  + native re-init (migration checksum changed, DB disposable); (3) reinstall Pi integration to **v4** state-only.

---

# v0.7.0 PORT LEDGER

Port of upstream Herdr **v0.7.0** (`0bf9bb5`) into the zynk hard fork. Approved by operator after swarm review.
Planned, revised, peer-cleared (target + reviewer), and handed off via the decorrelated review process.
Decisions D1–D10 ratified (full zynk identity, modular schema split,
Devin zynk-native, v0.7.0 stable boundary + 41d1c14 doc fold, manual M1 + grouped ports not raw merge,
`ZYNK_WORKSPACE_ID`+`ZYNK_TAB_ID`+`ZYNK_PANE_ID` triple, no Herdr in active UX per D8). Milestone order
Port-M0 → M1 → M2 → M3a → M3b → M4 → M5 → M6 → M7 → M8.

**Framing:** fork base (`58c08b6`) is AHEAD of v0.7.0 on rebrand, so the `base..v0.7.0` diff is mostly
`zynk→herdr` reversion noise. Port = re-apply only the genuine upstream deltas ON TOP of zynk identity; never
apply reversion hunks (this is why raw merge is rejected, D5).

## Every-SHA classification — `d35c642..v0.7.0` (28 commits)

| SHA | subject | class | milestone / handling |
|---|---|---|---|
| fbd20ad | plugin v1 system | PORT-CODE | M1 schema split + M3a host-APIs + M3b plugin |
| de0f43a | state invariants test | PORT-CODE | M7 |
| 7f702a7 | pane identity regression test | PORT-CODE | M7 |
| 17544c3 | macos ci socket determinism | PORT-CODE | M7 (D9: port if code-useful, else skip+reason) |
| 3583a10 | normalize plugin path tests macos | PORT-CODE | M3b |
| 226e873 | preserve tab identity restored sessions | PORT-CODE | M6 |
| eb8c8c5 | warn unknown config sections | PORT-CODE | M6 |
| e1446fa | approve contributor | SKIP-ADMIN | herdr-repo bookkeeping |
| ea3a4db | ci skip duplicate approval replies | SKIP-CI | herdr-repo CI |
| c1ca803 | drop plugin pane records on layout apply | PORT-CODE | M3b (needs layout.apply from M3a) |
| 54b58bb | mark zoomed tabs | PORT-CODE | M6 |
| d3be4f8 | windows plugin support | PORT-CODE | M3b |
| 001a6b2 | plugin authoring docs | PORT-DOC | M7 (rebranded) |
| 3015ce0 | stabilize plugin config dirs | PORT-CODE | M3b |
| c91348e | require plugin min version | PORT-CODE | M3b (rebrand min_zynk_version) |
| dcfc45b | preserve claude session rotations | PORT-CODE | M5 |
| 3e8f906 | ci make preview manual | SKIP-CI | + herdr install/config doc-lines, skip |
| efe7c55 | accept f1-f4 sequences | PORT-CODE | M6 |
| 4ffd99c | compact auto tab labels | PORT-CODE | M6 |
| 08bb076 | kitty keypad input | PORT-CODE | M6 |
| 07261e0 | devin cli detection+restore | PORT-CODE | M4 (+website manifest via befe629) |
| 0099c10 | macos ctrl-click pane links | PORT-CODE | M6 |
| 9805e23 | plugin link-handler ctrl-click docs | PORT-DOC | M7 (plugins.mdx) |
| 16b69b6 | key-combo #613 | PORT-CODE | M6 |
| 99bf8da | approve contributor dmmulroy | SKIP-ADMIN | herdr-repo bookkeeping |
| befe629 | finalize release docs | SPLIT | PORT-ASSET M4 (website/agent-detection/devin.toml+index.toml) + PORT-DOC M7 + SKIP-ADMIN (CHANGELOG/README) |
| 2ace7f2 | key-combo docs + credit | PORT-DOC | M7 (dedupe w/16b69b6) + SKIP-ADMIN (credit) |
| 0bf9bb5 | release v0.7.0 | SPLIT | PORT-VERSION pre-dogfood (`Cargo.toml`/`Cargo.lock` 0.7.0) + SKIP-ADMIN (tag/release finalize) |

## Post-stable `v0.7.0..upstream/master` (4 commits, per D4)

| SHA | subject | class | handling |
|---|---|---|---|
| 4cf9f8e | preview manifest | SKIP-ADMIN | post-stable, fork owns releases |
| 517ca81 | ci preview baseline | SKIP-CI | post-stable CI |
| 41d1c14 | plugin trust/security docs | PORT-DOC | M7 (D4 fold, rebranded) |
| 61ede89 | website manifest v0.7.0 | SKIP-ADMIN | post-stable |

Tally: PORT-CODE 17 · PORT-DOC 4 · PORT-ASSET 1 · PORT-VERSION 1 · SKIP-ADMIN 6 · SKIP-CI 3 = 28 + 4 post-stable, every SHA assigned (SPLIT rows may carry both PORT-* and SKIP-* actions).

## Port-M0 — baseline & guardrails (2026-06-15)

Branch `port/herdr-latest` @ `58c08b6` (clean worktree, no stash). Baseline verified GREEN before any port work:
- `just lint`: fmt `--check` + clippy `--all-targets --locked -D warnings` clean.
- `just test`: nextest **2308 passed / 0 skipped** (6.3s) + python **49 OK**.
- HERDR_* residue floor: `rg 'HERDR_|herdr:|herdr-plugin|min_herdr|herdr-agent-state' src/ tests/` = **0** (Herdr only in NOTICE/LICENSE legal + this ledger/ADR history).
- Protocol `PROTOCOL_VERSION = 14` (`src/protocol/wire.rs:16`); ADR 0008 DB `~/.zynk/zynk.db`; ADR 0009 header/no-footer; receipt boundary intact.

No code touched in Port-M0; this section is the every-SHA gate artifact + green-baseline record. Next: M1 schema modularization.

## Port-M1 — schema modularization (structural half of fbd20ad) (2026-06-15)

Adopted upstream v0.7.0's modular `src/api/schema/` layout (D2). PURE code-move; wire format byte-identical
(2308-test serialization suite stays green). Split the monolithic `src/api/schema.rs` (1647L; only `panes.rs`
was pre-extracted) into: NEW `common.rs` (targets/EmptyParams/SplitDirection/NotificationShow*/AgentStatus/
PaneAgentState), `events.rs`, `response.rs` (`SuccessResponse`/`ErrorResponse`/`ResponseResult` incl the
`ZynkMessageReceived{..}` variant/`AgentManifestInfo`), `server.rs`, `agents.rs`, `integrations.rs`
(`IntegrationTarget` — fork's 12 variants Pi..Cursor, NO Devin yet), `tabs.rs`, `workspaces.rs`,
`worktrees.rs`, fork-only `zynk.rs` (`ZynkMessageReceivedParams`), and empty `plugins.rs` skeleton (filled M3b).
Parent `schema.rs` keeps `struct Request`, full `enum Method` (incl `ZynkMessageReceived` rename
`"zynk.message_received"`), the `pub mod`/`pub use` lists, and inline `mod tests`. NO new methods/types added
(host-API/plugin/devin land with their handlers in M3a/M3b/M4 to keep dispatch exhaustive). `pub use plugins::*`
intentionally omitted until M3b (empty-module glob would trip `-D warnings`). `ToastZynkPosition` left in
`crate::config`. `src/protocol/wire.rs` untouched (PROTOCOL_VERSION 14). No call-site edits (re-exports keep
all consumers working). No `herdr`/`HERDR_` introduced (rg schema/ = 0).

**Validation:** `cargo check --all-targets` 0 errors; `just lint` (fmt + clippy `-D warnings`) clean; `cargo
nextest --no-fail-fast --retries 2` = **2308 passed / 0 skipped** (the `live_handoff` real-server test is a
known env flake, passes on retry). Files: `src/api/schema.rs` + `src/api/schema/{common,events,response,server,
agents,integrations,tabs,workspaces,worktrees,zynk,plugins}.rs` + `panes.rs` (SplitDirection import re-point).

## Port-M2 — PaneLaunchEnv env-injection rewrite (D7) (2026-06-15)

Adopted upstream v0.7.0's `PaneLaunchEnv` abstraction (hotspot #1), replacing the fork's
`apply_pane_env(cmd, pane_id, public_pane_id)`. New in `src/pane.rs`: `struct PaneLaunchEnv { extra, identity:
Option<PaneLaunchIdentity{workspace_id,tab_id,pane_id}> }` + `from_extra`/`with_identity` +
`apply_pane_launch_env(cmd, launch_env)` (applies extra env → `ZYNK_ENV=1` → `apply_pane_base_env` → if
identity, the **D7 triple `ZYNK_WORKSPACE_ID`/`ZYNK_TAB_ID`/`ZYNK_PANE_ID`**). `src/integration/mod.rs`:
`apply_pane_env` → `apply_pane_base_env(cmd)` (socket only) + new consts `ZYNK_TAB_ID_ENV_VAR`,
`ZYNK_WORKSPACE_ID_ENV_VAR`. The 3 `pane.rs` spawn fns + 4 `terminal/runtime.rs` wrappers now take
`&PaneLaunchEnv` (fork's separate `extra_env` folded into `PaneLaunchEnv::extra`); `ZYNK_ENV` moved out of
`apply_pane_terminal_env` into `apply_pane_launch_env` (matches upstream HERDR_ENV placement).

**Critical invariant preserved:** every pane that previously got `ZYNK_PANE_ID` still gets it — each former
real-`public_pane_id` caller now builds `with_identity(workspace_id, tab_id, public_pane_id)`, sourcing
ws/tab ids via new helpers `workspace.rs::launch_env_for_new_pane`/`public_tab_id_for_number` and
`app/ids.rs::pane_launch_env` (resolves public ws/tab/pane ids). Identity-full at the live/agent respawn paths
(`app/api.rs respawn_shell_for_launch_pane`, `app/agent_resume.rs start_pending_agent_resume`,
`persist/restore.rs restore_tab`, `workspace.rs`/`workspace/tab.rs` create+split); identity-less
`PaneLaunchEnv::default()` for the no-public-id helpers (actions/api tests, notifications, mobile, sidebar).

**Validation:** `cargo check --all-targets` 0 err; `just lint` clean; `cargo nextest --no-fail-fast --retries
2` = **2308 passed / 0 skipped**. Invariant tests `pane_launch_env_exports_identity_triple_and_extra` (asserts
ZYNK_WORKSPACE_ID/ZYNK_TAB_ID/ZYNK_PANE_ID + extra + ZYNK_ENV) + `..._without_identity` +
`apply_pane_base_env_exports_zynk_socket_path` all pass. Zero `herdr`/`HERDR_`; protocol 14. Port SHA: fbd20ad
(PaneLaunchEnv refactor co-travels). Files (13): `src/{pane,workspace,integration/mod}.rs`,
`src/workspace/tab.rs`, `src/app/{actions,agent_resume,api,ids}.rs`, `src/persist/restore.rs`,
`src/server/notifications.rs`, `src/terminal/runtime.rs`, `src/ui/{mobile,sidebar}.rs`.

## Port-M3a — non-plugin host APIs (net-new in fbd20ad) (2026-06-15)

Ported the 6 general-purpose "plugin host APIs" fbd20ad added (ABSENT in fork base), each end-to-end so the
`Method` dispatch stays exhaustive: `client.window_title.set`, `client.window_title.clear`,
`pane.process_info`, `layout.export`, `layout.apply`, `pane.current`. Schema: `ClientWindowTitleSetParams` +
`ClientWindowTitleReason` -> `schema/common.rs`; `PaneProcessInfoParams`/`PaneCurrentParams`/`LayoutExportParams`/
`LayoutApplyParams` + result types (`LayoutDescription`/`LayoutNode`/`LayoutPane`/`PaneProcessInfo`) ->
`schema/panes.rs`; `ResponseResult::{ClientWindowTitle,PaneCurrent,PaneProcessInfo,LayoutExport,LayoutApply}`
-> `schema/response.rs` (wire `type` serde-derived, e.g. `"client_window_title"`); 6 `Method` variants ->
parent `schema.rs`; `api_method_name` arms -> `api/server.rs`; `request_changes_ui` classifier gains
`Method::LayoutApply(_)` -> `api/mod.rs`. Handlers: NEW `src/app/api/layouts.rs` (`handle_layout_export`/
`handle_layout_apply`/`resolve_layout_export_target`) + NEW `src/app/api/env.rs` (`normalize_launch_env`, fork
lacked it); `handle_pane_current`/`handle_pane_process_info` -> `app/api/panes.rs`; window-title ->
`server/headless.rs::handle_client_window_title_api` + net-new client OSC-0 path (`client/mod.rs`,
`protocol/wire.rs` `ServerMessage::WindowTitle`). To make `layout.apply` thread caller env, added env-accepting
`Workspace`/`Tab` split/create variants (existing callers pass `Vec::new()`); incidentally fixed a latent
fork bug where the argv split path dropped the requested ratio.

**C1ca803 DEFERRED to M3b:** `handle_layout_apply` + `rollback_layout_tab` carry `// M3b: drop plugin pane
records ...` TODOs; upstream test `layout_apply_replace_drops_plugin_pane_records_of_replaced_tab` SKIPPED
(other 3 layout tests + 2 env tests ported). **Rebrand:** window-title default = `"zynk"` (clear restores
"Zynk's default title"); ported test `HERDR_ROLE`->`ZYNK_ROLE`; response type literal unchanged.

**Validation:** `cargo check --all-targets` 0 err/0 warn; `clippy -D warnings` clean; `cargo nextest
--no-fail-fast --retries 2` = **2315 passed / 0 skipped** (+7: 4 layout, 2 env, 1 window-title). Zero
`herdr`/`HERDR_`/`herdr api`; protocol 14. Port SHA: fbd20ad (host-API half). Files (17): 15 mod + NEW
`src/app/api/{env,layouts}.rs`.

## Port-M3b — plugin v1 subsystem + plugin-pane lifecycle (2026-06-15)

Ported the entire plugin v1 subsystem (fork had NONE). SHAs: fbd20ad (plugin half) + 3015ce0 (config dirs) +
c1ca803 (pane lifecycle) + 3583a10 (path tests) + d3be4f8 (windows plugin) + c91348e (min version). NEW:
`src/app/api/plugins/{mod,manifest,env,context,runtime,panes}.rs`, `src/persist/plugin_registry.rs`
(`plugins.json` beside `session.json`), `src/cli/plugin.rs` (`zynk plugin` link/list/unlink/enable/disable/
action/log/install), `src/plugin_paths.rs` + `src/plugin_command.rs` (brand-neutral, inherit zynk root),
`tests/fixtures/plugin-smoke/zynk-plugin.toml`. Filled `src/api/schema/plugins.rs` (M1 skeleton) + `pub use
plugins::*`; 11 `Method::Plugin*` variants (plugin.link/list/unlink/enable/disable/action.list/action.invoke/
log.list/pane.open/pane.focus/pane.close) + `ResponseResult::Plugin*` + `api_method_name` + `request_changes_ui`
arms. Plugin-pane lifecycle in `app/actions.rs` (`plugin_panes` via `app/state.rs` `PluginPaneRecord`,
`pane_ids_for_workspace`/`_for_tab`/`remove_plugin_pane_records`, `PluginCommandFinished` arm, cleanup in
close_pane/tab/workspace + handle_pane_died); **the two M3a `layouts.rs` TODOs are now filled** (remove plugin
pane records on layout.apply replace + rollback) and the previously-skipped test
`layout_apply_replace_drops_plugin_pane_records_of_replaced_tab` is ported + passing. Event hooks via
`schema/events.rs` (`PLUGIN_HOOK_EVENT_KINDS`, `plugin_hook_event_names`).

**Rebrand (D1/D6/D8):** `zynk-plugin.toml` (manifest filename, 5 files), `min_zynk_version`
(field/key/`validate_min_zynk_version`/error code `invalid_plugin_min_zynk_version`/all messages), all 11
`ZYNK_PLUGIN_*` env vars + `starts_with("ZYNK_PLUGIN_")` allowlist filters (no legacy herdr dual-read, ADR 0007).
**D8:** no shipped `herdr-plugin-examples` default existed to ship — `plugin install` REQUIRES explicit
`owner/repo` and errors otherwise (zynk message); the repo name appeared only in `#[cfg(test)]` fixtures,
rebranded to neutral `example/plugin-examples`. **Fork divergence:** fork base has no worktree events, so 3
worktree-keyed plugin-hook EventData arms were dropped + the affected test fixtures retargeted to valid fork
events (`pane.created`/`workspace.focused`).

**Validation:** `cargo check --all-targets` 0 err/0 warn; `clippy -D warnings` clean; `cargo nextest
--no-fail-fast --retries 2` = **2378 passed / 0 skipped** (+63 plugin tests). `rg
'HERDR_|herdr:|herdr-plugin|min_herdr|HERDR_PLUGIN' src/ tests/` = 0 (case-insensitive `herdr` in all new
plugin files = 0). Protocol 14. Files (28): 22 mod + 6 new (`src/app/api/plugins/` dir, `src/cli/plugin.rs`,
`src/persist/plugin_registry.rs`, `src/plugin_{paths,command}.rs`, `tests/fixtures/plugin-smoke/`).

## Port-M4 — Devin zynk-native integration (07261e0 + befe629 website manifest) (2026-06-15)

Ported Devin (fork had none) as a zynk-native integration, modeled on the fork's cursor integration
(session-only, no `.ps1`). NEW: `src/integration/assets/devin/zynk-agent-state.sh` (`ZYNK_INTEGRATION_ID=devin`,
`ZYNK_INTEGRATION_VERSION=1`, env `ZYNK_ENV`/`ZYNK_SOCKET_PATH`/`ZYNK_PANE_ID`/`ZYNK_HOOK_INPUT_FILE`/
`ZYNK_DEVIN_LIST_JSON`, python `SOURCE = "zynk:devin"`; kept verbatim the `devin list --format json` workdir
session-id resolution, `DEVIN_PROJECT_DIR`, session-only report, UserPromptSubmit/SessionStart-startup fallback
suppression); `src/detect/manifests/devin.toml` (verbatim, 8 brand-neutral rules); `website/agent-detection/
devin.toml` + `index.toml` registration (from befe629). Wiring (mirrors cursor): `detect/manifest.rs`
BUNDLED row; `detect/mod.rs` `Agent::Devin` + `ALL 17→18` + label + aliases (`devin`/`devin-cli`/`devin cli`,
NOT in `full_lifecycle_hook_authority`); `integration/mod.rs` `DEVIN_*` consts + `install_devin`/`uninstall_devin`/
`devin_dir` (XDG→`~/.config/devin`, `config.json`) + match arms + `integration_specs 12→13`, NOT in
`integration_target_supported` (available-by-command-presence); `api/schema/integrations.rs` `IntegrationTarget::Devin`;
`agent_resume.rs` `zynk:devin` in 2 source classifiers + resume plan `["devin","--resume",id]`;
`cli/integration.rs` parse + usage; `config/sound.rs` devin field; `config/model.rs` cjk doc. `DEVIN_INTEGRATION_VERSION=1`.

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; `cargo nextest --no-fail-fast
--retries 3` = **2385 passed / 0 skipped** (+7 devin; `live_handoff` real-server flake settled on retry);
`just test-ts` 9/0; `python scripts.test_agent_detection_manifest_check` 5 OK. `rg
'HERDR_|herdr:|herdr-agent-state|herdr-devin' src/ website/ tests/` = 0. Protocol 14. Port SHAs: 07261e0 +
befe629 (website devin manifest). Files (15): 12 mod + 3 new (devin asset, `detect/manifests/devin.toml`,
`website/agent-detection/devin.toml`).

## Port-M5 — Claude hook v6 session-rotation (dcfc45b) (2026-06-15)

Ported dcfc45b end-to-end (`CLAUDE_INTEGRATION_VERSION` 5->6): a `session_start_source` data path that stops
nested/child `SessionStart` events from clobbering the active claude session ref. **Assets** (both bumped
`# ZYNK_INTEGRATION_VERSION=6`): `claude/zynk-agent-state.sh` python now adds `agent_session_path`
(<-`transcript_path`) + `session_start_source` (<-`source`, SessionStart only) to the `pane.report_agent_session`
params; `claude/zynk-agent-state.ps1` switched to a `$args` array (`& zynk @args`) with conditional
`--agent-session-path`/`--session-start-source`. **Server path (net-new in fork):** `cli/pane.rs`
`--session-start-source` flag; `api/schema/panes.rs` `PaneReportAgentSessionParams.session_start_source:
Option<String>` (serde default+skip_none); `agent_resume.rs` `normalize_claude_session_start_source` (whitelist
startup/resume/clear/compact); `app/api/panes.rs` normalizes when building the event; `events.rs`
`AgentSessionReported.session_start_source`; `app/actions.rs` calls new
`terminal.set_agent_session_ref_for_session_start(...)`; `terminal/state.rs` adds that method +
`conflicting_current_session_ref(session_start_source)` arg + `session_start_source_allows_session_replacement`.

**Critical correctness:** the replacement gate uses the FORK literal `source == "zynk:claude"` (NOT upstream's
`herdr:claude`) && agent==claude && source in {clear,resume,compact} — so `SessionStart source="startup"` does
NOT replace the session ref; only clear/resume/compact rotations do. `set_agent_session_ref` retained
(delegates with `None`) so non-claude agents are unaffected.

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; `cargo nextest --no-fail-fast
--retries 3` = **2388 passed / 0 skipped** (+3; ported startup-rejected / clear-resume-compact-accepted /
normalize-whitelist tests + CLAUDE_HOOK_ASSET content asserts). `just test-ts` 9/0. `rg
'HERDR_|herdr:|herdr-agent-state' src/` = 0. Protocol 14. Port SHA: dcfc45b. Files (11): claude `.sh`+`.ps1`,
`integration/mod.rs`, `cli/pane.rs`, `api/schema/panes.rs`, `api/schema.rs`, `agent_resume.rs`,
`app/api/panes.rs`, `events.rs`, `app/actions.rs`, `terminal/state.rs`.

## Port-M6 — stability / input / UI fixes (grouped) (2026-06-15)

Ported 8 upstream fix commits' logic onto the fork (manual, reconciled with M1-M5):
- **54b58bb** mark zoomed tabs — `ui/tabs.rs` `Z` marker (later reshaped by 4ffd99c).
- **0099c10** ctrl-click captured pane links on macOS — `app/input/mod.rs` `modified_url_click_modifier()` always `CONTROL` (SGR mouse can't observe Cmd); `app/input/terminal.rs` + `raw_input.rs` tests.
- **08bb076** kitty keypad — `input/parse.rs` codepoints 57399-57416 (keypad 0-9 + `. / * - + = ,`) + corpus fixtures.
- **efe7c55** alternate F1-F4 — `input/parse.rs` `\x1b[11~`..`[14~` alias F1-F4.
- **16b69b6** key-combo API send-keys (#613) — `app/api_helpers.rs::parse_api_key` delegates to `config::parse_key_combo` (made `pub(crate)`); RECEIPT BOUNDARY PRESERVED: `encode_api_keys` validates all keys up front (invalid -> `invalid_key`, nothing written), success writes PTY bytes + returns `Ok{}`, no receipt/delivery-status mutation -> send-keys stays "submitted".
- **eb8c8c5** warn on unknown config sections — `config/io.rs` `KNOWN_TOP_LEVEL_CONFIG_KEYS` (11 upstream + fork's `zynk` = 12) + diagnostics on load + live-reload.
- **4ffd99c** compact auto tab labels — removed `Tab::display_name`, `Workspace::tab_display_name` uses `tab_idx+1` for auto names; 12 call sites updated.
- **226e873** preserve tab identity across restored sessions — `app/ids.rs` `parse_tab_id` resolves encoded `:tN` by stable `tab.number` but bare `:N` by position; `public_tab_id_for_index` log routing.

**Reconciliation:** 4ffd99c (chronologically latest) supersedes the tab-label parts of 54b58bb/226e873; kept
4ffd99c's final `number: tab_idx+1` + compact labels, skipped one 226e873-era test
(`tab_info_number_uses_stable_public_tab_number`) that 4ffd99c reverted. Rebranded ported-test strings
(`min_herdr_version`->`min_zynk_version`, `herdr-plugin.toml`->`zynk-plugin.toml`, `delivery="herdr"`->`"zynk"`,
snapshot source `herdr:codex`->`zynk:codex`, temp filenames).

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; `cargo nextest --no-fail-fast
--retries 3` = **2406 passed / 0 skipped** (+18). `rg 'HERDR_|herdr:' src/` = 0. Protocol 14. Port SHAs:
54b58bb, 0099c10, 08bb076, efe7c55, 16b69b6, eb8c8c5, 4ffd99c, 226e873. Files (24).

## Port-M7 — test hardening + docs/provenance (2026-06-15)

**Tests:** `de0f43a` state invariants — `AppState`/`Workspace` `test_*_adversarial_identity_state()` +
`assert_invariants_for_test()` + adversarial-mutation unit tests + AGENTS.md refactor-risk note (rebranded).
`7f702a7` pane-identity regression — extended `assert_invariants_for_test()` with alias/projection checks
(pane_id_aliases, public_pane_id_aliases, toast target, plugin_panes, copy_mode/selection/drag/context_menu);
added invariant asserts to ~17 actions tests; `server_start_restores_legacy_session_through_api_identity`
(cli_wrapper.rs); client_mode.rs cleanup (faithful to upstream: removes 10 obsolete tests, adds 3 -> net -7
test count). **D9 `17544c3`:** PARTIAL — the macOS-only canonical-path half was already present (M3); the
socket-readiness hardening is useful on Linux too (race: socket file exists before accept()) so ported it
(`wait_for_file`->require `UnixStream::connect().is_ok()`; `wait_for_file`->`wait_for_socket` in
client_mode/detach_reattach). Pure-macOS-CI scaffolding skipped (no macOS CI here).

**Docs (rebranded, both `docs/next/website/` + `website/` mirror + root):** plugins.mdx verified (M3b port of
fbd20ad/001a6b2/9805e23/41d1c14 authoring+ctrl-click+trust/security); synced devin content to docs/next mirror;
Claude integration version 5->6; cli-reference.mdx key-combo + metadata-normalization + Plugins CLI section +
ZYNK_ENV/PANE_ID/TAB_ID/WORKSPACE_ID env table; socket-api.mdx pane.current/process_info/layout.export+apply/
client.window_title docs + key-combo note; zynk-voiced v0.7.0 port CHANGELOG entry (docs/next + root). SKIPPED
release/website manifests + contributor bookkeeping (per ledger).

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; `cargo nextest --no-fail-fast
--retries 3` = **2399 passed / 0 skipped** (net -7, faithful to 7f702a7); `just test-ts` 9/0; python
maintenance 49 OK; `just release-docs-check` PASS (all mirrors identical). herdr in active docs/src/tests = 0
(only `docs/zynk/fork-patch-ledger.md` provenance retains herdr strings, by design). Protocol 14. Port SHAs:
de0f43a, 7f702a7, 17544c3(partial), 001a6b2, 9805e23, 41d1c14, 2ace7f2, befe629(doc-content). Files (29).

## Port-M8 — final validation + dogfood gate (2026-06-15)

v0.7.0 port COMPLETE. All milestones green + SSH-signed on `port/herdr-latest` over base `58c08b6`:
`e3e4a1c` M0 · `1f32380` M1 · `b536fb3` M2 · `4654104` M3a · `421327d` M3b · `957da05` M4 · `2653e4d` M5 ·
`bf49a8c` M6 · `2d4edb2` M7.

**Final gate run (no code changes in M8):**
- `just lint` (fmt + clippy `--all-targets -D warnings`): clean.
- `cargo nextest --locked --no-fail-fast --retries 3`: **2399 passed / 0 skipped**.
- `just test-ts`: 9/0. Python maintenance (4 suites): 49 OK. `just release-docs-check`: PASS.
- **Zero-herdr floor:** `git ls-files | xargs rg -lI herdr` excl NOTICE/LICENSE + this ledger + `docs/zynk/
  decisions/` = CLEAN; `HERDR_|herdr:|herdr-plugin|min_herdr|herdr-agent-state` in src+tests = 0.
- **Invariants held:** protocol `PROTOCOL_VERSION=14`; ADR 0008 DB `~/.zynk/zynk.db` (`sqlite_home`/`DB_FILE_NAME`
  preserved — v0.7.0's `ZynkConfig` deletion correctly NOT ported, risk R1); ADR 0009 visible header / no
  message-footer (0 footer-machinery hits); receipt boundary (send-keys/messages stay "submitted", never fake
  receipt); plugin registry persists to `plugins.json` (not the DB).

**Every-SHA gate satisfied:** all 28 (`d35c642..v0.7.0`) + 4 post-stable SHAs classified & actioned —
PORT-CODE 17, PORT-DOC 4, PORT-ASSET 1, PORT-VERSION 1, SKIP-ADMIN 6, SKIP-CI 3 (see Port-M0 table; SPLIT rows
may carry both PORT-* and SKIP-* actions); each PORT recorded in its Port-M* section.

**DOGFOOD: GATED — operator-triggered ONLY, NOT performed.** Per the handoff, no live install / no autonomous
dogfood / no push / no tag / no release. Tree is local-only on `port/herdr-latest`; the live zynk runtime +
`~/.zynk` DB are untouched by this work. Awaiting operator decision on dogfood/install/release.

## Port-M8a — package version alignment (2026-06-16)

Operator caught a pre-dogfood UX gap after install: `zynk --version` still printed `0.6.10` even though the
v0.7.0 feature port was complete. Root cause: upstream `0bf9bb5 release: v0.7.0` had been classified as
pure `SKIP-ADMIN`, so the release/tag/changelog finalization skip also skipped the package version bump.
Corrected classification to `SPLIT`: port only the package-version bump (`Cargo.toml` + `Cargo.lock` to
`0.7.0`) while continuing to skip upstream release/tag/admin automation. No protocol/schema/runtime behavior
changed.

---

# FEATURE #107 — trace_id first-class + header v2 (net-new zynk, post-port)

Operator-approved after dogfood PASS. Plan `ZYNK_TRACE_HEADER_PLAN2_...012948Z`, impl `...IMPL_20260616T014152Z`.
Decisions: Q1 hide reply+note by default (verbose escape hatch) · Q2 trace_id free-form printable ≤128, reject
control chars w/ explicit error · Q3 `--trace inherit` from parent/derived-target trace, null-safe, no invented
conversation trace · Q4 closed unicode-width box, cap 100, middle-ellipsis path / tail-ellipsis ids · Q5 DEFER
conversation-level trace · Q6 DEFER trace_refs (no CLI/index).

## IM1 — trace storage + CLI (2026-06-16)

Per-message `trace_id` stored in `messages.meta_json` ONLY (NOT protocol_json, NOT body). `SendAttempt` +
`PersistedSend` gain `trace_id: Option<String>` (record carries it for IM3 header); the messages INSERT builds
`meta_json` escape-safely via `serde_json::json!({"trace_id": id})` when present, else `'{}'`
(`src/zynk/persistence.rs`). `validate_trace_id` (`src/zynk/message.rs`): trim, reject empty/>128/control-chars
with explicit CLI error. `--trace <id>` + `--trace inherit` wired on the 4 message-producing commands —
`zynk send`/`zynk reply` (`cli/native.rs`), `pane run`/`pane send-text` (`cli/pane.rs`) — via leading-option
parser `parse_type_trace_and_text` (interleaves with `--type`, `--` stops, last-wins). `--trace inherit`
reads the parent/derived-reply-target's `meta_json.trace_id`; no parent trace → sends WITHOUT trace + concise
stderr note (never invents a conversation trace). Invalid explicit `--trace` → `code=invalid_trace_id`, sends
nothing. body/body_hash/protocol_json/FTS UNCHANGED. No header change yet (IM3). Q5/Q6 deferred.

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; `cargo nextest --no-fail-fast
--retries 3` = **2412 passed / 0 skipped** (+12: trace-persist/escape-safe/inherit-null-safe + validate +
parser). herdr 0; protocol 14. Files (9): `cli/{agent,native,pane}.rs`,
`zynk/{message,persistence,header,receipt,embedding_worker}.rs`, `zynk/retrieval/vector.rs`.

## IM2 — trace query/filter/surface + partial index (2026-06-16)

NEW migration `migrations/zynk/0003_trace_index.sql`: partial expression index `idx_messages_trace_id ON
messages(json_extract(meta_json,'$.trace_id')) WHERE ... IS NOT NULL` (sqlx Migrator, idempotent, additive;
old rows unindexed). `QueryFilters` gains `trace_id`; both `retrieval/fts.rs` + `retrieval/vector.rs` (kept
MIRRORED) add `AND (? IS NULL OR json_extract(m.meta_json,'$.trace_id') = ?)` prefilter + `... AS trace_id`
SELECT col. `QueryHit` + `ThreadMessage` (shared by thread+inbox, `zynk/inbox.rs`) gain `trace_id`
(skip_serializing_if none) → surfaced in query/thread/inbox `--json` when present, omitted when absent. CLI
`--trace <id>` parsed in `run_query_command` (`cli/native.rs`) + the legacy `cli/zynk.rs` query builder
(validated via IM1 `validate_trace_id`). **`zynk trace <id> [--json]` ADDED** (small/clean): read-only,
runtime-scoped, lists all messages with a trace across conversations via the new index (mirrors `run_inbox`,
dispatch arm `cli.rs:82`). body/body_hash/protocol_json/FTS UNCHANGED.

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; `cargo nextest --no-fail-fast
--retries 3` = **2418 passed / 0 skipped** (+6: migration applies+idempotent+partial, trace-prefilter
excludes other/no-trace rows, JSON include/omit, native e2e). herdr 0; protocol 14 (wire untouched). Files
(10): NEW `migrations/zynk/0003_trace_index.sql` + `cli.rs`, `cli/{native,zynk}.rs`, `zynk/inbox.rs`,
`zynk/retrieval/{fts,mod,vector}.rs`, `tests/{zynk_embed,zynk_native_cli}.rs`.

## IM3 — Header v2 closed content-width box (2026-06-16)

Rewrote `src/zynk/header.rs::render_header` from the fixed-width LEFT RAIL into a CLOSED content-width box.
`inner = widest_field_display_width.clamp(title_floor, max_width-4)` measured with `unicode_width` DISPLAY
columns (not byte/char len); top `╭─ Zynk message ─…─╮`, interior `│ <content padded to inner> │`, bottom
`╰─…─╯` — all rows equal display width (right border aligned). **Default lines (Q1):** from, to, optional type,
id, conv, and `trace: <id>` ONLY when `record.trace_id` is Some; `reply:` + `note:` HIDDEN by default. **Verbose**
re-adds reply+note via `[header] verbose=true` config OR `ZYNK_HEADER_VERBOSE=1` env. **Truncation (Q4, never
wrap):** `truncate_middle` for cwd/paths, `truncate_tail` for ids/trace/conv, display-width-aware, 1 col reserved
for `…`, glyph-safe. **Config:** new `HeaderConfig{verbose=false, max_width=100}` `[header]` section
(`config/model.rs`); `MIN_HEADER_MAX_WIDTH=24` floor used in clamp; `render_header(.., HeaderOptions)` resolved
at the 3 call sites (pane/native/agent) via env_first + live Config. **Wire-only UNCHANGED:** header (incl trace
line) only prepended to delivery text; `messages.body`/`body_hash`/FTS exclude it.

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; `cargo nextest --no-fail-fast
--retries 3` = **2431 passed / 0 skipped** (+13: closed-box invariant, default-hides-reply/note, verbose-shows,
trace-iff-Some, CJK/emoji alignment, middle/tail truncation, clamp, wire-only purity, config resolve/env). herdr
0; protocol 14. Files (8): `zynk/header.rs`, `config/model.rs`, `config.rs`, `cli/{pane,native,agent}.rs`,
`zynk/persistence.rs`, `tests/zynk_header.rs`.

## IM4 — docs + final validation (2026-06-16) — #107 COMPLETE

DOCS ONLY (no src/). Both `website/` + `docs/next/website/` mirrors edited byte-identically:
`cli-reference.mdx` (`--trace <id>`/`--trace inherit` on send/reply/pane run/send-text; `query --trace`; new
`zynk trace <id> [--json]`; new "Conversation messages" section; `ZYNK_HEADER_VERBOSE` env row),
`configuration.mdx` (new `[header]` section: `verbose` default false, `max_width` default 100 floor 24,
`ZYNK_HEADER_VERBOSE` override; Header v2 closed-box note), `socket-api.mdx` (trace_id rides meta_json, surfaced
in thread/inbox/query JSON, body-purity/receipt framing intact). CHANGELOG entry (root + docs/next) for #107.
Every claim verified against code/`--help` (no invention).

**#107 FINAL VALIDATION:** `just lint` clean; `cargo nextest --no-fail-fast --retries 3` = **2431 passed / 0
skipped**; `just test-ts` 9/0; `just release-docs-check` PASS; herdr in active docs/src/tests = 0; protocol 14;
ADR 0008 DB + ADR 0009 wire-only header + receipt boundary intact. Commits: IM1 fff52a9 · IM2 1da57c8 ·
IM3 b72ee89 · IM4 (this). Feature #107 (trace_id correlation + header v2) COMPLETE, local-only, no push/tag/
release/install/dogfood.

## #107 review fixes (Codex CONCERN + addendum A/B/C + pR) (2026-06-16)

Decorrelated review returned CONCERN; fixed all blockers.
- **Docs scope (commit 94a86fe):** per AGENTS.md unreleased feature docs belong in docs/next ONLY; reverted the
  #107 stable/root doc edits (root `CHANGELOG.md` + `website/src/content/docs/{cli-reference,configuration,
  socket-api}.mdx`) to pre-#107 (b72ee89); #107 docs stay in `docs/next/*`. Stable<->next divergence is expected
  for feature work (no release-docs-check mirroring). Fixed docs/next socket-api cross-link -> `/docs/cli-reference/`.
- **A) Help text (required):** `--trace <id|inherit>` added to usage/help for `zynk send`/`reply` (cli/native.rs),
  `pane run`/`pane send-text` (cli/pane.rs); `zynk query` already had `--trace <id>`; top-level `--help`
  (src/main.rs) now lists `send/reply/thread/inbox/query/trace/whoami/who`. Test
  `cli_wrapper.rs::help_usage_advertises_trace_flags` drives the real binary.
- **B) [header] config registration (pR):** added `"header"` to `KNOWN_TOP_LEVEL_CONFIG_KEYS` (config/io.rs) so
  startup + live-reload no longer warn "unknown section"; `load_live_config_from_str` now carries the parsed
  `[header]`. `[header]` is CLIENT-SIDE (read per-send by `resolve_header_options()`, not server-live-reloaded) —
  documented in code + docs/next configuration.mdx. Tests: load-live carries header w/o warning; resolved
  HeaderOptions honor `[header]`.
- **C) #117 live-DB isolation (P0):** root cause = `zynk server` boot runs `open_migrated()`, and 8 integration
  suites spawned the bin WITHOUT `ZYNK_SQLITE_HOME`/`env_remove(ZYNK_HOME)`, so a new fork migration hit the live
  `~/.zynk/zynk.db`. Fixed: (1) `db_path.rs` `#[cfg(test)]` net redirects the default-home branch to a per-pid temp
  when no sqlite_home/ZYNK_SQLITE_HOME/ZYNK_HOME set; (2) every bin/server spawn in the 8 suites
  (api_ping/auto_detect/client_mode/cross_area/detach_reattach/live_handoff/multi_client/server_headless) now sets
  `ZYNK_SQLITE_HOME` + scrubs inherited `ZYNK_HOME`; (3) regression `zynk_db_cutover.rs::
  inherited_live_env_cannot_defeat_test_db_isolation` proves an inherited live `ZYNK_HOME` is overridden;
  (4) read-path (`open_query_readonly` migrates-then-query_only) documented, isolation makes it test-safe.

**Validation:** `cargo check --all-targets` 0/0; `clippy -D warnings` clean; **`cargo nextest --no-fail-fast
--retries 3` with NO ZYNK_SQLITE_HOME (the exact original leak condition, default target) = 2437 passed / 0
skipped, and the live `~/.zynk/zynk.db` stayed at migrations `1 2` with NO `idx_messages_trace_id` BEFORE and
AFTER** — proving the harness self-isolates. herdr 0; protocol 14. Files (18): A cli/{native,pane}.rs+main.rs;
B config/io.rs+header.rs+docs/next/configuration.mdx; C db_path.rs+db.rs+8 test harnesses+zynk_db_cutover.rs;
+ cli_wrapper.rs.

## #116 — Settings UI for [header] (net-new zynk) (2026-06-16)

First-class Settings UI to edit the #107 `[header]` config. Operator decisions: Q1 new section, Q2 stepper ±8
[24,200], Q3 env-override indicator, Q4 cap 200 (floor 24 unchanged). New `SettingsSection::Header`
(`app/state.rs`, in `ALL`, label "header") + `HeaderSettingRow{Verbose,MaxWidth}` + AppState
`header_verbose()`/`header_max_width()` accessors (populated by `apply_config_from_disk`); `MAX_HEADER_MAX_WIDTH=200`
const (config/model.rs, re-exported config.rs). Render arm `render_settings_header` (`ui/settings.rs`): row0
verbose toggle with dim `(overridden by env)` when `ZYNK_HEADER_VERBOSE` truthy, row1 `max_width < N >` stepper,
+ hint "Applies to outgoing zynk message headers (per-send). ZYNK_HEADER_VERBOSE env overrides verbose." Input
(`app/input/settings.rs`): Up/Down rows, Enter/Space toggles verbose, Left/Right step max_width ±8 clamp [24,200];
new `SettingsAction::{SaveHeaderVerbose(bool),SaveHeaderMaxWidth(usize)}` -> `app/input/mod.rs` ->
`app/config_io.rs::save_header_verbose`/`save_header_max_width` (clamp [24,200], `upsert_section_value(content,
"header", ..)` + `apply_config_from_disk` — preserves other sections, no server reload; per-send client-side).

**Invariants:** #107 untouched — `resolve_header_options` precedence (env > config) + `MIN_HEADER_MAX_WIDTH=24`
floor unchanged (200 cap bounds only the Settings write path); body-purity/trace-meta_json-only/receipt/wire-only/
protocol 14 intact. Docs: `docs/next/configuration.mdx` only (stable untouched, AGENTS.md scope).

**Validation:** `cargo check --all-targets` 0/0; `fmt --check` clean; `clippy -D warnings` clean; `cargo nextest
--no-fail-fast --retries 3` (default target) = **2444 passed / 0 skipped** (+ operator-run just test PASS 2444/0 +
49 python). #116 tests: writeback preserves other sections + clamp 10->24/999->200; Header in ALL; verbose
Enter-toggle; max_width ±8 clamp; env-override indicator; `resolve_header_options` honors UI-written config + env
wins. herdr 0; protocol 14. **Live ~/.zynk/zynk.db stayed `1 2 3`** (isolation holds). Files (10).

## #116 review fix — settings tab-overflow (Codex CONCERN) (2026-06-16)

Adding the 7th tab (`Header`) made the settings tab row overflow: `SETTINGS_POPUP_WIDTH=76` -> inner 74, but the
7 tabs (label + `.padding(" "," ")` 2 cols + 1-col dividers) summed to 76 > 74 -> rightmost (Experiments)
clipped, and `settings_tab_at` mouse math ran past the visible width. Fix (Codex's preferred minimal option,
keeps the padded style — did NOT drop padding globally): shorten ONLY the `PaneLabels` TAB label
`"pane labels"`->`"labels"` (the section content title stays "agent border labels") -> row = 71 (no badge) /
73 (Integrations badge) <= 74. Added a single source-of-truth `SettingsSection::tab_cell_width(has_badge)`
(label display-width via unicode_width + 2 padding + 2 badge) used by BOTH the `settings_tab_at` hit-test and a
test, and a `x >= inner.x+inner.width` bound in `settings_tab_at` so it never maps a column past the visible
tab row (the actual mouse-overflow Codex flagged). Note: widening the modal is futile — `centered_popup_rect`
caps the popup at `area.width-4`.

**Tests (RED pre-fix -> GREEN):** `settings_tab_row_fits_inner_width` (sum tab_cell_width + dividers <= inner,
with+without badge), `settings_tab_at_rightmost_reachable_and_within_inner` (rightmost tab clickable; cols past
inner -> None), `settings_tab_row_renders_rightmost_label_without_clipping` (buffer-render: "experiments" not
clipped). The pre-existing `settings_tab_hit_area_includes_integration_update_badge` + the 3 new tests now set
`view.terminal_area=80x24` (default test area was 0x0; the OLD hit-test was unbounded — the very bug — so it
"passed" on a 0-wide inner). **Validation:** cargo check 0/0; fmt clean; clippy `-D` clean; nextest **2447/0**;
herdr 0; protocol 14; live DB `1 2 3`. No `allow(unused*)` added. Files (3): `app/state.rs`,
`app/input/settings.rs`, `ui/settings.rs`.

## #116 hardening — settings_tab_at ultra-narrow edge guard (Codex pZ, non-blocking) (2026-06-16)

Codex approved #116 but suggested a robustness nit: on a terminal narrower than the 80-col-safe case,
`centered_popup_rect` caps the popup so a tab cell can START before the visible edge but STRADDLE it; the
per-cell `x >= tab_row_end` break wouldn't stop a `col` inside that straddling cell. Added an explicit early
guard in `settings_tab_at` — `let tab_row_end = inner.x + inner.width; if col >= tab_row_end { return None; }`
— BEFORE the per-cell loop, so any column at/after the visible row end returns `None` regardless of a
partly-visible cell. New focused test `settings_tab_at_none_at_visible_edge_even_when_a_cell_straddles_it`
(26x30 terminal -> capped popup -> asserts a cell genuinely straddles the edge, then `col==edge` and beyond ->
`None`, inner col still resolves). Validation: cargo check 0/0; fmt clean; clippy `-D` clean; nextest **2448/0**;
herdr 0; protocol 14; live DB `1 2 3`; no `allow(unused*)`. Files (1): `app/input/settings.rs`.

## #114 — integration status native-identity gate (Copilot false-positive) (2026-06-16)

Incident: a stale Herdr-era `~/.copilot/hooks/zynk-agent-state.sh` (source `herdr:copilot`, `HERDR_*` residue)
falsely showed "copilot: current (v2)". Root cause: `integration_status_at` (src/integration/mod.rs) marked
`Current` on a SINGLE conjunct — `installed_version >= expected_version` — so any hook whose
`ZYNK_INTEGRATION_VERSION=` marker parsed at/above the expected version was "current", regardless of identity or
foreign residue. Fix (operator-approved Q1/Q2/Q3): `Current` now requires THREE conjuncts — `version >= expected`
**AND** the hook declares the correct `ZYNK_INTEGRATION_ID=<id>` for that target **AND** carries no Herdr residue
(`HERDR_` / `herdr:`). A present-but-non-native hook (stale Herdr, foreign id, or missing id) is `Outdated` ->
`needs_install()` -> CLI prompts `zynk integration install <agent>`, whose reinstall overwrites it with the
native hook. New `const INTEGRATION_ID_MARKER`; helpers `hook_has_herdr_residue`, `hook_is_native` (comment-prefix
stripping mirrors `parse_integration_version`, so it works across `.sh`/`.ps1`/`.ts`/`.js`/`.py`), and an EXPLICIT
`expected_integration_id(target)` (per-target match, NOT derived from the display label — Q2 — so a label rename
can't silently weaken the gate). `integration_status_at` reads hook content once. All 13 `integration_specs()`
status paths embed `ZYNK_INTEGRATION_ID=<label>` (verified incl. hermes -> `__init__.py`), so the gate applies
uniformly. **Adjacent (approved):** deduped the copy-paste artifact in `remove_legacy_pi_extension_from_omp_dir`
(`content.contains("ZYNK_INTEGRATION_ID=pi") || content.contains("ZYNK_INTEGRATION_ID=pi")` -> single clause) —
behavior-preserving (`A || A` == `A`), covered by the existing `install_omp_removes_legacy_pi_*` +
`install_omp_preserves_non_zynk_file_*` tests.

**Tests (RED pre-fix -> GREEN):** `stale_herdr_copilot_v2_hook_is_outdated_not_current`,
`copilot_v2_hook_missing_integration_id_is_outdated`, `copilot_v2_hook_with_foreign_integration_id_is_outdated`
(all three returned `Current` pre-fix — verified RED), `native_copilot_v2_hook_is_current` (regression guard),
`hook_has_herdr_residue_flags_legacy_tokens`, `hook_is_native_requires_matching_id_and_no_residue`, and
`all_native_status_assets_pass_identity_gate` (enumerates all 13 native status assets, asserts each passes the
gate AND embeds its marker, and `assets.len() == integration_specs().len()` so the list must grow with new
integrations). Existing status tests (claude/codex/copilot/droid Outdated; cursor Current) unaffected — their
fixtures already carry the correct id + no herdr. **Validation:** `fmt --check` clean; `clippy --all-targets -D
warnings` clean (the editor dead_code diagnostics on the new helpers were STALE — real clippy is clean);
`integration::` module **96/96**; full `nextest --no-fail-fast` **2454 passed / 1 failed** — the lone failure
`live_handoff::live_server_holds_one_pty_master_fd_per_pane` fails IDENTICALLY on clean stashed HEAD 0f52da6
(pre-existing environmental server-spawn limitation in this sandboxed session; causally disjoint from integration
status). herdr tokens in shipped code: 0. **No real `~/.copilot` mutation** (temp-HOME fixtures only); status is
read-only — live `~/.zynk/zynk.db` untouched. No `allow(unused*)`. Files (3): `src/integration/mod.rs`,
`docs/next/website/src/content/docs/integrations.mdx`, `docs/next/CHANGELOG.md`.

## #127 — native `zynk skill` install/status (researched target registry) (2026-06-16)

New zynk-native feature: `zynk skill install <claude|pi|codex> [--force] | --all` + `zynk skill status [agent]
[--json]`. Installs the repository `SKILL.md` (embedded via `include_str!("../../SKILL.md")`, source of truth) into
per-agent skill dirs. Research (live FS + integration source) found only THREE agents have a verified
`<base>/skills/<name>/SKILL.md` convention — claude (`~/.claude/skills`, `CLAUDE_CONFIG_DIR`), pi (`~/.pi/agent/
skills`, `PI_CODING_AGENT_DIR`), codex (`~/.codex/skills`, `CODEX_HOME`); the other 10 known agents are reported
`unsupported` (no writes). Status: ownership via the `<!-- zynk-skill-version: N -->` marker + freshness via sha256
byte-identity vs the embedded asset (reuse `crate::zynk::message::lowercase_hex_sha256`); version is display-only.
States `current|outdated|not-installed|conflict-custom|unsupported`. No-clobber: a marker-less file =>
`conflict-custom` => install refuses unless `--force`, which first backs the existing file up to a unique,
content-addressed `SKILL.md.bak-<sha8>` (`-N` on collision, fail-safe; never overwrites a differing backup); all
writes are atomic (tmp + rename); only `<base>/skills/zynk/` is ever touched. CLI rejects mixed `install <agent>
--all`, unknown agent, unsupported install, and unknown flags with exit 2; rich `zynk skill help`.

**Fork discipline:** core in zynk-owned `src/zynk/skill.rs` (NOT a generic `src/skill/`); CLI dispatch in
`src/cli/skill.rs`; the skill module does NOT depend on `integration` internals — a neutral leaf `src/config_dir.rs`
now owns `config_dir_from_env_or_home`/`expand_tilde_path`/`home_dir` and is used by BOTH `integration` and
`zynk::skill`. **Upstream-touch files:** `src/cli.rs` (mod + dispatch only), `src/main.rs` (mod decl + help
allowlist/usage), `src/integration/mod.rs` (mechanical: deleted the 3 helper defs, `use crate::config_dir::{...}`).

**Tests (16, TDD, temp env only — NO real `~/.claude`/`~/.pi`/`~/.codex` mutation; per-agent env overrides point at
temp dirs):** exact supported paths; not-installed/current/idempotent/outdated; conflict-custom mirroring the real
stale `~/.codex/skills/zynk` (no marker + herdr text) refusing without `--force`; unique-backup-never-overwrites
(two distinct customs -> two distinct backups, first preserved, no plain `.bak`); no-clobber of sibling skill dirs;
all 10 unsupported agents write-nothing + install-rejects; registry 3-supported/10-unsupported; embedded-asset-clean
(#114-style: no herdr/`HERDR_`, has marker, has `w1:p`/`ZYNK_ENV=1`/`zynk send`/`zynk reply`); marker parse; CLI
parser/help (reject mixed/unknown/unsupported/unknown-flag, help lists supported + unsupported).

**Validation:** `cargo fmt --check` clean; `cargo clippy --all-targets --locked -- -D warnings` clean; targeted
`nextest -E 'test(/skill::/)'` **16/16**; full `nextest --no-fail-fast` **2470 passed / 1 failed** — the lone
failure `live_handoff::live_server_holds_one_pty_master_fd_per_pane` is the pre-existing environmental flake (fails
on clean HEAD; documented). No `allow(unused*)`. Live user dirs + `~/.zynk/zynk.db` untouched. Files: NEW
`src/zynk/skill.rs`, `src/cli/skill.rs`, `src/config_dir.rs`; EDIT `src/zynk/mod.rs`, `src/cli.rs`, `src/main.rs`,
`src/integration/mod.rs`; DOCS `docs/next/website/src/content/docs/agent-skill.mdx`, `.../cli-reference.mdx`,
`docs/next/CHANGELOG.md`.

## #125 — CLI `--help` leaf handling: exit 0 + position-safe guard (2026-06-16)

Issue ref #125 is operator-assigned for the CLI-help audit (note: the skill installer above was renumbered to
#127 in the CHANGELOG; numbering is the operator's to reconcile). Audit (R3, approved 7/7) found that NO leaf
command — nor `send`/`reply` — honored `--help`: id-positional leaves (`pane read/get`, `agent get/read`,
`workspace focus/close`, `tab focus`) exited 1 with a raw OS error (the flag was consumed as the id and the op
ran); required-flag leaves (`wait output/agent-status`, `agent wait`, `integration install`) exited 2 with a
misleading domain error; others exited 2. Fix: a shared `crate::cli::is_help_flag` (matches `--help`/`-h`, NEVER
the bare word `help`) + `leaf_help_requested(args) = args.len()==2 && is_help_flag(&args[1])` — fires ONLY in the
exact `<group> <leaf> --help` position, added at the TOP of every group dispatcher (pane/wait/agent/workspace/tab/
notification/session/worktree/integration/skill/server/config/channel/terminal) so leaf `--help` prints that
group's help and exits 0. `send`/`reply` (native): hoisted a first-position help check ABOVE the arity check and
enriched the help (target `w2:p2`, `--type` examples, `--trace`, `--` body separator, JSON `delivery_status`/`proof`
= submission proof, not receipt).

**Delimiter/payload safety (the Codex `unsafe-help-interceptor` blocker):** the guard NEVER scans the whole arg list,
so a `--`-delimited payload or flag value keeps working — `pane run w1:p1 -- --help`, `pane send-text w1:p1 -- --help`,
`send w2:p2 -- --help` (body `--help`), `workspace create --label --help` (label `--help`), `pane rename w1:p1 help`
(label) are all preserved, proven by predicate unit tests. **Plugin sub-groups preserved:** `plugin action`/`pane`
are excluded from the plugin guard (and have their own), so `plugin action --help` still shows sub-group help.
**Group-help accuracy (since group help is now the leaf-`--help` output):** corrected two VERIFIED-stale lines —
`agent send` now shows `[--type T] [--]` (parser supports it; not `--trace`, which it does not) and `agent explain`
shows `[--json|--verbose]` — in `print_agent_help` + `docs/next/.../cli-reference.mdx`. Unknown leaf in help
position (`pane bogus --help`) intentionally shows group help (lists valid leaves); tested. Global `main.rs:457`
interceptor untouched.

**Tests (parser-only, socket-free):** `is_help_flag`/`leaf_help_requested` predicate (incl. all payload/value/label
non-hijack cases); command-level `<leaf> --help` => Ok(0) for the P0 leaves; `send`/`reply --help` Ok(0) + arity
still Ok(2); `send_help_text` content (flags + proof≠receipt); plugin sub-group preservation; unknown-leaf intentional.
**Validation:** `cargo fmt --check` clean; `clippy --all-targets --locked -- -D warnings` clean (no `allow(unused*)`);
targeted nextest 157/157 (cli/skill/integration); full `nextest --no-fail-fast` **2479 passed / 1 failed** — the lone
failure is the pre-existing `live_handoff` env flake. No socket/live mutation (help paths return before dispatch).
Flag-only (NOT fixed, pre-existing blind-rebrand artifact, same family as the hermes/omp `X or X` dups):
`src/cli/native.rs:28-30` `caller_pane_id_env` reads `ZYNK_PANE_ID` twice. Files: EDIT `src/cli.rs`,
`src/cli/{native,pane,workspace,tab,agent,notification,worktree,integration,skill,server,plugin,status}.rs`; DOCS
`docs/next/CHANGELOG.md`, `.../cli-reference.mdx`. (FIX1: added the `status` group leaf-help guard — `zynk status
server/client --help` now exit 0 — which the first pass missed; + a status help test.) (FIX2 — SAFETY: the
top-level `zynk db` command is dispatched directly (`crate::zynk::db_cutover`), bypassing the group leaf-help work;
`zynk db adopt/backup/import --help` could call `cmd_relocate` and MUTATE the DB. Added an exact-position safe-help
gate in `src/zynk/db_cutover.rs::run_db_command_at_code` (`classify_db_leaf_args`): `db <status|adopt|backup|import>
--help`/`-h` -> usage, exit 0, NO run/relocate; a stray trailing arg -> exit 2 + usage, no mutation; the bare-word
`help` and top-level `db help`/`--help` unchanged. Tests via the path-injectable `run_db_command_at_code` on temp
foreign-DB fixtures prove exit 0 + no relocate for all four leaves x `--help`/`-h`, and exit 2 + no mutation for a
stray arg. CHANGELOG wording corrected — the first pass said "every group leaf"; `db` is a direct top-level command,
now covered.)

## #122 — agent-UX review fix batch (help/docs accuracy + read/get `--help` UX) (2026-06-16)

Verified-finding cleanup from the #122 agent-UX review (baseline 8d42323), as a new commit (NOT amending #125).
(1) `detection` read source — `parse_read_source` (src/cli.rs:824) accepts `--source detection`, but help omitted it:
added `|detection` to the `pane read` / `agent read` / `wait output` usage + group-help strings
(`src/cli/pane.rs`, `src/cli/agent.rs`, `src/cli.rs`), the `wait output` line in `docs/next/.../cli-reference.mdx`
(pane/agent already had it; the Read-sources table already had it), and a `--source detection` bullet in root
`SKILL.md` (described as the bottom-buffer snapshot agent detection reads). (2) Root help common rows — `src/main.rs`
send/reply common-command rows now show `[--type T]` (the synopsis already did) + "(optional --type/--trace)".
(3) Proof honesty in docs — added a paragraph to the cli-reference Conversation messages section: `send`/`reply`
JSON `delivery_status`+`proof` prove submission/input delivery, NOT recipient comprehension; receipt needs the
recipient's reply or stored evidence (`thread`/`inbox`/`query`); the header is awareness, not a receipt. (4) Root
help env wording — `src/main.rs` `Env: ZYNK_CONFIG_PATH overrides config file path` (removed the nonsensical
"(ZYNK_CONFIG_PATH compat)" that claimed the var was its own compat alias). (5) `src/cli/native.rs`
`caller_pane_id_env` — collapsed the duplicate `var("ZYNK_PANE_ID").or_else(|| var("ZYNK_PANE_ID"))` to a single
lookup + truthful native-only doc comment (no `HERDR_*` introduced). (6) Friendlier positional `--help` for
read/get: `pane read`/`agent read` add a position-aware `other if is_help_flag(other)` arm in the option loop
(read takes no body, so a help flag in option position is unambiguous; a `--help` consumed as a flag VALUE still
errors as a bad value — proven by a test); `pane get`/`agent get` add an exact `args.len()==2 && is_help_flag`
check. So `zynk pane read w1:p1 --help` etc. now exit 0 with command usage. NO blanket `args.iter().any` scan; no
change to send/reply payload, pane run/send-text body, or any `--`-delimited handling.

**Rejected (per operator):** did NOT change `parse_type_trace_and_text` — value-missing `--type` correctly becomes
the literal body `--type`, not empty text.

**Tests (`tests/cli_wrapper.rs`, run the built binary with an isolated socket):** `read_help_lists_detection_source`
(pane/agent/wait help include `detection`); `root_help_common_rows_show_type_for_send_and_reply`;
`positional_read_get_help_flag_exits_zero_with_command_usage` (4 read/get leaves -> exit 0 + usage);
`read_help_flag_as_option_value_is_not_hijacked` (`pane read w1:p1 --lines --help` -> non-zero, not help).
**Validation:** `fmt --check` clean; `clippy --all-targets --locked -- -D warnings` clean; targeted nextest 54/54 +
the 4 new cli_wrapper help tests 4/4. (Full suite not re-run this batch: the isolated `/tmp` target is tmpfs and hit
a disk-quota ceiling; help/comment/parser-arm edits are low-regression and clippy `--all-targets` compiled every
target.) No `allow(unused*)`. No live socket/runtime mutation (help paths return before dispatch; tests use an
isolated socket). Files: EDIT `src/cli.rs`, `src/cli/{pane,agent,native}.rs`, `src/main.rs`, `SKILL.md`,
`docs/next/website/src/content/docs/cli-reference.mdx`, `docs/next/CHANGELOG.md`, `tests/cli_wrapper.rs`.

## #RESUME-FLAGS — native resume command flag preservation (Tier A) (2026-06-17)

Plan `docs/zynk/plans/2026-06-16-resume-command-flag-preservation.md` (approved 2b30f87). Native agent resume
previously rebuilt a fixed-minimal `agent --resume <id>` from `(source, agent, session_ref)` only, dropping the
operator's runtime/policy flags (`--dangerously-skip-permissions`, `--yolo`, etc.). Tier A threads the already-
persisted `launch_argv` (zynk argv-spawned panes + a post-resume record-back) into the planner and rewrites it with
default-deny per-agent allowlist adapters that preserve recognized policy flags and replace only the session
selector. `agent_resume.rs`: `plan()` gains a 4th `original_argv: Option<&[String]>` arg; the old fixed match is
extracted to `canonical_resume_argv`; new `valid_argv`/`valid_argv_token`/`argv0_matches_agent` (argv0 basename ==
agent, so wrapped launchers fall back — **plan deviation:** self-contained basename check instead of widening the
inherited `detect/mod.rs`, which is strictly more conservative); `AgentRewriteSpec` + `CLAUDE_SPEC`/`PI_SPEC`/
`CODEX_BOOL_ALLOW` seeds; `rewrite_preserving_flags` dispatcher + `rewrite_flag_selector_agent` (Claude/Pi: keep
allowlisted flags, replace selector at its original index, `None` on `--`/positional/unknown/ambiguous) and
`rewrite_codex` (accept interactive or `resume [old-id]`, reject other subcommands, normalize to
`argv0 resume <id> <flags>`). `persist/restore.rs`: `restore_plan_for_snapshot` + `pane_restore_startup` thread
`saved_launch_argv.as_deref()`; the deferred native-resume terminal also carries `with_launch_argv(...)` (no
`respawn_shell_on_exit`). `app/agent_resume.rs` `start_pending_agent_resume`: records the effective `plan.argv`
back into `terminal.launch_argv` after a successful launch (reboot stability). No on-disk schema/`SNAPSHOT_VERSION`
change (`launch_argv` already persisted). Default behavior unchanged when `original_argv` is absent/unrecognized.

**Tests:** `agent_resume.rs` validation (`argv_validation_*`, `argv0_matches_agent_*`), happy-path
(`claude_resume_preserves_policy_flags_and_swaps_selector`, `*_strips_existing_resume_selector_before_reinjecting`,
`*_inserts_selector_when_original_had_none`, `pi_resume_*`, `codex_resume_replaces_existing_session_id_preserving_yolo`,
`codex_resume_preserves_yolo_from_interactive_shape`), adversarial fallback (`*_inserts_selector_before_double_dash`,
`*_rejects_positional_prompt_payload`/`*_rejects_bare_positional_payload`, `codex_resume_rejects_non_resume_subcommands`,
`rewrite_falls_back_to_canonical_on_unknown_flag`/`_argv0_mismatch`/`_invalid_token`, `preserved_value_flags_are_data_not_shell_text`);
`persist/restore.rs` (`restore_plan_threads_launch_argv_into_resume`, `restore_plan_without_launch_argv_falls_back_to_identity`);
`app/agent_resume.rs` (`pending_agent_resume_records_effective_argv_back_into_launch_argv`,
`shell_command_from_argv_quotes_preserved_flags`); `persist/snapshot.rs` (`launch_argv_round_trips_through_snapshot`).
**Validation (isolated `CARGO_TARGET_DIR=~/tmp/zynk-resume-target`, non-tmpfs Btrfs):** `fmt --check` clean;
`clippy --all-targets --locked -- -D warnings` exit 0 / 0 warnings; `nextest run agent_resume` 43/43;
`nextest run persist::restore persist::snapshot` 49/49. No `allow(unused*)`; no live socket/runtime/DB mutation.
Files: EDIT `src/agent_resume.rs`, `src/persist/restore.rs`, `src/persist/snapshot.rs`, `src/app/agent_resume.rs`,
`docs/next/CHANGELOG.md`, `docs/next/website/src/content/docs/{session-state,configuration}.mdx`. No inherited-file
touch (`detect/mod.rs` deliberately untouched).

**FIX1 (2026-06-17, impl review RF-001 / Codex blocker):** allowlists finalized from the installed CLI `--help`
(`claude --help`, `codex --help`, `codex resume --help`, `pi --help`) — the seed lists dropped legitimate
documented policy flags. `AgentRewriteSpec.value_selector` generalized to `value_selectors: &[&str]` (Claude
`-r`/`--resume`; Pi `-c`/`-r`/`--continue`/`--resume` bool + `--session`/`--session-id` value). Codex gains a
value-flag allowlist + a picker-flag drop list. Finalized coverage: **Claude** bool `--dangerously-skip-permissions`
/`--allow-dangerously-skip-permissions`/`--fork-session`/`--safe-mode`, value `--model`/`--fallback-model`/
`--permission-mode`/`--agent`/`--effort`. **Codex** bool `--dangerously-bypass-approvals-and-sandbox`/
`--dangerously-bypass-hook-trust`/`--oss`/`--search`/`--yolo`, value `-m`/`--model`, `-s`/`--sandbox`,
`-a`/`--ask-for-approval`, `-C`/`--cd`, `--add-dir`, `-p`/`--profile`, `--local-provider`, `--enable`/`--disable`,
`--remote`; drop `--last`/`--all`/`--include-non-interactive`. **Pi** bool `--no-tools`/`-nt`/`--no-builtin-tools`/
`-nbt`/`--approve`/`-a`/`--no-approve`/`-na`/`--offline`/`--no-extensions`/`-ne`/`--no-skills`/`-ns`/
`--no-context-files`/`-nc`, value `--provider`/`--model`/`--thinking`/`--tools`/`-t`/`--exclude-tools`/`-xt`/
`--models`/`--mode`. **Deliberately excluded (→ canonical fallback):** secret-bearing `pi --api-key`; arbitrary
`codex -c/--config` + `--remote-auth-token-env`; variadic Claude flags (`--add-dir <dirs...>`,
`--allowedTools <tools...>`, …); exit-and-quit modes (`-p/--print`, `--export`, `--list-models`, `--version`).
New tests: `claude_resume_replaces_short_resume_alias_and_preserves_permission_mode`,
`codex_resume_preserves_model_alias_and_eq_value_forms`, `codex_resume_preserves_sandbox_approval_and_bypass_flags`,
`codex_resume_drops_redundant_picker_flags`, `pi_resume_preserves_provider_model_thinking_and_tools`,
`pi_resume_rejects_secret_bearing_api_key`. Re-validation: `fmt --check` clean; clippy `-D warnings` exit 0;
`nextest agent_resume` 49/49; `nextest persist::restore persist::snapshot app::agent_resume` 59/59.

**FIX2 (2026-06-17, post-FIX1-approval optional patch, operator-requested):** the FIX1 re-review approval
included a non-blocking note that Claude `--session-id <uuid>` was not treated as a session selector (that
launch shape fell back to canonical, safely). Per operator request, added `--session-id` to
`CLAUDE_SPEC.value_selectors` (now `["--resume", "-r", "--session-id"]`), so `claude --session-id OLD …` /
`claude --session-id=OLD …` strip the old selector and inject the canonical `--resume <new-id>` while
preserving allowlisted flags (e.g. `claude --session-id OLD --model sonnet --permission-mode plan` ->
`claude --resume new-id --model sonnet --permission-mode plan`). No other allowlist broadened; no
secret/variadic/config flag added. New test `claude_resume_replaces_session_id_selector_space_and_eq_forms`
(space + `=` forms). Re-validation: `fmt --check` clean; clippy `--all-targets -D warnings` exit 0;
`nextest agent_resume` 50/50. Files: EDIT `src/agent_resume.rs`, `docs/zynk/fork-patch-ledger.md`.

**Tier B (2026-06-17, operator dogfood blocker — real/manual launch UX):** the approved Tier A only preserved
flags when `TerminalState.launch_argv` was already populated (zynk argv-spawned panes + post-resume record-back).
The real flow — operator types `claude --continue --dangerously-skip-permissions` (likewise codex/pi) into a
shell pane — never populates `launch_argv`, so restore fell back to canonical and dropped the flags (confirmed
by operator dogfood: session.json had `["claude","--resume",<id>]`). Tier B captures the **live foreground
command** at snapshot time and persists only a **sanitized** resume argv. `detect/mod.rs`: new
`foreground_agent_argv(child_pid, expected_agent)` — `foreground_job(pid)` + `identify_agent_in_job`, returns
the matched process argv ONLY when the identified agent equals the hook-reported agent (wrapped/mismatched
launchers → None). `agent_resume.rs`: new pure `persisted_resume_argv(source, agent, session_ref,
foreground_argv, existing_launch_argv)` — runs each candidate through `plan()` (default-deny), prefers the
foreground-sanitized argv, then the existing-launch-argv-sanitized, and returns `Some` ONLY when it preserves a
flag beyond canonical (else `None`). `persist/snapshot.rs`: `capture_tab` computes `agent_session` first, then
`persisted_agent_launch_argv(agent_session, child_pid, existing_launch_argv)` — gated on an official
`agent_session`, reads `TerminalRuntime.child_pid()` → `foreground_agent_argv`, and `.or(existing_launch_argv)`
so non-agent panes + the handoff respawn argv (`was_imported` branch) are unchanged. Generic across
Claude/Codex/Pi (reuses the FIX1/FIX2 adapters). Raw argv is NEVER persisted; secret-bearing
(`pi --api-key`), unknown, `--` payload, variadic, and non-resume forms collapse to canonical → filtered → not
stored. New tests: `agent_resume` `manual_foreground_{claude_preserves_dangerously_skip_permissions,
codex_preserves_yolo_and_model,pi_preserves_model_and_thinking}`, `manual_foreground_secret_argv_is_not_persisted`,
`persisted_resume_argv_prefers_foreground_then_existing_then_none`; `detect`
`foreground_agent_argv_does_not_misfire_for_non_agent_process`; `persist::snapshot`
`persisted_agent_launch_argv_gates_and_falls_back`. Validation (isolated `~/tmp/zynk-resume-target`):
`fmt --check` clean; clippy `--all-targets -D warnings` exit 0; `nextest agent_resume persist::snapshot
persist::restore app::agent_resume detect::tests::foreground_agent_argv` 104/104; `nextest persist handoff`
128/128 (no handoff/live_handoff regression). Files: EDIT `src/detect/mod.rs`, `src/agent_resume.rs`,
`src/persist/snapshot.rs`, `docs/next/CHANGELOG.md`, `docs/next/website/src/content/docs/session-state.mdx`,
`docs/zynk/fork-patch-ledger.md`. `detect/mod.rs` is the only inherited-file touch (one additive pub fn).

**Tier B FIX1 (2026-06-17, TB-001 — Codex + Pi arbiter blocker):** `persist::snapshot::persisted_agent_launch_argv`
ended with `persisted_resume_argv(...).or(existing_launch_argv)`, which re-persisted the RAW
`existing_launch_argv` for an official agent pane whenever the sanitizer returned `None` — e.g. a Pi pane with
`["pi","--api-key","sk-secret","--model","x"]` would still write the secret to the snapshot. Fix: dropped the
`.or(existing_launch_argv)` for the official-native-`agent_session` branch — it now persists sanitized-or-`None`
ONLY (raw is never the resume input). Non-agent panes and panes whose `agent_session` is not a valid official
native session still pass `existing_launch_argv` through unchanged (handoff respawn / operator's own command —
not a resume input; verified the `live_handoff_keeps_agent_started_pane_after_agent_exits` pane has no official
`agent_session`, so its raw argv + respawn are untouched). Tests: updated
`persisted_agent_launch_argv_gates_and_falls_back` (canonical-only official existing argv -> `None`, not kept)
+ new `persisted_agent_launch_argv_never_persists_raw_unsafe_argv_for_official_agent` (TB-001 secret case ->
`None`). Re-validation: `fmt --check` clean; clippy `--all-targets -D warnings` exit 0; `nextest agent_resume
persist::snapshot persist::restore app::agent_resume detect::tests::foreground_agent_argv` 105/105; `nextest
persist handoff` 129/129. Files: EDIT `src/persist/snapshot.rs`, `docs/zynk/fork-patch-ledger.md`.


---

# v0.7.1 PORT LEDGER

Port of upstream Herdr **v0.7.1** (`23b96e4`) into the zynk hard fork, on top of the v0.7.0-ported base
(`fa14c5d`). Driven through the gated workflow: Gate-1 spec + plan (Codex, both APPROVED after revisions) →
per-milestone Gate-2 (Codex, M1–M5 + this M6) → Gate-3 swarm → operator merge. Per-SHA ledger model
(ADR 0010, full fork): re-apply only the genuine `v0.7.0..v0.7.1` upstream deltas ON TOP of zynk identity;
never raw-merge.

**Framing:** the fork base is AHEAD of v0.7.1 on rebrand + the net-new `src/zynk/` conversation layer, so the
`base..v0.7.1` diff is mostly `zynk→herdr` reversion noise. Port = apply only the genuine deltas, adapted to
zynk identity (no `herdr` in active source), dropping website/blog/docs-site/preview/admin hunks.

**Totals (51 commits):** 36 PORT/SPLIT · 2 EVALUATE · 11 SKIP · 2 SKIP-DEFER. Branch `port/herdr-v0.7.1`;
zynk version stays **3.0.1** (the `0.7.1` bump is SKIP). Every milestone ended green (`just check` + `just gate`)
and Gate-2-approved by Codex.

## Applied PORT/SPLIT — by milestone (zynk sha ← upstream sha)

### M1 — Windows-ConPTY portable-pty vendoring
| zynk | upstream | subject / handling |
|---|---|---|
| d04c136 | cc802c8 | drop needless return in windows stdin reader dispatch |
| b3a43fc | b7a504b | add windows-msvc clippy recipe (justfile `windows-lint`) |
| c4433f0 | 052f202 | preserve windows terminal multiline paste (vti backend) |
| ef4a273 | 3366121 | record windows named-pipe timeout skip (test deferred to M5) |
| bb984df | d7ae163 | **SPLIT** windows installer PSModulePath cleanup — `src/update.rs` only; dropped the installer-script hunk (fail-closed updater) |
| bce7987 | c580840 | detect npm-wrapped pi on windows |
| 27cb7c3 | a66f4c6 | **vendor portable-pty + force system ConPTY** — `vendor/portable-pty` + `0001-force-system-conpty.patch` + `vendor/portable-pty.patches.md` + `scripts/test_vendor_portable_pty.py` (wired into justfile) + `windows_smoke_conpty_path.ps1` + Cargo `[patch]`/`include` |
| cef42a2 | 64a12de | include vendored portable-pty in nix package fileset |
| 3cbb2c0 | — | (hygiene) scrub `herdr` from the windows-lint recipe comment |

**§8 unix.rs check (M1.7):** vendored `vendor/portable-pty/src/unix.rs` is **sha256-identical** to crates.io
`portable-pty 0.9.0/src/unix.rs` (also lib/cmdbuilder/serial/win-{conpty,mod,procthreadattr}); the ONLY source
delta is `win/psuedocon.rs` (force system ConPTY via kernel32.dll). No Unix delta. `scripts.test_vendor_portable_pty` 6/6.

### M2 — agent panel + theme
| zynk | upstream | subject / handling |
|---|---|---|
| dc83d1d | 5449025 | sort agent panel by priority (zynk ranks Working>done — fork divergence; test order adapted) |
| 0d896a9 | 36b4001 | sync theme with host appearance |
| 9abdca5 | 9f4f163 | background update-check toggles (disable-only; updater stays fail-closed) |
| 2b0838f | 4421c0f | configurable pane gaps (render stays pure) |

### M3 — lifecycle + restore
| zynk | upstream | subject / handling |
|---|---|---|
| bfe4fad | ebff340 | restore omp sessions after same-pane restart |
| 4244907 | 5ffaf4f | isolate omp hook state to ui sessions |
| 29980a6 | d74ba8c | emit plugin lifecycle events from ui worktree flows (UI-flow events ported; the dropped worktree-keyed plugin EventData arms stay dropped — fork divergence) |
| d9cd202 | 26c5f97 | reanchor lifecycle hook generations (no `Agent::Omp` variant — fork keeps omp hook-only) |
| 6a7d9b6 | 92a10fc | protect root agent restore sessions (owner-keyed, never detection-derived) |
| b979a99 | 734bf3c | release pi and omp agents on shutdown |
| 88a4d76 | 89ca3ba | check out existing branches for worktree create |
| b883a8e | 46a2b25 | **defer api worktree operations** — full deferral plumbing (runtime/headless intercept, pending-op state, operation-ids, async response, recovery), adapted to zynk's event surface (no worktree-specific plugin events → workspace-lifecycle events). **Initially over-skipped (mis-attributed to the dropped plugin-EVENT subsystem); Codex Gate-2 caught it; re-ported.** |
| b57fb05 | 89ca3ba | (test) deferred-API existing-branch regression test — portable only once 46a2b25 landed (Codex M3 non-blocking note) |

### M4 — terminal / render / input
| zynk | upstream | subject / handling |
|---|---|---|
| 7cd5e5f | 6f9ca0e | hold lone escape while awaiting host color reply (render pure) |
| dbc87d6 | aca35ea | scope image paste shortcut to remote |
| bbee9bf | 73b137a | preserve focus after temporary pane commands |
| 2671043 | afc56d5 | enable kitty file media |
| 8f14972 | 4846cb1 | skip wide spacer cells in pane reads (detection-snapshot tail UNCHANGED; manifest + 83 detect tests green) |
| e4d4875 | 569c33b | active pane color for border intersections (render pure) |
| e7d9e0d | 088922d | let user keybinds displace defaults (source-aware two-pass User-then-Default registry) |
| a4c0838 | 520d0b8 | prevent release-key fallback from doubling input |
| — | d998753 | **PORT-NOOP** cjk branch truncation — zynk's `truncate_text` (`src/ui/sidebar.rs:170-183`) is already char-based; never had the byte-slice bug (covered by `branch_row_truncates_non_ascii_branch_without_panic`) |

### M5 — hooks / detect / remote
| zynk | upstream | subject / handling |
|---|---|---|
| f79f6f9 | 2fe57a5 | **SPLIT** stabilize flaky plugin capture tests — test stabilization only; dropped the auto-website-deploy hunk |
| bba0839 | ef0b4bc | support agent env hints (env hint `HERDR_AGENT`→`ZYNK_AGENT`) |
| 1152bc5 | a518228 | support devin hook on python 3.9 (integration asset; `DEVIN_INTEGRATION_VERSION` 1→2 + marker) |
| 630ec26 | 27ff4dd | block idle client writer (Condvar-backed queue; binary wire format unchanged) |
| ca68184 | b76fb49 | **SPLIT** detect copilot ask_user accept prompt — `github-copilot.toml` manifest only (bounded `whole_recent` region); dropped the website mirror |
| 631bf12 | 74fa90c | scope opencode hook to main agent + adopt new sessions (hook-authoritative; `OPENCODE_INTEGRATION_VERSION` 5→7 + marker) |
| 93deec1 | a720367 | extend remote client handshake timeout for high-latency links |
| 18914ff | b7a504b | **windows-cfg fix** — gate `events_require_host_terminal_theme_query` with `#[cfg(any(not(windows), test))]` (the deferred b7a504b gate). The fn was added by M2/`36b4001` WITHOUT the gate → dead code on windows; **caught by the windows-gnu cross-check** (linux `just check` never compiles `#[cfg(windows)]` code, so 5 milestones missed it). Re-ran windows-gnu clippy on the full HEAD → clean. |

## EVALUATE (2) — macOS CI zig → DECISION: KEEP `mlugg/setup-zig` (no-op, with evidence)
`5981ba4` ci: use homebrew zig on macos · `dbc45f6` ci: use homebrew zig in release workflow.
zynk's `mlugg/setup-zig@v2.2.1` is SHA-pinned (`d1434d08`) AND pins the exact `version: 0.15.2` (the locked
toolchain zig). Upstream's Homebrew `zig@0.15` tracks the latest 0.15.x (drifts off 0.15.2). For a fork that
values reproducibility + a locked zig 0.15.2, the pinned mlugg action is strictly better; zynk shipped v3.0.0
on it with no mlugg-attributed macOS fragility. **Kept mlugg unchanged**; re-evaluate only on a concrete
mlugg-caused macOS CI failure.

## SKIP (11) — website / blog / docs-site / preview-channel / chore / release-admin (zynk has none of these)
`61ede89` website manifest · `41d1c14` plugin-trust/security docs-site guidance · `517ca81` preview-release CI
baseline · `4cf9f8e` preview manifest · `003bef7` approve contributor (chore) · `24c7377` preview-preflight CI
bun · `995a429` preview manifest · `705403f` blog author card · `74076a8` sponsorship docs · `bae1f14` finalize
release docs · `23b96e4` release v0.7.1 (version bump — zynk stays 3.0.1).

## SKIP-DEFER (2) — plugin marketplace (recorded for future, NOT rejected)
`2eeea9a` feat: add plugin marketplace · `bf75226` feat: add plugin marketplace blacklist. The marketplace is a
hosted Cloudflare worker (`workers/plugin-marketplace`) + an in-app client needing a registry backend zynk has
no equivalent for. Deferred (not rejected): revisit when zynk has a registry backend. The ported plugin
lifecycle code (M3) does NOT depend on the marketplace, so deferring it never breaks ported code — verified:
`git grep -i marketplace` in committed source finds no half-ported marketplace symbol.

## crates.io packaging caveat (M6.2b — operator-decided: accept crates.io-source as a non-primary channel)
`cargo package` strips `[patch.crates-io]` + excludes the nested `vendor/portable-pty` source (it has its own
`Cargo.toml` → package boundary), so the packaged crate builds against **registry `portable-pty 0.9.0`**. Net:
Linux/core crates.io-SOURCE install builds fine; **only a Windows crates.io-SOURCE install lacks the ConPTY
patch** (binary/Homebrew/Nix ship the patched vendored copy). The `include`-list keeps the patch/index files in
the tarball for provenance. `cargo publish --dry-run --locked` passes; single `libsqlite3-sys` node preserved
(ADR 0006, vec0). Accepted by the operator; documented here + in the release/package docs.

## Windows verification note
zynk's CI windows target is `x86_64-pc-windows-msvc`, which can't cross-build on the dev host (the C deps
`sqlite-vec`/`libsqlite3-sys` need an MSVC archiver/SDK). The local windows proxy is
`DOCS_RS=1 cargo clippy --target x86_64-pc-windows-gnu -- -D warnings` (or a temporary, uncommitted `build.rs`
gnu mapping). This caught the M2/M5 `events_require_host_terminal_theme_query` dead-code break that 5 milestones
of linux `just check` missed. The CI windows-latest msvc job remains authoritative.

**herdr-base marker: v0.7.0 → v0.7.1.** zynk is now based on upstream Herdr **v0.7.1** (`23b96e4`).


## POST-MERGE CI FIX (2026-06-25) — macOS + Windows CI failures (branch fix/v071-ci)

The post-merge CI on `main` (`f640fdd`) surfaced 2 platform failures that the local linux `just check` +
windows-gnu proxy missed — the CI macOS/Windows jobs are the AUTHORITATIVE cross-platform check, and they
were not watched before the merge was declared done (a process miss; corrected going forward).

1. **Windows** — `server::client_transport::tests::client_writer_closes_queue_after_socket_write_failure`
   panicked: `named pipes do not support I/O timeouts` (`client_transport.rs:916`). The upstream `3366121`
   `#[cfg(not(windows))]` guard on the test's `set_send_timeout` was DEFERRED-to-M5 during M1 (the test
   ships with `27ff4dd`), but M5 never applied it. **Fixed:** guard applied in
   `src/server/client_transport.rs`. (The windows-gnu proxy ran clippy only, not the windows tests, so it
   slipped.)
2. **macOS** — the zig build-runner can't link libSystem on the macos-latest runner. **The M6.1 EVALUATE
   decision to KEEP `mlugg/setup-zig` was WRONG:** the upstream `5981ba4`/`dbc45f6` switch to Homebrew's
   macOS-patched `zig@0.15` was a macOS-zig-LINK FIX, not a mere preference. **M6.1 REVISED — adopt the
   homebrew-zig switch** for the macOS jobs in `.github/workflows/{ci,release-dryrun,build-artifacts-manual}.yml`
   (gate `mlugg` to `runner.os != 'macOS'` + `brew install zig@0.15` on macOS). `mlugg/setup-zig@v2.2.1`
   with `version: 0.15.2` stays for linux + windows (still SHA-pinned + reproducible there).

---

# RELEASE 3.1.0 PREP (2026-09-07) — fork-owned changes on `release/3.1.0`

Not an upstream port. Recorded because several touches land in upstream-inherited files (rule: log every
touch). Branch base `main` @ `3d10a90`.

| area | files | what / why |
|---|---|---|
| Rust 1.98 clippy | `src/ghostty/mod.rs`, `src/server/handoff.rs`, `src/terminal_theme.rs` | CI `stable` moved to 1.98 (2026-08-18): `chunks_exact_to_as_chunks` ×2, `byte_char_slices`, `some_filter`. The `some_filter` site is restructured to bind the parsed color BEFORE rejecting trailing components (the lint's mechanical rewrite would reorder the `parts.next()` calls). |
| DB init race (product bug) | `src/zynk/db.rs` (fork-owned) | Two zynk processes opening a fresh shared DB at once (two named-session servers; a CLI racing a starting server): sqlx creates `_sqlx_migrations` before the first migration commits, the second opener classified the ledger-only DB as FOREIGN and exited (`db_foreign_conflict`). Fix: `InitLock` (advisory lock on `<db>.init-lock` around classify + migrate, acquired COOPERATIVELY — `try_lock` + async sleep, 5 s bound → `db_init_lock_timeout` — so two openers on one executor cannot deadlock) + classification now reasons on the COMPLETE schema-object set (`schema_objects`; the suffix-filtered `user_table_names` is display-only): a ledger-only/EMPTY DB classifies `Empty`, any other foreign table — including suffix-filtered names like `customer_data` — stays Foreign and byte-identical (ADR 0008 boundary unchanged; this also closes a pre-existing hole where a DB whose only table was suffix-filtered classified Empty). Found via the `named_sessions_use_separate_servers_and_workspace_state` flake (2/40 under load, captured). |
| DB guard hardening (Gate-3) | `src/zynk/db.rs` (fork-owned), `src/server/headless.rs`, new `docs/zynk/decisions/0011-db-inspection-sidecar-boundary.md` | Swarm Gate-3 reproduced three ways the guard granted writable authority from non-provenance evidence (foreign tables named like our lineage tables; foreign view/index/trigger only; a non-UTF-8 table name silently dropped). Classification now reads the complete `sqlite_master` object set losslessly and recognizes a native DB by migration PROVENANCE (ledger version+checksum vs the built-in migrator); the init lock is taken only when initialization OR a pending migration is needed (a fully current native DB never waits; bound 5 s); the schema, ledger and currentness are read inside ONE read transaction (single snapshot — a concurrent initializer's commit between two reads used to turn a genuine init into a false Foreign) with a production-wrapper interleaving regression + a negative control; the ledger table must match the canonical sqlx structure — `PRAGMA table_xinfo` (no hidden/generated/extra columns) AND a narrow supported-DDL policy on the stored `CREATE TABLE` text (exactly the six sqlx definitions, no CHECK/FK/GENERATED, nothing after the closing parenthesis; `table_info` alone is lossy) — so a name-borrowing table is Foreign before any writable connect; positive compatibility is tested against the real sqlx initializer; the lineage policy requires a valid prefix of our migrations with our checksums, `success` = literal `1`, and tolerates unknown versions only when newer; every DB open error is fatal at server start AND written to the durable server log (the daemon launcher nulls stderr). ADR 0011 records the inspection boundary: existing data bytes (main file + `-wal` content) never modified; `-shm` and an empty `-wal` may be created (EXCLUSIVE/immutable alternatives rejected with evidence). |
| tracked gaps (not fixed here) | — | (1) handoff-import server path (`run_handoff_import_server`) binds before the DB guard and `new_from_handoff` leaves the DB workers `None` — pre-existing; **FIXED in the Gate-3 round 2 row below** (pre-flight + workers + two live-handoff regressions). (2) bundled SQLite 3.46.0 (`libsqlite3-sys` 0.30.1) predates the 3.51.3 WAL-reset fix (very rare per SQLite) — bump on the dependency branch with vec0/ADR 0006 re-verification. (3) CLI processes drop their SQLite connection at exit without a clean close, so no checkpoint-on-close runs: after `zynk query`/`send` the schema and rows can live entirely in `-wal` until a server closes cleanly or autocheckpoint runs (observed in round 8: 4 KiB main + 206 KiB `-wal`; pre-existing; SQLite semantics intact, `adopt`/`backup` move sidecars) — consider an explicit close at the CLI boundary. (4) SQLx-lineage maintenance: native recognition pins the sqlx-sqlite 0.8.6 `_sqlx_migrations` ledger structure (`CANONICAL_LEDGER_COLUMNS` / `CANONICAL_LEDGER_DDL_COLUMNS` in `src/zynk/db.rs`, tested against the real `Migrate::ensure_migrations_table`); an SQLx upgrade that changes that DDL must update the pins and fixtures in the same change (else every native DB classifies Foreign — the fail-closed direction, caught by `real_sqlx_empty_ledger_is_empty_and_initializes`). All: review by **2026-09-30**. |
| DB guard hardening (Gate-3 round 2) | `src/zynk/db.rs`, `src/zynk/db_cutover.rs`, `src/zynk/retrieval/mod.rs`, `src/server/headless.rs`, `tests/cli_wrapper.rs`, `tests/live_handoff.rs`, ADR 0011 addendum | Swarm round 2 (arbiter `msg_84c8526c070c921d`) reproduced 3 blockers + 4 majors on `aa0a469`: (DB-001) inspection closed a read-only connection and reopened the PATH writable — an atomic symlink alternator redirected the migration into a foreign DB; (DB-002) a nonempty `zynk.db-wal` beside an absent main was classified Absent and consumed; (DB-003) `NOT GLOB 'sqlite_*'` hid a raw-renamed readable table (Empty → migrated); (DB-004/005) a partial or complete lineage plus an unknown newer row was `native … ready` although sqlx rejects it (`migration_missing`; with DELETE journaling the failed open changed the main SHA); (DB-006) LF/ESC in schema names printed raw; (SRV-001) handoff-import bound public sockets without the DB pre-flight or workers. Fixes: ONE pinned read-write connection for inspection + writes (no journal-mode change at connect; `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` until success; Unix file-identity check around the WAL switch + post-switch re-inspection → `db_target_changed`; `SQLITE_FCNTL_PERSIST_WAL` so the close path never deletes `<db>-wal` by name); lock BEFORE create (sentinel: a held lock + absent DB created a zero-byte file); `db_orphan_sidecar` for nonempty `-wal`/`-journal` beside an absent/zero-byte main, checked before any connection (also `db status`); exact-shape SQLite-owned catalog filter (autoindex of a present table, `sqlite_sequence`/`sqlite_stat1`/`sqlite_stat4` with fixed DDL) with ANALYZE/AUTOINCREMENT positive controls; `MigrationState::{Current,Pending,Newer}` — newer rows tolerated only after a complete lineage, `db status` says NEWER never ready, opener refuses `db_newer_lineage` before pragmas, partial+newer is Foreign; `printable_name` escapes control chars (hex for non-UTF-8); handoff-import runs `preflight_native_db` after `receive` (exit → old server rolls back) and `install_db_workers`. Regressions (RED-first): 11 in `db.rs` (symlink flip + atomic rename with foreign main+`-wal` byte identity after the whole write path, fresh-init/rollback-mode rename → target_changed, WAL-copy refusal without checkpoint, held-lock-no-create, raw-prefix negative + owned-catalog positive controls, orphan `-wal`/`-journal`, partial+newer, newer-never-ready, control chars), `db_cutover` status, `retrieval` hint, 3 CLI-boundary (`cli_wrapper`: orphan WAL, newer, control chars) and 2 live-handoff (foreign DB → rollback + durable log; workers installed). Minor follow-ups closed: this ledger's DB-init-race row corrected in place (5 s bound; `schema_objects`), `db_unavailable` hint no longer names the retired `zynk-v2` path, Cargo rebrand comment completed. |
| DB guard + handoff hardening (Codex Gate-2 R8) | `src/zynk/db.rs`, `src/zynk/db_cutover.rs`, `src/server/headless.rs`, `src/zynk/embed/{mod,fake}.rs`, `src/zynk/embedding_worker.rs`, `src/zynk/persistence.rs`, `tests/cli_wrapper.rs`, `tests/live_handoff.rs`, ADR 0011 addendum 12-14 | Codex Gate-2 on 473acba (`msg_2f8cf0a6df22f803`): (P1) the pinned read-write inspection played back an ordinary FOREIGN hot rollback journal on its first read (main SHA changed, journal deleted; the read-only R7 path refused with SQLITE_READONLY_ROLLBACK) → conservative pre-connect refusal `db_hot_journal` at every connect path incl. after the init-lock wait (a zynk initializer's own journal-mode-switch journal is waited out under that lock), metadata failures never read as absence (`db_io_error`), PERSIST/TRUNCATE positive controls; regression = real crashed writer (child process, spill past cache, exit without commit) at unit + CLI boundary with main+journal byte identity. (P1) the handoff preflight ran `recover_orphan_messages` and synthesized `failed` for a sender between persisting and its first transport event (later `submitted` rejected) → `DbPreflightRecovery::{ColdStart,None}`: the replacement validates without recovery; regressions across successful + rolled-back handoff (`live_handoff_does_not_fail_an_in_flight_send`, `rolled_back_…`) + `a_failed_event_freezes_the_message`. (P2) replacement workers started before old-worker ownership ended (a blocked job re-run: attempts 2, stale overwrite) → old server quiesces (joins) its workers before "committed", the replacement installs its workers only after "committed", a failed commit restores them (`ZYNK_TEST_HANDOFF_COMMIT_FAIL` test hook); `fake-blocking` test embedder (`ZYNK_TEST_EMBED_RELEASE_FILE`); unit ordering tests (`a_joined_old_worker_hands_a_blocked_job_over_exactly_once` + the double-processing negative control) and live tests with a blocked old job, a real validated receipt and embedding progress after handoff and after a failed commit (`live_handoff_hands_the_db_workers_over_without_duplicate_processing`, `failed_live_handoff_commit_restores_the_db_workers`); the log-count test was replaced. Nonblocking: UTF-16 catalogs documented as foreign (conservative); printed paths escaped; PERSIST_WAL comment no longer promises truncation. |
| DB path + handoff hardening (Codex Gate-2 R9) | `src/zynk/db.rs`, `src/zynk/worker_pause.rs` (new), `src/zynk/embedding_worker.rs`, `src/zynk/receipt_worker.rs`, `src/server/handoff.rs`, `src/server/headless.rs`, `tests/cli_wrapper.rs`, `tests/live_handoff.rs`, ADR 0011 items 15-17 | Codex Gate-2 on a814f8a (`msg_2f8cf0a6df22f803`… R9 `msg_0eaf82f1f3a5c8ca`): (P1) a STABLE `zynk.db -> foreign.db` link bypassed the sidecar guards (they read `zynk.db-journal`; SQLite replayed `foreign.db-journal`) → `sqlite_effective_path` mirrors `unixFullPathname` (absolute + final-component symlink chain, no lexical normalization) and is used for guards, InitLock, identity and the connection in the opener and `db status`; regressions: stable/relative/chained symlink → target hot journal refused with bytes identical (unit + CLI), target-side orphan WAL behind a link, target-held InitLock blocks a linked open, flip/rename controls kept. (P2) the worker handover was not negotiated with legacy peers (an unmodified v3.0.1 sender committed with running workers → job re-run) → HANDOFF_VERSION 2, `validate_manifest` refuses version 1 before "validated" with the remedy sent back (`rejected: …`) and logged; `ZYNK_TEST_HANDOFF_MANIFEST_VERSION` sender hook; regression `legacy_handoff_sender_is_rejected_before_service_moves`. (P2) the commit-time worker join was unbounded while the replacement's wait_committed is 30 s → `PauseControl` (idle/paused state atomic with work selection; `stop` releases a paused consumer) in both workers; `start_live_handoff` pauses both BEFORE withdrawing any service with a bounded wait (`ZYNK_HANDOFF_WORKER_IDLE_MS`, default 10 s), refuses when busy (workers resumed), resumes on every pre-commit rollback, and joins the idle handles at commit; regressions `busy_db_workers_reject_a_live_handoff_within_the_deadline`, `pause_waits_for_the_in_flight_batch_and_resume_continues`, `receipt_worker_pauses_when_idle_and_drops_promptly`, `worker_pause::pause_is_atomic_with_work_selection`; the blocked-job live tests became "released within the deadline" (not complete + service not withdrawn while blocked, injected commit failure asserted exactly, bounded waits, replacement pid registered). Nonblocking: `inspect_db_at` metadata NotFound-only; `printable_path` in every guard/io/lock message; "looks hot" wording. |
| handoff quiescence precision (Codex Gate-2 R10) | `src/zynk/receipt_worker.rs`, `src/zynk/worker_pause.rs`, `src/server/handoff.rs`, `src/server/headless.rs`, `src/zynk/db.rs` (doc), `tests/live_handoff.rs`, ADR 0011 items 15-17 | Codex Gate-2 on 4f85c7b (`msg_260aefab7e9986b0`): (P2) the receipt worker acknowledged idle BEFORE its runtime/initial DB open, so a live-handoff pause returned true while the open still waited on the init lock and the drop then blocked → idle is reported only once the initial open completed or failed (a failed open is retried per job); regressions `receipt_worker_startup_is_busy_until_its_db_open_completes` (held init lock: pause(300 ms) busy → release → idle → prompt drop), `receipt_worker_startup_error_path_reaches_idle` (foreign DB: fast failure → idle, bytes untouched). (P2) an old replacement (3.0.x) closing without answering surfaced only "handoff stream closed while reading line" → the validation read now keeps the transport cause and adds the possible-incompatible-peer / restart-normally hint, and every validation failure is logged durably on the sender (`handoff replacement did not validate the manifest`); child hook `ZYNK_TEST_HANDOFF_IMPORT_FAIL=close_before_validate`; regression `legacy_handoff_destination_eof_is_reported_with_the_restart_hint`. Nonblocking: `sqlite_effective_path` attribution corrected (SQLite walks every component and folds dot segments; zynk resolves the final component only — identity and sidecars coincide through directory links; no lexical normalization); drop policy made explicit — a paused receipt consumer is released to DRAIN queued jobs (`begin_work` never refuses; `receipt_worker_drains_a_job_queued_while_paused_on_drop`); the blocked-job live tests wait for the durable "pausing DB workers" phase line instead of sleeping. |
| receipt quiescence accounting (Codex Gate-2 R11) | `src/zynk/worker_pause.rs`, `src/zynk/receipt_worker.rs`, `src/app/api/zynk.rs`, `src/server/handoff.rs`, `src/server/headless.rs`, `tests/live_handoff.rs`, ADR 0011 item 17 | Codex Gate-2 on 07173ea (`msg_176b6c262732255c`): (P2) `pause` tested only in-flight work; receipts accepted by the API (callers timed out at 5 s) but not yet started were drained AFTER service withdrawal, so the commit-time join outran the replacement's 30 s wait (reproduced with three real hook-authorized receipts + a slow-SQL fixture: replacement expired at 50 s, rollback at 66 s). Fix: `PauseControl` accounts for accepted-but-unstarted units — `try_enqueue` (admission under the pause mutex, refused while paused/stopping) / `unenqueue` on a failed hand-off to the channel / `begin_work` takes a unit (count and in-flight flipped under one lock, so a prefetched job still counts) — and `pause` acknowledges only when nothing is in flight AND nothing is queued; a paused receipt worker answers `submit` with `receipt_worker_busy` ("quiescing … retry"), `resume` reopens admission; the runtime-failure drain path accounts too. Regressions: unit `receipt_worker_pause_counts_queued_and_prefetched_jobs` (block hook `ZYNK_TEST_RECEIPT_BLOCK_FILE` holds a taken job before it is in flight; three timed-out submitters; pause busy → refusal while quiescing → release → drained → admission reopened → prompt drop), `pause_is_atomic_with_work_selection` (queue accounting), live `receipt_backlog_rejects_a_live_handoff_then_drains_and_hands_over` (three real `zynk.message_received` calls time out at `ZYNK_RECEIPT_TIMEOUT_MS`=300; handoff refused within the bound with the API socket inode unchanged and no receipt committed; after release each receipt commits exactly once and the handoff succeeds; RED: the handoff proceeded and withdrew service). Nonblocking: `ZYNK_RECEIPT_TIMEOUT_MS` knob; validation-read failures get a neutral context unless the kind is EOF/reset (closure wording only then); the EOF regression asserts the original `handoff stream closed while reading line`; ADR 16 actor corrected. |
| release build known gaps (public rationale) | `.github/workflows/release-dryrun.yml` (the only tracked release workflow; the operator's actual release procedure is private and untracked and is not public evidence) | The public release dry-run workflow carries two accepted gaps, recorded here so their comments resolve to a tracked location (Gate-3 round 3, WARDEN-R3-REL-002): (1) the vendored libghostty-vt Zig build eagerly fetches the `uucode` dependency over the network (deps.files.ghostty.org) on a cold cache, so release builds — macOS especially — are network-dependent and occasionally flaky; follow-up: pre-warm/vendor that Zig cache for determinism. (2) Linux release binaries are glibc (gnu) DYNAMIC builds, not static musl: the bundled sqlite-vec 0.1.9 C source uses BSD `u_int8_t`/`u_int16_t`/`u_int64_t` typedefs that musl does not define, so the musl target fails to compile with both musl-gcc and zig cc; restoring a static musl build is a separate gated change (patch the typedefs or upgrade to a fixed release after a musl + vector/retrieval test pass). Both: review with the dependency branch. |
| cutover bundle + terminal-safe output (Gate-3 round 3) | `src/zynk/db_cutover.rs`, `src/zynk/db.rs`, `SECURITY.md`, `.github/workflows/release-dryrun.yml`, ADR 0011 items 18-19 | Swarm round 3 on 8271e9a (auditor `msg_088c37cb2d0747a3`, arbiter `msg_acc1139b9b3af418`/`msg_00f422fe1a8c17e4`, warden `msg_5e35968eaf80032a`): (AUD-310-CUTOVER-001, blocker) `adopt`/`backup` moved main + `-wal`/`-shm` but not `-journal` (best-effort, errors ignored) — a PERSIST-inactive or hot journal stayed behind and the orphan-sidecar guard then refused the "clean" native path; (AUD-310-CUTOVER-002, blocker) the backup slot checked only the main target, so an existing `<slot>-wal` was overwritten by POSIX rename; (SENT-R3-DB-001, major) db_cutover printed paths raw and `printable_name` escaped only Cc, so ESC in a path and a U+202E table name reached the terminal; (WARDEN-R3-REL-001/002, minor) SECURITY.md claimed no published release; the release workflow cited a private, untracked path. Fixes: relocation is a preflighted all-or-nothing SQLite BUNDLE move (`BUNDLE_SUFFIXES` = -journal/-wal/-shm + main; a slot is free only when the base and every member target are absent; every target re-checked before the first rename; sidecars first, main last; any failure moves the already-moved members back and returns an error — never a success with a stranded member); `adopt`/`backup` now also relocate what the guards refuse to open (`db_hot_journal`, `db_orphan_sidecar` → the bundle, main optional), which is the remedy those messages name; `is_terminal_hostile` = Cc + Unicode Cf/bidi/invisible + Zl/Zp, used by `printable_name`/`printable_path`, and every db_cutover renderer (outcomes, no-database line, classify wrapper, rename/copy errors) prints through it; SECURITY.md states the actual policy (latest published release supported; fixes ship in the next release); the workflow comments point at the ledger row above. Regressions: `adopt_moves_the_complete_bundle_including_an_inactive_persist_journal`, `adopt_moves_a_hot_journal_bundle_and_native_init_follows` (real crashed-writer fixture), `backup_slot_reserves_every_bundle_member` (slot-0 `-wal` sentinel byte-identical, slot 1 used), `relocation_rolls_back_when_a_member_cannot_move` (injected mover), `cutover_output_never_emits_raw_control_or_format_characters` (ESC in the path through status/adopt/the orphan failure, U+202E table name), `terminal_hostile_characters_cover_format_and_bidi_controls`. Pre-read on 79ec55c (arbiter `msg_3f7f2c8c8216d38f`/`msg_a71c90297402500c`, Codex `msg_0b56aefc1545abbd`): `Path::exists` follows symlinks, so a dangling `<slot>-wal` symlink read as a free slot and POSIX rename replaced it, a competitor created between the preflight and the rename was overwritten, and relocation used the configured spelling while inspection resolves the final symlink, so `zynk.db -> absent target.db` with a nonempty `target.db-wal` was refused yet `adopt` found "nothing to relocate". Fixes: `entry_exists` (`symlink_metadata`) for every slot/target check; `move_no_replace` = hard link (fails when the name exists in any form) then unlink, `rename_or_copy` fallback only where hard links are unsupported (documented residual); `relocate_aside` resolves `sqlite_effective_path` first (the link stays). Regressions (RED first): `backup_slot_treats_a_dangling_symlink_as_occupied` (symlink replaced), `relocation_refuses_a_target_that_appears_after_preflight` (injected mover plants the target; rename overwrote it), `adopt_through_a_final_symlink_relocates_the_target_bundle` (exit 1, WAL left). Codex Gate-2 R13 on da2dca7 (`msg_e919fb04502c46f0`, LD_PRELOAD fault probe `r16_cutover_probe.py`): (P1) every hard-link error except EEXIST fell back to a REPLACING rename/copy — with `linkat` returning EOPNOTSUPP or EPERM a competitor created after the entry check was destroyed and adopt exited 0; (P2) a source unlink failing after a successful link left the backup as an alias of the untouched source (same inode) outside rollback accounting. Fixes at that round: a no-replace chain (`renameat2(RENAME_NOREPLACE)` → hard link → exclusive-create copy) with partial-move cleanup; superseded one round later (next sentences) once the two-step fallbacks proved unsafe against concurrent writers. Codex Gate-2 R13 on b135137 (`msg_b56e9dc545857f76`, `r17_copy_probe.py`) + arbiter G3-PRE-B135-001/002 (`msg_15f2229d407bd1ca`, `msg_4062727cbd8ee71f`, `msg_62b0fac766f15d10`): the exclusive-copy fallback swallowed a failed cleanup (a partial backup remained while the error said "nothing was relocated"), was created with default permissions before the bytes landed, and — decisive — every two-step fallback (link or copy, then unlink of the source NAME) has a source/target-swap window: fault injection showed `db adopt` exiting 0 while deleting a file another writer had atomically placed at the source, and losing the original when the target was swapped after the link. Resolution: the fallbacks are REMOVED; `move_no_replace` is one atomic no-replace rename per platform — `renameat2(RENAME_NOREPLACE)` (Linux, via the existing `libc` dep), `renamex_np(RENAME_EXCL)` (macOS), `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (Windows; `windows-sys` feature `Win32_Storage_FileSystem` added, lock unchanged) — refused with an actionable `Unsupported` error where the filesystem lacks it (`primitive_unavailable`: ENOSYS/ENOTSUP/EOPNOTSUPP/EXDEV/EINVAL; ordinary failures keep their cause). Regressions: `a_missing_atomic_move_is_an_actionable_refusal_not_a_fallback`, `an_ordinary_move_failure_keeps_its_cause`, `a_source_replaced_before_the_atomic_move_moves_whole_and_nothing_else_is_deleted` (the guaranteed boundary), plus the retained `backup_slot_treats_a_dangling_symlink_as_occupied` and `relocation_refuses_a_target_that_appears_after_preflight` now exercising the atomic primitive. |
| shared-database ownership: job claim token, orphan grace, receipt target binding (Gate-3 round 3) | `src/zynk/embedding_worker.rs`, `src/zynk/db.rs`, `src/zynk/receipt.rs`, `src/app/creation.rs`, `src/zynk/persistence.rs` (tests), `tests/cli_wrapper.rs`, `tests/zynk_receipt.rs`, ADR 0011 items 20-22 | Swarm round 3 on 8271e9a, blockers confirmed by the arbiter (`msg_408a691e92cf8bbe`, `msg_7b571146455206d0`, `msg_b31e8e140df20bc7`; artifacts `wr-y6GY32`, `or3-f1NTDN`; auditor `receipt-wrong-participant-1788798178-1333137`): (AUD-310-WORKER-001, blocker) two servers sharing the database each ran the same embedding job — the worker SELECTed pending jobs and marked them running without a guard, and every start reset ALL `running` jobs; (arbiter, blocker) every named server's cold start failed EVERY event-less message, including a live peer's send that had been persisted but not yet dispatched; (AUD-310-RECEIPT-001, blocker) a receipt was accepted from ANY hook-authoritative pane carrying the target's agent label (`receiver.agent_label == tp.agent_label`), so a second same-label pane could receipt a message addressed to another terminal. Fixes: the claim is one atomic status-guarded UPDATE (`pending`, or `failed` under the attempt cap) RETURNING the incremented `attempts`, which is the claim token (`JobClaim`); `done`/`failed` updates match id + `status='running'` + token, so a stale worker (recovered past `RUNNING_LEASE` 600 s, `RECOVERY_INTERVAL` 60 s) can neither complete nor fail the job its successor owns; `recover_orphan_messages` fails only event-less messages older than `ORPHAN_GRACE` (300 s); the receipt binds to the STORED target participant: when it carried a hook session value the receiver must present the same one (the durable anchor — it survives pane churn, restarts and the live handoff, whose restored terminals get NEW terminal ids); when it carried none, `AuthoritativeReceiver.terminal_id` (the `TerminalId` string `agent.get`/`pane.get` report) must equal the stored `terminal_id`, within this server's lifetime (pane ids rotate and are not part of the participant key — pane churn on the same terminal/session is accepted by design). A first cut that required the terminal id whenever stored was caught by two live-handoff regressions (receipt after the handoff: `receiver terminal term_… is not the terminal the message was addressed to`); the handoff/receipt fixtures now report as a session-carrying `pi` integration (a generic `hook` source cannot carry a session id; reserved-native labels get their authority from the native path). The HEAD negative control `an_unjoined_old_worker_double_processes_a_blocked_job` is re-scoped to a claim older than the lease (inside the lease the job is left alone). The HEAD test `orphaned_message_without_event_recovers_as_failed_system_recovery` now ages its orphan beyond the grace window (a fresh event-less message is in flight, by design). Regressions (RED first): `a_job_is_claimed_by_exactly_one_worker`, `recovery_resets_only_stale_running_jobs`, `a_stale_worker_cannot_fail_a_job_another_worker_is_running` (RED: `("failed", 2)` under the new owner), live `a_second_named_session_never_reruns_a_job_another_server_owns`; `orphan_recovery_spares_messages_inside_the_grace_window` + live `a_second_named_session_does_not_fail_a_peers_in_flight_send` (RED with the age predicate removed: beta failed the peer's in-flight send); `receipt_from_a_different_terminal_with_the_same_label_is_rejected`, `receipt_from_the_same_terminal_under_another_session_is_rejected`, `receipt_after_pane_churn_on_the_stored_terminal_is_accepted`, `receipt_after_a_restore_with_the_stored_session_is_accepted`, live `receipt_from_a_same_label_pane_that_is_not_the_target_is_rejected` (RED: the impostor pane's receipt returned `received`). Pre-read on 79ec55c (arbiter `msg_363f03b4a170ca5c`, `msg_d3e8138593eb1cc1`, `msg_af4472998987f6b5`): (a) the token-guarded `done` UPDATE came AFTER the vector writes and its row count was ignored, so a stale claim's late SUCCESS committed a vector under the new owner, whose completion then failed on the vec0 UNIQUE (artifact `r79-stale-iMLXNn`) → the fence is the first statement of the immediate transaction, zero rows = `WriteOutcome::Stale` → rollback, nothing recorded; (b) orphan recovery SELECTed ids outside a transaction and bumped/inserted per id without re-checking, so two named servers cold-starting together recorded TWO failed events per old orphan (artifact `r79-orphanrace-mSP042`) → `fail_orphan_message`: `BEGIN IMMEDIATE` + `UPDATE messages … WHERE id = ? AND NOT EXISTS (event) RETURNING delivery_seq`, zero rows = skip; (c) the receipt compared only `agent_session_value` while the participant identity is the triple (source, kind, value) → all stored components must match. Regressions (RED first): `a_stale_worker_success_never_writes_a_vector_under_the_new_owner` (RED: one embedding row under `running:2`), `failing_an_orphan_is_recorded_exactly_once_across_callers`, live `concurrent_named_server_cold_starts_fail_each_old_orphan_exactly_once` (300 old orphans, alpha+beta together; RED: every one doubled), `receipt_with_the_stored_session_value_but_another_source_is_rejected`, `…another_kind_is_rejected`, `receipt_with_the_full_stored_session_triple_is_accepted`. Codex Gate-2 R13 on da2dca7 (`msg_e919fb04502c46f0`, API probe `r16_identity_probe.py`): (P2) a pane holding hook authority for `pi` plus a session persisted for `codex` (`pane.report_agent_session` source `zynk:codex`) exposed that session as its identity (`terminal_agent_session_info` falls back to the persisted session), so two such panes sharing the codex session id could receipt each other's messages. Fix: owner coherence — `persistence::session_fields` and `App::authoritative_receiver_identity` treat a session as identity only when its `agent` equals the party's label; an incoherent session is absent on both sides and the terminal binding applies (matching-owner native fallback and restored sessions unchanged). Regressions (RED first): API-level `a_session_persisted_for_another_owner_is_not_a_receipt_anchor` (Codex's exact reproduction; RED: B's receipt returned `received`), `a_coherent_persisted_session_still_anchors_the_receipt`, unit `a_session_owned_by_another_agent_is_not_participant_identity`. |
| Gate-3 round 4 (swarm on ba76519): directory-reserved backup slot, sidecar-link guard, adopt path boundary, durable self-receipt, CI evidence | `src/zynk/db_cutover.rs`, `src/zynk/db.rs`, `src/zynk/receipt.rs`, `tests/cli_wrapper.rs`, `tests/live_handoff.rs`, `.github/workflows/nix.yml`, `SECURITY.md`, ADR 0011 items 18/22/23/24 | Swarm round 4 on ba76519 (arbiter verdict `msg_af8b513473b66097`; sentinel `msg_adbd54f1f6c9a26b`, architect `msg_aa77d359cf20939d`, auditor `msg_430963e0da6f1186`, warden `msg_12b49967a08c8414`, inspector `msg_f009910151cc0fb9`; Codex concurrence `msg_467e9e361642371f`): (SENT-R4-CUTOVER-001, blocker) the source plan used `Path::exists`, so a dangling sidecar link stayed at the native path and the next start failed with SQLite code 14; (SENT-R4-CUTOVER-002, blocker) per-filename no-replace could not stop a competitor from adding an unplanned member beside a moved main — a mixed backup reported as complete; (SENT-R4-CUTOVER-003, major) an absent explicit `ZYNK_SQLITE_HOME` path made `adopt` relocate `$HOME/.zynk/zynk-v2/zynk.db`; (ARCH-RECEIPT-SELF-001 / AUD-310-RECEIPT-002, major) the self-receipt guard compared the rotating pane id, so a self-addressed message could be receipted after pane churn (auditor runtime repro); (ARCH-CUTOVER-ATOMICITY-001, minor) the all-or-nothing wording overstated per-member moves; (WARDEN-R4-WIN-001, major) the Windows move path was never executed by CI; INSPECTOR notes: deadline assertion slack, no hosted init-lock evidence, Nix triggers missing `src/**`. Fixes: the slot is a directory `<db>.wrapper-backup-N/` reserved by one atomic owner-only `mkdir` (`create_private_dir`), the source plan uses `entry_exists`, members keep their names inside the slot, `unexpected_slot_entry` revalidates the slot after the moves (rollback + failure otherwise), and a blocked rollback names the members left under the slot; `refuse_sidecar_links` (`db_sidecar_link`) fails closed on symlinked sidecars before initialization and `adopt` relocates them; `run_db_command_code` probes the legacy path only for `DbPathSource::DefaultHome` (explicit paths pass `probe_legacy = false`); the receipt selects `from_participant_id`/`to_participant_id` and rejects equality before any event (pane id kept as a secondary check); `windows_`-prefixed variants of four cutover regressions and of the init-lock deadline test run on windows-latest; init-lock and worker-idle deadline assertions tightened (8 s / 2 s); `nix.yml` triggers on `src/**`, `migrations/**` and `workflow_dispatch`; SECURITY.md support = newest version per channel; ADR item 18 states the per-member contract (Codex's clarification). Regressions (RED first): `adopt_moves_a_dangling_source_sidecar_link_and_native_startup_follows` (all three suffixes; RED: exit 1, link left), `a_dangling_sidecar_entry_is_refused_before_initialization` (RED: SQLite code 14), `a_member_planted_in_the_reserved_slot_is_refused_and_rolled_back`, `a_source_name_recreated_during_rollback_is_reported_not_replaced`, `an_explicit_db_path_never_relocates_the_ambient_legacy_database` (RED: legacy relocated), `a_self_addressed_message_is_rejected_after_pane_churn` + `a_stale_participant_pane_snapshot_does_not_enable_self_receipt` (RED: received), `backup_slot_reserves_every_bundle_member` / `backup_slot_treats_a_dangling_symlink_as_occupied` re-anchored on the slot name. Codex Gate-2 R20 in-flight on a3a71bc (`msg_7af8c2b0dd19be85`, fault probe `r20_cutover_probe.py`): `entry_exists` (`symlink_metadata(..).is_ok()`) read an EIO on the source `-wal` as absent (main + shm moved, the WAL left, exit 0) and `unexpected_slot_entry` swallowed `read_dir`/per-entry errors (mixed backup, exit 0) → `BundleInspect` (injectable `entry`/`list`): `entry_present` (only NotFound is absent; any other error refuses the relocation before a move), `list_dir` propagates listing and per-entry errors, `next_backup_path` returns `Result`; regressions `a_member_whose_metadata_cannot_be_read_refuses_the_relocation`, `a_slot_that_cannot_be_listed_is_not_clean` (opendir EACCES / readdir EIO). Codex Gate-2 R20 consolidated on a3a71bc (`msg_338d69e978ddd633`): (P2) participant-id equality was narrower than the receiver's session-first identity — the same label + coherent session across two terminal rows (a restore in between) could receipt its own message after churn; (P2) the stored sender-pane veto was unconditional — a distinct participant resuming at the sender's old pane id was refused. Fix: `receiver_matches_session` (shared with the target binding) decides self-receipt by the sender's label + session triple, else terminal, else (unanchored rows only) the stored pane id, plus row equality. Regressions: `the_same_session_across_terminal_rows_cannot_receipt_its_own_message` (RED: received; delivery_seq/events unchanged asserted), `a_distinct_participant_resuming_at_the_senders_old_pane_is_accepted` (RED: self_receipt_rejected). Gate-3 round 5 on 09f7ca6 (architect `msg_d48d0409307d0490`): (ARCH-RECEIPT-ANCHOR-001, major) a stored target with neither session nor terminal degraded to a label-only check and a partial session triple to value-only (absent source/kind were wildcards) → `canonical_anchor` (complete triple, else terminal, else none — none fails closed with `receiver_identity_mismatch`), `receiver_matches_session` strict on all three components, fixtures anchored on `term-<pane>`; regressions `a_stored_target_with_no_durable_anchor_is_refused`, `a_partial_stored_session_triple_is_refused` (RED: received) plus the isolated integration shapes the auditor asked for (`msg_60e81924859c3d19`, AUD-310-RECEIPT-003): `a_legacy_target_row_without_an_anchor_is_not_receipt_capable`, `a_partial_stored_session_triple_is_not_receipt_capable` (target row mutated in the fixture DB; impostor and original pane both refused). (ARCH-CUTOVER-WINDOWS-ACL-001, minor) the slot was documented owner-only on every platform while only Unix sets a creation mode → comment/ADR/CHANGELOG narrowed: owner-only on Unix, parent-inherited elsewhere; the post-move member check remains the guard. Custody (arbiter preliminary `msg_df69120901ad0f09`, sentinel): after the real move, a competitor atomically renamed over the moved member inside the slot passed the name-only post-move check and adopt claimed success with the competitor's bytes → `entry_identity` (Unix dev/inode of the source entry, captured before its move) is verified for every moved member after the moves; a displaced member fails the relocation, is named, and is not moved back; regression `a_member_replaced_inside_the_slot_after_its_move_is_not_claimed`. A partial stored session triple is refused outright (`StoredAnchor::Partial`), not degraded to the terminal. Warden round 5 (`msg_62bb1cc536b7fc99`): (WARDEN-09F-CUSTODY-001, blocker) a sidecar created at the source after the plan and before the main move survived a "successful" relocation → the source names are rescanned fail-closed after the moves (any member present → rollback + error); regression `a_sidecar_created_at_the_source_after_the_scan_fails_the_relocation` (RED: success with the WAL left). (WARDEN-09F-WINPATH-001, blocker) on non-Unix builds `sqlite_effective_path` returned the link unchanged while SQLite follows it → a final-component link fails closed (`db_path_link`) for guards, lock, opener and cutover; `windows_a_symlinked_database_path_is_refused` (skips explicitly only when the runner cannot create symlinks); ADR 0011 item 25; resolving reparse points is a follow-up. Sentinel round 5 (`msg_6b6342133c128415`, SENT-R5-CUTOVER-001) + arbiter final on 09f7ca6 (`msg_ea41eb55060c83aa`): the identity check is platform-complete — `entry_identity` = device + inode (Unix) / volume serial + file index via `GetFileInformationByHandle` on a reparse-point-preserving handle (Windows); a member whose identity cannot be read before the move is not moved (fail closed), a moved member missing or replaced afterwards fails the relocation; regression `a_member_deleted_from_the_slot_after_its_move_is_not_claimed`; ADR 0011 item 18 no longer presents the owner-only mkdir as actor-proof. Persist-time canonicalization: `session_fields` stores a session only as a complete triple (`a_partial_session_is_not_persisted_as_identity`); the receipt-time refusal of a legacy partial row is exercised by planting the row directly. ARB-DA4-CUSTODY-ROLLBACK-001 (arbiter pre-read on da4b8b7 `msg_645f40aebbb7655f`; Codex in-flight P1 `msg_d008c35c3b9bb0ca`, three variants): the displacement check was gated on no earlier failure, so a replaced member could be moved back over the source name when another verification (unexpected slot entry, listing error, late source sidecar) had failed first → custody is computed for every moved member immediately after the moves, before any other verification and before any rollback; displaced members are always excluded from rollback and named together with the original failure; regression `a_replaced_member_is_never_rolled_back_when_another_check_fails_first` (replacement + unexpected entry; replacement + late source sidecar; RED: the competitor landed at the source). The opener's `file_identity` gains the Windows volume serial + file index too (`msg_a7d77e8713850e7d`: the ADR residual no longer calls it Unix-only). |
| pre-tag cycle after the 3.1.0 merge gate: clarifications + hosted-evidence fixes | `CHANGELOG.md`, `src/zynk/db.rs`, `src/zynk/receipt.rs`, `src/zynk/db_cutover.rs`, `tests/zynk_message.rs`, `tests/zynk_receipt.rs`, `build.rs`, `.github/workflows/release-dryrun.yml`, `flake.nix`, `flake.lock` | After the operator's ff-only merge of 246a901 (Gate-3 APPROVE `msg_23754e03b47190d9`, Codex Gate-2 `msg_5f0a911aad8875d5`). (1) Nonblocking notes: CHANGELOG said a symlinked database path is relocated universally while Windows refuses it (ADR 0011 item 25); the `sqlite_effective_path` doc comment said Windows does not follow links (it does; zynk refuses there); the `StoredAnchor` comment described a partial triple as a terminal fallback (it is refused at the target); the Windows identity handle requested data-read access (`access_mode(0)` now: querying file information needs none); the displaced-member diagnostic states that an unreadable identity is an I/O condition, not proof of a writer. (2) Hosted evidence on 246a901 — CI ubuntu: `agent_send_to_real_agent_submits_and_returns_f4_ok` rendered the body as `h` / `ello there`: the target pane was a live `/bin/sh` executing the pasted lines, and its `not found` output interleaved at byte level with the tty echo (the PTY write path is unchanged since 3d10a90; the assertion was racy since the initial release). The message and dormant-receipt tests now park the pane in `stty -echo; exec cat` before reporting it as an agent target, as the header tests already did (8/8 local iterations). Release dry-run macOS (both arches): `ld: 64-bit mach-o member 'compiler_rt.o' not 8-byte aligned in libghostty-vt.a` — Zig 0.15.2 pads archive members to 2 bytes and Apple's ld requires 8; reproduced by cross-building the archives on Linux: with `LIBGHOSTTY_VT_SIMD=false` (the release setting) `compiler_rt.o` (aarch64) / the main object (x86_64) sits at offset ≡ 4 mod 8, with SIMD on both align by chance (CI green). `build.rs` parses the archive and, on a macOS host, rewrites a misaligned one with Apple `libtool -static` (`LIBGHOSTTY_VT_LIBTOOL` override; `xcrun libtool` then `libtool`), verifying the result; other hosts warn and link as is. Release dry-run linux-aarch64: `zig cc` rejected `-Wl,--fix-cortex-a53-843419`, which rustc 1.98 (runner: 1.98.1) adds to `aarch64-unknown-linux-gnu` pre-link args (rust-lang/rust#155453); cargo-zigbuild 0.23.4 filters it (`src/zig/linker_args.rs`) — pinned via taiki-e/install-action v2.87.8 (`tool: cargo-zigbuild@0.23.4`); the aarch64 artifact is therefore linked without the erratum workaround, as every earlier zynk aarch64 build was. Nix: the push-triggered check failed twice with HTTP 403 from `crates.io/api/v1/crates/*/download` (pinned nixpkgs f83fc3c, 2026-05-21, fetched crates through the rate-limited API; nixos-unstable head dc5d91f fixes that but dropped `x86_64-darwin`, which the flake declares and `nix flake check --all-systems` evaluates) → the flake input now tracks `nixos-26.05` (locked c257840, 2026-09-06: `static.crates.io` + `x86_64-darwin` supported until end of 2026); relocked and checked in the `nixos/nix` container (both workflow commands pass). |
| platform support tiers (ADR 0012) — required Linux x86_64, evidence-gated optional targets | `docs/zynk/decisions/0012-platform-support-tiers.md`, `WORKFLOW.md`, `README.md`, `SECURITY.md`, `CHANGELOG.md`, `.github/workflows/ci.yml`, `.github/workflows/release-dryrun.yml`, `scripts/release_binary.py`, `scripts/release_evidence.py`, `scripts/release_manifest.py`, `scripts/test_release_evidence.py`, `scripts/test_release_manifest.py`, `justfile` | Operator decision 2026-09-08 (team on Fedora/Ubuntu x86_64; six-month productivity focus) after the 246a901 candidate was held by macOS/aarch64/Nix failures while the Linux artifact was fine; Codex Gate-1 design approval `msg_11ad2ad15512406f` (trace `ZYNK_SUPPORT_TIERS_PROPOSAL_20260908`, three proposal rounds). Per-push CI = `check-required` running `just check` (hosted Ubuntu never ran the maintenance unittests before: `just ci` omits them); the release dry-run becomes the candidate-evidence workflow with fixed per-target job ids, `optional_targets: none|eligible|all`, producer-side `EVIDENCE.json` (the packaged binary executed on its native runner for the eligible-capable targets — the build-only linux-aarch64 (`not_run`) and macos-x86_64 (Rosetta `ran` at most) stay BUILT_UNVERIFIED; archive/binary sha256, `GITHUB_SHA` + checkout HEAD, version, target/CPU/ABI, producer run id/attempt — CI checkout provenance, since `zynk --version` prints only the version) and a `manifest` job that downloads only by immutable artifact id and decides ELIGIBLE / BUILT_UNVERIFIED / OMITTED / INCONSISTENT per target (same-SHA reuse across re-run attempts allowed with the producer's real attempt recorded; another run or commit rejected; glibc floor ≤ 2.30 enforced); `SHA256SUMS` lists ELIGIBLE archives only and nothing when the required target is not ELIGIBLE. `macos-x86_64` and `linux-aarch64` have no hosted test job → not shipped in 3.1.0. Rule 4 of WORKFLOW.md now scopes STOP to required checks; informational checks never block or hide. Homebrew per OS+arch stanza policy in the ADR. Codex Gate-2 on 0ece9be (`msg_b61a53b8f9dfd8fc`) found five evidence-design gaps, all fixed RED-first: a `null` sidecar or a directory in place of the archive bypassed validation (mandatory structure + typed-schema phase now precedes every decision); decoding errors (BadZipFile/TarError/struct.error) aborted the whole manifest (contained per target, CLI regressions on files + exit codes); an optional download failure blocked the required manifest (download steps continue-on-error with recorded outcomes, failed targets excluded visibly and their files never read); an empty required artifact id would have triggered download-all (`scripts/release_artifact_ids.py` exports only validated numeric ids); the glibc floor was a string scan (now PT_INTERP + `.gnu.version_r` version needs, cross-checked with the native objdump evidence; non-glibc-dynamic or version-need-less binaries are INCONSISTENT). Sidecar records libghostty build inputs; hosted required jobs install gitleaks so no maintenance test skips. Gate-2 R2 (`msg_735001fb4252e54d`): a `null` downloads.json disabled the outcome gate (now mandatory typed object, fail-closed before artifact reads); download steps shared the job deadline (per-step 2-minute bounds, job 20 minutes with a documented reserve, guarded by `scripts/test_release_workflow.py`); native objdump evidence mandatory; artifact ids bounded to the JavaScript safe-integer range. Gate-3 on 4f1ef90 (arbiter `msg_419d3d09e2cbbae1`, 4 major + 3 minor): raw native output re-derived by the consumer; producer runner OS/arch validated (ARM64 asserted on the macOS jobs); symlinked target/dist directories refused; clean-tracked-tree assertion after every build bound into the sidecar (`tree_clean`) and an optional `candidate_sha` dispatch assertion; sizes and attempt order checked; strict `build.rs` ar parser with `tests/darwin_archive_parser.rs`; docs qualified (native execution only for eligible-capable targets; `just check` the sole full check; candidate step in the shipping sequence). Gate-3 second pass (`msg_9305ee5a456d1f3e`, 4 minor): the parser now rejects a missing pad byte for the final member too; a Rosetta-executed macos-x86_64 regression pins BUILT_UNVERIFIED; the skill's Basic CI template runs `just check`; the 3.1.0 changelog heading is pre-tag until the tag exists. |
| claude detection: half-circle busy spinner (operator-reported bug; folded into the 3.1.0 candidate) | `src/detect/manifests/claude.toml`, `src/detect/manifest/tests.rs` | Claude Code >= 2.1.228 spins with half-circle frames (U+25D0..U+25D3) in the OSC title; the fork's manifest (byte-identical to the v0.7.1 base) matched only Braille, so a working Claude read as idle. Re-applied the spinner part of the upstream manifest fixes 9ba32c25 + 2075de5d (2026-08-12/14; upstream file version 2026.08.13.1) as manifest 2026.09.08.1 with the four-frame regression; the /btw-overlay (a47bc3e0) and confirmation-prompt (4473e391) rules from the same upstream file are deferred to the v0.8.2 port. Codex co-author alignment `msg_19a77e878b3e41db`; verified end to end on the installed binary through the existing local-override + `reload-agent-manifests` path (isolated runtime), which the operator also applied to the live runtime as immediate relief. Gate-3 scoped pass (`msg_f4705bdd02c149db`, ARB-4FDA-OSC-PRECEDENCE-001, major): at priority 1100 the busy-title rule outranked the current blocker forms (`live_blocked_form` / `dynamic_workflow_prompt` at 980) although Claude keeps its spinner title while a permission or selection form waits (upstream issue #3467) — the rule now sits at 970 (above the idle prompt box 950 and stale scrollback blockers ≤ 850, below the current forms); regressions: every busy frame + the live form → Blocked, busy title + prompt box / stale scrollback → Working, adjacent code points and a missing space → not busy; manifest 2026.09.08.2; CHANGELOG entry added (WARDEN-4FDA-CHANGELOG-001). Codex Gate-2 (`msg_9044a7ddd1261687`): the broad `dynamic_workflow_prompt` (whole recent screen, 980) had thereby been promoted above the spinner — a stale "Run a dynamic workflow?" in scrollback forced Blocked while Claude worked; it now sits at 960 (below the busy title, above the prompt box) with a stale-text regression and a current-prompt control; manifest 2026.09.08.3. Gate-3 scoped (`msg_0e0d9908b7a10e56`, ARB-EE13-OSC-FRESHNESS-001 major + ARB-EE13-PROMPT-TEST-001 minor): a scalar priority cannot express freshness — the real #3467 Bash approval tail has no Enter/navigation grammar (it matched only the ≤ 850 fallbacks and still lost to the spinner) and `live_blocked_form`'s `after_last_horizontal_rule` is a suffix selector (an answered form followed by work still blocked). Redesign: new `current_approval_dialog` (990, `bottom_non_empty_lines(2)`: "esc to cancel" + a second hint — tab to amend / enter to select / enter to confirm / to navigate / ctrl+e to explain) is the only blocker above the busy title (970); `live_blocked_form` drops to 960 beside the dynamic fallback; paired regressions over five dialog shapes (bash, generic, selection, dynamic, boxed) × five busy titles: current → Blocked, answered-then-work → Working, historical-above-divider → Working; the prompt-box control now uses a real two-border box proved via `evaluated_rules`; manifest 2026.09.08.4. |
| test harness | `tests/cli_wrapper.rs` | `spawn_named_server` captured nothing (`Stdio::null`); now logs the server's stdout/stderr and `wait_for_named_server_socket` reports the child's exit status + output when the socket never appears. |
| removed config key | `src/config/io.rs` | `ui.agent_panel_scope` was removed by the v0.7.1 port (dc83d1d); serde ignored it silently. Startup/live load now emit a diagnostic ("no longer supported … `ui.agent_panel_sort` controls ordering only"). Operator decision A (config-contract removal in a minor, documented). |
| self-update messages | `src/update.rs`, `src/cli.rs`, `src/main.rs`, `src/remote/unix.rs` | User-facing text falsely said "Zynk has no published release"; releases exist (v3.0.0 binaries, crates.io 3.0.1, Homebrew). Reworded: self-update/remote fetch unavailable because there is no update-MANIFEST hosting; update manually via Homebrew / release binary / Nix / `cargo install zynk --locked`. Updater stays fail-closed; the remote-bootstrap test asserts the new phrasing. |
| `cargo audit` triage | `Cargo.lock` (unchanged) | RUSTSEC-2026-0204 (`crossbeam-epoch`) and -0258 (`h2`) reach zynk only through the opt-in `fastembed` feature (no workflow/justfile/nix enables it; `cargo tree -i` finds no dependents for any target with default features); RUSTSEC-2023-0071 (`rsa`, no fix) is in no target graph at all. A narrow `--precise` bump was tried and reverted: it re-pinned `windows-sys` edges of `errno`/`rustix`/`tempfile` (inert, Unix-only crates) — a resolution change for zero shipped-binary benefit. Deferred, justified, to the post-release dependency branch (**review by 2026-09-30**; `--features fastembed` builds are NOT audited-clean); warnings (unmaintained `bincode`/`paste`, unsound `anyhow`/`lru`/`event-listener` — 0 zynk call sites of the flagged APIs) recorded there too. |
| release metadata | `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `README.md`, `.github/workflows/release-dryrun.yml` | Version 3.0.1 → 3.1.0 (root package only; no dependency refresh); `Cargo.toml` packaging comment rewritten (was stale: "2.x", "tag stays 3.0.0", private doc pointer); CHANGELOG 3.1.0 entry (Added / Changed / Fixed / Install notes); README install snippet (single `TAG` var, `mkdir -p ~/.local/bin`, PATH hint), source-build notes (Windows crates.io-source ConPTY caveat, Homebrew `zig@0.15` on macOS, `--locked`), a config-options snippet (defaults taken from `zynk --default-config`) and the `agent_panel_scope` migration note; dry-run workflow default version 3.1.0. README keeps the v3.0.0 download pins until the 3.1.0 assets exist (G7). |


---

# LINUX-ONLY M1 (2026-09-08) — branch `port/linux-only-v0.8.2`, base `18c316d`

Not an upstream port. **ADR 0013** (`docs/zynk/decisions/0013-linux-only-platform-scope.md`) narrows zynk to
Linux x86_64: the macOS/Windows implementations, the platform-only dependencies, the Nix/Homebrew
distribution and the multi-target release-evidence machinery are **removed, not made optional**. Recorded
here because nearly every touch lands in an upstream-inherited file (rule: log every touch). ADR 0013
supersedes ADR 0012 in full and narrows ADR 0011's per-platform atomic-move contract to the Linux arm; both
ADRs stay frozen as written. 12 commits; the counts are net deleted lines.

| commit | files | what / why |
|---|---|---|
| `111e127` | `build.rs`, `tests/darwin_archive_parser.rs` | Drop the darwin `libtool -static` archive-rewrite path (the 3.1.0 macOS `ld` alignment fix) and the non-Linux zig target selection; delete the archive-parser regression set that only existed for it. `-Demit-xcframework=false` STAYS — it is a vendor build argument, not a target branch. (−339) |
| `5a712a0` | `src/platform/{macos,windows,fallback}.rs` + `mod.rs`, `src/app/{mod,runtime,state,config_io}.rs`, `src/app/input/{mod,settings}.rs`, `src/config/{io,model}.rs`, `src/ui/settings.rs`, `src/main.rs` | `src/platform/` keeps only `linux.rs`; `main.rs` gains the `compile_error!` guard naming ADR 0013. Removes the macOS input-source seam and its config key `experimental.switch_ascii_input_source_in_prefix` — Settings › Experiments loses that row and the key now reports as a removed key at startup (same diagnostic path as `ui.agent_panel_scope`). (−2309) |
| `ce41f13` | `src/client/input/windows_vti.rs`, `src/client/{input,mod}.rs`, `src/protocol/wire.rs`, `src/raw_input.rs`, `src/main.rs` | Drop the Windows console input backend (VTI), its crossterm event conversions and the wire types that only carried Windows key records. (−2304) |
| `f21799a` | `src/pty/{actor,backend}.rs`, `src/ipc.rs`, `src/remote.rs`, `src/server/{autodetect,client_transport,clipboard_image,headless}.rs` | Drop the Windows ConPTY backend, the named-pipe IPC transport and the Windows remote/clipboard stubs. `src/ipc.rs` is Unix-socket-only; the client transport loses its named-pipe timeout branch. (−772) |
| `ee87a6a` | `src/integration/mod.rs`, `src/pane.rs`, `src/pane/{osc,terminal}.rs`, `src/update.rs`, `src/worktree.rs`, `src/sound.rs`, `src/plugin_command.rs`, `src/persist/{restore,plugin_registry}.rs`, `src/config/io.rs`, `src/config_dir.rs`, `src/app/**` (7 files), `src/api/client.rs`, `src/protocol/render_ansi.rs` | Collapse the Windows/macOS branches in the integration installer, pane launch, the updater, worktrees, sound and plugin commands to their Linux arms. KEPT by design: the `"powershell"` JSON keys in the hook-config merge (`src/integration/mod.rs` ~2528/2560) — data compatibility for dotfiles shared with other machines, not a code path. (−1368) |
| `424d975` | `src/pane/cursor.rs`, `src/pane/terminal.rs`, `src/worktree.rs`, `src/integration/mod.rs` + 6 `*/zynk-agent-state.ps1` assets, `src/config/keybinds.rs`, `src/app/api/plugins/{mod,manifest}.rs` | Residues commit 5 surfaced: the Windows cursor-settle mechanism, the worktree `is_windows` tilde-expansion thread, the always-false `remove_legacy_bash_hook_file` helper, the six orphaned PowerShell hook assets (claude, codex, copilot, droid, kimi, qodercli) and the macOS arms of `current_platform()` / `super_modifier_label()`. KEPT: the `PluginPlatform::{Macos,Windows}` enum variants — plugin-manifest data values, not target branches. (−726) |
| `457067e` | `src/zynk/db_cutover.rs`, `src/zynk/db.rs`, `src/zynk/{embedding_worker,receipt_worker,worker_pause}.rs` | Narrow ADR 0011's bundle move to the Linux `renameat2(RENAME_NOREPLACE)` arm (macOS `renamex_np`, Windows `MoveFileExW` gone); refuse-rather-than-copy is unchanged. The non-Unix `db_path_link` refusal in `sqlite_effective_path` (ADR 0011 item 25) goes with it — on Linux a symlinked DB path is always resolved the way SQLite resolves it. (−275) |
| `d649f4b` | `Cargo.toml`, `Cargo.lock`, `justfile`, `vendor/portable-pty/**`, `vendor/portable-pty.patches.md`, `scripts/test_vendor_portable_pty.py`, `scripts/windows_smoke_conpty_path.ps1` | Un-vendor `portable-pty` — the vendoring existed only to force the system ConPTY (v0.7.1 PORT LEDGER row `27cb7c3`, this file). zynk links the registry crate, so `[patch.crates-io]`, the vendor patch index, its maintenance unittest, the ConPTY smoke script and the `windows-sys` dependency block are all gone; a crates.io source build and a Git-source build now use identical PTY code. (−3312) |
| `fff320b` | `.github/workflows/{release-dryrun,build-artifacts-manual,nix}.yml`, `.github/workflows/ci.yml`, `justfile`, `scripts/release_{artifact_ids,binary,evidence,manifest}.py` + their 4 test modules, `scripts/test_release_workflow.py` | Remove the multi-target release machinery: the candidate-evidence workflow and its per-target `EVIDENCE.json` / `RELEASE_MANIFEST.txt` / ELIGIBLE vocabulary, the manual build-artifacts workflow, and the producer/consumer scripts with their maintenance unittests. `ci.yml` keeps only `check-required`. (−3444) |
| `8a7fa8c` | `flake.nix`, `flake.lock`, `nix/package.nix`, `.github/workflows/ci.yml` | Remove the Nix flake and package (its fileset referenced the vendored portable-pty) and the ci.yml trigger that watched them. (−223) |
| `fc383f6` | 41 files across `src/`, `tests/` | Predicate sweep: 193 platform `cfg` sites → 1 (the `compile_error!` guard, kept by design). 172 always-true attributes deleted, 4 `all(test, unix)` → `cfg(test)`, 14 `cfg!()` occurrences folded to literals, 3 always-false items deleted (the macOS twins in `tests/live_handoff.rs`; the `plugin_checkout_lifecycle_error` identity fn in `src/cli/plugin.rs` plus its 3 `map_err` call sites — behavior-identical on Linux), 2 dangling comments. Total `cfg` sites 476 → 288; the remainder are test/feature/arch predicates. (−298) |
| *(this commit)* | `docs/zynk/decisions/0013-linux-only-platform-scope.md` (new), `README.md`, `SECURITY.md`, `CHANGELOG.md`, `WORKFLOW.md`, `CLAUDE.md`, `DEVELOPMENT.md`, `.agents/skills/{ci-cd-and-automation,shipping-and-launch,zynk-pre-release-audit}`, `.pi/prompts/pre-release-audit.md`, `scripts/test_release_audit_refs.py`, this ledger | ADR 0013 + every active doc/skill that still promised macOS/Windows/Nix/Homebrew or multi-target evidence. README install is source-only, SECURITY names one platform and one channel, the CHANGELOG 3.1.0 section gains **Removed**, WORKFLOW release gates collapse to `just check` + `just gate` + Gate-1/2/3 + operator. `test_release_audit_refs.py` drops the `cargoHash` anchor with the Nix procedure it guarded. |

**Residues kept by design** (inert on Linux, not target branches — do not "clean up" without a decision):
`src/integration/mod.rs` ~2528/2560 `"powershell"` hook-config JSON keys (shared-dotfile data compatibility);
`src/detect/mod.rs` ~535 executable-suffix stripping (`.exe`/`.cmd`/`.bat`/`.ps1`) in agent identification —
upstream detection code, revisited in M3; `PluginPlatform::{Macos,Windows}` plugin-manifest enum variants.

---

# LINUX-ONLY + v0.8.2 PORT LEDGER — M2 build base

The upstream `v0.7.2..v0.8.2` port, on branch `port/linux-only-v0.8.2`. **Upstream relicensed** from
AGPL-3.0-or-later to Apache-2.0 at upstream `cd5ea1be` (2026-07-22) and moved to a new organization
repository at upstream `b44b5b10` (2026-07-30), so every upstream hunk taken from here on arrives under
Apache-2.0. zynk stays AGPL-3.0-or-later: Apache-2.0 §4 permits incorporating that code into the AGPL
combined work as long as the Apache license text travels with it (§4a), changed files are recorded (§4b)
and the upstream attribution notices are kept (§4c). §4d does not apply — upstream ships no NOTICE file.

| zynk | upstream | what / where / why / adaptations |
|---|---|---|
| *(this commit)* | `cd5ea1be` + `b44b5b10` | **Provenance only — no upstream code taken.** New `LICENSE-APACHE-2.0.upstream` is the Apache-2.0 text verbatim from upstream `cd5ea1be:LICENSE` (201 lines, sha256-identical), added to the Cargo `include` list so crates.io packages carry it. `NOTICE` rewritten: upstream code through upstream tag v0.7.1 was received under AGPL-3.0-or-later, upstream code from `cd5ea1be` onward under Apache-2.0 and redistributed inside the AGPL combined work; the false "the same license as upstream" sentence is gone; the attribution line carries the new upstream repository URL alongside the historical one. Adds the §4(b) section *Modified files (Apache-2.0 provenance)* with an initially empty `<!-- apache-modified-files: begin/end -->` list that later M2 commits append to as they touch post-`cd5ea1be` upstream files. `CHANGELOG.md` 3.1.0 **Changed** gains the licensing line. **EXCLUDED** from upstream `cd5ea1be`: its `Cargo.toml` `license = "Apache-2.0"` flip, `README.md` license section, `nix/package.nix` and `docs/next/*` hunks — zynk's own license is unchanged (ADR 0010) and the Nix packaging is gone (ADR 0013). **EXCLUDED** from upstream `b44b5b10`: every rename hunk (upstream URLs/branding, website and vendor-patch headers) — only the NOTICE attribution URL is adopted. |
| *(this commit)* | `4eceb75d` | **Kitty graphics: partial-update freeze + host-image leak.** Four fixes plus the vendored accessor they need. (1) `src/pane/terminal.rs` — a pty write carrying a kitty graphics sequence now requests an immediate render (`request_render = !synchronized_output`) instead of relying only on the deferred settle render, whose wake-up is dropped when the dirty flag is already set; the settle render stays as the backstop, and the dirty flag plus the minimum render interval still coalesce chunked writes. (2) `src/ghostty/mod.rs` — `kitty_image_fingerprint` now hashes the full payload instead of three 4 KiB sample windows: a source that streams updates by retransmitting one image id keeps length and dimensions constant, so a frame whose changed pixels all fell outside those windows produced the same fingerprint, emitted no graphics bytes and froze the pane on the last upload. `KITTY_FINGERPRINT_SAMPLE_BYTES` and `hash_kitty_image_data_sample` are gone. To keep the full hash off the render hot path, `Terminal` gains `kitty_fingerprints: Mutex<HashMap<u32, KittyImageFingerprintEntry>>`, keyed by image id and invalidated by the image's transmit time (plus data ptr/len), pruned against the live placement set at the end of every placement pass — one map lookup per render for a static image, one hash per transmitted frame for a streaming one. (3) `src/kitty_graphics.rs` — host image ids derive from content, so a pane image whose pixels change maps to a fresh host id every update and nothing deleted the one it replaced. `HostGraphicsCache` gains `sources: HashMap<(PaneId, u32), u32>` recording which host image backs each (pane, source image id) pair; `release_superseded_source_image` emits `a=d,d=I` for the replaced host image once no other source references it and drops its placements from the cache. (4) the same encode pass first prunes `sources` against the visible set, so an entry whose pane closed or whose placement disappeared can neither keep a dead host image alive through the shared-reference check nor accumulate without bound. **Vendor:** new patch `vendor/patches/libghostty-vt/0002-expose-kitty-image-transmit-time-ns.patch` adds `GHOSTTY_KITTY_IMAGE_DATA_TRANSMIT_TIME_NS = 9` to `include/ghostty/vt/kitty_graphics.h` and the `transmit_time_ns` arm plus the `instantNanos` helper and its zig test to `src/terminal/c/kitty_graphics.zig`, applied to the vendored tree at base `0f7cd84b` and indexed in `vendor/libghostty-vt.patches.md`; `src/ghostty/bindings.rs` gains the matching generated constant. **Adaptations:** the `src/pane/terminal.rs` hunk was re-applied by hand because the fork's `render_delay_after_pty_write` call is single-line after the ADR 0013 sweep and upstream's context did not match; the patch-index entry follows 0001's fork style — the upstream issue-tracker link is replaced by an `upstream issue:` description line, the ghostty discussion link is kept, and the rationale names zynk. Everything else is byte-identical to upstream. **Pre-relicense** (2026-07-04 < `cd5ea1be`): AGPL provenance, so no `NOTICE` modified-files entry. |
| *(this commit)* | `4a3302d1` | **Vendored libghostty-vt base bump `0f7cd84b` -> `c5a21edf`, and the API changes it forces.** The vendored tree is taken wholesale from upstream at this commit and is byte-identical to it (`vendor/libghostty-vt` tree `6ad6d74a`); the changed file set matches upstream's `vendor/` delta exactly (146 files, same add/modify/delete status). **Patch set replaced:** `0001-backport-resizecols-cursor-subtraction.patch` is dropped (upstream ghostty PR #12907 is in the new base), `0002-expose-kitty-image-transmit-time-ns.patch` (added one commit ago from upstream `4eceb75d`) is dropped too, and the new `0001-default-grapheme-cluster-mode.patch` sets `default_modes = { .grapheme_cluster = true }` in `src/terminal/c/terminal.zig` so DEC mode 2027 is on for new terminals and survives RIS. `vendor/libghostty-vt.vendor.json` loses the `source_repo` field. **The base now carries generation stamps**, so `src/ghostty/mod.rs` keys its per-image fingerprint cache on `GHOSTTY_KITTY_IMAGE_DATA_GENERATION` instead of the removed transmit-time accessor plus data ptr/len; `KittyImageFingerprintEntry` shrinks to `{ generation, fingerprint }`. `Terminal::new` now disables the glyph protocol (`GHOSTTY_TERMINAL_OPT_GLYPH_PROTOCOL = false`) so panes stop advertising a capability the fork does not implement, and `enable_grapheme_cluster_mode()` is gone (the vendor patch is the default now), taking its two call sites in `src/pane.rs` with it. `RowCellIter::grapheme_text_into` reads `GRAPHEMES_UTF8` into a `GhosttyBuffer` byte buffer instead of the codepoint array the old vendor under-reported, so its scratch parameter and every caller in `src/pane/terminal.rs` change from `Vec<u32>` to `Vec<u8>`. `src/ghostty/bindings.rs` is regenerated against the new headers (opaque handle typedefs lose the `_ptr` suffix, `GhosttyBuffer`, the color-scheme/unicode/render headers and the generation constants arrive). **Two behavior fixes ride along:** `src/pane/osc.rs` parses OSC 10/11 *multi-value* set sequences (`parse_default_color_event` -> `parse_default_color_events`, returning every event in one body) and gains `in_progress_event()` for a query still mid-sequence; `src/pane/terminal.rs` uses both to drop libghostty's own reply for a color query the fork answers itself (`remove_last_matching_libghostty_color_reply`) instead of suppressing every libghostty color reply, so untracked aggregate replies (`OSC 4;0;?;1;?`, `OSC 10;?;?;?`) now reach the child. `respond_to_default_color_event` returns the replacement reply rather than pushing it. `src/persist/restore.rs` asserts styled Unicode and a hyperlink survive history replay. **Adaptations:** the fork has no `DefaultColorQuery::Cursor` (an unported upstream commit), so the `12`/cursor arms of `parse_default_color_events` and `parse_default_color_set_events` are omitted and the catch-all rejects offset 12; `default_color_query_response` keeps the fork's `&GhosttyPaneCore` receiver (upstream widened it to `&mut`), and `default_color_event_response` reborrows into it; the four new `process_pty_bytes_*` colour tests are anchored after `process_pty_bytes_returns_default_color_query_responses_without_queuing_input` because upstream's trailing context is a cursor-query test the fork does not have. `vendor/libghostty-vt.patches.md` and the new patch file's prose header follow the fork's format and naming: the upstream issue-tracker link becomes an `upstream issue:` description, an `introduced upstream:` field is added, the rationale names zynk, the `From:` line is `vendored patch <noreply@localhost>`, and the verification commands carry `--bin zynk` because the crate is binary-only. **EXCLUDED:** nothing — upstream's delta is entirely vendor, scripts and Linux-portable source. `scripts/vendor_libghostty_vt.py` gains `require_clean_checkout` (called before and after `zig build dist`) with its two unittests in `scripts/test_vendor_libghostty_vt.py`, keeping the fork's own `zynk` description string and `../ghostty` default source path. **Pre-relicense** (2026-07-16 < `cd5ea1be`): AGPL provenance, so no `NOTICE` modified-files entry. |
| *(this commit)* | `8d2af4e0` | **Rust-side adoption of the APIs the new vendored base exposes.** Four independent adoptions. (1) **Unicode width comes from the emulator.** `src/ghostty/mod.rs` gains `unicode_codepoint_width` and `unicode_grapheme_width` over `ghostty_unicode_codepoint_width` / `ghostty_unicode_grapheme_width`, and `src/app/actions.rs` (`text_cells` plus its test helper) and `src/app/input/copy_mode.rs` (`char_cell_width`) call them instead of `unicode_width::UnicodeWidthChar`, so predicted column layout is the same width table the terminal uses when it actually prints the text. The `UnicodeWidthChar` import goes from both files; `unicode-width` stays a dependency for `UnicodeWidthStr` elsewhere. (2) **cwd comes from libghostty's pwd callback.** `WritePtyCallbackState` becomes `TerminalCallbackState { write_pty, pwd_changes }`, registered once as `GHOSTTY_TERMINAL_OPT_USERDATA` in `Terminal::new` alongside the new `GHOSTTY_TERMINAL_OPT_PWD_CHANGED` trampoline, which reads `GHOSTTY_TERMINAL_DATA_PWD` and pushes it; `set_write_pty_callback` now only installs the write callback and `take_pwd_changes()` drains the queue. That deletes the fork's hand-rolled `CwdOscTracker` OSC state machine in `src/pane/osc.rs` (~85 lines) in favour of `parse_reported_cwd`, which parses one already-terminated value (`file://` URI, bare or quoted path). Coverage widens for free: the emulator reports OSC 7, OSC 9;9 **and** OSC 1337 `CurrentDir=`, the last of which the fork's parser never handled. `process_pty_bytes` drains stale changes before the write so replayed history cannot leak a cwd into live output, and takes the last one after. (3) **Kitty placement enumeration short-circuits on a proven-empty storage generation.** `Terminal` gains a `kitty_graphics()` accessor, `kitty_graphics_generation()` over the base's new `GHOSTTY_KITTY_GRAPHICS_DATA_GENERATION` stamp (read via the new `kitty_graphics_u64` helper), and `kitty_empty_generation: Cell<Option<u64>>`; `kitty_image_placements_with_data_filter` returns early when the generation is 0 or is one already walked to zero placements, so a pane that transmitted an image but placed none, or whose images all scrolled out, costs one `get` per render instead of an iterator allocation and a full walk. The cache is only armed after a walk actually finds no stored placements, so a generation whose placements are merely off-viewport still re-walks. (4) **Absolute-row viewport scrolling.** `scroll_viewport_row` wraps `ghostty_terminal_scroll_viewport` with `GHOSTTY_SCROLL_VIEWPORT_ROW`; `ghostty_restore_scroll_offset_from_bottom` becomes `ghostty_set_scroll_offset_from_bottom` and seeks once to `max_offset - offset_from_bottom` instead of jumping to the bottom and then walking back by a relative delta, and `GhosttyPaneTerminal::set_scroll_offset_from_bottom` routes through it too, so both the resize-restore path and the scroll path clamp identically (a failed `scrollbar()` read still falls back to the bottom). **Adaptations:** the fork has no `visible_text_cells` / `logical_cell_for_visible_cell` in `src/app/actions.rs` and no `last_character_col` or copy-mode search in `src/app/input/copy_mode.rs` (unported upstream additions), so those four hunks are dropped and only the call sites the fork actually has are converted. `kitty_graphics()` / `kitty_graphics_generation()` were inserted by hand after `get_bool` because upstream's preceding context differs. In `src/pane/terminal.rs` the `reported_cwd` binding is anchored on `crate::render_prof::duration_since("pty.ghostty_write", ...)` since upstream's `#[cfg(windows)] windows_recent_fallback::update` context is gone (ADR 0013), `ghostty_set_scroll_offset_from_bottom` was re-applied by hand (the fork's preceding item is `ghostty_recent_ansi`, not `ghostty_recent_read_range`), and the new `process_pty_bytes_reports_latest_libghostty_pwd_callback` test keeps only the Linux expectation. The `src/pane/osc.rs` test hunk was re-applied by hand: the fork carries two `CwdOscTracker` tests, not three (the quoted-PowerShell one went with ADR 0013), and every fixture path is renamed to `zynk`. **EXCLUDED:** the `#[cfg(windows)]` expectation arm noted above; upstream touched no macOS/Nix/website/docs-next files here. **Pre-relicense** (2026-07-17 < `cd5ea1be`): AGPL provenance, so no `NOTICE` modified-files entry. |
