---
name: zynk
description: "Control zynk from inside it. Manage workspaces, tabs, and panes, spawn and coordinate agents, read pane output, wait for state changes, and exchange native messages — all via CLI commands that talk to the running zynk instance over a local unix socket. Use when running inside zynk (ZYNK_ENV=1)."
---
<!-- zynk-skill-version: 2 -->

# zynk — agent skill

Before using this skill, check that `ZYNK_ENV=1`. If it is not set to `1`, say you are not running inside a zynk-managed pane and stop.

**Your identity comes from the environment, never from focus.** You are the pane named in `ZYNK_PANE_ID` (confirm with `zynk whoami --json`). UI focus — the `focused: true` field in `pane list` — is user-controlled view state; the focused pane may be yours or any other pane. Never infer your identity, or your permission to act on a pane, from focus.

Input rules (reading is always safe; sending input is what needs care):

- Do not send text, keys, or input to your **own** pane — that types into your own agent session — unless you are explicitly told to.
- Only send input to panes you **created for this task** (and captured from a split/create JSON response), or panes the **operator explicitly assigned** to you.
- `pane read`, `pane list`, `whoami`, and `who` are always safe.

You are running inside zynk, a terminal-native workspace manager for AI coding agents. zynk gives you workspaces, tabs, and panes — each pane is a real terminal with its own shell, agent, server, or log stream — plus a native conversation layer for audited messages between agents. Drive all of it from the `zynk` binary, which is on your PATH and talks to the running instance over a local unix socket (`ZYNK_SOCKET_PATH`).

Run `zynk <command> --help` for exact flags. The public socket-API reference at https://zynk.dev/docs/socket-api/ covers released commands; net-new conversation commands are best confirmed with `--help` on this build. The wire protocol is version 14.

## Concepts

**workspaces** are project contexts. Each has one or more tabs. Unless renamed, a workspace's label follows its first tab's root pane — usually the repo name, otherwise the root pane's folder name.

**tabs** are subcontexts inside a workspace. Each tab has one or more panes.

**panes** are terminal splits inside a tab. Each pane runs its own process — a shell, an agent, a server, anything.

**agent status** is detected automatically by zynk. The public `agent_status` field reports `idle`, `working`, `blocked`, `unknown`, or `done`. `done` means the agent finished but you have not yet viewed that pane (an idle pane you have not looked at). Plain shells exist as panes, but the sidebar's agent section focuses on detected agents.

**ids** are short, stable, session-scoped handles:

- workspace: `w1`, `w2`
- tab: `w1:t1`, `w2:t1`
- pane: `w1:p1`, `w2:p2`

Ids are allocated monotonically and are **not reused** when a workspace, tab, or pane closes — a closed `w2:p2` will never later point at a different pane. But they are **session-local**, not durable global identifiers: a fresh zynk session numbers again from `w1`, and you cannot predict the next handle. Never hardcode an id. Re-read the current id from `pane list`, `workspace list`, `tab list`, `whoami --json`, or a create/split response before acting on it.

## Discover yourself

```bash
zynk pane list      # JSON: every pane + its agent, agent_status, cwd, and UI focus (`focused`)
zynk whoami --json  # which pane is YOU — pane_id/tab_id/workspace_id, agent, hook-authoritative agent_session
zynk who --json     # live participant topology of agents in the session
zynk workspace list # JSON
```

`pane list` shows which pane has UI focus; it does **not** tell you which pane is you — `ZYNK_PANE_ID` and `whoami --json` do. Use `--json` for any identity you parse; the plain forms are for humans.

## Read another pane

```bash
zynk pane read w1:p1 --source recent --lines 50
```

- `--source visible` — current viewport
- `--source recent` — recent scrollback as rendered
- `--source recent-unwrapped` — recent text with soft wraps joined back together
- `--source detection` — the bottom-buffer snapshot zynk's agent detection reads (what status detection sees)
- `--format ansi` (or `--ansi`) — rendered ANSI snapshot for TUI feedback loops

`pane read` prints text, not JSON.

## Split a pane and run a command

Split your own pane (`$ZYNK_PANE_ID`) to the right and keep focus where it is. The response is JSON with the new pane at `result.pane.pane_id` — always capture it; never assume the split produced a specific id:

```bash
NEW_PANE=$(zynk pane split "$ZYNK_PANE_ID" --direction right --no-focus \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["pane"]["pane_id"])')
zynk pane run "$NEW_PANE" "npm run dev"
```

Split downward with `--direction down`. `--no-focus` keeps your terminal context focused. You own `$NEW_PANE` (you created it), so sending input to it is fine.

## Wait for output

Block until specific text appears in a pane — useful for servers, builds, tests. For `--source recent`, matching uses unwrapped recent text, so pane width and soft wrapping do not break matches:

```bash
zynk wait output "$NEW_PANE" --match "ready on port 3000" --timeout 30000
zynk wait output "$NEW_PANE" --match "server.*ready" --regex --timeout 30000
```

Exit code is `1` on timeout.

## Wait for an agent status

```bash
zynk wait agent-status "$PANE" --status done --timeout 60000
```

Uses the same `done` / `idle` distinction the UI shows. Replace `$PANE` with the actual id you were assigned or captured.

## Send text, keys, or a command to a pane

Only to a pane you created or were assigned (see the input rules at the top):

```bash
zynk pane send-text "$PANE" -- "hello"     # text only, no Enter
zynk pane send-keys "$PANE" Enter          # press Enter or other keys
zynk pane run "$PANE" -- "echo hello"      # text + a real Enter, atomic
```

