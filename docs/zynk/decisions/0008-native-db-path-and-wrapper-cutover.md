# ADR 0008 — Native DB path & wrapper→native cutover

**Status:** Proposed (gate: Arbiter review → operator). **Supersedes ADR 0003 Decision #1 (DB path only)**;
all other ADR 0003 decisions (schema, hybrid retrieval, async embedding, multi-runtime provenance) stand.
**Date:** 2026-06-15 · **Spec:** `docs/zynk/SPEC.md` §7 · **Milestone:** M6.

## Context

ADR 0003 chose `$ZYNK_HOME/zynk-v2/zynk.db` (default `~/.zynk/zynk-v2/zynk.db`) **specifically to coexist**
with the frozen wrapper-era `zynk` v1.5.1 DB at `~/.zynk/zynk.db`, which has a different schema and must
never be migrated/overwritten in place. ADR 0003 explicitly anticipated this milestone: *"a future M6
command may explicitly import from it… M6 may add an explicit import command but must not silently move
native data back."*

The operator's M6 direction: native Zynk is the product, not an eternal coexistence workaround. The final
native DB path should be the simple `~/.zynk/zynk.db` — **but that path is currently occupied by real
wrapper-era data** (`~/.zynk/zynk.db` exists today). So the final path cannot just be claimed; the
wrapper→native transition must be deterministic and safe, with no silent overwrite or in-place migration.
(Config lives separately at `~/.config/zynk/`, ADR 0007 §5 — `~/.zynk/` is the **data** home only.)

## Decision

1. **Final native default DB path = `$ZYNK_HOME/zynk.db`** (default `~/.zynk/zynk.db`). The `zynk-v2`
   subdirectory is removed as the product default. Resolution precedence (unchanged shape, minus the v2
   subdir): config `zynk.sqlite_home` → `ZYNK_SQLITE_HOME` (exact dir) → `ZYNK_HOME` (+ `zynk.db`,
   **no** `zynk-v2`) → default `~/.zynk/zynk.db`. The legacy `…/zynk-v2/zynk.db` location is recognized
   only for transitional detection/import (so an existing native-v2 DB is found and can be adopted), and
   is never created as a new default.

2. **Foreign-DB fail-closed guard (safety-critical).** Before native Zynk opens/migrates the DB at a
   resolved path, it classifies any existing file:
   - **Absent/empty** → native initializes it (run `MIGRATOR`).
   - **Native** (recognized: the DB carries native Zynk's `_sqlx_migrations` lineage / a native
     `zynk_meta` marker) → open normally.
   - **Foreign** (non-empty and **not** a recognized native DB — this covers the wrapper-era schema **and
     any unknown DB**) → **FAIL CLOSED**: refuse normal operation and return a clear, **Zynk-branded**
     error naming the path, stating that legacy/foreign data was found, and instructing the explicit
     cutover/import/backup action. Native Zynk **never** auto-migrates, overwrites, or mutates a foreign
     DB. (Recognition is "is this our DB?", robust to not knowing the wrapper's exact schema; a positive
     wrapper sniff may sharpen the message but the default-deny is the guarantee.)

3. **Explicit, non-destructive cutover command.** A repo-side `zynk db` command provides the deterministic
   transition (no automatic/destructive/silent action): inspect/status the DB at the final path; and an
   explicit `adopt`/`import`/`backup` action that **backs up or relocates** the legacy wrapper DB (e.g. to
   `~/.zynk/zynk.db.wrapper-backup-<stamp>`) **before** native Zynk owns the final path. If a full command
   is not shipped in M6, a complete command-ready cutover plan is documented so the operator's later live
   cutover is deterministic. Either way the legacy data is preserved (backup/relocate), never destroyed.

4. **Dev isolation is unchanged.** The dev runtime resolves to `/tmp` via `ZYNK_SQLITE_HOME` and the
   `preflight` continues to **refuse** `~/.zynk/zynk.db` and the production `~/.zynk` root for the dev
   runtime (hard rule §2). The foreign-DB guard governs the **native product** path; the two are
   independent and both hold. All DB tests use isolated temp homes (`ZYNK_HOME`/`XDG_*`) with a **planted
   fake wrapper-schema DB**; M6 never touches the operator's real `~/.zynk/zynk.db`.

5. **Docs/help distinguish final vs transitional.** User docs/help present the final native layout
   (`~/.zynk/zynk.db` data, `~/.config/zynk/` config) and describe any import/adopt/backup path as an
   explicit, transitional cutover action — not as a permanent coexistence default.

## Alternatives considered

- **Keep `~/.zynk/zynk-v2/zynk.db` as the final default** — rejected: the operator's product is native
  Zynk, not a permanent coexistence workaround; `zynk-v2` must not survive by inertia.
- **Auto-import / auto-migrate the wrapper DB into native schema** — rejected: schemas differ; silent
  migration risks data loss and is destructive. Import must be explicit and non-destructive.
- **Silently create a fresh native DB at `~/.zynk/zynk.db`, ignoring/overwriting wrapper data** —
  rejected: destroys real user data; the whole point of the guard is to refuse this.
- **Wrapper-schema-specific detection only** — rejected as the *primary* mechanism: default-deny on any
  non-native DB is safer and doesn't depend on knowing the wrapper's exact schema (a positive wrapper
  sniff is additive, for a clearer message).

## Consequences

- The final native DB path is the clean `~/.zynk/zynk.db`; `zynk-v2` is retired as a default.
- When wrapper-era (or any foreign) data occupies the final path, native Zynk fails closed with a
  Zynk-branded error and a deterministic, non-destructive cutover/import/backup action — never a silent
  overwrite or migration.
- Tests prove: path precedence; planted fake-wrapper-schema DB at the final path ⇒ native refuses;
  explicit isolated adopt/import/backup behavior; isolated test-DB behavior. The operator's real
  `~/.zynk/zynk.db` is never touched during M6.
- ADR 0003's schema/retrieval/embedding decisions are unchanged; only its DB-path default is superseded.
