---
name: debugging-difficult-bugs
description: Use early when debugging a medium or hard bug, especially when tests alone may not reveal the real runtime failure. Trigger this before extended TDD iteration when a bug involves runtime state, PTY/terminal ordering, persistence, streaming, async/concurrency, the TUI/manual reproduction, the IPC socket layer, or when a red or newly passing test may not model the real issue. Skip only when the root cause is already directly proven by a stack trace or deterministic test that exercises the real runtime path.
---

# Debugging Difficult Bugs

Reach for this skill at the start of a medium or hard bug, when an ordinary TDD loop can mislead you
because the test never touches the failure. zynk runs into this constantly: a unit test over pure
`AppState` can stay green while the breakage actually lives in the live `PaneRuntime`, in tokio's
async ordering, in the PTY/terminal stream, in the unix-socket IPC layer, or somewhere on the
persistence path.

The central move: **wire instrumentation into the path the runtime actually takes, trigger the real
failure, and study the append-only JSONL it produces before you commit to a fix.**

## When to Use

Pull this workflow in early whenever one or more of the following holds:

- The bug rates medium or hard, particularly when it crosses module lines, separate processes
  (the CLI client versus the socket server), or the TUI/runtime divide.
- A test is failing, but you suspect the failure only partially represents the real bug — for
  instance, it covers `AppState` while the live runtime goes untested.
- You feel the urge to try another guess at a fix without having gathered fresh runtime evidence.
- The defect hinges on runtime ordering, state, caching, PTY/terminal streaming, tokio concurrency,
  persistence (the conversation DB), TUI interaction, detection gates, or an external service.
- The user reports they can trigger the failure by hand inside the live zynk runtime.
- A change turns the test green, yet you cannot be sure that proves the bug as reported is actually
  resolved.

Resist the pull to keep grinding on tests alone when the runtime behavior is still a mystery to you.

Bypass this workflow only if a stack trace or a deterministic failing test that runs the genuine
runtime path has already pinned down the root cause. The moment you catch yourself reaching for a
second speculative fix, switch to this workflow.

## Required Approach

1. **Name what you don't know**
   - Admit out loud that the current test might not be hitting the real defect.
   - Pin down the exact code path you need eyes on (which module, which boundary — the IPC handler,
     the PTY read loop, render/compute_view, detection, or the persistence write).

2. **Drop in temporary, always-on instrumentation**
   - Place just enough logging across the code flow you suspect — enough to follow it, no more.
   - Capture boundaries, the branch decisions that matter, state transitions, async ordering points,
     return values, and any errors you catch; skip line-by-line noise.
   - Keep the logging unconditional: never run it through `tracing` levels, an env var, or a debug
     flag. You want the real path recorded no matter how logging happens to be set up.
   - Every log site appends a single JSON object on its own line to a `.jsonl` file under the
     current working directory.
   - Record enough to rebuild the path later: an event name, a timestamp, the ids that matter
     (pane id, session id, trace id), the shape of the input, state transitions, branch decisions,
     return values, and caught errors.

3. **Make the real failure happen**
   - Run the repro yourself when you can — drive an isolated dev runtime (isolated
     `CARGO_TARGET_DIR`, never the live socket/config) so the live installed binary stays untouched.
   - When the bug needs the user's environment or hands-on TUI interaction, hand instrumentation
     over and have them reproduce it.
   - Point the user at the precise `.jsonl` file to return to you, or have them ping you the moment
     the repro finishes so you can read it.

4. **Read the log before you touch the code**
   - Walk the JSONL log in time order.
   - Hold the flow you expected against the flow that actually happened.
   - Find the first place where state or behavior peels away from what you expected.
   - Write the fix only after that.

5. **Tear the instrumentation back out**
   - Once you understand the root cause and have confirmed the fix, strip every temporary
     unconditional log.
   - Delete debug imports, helper functions, the generated `.jsonl` files, and anything else you
     added for the hunt.
   - Scan the final diff for leftovers (`git diff`). A forgotten `debug_bug` call or a committed
     `.jsonl` will trip review and lint.
   - Leave no debug files, log helpers, or chatty runtime logging in the final diff unless the user
     asked for it.

6. **Lock the lesson into a test**
   - With the real bug now understood, write or refine a tight regression test.
   - Have the test assert the actual broken behavior the logs exposed — not the wrong guess you
     started with. Push it to run the real runtime path where you can, rather than pure state alone.

## JSONL Logging Pattern

Append-only JSONL in `cwd` is the right vehicle because it survives the CLI, the socket server, the
test harness, and a manual TUI repro alike.

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

Drop a call at each branch or state change worth recording:

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

Favor tight, structured records over sprawling dumps.

Capture:

- The function, module, or phase you are in.
- Stable ids: pane id, session id, runtime session id, trace id, request id.
- The **shape** of inputs and outputs: keys, counts, byte lengths, statuses, pane states.
- Which branch was taken and the data behind that choice (e.g. the detection gate that fired).
- State on either side of a mutation (`AppState`/`PaneState` transitions).
- Error names and messages plus the metadata that matters.
- Sequence markers for async tokio tasks, PTY streaming, or concurrent flows.

Keep out:

- API keys, auth headers, tokens, cookies, credentials, or socket paths that carry secrets.
- Whole pane/terminal contents, unless it is both needed and safe.
- Bulky payloads (full screen snapshots, full diffs) that drown the log.
- Raw binary PTY data or entire model responses, unless the bug genuinely demands them.

Assume debug logs may carry sensitive material. Don't ask the user to drop them into public issues,
PRs, or shared channels before they have looked them over and redacted as needed.

When sensitive values could surface, log a redacted summary instead:

```rust
debug_bug("request.received", serde_json::json!({
    "has_auth": headers.contains_key("authorization"),
    "body_keys": body_keys,
    "message_count": message_count,
}));
```

## Reproduction Handoff to User

When the bug needs a hands-on repro in the live TUI, phrase the handoff roughly like this:

```text
I added temporary unconditional JSONL instrumentation. Please reproduce the issue once, then send me
or point me at:

<cwd>/debug-difficult-bug.jsonl

After I inspect that log, I'll remove the instrumentation and make the actual fix.
```

When separate processes run with different working directories (the CLI client versus the socket
server), pick one of:

- log the absolute `std::env::current_dir()`, the process role, and the pid at startup, or
- split the output into named files such as `debug-server-flow.jsonl`, `debug-pty-flow.jsonl`, and
  `debug-client-flow.jsonl`.

## Analysis Checklist

Settle these before you write the fix:

- Did the instrumented path even execute?
- What sequence of events did you expect?
- What sequence actually occurred?
- Where is the first wrong state, missing value, duplicated event, or mistaken branch?
- Does the original failing test pin down that precise divergence?
- If it doesn't, what does the regression test need to become?

## Final Verification

A difficult bug stays open until all of these hold:

- The real reproduction path runs clean.
- The regression test fails before the fix and passes after it, wherever that is feasible.
- Every temporary unconditional log is gone.
- The final diff carries only the fix and the tests you meant to keep.
- `just check` is clean.
- You can narrate the root cause straight from the JSONL evidence.
