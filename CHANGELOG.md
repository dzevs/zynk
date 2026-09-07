# Changelog

## [3.1.0] — 2026-09-07

The **herdr v0.7.1 port** (36 upstream changes re-applied on top of the Zynk identity — see
`docs/zynk/fork-patch-ledger.md`, *v0.7.1 PORT LEDGER*) plus a hardened single public repo. No wire/protocol
change: socket method IDs, protocol-ID fields, and the delivery/receipt matrix are unchanged. One documented
config key is removed (see **Changed**).

**Added**

- Agent-panel ordering: `ui.agent_panel_sort = "spaces"` (default) or `"priority"`. Priority keeps Zynk's
  blocked > working > done ranking.
- Optional host light/dark theme switching: `theme.auto_switch = true` (default **off**) with
  `theme.dark_name` / `theme.light_name`.
- `ui.pane_borders` / `ui.pane_gaps` for pane chrome.
- `update.version_check` / `update.manifest_check` toggles. They only *disable* checks; self-update stays
  unavailable (no update-manifest hosting yet).
- Linux: `ZYNK_AGENT` environment hints identify agents running inside wrapped foreground processes.

**Changed**

- `ui.agent_panel_scope` is **no longer supported**; the agent panel shows all workspaces.
  `ui.agent_panel_sort` controls ordering only and does not restore current-workspace filtering. An old
  `agent_panel_scope` key is ignored and reported as a startup diagnostic. *Policy note:* Zynk may remove a
  documented config key in a minor release when the changelog carries a migration note and startup reports the
  removed key; this is a deliberate config change, not a backward-compatible one.
- Custom keys and prefixes now **displace** conflicting default bindings instead of being rejected; a config
  reload keeps the valid subset of bindings.
- The raw image-paste shortcut is remote-only: `keys.remote_image_paste` (default `ctrl+v`; empty disables).

**Fixed**

- Windows: the terminal backend vendors `portable-pty` and forces the **system ConPTY** (`kernel32.dll`; no
  `conpty.dll` sideload); multiline paste is preserved; npm-wrapped `pi` is detected.
- Startup: two zynk processes opening a fresh shared database at the same time (for example two named-session
  servers) no longer make the second one fail closed with a false "foreign database" error — first-time
  initialization is serialized across processes, and a database that holds only an empty migration ledger is
  treated as new. A database is recognized as zynk's own by its recorded migration lineage (versions and
  checksums), never by table names alone; any other non-empty database — whatever its objects are named — fails
  closed and its existing data bytes are left unmodified (inspecting a WAL-mode database may create the usual
  SQLite `-shm`/empty `-wal` sidecars; see ADR 0011). A server that cannot initialize or migrate its database
  now exits with the error — printed and written to its server log — instead of starting without persistence.
  The guard inspects and writes on one pinned connection (a symlink flip or rename under it cannot redirect
  the writes), creates a new database only after the init lock is held, refuses to initialize over a
  nonempty `-wal`/`-journal` left beside a missing database, counts `sqlite_*`-named user tables, reports a
  database migrated by a newer zynk as *newer* (never "ready") and refuses to open it, refuses a database
  whose rollback journal looks hot (a crashed writer) instead of rolling it back, resolves a symlinked
  database path the way SQLite does so its guards and lock apply to the file actually opened, and escapes
  control characters in the schema names and paths it prints. The Unix live-handoff replacement runs the same
  pre-flight before serving (a failure rolls back to the old server) without orphan-message recovery; the old
  server pauses its DB workers before withdrawing any service (a worker busy for longer than
  `ZYNK_HANDOFF_WORKER_IDLE_MS`, default 10 s, fails the update quickly and cleanly) and the replacement
  takes the workers over only after "committed" — so receipts keep working after a handoff, an in-flight send
  is never marked failed by it, and no embedding job runs twice.
- Several zynk servers sharing one database (named sessions, a live update) no longer trip over each other:
  an embedding job is claimed atomically and owned by that claim — a job another server is running inside its
  10-minute lease is not re-run, and a worker that stalled past the lease can neither complete nor fail the
  job its successor owns (nor write a vector under it); start-up recovery of messages that never got a delivery
  event fails only those older than five minutes, exactly once even when several servers start together, so a
  peer server's in-flight send is never marked failed by another server starting; and a receipt is accepted
  only from the participant the message was addressed to (its full hook session — source, kind and value,
  reported for that same agent — which survives pane-id churn, restarts and live updates; its terminal when
  the hook reported no session of its own), never from another pane that merely carries the same agent label
  or a session another agent persisted on it.
