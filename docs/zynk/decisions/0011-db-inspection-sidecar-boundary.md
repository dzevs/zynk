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

1. The **durable boundary** of the fail-closed guard is the **existing data bytes**: the main database file
   and any existing `-wal` journal content are never modified by classification or by a refused open. This
   is what the regressions assert (`read_only_classification_leaves_existing_data_bytes_untouched_on_live_wal`,
   `…_on_wal_copy_without_shm`, `…_on_checkpointed_wal`, and every `*_fails_closed` test's byte-identity
   check).
2. The `-shm` wal-index is **coordination state, not data**: a read-only inspection may create or update
   it, and on a cleanly checkpointed WAL database with no sidecars SQLite also creates an **empty** `-wal`
   (0 bytes) alongside a 32 KiB `-shm`. Both are reconstructible/derivable, carry no rows or schema, and
   SQLite itself treats them as disposable. `zynk db status` and the guard's classification therefore remain "non-mutating" in the
   sense that matters — no byte of user data or schema changes — and the documentation says so explicitly.
3. Classification reasons over the **complete, lossless** `sqlite_master` object set (tables, views,
   indexes, triggers; names as bytes) and recognizes a native database by its recorded **migration
   provenance** (`_sqlx_migrations` rows matching the built-in migrator's version + checksum), never by
   table names. A database whose only object is an **empty** migration ledger is treated as new
   (a native initialization in progress or aborted). Everything else non-empty fails closed.
4. First-time initialization is serialized across processes by a cooperative advisory lock beside the
   database (`<db>.init-lock`, zero bytes), taken when the database is absent, new, **or native with pending
   migrations** (sqlx's SQLite migrator has no cross-process lock of its own); only a fully **current** native
   database — every built-in migration recorded — opens without waiting on it. Inspection reads the schema,
   the ledger and the currentness verdict inside **one read transaction** (a single SQLite snapshot), ends it
   before any lock wait, and takes a fresh snapshot under the lock. A server that cannot initialize, classify or
   migrate its database at startup **exits** with the error instead of running without persistence.

## Consequences

- Operators auditing "does zynk touch my database?" should compare the main file and the existing `-wal`
  bytes; a changed or newly created `-shm`, or a newly created empty `-wal`, next to a WAL-mode database is
  expected and harmless.
- Tests must snapshot the main file + existing `-wal` content (never `-shm`) when asserting non-mutation.
- Native recognition is by migration provenance and is not authentication: a deliberately fabricated ledger
  with copied public checksums is outside ADR 0008's threat model (accidental data loss).
- The lock sidecar `<db>.init-lock` may appear next to a database that zynk was asked to open; it holds no
  data and is safe to delete when no zynk process is running.

## Addendum — Gate-3 round 2 (release 3.1.0 prep, before merge; findings G3-R2-DB-001…006, G3-R2-SRV-001)

The decision above stands; the swarm's second round showed the guard could still be **redirected** or
**bypassed** around it. The following are part of the same decision:

5. **One pinned connection.** The opener inspects and writes on the SAME read-write connection: the open
   binds the target file, so a symlink flip or an atomic rename between the verdict and the writable use
   cannot redirect the writes to a different file (classification authority is never reused for a swapped
   target). Connecting changes no journal mode (no header write), and SQLite's checkpoint-on-close is
   disabled (`SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`) until the open succeeds, so a refused foreign WAL database
   is never checkpointed into its main file. A WAL-mode database — every zynk-initialized database — binds
   its `-wal`/`-shm` to the connection during the inspection read; where a by-name sidecar open is still
   ahead (first-time initialization; a database externally converted to rollback journaling) the opener
   verifies the path's file identity (Unix `dev`/`inode`) around the switch to WAL and re-inspects on the
   same connection afterwards, failing closed (`db_target_changed`) if the name no longer refers to the
   inspected file. The connection also keeps its WAL **persistent** (`SQLITE_FCNTL_PERSIST_WAL`): SQLite's
   close path otherwise removes `<db>-wal` by NAME after its checkpoint, the one sidecar operation the
   connection could not bind — so a zynk-opened database keeps an (empty or checkpointed) `-wal`/`-shm`
   beside it, which this ADR already allows. The `zynk db status` read-only inspection is unchanged.
6. **Lock before create.** An absent or zero-byte database is created only AFTER the init lock is held;
   nothing is created while another opener initializes or a foreign holder blocks us (an existing database is
   inspected first, then re-inspected under the lock when initialization or an upgrade is needed).
7. **Orphan sidecars are data.** A nonempty `-wal` or `-journal` beside an absent/zero-byte main file is
   refused before ANY connection exists (`db_orphan_sidecar`): SQLite discards a stale `-wal` on the first
   read of a zero-page database and would replay a hot journal, so initialization would consume or destroy
   existing bytes. `zynk db status` reports the same refusal.
8. **`sqlite_*` is not a free pass.** The complete object set excludes only entries SQLite itself maintains,
   recognized by exact kind/name/shape — the implicit autoindex `sqlite_autoindex_<table>_<N>` (no SQL) of a
   table that is present, and `sqlite_sequence` / `sqlite_stat1` / `sqlite_stat4` with their fixed DDL. A
   readable table raw-renamed into the reserved prefix counts as foreign schema (and is named, escaped, in
   the diagnostic). ANALYZE'd native databases and AUTOINCREMENT bookkeeping remain native/empty as before.
9. **Newer lineage is ours but not openable.** Unknown newer ledger rows are tolerated only when EVERY
   built-in migration is recorded with matching checksums; the result is native **newer** — `zynk db status`
   never calls it "ready", and the opener refuses it (`db_newer_lineage`) before any writable pragma. A
   partial known prefix plus unknown newer rows is not serially producible and fails closed as foreign.
10. **Diagnostics are terminal-safe.** Schema names in `db status` and foreign-database errors have control
    characters (LF, ESC, C1) escaped; non-UTF-8 names are shown as hex.
11. **The handoff-import server is covered.** The Unix live-handoff replacement runs the same fail-closed
    DB pre-flight before any public service (on failure it exits before "restored" and the old server rolls
    back and keeps serving) and installs the receipt/embedding DB workers like a primary start.
12. **Hot rollback journals are refused, conservatively** (Codex Gate-2 round 8). A nonempty `<db>-journal`
    with a non-zero first byte beside a non-empty main file is refused (`db_hot_journal`) BEFORE any
    connection exists, at every connect path — a read-write pager plays a hot journal back on its first
    shared lock (rewriting the main file and deleting the journal) before any verdict could run. SQLite's own
    test also consults lock state (a live writer's journal is not hot); zynk cannot observe that without a
    pager, so it refuses every journal that looks hot, re-checking once after acquiring the init lock (a zynk
    initializer's own brief journal during its journal-mode switch is held under that lock). Inactive
    PERSIST-mode (zeroed header) and TRUNCATE-mode (empty) journals pass; an unreadable journal fails closed;
    metadata failures are never read as absence (`db_io_error`). `zynk db status` reports the same refusal.
13. **No orphan-message recovery beside a live server.** The replacement's pre-flight validates readiness
    and migrations WITHOUT `recover_orphan_messages`; only a cold start recovers. A synthesized `failed`
    freezes a message (no `submitted`/`received` may follow), and during a handoff a sender may legitimately
    be between persisting its message and its first transport event.
14. **The DB-worker handover is ordered.** The old server stops and joins its receipt/embedding workers
    BEFORE it sends "committed" (its public sockets are already down); the replacement starts its workers
    only AFTER "committed"; a failed commit restores the old server's workers before it rolls back. A job is
    therefore never owned by two workers (the replacement's startup recovery resets `running` jobs, and two
    pollers would select the same pending batch).
15. **One SQLite-effective pathname** (Codex Gate-2 rounds 9-10). SQLite's `unixFullPathname` (bundled 3.46.0:
    `appendAllPathElements`) walks every component — it folds `.`/`..` and follows a symlink in any component,
    directories included. The opener and `db status` resolve the configured path made absolute plus the FINAL
    component's symlink chain (a relative target joined with the link's directory, bounded), with no lexical
    normalization added: a directory link maps a whole directory, so the main file, its sidecars and the init
    lock already coincide through it and the kernel applies the same resolution to the unresolved prefix on
    every open; only a linked FINAL component moves the sidecars away from the configured name, and that is
    what is resolved. That one name is used for the sidecar guards, the init lock, the identity capture and the
    connection. A stable `zynk.db -> foreign.db` link used to let the guards inspect `zynk.db-journal` while
    SQLite replayed `foreign.db-journal`.
16. **The DB-worker handover is a protocol contract: handoff version 2.** A version-1 peer (zynk 3.0.x sends
    "committed" with its workers still running) is refused before "validated" in either direction; the sender
    rolls back and keeps serving. What the diagnostics can promise depends on which side is new: a 3.1.0
    replacement that REFUSES an old sender's manifest logs the reason and remedy durably (the immutable old
    sender reports only its own generic error); a 3.1.0 sender whose old replacement closes without answering reports the
    transport cause plus a *possible* incompatible-peer / restart-normally hint to the requester and logs it
    durably (the old replacement cannot); between two 3.1.0 servers the exact reason travels back as a
    `rejected: …` line.
17. **Worker quiescence is bounded and decided before service moves.** Both DB workers are paused — a bounded
    wait for an idle point between units of work (`ZYNK_HANDOFF_WORKER_IDLE_MS`, default 10 s), after which no
    new unit starts until resume — BEFORE any socket is withdrawn. Busy past the deadline: the handoff is
    refused, the workers resume, ownership is retained. Idle: the handoff proceeds and the idle handles are
    joined at commit (immediate), so a slow or stuck provider never couples to the replacement's 30 s
    "committed" wait; every pre-commit rollback resumes the workers. A worker's startup (runtime + initial DB
    open, which may wait on the init lock) counts as work — idle is acknowledged only once it completed or failed.
    Quiescent means **nothing in flight AND nothing queued**: a receipt accepted by the API but not yet finished
    (its caller may have timed out long ago) is outstanding work — a paused receipt worker accepts nothing new
    (`receipt_worker_busy`, retryable) while already-queued jobs drain inside the deadline; if they cannot, the
    handoff is refused and admission reopens. So no DB work ever starts after a pause was acknowledged, and the
    commit-time join is immediate. Ordinary shutdown still drains queued receipts.