`pane send-text` and `pane run` print a JSON outcome (target resolution, delivery status, proof). `pane send-keys` prints nothing on success. Prefer `pane run` for "type this and submit it" — it avoids a stuck draft with no Enter. Both accept `--type <T>` and `--trace <id|inherit>` when the input is also a tracked message.

## Native conversation layer

zynk has a built-in, audited message store for agent-to-agent coordination. Messages are addressed to a pane target (e.g. `w2:p2`) and recorded with sender/receiver identity, a body hash, and delivery proof.

```bash
zynk send  w2:p2 [--type T] [--trace <id|inherit>] -- "<text>"   # new message (JSON result)
zynk reply w2:p2 [--type T] [--trace <id|inherit>] -- "<text>"   # reply; parent auto-derives (JSON result)
zynk thread <conversation_id|message_id> [--json]                # read-only transcript
zynk inbox [--json]                                              # messages addressed to you (defaults to caller)
zynk trace <id> [--json]                                         # every message carrying a trace id
zynk query "<text>" [--trace <id>] [filters] [--json]            # search the store
```

Key rules:

- **Header is never receipt proof.** When a message arrives you see a header (from/to/id/conv/trace/reply). It is for awareness only.
- **Send JSON proves submission, not comprehension.** Your `send`/`reply` result proves input was delivered — `delivery_status` (e.g. `submitted`) + `proof` (e.g. `pane.send_input`). It does **not** prove the recipient read or understood the message.
- **Receipt/comprehension needs evidence.** Claim a message was received or understood only from the recipient's own reply, or explicit stored evidence (`thread` / `inbox` / `query`). Never fabricate a "received".
- **Trace correlation.** `--trace <id>` tags a message; `--trace inherit` continues the current trace. Trace ids live in message metadata (retrievable with `zynk trace <id>`, filterable in `query`) — not in the body.
- **Body purity.** The body backs the body hash and full-text search; keep it pure. Do not stuff control tokens into the body expecting them to be parsed — use `--type` and `--trace`.
- **Typed messages.** `--type` carries a message type (e.g. review handoffs like `request-review` / `request-changes` / `approve`) for structured workflows.
- **Inbox scope.** `zynk inbox` defaults to your own identity; pass `--agent <label>` with a real agent label only to inspect a specific agent's inbox.
- Re-read the live target id (`zynk pane list`) before sending; ids are session-local.

## Workspace and tab management

```bash
zynk workspace create --cwd /path/to/project [--label "api"] [--no-focus]
zynk workspace focus w2
zynk workspace rename w1 "api server"
zynk workspace close w2

zynk tab list --workspace w1
zynk tab create --workspace w1 [--label "logs"]
zynk tab rename w1:t2 "logs"
zynk tab focus w1:t2
zynk tab close w1:t2

zynk pane close "$PANE"
```

Without `--label`, create keeps the default (cwd-based for workspaces, numbered for tabs).

## Agent safety

- Identity is from `ZYNK_PANE_ID` / `whoami --json`, never from `focused: true`. Focus is user view state, decoupled from who you are and what you may touch.
- Send input only to panes you created (captured from JSON) or panes the operator assigned. Do not type into your own pane unless told to.
- Re-read ids; never guess them.
- Avoid destructive shell commands in sibling panes (`rm -rf`, force resets, etc.); you are driving real terminals.
- Do not push, merge, tag, or release unless the operator explicitly approves it.
- The `zynk` on your PATH is the live installed binary controlling this session. If you build a development copy of zynk, do not point it at the live socket or config — use an isolated socket/config for dogfooding.

## Recipes

Run a server and wait until it is ready:

```bash
NEW_PANE=$(zynk pane split "$ZYNK_PANE_ID" --direction right --no-focus \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["pane"]["pane_id"])')
zynk pane run "$NEW_PANE" "npm run dev"
zynk wait output "$NEW_PANE" --match "ready" --timeout 30000
zynk pane read "$NEW_PANE" --source recent --lines 20
```

Run tests in a separate pane and inspect the result:

```bash
NEW_PANE=$(zynk pane split "$ZYNK_PANE_ID" --direction down --no-focus \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["pane"]["pane_id"])')
zynk pane run "$NEW_PANE" "cargo test"
zynk wait output "$NEW_PANE" --match "test result" --timeout 60000
zynk pane read "$NEW_PANE" --source recent --lines 30
```

Spawn a sibling agent and give it a task:

```bash
NEW_PANE=$(zynk pane split "$ZYNK_PANE_ID" --direction right --no-focus \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["pane"]["pane_id"])')
zynk pane run "$NEW_PANE" "claude"
zynk wait output "$NEW_PANE" --match ">" --timeout 15000
zynk pane run "$NEW_PANE" "review the test coverage in src/api/"
```

Coordinate with an agent you were assigned, then read its result (replace `$PEER` with the actual pane id):

```bash
zynk wait agent-status "$PEER" --status done --timeout 120000
zynk pane read "$PEER" --source recent --lines 100
```

## Notes

- JSON on success: `workspace list/create`, `tab list/create/get/focus/rename/close`, `pane list/get/split/move`, `wait output`, `wait agent-status`, `pane send-text`, `pane run`, `send`, `reply`. `thread`, `inbox`, `trace`, `query`, `whoami`, `who` emit JSON with `--json`.
- `pane read` prints text (or ANSI with `--format ansi`); `pane send-keys` prints nothing on success.
- Parse new ids from responses: `workspace create` → `result.workspace` / `result.tab` / `result.root_pane`; `tab create` → `result.tab` / `result.root_pane`; `pane split` → `result.pane.pane_id`.
- Use `pane read` for output that already exists; use `wait output` for the next output you expect.
- Agents without a skill system: the same `zynk ...` commands work directly from any shell inside a zynk pane — this file is just guidance.