- `zynk db adopt` / `zynk db backup` relocate the **complete SQLite bundle** (main file plus any `-journal`,
  `-wal`, `-shm`) to one backup slot that is entirely free, all or nothing: a leftover journal no longer strands
  the native path, an existing backup sidecar is never overwritten (a dangling symlink counts as occupied, the
  move never replaces an entry that appears after the check — on any filesystem, refusing with an error when
  no non-replacing move exists — and a member is either moved or left untouched), a failed move rolls back and
  reports an error, and a symlinked database path is relocated where SQLite would open it (the link stays). They also move aside what the startup guards refuse to open (a rollback journal that looks hot, orphan
  sidecars beside a missing database) — the remedy those refusals name. Output from every `zynk db` command
  escapes control and Unicode format/bidi characters in names and paths.
- Sessions: OMP resumes in the same pane after a restart with root-only hook state; lifecycle hook generations
  re-anchor; root-agent restore ownership is protected; Pi/OMP agents are released on shutdown.
- Plugins: workspace/tab/pane lifecycle events also fire for panes created from the UI.
- Worktrees: `worktree.create` / `worktree.remove` run their Git work in the background — the UI and server
  stay responsive and the requesting client still receives the final response; duplicate/stale operations are
  guarded; forced removal recovers leftover checkouts; creating a worktree checks out an existing branch.
- Terminal/render: wide-character cells in pane text, border intersections use the active pane color, Kitty
  file/temp/shared-memory image media, split host-color replies, duplicate release-key input, focus after
  temporary pane commands.
- Agents/remote: Devin hook on Python 3.9; Copilot `ask_user` accept prompt detected; OpenCode hook scoped to
  the root agent and adopting new sessions; the idle client writer blocks instead of busy-polling; remote
  handshakes get a 60 s budget (local stays 5 s).
- Self-update messages now say accurately that self-update is unavailable; the updater remains fail-closed.

**Install notes**

- A **Windows** `cargo install zynk` (crates.io source) build links the registry `portable-pty` and therefore
  lacks only the ConPTY patch — Cargo strips `[patch.crates-io]` and excludes the nested vendored source.
  Linux/macOS source builds are unaffected; Git-source builds ship the patched copy, and so will the 3.1.0
  release binaries, Homebrew formula, and Nix package once each channel is published for 3.1.0 (until then the
  published channels are still 3.0.x). When 3.1.0 is on crates.io, prefer `cargo install zynk --version 3.1.0 --locked`
  (without `--locked` Cargo ignores the packaged lockfile).
- Refresh installed integrations (`zynk integration install …`) after upgrading to receive the hook changes.
- A **live update** (`server.live_handoff`) between a 3.0.x server and 3.1.0 — in either direction — is refused
  (handoff protocol version 2 hands the DB workers over in order; 3.0.x cannot); the running server keeps
  serving. Restart zynk normally for that upgrade.
- Contributors: Zynk is now one canonical public repo with public design docs (`docs/zynk/`) and
  contributor/tooling guides; private content is kept out by fail-closed gates.

## [3.0.1] — 2026-06-23

Source crate on crates.io: `cargo install zynk` now installs the native Zynk terminal app **from source**. It
builds with Rust (stable) and **Zig 0.15.2**, and requires **network access during the build** — after Cargo
fetches the crate, the build fetches libghostty-vt's Zig package dependencies into Zig's global cache
(`cargo install --offline` is not supported). For a no-build install, prefer the prebuilt binaries (GitHub
Releases / Homebrew) or Nix. This is a packaging-only release — app behavior is identical to 3.0.0; the
GitHub Release binaries and Homebrew formula remain at 3.0.0.

## [3.0.0] — 2026-06-20

First public installable release of the **native Zynk terminal app** (AGPL-3.0-or-later), a fork of
[herdr](https://github.com/ogulcancelik/herdr) with a net-new multi-agent conversation layer (global SQLite
persistence, native protocol metadata + a visible message header, honest delivery/receipt, and hybrid
retrieval) on top of the inherited terminal-multiplexer base (workspaces / tabs / panes / agent awareness).

This is an early, evolving release — expect rough edges.

**Downloads** ([GitHub Releases](https://github.com/dzevs/zynk/releases)): prebuilt binaries for
`linux-x86_64`, `linux-aarch64` (GNU/glibc dynamic, **glibc ≥ 2.30**), `macos-x86_64`, `macos-aarch64`, and
`windows-x86_64`, plus `SHA256SUMS`. The macOS and Windows binaries are **unsigned** (clear the macOS
quarantine with `xattr -dr com.apple.quarantine`; use Windows SmartScreen "Run anyway"). Homebrew
(`brew install dzevs/tap/zynk`), Nix (`nix run github:dzevs/zynk`), and a source build (Rust + Zig 0.15.2)
also work.

**Deferred:** `cargo install zynk` is not the native app yet (the crates.io `zynk` is the retired 2.x), and
self-update/auto-update — both planned.

**Lineage:** the 2.x `zynk` crate on crates.io was a separate, now-retired ACP portable protocol/helper CLI
(MIT); the native app continues the name on the 3.x line under AGPL-3.0-or-later.