18. **Cutover moves the complete SQLite bundle into one reserved backup directory** (Gate-3 rounds 3-4). `zynk
    db adopt`/`backup` relocate the main file together with every existing `-journal`, `-wal` and `-shm` — every
    directory ENTRY beside the path, a dangling sidecar link included (`exists` follows links and skipped it) —
    into `<db>.wrapper-backup-<N>/`, a directory created with one atomic `mkdir` (owner-only; it fails when any
    entry exists at that name), so the whole bundle namespace is reserved at once: flat per-file renames could
    never reserve four names together, and a competitor's sidecar landing beside a moved main was reported as a
    complete backup. Members keep their own names inside the slot (SQLite can open the backup in place).
    Sidecars move first and the main file last, each in ONE atomic no-replace step — the platform's no-replace
    rename (Linux `renameat2(RENAME_NOREPLACE)`, macOS `renamex_np(RENAME_EXCL)`, Windows `MoveFileExW` without
    the replace flag); where no such primitive exists the member is refused with an actionable error, never a
    two-step link/copy + unlink of the source name (a writer that atomically replaces the source or the target
    between two steps loses a file; an identity check before the unlink only narrows that window). After the
    moves the slot must hold exactly the members that were moved; anything else means the backup is not this
    bundle: the members move back and the command fails. The guaranteed contract is per-member atomicity, never
    an overwrite and never a successful partial relocation — NOT a crash-atomic transaction over the bundle nor a
    consistent snapshot of a database that is being written (quiesce writers first). A rollback can be blocked
    by a writer that recreated an original name in the meantime; that state is reported explicitly, naming every
    member that now lives under the slot. The commands also relocate what the guards refuse to open (a journal
    that looks hot; orphan sidecars beside an absent main file; a sidecar that is a symbolic link), which is
    exactly the remedy those refusals name. Relocation acts on the SQLite-effective path — the final symlink
    chain resolved, exactly as inspection classifies — so what the guards refuse through a link is what moves;
    the link stays.
