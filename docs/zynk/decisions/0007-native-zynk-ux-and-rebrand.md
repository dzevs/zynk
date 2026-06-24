# ADR 0007 — Native Zynk UX & user-facing rebrand

**Status:** Proposed (gate: Arbiter review → operator). Builds on ADR 0001; relates to ADR 0002/0004/0005.
**Date:** 2026-06-15 · **Spec:** `docs/zynk/SPEC.md` §4 (command surface) · **Milestone:** M6.

## Context

Native Zynk is the product. The frozen wrapper-era `zynk` v1.5.1 and the live upstream `zynk`
install are **temporary scaffolding** the operator expects to uninstall at a later, explicitly-gated
cutover — they are NOT to be preserved as the final story, and user-facing surfaces must not imply
they are permanent. Today the data-dir name is already `zynk`/`zynk-dev` (`src/config/io.rs`), but the
produced binary is `zynk`, the CLI is a hand-rolled positional dispatcher (not clap) with ~277
user-facing `zynk` help/usage/error strings, and the only native message commands are
`zynk zynk {query,message-received}`. The six other user-facing verbs the operator wants
(`send/reply/thread/inbox/whoami/who`) do not exist as commands, though the durable data + write/read
primitives to back them all exist (M2–M5c). M6 makes native UX + rebrand **repo-ready** without any
live install/dogfood/cutover.

## Decision

1. **Binary name = `zynk`.** Add `[[bin]] name = "zynk"` (path `src/main.rs`); the produced executable
   is `zynk`. The Cargo **package/crate name stays `zynk`** (a broad internal `zynk`→`zynk` rename is
   forbidden — it would destroy upstream-merge survivability). `cargo update -p zynk` and crate-internal
   symbols are unaffected. There is **no `zynk` product binary**: the in-repo integration hooks are
   rebranded to invoke `zynk`, so nothing in-repo needs `zynk`. Any retained `zynk` invocability is a
   **bounded, documented transitional bridge** for already-installed legacy hooks, resolved by
   reinstalling integrations at cutover — never presented as the final UX.

2. **Native command surface (all return the F4 envelope).** Top-level verbs on the `zynk` binary:
   - `zynk send` / `zynk reply` — thin native verbs over the existing transport (resolve target →
     `PaneSendInput` atomic submit, the `agent send`/`pane run` path); body purity, footer semantics,
     and honest delivery states unchanged. **`reply` has no `--reply-to`** (SPEC §5: parent is
     auto-derived via `derived_parent_id`); `reply <target>` sends with the same auto-derivation;
     replying to a *non-latest* message stays "quote in body" (unchanged design).
   - `zynk thread` / `zynk inbox` — **read-only** over the global DB via `open_query_readonly`
     (`PRAGMA query_only=1`), runtime-scoped on `socket_namespace`, **zero delivery-event writes**.
     `thread` walks `conversation_id`/`conversation_seq`/`derived_parent_id`; `inbox` lists
     messages addressed to the caller with their honest `delivery_status`. No new "unread" column is
     invented (unread, if ever, derives from delivery state — out of scope, non-blocking).
   - `zynk whoami` / `zynk who` — live-socket compose (`pane.get` / `agent.list` / `pane.list`).
     Identity is **hook-authoritative** (`agent_session`/`authoritative_receiver_identity`); any
     detection-derived label is surfaced as explicitly detected, never as authoritative (M3b predicate).
   - `zynk query` — the existing retrieval command, promoted to a top-level verb; the legacy
     `zynk` subcommand group (`zynk zynk query/message-received`) is **retained transitionally** for
     back-compat.
   No invariant is weakened: receipt stays server-authoritative (`zynk.message_received` only; ADR 0002
   §Decision 4); `pane send-text` stays drafted unless the ADR 0004 exact-proof is solved; query stays
   read-only.

3. **User-facing rebrand + strict residual classification.** Every user-facing surface presents Zynk:
   CLI binary/help/usage/banner/examples/errors/status; README/docs/SPEC current-state wording;
   justfile/dev commands; config/socket/env docs + default examples (incl. fixing the stale
   `~/.config/zynk/config.toml` hint — runtime is already `~/.config/zynk[-dev]`); integration
   install/status/uninstall/help text + user-visible asset names/paths/hooks; runtime socket/log
   basenames (now in-scope, see §5); M6 reports. Every remaining `zynk` token is classified: **(1)**
   upstream provenance/license/history; **(2)** private internal detail, not user-visible, justified for
   upstream-merge survivability; **(3)** bounded transitional compat alias, clearly labeled; **(4)**
   user-facing residue that makes Zynk look like Zynk or implies the wrapper/Zynk stack is permanent — a
   **BUG**, fixed before completion (zero category-4 at completion). Release-asset filenames
   (`zynk-{target}`) stay category-2 (release infra is M8-gated). Internal crate/package symbol `zynk`
   stays category-2 (survivability).

