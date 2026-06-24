# Performance Checklist

Quick reference checklist for zynk's runtime performance — a ratatui TUI driven by a tokio async core over Unix-socket IPC, with PTY-backed panes and a SQLite conversation layer. Use alongside the `performance-optimization` skill.

## Table of Contents

- [Latency Targets](#latency-targets)
- [Slow-Frame Diagnosis](#slow-frame-diagnosis)
- [Render Hot Path Checklist](#render-hot-path-checklist)
- [Async / Runtime Checklist](#async--runtime-checklist)
- [IPC / PTY Throughput Checklist](#ipc--pty-throughput-checklist)
- [SQLite Checklist](#sqlite-checklist)
- [Measurement Commands](#measurement-commands)
- [Common Anti-Patterns](#common-anti-patterns)

## Latency Targets

These are operator-perceptible budgets for an interactive terminal workspace. Keep input responsive even when many panes are streaming output.

| Metric | Good | Needs Work | Poor |
|--------|------|------------|------|
| Input-to-render latency (keypress → frame on screen) | ≤ 16ms | ≤ 50ms | > 50ms |
| Full-frame compute + render (`compute_view` + `render`) | ≤ 8ms | ≤ 16ms | > 16ms |
| `zynk send`/IPC command round-trip (local socket) | ≤ 20ms | ≤ 100ms | > 100ms |
| SQLite read on the interactive path (FTS/vector retrieval) | ≤ 25ms | ≤ 100ms | > 100ms |

A dropped frame budget is ~16ms at 60fps; treat anything that blocks the UI thread longer than one frame as a regression to chase.

## Slow-Frame Diagnosis

When the UI feels laggy under load (panes streaming, large scrollback), isolate which stage is over budget:

- [ ] **Input not reaching render** → the event loop is blocked. Check for a long synchronous step between reading the input event and the next `compute_view()` / `render()` pass.
- [ ] **`compute_view()` slow** → geometry + state mutation is heavy. Look for per-frame allocation, re-clamping every pane, or recomputing layout that didn't change.
- [ ] **`render()` slow** → drawing is heavy. `render()` takes `&AppState` and only draws — confirm it isn't accidentally doing work that belongs in `compute_view()`.
- [ ] **Frame stalls but CPU is idle** → a tokio task is awaiting (lock contention, `SQLITE_BUSY` back-off, a blocking call on the executor). See the Async checklist.

## Render Hot Path Checklist

The render path is `compute_view()` (geometry + mutations) → `render(&AppState, …)` (pure draw). Per the architecture rules: **never mutate state during render.**

- [ ] `render()` performs zero state mutation — it only reads `&AppState` and draws (mutating during render is a correctness *and* perf bug)
- [ ] No per-frame heap allocation in the hot path — reuse buffers; avoid building throwaway `String`/`Vec` every frame
- [ ] No per-frame re-layout of panes that didn't change geometry (use `compute_view_without_resizing_panes` style paths where a resize isn't needed)
- [ ] Scrollback / large screen state is not cloned per frame — render reads the existing screen snapshot
- [ ] Diff-based frame output where the protocol supports it (only changed cells go over the wire to the client) rather than redrawing the full grid every frame
- [ ] Expensive detection (`src/detect/`) is not run on every frame — it reads a screen snapshot on a cadence, never the per-frame render path
- [ ] No layout work duplicated between `compute_view()` and `render()` — geometry is computed once in `compute_view()`
- [ ] Wide / multi-pane workspaces don't redraw idle panes every tick when only one pane produced output

## Async / Runtime Checklist

zynk's core is tokio. The cardinal sins here are blocking the executor and holding a lock across an `.await`.

- [ ] No lock held across an `.await` point — acquire, mutate, drop the guard *before* awaiting (a `MutexGuard` alive across `.await` serializes the runtime and can deadlock)
- [ ] No blocking syscalls on async worker threads — file/PTY/`std::process::Command` blocking work goes through `spawn_blocking` or a dedicated thread, never inline on the executor
- [ ] No unbounded channels on hot producer paths — bounded channels with explicit backpressure; a saturated queue means "not enqueued," not "block forever"
- [ ] Per-client write tasks don't starve each other — one slow/blocked client must not stall the server's render-broadcast loop
- [ ] CPU-heavy work (embedding, large parse) is off the interactive path — pushed to a worker (e.g. the embedding worker) so retrieval/render stay responsive
- [ ] Timeouts wrap any await that talks to an external process or socket so a hung peer can't wedge a task indefinitely
- [ ] No busy-poll loops without a yield/sleep — they pin a core and steal time from real work

## IPC / PTY Throughput Checklist

The CLI is a thin client over a local Unix socket; PTYs feed pane terminal state. Throughput here gates how many panes can stream at once.

- [ ] Frames are length-prefixed and bounded — oversized length prefixes are rejected up front (`MAX_FRAME_SIZE`) so a bad/huge frame can't trigger a giant allocation or DoS
- [ ] PTY reads use a reasonably sized buffer — not byte-at-a-time; drain available output per wake, not per byte
- [ ] High-volume pane output is coalesced before it hits the terminal emulator/screen state rather than processed in tiny chunks
- [ ] Server→client frame broadcast doesn't serialize the same frame N times when it can serialize once and fan out
- [ ] No synchronous flush-per-write on the socket — batch writes where the protocol allows
- [ ] Backpressure on a slow client doesn't block PTY draining for other panes (decouple PTY read from client write)
- [ ] Wire (de)serialization avoids redundant copies of the cell grid on every frame

## SQLite Checklist

The conversation layer (`src/zynk/`) is SQLite via `sqlx`, with FTS5 + vector retrieval. It runs in WAL mode and fails closed on a foreign DB.

- [ ] No N+1 query pattern — fetch related rows (e.g. latest `delivery_events` per message) with a join/subquery, not a query per row in a loop
- [ ] Queries that filter/sort on a column have a supporting index (and FTS/vector lookups use their virtual-table indexes)
- [ ] List/history reads are bounded with `LIMIT` — never an unbounded `SELECT` over the full message history on the interactive path
- [ ] Retrieval (FTS `bm25()` ranking, vector KNN) stays off the per-frame render path — it runs on demand, not every tick
- [ ] WAL mode keeps readers from blocking the writer; `SQLITE_BUSY` is handled with bounded retry/back-off, not an unbounded spin
- [ ] Writes (delivery events, messages) are batched into a transaction where a burst would otherwise be one commit per row
- [ ] Prepared/parameterized queries are reused rather than rebuilt as strings per call (parameterization is also the security rule — see the security checklist)

## Measurement Commands

### Frame / render profiling

zynk ships a render profiler (`src/render_prof.rs`). Prefer real measurement over guessing which stage is slow.

1. **Measure first** — enable the render profiler and capture per-stage timings for `compute_view()` vs `render()` under a realistic multi-pane load before changing anything.
2. **Reproduce under load** — many panes streaming output is where frame budget blows; profile that, not an idle single pane.
3. **Separate compute from draw** — attribute the cost to geometry/mutation (`compute_view`) or drawing (`render`) so you optimize the right stage.

```bash
# Run the suite under an isolated target dir (never the live runtime/socket)
CARGO_TARGET_DIR=/tmp/zynk-perf-target cargo nextest run --locked render

# Build release with native opts for realistic timing
cargo build --release --locked

# Profile a release binary with perf (Linux): record then report hot symbols
perf record -g -- ./target/release/zynk <args>
perf report --stdio | head -50

# Flamegraph of the hot path (if cargo-flamegraph is installed)
cargo flamegraph --release -- <args>

# Microbenchmarks for a hot function (if a bench harness is present)
cargo bench --locked
```

## Common Anti-Patterns

| Anti-Pattern | Impact | Fix |
|---|---|---|
| Lock held across `.await` | Runtime serialization, deadlocks | Drop the guard before awaiting; minimize critical section |
| Blocking call on the tokio executor | Stalls every task on that worker | `spawn_blocking` or a dedicated thread for PTY/process/file work |
| Mutating state in `render()` | Correctness bug + redundant per-frame work | Move all mutation into `compute_view()`; `render()` is read-only |
| Per-frame allocation in the hot path | GC-like churn, dropped frames | Reuse buffers; build strings/vecs once, not per frame |
| Re-layout of unchanged panes every frame | Wasted CPU under multi-pane load | Skip resize when geometry is unchanged |
| N+1 SQLite queries | Linear DB load growth on retrieval | Join/subquery; fetch related rows in one query |
| Unbounded query over full history | Memory/time blowup as messages accumulate | Always `LIMIT`; paginate history |
| Missing index on a filtered column | Slow reads as the DB grows | Add an index for filtered/sorted columns |
| Unbounded channel on a hot path | Memory growth, no backpressure | Bounded channel; treat full queue as "not enqueued" |
| Byte-at-a-time PTY/socket I/O | Syscall-bound throughput ceiling | Buffered reads/writes; coalesce output |
| Full-grid frame redraw every tick | IPC + render waste | Diff cells; redraw only what changed |
