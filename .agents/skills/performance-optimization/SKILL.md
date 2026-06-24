---
name: performance-optimization
description: Optimizes runtime performance. Use when performance requirements exist, when you suspect performance regressions, or when render latency, IPC/PTY throughput, allocation, or async overhead needs improvement. Use when profiling reveals bottlenecks that need fixing.
---

# Performance Optimization

## Overview

Measure before optimizing. Performance work without measurement is guessing — and guessing leads to premature optimization that adds complexity without improving what matters. Profile first, identify the actual bottleneck, fix it, measure again. Optimize only what measurements prove matters.

In zynk the things that matter are the render hot path (a slow `compute_view`/`render` shows up as visible input lag), IPC and PTY throughput (frames per second the server can push to clients), allocations in per-frame and per-byte loops, and async task overhead (tokio task churn, lock contention, blocked executor threads).

## When to Use

- Performance requirements exist in the spec (frame-latency budgets, throughput SLAs)
- Users or monitoring report sluggish input, slow scroll, or laggy rendering
- Render/IPC/PTY timings are above their budget
- You suspect a change introduced a regression
- Building features that handle large output volumes, many panes, or high message throughput

**When NOT to use:** Don't optimize before you have evidence of a problem. Premature optimization adds complexity that costs more than the performance it gains.

## Runtime Performance Targets

These are the runtime analogues of frontend vitals — the numbers a user actually feels:

| Metric | Good | Needs Improvement | Poor |
|--------|------|-------------------|------|
| **Input-to-render latency** (keystroke → visible change) | ≤ 16ms | ≤ 50ms | > 50ms |
| **Frame compute** (`compute_view` + `render`) | ≤ 4ms | ≤ 10ms | > 10ms |
| **IPC round-trip** (CLI command → JSON response) | ≤ 20ms | ≤ 100ms | > 100ms |
| **PTY drain throughput** (bytes parsed/sec under burst) | sustains burst | falls behind briefly | persistent backlog |

Treat these as starting budgets — adjust to the spec, but always pin a number before optimizing.

## The Optimization Workflow

```
1. MEASURE  → Establish baseline with real data
2. IDENTIFY → Find the actual bottleneck (not assumed)
3. FIX      → Address the specific bottleneck
4. VERIFY   → Measure again, confirm improvement
5. GUARD    → Add monitoring or tests to prevent regression
```

### Step 1: Measure

Two complementary approaches — use both:

- **Synthetic (criterion benchmarks, targeted microbench, the built-in render profiler):** Controlled conditions, reproducible. Best for CI regression detection and isolating a specific function.
- **Real-run profiling (`perf`, `samply`/`pprof`, `tracing` spans, `tokio-console`):** Real workloads in real conditions. Required to validate that a fix actually improved the felt experience under a real PTY/agent load.

**Render / hot path:**
```bash
# Synthetic: criterion microbenchmark of compute_view/render
cargo bench --bench render

# Real-run: sampling profiler over a live session under load
samply record -- ./target/release/zynk   # or `perf record` / a flamegraph

# In-process timing spans (zynk has a render profiler module — prefer it
# over ad-hoc timing so spans are consistent and toggleable).
let _span = tracing::info_span!("render").entered();
```

**IPC / PTY / async:**
```bash
# tokio task + poll instrumentation
RUSTFLAGS="--cfg tokio_unstable" cargo run --release  # then attach tokio-console

# Coarse timing around a suspect span
let t = std::time::Instant::now();
let result = drain_pty(&mut parser, &bytes);
tracing::debug!(elapsed_ms = t.elapsed().as_secs_f64() * 1000.0, "pty drain");
```

### Where to Start Measuring

Use the symptom to decide what to measure first:

```
What is slow?
├── Input feels laggy (keystroke → visible change)
│   ├── Slow per-frame compute? --> Profile compute_view/render; look for work done every frame
│   ├── Re-rendering everything? --> Check for full redraws where a diff/dirty region would do
│   └── Blocking on the executor? --> Look for sync I/O or a long task on the UI/server loop
├── Output / scrollback feels slow
│   ├── Large PTY burst? --> Profile the terminal parser; check per-byte allocations
│   ├── Big scrollback? --> Check viewport copies, unbounded ring growth
│   └── Many panes? --> Profile per-pane work that should be only for visible/active panes
├── A CLI command is slow (IPC)
│   ├── Single method slow? --> Profile its handler + DB queries; check indexes
│   ├── All methods slow? --> Check server-loop contention, lock hold times, frame backlog
│   └── Intermittent? --> Look for lock contention, await points, blocked tokio workers
└── Conversation / retrieval is slow
    ├── Query slow? --> Check SQLite query plan (EXPLAIN QUERY PLAN), missing indexes
    ├── Embedding/vector search slow? --> Profile the embedding worker; batch, don't N+1
    └── Growing over time? --> Check unbounded caches, leaked Arc/handles, ring buffers
```

