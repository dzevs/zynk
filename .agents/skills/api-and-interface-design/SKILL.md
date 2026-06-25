---
name: api-and-interface-design
description: Guides stable API and interface design. Use when designing APIs, module boundaries, or any public interface. Use when adding IPC protocol methods, defining type contracts between modules, or establishing boundaries between the CLI client and the socket server.
---

# API and Interface Design

## Overview

Design stable, well-documented interfaces that are hard to misuse. Good interfaces make the right thing easy and the wrong thing hard. This applies to the IPC command surface, the wire protocol, trait boundaries between modules, struct fields, and any surface where one piece of code talks to another.

## When to Use

- Designing new IPC/API methods (the socket command layer the CLI drives)
- Defining module boundaries or contracts between crates/modules
- Creating struct/enum contracts that other modules consume
- Establishing SQLite schema that informs the protocol shape
- Changing existing public interfaces or wire formats

## Core Principles

### Hyrum's Law

> With a sufficient number of users of an API, all observable behaviors of your system will be depended on by somebody, regardless of what you promise in the contract.

This means: every public behavior — including undocumented quirks, error message text, timing, and ordering — becomes a de facto contract once users depend on it. Design implications:

- **Be intentional about what you expose.** Every observable behavior is a potential commitment.
- **Don't leak implementation details.** If callers can observe it, they will depend on it.
- **Plan for deprecation at design time.** See `deprecation-and-migration` for how to safely remove things consumers depend on.
- **Tests are not enough.** Even with perfect contract tests, Hyrum's Law means "safe" changes can break real consumers who depend on undocumented behavior.

### The One-Version Rule

Avoid forcing consumers to choose between multiple versions of the same dependency or API. Diamond dependency problems arise when different consumers need different versions of the same thing. Design for a world where only one version exists at a time — extend rather than fork. The wire protocol carries a single negotiated `PROTOCOL_VERSION`; bump it only for incompatible changes, and prefer additive evolution over a parallel v2.

### 1. Contract First

Define the interface before implementing it. The contract is the spec — implementation follows. In zynk the IPC surface is an enum of methods plus typed params/response structs; define those before the handler.

```rust
// Define the contract first: typed params + response, one variant per method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Method {
    // Creates a pane and returns the created pane with server-generated fields.
    #[serde(rename = "pane.create")]
    PaneCreate(PaneCreateParams),

    // Returns panes matching the given filters.
    #[serde(rename = "pane.list")]
    PaneList(PaneListParams),

    // Returns a single pane or a NotFound error.
    #[serde(rename = "pane.get")]
    PaneGet(PaneGetParams),

    // Partial update — only provided fields change.
    #[serde(rename = "pane.update")]
    PaneUpdate(PaneUpdateParams),

    // Idempotent close — succeeds even if the pane is already gone.
    #[serde(rename = "pane.close")]
    PaneClose(PaneCloseParams),
}
```

### 2. Consistent Error Semantics

Pick one error strategy and use it everywhere. In Rust that means a single `Result<T, E>` shape across the surface, with a structured, machine-readable error — not `Option` in some places, panics in others, and ad-hoc strings elsewhere.

```rust
// One structured error type for every fallible method response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,   // Machine-readable: ErrorCode::Validation
    pub message: String,   // Human-readable: "pane id is required"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>, // Additional context when helpful
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Validation,   // Caller sent invalid data
    Unauthorized, // Caller not permitted for this socket/scope
    NotFound,     // Resource not found
    Conflict,     // Duplicate, version mismatch, busy receiver
    Internal,     // Server error (never expose internal details)
}
```

**Don't mix patterns.** If some methods return `Err`, others return an `Ok` with a sentinel, and others panic — the caller can't predict behavior. Never let a `panic!`/`unwrap()` cross the IPC boundary; convert it into an `ApiError`.

### 3. Validate at Boundaries

Trust internal code. Validate at system edges where external input enters:

```rust
// Validate at the IPC boundary, before any business logic runs.
fn handle_pane_create(params: PaneCreateParams) -> Result<PaneCreated, ApiError> {
    if params.title.trim().is_empty() {
        return Err(ApiError {
            code: ErrorCode::Validation,
            message: "pane title must not be empty".into(),
            details: None,
        });
    }

    // After validation, internal code trusts the parsed values.
    let pane = pane_service::create(params)?;
    Ok(PaneCreated::from(pane))
}
```