4. **Normal dev/test UX (no operator-facing wrapper).** `just` recipes (`check`, `test`, `test-one`,
   `lint`, `build`, …) internally enforce isolation + live-env scrubbing (the `scripts/zynk-dev.sh`
   guard logic is invoked/folded so a bare `just check` is safe), so operators never type
   `CARGO_TARGET_DIR=… ./scripts/zynk-dev.sh …`. The low-level wrapper remains as the internal guard the
   recipes call. The validation commands and their isolation proof are documented + in the M6 report.

5. **Config/data separation, env overrides, and socket/log naming.**
   - **Config and data are separate trees.** Config lives at `$XDG_CONFIG_HOME/zynk/config.toml`
     (default `~/.config/zynk/config.toml`) — the runtime dir is **already** `zynk`/`zynk-dev`
     (`src/config/io.rs::app_dir_name`); only the stale `~/.config/zynk/config.toml` hint in the
     embedded `DEFAULT_CONFIG` template must be fixed. Data (the SQLite DB + artifacts) lives at
     `$ZYNK_HOME/zynk.db` (default `~/.zynk/zynk.db`; ADR 0008). Config is **never** placed under the
     data home, and no product-facing `~/.config/zynk` path remains.
   - **Zynk-branded env overrides are primary; `ZYNK_*` are bounded transitional compat.** The
     user-facing override env vars expose `ZYNK_*` as the primary, documented name and accept the
     existing `ZYNK_*` as a clearly-transitional compat alias: `ZYNK_CONFIG_PATH` (primary) /
     `ZYNK_CONFIG_PATH` (compat); likewise `ZYNK_SOCKET_PATH` / `ZYNK_SOCKET_PATH`,
     `ZYNK_CLIENT_SOCKET_PATH` / `ZYNK_CLIENT_SOCKET_PATH`, `ZYNK_SESSION` / `ZYNK_SESSION`. **When
     both are set the Zynk-branded var wins**, and this precedence is tested. The host-protocol vars the
     binary sets for its hooks (`ZYNK_ENV`, `ZYNK_PANE_ID`) gain `ZYNK_*` primaries too, exported
     alongside `ZYNK_*` (read ZYNK-first, ZYNK fallback) since the in-repo hooks are rebranded in the
     same cycle; `ZYNK_*` remains a category-3 transitional bridge for already-installed legacy hooks.
   - **Socket/log basenames are user-facing → Zynk-branded.** `zynk.sock` → `zynk.sock`,
     `zynk-client.sock` → `zynk-client.sock`, `zynk.log`/`zynk-client.log`/`zynk-server.log` →
     `zynk*.log`. Sockets and logs are created fresh at runtime, so the rename needs no migration; the
     dev preflight + isolation continue to apply to the new names.

## Alternatives considered

- **Keep the binary `zynk`** — rejected: a `zynk`-named product binary + `zynk …` help is exactly the
  category-4 user-facing residue the operator forbids.
- **Ship both `zynk` and `zynk` binaries co-equally** — rejected: presents `zynk` as a permanent
  co-equal UX, needs `--bin` across wrapper/justfile/tests, and contradicts "native is the product".
- **Broad internal `zynk`→`zynk` crate/symbol rename** — rejected: destroys `git merge upstream`
  survivability (the fork's core constraint); the package name stays `zynk`.
- **New `--reply-to` flag for `reply`** — rejected: contradicts the accepted SPEC §5 / ADR 0002 design
  (parent auto-derived; non-latest = quote in body).

## Consequences

- The produced binary is `zynk`; help/usage/errors/integration hooks reference `zynk`; the crate/package
  stays `zynk` internally (upstream-merge-safe). Tests that locate the binary move from
  `CARGO_BIN_EXE_zynk` to `CARGO_BIN_EXE_zynk`.
- User-facing override env vars expose `ZYNK_*` primaries (`ZYNK_CONFIG_PATH`, `ZYNK_SOCKET_PATH`,
  `ZYNK_CLIENT_SOCKET_PATH`, `ZYNK_SESSION`) that win over the retained `ZYNK_*` transitional aliases;
  `ZYNK_ENV`/`ZYNK_PANE_ID` gain `ZYNK_*` primaries exported alongside the `ZYNK_*` bridge. Socket/log
  basenames become `zynk*.sock`/`zynk*.log`. Config stays at `~/.config/zynk/`, data at `~/.zynk/`.
- Legacy already-installed hooks that call `zynk` are fixed by reinstalling integrations at the
  operator's later cutover (M7); M6 ships the rebranded installers but performs no live install.
- `send/reply/thread/inbox/whoami/who` become first-class, each F4-enveloped and invariant-preserving.