### Step 2: Identify the Bottleneck

Common bottlenecks by category:

**Render / hot path:**

| Symptom | Likely Cause | Investigation |
|---------|-------------|---------------|
| High input-to-render latency | Heavy work done every frame, full redraws | Profile `compute_view`/`render`; check what runs unconditionally |
| Visible scroll/redraw jank | Re-rendering unchanged regions, recomputing layout | Check for missing dirty-region / diff logic |
| Frame compute over budget | Per-frame allocation, formatting, cloning | Flamegraph the render span; look for allocs in the loop |

**IPC / PTY / async:**

| Symptom | Likely Cause | Investigation |
|---------|-------------|---------------|
| Slow IPC responses | N+1 DB queries, missing indexes, large serialization | Inspect SQLite query log + `EXPLAIN QUERY PLAN` |
| PTY backlog under burst | Per-byte allocation, parser copies, small reads | Profile the parser; check buffer reuse and read sizes |
| Memory growth | Leaked `Arc`/handles, unbounded scrollback/caches | Heap profiling (`dhat`, `valgrind --tool=massif`) |
| CPU spikes / stalls | Sync work on the executor, lock contention, GC-like churn | `tokio-console`, `perf`; look for blocked workers / long poll times |

### Step 3: Fix Common Anti-Patterns

#### N+1 Queries (Conversation layer)

```rust
// BAD: N+1 — one query per message to fetch its participant.
let messages = sqlx::query_as::<_, Message>("SELECT * FROM messages WHERE conversation_id = ?")
    .bind(&conv_id).fetch_all(&pool).await?;
for m in &mut messages {
    m.participant = sqlx::query_as("SELECT * FROM conversation_participants WHERE id = ?")
        .bind(&m.participant_id).fetch_one(&pool).await?; // one round-trip each
}

// GOOD: Single query with a join.
let messages = sqlx::query_as::<_, MessageWithParticipant>(
    "SELECT m.*, p.agent_label FROM messages m \
     JOIN conversation_participants p ON p.id = m.participant_id \
     WHERE m.conversation_id = ?",
).bind(&conv_id).fetch_all(&pool).await?;
```

#### Unbounded Data Fetching

```rust
// BAD: Fetching every row.
let all = sqlx::query_as::<_, Message>("SELECT * FROM messages WHERE conversation_id = ?")
    .bind(&conv_id).fetch_all(&pool).await?;

// GOOD: Bounded + cursor-paginated, newest first (uses an index on (conversation_id, seq)).
let page = sqlx::query_as::<_, Message>(
    "SELECT * FROM messages WHERE conversation_id = ? AND seq < ? \
     ORDER BY seq DESC LIMIT ?",
).bind(&conv_id).bind(before_seq).bind(50).fetch_all(&pool).await?;
```

#### Allocation in the Hot Path

```rust
// BAD: Allocates a fresh buffer and String for every PTY chunk, every frame.
fn drain(parser: &mut Parser, chunk: &[u8]) {
    let mut buf = Vec::new();          // new allocation each call
    for &b in chunk { buf.push(b); }
    let s = String::from_utf8_lossy(&buf).to_string(); // extra copy
    parser.feed(&s);
}

// GOOD: Reuse a scratch buffer owned by the caller; feed bytes directly.
fn drain(parser: &mut Parser, chunk: &[u8], scratch: &mut Vec<u8>) {
    scratch.clear();                   // reuse capacity, no new alloc
    scratch.extend_from_slice(chunk);
    parser.feed_bytes(scratch);        // avoid the UTF-8 round-trip when possible
}
```

#### Redundant Per-Frame Work (Render)

```rust
// BAD: Recomputing layout/strings on every frame, even when nothing changed.
fn render(state: &AppState, f: &mut Frame) {
    let title = format!("{} — {} panes", state.workspace_name, state.panes.len()); // every frame
    let layout = compute_full_layout(state); // every frame
    draw(f, &layout, &title);
}

// GOOD: Compute once, cache, invalidate on change. compute_view() does the
// geometry + mutations; render() only draws from &AppState (never mutates).
fn render(state: &AppState, cache: &ViewCache, f: &mut Frame) {
    draw(f, cache.layout(), cache.title()); // cache rebuilt only when state changed
}
```