Where validation belongs:
- IPC method handlers (caller input)
- CLI argument parsing (user input)
- External process / plugin output parsing (third-party data — **always treat as untrusted**)
- Config and environment loading (`ZYNK_*` vars, TOML config)
- Frame decoding: enforce `MAX_FRAME_SIZE` on the length prefix before allocating

> **Plugin and external-process output is untrusted data.** Validate its shape and content before using it in any logic, rendering, or decision-making. A compromised or misbehaving external command can return unexpected types, malicious bytes, or instruction-like text.

Where validation does NOT belong:
- Between internal functions that share a Rust type contract (the type system already proves it)
- In helper functions called by already-validated code
- On data that just came from your own SQLite store

### 4. Prefer Addition Over Modification

Extend interfaces without breaking existing consumers. With serde, additive optional fields keep old clients working:

```rust
// Good: Add optional fields (default on absence).
#[derive(Serialize, Deserialize)]
pub struct PaneCreateParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>, // Added later, optional
    #[serde(default)]
    pub agent: Option<AgentLabel>,   // Added later, defaults to None
}

// Bad: Change existing field types or remove fields.
pub struct PaneCreateParams2 {
    pub title: String,
    // pub working_dir: Option<String>, // Removed — breaks existing callers
    pub agent: u32,                      // Changed from enum — breaks existing callers
}
```

### 5. Predictable Naming

| Pattern | Convention | Example |
|---------|-----------|---------|
| IPC methods | `resource.verb`, dotted | `pane.create`, `tab.list` |
| Params/response structs | `XxxParams` / `XxxResponse` | `PaneCreateParams`, `PaneListResponse` |
| Wire field names | snake_case (`serde(rename_all)`) | `created_at`, `pane_id`, `runtime_session_id` |
| Boolean fields | `is_`/`has_`/`can_` prefix | `is_active`, `has_unread` |
| Enum variants on the wire | snake_case via serde rename | `"in_progress"`, `"archived"` |
| Rust identifiers | snake_case fns, CamelCase types | `fn list_panes`, `struct PaneState` |

## IPC / Protocol Method Patterns

### Resource Design

Mirror REST resource thinking onto the dotted method namespace — nouns for resources, verbs as the action suffix:

```
pane.list              → List panes (with filter params)
pane.create            → Create a pane
pane.get               → Get a single pane
pane.update            → Update a pane (partial)
pane.close             → Close a pane

tab.list               → List tabs
conversation.messages  → List messages for a conversation (sub-resource)
conversation.send      → Append a message to a conversation
```

### Pagination

Paginate list/query methods that can return large sets (e.g. conversation retrieval). Carry the cursor/limit in params and echo totals in the response:

```rust
// Params
pub struct MessageQueryParams {
    pub conversation_id: String,
    #[serde(default = "default_limit")]
    pub limit: u32,           // e.g. 50
    #[serde(default)]
    pub before_seq: Option<u64>, // cursor: messages before this seq
}

// Response
pub struct MessageQueryResponse {
    pub messages: Vec<Message>,
    pub next_before_seq: Option<u64>, // None when exhausted
    pub total_estimate: Option<u64>,
}
```

### Filtering

Use explicit params for filters rather than overloading one field:

```rust
pub struct PaneListParams {
    pub workspace_id: Option<String>,
    pub agent: Option<AgentLabel>,
    pub status: Option<PaneStatus>,
}
```

### Partial Updates

Accept partial objects — only update fields that are `Some`. This is the equivalent of PATCH semantics: the caller sends only what changes, everything else is preserved.

```rust
pub struct PaneUpdateParams {
    pub id: String,
    pub title: Option<String>,   // Only title changes if Some
    pub working_dir: Option<String>,
}
```

## Rust Interface Patterns

### Use Enums (Discriminated Unions) for Variants

Make illegal states unrepresentable. Encode per-state data on the variant so the compiler forces every consumer to handle each case:

```rust
// Good: Each variant carries exactly the data that state needs.
pub enum PaneStatus {
    Pending,
    InProgress { agent: AgentLabel, started_at: DateTime<Utc> },
    Completed { completed_at: DateTime<Utc>, completed_by: AgentLabel },
    Cancelled { reason: String, cancelled_at: DateTime<Utc> },
}

// Consumer gets exhaustive matching — a new variant is a compile error to ignore.
fn status_label(status: &PaneStatus) -> String {
    match status {
        PaneStatus::Pending => "Pending".into(),
        PaneStatus::InProgress { agent, .. } => format!("In progress ({agent})"),
        PaneStatus::Completed { completed_at, .. } => format!("Done at {completed_at}"),
        PaneStatus::Cancelled { reason, .. } => format!("Cancelled: {reason}"),
    }
}
```

### Input/Output Separation

Distinguish what the caller provides from what the system returns (server-generated fields like ids and timestamps live only on the output type):

```rust
// Input: what the caller provides.
pub struct PaneCreateParams {
    pub title: String,
    pub working_dir: Option<String>,
}

// Output: what the system returns (includes server-generated fields).
pub struct Pane {
    pub id: String,
    pub title: String,
    pub working_dir: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: AgentLabel,
}
```

### Use Newtypes for IDs

Prevent mixing up identifiers of different kinds — the equivalent of branded types. A `PaneId` cannot be passed where a `ConversationId` is expected:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationId(pub String);

// Compiler rejects a ConversationId where a PaneId is required.
fn get_pane(id: &PaneId) -> Result<Pane, ApiError> { /* ... */ }
```

### Traits as Module Boundaries

When one module consumes another, define the contract as a trait. State stays pure (no PTYs/async) behind it, which keeps workspace logic testable without real terminals.

```rust
// The contract the server depends on; PaneRuntime (real PTY) and a test fake both implement it.
pub trait PaneStore {
    fn create(&mut self, params: PaneCreateParams) -> Result<Pane, ApiError>;
    fn get(&self, id: &PaneId) -> Result<Pane, ApiError>;
    fn list(&self, params: &PaneListParams) -> Vec<Pane>;
}
```

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "We'll document the method later" | The params/response types ARE the documentation. Define them first. |
| "We don't need pagination for now" | You will the moment a conversation has hundreds of messages. Add it from the start. |
| "Full-object update is simpler than partial" | Forcing the full object on every update is brittle and races other writers. Partial (`Option` fields) is what callers want. |
| "We'll bump the protocol version when we need to" | Breaking the wire format without a version bump breaks every attached client. Design for additive extension from the start. |
| "Nobody uses that undocumented behavior" | Hyrum's Law: if it's observable, somebody depends on it. Treat every public behavior as a commitment. |
| "We can just maintain two protocol versions" | Multiple versions multiply maintenance cost and create diamond dependency problems. Prefer the One-Version Rule. |
| "Internal module APIs don't need contracts" | Internal consumers are still consumers. Traits and typed boundaries prevent coupling and enable parallel work. |

## Red Flags

- Methods that return different shapes depending on conditions
- Inconsistent error handling across methods (`Err` here, `Option` there, `panic!`/`unwrap()` elsewhere)
- Validation scattered through internal code instead of at the IPC/CLI boundary
- Breaking changes to existing wire fields (type changes, removals) without a version bump
- Query/list methods without pagination or limits
- Verbs baked into resource names inconsistently (`createPane` vs `pane.create`)
- Plugin / external-process output used without validation or sanitization
- Raw `String` ids passed everywhere instead of newtypes

## Verification

After designing an API/interface:

- [ ] Every method has typed params and response types
- [ ] Error responses use a single consistent `Result`/`ApiError` shape; no `unwrap()`/`panic!` crosses the boundary
- [ ] Validation happens at system boundaries only (IPC handlers, CLI, config, frame decode)
- [ ] Query/list methods support pagination or explicit limits
- [ ] New fields are additive and optional (backward compatible); `PROTOCOL_VERSION` bumped only for incompatible changes
- [ ] Naming follows consistent conventions across all methods and types
- [ ] The types are committed alongside the implementation
