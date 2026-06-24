# ADR 0006 — sqlite-vec loading mechanism + offline embedding runtime (M5b B0)

**Status:** Proposed (M5b B0 spike outcome; operator-directed; gate: Arbiter B0/ADR review → operator). Builds on ADR 0003 (persistence/retrieval) + the M5 plan §4/§12 Task B0.
**Date:** 2026-06-14 · **Spec:** `docs/zynk/SPEC.md` §F3 (lines 76–94, 195–200, 230–231).
**Spike evidence:** `agent-collab-protocol/.../zynk-takeover-current/claude-m5b-b0-spike-report.md` (all proofs run in `/tmp` scratch against the exact `sqlx 0.8.6` + bundled `libsqlite3-sys 0.30.1` stack; repo untouched).

## Context

M5b/M5c (F3 hybrid retrieval) need a vector index. The M5 plan (§4.1) named **sqlite-vec** (`vec0`, brute-force)
loaded into the SAME SQLite the persistence layer uses, but flagged the **loading mechanism** as UNPROVEN in
our exact stack (sqlx exposes only dynamic `extension()`/`extension_with_entrypoint`, not `sqlite3_auto_extension`;
sqlx #3147 / sqlite-vec #198 open) and required a **throwaway spike (Task B0)** to decide it, plus the
**offline embedding** discipline, BEFORE any dep enters `Cargo.toml`. This ADR records that decision. **No
implementation, no committed deps, no migration is added by B0** — the only repo artifact is this ADR.

The B0 spike tried all three candidate loading paths and the fastembed/ORT offline boundary. **All three load
paths PASSED** the full criterion (load vec0 → `CREATE VIRTUAL TABLE … USING vec0` → insert a vector → KNN
returns it, across separate **freshly-opened per-op connections** for both the worker-write and the query-read
roles, on a temp **file** DB). The decision below selects among passing options on cleanliness/risk grounds.

## Decision

### D1 — Loading mechanism: STATIC `sqlite3_auto_extension` (compiled-in sqlite-vec)

Load vec0 by **compiling sqlite-vec into the binary** (the `sqlite-vec` crate vendors `sqlite-vec.c`, built
with `-DSQLITE_CORE`) and registering it ONCE at process start, before any sqlx connection is opened:

```rust
// once, at DB-subsystem init, BEFORE any sqlx connection opens:
unsafe {
    libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute(
        sqlite_vec::sqlite3_vec_init as *const (),
    )));
}
```

This works **only because** Cargo unifies `sqlx-sqlite 0.8.6` and our crate to **exactly ONE** `libsqlite3-sys
0.30.1` node (the `bundled` feature feature-unifies onto the single node sqlx links). The spike verified this:
`grep -c 'name = "libsqlite3-sys"' Cargo.lock` ⇒ **1** (v0.30.1); `cargo tree -i libsqlite3-sys` shows a single
shared node; and a negative control with the registration removed FAILS (`no such function: vec_version` /
`no such module: vec0`). Re-verified independently before writing this ADR.

**Why static (over the two other PASSING paths):**
- **vs. dynamic `vec0.so` via sqlx `.extension_with_entrypoint` (Path 1, PASSED):** rejected for a single-binary
  fork because it ships an **external `vec0.so`** that must be located at an absolute runtime path and `dlopen`'d
  (an unsigned native lib → placement + supply-chain/trust surface), AND it **mandates per-connection
  re-registration** on EVERY fresh `SqliteConnection` (sqlx disables the extension loader after each load — the
  spike's negative control proved a fresh conn without `.extension_with_entrypoint` fails). More moving parts,
  weaker single-binary cleanliness. **Kept as the documented fallback** if the single-libsqlite3 unification (D-Risk-1)
  ever breaks.
- **vs. `rusqlite` read-side (Path 3, PASSED):** rejected because it adds a **second SQLite binding crate**, still
  loads the `.so` per connection, AND introduces a **hard lockstep version-pin** — `rusqlite 0.32.x` is bolted to
  sqlx's `libsqlite3-sys ^0.30`; a future sqlx bump to `libsqlite3-sys 0.31+` would force a matching rusqlite bump
  or Cargo compiles two `libsqlite3-sys` copies and the `links = "sqlite3"` native key COLLIDES (build failure).

### D2 — Applied IDENTICALLY to worker-write and query-read connections

With static auto-extension, vec0 is registered **once, process-globally**, on the single bundled libsqlite3
BEFORE any connection opens. Therefore **every subsequently-opened fresh connection** — the short-lived
worker-write connection AND the short-lived query-read connection — has vec0 visible with **zero per-connection
setup and no `.extension()` call**, symmetrically. The spike proved exactly this: Role 1 (fresh conn #1,
worker-write) ran `CREATE VIRTUAL TABLE`/`INSERT` then closed, with NO `.extension()`; Role 2 (fresh, independent
conn #2, query-read, no shared pool) ran the KNN `MATCH … k=1` and got the row, also with NO `.extension()`.
(It also works transparently with a `SqlitePool` if one is ever used, but the proof did not rely on a pool.)
This removes the per-conn-boilerplate asymmetry the dynamic/rusqlite paths would have forced on both roles.

### D3 — vec0 table creation is LAZY, OUT of migrations

`CREATE VIRTUAL TABLE … USING vec0(…)` MUST NOT live in a sqlx migration: vec0 is a runtime-registered
virtual-table module, so a migration creating a vec0 table would fail on any connection where the extension was
not yet registered, and migrations must not depend on extension-load ordering. Instead the **embedding worker**
creates it lazily at runtime on its (auto-extension-loaded) connection — `CREATE VIRTUAL TABLE IF NOT EXISTS
message_vec_<model> USING vec0(message_rowid INTEGER PRIMARY KEY, embedding float[N] distance_metric=cosine)` —
once the active model's dim is known. Migration `0002` (M5b) stays extension-free (only the plain
`embedding_models`/`embedding_jobs`/`message_embeddings` tables). The spike confirmed the lazy-on-loaded-conn
pattern (`CREATE VIRTUAL TABLE IF NOT EXISTS … USING vec0` succeeds after registration on a fresh conn).

### D4 — New crates + C symbols (introduced later by B1+, NOT by B0)

- **crate `sqlite-vec` 0.1.9** (crates.io) — vendors `sqlite-vec.c` (`SQLITE_VEC_VERSION v0.1.9`, source commit
  `e9f598a…`); its `build.rs` compiles it with `cc -DSQLITE_CORE` into the static `sqlite_vec0` lib. **Pin the
  exact version**; re-verify the init symbol on any bump.
- **crate `libsqlite3-sys` 0.30.1** promoted to a **direct** dependency with the `bundled` feature (already pulled
  transitively by sqlx-sqlite; must stay feature-unified to the SINGLE node — verified count = 1).
- **C symbol `sqlite3_vec_init`** — the one FFI init symbol exported by the compiled `sqlite-vec.c`, transmuted
  and passed to `sqlite3_auto_extension`.
- **C symbol `sqlite3_auto_extension`** — called via the `libsqlite3-sys` FFI to register vec0 process-globally
  (must be the SAME `libsqlite3-sys` sqlx-sqlite links).
- (vec0 also links `sqlite3_create_module_v2` + the vec0 module into the binary; `ldd` shows no dynamic
  libsqlite3 — statically bundled.)

### D5 — Offline embedding runtime: PROVEN, gated by explicit provisioning

fastembed CAN be made **fully no-network at BOTH build and index/query** — PROVEN in the spike — but it is **NOT
no-network by default**; explicit provisioning is required at both layers, and that provisioning gate is a
**precondition for B1+ adding the dep**:
- **Build layer (`ort`/ONNX Runtime):** the default `download-binaries` fetches ONNX Runtime (`ms@1.24.2`, ~91 MB
  static `.a`) from `cdn.pyke.io` over HTTPS into `~/.cache/ort.pyke.io/dfbin/…`. A no-network build is
  ACHIEVABLE from a prewarmed cache, but **`CARGO_NET_OFFLINE=true` alone is NOT sufficient** (corrected per
  Arbiter R1 verification — `ort-sys 2.0.0-rc.12` source `build/main.rs:65-72` sets `cfg(link_error)` and RETURNS
  when `download::should_skip()` is true, BEFORE the cached `dfbin` linking at `:97-103`). The two PROVEN
  no-network build routes are:
  1. **Severed network, `CARGO_NET_OFFLINE` NOT set:** with the cache prewarmed, `unshare -rn cargo build --locked`
     ⇒ exit 0 — `should_skip()` is false, so `build.rs` reaches and links the existing cached `dfbin` lib without
     fetching (the download attempt simply can't reach the network and falls through to the cached lib).
  2. **`CARGO_NET_OFFLINE=true` + an explicit local ONNX Runtime source:** `CARGO_NET_OFFLINE=true
     ORT_LIB_PATH=<…/dfbin/x86_64-unknown-linux-gnu/<hash>> cargo build --locked` ⇒ exit 0 (or `ORT_LIB_LOCATION`,
     or `ort`'s `load-dynamic` feature + `ORT_DYLIB_PATH` at a local `libonnxruntime`).

  Verified exit codes (clean target dirs): `CARGO_NET_OFFLINE=true` + warm cache only ⇒ **exit 101**
  (`could not link onnxruntime … Neither ORT_LIB_PATH nor ORT_LIB_LOCATION were set`); route 1 (`unshare -rn`) ⇒
  exit 0; route 2 (`+ ORT_LIB_PATH`) ⇒ exit 0. (I independently re-confirmed the failing `CARGO_NET_OFFLINE`-only
  case ⇒ 101; the two passing routes are the Arbiter's command-level evidence, grounded in the `ort-sys` source
  above.) Net: a B1+ build pipeline MUST choose route 1 (severed-network CI step over a prewarmed cache) or route 2
  (`CARGO_NET_OFFLINE` + a pinned local ORT lib path) — never `CARGO_NET_OFFLINE` alone.
- **Index/query layer (model weights):** model weights download from HuggingFace Hub on first use
  (`multilingual-e5-small`: ~465 MB across 11 blobs). Offline PROVEN with the network SEVERED (`unshare -rn`):
  a pre-populated cache (`with_cache_dir`) or `try_new_from_path`/`try_new_from_user_defined` (vendored model
  bytes) yields embeddings with NO network at index/query time.
- **B1+ gate:** `fastembed`/`ort` enter `Cargo.toml` ONLY behind an opt-in feature/config, AND only after the
  build-cache + a model-provisioning step (`zynk embed-provision`-style or documented manual staging) are in
  place on the target box. Default model: `intfloat/multilingual-e5-small` (dim 384, CPU-light); `bge-m3`
  (dim 1024) opt-in. The model choice is recorded in `embedding_models` (no further ADR needed).

### D6 — FakeEmbedder is the test default + the no-network invariant

`FakeEmbedder` is the default embedder for tests/dev and the **enforced no-network invariant**. Verified
zero-dependency (std-only: `DefaultHasher` over (dim_index, text) → fill → L2-normalize to a unit vector;
deterministic, unit-norm, distinct outputs). **The default code path and the ENTIRE test suite MUST use
FakeEmbedder and MUST NOT touch the network** (no ORT download, no HF Hub fetch). The real fastembed/ORT path is
opt-in behind a feature/config and is only ever exercised against a warm cache or vendored bytes — never a live
download. This keeps `just test`/`just check` hermetic and offline by construction.

## Consequences / required guards

1. **Single-`libsqlite3-sys` invariant (HIGHEST RISK).** Static auto-extension works ONLY while Cargo unifies to
   exactly ONE `libsqlite3-sys 0.30.1`. A future merge/dep pulling a second `libsqlite3-sys` (a different sqlx
   minor, or any crate on a different version) would make the auto-extension registry on one invisible to the
   other ⇒ silent `no such module: vec0`. **B1+ MUST add a CI guard** asserting a single `libsqlite3-sys` version
   (e.g. a test that `cargo tree -i libsqlite3-sys` / the lockfile shows exactly one). Verified today: count = 1, 0.30.1.
2. **Registration ordering.** `sqlite3_auto_extension` MUST be called at process start BEFORE any sqlx connection
   opens; it is process-global. Encode as a single guaranteed init step (in the DB-init path), with a test.
3. **`transmute` soundness.** The static path transmutes `sqlite3_vec_init` (C sig
   `int(sqlite3*, char**, const sqlite3_api_routines*)`) to `sqlite3_auto_extension`'s `Option<unsafe extern "C" fn()>`
   — the upstream-documented pattern; works because sqlite-vec compiles with `-DSQLITE_CORE`. Pin the sqlite-vec
   version; re-verify if the symbol/API changes.
4. **C-build cost.** `bundled` compiles the SQLite amalgamation + `sqlite-vec.c` via cc/gcc (~16 s clean; gcc
   present 16.1.1) — no sudo/system install.
5. **Reverting** is dropping the two deps + the init call + the lazy vec0 create; M2/M3/M4/M5a data unaffected
   (vec0 tables are additive + runtime-created).

## Alternatives considered (all PASSED the spike; rejected per D1)
- **Dynamic `vec0.so` via sqlx `.extension_with_entrypoint`** — PASSED; rejected (runtime `.so` trust/placement +
  per-conn re-registration). **Retained as the fallback** if the single-libsqlite3 invariant ever breaks.
- **`rusqlite` read-side** — PASSED (incl. a combined sqlx-write + rusqlite-read project unifying to one
  `libsqlite3-sys`); rejected (second binding + lockstep version-pin + per-conn `.so`).
- **ANN (HNSW)** — out of scope (SPEC §231: brute-force vec0 in v1; HNSW deferred to >~1M messages).

## Status note
This ADR is **Proposed** pending the Arbiter B0/ADR gate + operator approval. B1+ (deps + implementation) does
NOT start until this ADR is accepted. Amend via a new ADR, never rewrite (hard rule §3).