19. **Terminal-safe output covers Unicode format and bidi controls, and paths.** The hostile-character rule
    is C0/C1 controls plus the Unicode format (`Cf`), bidi/invisible characters and the line/paragraph
    separators (`Zl`/`Zp`) — a U+202E override can reorder displayed text as surely as ESC can recolor it —
    escaped as `\u{…}`; it applies to schema names AND to every printed database path, in `db status`, the
    cutover commands and every error the guards raise.

20. **An embedding job is owned by its claim, across processes** (Gate-3 round 3). Several zynk servers share one
    database (named sessions; a live handoff). A worker takes a job with ONE atomic status-guarded update that
    returns the job's incremented `attempts` — nothing else ever changes that counter — and that value is the
    claim token: the job's `done`/`failed` updates match the id, `status = 'running'` AND the token, so a worker
    that stalled past the running lease (10 min; recovery hands such jobs back to the queue) can neither
    complete nor fail the job its successor owns. The fence is the FIRST statement of the immediate write
    transaction, before the vector and `message_embeddings` rows: a stale claim's late success rolls back and
    leaves no trace (a vector committed under the new owner would make the owner's own completion fail on the
    vec0 uniqueness and strand the job `failed` beside a live vector). Zero rows affected on the claim means
    another worker owns the job and it is skipped, never run twice.
