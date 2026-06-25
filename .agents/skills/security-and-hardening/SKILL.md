---
name: security-and-hardening
description: Hardens code against vulnerabilities. Use when handling untrusted input, IPC/socket access, data storage, or external integrations. Use when building any feature that accepts data from clients, plugins, or external processes, or that touches the local socket, the conversation DB, or secrets.
---

# Security and Hardening

## Overview

Security-first development practices for a local-socket terminal workspace manager. Treat every external input as hostile, every secret as sacred, and every access check as mandatory. zynk runs untrusted agent processes inside PTYs, exposes a Unix-domain-socket command surface, and persists a conversation DB — so security isn't a phase, it's a constraint on every line that touches client input, the socket, plugins, the PTY, or the store.

## When to Use

- Building anything that accepts data over the IPC socket or from the CLI
- Implementing socket access control, namespacing, or capability checks
- Storing or transmitting sensitive data (the conversation DB, headers, receipts)
- Integrating with external processes, plugins, or agent runtimes
- Adding remote bridging, clipboard/image paste, or callback handling
- Handling tokens, keys, or PII that pass through panes or messages

## The Three-Tier Boundary System

### Always Do (No Exceptions)

- **Validate all external input** at the system boundary (IPC handlers, CLI args, frame decode)
- **Enforce frame/payload size limits** (`MAX_FRAME_SIZE`) on the length prefix *before* allocating — an attacker-controlled length is a DoS vector
- **Parameterize all database queries** — never format user input into SQL strings; use `sqlx` bind parameters
- **Restrict socket file permissions** to the owning user (mode `0600`); place sockets in a per-user runtime dir
- **Hash/redact secrets at rest** — never persist plaintext tokens; redact before logging
- **Use `tracing` with care** — never log message bodies, tokens, or full credentials
- **Fail closed** — when the DB, migration, or auth state is uncertain, refuse rather than proceed (ADR-backed)
- **Run `cargo audit`** (and `cargo deny` where configured) before every release

### Ask First (Requires Human Approval)

- Adding new authentication/capability flows or changing socket access logic
- Persisting new categories of sensitive data (tokens, PII, message bodies)
- Adding new external process / plugin / agent-runtime integrations
- Changing remote bridging or cross-host exposure of the socket
- Adding clipboard, image-paste, or file-ingest handlers
- Modifying rate limiting, backpressure, or busy-receiver handling
- Granting a pane/agent elevated capability or a wider scope

### Never Do

- **Never commit secrets** to version control (API keys, tokens, signing keys)
- **Never log sensitive data** (tokens, message bodies, full credentials)
- **Never trust a client-supplied identity** as an authorization boundary — verify the hook-authoritative session, not a self-reported label
- **Never disable size/limit guards** for convenience
- **Never pass untrusted bytes to a shell** (`sh -c`, string-interpolated commands) — spawn argv directly
- **Never store secrets in world-readable files** or a socket with loose permissions
- **Never expose internal errors / panics** across the IPC boundary to a client

## Common Vulnerability Classes (and Prevention)

### 1. Injection (SQL, OS Command)

```rust
// BAD: SQL injection via string formatting.
let q = format!("SELECT * FROM messages WHERE conversation_id = '{conv_id}'");
sqlx::query(&q).fetch_all(&pool).await?;

// GOOD: Parameterized query — the value can never alter the statement.
let rows = sqlx::query("SELECT * FROM messages WHERE conversation_id = ?")
    .bind(&conv_id)
    .fetch_all(&pool)
    .await?;

// BAD: OS command injection via a shell string built from input.
Command::new("sh").arg("-c").arg(format!("grep {pattern} log")).spawn()?;

// GOOD: Spawn argv directly — no shell, the input is a single inert argument.
Command::new("grep").arg(&pattern).arg("log").spawn()?;
```

### 2. Broken Authentication / Access Control

The socket is the trust boundary. Authorize against the *hook-authoritative* session identity, never a value the client supplied about itself.

```rust
// Always check the action is permitted for the *verified* caller, not a self-reported label.
fn handle_conversation_send(caller: &AuthenticatedSession, params: SendParams)
    -> Result<(), ApiError>
{
    // The participant must already be a verified member of this conversation.
    if !store.is_participant(&params.conversation_id, &caller.agent_session) {
        return Err(ApiError {
            code: ErrorCode::Unauthorized,
            message: "not a participant in this conversation".into(),
            details: None,
        });
    }
    store.append_message(&params.conversation_id, &caller.agent_session, params.body)
}
```

Socket hardening:

```rust
// Bind the socket in a per-user runtime dir and lock it to the owner only.
let socket_path = per_user_runtime_dir().join("zynk.sock");
let listener = UnixListener::bind(&socket_path)?;
std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
// Reject clients whose peer credentials don't match the expected uid where the OS supports it.
```

### 3. Untrusted Output Rendering (Terminal Injection)

PTY and plugin output is attacker-influenced bytes. Don't let them smuggle control sequences or instruction-like text into trusted surfaces.

```rust
// BAD: Splicing raw external output straight into a trusted render/log path.
status_line.set_text(plugin_stdout); // may contain escape sequences / spoofed status

// GOOD: Treat external output as opaque data — sanitize/strip control bytes before
// it reaches any trusted surface (status line, header, persisted message).
let safe = strip_dangerous_control_sequences(&plugin_stdout);
status_line.set_text(safe);
```

Detection follows the same rule: read a screen snapshot only, encode explicit AND/OR gates, and never match incidental whole-pane text — incidental matches are an injection surface.

### 4. Frame / Payload Size Abuse (DoS)

```rust
// Decode the u32 length prefix, then REJECT before allocating if it exceeds the cap.
let len = u32::from_le_bytes(prefix) as usize;
if len > MAX_FRAME_SIZE {
    return Err(ApiError {
        code: ErrorCode::Validation,
        message: "frame exceeds maximum size".into(),
        details: None,
    });
}
let mut buf = vec![0u8; len]; // safe: bounded allocation
```

Apply the analogous cap to graphics/clipboard payloads (`MAX_GRAPHICS_FRAME_SIZE`, `MAX_CLIPBOARD_IMAGE_PAYLOAD`) — never allocate to a client-supplied length without a bound.

### 5. Sensitive Data Exposure

```rust
// Never serialize sensitive fields into a client-facing response.
fn public_participant(p: &ParticipantRecord) -> PublicParticipant {
    PublicParticipant {
        agent_label: p.agent_label.clone(),
        joined_at: p.joined_at,
        // token / raw session value deliberately omitted
    }
}

// Load secrets from the environment / a restricted file, never hardcode them.
let signing_key = std::env::var("ZYNK_SIGNING_KEY")
    .map_err(|_| ApiError::internal("ZYNK_SIGNING_KEY not configured"))?;
```

### 6. Fail-Closed on Uncertain State

```rust
// If the DB is missing, the migration set doesn't match, or identity can't be
// established, refuse the operation rather than silently degrading.
match db::open_and_migrate(&db_path).await {
    Ok(pool) => pool,
    Err(e) => {
        tracing::error!(error = %e, "conversation DB unavailable — failing closed");
        return Err(ApiError::internal("conversation store unavailable"));
    }
}
```

## Input Validation Patterns

### Validation at the Boundary

```rust
fn validate_send_params(params: &SendParams) -> Result<(), ApiError> {
    if params.body.is_empty() {
        return Err(ApiError::validation("message body must not be empty"));
    }
    if params.body.len() > MAX_MESSAGE_BYTES {
        return Err(ApiError::validation("message body too large"));
    }
    if params.conversation_id.trim().is_empty() {
        return Err(ApiError::validation("conversation id required"));
    }
    Ok(())
}
```

### Ingest / Paste Safety

```rust
// Restrict accepted payload kinds and sizes for clipboard image / file ingest.
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const ALLOWED_IMAGE_KINDS: &[&str] = &["png", "jpeg", "webp"];

fn validate_image(kind: &str, bytes: &[u8]) -> Result<(), ApiError> {
    if !ALLOWED_IMAGE_KINDS.contains(&kind) {
        return Err(ApiError::validation("image kind not allowed"));
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ApiError::validation("image too large"));
    }
    // Don't trust the claimed kind — verify magic bytes when it matters.
    Ok(())
}
```

## Triaging `cargo audit` Results

Not all advisories require immediate action. Use this decision tree:

```
cargo audit reports an advisory
├── Severity: critical or high
│   ├── Is the vulnerable code reachable in your build?
│   │   ├── YES --> Fix immediately (update, patch, or replace the crate)
│   │   └── NO (dev-only dep, unused feature/code path) --> Fix soon, not a blocker
│   └── Is a fix available?
│       ├── YES --> Update to the patched version (mind the Zig/Rust toolchain pins)
│       └── NO --> Check for workarounds, consider replacing the crate,
│                  or add to the audit ignore list with a review date
├── Severity: moderate
│   ├── Reachable at runtime? --> Fix in the next release cycle
│   └── Dev/build-only? --> Fix when convenient, track in backlog
└── Severity: low / informational
    └── Track and fix during regular dependency updates
```