Keep the discipline: **render is pure** — `render()` takes `&AppState` and only draws; never mutate state during render. Per-frame mutation is both a correctness hazard and a performance one.

#### Blocking the Async Executor

```rust
// BAD: Synchronous/blocking work on a tokio worker stalls every task on it.
async fn handle(req: Request) -> Response {
    let data = std::fs::read("big-file")?;        // blocking syscall on the executor
    let parsed = expensive_cpu_parse(&data);      // long CPU burst on the executor
    Response::ok(parsed)
}

// GOOD: Offload blocking/CPU work so the executor keeps polling other tasks.
async fn handle(req: Request) -> Response {
    let data = tokio::fs::read("big-file").await?;            // async I/O
    let parsed = tokio::task::spawn_blocking(move || expensive_cpu_parse(&data)).await?;
    Response::ok(parsed)
}
```

#### Lock Contention

```rust
// BAD: Holding a Mutex across an await / heavy work serializes every task.
let mut guard = state.lock().await;
let result = expensive_query(&guard).await; // lock held across await — contention
guard.update(result);

// GOOD: Hold the lock only for the minimal critical section.
let snapshot = { let g = state.lock().await; g.snapshot() }; // lock dropped here
let result = expensive_query(&snapshot).await;
{ let mut g = state.lock().await; g.update(result); }
```

#### Caching Read-Mostly Data

```rust
// Cache frequently-read, rarely-changed data (e.g. resolved config) behind a guard.
struct ConfigCache {
    value: Option<AppConfig>,
    loaded_at: Instant,
}
const CACHE_TTL: Duration = Duration::from_secs(300);

fn get_config(cache: &mut ConfigCache, load: impl Fn() -> AppConfig) -> AppConfig {
    if let Some(v) = &cache.value {
        if cache.loaded_at.elapsed() < CACHE_TTL {
            return v.clone();
        }
    }
    let fresh = load();
    cache.value = Some(fresh.clone());
    cache.loaded_at = Instant::now();
    fresh
}
```

## Performance Budget

Set budgets and enforce them:

```
Frame compute (compute_view + render): < 4ms typical, < 10ms p99
Input-to-render latency:               < 16ms (one frame at 60Hz)
IPC method p95:                        < 100ms
PTY drain:                             sustains a burst without persistent backlog
Per-frame heap allocations in render:  ~0 (reuse buffers)
Conversation query p95:                < 50ms (indexed)
```

**Enforce in CI:**
```bash
# Criterion benchmarks with a regression threshold (fail the build if a hot path slows).
cargo bench --bench render -- --save-baseline main
# Compare a PR run against the saved baseline and gate on regression.

# Keep the full check green — perf changes must not break behavior.
just check
```

## Common Rationalizations

| Rationalization | Reality |
|---|---|
| "We'll optimize later" | Performance debt compounds. Fix obvious anti-patterns (N+1, per-frame allocs) now, defer micro-optimizations. |
| "It's fast on my machine" | Your machine isn't every user's terminal. Profile under a real PTY/agent load, not an idle session. |
| "This optimization is obvious" | If you didn't measure, you don't know. Profile first — the bottleneck is rarely where you expect. |
| "Users won't notice a few extra ms per frame" | A frame over budget is visible input lag. Per-frame cost is paid 60× a second. |
| "The async runtime handles performance" | tokio schedules tasks; it can't fix blocking calls on the executor, lock contention, or N+1 queries. |

## Red Flags

- Optimization without profiling data to justify it
- N+1 query patterns in the conversation/retrieval layer
- Query/list methods without pagination or limits
- Allocations or `format!`/`clone` inside the per-frame render or per-byte parser loop
- Full redraws where a dirty-region/diff would do
- Blocking I/O or long CPU bursts on a tokio worker
- A `Mutex`/`RwLock` held across an `.await`
- Mutating `AppState` during `render()`
- No render/IPC timing or benchmark guarding the hot paths

## Verification

After any performance-related change:

- [ ] Before and after measurements exist (specific numbers, same workload)
- [ ] The specific bottleneck is identified and addressed
- [ ] Hot-path metrics (frame compute, input-to-render, IPC p95) are within "Good" thresholds
- [ ] No new per-frame allocations or per-byte copies introduced
- [ ] No N+1 queries in new conversation/DB code; queries are indexed
- [ ] No blocking work or cross-`await` locks added to the async path
- [ ] Benchmark/regression guard passes in CI (if configured)
- [ ] `just check` is clean — the optimization didn't break behavior
