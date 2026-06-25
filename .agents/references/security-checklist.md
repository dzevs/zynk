# Security Checklist

Quick reference for zynk's security. zynk is a local terminal workspace manager: a CLI client talks to a per-user server over a **Unix domain socket**, panes run untrusted agent/terminal output, and a SQLite conversation layer persists messages. The threat model is **local**: there is no web surface, no remote-by-default listener, no browser. Use alongside the `security-and-hardening` skill.

## Table of Contents

- [Pre-Commit Checks](#pre-commit-checks)
- [Socket & Server Surface](#socket--server-surface)
- [Session & Identity](#session--identity)
- [Process Spawning](#process-spawning)
- [Frame & Allocation Guards](#frame--allocation-guards)
- [SQL / Persistence](#sql--persistence)
- [Untrusted Pane / Terminal / Plugin Output](#untrusted-pane--terminal--plugin-output)
- [Fail-Closed Defaults](#fail-closed-defaults)
- [Error Handling & Logging](#error-handling--logging)
- [Dependency Security](#dependency-security)
- [Threat Quick Reference](#threat-quick-reference)

## Pre-Commit Checks

The repo enforces a fail-closed private-content gate (`just gate`) in pre-commit and CI; don't bypass it.

- [ ] No secrets in code (`git diff --cached | grep -iE "password|secret|api_key|token|PRIVATE KEY"`)
- [ ] `.gitignore` covers private dev artifacts and any local key/credential material
- [ ] Private-content gate passes (`scripts/check_public_tree.py` structural + `scripts/scrub_check.py` + `gitleaks`) — never `--no-verify`
- [ ] No real paths, private emails, or internal references introduced into tracked files

## Socket & Server Surface

The server binds a Unix domain socket under the user's runtime/config dir. The OS file permissions ARE the access-control boundary.

- [ ] Socket file is created with owner-only permissions (`0o600` / parent dir `0o700`) — a world-writable socket lets any local user drive the server
- [ ] Socket path lives under a per-user, non-world-writable directory (not a shared `/tmp` location another user can pre-create)
- [ ] Stale-socket handling re-binds safely (verify ownership before reusing/removing a found socket; don't blindly trust a pre-existing path)
- [ ] No network listener is opened by default — the local socket is the only transport unless the operator explicitly opts into the remote path
- [ ] The remote/relay path (if used) authenticates and is opt-in, never on by default
- [ ] Connecting clients are constrained to the same user — cross-user command injection over the socket is treated as a vulnerability

## Session & Identity

Hand-offs and message authorship are authoritative; identity must not be spoofable by pane content.

- [ ] Receiver/author identity is taken from the hook-authoritative `agent_session`, never from detection-tainted fields (e.g. `to.agent` / `effective_agent_label`) that pane output can influence
- [ ] Detection (`src/detect/`) reads only a screen snapshot for *status*, and its output is never used as a security/identity decision
- [ ] Pane `source` is treated as ephemeral hook-detected state, not an authoritative identity claim — verify against the DB participants before acting on it
- [ ] Runtime session IDs are validated against the live server, so a stale/forged session ID can't replay onto a different runtime
- [ ] Privileged/irreversible operations (merge/push/release in the dev flow) require an explicit operator gate, not just an agent message

## Process Spawning

Panes and integrations spawn child processes. The rule: **build an argv, never a shell string.**

- [ ] Child processes spawned via `Command::new`/`CommandBuilder` with explicit arguments — never by concatenating user/agent input into a shell command line
- [ ] No `sh -c "<interpolated string>"` with untrusted data — pass args as a vector so a value can't break out into shell metacharacters
- [ ] Environment passed to panes is the intended base env (`apply_pane_base_env`-style), not an unfiltered inherit of secrets
- [ ] Binary/integration discovery validates the resolved path and executable bit before running it (don't exec an attacker-droppable file from a writable dir)
- [ ] Filenames/paths from messages or config are not interpolated into command lines without validation
- [ ] Spawned-process exit/output is handled; a failed spawn fails closed, it doesn't silently fall through to an unsafe default

## Frame & Allocation Guards

The wire protocol is length-prefixed. Without guards, a hostile/garbled length prefix is a denial-of-service.

- [ ] Frame payload length is bounded before allocation (`MAX_FRAME_SIZE`) — reject oversized frames instead of allocating from an attacker-controlled length
- [ ] The length prefix can't overflow/truncate (reject lengths that exceed `u32::MAX` / the configured max rather than wrapping)
- [ ] Larger caps (e.g. for Kitty graphics) are explicit and bounded, not an unbounded escape hatch
- [ ] Reads of a declared length validate the length against the cap *before* reserving the buffer
- [ ] Deserialization of frames is robust to truncated/garbage input — it errors, it doesn't panic or hang

## SQL / Persistence

The conversation DB uses `sqlx` against SQLite. Injection here is a local-trust escalation and a data-integrity issue.

- [ ] Every query is parameterized with bind parameters — no string concatenation of message/agent/user values into SQL
- [ ] Dynamic query fragments (filters, limits) are bound, not interpolated; only a fixed, known FTS5 `MATCH` expression is built as text and even then user terms are bound
- [ ] FTS query errors are classified as *query* errors (bad input), not infra failures, and never crash the server
- [ ] Stored message bodies are never re-interpreted as code/SQL on read
- [ ] DB writes that establish provenance (delivery events, protocol-ID fields) are derived from authoritative state, not pane-tainted input

## Untrusted Pane / Terminal / Plugin Output

Everything a pane emits is untrusted: it's controlled by whatever process runs there (an agent, a shell, a remote tool).

- [ ] Terminal escape sequences from panes are handled by the emulator/screen layer, not passed through to the controlling TTY in a way that could re-program the host terminal
- [ ] Pane output is never `eval`'d, shelled out, or used to build a command line
- [ ] Pane output is never used as an identity/authorization signal (see Session & Identity)
- [ ] Plugin/integration output is treated as untrusted input — validated/bounded the same as pane output, not implicitly trusted because it came from a configured integration
- [ ] Detection logic matches explicit invariant evidence, never incidental whole-pane text an attacker could print to spoof a state

## Fail-Closed Defaults

zynk's persistence layer fails closed by design (ADR 0008): a foreign/incompatible DB is refused, not silently migrated or corrupted.

- [ ] A foreign or wrong-schema DB is rejected with a branded, structured error — never opened, migrated, or written to
- [ ] DB resolution uses the authoritative home (`ZYNK_HOME`/configured path); it does not silently fall back to writing somewhere unexpected
- [ ] Receipt/delivery rejections fail closed with structured error codes, not a permissive default
- [ ] When in doubt the server denies/aborts rather than proceeding on an unverified assumption
- [ ] An unavailable security-relevant resource (socket, DB, identity) produces a hard failure, not a degraded "open" mode

## Error Handling & Logging

```rust
// Production: structured, branded error to the client — no internal leakage
return Err(DbError::new("foreign_database", "refusing to open a non-zynk database"));

// NEVER: leak internals / raw paths / raw SQL back to the caller or into logs
// e.g. echoing err.to_string() that embeds the absolute DB path or the full SQL,
// or logging full message bodies / tokens at info level.
```

- [ ] Errors returned to clients are structured codes, not raw internal strings (no absolute paths, no raw SQL, no stack internals)
- [ ] Secrets / tokens / full message bodies are not logged; use `tracing` with care about what gets recorded
- [ ] No `unwrap()` in production code — a panic is a denial-of-service, not error handling (this is also a repo convention)
- [ ] Security-relevant events (refused foreign DB, rejected oversized frame, failed identity check) are logged for forensics, without logging the sensitive payload itself

## Dependency Security

```bash
# Audit Rust dependencies for known advisories
cargo audit

# Policy gate: licenses, advisories, banned/duplicate deps
cargo deny check

# Find unused dependencies to shrink the surface
cargo machete

# Secret scan the working tree (also wired into the private-content gate)
gitleaks detect --no-git --config .gitleaks.toml --source . --redact
trufflehog filesystem .
```

- [ ] `cargo audit` clean (no unaddressed advisories)
- [ ] `cargo deny` policy passes (licenses + advisories + bans)
- [ ] Dependency surface kept minimal; unused deps removed (`cargo machete`)

## Threat Quick Reference

| # | Threat | Prevention |
|---|---|---|
| 1 | Cross-user socket access | Owner-only socket perms (`0o600`), per-user non-world-writable dir, verify ownership on reuse |
| 2 | Identity spoofing via pane content | Use hook-authoritative `agent_session`; never trust detection-tainted identity fields |
| 3 | Command injection via spawning | Build argv with `Command`/`CommandBuilder`; never `sh -c` with interpolated input |
| 4 | SQL injection | `sqlx` bind parameters everywhere; only fixed FTS `MATCH` built as text, terms bound |
| 5 | Frame-size DoS / allocation bomb | Bound length before allocating (`MAX_FRAME_SIZE`); reject overflow-prone prefixes |
| 6 | Untrusted terminal/plugin output | Emulate, don't pass through; never eval/shell/authorize on pane output |
| 7 | Wrong/foreign DB corruption | Fail closed (ADR 0008): refuse non-zynk DB with a branded error |
| 8 | Privilege escalation via agent message | Irreversible ops (merge/push/release) require an explicit operator gate |
| 9 | Information leakage in errors/logs | Structured branded errors; no raw paths/SQL; don't log secrets or full bodies |
| 10 | Vulnerable dependencies | `cargo audit` / `cargo deny`; minimal dependency surface |