**Key questions:**
- Is the vulnerable function actually on a reachable code path?
- Is the crate a runtime dependency or build/dev-only?
- Is it exploitable in zynk's context (local socket, no network listener by default)?

When you defer a fix, document the reason and set a review date. Prefer `cargo deny` to enforce the policy in CI.

## Rate Limiting / Backpressure

A local socket still needs flow control — a misbehaving client or a flood of events can starve the server loop.

```rust
// Bound per-client in-flight work; shed or queue beyond the limit instead of
// blocking the whole server loop on one slow/busy receiver.
const MAX_INFLIGHT_PER_CLIENT: usize = 64;

if client.inflight() >= MAX_INFLIGHT_PER_CLIENT {
    return Err(ApiError {
        code: ErrorCode::Conflict, // busy / backpressure
        message: "client has too many in-flight requests".into(),
        details: None,
    });
}
```

Apply stricter limits to expensive paths (embedding/retrieval, large queries) than to cheap ones.

## Secrets Management

```
Config / env layout:
  ├── config.example.toml  → Committed (template with placeholder values)
  ├── ~/.config/zynk/*.toml → NOT committed (may hold real config)
  └── ZYNK_* env vars        → secrets injected at runtime, never baked into the binary

.gitignore must include:
  *.pem
  *.key
  .env
  CLAUDE.local.md
  any local secret / token files
```

**Always check before committing** (a content gate enforces this in pre-commit + CI):
```bash
# Scan staged changes for accidentally added secrets.
git diff --cached | rg -i "password|secret|api_key|token|BEGIN .*PRIVATE KEY"
# Project gate: gitleaks/trufflehog-style content scan + structural tree check.
```

## Security Review Checklist

```markdown
### Access control
- [ ] Socket bound in a per-user runtime dir, mode 0600
- [ ] Authorization checks the hook-authoritative session, not a client-reported label
- [ ] Participants can only act on conversations they belong to

### Input
- [ ] All external input validated at the IPC/CLI boundary
- [ ] SQL queries parameterized with sqlx bind params (no format!)
- [ ] External commands spawned as argv, never via a shell string
- [ ] Frame/payload length checked against MAX_FRAME_SIZE before allocation

### Untrusted output
- [ ] PTY/plugin output sanitized before reaching trusted surfaces
- [ ] Detection reads a snapshot, uses explicit gates, no incidental whole-pane matches

### Data
- [ ] No secrets in code or version control
- [ ] Sensitive fields excluded from client-facing responses
- [ ] Tokens/credentials never written to logs or persisted in plaintext

### Resilience
- [ ] Fails closed on uncertain DB/migration/identity state
- [ ] Backpressure / in-flight limits prevent one client starving the server
- [ ] Dependencies audited (cargo audit / cargo deny)
- [ ] Errors crossing the IPC boundary don't expose internals or panics
```

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "It's a local socket, security doesn't matter" | Local sockets get reached by other users, malware, and untrusted agent processes. The socket is your trust boundary. |
| "We'll add the size check later" | An unbounded allocation from a client length is a one-line DoS. Add the guard now. |
| "The client tells us who it is" | A self-reported identity is not an authorization boundary. Verify the hook-authoritative session. |
| "No one would craft a malicious frame" | Automated fuzzers and buggy clients will. Bound everything that comes off the wire. |
| "The framework/crate handles security" | Crates provide tools, not guarantees. You still have to bind params, set permissions, and validate. |
| "It's just a dev build" | Dev builds run inside the live runtime with real agents. Security habits from day one. |

## Red Flags

- User/client input formatted into SQL or shell command strings
- Allocations sized by a client-supplied length with no `MAX_FRAME_SIZE` check
- Secrets in source code, logs, or commit history
- IPC methods that act without checking the verified caller's membership/scope
- Socket created with default (group/world-readable) permissions
- PTY/plugin output spliced into a trusted surface unsanitized
- `unwrap()`/`panic!` reachable from an IPC handler (crashes or leaks internals)
- Dependencies with known critical advisories left unaddressed

## Verification

After implementing security-relevant code:

- [ ] `cargo audit` shows no unaddressed critical/high advisories
- [ ] No secrets in source code or git history (content gate clean)
- [ ] All external input validated at system boundaries; frames bounded by `MAX_FRAME_SIZE`
- [ ] Authorization checked against the verified session on every protected method
- [ ] SQL parameterized; external commands spawned as argv
- [ ] Socket permissions are 0600 in a per-user runtime dir
- [ ] No `unwrap()`/`panic!` can cross the IPC boundary; errors don't expose internals
- [ ] Backpressure/in-flight limits active on expensive paths
