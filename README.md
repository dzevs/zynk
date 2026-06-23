# zynk

**Terminal-native command center for AI agents.**

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#agent-messaging-the-zynk-layer">agent messaging</a> · <a href="#supported-agents">supported agents</a> · <a href="#integrations">integrations</a> · <a href="#configuration">configuration</a>
</p>

---

Run Claude, Codex, Pi, and other coding agents in **real terminal panes** — tmux-style workspaces, tabs, and
splits — then let them **message each other** over a native, persisted bus. Detach and the agents keep
running; reattach from anywhere. See every agent's state at a glance — blocked, working, done — and search
the whole conversation history later.

zynk is a single Rust binary that lives in the terminal you already use. Not a web dashboard, not an Electron
shell, not a screenshot wrapper around someone else's view: you see each agent's own terminal, with a
coordination layer on top.

## why zynk

Running several agents in terminals is powerful and quickly turns chaotic — panes scattered across windows,
no clear "who's blocked", handoffs lost in scrollback, no shared memory or protocol between them.

zynk gives the terminal that missing coordination layer:

- **workspaces, tabs, and panes** that persist across detach and full restart
- **agent awareness** — a sidebar that shows what every agent is doing right now
- a **native message bus** — agents address each other by pane, with honest delivery state and a persisted,
  searchable history

## what you get

- **real terminal workspaces** — workspaces (per repo or folder), tabs, and panes that are actual processes,
  not rewritten agent views
- **agent awareness** — blocked / working / done / idle, detected from process names and output, no hooks
  required
- **native agent messaging** — `zynk send` / `reply` / `thread` / `inbox` / `query`
- **persistent conversation store** — every message in a global SQLite DB, retrievable by keyword + meaning
- **detach / reattach + restore** — pane processes survive client detach; sessions restore panes after a full
  restart, with opt-in recent screen history
- **integrations** — official agent hooks add native session identity and semantic state reporting
- mouse-native throughout, 18 built-in themes, keyboard copy mode, and sound/toast notifications

## install

Install with Homebrew, download a prebuilt binary, use Nix, or build from source.

**Homebrew** (macOS and Linux):

```bash
brew install dzevs/tap/zynk
```

