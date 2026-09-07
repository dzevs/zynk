# ADR 0011 — Foreign-DB inspection: the durable boundary is the data bytes, not the `-shm` wal-index

- **Status:** Accepted (release 3.1.0 prep; Gate-3 finding G3-DB-002)
- **Amends:** ADR 0008 §2 (foreign-DB fail-closed guard) — clarifies what "never mutates a foreign DB" covers.

## Context

ADR 0008 §2 requires Zynk to classify an existing database at the resolved native path with a READ-ONLY
connection before any writable open, and to fail closed on a foreign database without mutating it. Gate-3
for 3.1.0 showed that inspecting a foreign database in SQLite **WAL** mode leaves the main file and the
`-wal` journal byte-identical but can create or update the `-shm` sidecar — SQLite's shared-memory
wal-index, which every WAL reader needs and rebuilds from `-wal` on demand.

Two sidecar-clean alternatives were tried and rejected:

- `locking_mode = EXCLUSIVE` keeps the wal-index on the heap (no `-shm`), but in WAL mode it requires the
  inspecting connection to be the only one: it fails with "database is locked" whenever a Zynk server
  already holds the database open, i.e. in the normal CLI/worker case.
- `immutable = 1` skips locks and sidecars entirely, but then SQLite ignores the `-wal` journal: a foreign
  database whose objects live only in un-checkpointed WAL frames would look empty and be migrated into —
  exactly what ADR 0008 forbids.

## Decision

1. The **durable boundary** of the fail-closed guard is the **data-bearing bytes**: the main database file
   and its `-wal` journal are never modified by classification or by a refused open. This is what the
   regressions assert (`read_only_classification_leaves_wal_sidecars_untouched`,
   `read_only_classification_does_not_create_shm_for_wal_db_without_one`, and every `*_fails_closed`
   test's byte-identity check).
2. The `-shm` wal-index is **coordination state, not data**: a read-only inspection may create or update
   it. It is reconstructible from `-wal`, carries no rows or schema, and SQLite itself treats it as
   disposable. `zynk db status` and the guard's classification therefore remain "non-mutating" in the
   sense that matters — no byte of user data or schema changes — and the documentation says so explicitly.
3. Classification reasons over the **complete, lossless** `sqlite_master` object set (tables, views,
   indexes, triggers; names as bytes) and recognizes a native database by its recorded **migration
   provenance** (`_sqlx_migrations` rows matching the built-in migrator's version + checksum), never by
   table names. A database whose only object is an **empty** migration ledger is treated as new
   (a native initialization in progress or aborted). Everything else non-empty fails closed.
4. First-time initialization is serialized across processes by a cooperative advisory lock beside the
   database (`<db>.init-lock`, zero bytes), taken **only** when the database is absent or new; a fully
   migrated native database opens without waiting on it. A server that cannot initialize, classify or
   migrate its database at startup **exits** with the error instead of running without persistence.

## Consequences

- Operators auditing "does zynk touch my database?" should compare the main file and `-wal` bytes; a
  changed or newly created `-shm` next to a WAL-mode database is expected and harmless.
- Tests must snapshot db + `-wal` (never `-shm`) when asserting non-mutation.
- The lock sidecar `<db>.init-lock` may appear next to a database that zynk was asked to open; it holds no
  data and is safe to delete when no zynk process is running.
