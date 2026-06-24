# Testing Patterns Reference

Quick reference for common testing patterns in zynk — a Rust workspace tested with `cargo nextest`, mixing pure-state unit tests, `#[tokio::test]` async tests, and characterization/parity tests over the wire protocol and conversation layer. Use alongside the `test-driven-development` skill.

Run the suite with `just test` (cargo nextest + maintenance-script tests); one test with `just test-one <filter>`; raw with `cargo nextest run --locked <filter>`. Tests are hermetic — each spawns its own temp config/socket.

## Table of Contents

- [Test Structure (Arrange-Act-Assert)](#test-structure-arrange-act-assert)
- [Test Naming Conventions](#test-naming-conventions)
- [Common Assertions](#common-assertions)
- [Pure-State Tests with `AppState::test_new()`](#pure-state-tests-with-appstatetest_new)
- [Async Tests (`#[tokio::test]`)](#async-tests-tokiotest)
- [Test Doubles at the Boundary](#test-doubles-at-the-boundary)
- [Hermetic Config / Socket / DB](#hermetic-config--socket--db)
- [Characterization & Parity Tests](#characterization--parity-tests)
- [Test Anti-Patterns](#test-anti-patterns)

## Test Structure (Arrange-Act-Assert)

```rust
#[test]
fn describes_expected_behavior() {
    // Arrange: set up test data and preconditions
    let mut app = AppState::test_new();

    // Act: perform the action being tested
    let result = app.create_workspace("scratch");

    // Assert: verify the outcome
    assert_eq!(result.name(), "scratch");
    assert_eq!(app.workspaces().len(), 1);
    assert!(app.active().is_some());
}
```

## Test Naming Conventions

zynk test names are descriptive snake_case sentences — the function name reads as `[unit] [expected behavior] [condition]`. No `should_` prefix needed.

```rust
mod create_workspace {
    #[test]
    fn creates_a_workspace_with_default_focus() {}
    #[test]
    fn returns_error_when_name_is_empty() {}
    #[test]
    fn trims_whitespace_from_name() {}
    #[test]
    fn generates_a_unique_id_for_each_workspace() {}
}
```

Real examples from the tree: `protocol_id_fields_omit_type_when_none`, `compute_view_clamps_sidebar_width_to_configured_max`, `failing_then_ok_one_fails_first_call_then_succeeds`.

## Common Assertions

```rust
// Equality
assert_eq!(result, expected);
assert_ne!(result, other);

// Booleans
assert!(condition);
assert!(!condition);

// Option / Result
assert!(value.is_some());
assert!(value.is_none());
assert_eq!(value, Some(expected));
let v = result.expect("descriptive context when unwrapping in a test");
assert!(result.is_err());

// Error matching
assert!(matches!(err, DbError { .. }));
assert_eq!(err.code(), "foreign_database");

// Strings / collections
assert!(output.contains("substring"));
assert_eq!(items.len(), 3);

// Floats
assert!((result - 0.3).abs() < 1e-6);

// Panics (rare — production code avoids unwrap, so prefer Result asserts)
let res = std::panic::catch_unwind(|| risky());
assert!(res.is_err());
```

> Note: `unwrap()` is banned in production code, but `.expect("…")`/`.unwrap()` is fine in tests where a failure should fail the test loudly. Always give `.expect()` a message.

## Pure-State Tests with `AppState::test_new()`

zynk separates state from runtime: `AppState` / `PaneState` are pure, testable data with **no PTYs and no async**, while `PaneRuntime` holds the live terminal. Workspace and view logic can be tested without real terminals.

```rust
#[test]
fn compute_view_clamps_sidebar_width_to_configured_max() {
    // Arrange: a pure AppState, no channels, no PTYs
    let mut app = AppState::test_new();
    let area = Rect::new(0, 0, 200, 50);

    // Act: compute_view does geometry + mutations; render is pure and only draws
    compute_view(&mut app, area);

    // Assert on the resulting state — no terminal needed
    assert!(app.sidebar_width() <= app.config().max_sidebar_width());
}
```

Rules this leans on:
- `compute_view()` mutates state (geometry + clamps); `render(&AppState, …)` only draws. **Test the mutation through `compute_view()`; never assert on a render that mutated state** (that would be a bug).
- Because `AppState::test_new()` has no channels/PTYs, these tests are fast and deterministic — favor them over spinning a real runtime.

## Async Tests (`#[tokio::test]`)

Use `#[tokio::test]` for anything touching tokio channels, the PTY actor, IPC, or the server. Drive behavior through channels and assert on what comes out.

```rust
#[tokio::test]
async fn async_user_input_waits_for_queue_capacity() {
    // Arrange: a bounded channel to simulate a full queue
    let (data_tx, mut data_rx) = mpsc::channel(1);
    data_tx
        .try_send(PtyIoDataCommand::WriteUserInput(Bytes::from_static(b"fill")))
        .expect("fill data queue");

    // Act: spawn the write that should block until capacity frees up
    let write = tokio::spawn(async move { /* ... */ });

    // Assert: draining the queue lets the blocked write proceed
    let _ = data_rx.recv().await;
    write.await.expect("write task completes once capacity frees");
}
```

Async test discipline:
- Always `.await` the spawned tasks/futures you assert on — an un-awaited future is a silently swallowed failure.
- Prefer bounded channels in tests to make backpressure observable.
- Wrap awaits that could hang (socket/process) in a `tokio::time::timeout` so a regression fails fast instead of hanging the suite.

## Test Doubles at the Boundary

Mock at the edges of the system, not its internals. zynk provides purpose-built doubles for the terminal and embedding boundaries.

```
Substitute these:                 Don't substitute these:
├── PaneRuntimeIo::TestChannel    ├── AppState / PaneState logic
│   (in place of a real PTY)      ├── compute_view() geometry
├── FakeEmbedder                  ├── protocol-ID field construction
│   (in place of the real model)  ├── wire (de)serialization
├── temp config dir / socket      ├── retrieval ranking (FTS bm25 / RRF)
└── temp SQLite home              └── pure data transformations
```

### `TestChannel` panes (no real PTY)

`PaneRuntimeIo::TestChannel` (a `#[cfg(test)]` variant) backs a pane with an mpsc/watch channel pair instead of a spawned PTY, so pane I/O and resize can be driven deterministically without a real terminal.

```rust
// A pane runtime wired to channels: feed bytes in, observe resize out,
// without spawning a real process/PTY.
let (sender, _rx) = mpsc::channel(8);
let (resize_tx, _resize_rx) = watch::channel((80, 24, 0, 0));
let io = PaneRuntimeIo::TestChannel { sender, resize_tx };
```

### `FakeEmbedder` (no real model)

`FakeEmbedder` produces deterministic vectors so retrieval/embedding tests don't load the real model. It has `with_dim(dim)` for a fixed dimension and `failing_then_ok(n)` to exercise retry/back-off paths.

```rust
use crate::zynk::embed::FakeEmbedder;

let mut embedder = FakeEmbedder::with_dim(8);          // deterministic dim-8 vectors
let mut flaky = FakeEmbedder::failing_then_ok(1);      // fails once, then succeeds
```

## Hermetic Config / Socket / DB

Every test must be isolated — no shared global config, socket, or database. The suite is hermetic by design.

- **Isolate config via a temp `XDG_CONFIG_HOME`** so a test never reads/writes the operator's real config. Tests that touch session/config globals take the shared `config` env lock and reset session state first:

```rust
let _guard = crate::config::test_config_env_lock().lock().unwrap();
crate::session::clear_explicit_session_for_test(); // BEFORE setting env, else later
                                                    // accept()/session lookups can hang
std::env::set_var("XDG_CONFIG_HOME", &temp_dir);
// ... test body ...
std::env::remove_var("XDG_CONFIG_HOME");
```

- **Isolate the socket** — each server-spawning test binds its own temp socket path under a temp dir; never the live socket.
- **Isolate the DB home** — point the conversation DB at a temp `ZYNK_HOME` (not the live `~/.zynk/zynk.db`). A test that migrates the live DB will break the installed binary with a migration mismatch; always redirect the DB home.
- **Run dev tests under an isolated `CARGO_TARGET_DIR`** (the machine may run a watcher on the default target), e.g. `CARGO_TARGET_DIR=/tmp/zynk-test-target cargo nextest run --locked`.

## Characterization & Parity Tests

A large class of zynk tests pin down exact wire/persistence behavior so a refactor can't silently change it. These are the highest-value tests in the conversation/protocol layer — protect these invariants:

- **Wire IDs / protocol-ID fields** — characterize the exact persisted `protocol_json` shape: which fields appear, which are omitted when `None`, and the schema version. E.g. `protocol_id_fields_omit_type_when_none` asserts the optional `type` field is absent unless set. A change here is a wire/format break and must be intentional.
- **Delivery transitions** — assert the `delivery_status` derived from the latest `delivery_events` row resolves correctly across the real `DeliveryEventType` set (`drafted`/`submitted`/`received`/`failed`). Pin the "latest event wins" ordering (`ORDER BY seq DESC LIMIT 1`) and that only `submitted → received` auto-advances.
- **FTS body purity** — the FTS path reads body/FTS only and ranks by `bm25(...)` ASC. Tests assert that retrieval reads the message body unchanged (no injection of header/protocol noise into the searchable body) and that a malformed query is classified as a *query* error, not an infra failure.
- **Retrieval ranking** — characterize FTS bm25 ordering and RRF fusion against known fixtures (e.g. `both_list_doc_outranks_single_mode`) so a ranking change is a deliberate, reviewed delta.

Pattern for a characterization test:

```rust
#[test]
fn protocol_id_fields_omit_type_when_none() {
    // Pin the exact serialized shape — this is a wire contract.
    let v = protocol_id_fields("m", "c", 1, "rt", "s", "h", None);
    assert!(v.get("type").is_none());

    let v2 = protocol_id_fields("m", "c", 1, "rt", "s", "h", Some("approve"));
    assert_eq!(v2["type"], "approve");
}
```

If one of these fails after a change, decide deliberately: either the change is a real (reviewed) format/behavior break — update the expectation and call it out — or it's an unintended regression to fix.

## Test Anti-Patterns

| Anti-Pattern | Problem | Better Approach |
|---|---|---|
| Testing implementation details | Breaks on refactor | Test observable inputs/outputs and pinned contracts |
| Spinning a real PTY when channels suffice | Slow, flaky, env-dependent | Use `PaneRuntimeIo::TestChannel` / `AppState::test_new()` |
| Loading the real embedding model in tests | Slow, non-deterministic | Use `FakeEmbedder` (`with_dim` / `failing_then_ok`) |
| Touching the live config/socket/DB | Pollutes the operator's runtime, breaks the installed binary | Temp `XDG_CONFIG_HOME` / temp socket / temp `ZYNK_HOME` |
| Setting env before clearing session state | `accept()`/session lookup can hang for minutes | `clear_explicit_session_for_test()` then set env, under the config lock |
| Un-awaited async work | Swallowed errors, false pass | Always `.await` the future you assert on |
| No timeout on a socket/process await | A regression hangs the whole suite | Wrap in `tokio::time::timeout` |
| Asserting on a render that mutated state | Hides a render-purity bug | Drive mutations through `compute_view()`; `render` is read-only |
| Overly broad assertions on wire output | Misses field-level regressions | Characterize exact fields (presence/omission/value) |
| Skipping a failing characterization test to pass CI | Hides a real wire/format break | Fix it, or update the expectation deliberately and call it out |
| Shared mutable global state across tests | Tests pollute each other | Per-test setup/teardown; hold the relevant env lock |
