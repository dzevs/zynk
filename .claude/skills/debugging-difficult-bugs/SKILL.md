---
name: debugging-difficult-bugs
description: Use early when debugging a medium or hard bug, especially when tests alone may not reveal the real runtime failure. Trigger this before extended TDD iteration when a bug involves runtime state, PTY/terminal ordering, persistence, streaming, async/concurrency, the TUI/manual reproduction, the IPC socket layer, or when a red or newly passing test may not model the real issue. Skip only when the root cause is already directly proven by a stack trace or deterministic test that exercises the real runtime path.
---

# Debugging Difficult Bugs

Use this skill early for medium or hard bugs where normal TDD may give false confidence because the
test does not fully capture the real bug. This is common in zynk: a passing unit test on pure
`AppState` can be green while the live `PaneRuntime`, the tokio async ordering, the PTY/terminal
stream, the unix-socket IPC layer, or the persistence path is where the real failure lives.

Core idea: **instrument the actual runtime path, reproduce the real issue, then inspect append-only
JSONL logs before deciding on a fix.**

## When to Use

Use this workflow near the start of debugging when any of these are true:

- The bug is medium or hard complexity, especially if it spans multiple modules, processes
  (CLI client vs. socket server), or TUI/runtime boundaries.
- A test is red, but the failing test might be an incomplete model of the real bug (e.g. it
  exercises `AppState` but not the live runtime).
- You are tempted to make a second speculative fix without new runtime evidence.
- The bug depends on runtime ordering, state, caching, PTY/terminal streaming, tokio concurrency,
  persistence (the conversation DB), TUI interaction, detection gates, or external services.
- The user says they can reproduce the issue manually in the live zynk runtime.
- The test passes after a change, but you are not confident it proves the actual reported bug is fixed.

Do **not** keep iterating only on tests if you do not understand the runtime behavior.

Skip this workflow only when the root cause is already directly proven by a stack trace or by a
deterministic failing test that exercises the real runtime path. If you are tempted to make a second
speculative fix, use this workflow.

## Required Approach

1. **State the uncertainty**
   - Acknowledge that the current test may not capture the actual bug.
   - Identify the real code path that must be observed (which module, which boundary — IPC handler,
     PTY read loop, render/compute_view, detection, persistence write).

2. **Add temporary unconditional instrumentation**
   - Add minimal but sufficient logs through the suspected code flow.
   - Log boundaries, meaningful branch decisions, state transitions, async ordering points, return
     values, and caught errors; do not log every line.
   - Logs must be unconditional: do **not** route them through `tracing` levels, an env var, or a
     debug flag. The point is to capture the real path regardless of how logging is configured.
   - Each log point must append one JSON object per line to a `.jsonl` file in the current working
     directory.
   - Include enough context to reconstruct the path: event name, timestamp, relevant ids
     (pane id, session id, trace id), input shape, state transitions, branch decisions, return
     values, and caught errors.

3. **Reproduce the real issue**
   - Prefer to run the reproduction yourself if possible — drive an isolated dev runtime (isolated
     `CARGO_TARGET_DIR`, never the live socket/config) so you never touch the live installed binary.
   - If the issue requires the user's environment or manual TUI interaction, ask the user to
     reproduce it after instrumentation is added.
   - Tell the user exactly which `.jsonl` file to send or ask them to tell you when reproduction is
     complete so you can inspect it.

4. **Analyze the log before fixing**
   - Read the JSONL log chronologically.
   - Compare expected flow vs actual flow.
   - Identify the first point where state or behavior diverges.
   - Only then implement the fix.

5. **Clean up instrumentation**
   - Remove all temporary unconditional logs after root cause is understood and the fix is verified.
   - Remove debug imports, helper functions, generated `.jsonl` files, and any other temporary
     artifacts.
   - Check the final diff for instrumentation remnants (`git diff`). A stray `debug_bug` call or a
     committed `.jsonl` will fail review and lint.
   - Do not leave debug files, log helpers, or noisy runtime logging in the final diff unless the
     user explicitly asks.

