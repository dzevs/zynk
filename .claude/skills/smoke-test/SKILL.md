---
name: smoke-test
description: Build zynk and run a fast end-to-end sanity check of the release binary against an isolated dev runtime (server starts, panes/agents spawn, conversation rows record) before claiming a build works or passing a release gate. Use after a build or before a release.
---

# Smoke Test

A fast end-to-end sanity check that the freshly built `zynk` binary starts, serves its socket API, runs
panes/agents, and records conversation rows. This is NOT `just check` (unit/integration tests) — it exercises
the REAL binary against an isolated runtime, the gap tests can't cover.

## When to use

- After `just build` / `cargo build --release`, before claiming the binary works.
- Before a release gate (pair with `zynk-pre-release-audit`).
- After changes to the server lifecycle, IPC/API surface, PTY/pane spawn, detection, or the conversation DB.

## Hard rules

- **Isolated runtime ONLY** — never the live socket/config (`~/.config/zynk/`), the live DB (`~/.zynk/zynk.db`),
  or the default `CARGO_TARGET_DIR` (the machine runs `cargo-watch` on it). Use an isolated `CARGO_TARGET_DIR`
  plus isolated `XDG_CONFIG_HOME` / `XDG_RUNTIME_DIR` / `ZYNK_SOCKET_PATH` / `ZYNK_SQLITE_HOME` under `/tmp`.
- **`zynk preflight` first** — it resolves + asserts every runtime path and exits nonzero if any resolves to a
  live default. If preflight fails, STOP (you would hit the live runtime).
- Discover exact subcommands/flags with `zynk --help` rather than assuming them.

## Procedure

1. **Build** into an isolated target: `CARGO_TARGET_DIR=/tmp/zynk-smoke-target cargo build --release --locked`
   → `BIN=/tmp/zynk-smoke-target/release/zynk`.
2. **Isolate** the runtime: export `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, `ZYNK_SOCKET_PATH`, `ZYNK_SQLITE_HOME`
   all under `/tmp/zynk-smoke/…`; `mkdir -p` them; run `"$BIN" preflight` — it MUST exit 0 with every path under `/tmp`.
3. **Start** the headless server in the background; wait for `$ZYNK_SOCKET_PATH` to exist (poll the condition, never a fixed sleep).
4. **Exercise the core surface** — each must return a clean F4 JSON envelope, not a bare error/exit code:
   - create a workspace + pane; spawn a pane running an agent; `zynk pane list` shows it.
   - send: `agent send` / `pane run` → `submitted` (atomic send + Enter); `pane send-text` → `drafted` (no Enter).
   - read paths: `who --json`, `thread`, `inbox`, `query "<term>"` return rows and write NO delivery events.
5. **Verify the DB** (isolated `$ZYNK_SQLITE_HOME/zynk.db`): the message rows exist and `body_hash == sha256(body)`.
6. **Teardown**: stop the server, remove `/tmp/zynk-smoke*`, and confirm the live runtime/DB were never touched.

## Pass criteria

preflight exits 0 (all-isolated) · server binds the socket · pane/agent spawn · send returns the correct
`submitted`/`drafted` status · read paths return data · DB rows exist with a valid `body_hash`. Any panic, a
bare error without an F4 envelope, a `received` status (nothing should reach it yet — ADR 0002/0009), or any
touched live path = FAIL.
