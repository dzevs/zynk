# CLAUDE.md

Guidance for Claude Code (and other agents) working in this repository.

zynk is a terminal workspace manager for AI coding agents (AGPL-3.0-or-later), a fork of
[herdr](https://github.com/ogulcancelik/herdr). Building requires Rust (stable) **and Zig 0.15.2**
(the bundled `libghostty-vt` is built with Zig); the TS asset test needs Bun.

## Commands (task runner: `just`)

- **Test:** `just test` (cargo nextest + maintenance-script tests). One test: `just test-one <filter>`. Raw: `cargo nextest run --locked <filter>`.
- **Lint:** `just lint` = `cargo fmt --check` + `cargo clippy --all-targets --locked -- -D warnings`.
- **CI/full check:** `just ci` / `just check`.
- **Build:** `just build` = `cargo build --release --locked`.
- **Git hooks:** `just install-hooks`.

## Architecture (big picture)

Rust + **ratatui** TUI, **portable-pty** for PTYs, **tokio** async, **interprocess** Unix-socket IPC. The
CLI is a thin client over a local socket server; most commands return JSON.

- **State is separated from runtime.** `AppState`/`PaneState` are pure, testable data (no PTYs/async);
  `PaneRuntime` holds the live terminal. Workspace logic does not need real terminals.
- **Render is pure.** `compute_view()` does geometry + mutations; `render()` takes `&AppState` and only draws.
  Never mutate state during render.
- **Platform code is isolated** in `src/platform/`; core modules avoid `#[cfg(target_os)]`.
- **Detection is decoupled + evidence-based.** `src/detect/` reads a screen snapshot only (never the
  parser/viewport); encode invariant vs alternative controls as explicit AND/OR gates; never match
  incidental whole-pane text; never use the scrollable user viewport for agent status.

Module map (`src/`): `app/` (state/actions/input), `server/` + `ipc.rs` + `api/` + `protocol/` (the socket
command layer the CLI drives), `pty/` + `terminal/` (PTY + emulator/screen state), `pane/`, `input/`,
`detect/`, `events.rs`, `persist/`, `remote/`, `client/`, `config/`. The conversation layer (DB/header/
protocol/retrieval) is in the `zynk_*` modules.

## Conventions

- Rust: no `unwrap()` in production code; use `tracing` for logging; OS-specific behavior lives in `src/platform/`.
- Lowercase conventional commits, no emojis, no AI co-author lines.
- **Licensing:** AGPL-3.0-or-later; preserve the upstream copyright + the `NOTICE` attribution in all builds
  (legally required — `NOTICE`/`LICENSE` carry the upstream herdr attribution).