6. **Keep or improve tests**
   - Add or adjust a focused regression test once the real bug is understood.
   - Make the test assert the actual broken behavior discovered from logs, not the earlier incorrect
     assumption. Where possible, make it exercise the real runtime path, not just pure state.

## JSONL Logging Pattern

Use append-only JSONL in `cwd` so it works across the CLI, the socket server, tests, and manual TUI
reproduction.

### Rust

```rust
fn debug_bug(event: &str, data: serde_json::Value) {
    use std::io::Write;
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "event": event,
        "data": data,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("debug-difficult-bug.jsonl")
    {
        let _ = writeln!(f, "{line}");
    }
}
```

Call it at every meaningful branch or state transition:

```rust
debug_bug("pane.spawn", serde_json::json!({ "pane_id": pane_id, "cmd": cmd_name }));

debug_bug("ipc.handle", serde_json::json!({
    "command": command_name,
    "pane_id": pane_id,
    "has_payload": payload.is_some(),
}));

match execute_step() {
    Ok(result) => {
        debug_bug("pty.read.ok", serde_json::json!({
            "pane_id": pane_id,
            "bytes": result.len(),
        }));
        Ok(result)
    }
    Err(error) => {
        debug_bug("pty.read.err", serde_json::json!({
            "pane_id": pane_id,
            "error": error.to_string(),
        }));
        Err(error)
    }
}
```

## What to Log

Prefer compact, structured data over huge dumps.

Log:

- Function, module, or phase name.
- Stable ids: pane id, session id, runtime session id, trace id, request id.
- Input/output **shape**: keys, counts, byte lengths, statuses, pane states.
- Branch decisions and the data that caused them (e.g. which detection gate fired).
- State before and after mutation (`AppState`/`PaneState` transitions).
- Error names/messages and relevant metadata.
- Ordering markers for async tokio tasks, PTY streaming, or concurrent flows.

Avoid logging:

- API keys, auth headers, tokens, cookies, credentials, socket paths that embed secrets.
- Full pane/terminal content unless necessary and safe.
- Large payloads (full screen snapshots, full diffs) that make the log unreadable.
- Binary PTY data or full model responses unless the bug requires it.

Treat debug logs as potentially sensitive. Do not ask the user to paste them into public issues,
PRs, or shared channels unless they have reviewed/redacted them first.

If sensitive data might appear, log redacted summaries:

```rust
debug_bug("request.received", serde_json::json!({
    "has_auth": headers.contains_key("authorization"),
    "body_keys": body_keys,
    "message_count": message_count,
}));
```

## Reproduction Handoff to User

When the user needs to reproduce manually in the live TUI, say exactly this shape:

```text
I added temporary unconditional JSONL instrumentation. Please reproduce the issue once, then send me
or point me at:

<cwd>/debug-difficult-bug.jsonl

After I inspect that log, I'll remove the instrumentation and make the actual fix.
```

If multiple processes have different working directories (CLI client vs. socket server), either:

- log the absolute `std::env::current_dir()`, process role, and pid at startup, or
- write distinct files like `debug-server-flow.jsonl`, `debug-pty-flow.jsonl`, and
  `debug-client-flow.jsonl`.

## Analysis Checklist

Before writing the fix, answer:

- Did the instrumented code path actually run?
- What was the expected sequence of events?
- What was the actual sequence?
- What is the first incorrect state, missing value, duplicate event, or wrong branch?
- Does the original red test capture that exact divergence?
- If not, how should the regression test change?

## Final Verification

A difficult bug is not done until:

- The real reproduction path passes.
- The regression test fails before the fix and passes after the fix, when feasible.
- Temporary unconditional instrumentation is removed.
- The final diff contains only the fix and intentional tests.
- `just check` is clean.
- You can explain the root cause using evidence from the JSONL log.
