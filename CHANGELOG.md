# Changelog

## [3.1.0] — unreleased (pre-tag; the date is set when the immutable tag is cut)

The **herdr v0.7.1 port** (36 upstream changes re-applied on top of the Zynk identity — see
`docs/zynk/fork-patch-ledger.md`, *v0.7.1 PORT LEDGER*) plus a hardened single public repo. No wire/protocol
change: socket method IDs, protocol-ID fields, and the delivery/receipt matrix are unchanged. Two documented
config keys are removed (see **Changed** and **Removed**), and zynk now builds for Linux x86_64 only.

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
- **Linux x86_64 only** ([ADR 0013](docs/zynk/decisions/0013-linux-only-platform-scope.md)): zynk builds for
  `x86_64-unknown-linux-gnu`; every other target fails at compile time with a message naming that ADR. There are
  no platform tiers, no optional targets and no release artifacts to verify — distribution is source only, from
  this repository or crates.io. CI is one Ubuntu `just check` (which now includes the maintenance unittests).
- `NOTICE` now records that upstream relicensed from AGPL-3.0-or-later to Apache-2.0 (upstream commit
  `cd5ea1be`), and the repository ships that Apache-2.0 text as `LICENSE-APACHE-2.0.upstream`. zynk's own
  license is unchanged: AGPL-3.0-or-later, as recorded in `LICENSE`.

**Fixed**

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
  or a session another agent persisted on it, and never by the sender of a self-addressed message, whatever
  pane it now occupies.
- `zynk db adopt` / `zynk db backup` relocate the **complete SQLite bundle** (main file plus any `-journal`,
  `-wal`, `-shm`, a dangling sidecar link included) into one backup **directory**, `<db>.wrapper-backup-N/`,
  reserved with a single atomic `mkdir`; a leftover journal no longer strands the native path, nothing at an
  existing slot name is ever touched, each member moves in one atomic no-replace step (Linux
  `renameat2(RENAME_NOREPLACE)`; where no atomic no-replace move exists the command refuses instead of
  copying), a backup that gained
  an entry that is not a bundle member is refused and rolled back, a blocked rollback is reported naming the
  members left under the slot, a symlinked database path is relocated where SQLite would open it (the link
  stays), and `adopt` never touches the ambient `~/.zynk/zynk-v2` database when the native path was selected
  explicitly. A sidecar that is a symbolic link is refused at startup instead of being opened through. They also move aside what the startup guards refuse to open (a rollback journal that looks hot, orphan
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
- Claude Code 2.1.228 and newer spin with half-circle frames in the terminal title; a working Claude was
  detected as idle. The title rule now recognizes those frames. Because Claude keeps that busy title while an
  approval, permission or selection dialog waits for you, a dialog whose hint footer sits at the bottom of the
  screen now outranks the retained title (blocked, not working); once you answer and work continues below it,
  the title wins again.

**Removed**

- **macOS and Windows support** ([ADR 0013](docs/zynk/decisions/0013-linux-only-platform-scope.md)): the
  platform implementations, the Windows PTY / named-pipe IPC / console input paths, the darwin archive-rewrite
  path in `build.rs`, the platform-only dependencies and the non-Linux CI targets. A build for any other target
  now stops at a compile error instead of producing a binary.
- The Nix flake and package, and distribution through a package manager. zynk is source only: this repository
  or crates.io. The prebuilt release binaries, `RELEASE_MANIFEST.txt` and `SHA256SUMS` flow are gone with them.
- The multi-target release workflows (`release-dryrun.yml`, `build-artifacts-manual.yml`, `nix.yml`) and the
  release evidence/manifest scripts.
- The vendored `portable-pty` (the Windows ConPTY patch). zynk links the registry crate directly, so a
  crates.io source build and a Git-source build now use identical PTY code.
- `experimental.switch_ascii_input_source_in_prefix` (macOS-only input-source switching). The
  Settings › Experiments section loses that row, and the key now emits a removed-key startup diagnostic.
- The PowerShell hook assets for claude, codex, copilot, droid, kimi and qodercli.
- `tests/darwin_archive_parser.rs`.

**Install notes**

- When 3.1.0 is on crates.io, prefer `cargo install zynk --version 3.1.0 --locked` (without `--locked` Cargo
  ignores the packaged lockfile).
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