21. **Orphan-message recovery has a grace window.** Every ordinary server start runs cold-start recovery over
    the shared database, but a message with no delivery event is IN FLIGHT until it is older than the grace
    window (5 min): a sender persists the message before its first transport event, and a peer server's
    start must not fail it. Only older event-less messages are failed (`system.recovery`), and each exactly
    once across every server recovering at the same time: the claim (bumping `delivery_seq`) is conditioned
    on the message STILL having no delivery event inside one immediate transaction, so the second claimant
    sees zero rows and records nothing.
22. **A receipt binds to the stored target participant, never to an agent label.** The participant key is
    label + terminal + hook session — pane ids rotate and are deliberately not part of it. The durable anchor
    is the hook session value: it survives pane churn, a restart and a live handoff (the restored terminal
    keeps its persisted agent session, while terminal ids are allocated per server lifetime). When the stored
    participant carried a session, the authoritative receiver (hook authority only) must present the same
    one — the full triple the participant key hashes (source, kind, value), never the value alone. A session
    counts as a party's identity only when it was reported for the SAME agent the party is labeled as (owner
    coherence, applied when the participant is stored and when the receiver is resolved): a terminal can
    carry a session persisted for another owner, and that session anchors nothing — it is treated as absent
    on both sides. Self-receipt is decided by the same durable identity: a message whose sender and addressee
    are the same participant is never receipted, from any pane — the stored pane id (the first one seen) is
    audit metadata, not the guard. When the participant carried none (a hook that reported no session id — a generic `hook` source cannot carry
    one), the terminal id binds instead, within this server's lifetime; otherwise
    `receiver_identity_mismatch`. A second pane carrying the same label under another session (or,
    session-less, on another terminal) is not the addressee. Limit: a session-less target cannot be re-bound
    after a restart or handoff (its terminal id changed); every shipped integration reports a session id.
23. **`adopt` is bounded to the resolved native path** (Gate-3 round 4). The ambient legacy database
    (`$HOME/.zynk/zynk-v2/zynk.db`) is a candidate for relocation only when the native path is the default one;
    a path selected by config, `ZYNK_SQLITE_HOME` or `ZYNK_HOME` that is absent is a no-op — a mutating command
    never leaves the path the operator (or an isolated harness) chose.
24. **A sidecar entry that is a symbolic link fails closed** (Gate-3 round 4), dangling or not: SQLite opens
    `-journal`/`-wal`/`-shm` by name and would create or write the sidecar through the link — elsewhere, or
    nowhere (a dangling link made the first native start fail after `metadata`, which follows links, had read it
    as absent). The refusal names `zynk db adopt`, which moves the entry itself.

Residual, documented limits (outside ADR 0008's accidental-data-loss threat model):

- During first-time initialization SQLite itself deletes a `-wal` that appears beside the zero-page database
  and creates the new `-wal`/`-shm` by name; the orphan-sidecar guard therefore runs before the database is
  created, and a sidecar renamed into place DURING initialization is SQLite's to discard.
- A rollback journal that becomes hot, or is renamed into place, between the pre-connect check and the
  connection's first read is played back by SQLite as for any client; the guard closes the case that
  matters (a crashed database found at the path), not that race.
- Catalog text is compared as UTF-8 bytes: a database whose text encoding is not UTF-8 cannot be zynk's and
  classifies as foreign, including its SQLite bookkeeping tables (conservative; never a write hole).
- Diagnostics escape control characters in schema names AND in the printed database path.
- A WAL-mode database separated from its `-wal` is, to SQLite and therefore to zynk, whatever its main file
  holds — a main file whose schema still lives in un-checkpointed WAL frames reads as **empty** and is
  initialized. Never move or copy a WAL database without its `-wal` (standard SQLite guidance; `zynk db
  adopt`/`backup` move the sidecars along).
- The file-identity check is Unix-only (`std` exposes no stable file identity on Windows); the pinned
  connection and the post-switch re-inspection apply everywhere.
- The orphan-recovery grace window (5 min) is a recovery policy, not a provable upper bound on the gap between
  persisting a message and its first transport event: a sender suspended for longer is recovered as failed by
  a peer's start (exactly once). The embedding attempt cap bounds retries after a recorded failure, not the
  lifetime claim count — a stale claim recovered past the lease is claimed again with a higher token.