This installs the prebuilt v3.0.0 binary from [GitHub Releases](https://github.com/dzevs/zynk/releases); on
Linuxbrew the binary is glibc-dynamic and requires **glibc ≥ 2.30**.

**Prebuilt binary** (without Homebrew) — Linux x86_64 (glibc ≥ 2.30):

```bash
curl -LO https://github.com/dzevs/zynk/releases/download/v3.0.0/zynk-v3.0.0-linux-x86_64.tar.gz
curl -LO https://github.com/dzevs/zynk/releases/download/v3.0.0/SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS
tar -xzf zynk-v3.0.0-linux-x86_64.tar.gz && install -m 755 zynk ~/.local/bin/zynk
zynk --version
```

Targets: `linux-x86_64`, `linux-aarch64` (GNU/glibc dynamic, **glibc ≥ 2.30**), `macos-x86_64`,
`macos-aarch64`, `windows-x86_64` — always verify against `SHA256SUMS`. The macOS and Windows binaries are
**unsigned**: on macOS clear the quarantine (`xattr -dr com.apple.quarantine ./zynk`), and on Windows use
SmartScreen's "More info → Run anyway".

**Nix:**

```bash
nix run github:dzevs/zynk
```

**Build from source** — requires Rust (stable), **Zig 0.15.2** (the bundled `libghostty-vt` is built with
Zig), and **network access during the build** (the Zig build fetches libghostty-vt's Zig package
dependencies into Zig's global cache; offline builds are not supported yet):

```bash
git clone https://github.com/dzevs/zynk
cd zynk
cargo build --release --locked
./target/release/zynk
```

`cargo install zynk` installs the native Zynk app **from source** (the `zynk` 3.x crate — the 2.x crate was
the retired ACP protocol/helper CLI). It needs Rust (stable), **Zig 0.15.2**, and **network access during the
build**: Cargo fetches the crate, then the build fetches libghostty-vt's Zig package dependencies into Zig's
global cache (`cargo install --offline` is not supported). For a no-build install, most users should prefer
Homebrew, the GitHub Release binaries, or Nix above.

## quick start

Start zynk in the directory where the work lives:

```bash
zynk
```

zynk starts or attaches to one background session server. When a session has no workspaces, zynk opens one
automatically. Run an agent in the root pane. Press `ctrl+b`, then `shift+n` to create another workspace,
`ctrl+b`, then `v` or `minus` to split panes, `ctrl+b`, then `c` to create a tab, and `ctrl+b`, then `w` to
switch workspaces.

Press `ctrl+b q` to detach the client. The server and pane processes keep running. Open another terminal and
run `zynk` again to reattach.

## core concepts

**Server and client.** By default, `zynk` attaches to a background server. Detaching closes only the client. `zynk server stop` stops the default server and kills its panes. Named sessions are separate server namespaces: use `zynk session attach work`, `zynk session stop work`, and `zynk session list` when you want fully separate runtime state.

**Workspaces, tabs, panes.** A workspace is the project-level container. Tabs group panes inside a workspace. Panes are real terminal processes, not rewritten agent views.

**Copy.** zynk copies pane text, not the sidebar. Drag-select inside a pane, double-click a word or token, or press `prefix+[` for keyboard copy mode. In copy mode, move with `h/j/k/l`, `w/b/e`, and `{`/`}`, start selection with `v` or Space, copy with `y` or Enter, and leave with `q` or Esc. In PuTTY and some SSH terminals, hold `Shift` while dragging to use the terminal's own selection, and `Shift` + right click to paste.

**Update and restore.** Auto-update and self-update remain fail-closed until release-manifest hosting exists, so `zynk update` does not fetch yet — update manually with `brew upgrade dzevs/tap/zynk` (if installed via Homebrew), by installing a newer [GitHub Release](https://github.com/dzevs/zynk/releases) binary, via Nix, or by rebuilding from source (`git pull && cargo build --release --locked`). A running server keeps using the old process until it is stopped, so stop the old server to pick up the new binary. Stopping exits pane processes. Run `zynk server stop`, then run `zynk` again for the default session. For a named session, run `zynk session stop <name>`, then run `zynk session attach <name>` again. With current official integrations installed, supported agent panes can restart from their native agent sessions after a server restart.

**Keybindings.** zynk uses explicit keybinding strings. `prefix+n` means press the configured prefix, then `n`. `ctrl+alt+n`, `cmd+k`, `alt+1`, and function-key chords are direct terminal-mode shortcuts and do not need the prefix. Plain direct printable keys such as `n` steal normal typing, so use `prefix+n` unless you intentionally want a modifier-gated direct binding.

**Agent awareness.** The sidebar shows blocked, working, done, and idle states. Detection works with process names and terminal output by default. Official integrations can add native session identity for restore, semantic state reports, or both.

## agent messaging (the zynk layer)

This is zynk's net-new layer on top of the multiplexer. Agents send each other **plain-text messages**; zynk
auto-attaches structured protocol metadata, prepends a visible awareness header, persists every message to a
global SQLite store, tracks honest delivery/receipt state, and lets agents retrieve past messages by keyword
+ meaning.

```bash
zynk send  <target> <text> [--type review|approve|…]   # resolve target → atomic submit, persisted
zynk reply <target> <text>                             # parent auto-derived; no --reply-to
zynk thread <conversation>                             # read-only: walk a conversation
zynk inbox                                             # read-only: messages addressed to you
zynk whoami                                            # this pane's hook-authoritative identity
zynk who                                               # live agents / panes in the session
zynk query <text> [--workspace|--conversation|--agent|--since|--limit]   # hybrid retrieval
```

Design guarantees (binding):

- **Honest delivery.** Send results distinguish `drafted` and `submitted`; persisted delivery events can
  later advance to `received` through the server-authoritative `zynk.message_received` event, or to `failed`
  on delivery failure. zynk never collapses these states. A `received` state comes from the receiving
  integration's event, never from screen scraping, status markers, or a socket ACK. A receiver without the
  zynk integration honestly stays at `submitted`.
- **Body purity.** The message body is pure text; all provenance (agent identity, workspace/tab, branch,
  `git_sha`, cwd) plus zynk's own protocol IDs (`message_id`/`conversation_id`/`conversation_seq`) are persisted
  as structured protocol metadata, indexed separately from the body. The protocol IDs + sender identity also
  render in a visible awareness header prepended to agent-targeted messages (awareness, not receipt proof).
- **Structured responses.** No silent success and no bare `ok`. Every command returns a clear structured
  response (stable JSON for automation + concise human text), with `result`, the relevant ids, delivery
  state, and a `next` hint.
- **Read-only retrieval.** `zynk query`, `zynk thread`, and `zynk inbox` open the DB read-only
  (`PRAGMA query_only=1`) and write **zero** delivery events. `query` is hybrid: FTS5 keyword (BM25) +
  on-device embeddings via sqlite-vec, fused with RRF, with metadata prefilters.

Agents can drive zynk over the same local Unix socket — create workspaces, split or zoom panes, spawn
helpers, read output, wait for state changes, and message each other. Start with [`SKILL.md`](./SKILL.md).

## update

Auto-update, self-update, and update channels remain fail-closed until release-manifest hosting exists, so
`zynk update` and `zynk channel set <stable|preview>` return a "not available" notice rather than fetching.
To update, run `brew upgrade dzevs/tap/zynk` (if installed via Homebrew), download a newer
[GitHub Release](https://github.com/dzevs/zynk/releases) binary, use `nix`, or rebuild from source:

```bash
git pull && cargo build --release --locked
```

If a session is still running the old server, use the same stop-and-run-again flow above to pick up the new
binary. Self-update, "new version available" notifications, and the stable/preview update channels remain
deferred until release-manifest hosting exists.

## how it compares

|                          | tmux | gui managers | zynk |
|--------------------------|------|--------------|------|
| persistent sessions       | ✓    | —            | ✓     |
| detach / reattach        | ✓    | —            | ✓     |
| panes, tabs, workspaces  | ✓    | ✓            | ✓     |
| agent awareness          | —    | ✓            | ✓     |
| lives in your terminal   | ✓    | —            | ✓     |
| real terminal views      | ✓    | —            | ✓     |
| mouse-native            | —    | ✓            | ✓     |
| lightweight binary       | ✓    | —            | ✓     |
| agents can orchestrate   | ?    | ?            | ✓     |
| native agent-to-agent messaging | — | —          | ✓     |
| persisted + retrievable conversation | — | —      | ✓     |

tmux gives you persistence and panes, but it was built before agents existed. gui managers show agent state, but they make you leave your terminal and use their wrapped view. zynk is persistence, awareness, and a native multi-agent conversation layer in one tool that stays out of your way.

## remote and attach

zynk works over normal SSH. Run it on the remote host, detach, and reattach later:

```
ssh you@yourserver
zynk
```

You can also attach from your local terminal without opening a shell first:

```bash
zynk --remote workbox
zynk --remote ssh://you@yourserver:2222
```

Remote attach adds fallback SSH keepalives by default while preserving your own SSH config. Set `[remote].manage_ssh_config = false` to use plain `ssh`.

Direct attach connects your current terminal to one server-owned terminal:

```bash
zynk agent attach <target>
zynk terminal attach <terminal_id>
```

## agent awareness

The sidebar shows which agents are blocked, working, or done. Workspaces roll up to their most urgent state so you can scan the full list at a glance.

States:

- 🔴 **blocked** — agent needs input or approval
- 🟡 **working** — agent is actively running
- 🔵 **done** — work finished, you have not looked at it yet
- 🟢 **idle** — done and seen

Detection works by reading the foreground process and terminal output — zero config, no hooks required. Official claude code, codex, github copilot cli, droid, kimi code cli, qodercli, and cursor agent cli integrations provide session restore identity; pi, omp, opencode, kilo code cli, hermes, and custom socket integrations can report their own state.

## supported agents

Automatic detection works out of the box — process-name matching plus terminal-output heuristics.

| agent | idle / done | working | blocked |
|-------|-------------|---------|---------|
| [pi](https://pi.dev) | ✓ | ✓ | partial |
| [claude code](https://docs.anthropic.com/en/docs/claude-code) | ✓ | ✓ | ✓ |
| [codex](https://github.com/openai/codex) | ✓ | ✓ | ✓ |
| [droid](https://factory.ai) | ✓ | ✓ | ✓ |
| [amp](https://ampcode.com) | ✓ | ✓ | ✓ |
| [opencode](https://github.com/anomalyco/opencode) | ✓ | ✓ | ✓ |
| [grok cli](https://x.ai/grok) | ✓ | ✓ | ✓ |
| [hermes agent](https://github.com/NousResearch/hermes-agent) | ✓ | ✓ | ✓ |
| [kilo code cli](https://kilo.ai/) | ✓ | ✓ | ✓ |
| cursor agent | ✓ | ✓ | ✓ |
| antigravity cli | ✓ | ✓ | ✓ |
| kimi code cli | ✓ | ✓ | ✓ |
| [github copilot cli](https://github.com/features/copilot) | ✓ | ✓ | ✓ |
| [qodercli](https://qoder.com/cli) | ✓ | ✓ | ✓ |
| [kiro cli](https://kiro.dev/docs/cli/) | ✓ | ✓ | — |

Detected but not fully tested: gemini cli, cline.

For agents outside the built-in list, zynk still works as a terminal multiplexer with workspaces, panes, and tiling. Custom integrations can report agent labels over the socket API.

## integrations

Official integrations have two roles. claude code, codex, github copilot cli, droid, kimi code cli, qodercli, and cursor agent cli report session identity for native restore, while their state still comes from screen detection. pi, opencode, kilo code cli, and hermes report both semantic state and session identity. omp reports semantic state without native session restore. Install with:

```bash
zynk integration install pi
zynk integration install omp
zynk integration install claude
zynk integration install codex
zynk integration install copilot
zynk integration install droid
zynk integration install kimi
zynk integration install opencode
zynk integration install kilo
zynk integration install hermes
zynk integration install qodercli
zynk integration install cursor
```

## keybindings

Press `ctrl+b` to enter prefix mode. Default actions are prefix-first and tmux-like:

| key | action |
|-----|--------|
| `prefix+c` | new tab |
| `prefix+n` / `prefix+p` | next / previous tab |
| `prefix+1..9` | switch tab |
| `prefix+w` | workspace navigation |
| `prefix+g` | session navigator |
| `prefix+shift+n` | new workspace |
| `prefix+shift+g` | new worktree |
| `prefix+shift+w` | rename workspace |
| `prefix+shift+d` | close workspace |
| `prefix+h/j/k/l` | focus pane |
| `prefix+shift+h/j/k/l` | swap pane |
| `prefix+v` / `prefix+minus` | split pane |
| `prefix+x` | close pane |
| `prefix+b` | toggle sidebar |
| `prefix+z` | zoom pane |
| `prefix+r` | resize mode |
| `prefix+q` | detach |

Mouse is supported throughout. Resize mode uses `h`/`l` for width, `j`/`k` for height, and `esc` to exit.

## configuration

zynk separates **config** from **data**:

- **Config:** `~/.config/zynk/config.toml`. Override the config-file path with `ZYNK_CONFIG_PATH`.
- **Data:** the global conversation SQLite DB at `~/.zynk/zynk.db`. Override the data home with `ZYNK_HOME`,
  or the DB directory with `ZYNK_SQLITE_HOME`. Config is never placed under the data home.

```bash
zynk --default-config   # print full default config
```

In-app settings cover theme, sound, and toast preferences. zynk writes runtime logs under its config dir
(`~/.config/zynk/`); in persistent session mode, `zynk-client.log` and `zynk-server.log` are usually the
useful files.

If a database from an earlier build already occupies `~/.zynk/zynk.db`, zynk **fails closed** rather than
overwrite it, and points you at the explicit `zynk db` adopt/backup/import action.

## docs

- [`SKILL.md`](./SKILL.md) — reusable agent skill for driving zynk over the socket
- [`AGENTS.md`](./AGENTS.md) — agent & contributor guide
- [`CONTRIBUTING.md`](./CONTRIBUTING.md) — how to contribute

If you are an AI agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and
[`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/dzevs/zynk
cd zynk
cargo build --release --locked
./target/release/zynk

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

Tests are hermetic — each spawns its own temporary config and socket — so `just test` / `just check` are safe
to run directly. See [`CLAUDE.md`](./CLAUDE.md) and [`AGENTS.md`](./AGENTS.md).

## provenance & license

zynk is a fork of **[herdr](https://github.com/ogulcancelik/herdr)**, a terminal workspace manager by
ogulcancelik and the herdr contributors. zynk keeps herdr's terminal-multiplexer foundation and adds a
net-new multi-agent conversation layer (global persistence, structured protocol metadata + a visible message
header, honest delivery/receipt, and hybrid retrieval).

zynk is distributed under the GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later); the fork
preserves that license unchanged. Upstream copyright notices and the AGPL license are preserved — see
[`LICENSE`](./LICENSE) and [`NOTICE`](./NOTICE).

- Copyright (C) ogulcancelik and the herdr contributors (upstream herdr).
- Copyright (C) 2026 Zevs <hi@zevs.gg> — the zynk fork and its additions.

The `zynk` crate on crates.io (the 2.x line) was a separate, now-retired protocol/helper CLI (MIT). This
native terminal app continues the name at the **3.x** line under AGPL-3.0-or-later — a different, new product.

New zynk-layer code is also AGPL-3.0-or-later as part of the combined work. Per AGPL, complete corresponding
source is available with any conveyed or network-served build.
