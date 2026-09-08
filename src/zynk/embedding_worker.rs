//! zynk fork: App-owned embedding worker (M5b §B4).
//!
//! Unlike the receipt worker (request/response over a bounded channel), the
//! embedding worker is POLL-driven: it owns a dedicated `std::thread` with ONE
//! current-thread Tokio runtime and ONE reused `SqliteConnection`, and on a fixed
//! cadence it drains `embedding_jobs` rows for the active model, computes their
//! embeddings out-of-band, and writes the vectors into the `vec0` table +
//! `message_embeddings`. The send path NEVER blocks on this — it only enqueues a
//! `pending` job (B3); this worker is the sole place embeddings are computed.
//!
//! Like the receipt worker, the worker only ever opens the native zynk DB; it never
//! touches live Zynk state. Dropping the handle signals shutdown and joins.
//!
//! The CORE behaviours (enqueue/backfill/embed/retry/recover) are implemented as
//! `pub(crate)` async fns and tested DIRECTLY against a temp DB — deterministic,
//! independent of the background poll loop's timing. The poll loop just stitches
//! them together on a cadence; it is intentionally NOT exercised by the unit tests.

#![allow(dead_code)]

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use sqlx::{Executor, Row, SqliteConnection};

use crate::zynk::db::DbError;
use crate::zynk::embed::{embedder_from_env, Embedder};
use crate::zynk::message::{body_hash, new_prefixed_id, now_rfc3339};
use crate::zynk::worker_pause::PauseControl;

/// Default poll cadence between `process_pending_batch` sweeps. Overridable via
/// `ZYNK_EMBED_POLL_MS` (mirrors the receipt worker's env-tunable timeout pattern).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Env var overriding the poll cadence (milliseconds).
pub const ZYNK_EMBED_POLL_MS_ENV: &str = "ZYNK_EMBED_POLL_MS";
/// How many missing-job backfill rows to enqueue per pass (the caller loops to 0).
pub const BACKFILL_BATCH: i64 = 256;
/// How many runnable jobs to process per poll sweep.
pub const PROCESS_BATCH: i64 = 32;
/// A failed job is retried while `attempts < MAX_ATTEMPTS`; at/after the cap it is
/// left `failed` and no longer selected.
pub const MAX_ATTEMPTS: i64 = 5;

/// A `running` job older than this is presumed orphaned by a crashed worker and is recovered
/// (reset to `pending`). Every zynk server sharing the global database runs its own worker, so
/// recovery must never touch a job another LIVE worker is still executing (Gate-3 round 3,
/// AUD-310-WORKER-001); a real embedding takes seconds, so ten minutes is a safe stale bound.
pub const RUNNING_LEASE: Duration = Duration::from_secs(600);

/// How often the poll loop re-runs stale-job recovery (a crashed peer's jobs come back without a
/// restart).
const RECOVERY_INTERVAL: Duration = Duration::from_secs(60);

/// Resolve the poll cadence from `ZYNK_EMBED_POLL_MS` (>0), else the default.
fn poll_interval() -> Duration {
    std::env::var(ZYNK_EMBED_POLL_MS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_POLL_INTERVAL)
}

/// Handle to the App-owned embedding worker thread. Owned by the headless server;
/// `None` in CLI / unit-test App constructors. Dropping it stops the worker.
pub struct EmbeddingWorkerHandle {
    shutdown: Option<SyncSender<()>>,
    join: Option<JoinHandle<()>>,
    control: std::sync::Arc<PauseControl>,
}

impl EmbeddingWorkerHandle {
    /// Stop starting new batches and wait (bounded) for the loop to be between batches — the
    /// in-flight `embed` call is never interrupted. `true` = idle now and staying idle until
    /// `resume`; `false` = still busy past `deadline` (the worker keeps running; call `resume`).
    /// Live-handoff quiescence (Codex Gate-2 round 9).
    pub fn pause(&self, deadline: Duration) -> bool {
        self.control.pause(deadline)
    }

    /// Let a paused loop process batches again.
    pub fn resume(&self) {
        self.control.resume();
    }
}

impl Drop for EmbeddingWorkerHandle {
    fn drop(&mut self) {
        // NON-blocking shutdown: `try_send` can't hang on a full queue. Then drop
        // the sender so the worker's `recv_timeout` disconnects and the loop breaks
        // at its next wake. `join` is bounded by the in-flight `process_pending_batch`
        // — up to PROCESS_BATCH synchronous `embedder.embed` calls (instant for the
        // default FakeEmbedder; seconds with the real model), then the conn writes —
        // so it always returns, never an indefinite block. (Mid-batch shutdown
        // responsiveness with a slow real embedder is future tuning; not redesigned
        // here.) Never touches live Zynk state.
        self.control.stop();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.try_send(());
            drop(shutdown);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Spawn the embedding worker thread. Call once at server startup; install the
/// returned handle on `App`. No args — it resolves the embedder + DB internally.
pub fn spawn() -> EmbeddingWorkerHandle {
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let control = std::sync::Arc::new(PauseControl::new());
    let loop_control = std::sync::Arc::clone(&control);
    let join = std::thread::Builder::new()
        .name("zynk-embed-worker".to_string())
        .spawn(move || worker_loop(shutdown_rx, loop_control))
        .expect("spawn zynk embedding worker thread");
    EmbeddingWorkerHandle {
        shutdown: Some(shutdown_tx),
        join: Some(join),
        control,
    }
}

fn worker_loop(shutdown_rx: Receiver<()>, control: std::sync::Arc<PauseControl>) {
    // Register vec0 process-globally BEFORE opening any connection (so every conn
    // this worker opens — and reopens — sees `vec0`).
    crate::zynk::embed::vec::register_sqlite_vec();

    // One current-thread runtime owns all DB work for this worker — safe because
    // this is a plain std::thread with no ambient Tokio runtime.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            // Cannot build a runtime: drain the shutdown signal so a Drop join is
            // bounded, then exit. Jobs simply stay `pending`.
            control.set_idle();
            let _ = shutdown_rx.recv();
            return;
        }
    };

    // Resolve the embedder. If the provider can't be built (e.g. fastembed not
    // provisioned), the worker simply does not run — jobs stay `pending`, which is
    // the by-construction "send never blocks on embedding" guarantee degrading
    // gracefully. Wait for shutdown so the join stays bounded.
    let mut embedder = match embedder_from_env() {
        Ok(embedder) => embedder,
        Err(_err) => {
            control.set_idle();
            let _ = shutdown_rx.recv();
            return;
        }
    };

    let mut conn = rt
        .block_on(crate::zynk::db::open_migrated_for_append())
        .ok();

    // Derive (model_id, vec_table, dim) and ensure the vec0 table exists. On any
    // failure here we cannot proceed; wait for shutdown so join stays bounded.
    let (model_id, vec_table, dim) = match conn.as_mut() {
        Some(c) => match rt.block_on(ensure_model_and_vec0(c, embedder.as_ref())) {
            Ok(triple) => triple,
            Err(_err) => {
                control.set_idle();
                let _ = shutdown_rx.recv();
                return;
            }
        },
        None => {
            control.set_idle();
            let _ = shutdown_rx.recv();
            return;
        }
    };

    // Crash recovery: reset any `running` jobs (a prior process died mid-flight)
    // back to `pending` so they are picked up again. Best-effort.
    if let Some(c) = conn.as_mut() {
        let _ = rt.block_on(recover_running_jobs(c, &model_id));
    }

    // Bounded backfill: enqueue jobs for any messages that predate the worker /
    // were never enqueued, in batches, until a pass returns 0. Best-effort.
    if let Some(c) = conn.as_mut() {
        loop {
            match rt.block_on(backfill_missing(c, &model_id, BACKFILL_BATCH)) {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
    }

    let interval = poll_interval();
    let mut last_recovery = std::time::Instant::now();
    control.set_idle();
    loop {
        if conn.is_none() {
            conn = rt
                .block_on(crate::zynk::db::open_migrated_for_append())
                .ok();
        }
        // Paused (live handoff): keep the connection, start no batch until resumed.
        if control.try_begin_work() {
            if let Some(c) = conn.as_mut() {
                if last_recovery.elapsed() >= RECOVERY_INTERVAL {
                    let _ = rt.block_on(recover_running_jobs(c, &model_id));
                    last_recovery = std::time::Instant::now();
                }
                let _ = rt.block_on(process_pending_batch(
                    c,
                    embedder.as_mut(),
                    &model_id,
                    &vec_table,
                    dim,
                    PROCESS_BATCH,
                ));
            }
            control.end_work();
        }
        match shutdown_rx.recv_timeout(interval) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => continue,
        }
    }
}

/// Deterministic, safe `vec0` table identifier for `model_id`: `message_vec_` +
/// the lowercased model_id with every non-`[a-z0-9]` char replaced by `_`. Always
/// an ASCII alnum/underscore identifier (so `ensure_vec0_table`'s validation passes).
pub(crate) fn vec_table_for(model_id: &str) -> String {
    let mut out = String::with_capacity("message_vec_".len() + model_id.len());
    out.push_str("message_vec_");
    for ch in model_id.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() {
            out.push(lowered);
        } else {
            out.push('_');
        }
    }
    out
}

/// Provider tag persisted in `embedding_models.provider`: `"fake"` for the fake
/// embedder, `"fastembed"` otherwise (the real arm).
fn provider_for(model_id: &str) -> &'static str {
    if model_id.starts_with("fake") {
        "fake"
    } else {
        "fastembed"
    }
}

/// Register the active model in `embedding_models` (idempotent) and ensure its
/// `vec0` table exists. Returns `(model_id, vec_table, dim)` — the tuple the poll
/// loop carries. `model_id`/`dim` come straight from the embedder; the worker MUST
/// process jobs for this id (it equals `active_model_id()` for the active provider).
pub(crate) async fn ensure_model_and_vec0(
    conn: &mut SqliteConnection,
    embedder: &dyn Embedder,
) -> Result<(String, String, usize), DbError> {
    let model_id = embedder.model_id().to_string();
    let dim = embedder.dim();
    let vec_table = vec_table_for(&model_id);
    let now = now_rfc3339();

    sqlx::query(
        "INSERT OR IGNORE INTO embedding_models (id, provider, model_name, dim, normalize, vec_table, created_at) VALUES (?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&model_id)
    .bind(provider_for(&model_id))
    .bind(&model_id)
    .bind(dim as i64)
    .bind(&vec_table)
    .bind(&now)
    .execute(&mut *conn)
    .await?;

    crate::zynk::embed::vec::ensure_vec0_table(conn, &vec_table, dim).await?;
    Ok((model_id, vec_table, dim))
}

/// Crash recovery: reset `running` jobs for `model_id` back to `pending` and clear
/// `started_at`. Returns the number of rows reset.
pub(crate) async fn recover_running_jobs(
    conn: &mut SqliteConnection,
    model_id: &str,
) -> Result<u64, DbError> {
    recover_running_jobs_older_than(conn, model_id, RUNNING_LEASE).await
}

/// Reset to `pending` the `running` jobs whose `started_at` is older than `lease` (or NULL): the
/// ones a crashed worker left behind. Jobs a live worker started inside the lease are left alone —
/// a claim is only ever taken over once it is stale.
pub(crate) async fn recover_running_jobs_older_than(
    conn: &mut SqliteConnection,
    model_id: &str,
    lease: Duration,
) -> Result<u64, DbError> {
    let cutoff = crate::zynk::message::rfc3339_utc(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            - lease.as_secs() as i64,
    );
    let result = sqlx::query(
        "UPDATE embedding_jobs SET status='pending', started_at=NULL \
         WHERE model_id=? AND status='running' AND (started_at IS NULL OR started_at < ?)",
    )
    .bind(model_id)
    .bind(&cutoff)
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected())
}

/// Enqueue up to `limit` `pending` jobs for messages that have NO `embedding_jobs`
/// row for `model_id` (pre-worker messages / never-enqueued). Returns how many were
/// enqueued; the caller loops until 0 (bounded backfill).
pub(crate) async fn backfill_missing(
    conn: &mut SqliteConnection,
    model_id: &str,
    limit: i64,
) -> Result<u64, DbError> {
    let rows = sqlx::query(
        "SELECT m.id AS id FROM messages m \
         LEFT JOIN embedding_jobs j ON j.message_id = m.id AND j.model_id = ? \
         WHERE j.id IS NULL ORDER BY m.created_at LIMIT ?",
    )
    .bind(model_id)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;

    let now = now_rfc3339();
    let mut enqueued: u64 = 0;
    for row in rows {
        let message_id: String = row.try_get("id")?;
        let result = sqlx::query(
            "INSERT OR IGNORE INTO embedding_jobs (id, message_id, model_id, status, attempts, enqueued_at) VALUES (?, ?, ?, 'pending', 0, ?)",
        )
        .bind(new_prefixed_id("ejob"))
        .bind(&message_id)
        .bind(model_id)
        .bind(&now)
        .execute(&mut *conn)
        .await?;
        enqueued += result.rows_affected();
    }
    Ok(enqueued)
}

/// Select up to `limit` runnable jobs for `model_id` (`pending`, or `failed` with
/// `attempts < MAX_ATTEMPTS`) in enqueue order and process each. Returns how many
/// were attempted. A single job's failure is recorded on that job, never propagated.
pub(crate) async fn process_pending_batch(
    conn: &mut SqliteConnection,
    embedder: &mut dyn Embedder,
    model_id: &str,
    vec_table: &str,
    dim: usize,
    limit: i64,
) -> Result<u32, DbError> {
    let rows = sqlx::query(
        "SELECT id, message_id, attempts FROM embedding_jobs \
         WHERE model_id = ? AND (status='pending' OR (status='failed' AND attempts < ?)) \
         ORDER BY enqueued_at LIMIT ?",
    )
    .bind(model_id)
    .bind(MAX_ATTEMPTS)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;

    let mut processed: u32 = 0;
    for row in rows {
        let job_id: String = row.try_get("id")?;
        let message_id: String = row.try_get("message_id")?;
        process_one_job(
            conn,
            embedder,
            &job_id,
            &message_id,
            model_id,
            vec_table,
            dim,
        )
        .await?;
        processed += 1;
    }
    Ok(processed)
}

/// Process ONE job: mark `running` (+attempt), then run EVERYTHING else (fetch,
/// embed, write) in a fallible inner step whose ANY `Err` is recorded on the job as
/// `failed` and NOT propagated. This is load-bearing: once a job is `running` (an
/// auto-committed state the runnable selector never re-selects), a later `?` MUST
/// NOT escape and strand it — every exit must leave a terminal state (`done`/
/// `failed`). A `failed` job with `attempts < MAX_ATTEMPTS` is re-selected (bounded
/// retry); persistent failure stops at the cap. The worker keeps going regardless.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_one_job(
    conn: &mut SqliteConnection,
    embedder: &mut dyn Embedder,
    job_id: &str,
    message_id: &str,
    model_id: &str,
    vec_table: &str,
    dim: usize,
) -> Result<(), DbError> {
    // Atomic claim: only a job that is still runnable becomes ours — a worker in another zynk
    // server sharing this database may have taken it between the SELECT and here. Zero rows
    // affected = not ours, skip it (the other worker owns it now).
    // The claim returns the job's new `attempts` value: every claim increments it and nothing else
    // ever changes it (recovery resets only status/started_at), so it is this claim's ownership
    // token — every terminal update below matches on it, never on `status='running'` alone.
    let now = now_rfc3339();
    let claimed = sqlx::query(
        "UPDATE embedding_jobs SET status='running', started_at=?, attempts=attempts+1 \
         WHERE id=? AND (status='pending' OR (status='failed' AND attempts < ?)) \
         RETURNING attempts",
    )
    .bind(&now)
    .bind(job_id)
    .bind(MAX_ATTEMPTS)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(claimed) = claimed else {
        return Ok(());
    };
    let claim = JobClaim {
        job_id,
        attempt: claimed.try_get::<i64, _>("attempts")?,
    };

    // Everything past the running-mark runs in `run_job_inner`. Whether it returns a
    // `JobError::Recorded` (a known, retryable per-job failure) OR a `JobError::Db`
    // (a transient DB error mid-step — e.g. SQLITE_BUSY on the message fetch), we
    // mark the job `failed` and return Ok. NO error path may escape this fn while the
    // job is still `running` — that is the no-strand invariant.
    let reason =
        match run_job_inner(conn, embedder, message_id, model_id, vec_table, dim, &claim).await {
            Ok(()) => return Ok(()),
            Err(JobError::Recorded(reason)) => reason,
            Err(JobError::Db(err)) => err.to_string(),
        };
    mark_job_failed(conn, &claim, &reason).await;
    Ok(())
}

/// This worker's ownership of one job: the `attempts` value its claim produced. A job recovered
/// as stale and re-claimed elsewhere carries a higher value, so a late `done`/`failed` from the
/// stale claim matches nothing (AUD-310-WORKER-001).
struct JobClaim<'a> {
    job_id: &'a str,
    attempt: i64,
}

/// A failure from `run_job_inner`. `Recorded` is a known per-job reason (missing
/// message, empty/non-finite embedding, provider error); `Db` is a transient DB
/// error from any step. BOTH are handled identically by the caller (mark `failed`),
/// so no `?` past the running-mark can ever strand a job in `running`.
enum JobError {
    Recorded(String),
    Db(DbError),
}

impl From<DbError> for JobError {
    fn from(err: DbError) -> Self {
        JobError::Db(err)
    }
}

impl From<sqlx::Error> for JobError {
    fn from(err: sqlx::Error) -> Self {
        // Route through DbError's classifier so a `row.try_get(...)?` mid-step yields
        // the same Db variant the caller turns into a `failed` job.
        JobError::Db(DbError::from(err))
    }
}

/// Fetch → embed → write, all fallible. Any `Err` (a transient DB error OR a known
/// per-job reason) is turned into a `failed` job by the caller — see `process_one_job`.
async fn run_job_inner(
    conn: &mut SqliteConnection,
    embedder: &mut dyn Embedder,
    message_id: &str,
    model_id: &str,
    vec_table: &str,
    dim: usize,
    claim: &JobClaim<'_>,
) -> Result<(), JobError> {
    let message = sqlx::query("SELECT rowid, body FROM messages WHERE id=?")
        .bind(message_id)
        .fetch_optional(&mut *conn)
        .await?;
    let (rowid, body): (i64, String) = match message {
        Some(row) => (row.try_get("rowid")?, row.try_get("body")?),
        // Should not happen (FK CASCADE), but if the message is gone, record a
        // terminal failure rather than spin forever.
        None => return Err(JobError::Recorded("message_missing".into())),
    };

    let embedded = match embedder.embed(&[body.as_str()]) {
        Ok(mut vectors) if !vectors.is_empty() => vectors.swap_remove(0),
        Ok(_) => return Err(JobError::Recorded("embedder_returned_no_vector".into())),
        Err(err) => return Err(JobError::Recorded(err.to_string())),
    };
    // Guard the vector before it reaches vec0: a wrong-dim vector would be rejected
    // by the `float[dim]` table, and a NaN/Inf component would serialize to invalid
    // JSON (`"NaN"`/`"inf"`) and be rejected too — both as an opaque permanent fail.
    // FakeEmbedder can't produce either (finite + dim-consistent), but the real
    // fastembed arm (B5) could under degenerate input; record a clear reason instead.
    if embedded.len() != dim {
        return Err(JobError::Recorded("embedding_wrong_dim".into()));
    }
    if embedded.iter().any(|f| !f.is_finite()) {
        return Err(JobError::Recorded("embedding_non_finite".into()));
    }

    // Success path: write the vec0 row + message_embeddings row + mark done, all in
    // ONE transaction so a crash never leaves a half-written embedding. `vec_table`
    // was validated as a safe identifier by `ensure_model_and_vec0`.
    match write_embedding_txn(
        conn, vec_table, rowid, &embedded, message_id, model_id, &body, claim,
    )
    .await?
    {
        WriteOutcome::Written => {}
        WriteOutcome::Stale => tracing::info!(
            message_id,
            "embedding job changed hands during the provider call; this claim's result is discarded"
        ),
    }
    Ok(())
}

/// Outcome of the fenced embedding write.
enum WriteOutcome {
    /// This claim still owned the job: vector + `message_embeddings` written, job marked `done`.
    Written,
    /// The job was recovered as stale and re-claimed while the provider call ran: nothing was
    /// written (the transaction is rolled back) and nothing is recorded — the job is its new
    /// owner's.
    Stale,
}

/// Write the embedding (one transaction): fence the claim's ownership, then vec0 row +
/// message_embeddings row, with the job marked done in the same transaction. Any error rolls
/// back; the caller records the job `failed`. A stale claim rolls back and reports `Stale`.
#[allow(clippy::too_many_arguments)]
async fn write_embedding_txn(
    conn: &mut SqliteConnection,
    vec_table: &str,
    rowid: i64,
    embedded: &[f32],
    message_id: &str,
    model_id: &str,
    body: &str,
    claim: &JobClaim<'_>,
) -> Result<WriteOutcome, DbError> {
    conn.execute("BEGIN IMMEDIATE").await?;
    let result = write_embedding_in_transaction(
        conn, vec_table, rowid, embedded, message_id, model_id, body, claim,
    )
    .await;
    match result {
        Ok(WriteOutcome::Stale) => {
            let _ = conn.execute("ROLLBACK").await;
            Ok(WriteOutcome::Stale)
        }
        Ok(WriteOutcome::Written) => match conn.execute("COMMIT").await {
            Ok(_) => Ok(WriteOutcome::Written),
            // In WAL, COMMIT can return SQLITE_BUSY and does NOT auto-rollback — the
            // reused long-lived conn would be left mid-transaction, folding the
            // caller's `mark_job_failed` and the next job's running-mark into a stale
            // txn. Best-effort ROLLBACK before surfacing the error closes the txn.
            Err(err) => {
                let _ = conn.execute("ROLLBACK").await;
                Err(err.into())
            }
        },
        Err(err) => {
            let _ = conn.execute("ROLLBACK").await;
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_embedding_in_transaction(
    conn: &mut SqliteConnection,
    vec_table: &str,
    rowid: i64,
    embedded: &[f32],
    message_id: &str,
    model_id: &str,
    body: &str,
    claim: &JobClaim<'_>,
) -> Result<WriteOutcome, DbError> {
    let now = now_rfc3339();
    // Ownership fence FIRST, inside the transaction: BEGIN IMMEDIATE holds the write lock, so no
    // claim can interleave between this check and the writes below. Only THIS claim completes
    // the job; zero rows means it was recovered as stale and re-claimed while the provider call
    // ran, and a late success must leave no trace — the caller rolls back before any side effect
    // (a vector committed under the new owner would make its own completion fail on the vec0
    // UNIQUE and strand the job `failed` beside a live vector).
    let fenced = sqlx::query(
        "UPDATE embedding_jobs SET status='done', last_error=NULL, finished_at=? \
         WHERE id=? AND status='running' AND attempts=?",
    )
    .bind(&now)
    .bind(claim.job_id)
    .bind(claim.attempt)
    .execute(&mut *conn)
    .await?;
    if fenced.rows_affected() == 0 {
        return Ok(WriteOutcome::Stale);
    }
    // Format the embedding as a JSON array string — vec0's bind form.
    let json = format!(
        "[{}]",
        embedded
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );

    // vec_table is a validated identifier (NOT user input); format it in.
    let insert_vec =
        format!("INSERT OR REPLACE INTO {vec_table} (message_rowid, embedding) VALUES (?, ?)");
    sqlx::query(&insert_vec)
        .bind(rowid)
        .bind(&json)
        .execute(&mut *conn)
        .await?;

    sqlx::query(
        "INSERT OR REPLACE INTO message_embeddings (message_id, model_id, vec_rowid, text_hash, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(message_id)
    .bind(model_id)
    .bind(rowid)
    .bind(body_hash(body))
    .bind(&now)
    .execute(&mut *conn)
    .await?;

    Ok(WriteOutcome::Written)
}

/// Best-effort: mark a job `failed` with `last_error` + `finished_at`. Swallows any
/// error (the worker must keep running even if this update fails).
async fn mark_job_failed(conn: &mut SqliteConnection, claim: &JobClaim<'_>, last_error: &str) {
    let now = now_rfc3339();
    let _ = sqlx::query(
        "UPDATE embedding_jobs SET status='failed', last_error=?, finished_at=? \
         WHERE id=? AND status='running' AND attempts=?",
    )
    .bind(last_error)
    .bind(&now)
    .bind(claim.job_id)
    .bind(claim.attempt)
    .execute(&mut *conn)
    .await;
}

#[cfg(test)]
mod tests {
    // NOTE: the core behaviours below are tested by driving the `pub(crate)` async
    // fns DIRECTLY against a temp-file DB — deterministic and independent of the
    // background poll loop's timing. `spawn_then_drop_is_clean` is the only test
    // that touches the thread, and it asserts nothing about the DB (it may or may
    // not find the live DB; it only proves the thread + Drop-join don't hang).
    use super::*;
    use crate::zynk::db::open_migrated_at_without_recovery;
    use crate::zynk::embed::FakeEmbedder;
    use crate::zynk::message::{Party, SendCommand};
    use crate::zynk::persistence::{begin_send_attempt_async, SendAttempt};
    use sqlx::Connection;

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "zynk-embed-worker-test-{}-{}.db",
            std::process::id(),
            new_prefixed_id("test")
        ))
    }

    /// Seed a message via the real send path (which also enqueues a B3 pending job).
    async fn seed_message(conn: &mut SqliteConnection, message_id: &str) -> i64 {
        let from = Party {
            agent: Some("alice".into()),
            workspace: Some("workspace".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        let to = Party {
            agent: Some("bob".into()),
            workspace: Some("workspace".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        begin_send_attempt_async(
            conn,
            SendAttempt {
                command: SendCommand::PaneRun,
                message_id,
                target_arg: "bob",
                from: &from,
                to: &to,
                message_type: None,
                body: "the quick brown fox",
                created_at: "2026-06-14T00:00:00Z",
                trace_id: None,
            },
            "rt_test".into(),
            "socket_test".into(),
        )
        .await
        .unwrap();
        let row = sqlx::query("SELECT rowid FROM messages WHERE id=?")
            .bind(message_id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        row.try_get::<i64, _>("rowid").unwrap()
    }

    /// A `FakeEmbedder` that blocks inside `embed` until released: models an in-flight job.
    struct BlockingEmbedder {
        inner: FakeEmbedder,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl Embedder for BlockingEmbedder {
        fn dim(&self) -> usize {
            self.inner.dim()
        }
        fn model_id(&self) -> &str {
            self.inner.model_id()
        }
        fn embed(
            &mut self,
            texts: &[&str],
        ) -> Result<Vec<Vec<f32>>, crate::zynk::embed::EmbedError> {
            let _ = self.release.recv();
            self.inner.embed(texts)
        }
    }

    /// Blocks inside `embed` until released, then FAILS: models a worker whose provider call
    /// stalled past the lease and comes back with an error after the job changed hands.
    struct FailingAfterRelease {
        inner: FakeEmbedder,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl Embedder for FailingAfterRelease {
        fn dim(&self) -> usize {
            self.inner.dim()
        }
        fn model_id(&self) -> &str {
            self.inner.model_id()
        }
        fn embed(
            &mut self,
            _texts: &[&str],
        ) -> Result<Vec<Vec<f32>>, crate::zynk::embed::EmbedError> {
            let _ = self.release.recv();
            Err(crate::zynk::embed::EmbedError::Provider(
                "stalled provider call failed late".into(),
            ))
        }
    }

    /// Run `process_one_job` for `message_id` on its own connection in a thread.
    fn spawn_job_worker(
        path: std::path::PathBuf,
        job: (String, String, String, String, usize),
        mut embedder: Box<dyn Embedder>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            crate::zynk::db::block_on(async {
                let mut conn = open_migrated_at_without_recovery(&path).await?;
                let (job_id, message_id, model_id, vec_table, dim) = job;
                process_one_job(
                    &mut conn,
                    embedder.as_mut(),
                    &job_id,
                    &message_id,
                    &model_id,
                    &vec_table,
                    dim,
                )
                .await
            })
            .unwrap()
        })
    }

    /// Rows in `message_embeddings` for the message (0 or 1: the table is keyed by message + model).
    fn embedding_rows(path: &std::path::Path, message_id: &str) -> i64 {
        crate::zynk::db::block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false),
            )
            .await?;
            let row =
                sqlx::query("SELECT COUNT(*) AS n FROM message_embeddings WHERE message_id=?")
                    .bind(message_id)
                    .fetch_one(&mut conn)
                    .await?;
            Ok::<i64, DbError>(row.try_get("n")?)
        })
        .unwrap()
    }

    /// `(status, attempts)` of the message's job — `None` while the database or the job row does not
    /// exist yet (the old worker creates both).
    fn job_state(path: &std::path::Path, message_id: &str) -> Option<(String, i64)> {
        crate::zynk::db::block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false),
            )
            .await?;
            let row = sqlx::query("SELECT status, attempts FROM embedding_jobs WHERE message_id=?")
                .bind(message_id)
                .fetch_one(&mut conn)
                .await?;
            Ok::<(String, i64), DbError>((row.try_get("status")?, row.try_get("attempts")?))
        })
        .ok()
    }

    fn wait_for_job_state(path: &std::path::Path, message_id: &str, wanted: (&str, i64)) {
        let started = std::time::Instant::now();
        loop {
            let state = job_state(path, message_id);
            if state.as_ref().map(|(s, a)| (s.as_str(), *a)) == Some(wanted) {
                return;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "job never reached {wanted:?} (now {state:?})"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// An "old worker" thread: seeds a message and processes the batch with a blocking embedder,
    /// returning how many jobs it attempted.
    fn spawn_blocked_old_worker(
        path: std::path::PathBuf,
        release: std::sync::mpsc::Receiver<()>,
    ) -> std::thread::JoinHandle<u32> {
        std::thread::spawn(move || {
            crate::zynk::db::block_on(async {
                let mut conn = open_migrated_at_without_recovery(&path).await?;
                seed_message(&mut conn, "msg_handover").await;
                let mut embedder = BlockingEmbedder {
                    inner: FakeEmbedder::with_dim(8),
                    release,
                };
                let (model_id, vec_table, dim) =
                    ensure_model_and_vec0(&mut conn, &embedder).await?;
                process_pending_batch(&mut conn, &mut embedder, &model_id, &vec_table, dim, 32)
                    .await
            })
            .unwrap()
        })
    }

    /// The replacement worker's startup: recovery of `running` jobs, then one poll sweep.
    fn replacement_startup_sweep(path: &std::path::Path) -> u32 {
        crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(path).await?;
            let mut embedder = FakeEmbedder::with_dim(8);
            let (model_id, vec_table, dim) = ensure_model_and_vec0(&mut conn, &embedder).await?;
            recover_running_jobs(&mut conn, &model_id).await?;
            process_pending_batch(&mut conn, &mut embedder, &model_id, &vec_table, dim, 32).await
        })
        .unwrap()
    }

    #[test]
    fn pause_waits_for_the_in_flight_batch_and_resume_continues() {
        // Codex Gate-2 round 9 (P2): the REAL spawned worker (blocking provider) — `pause` reports
        // busy while a batch is in flight, idle once it finished, and no batch starts until `resume`.
        crate::zynk::embed::vec::register_sqlite_vec();
        let home = std::env::temp_dir().join(format!(
            "zynk-pause-{}-{}",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("t")
        ));
        std::fs::create_dir_all(&home).unwrap();
        let release = home.join("release");
        std::env::set_var("ZYNK_SQLITE_HOME", &home);
        std::env::set_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV, "fake-blocking");
        std::env::set_var(
            crate::zynk::embed::ZYNK_TEST_EMBED_RELEASE_FILE_ENV,
            &release,
        );
        std::env::set_var(ZYNK_EMBED_POLL_MS_ENV, "20");
        let path = home.join("zynk.db");
        crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_pause_1").await;
            Ok::<(), DbError>(())
        })
        .unwrap();
        let worker = spawn();
        wait_for_job_state(&path, "msg_pause_1", ("running", 1));
        assert!(
            !worker.pause(Duration::from_millis(300)),
            "a blocked batch is not idle"
        );
        std::fs::write(&release, b"go").unwrap();
        assert!(
            worker.pause(Duration::from_secs(10)),
            "idle once the batch finished"
        );
        assert_eq!(
            job_state(&path, "msg_pause_1"),
            Some(("done".to_string(), 1))
        );
        // Paused: a new job is NOT picked up ...
        crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_pause_2").await;
            Ok::<(), DbError>(())
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            job_state(&path, "msg_pause_2"),
            Some(("pending".to_string(), 0))
        );
        // ... until resumed.
        worker.resume();
        wait_for_job_state(&path, "msg_pause_2", ("done", 1));
        assert!(worker.pause(Duration::from_secs(10)));
        let started = std::time::Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a paused idle worker joins promptly"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    async fn job_status(conn: &mut SqliteConnection, message_id: &str) -> Result<String, DbError> {
        Ok(
            sqlx::query("SELECT status FROM embedding_jobs WHERE message_id=?")
                .bind(message_id)
                .fetch_one(conn)
                .await?
                .try_get::<String, _>("status")?,
        )
    }

    #[test]
    fn a_job_is_claimed_by_exactly_one_worker() {
        // Gate-3 round 3 (AUD-310-WORKER-001): two workers (two zynk servers sharing the global
        // database) select the same pending job; the claim is an atomic status-guarded UPDATE, so
        // the second worker's claim affects zero rows and it skips the job — one attempt total.
        crate::zynk::embed::vec::register_sqlite_vec();
        std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
        let path = temp_db_path();
        crate::zynk::db::block_on(async {
            let mut a = open_migrated_at_without_recovery(&path).await?;
            let mut b = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut a, "msg_shared").await;
            let mut embedder = FakeEmbedder::with_dim(8);
            let (model_id, vec_table, dim) = ensure_model_and_vec0(&mut a, &embedder).await?;
            let job_id: String = sqlx::query("SELECT id FROM embedding_jobs WHERE message_id=?")
                .bind("msg_shared")
                .fetch_one(&mut a)
                .await?
                .try_get("id")?;
            // Both workers saw the job pending; A runs it first.
            process_one_job(
                &mut a,
                &mut embedder,
                &job_id,
                "msg_shared",
                &model_id,
                &vec_table,
                dim,
            )
            .await?;
            // B's claim of the same (now done) job affects nothing and B does not run it.
            process_one_job(
                &mut b,
                &mut embedder,
                &job_id,
                "msg_shared",
                &model_id,
                &vec_table,
                dim,
            )
            .await?;
            let (status, attempts): (String, i64) = {
                let row = sqlx::query("SELECT status, attempts FROM embedding_jobs WHERE id=?")
                    .bind(&job_id)
                    .fetch_one(&mut a)
                    .await?;
                (row.try_get("status")?, row.try_get("attempts")?)
            };
            assert_eq!(
                (status.as_str(), attempts),
                ("done", 1),
                "exactly one attempt"
            );
            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn a_stale_worker_cannot_fail_a_job_another_worker_is_running() {
        // Gate-3 round 3 (AUD-310-WORKER-001 follow-up): terminal job updates are conditional on
        // OWNERSHIP of the claim, not merely on `status='running'`. Worker A claims and stalls past
        // the lease; recovery hands the job to worker B (still running). A's late failure must not
        // flip B's job to `failed`, and B's completion must land.
        crate::zynk::embed::vec::register_sqlite_vec();
        std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
        let path = temp_db_path();
        let job = crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_owned").await;
            let embedder = FakeEmbedder::with_dim(8);
            let (model_id, vec_table, dim) = ensure_model_and_vec0(&mut conn, &embedder).await?;
            let job_id: String = sqlx::query("SELECT id FROM embedding_jobs WHERE message_id=?")
                .bind("msg_owned")
                .fetch_one(&mut conn)
                .await?
                .try_get("id")?;
            Ok::<_, DbError>((job_id, "msg_owned".to_string(), model_id, vec_table, dim))
        })
        .unwrap();
        let model_id = job.2.clone();

        let (release_a, gate_a) = std::sync::mpsc::channel();
        let worker_a = spawn_job_worker(
            path.clone(),
            job.clone(),
            Box::new(FailingAfterRelease {
                inner: FakeEmbedder::with_dim(8),
                release: gate_a,
            }),
        );
        wait_for_job_state(&path, "msg_owned", ("running", 1));

        // A's claim ages past the lease; recovery returns the job to the queue.
        crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            sqlx::query("UPDATE embedding_jobs SET started_at='1970-01-01T00:00:00Z' WHERE message_id='msg_owned'")
                .execute(&mut conn)
                .await?;
            assert_eq!(recover_running_jobs(&mut conn, &model_id).await?, 1);
            Ok::<(), DbError>(())
        })
        .unwrap();

        let (release_b, gate_b) = std::sync::mpsc::channel();
        let worker_b = spawn_job_worker(
            path.clone(),
            job.clone(),
            Box::new(BlockingEmbedder {
                inner: FakeEmbedder::with_dim(8),
                release: gate_b,
            }),
        );
        wait_for_job_state(&path, "msg_owned", ("running", 2));

        // A comes back late and fails: B still owns the job.
        release_a.send(()).unwrap();
        worker_a.join().unwrap();
        assert_eq!(
            job_state(&path, "msg_owned"),
            Some(("running".into(), 2)),
            "a stale claim failed the job under the current owner"
        );

        release_b.send(()).unwrap();
        worker_b.join().unwrap();
        assert_eq!(job_state(&path, "msg_owned"), Some(("done".into(), 2)));
        let last_error = crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            let row =
                sqlx::query("SELECT last_error FROM embedding_jobs WHERE message_id='msg_owned'")
                    .fetch_one(&mut conn)
                    .await?;
            Ok::<Option<String>, DbError>(row.try_get("last_error")?)
        })
        .unwrap();
        assert_eq!(last_error, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stale_worker_success_never_writes_a_vector_under_the_new_owner() {
        // Gate-3 round 3 pre-read on 79ec55c (arbiter, artifact r79-stale-iMLXNn): the token-guarded
        // `done` UPDATE came AFTER the vector writes, so a stale claim's late SUCCESS committed a
        // vector under the new owner, which then failed on the vec0 UNIQUE. Ownership must be
        // fenced inside the write transaction BEFORE any side effect: a stale success is a no-op.
        crate::zynk::embed::vec::register_sqlite_vec();
        std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
        let path = temp_db_path();
        let job = crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_stale_success").await;
            let embedder = FakeEmbedder::with_dim(8);
            let (model_id, vec_table, dim) = ensure_model_and_vec0(&mut conn, &embedder).await?;
            let job_id: String = sqlx::query("SELECT id FROM embedding_jobs WHERE message_id=?")
                .bind("msg_stale_success")
                .fetch_one(&mut conn)
                .await?
                .try_get("id")?;
            Ok::<_, DbError>((
                job_id,
                "msg_stale_success".to_string(),
                model_id,
                vec_table,
                dim,
            ))
        })
        .unwrap();
        let model_id = job.2.clone();

        let (release_a, gate_a) = std::sync::mpsc::channel();
        let worker_a = spawn_job_worker(
            path.clone(),
            job.clone(),
            Box::new(BlockingEmbedder {
                inner: FakeEmbedder::with_dim(8),
                release: gate_a,
            }),
        );
        wait_for_job_state(&path, "msg_stale_success", ("running", 1));
        crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            sqlx::query("UPDATE embedding_jobs SET started_at='1970-01-01T00:00:00Z' WHERE message_id='msg_stale_success'")
                .execute(&mut conn)
                .await?;
            assert_eq!(recover_running_jobs(&mut conn, &model_id).await?, 1);
            Ok::<(), DbError>(())
        })
        .unwrap();
        let (release_b, gate_b) = std::sync::mpsc::channel();
        let worker_b = spawn_job_worker(
            path.clone(),
            job.clone(),
            Box::new(BlockingEmbedder {
                inner: FakeEmbedder::with_dim(8),
                release: gate_b,
            }),
        );
        wait_for_job_state(&path, "msg_stale_success", ("running", 2));

        // The stale claim succeeds first: nothing may land.
        release_a.send(()).unwrap();
        worker_a.join().unwrap();
        assert_eq!(
            job_state(&path, "msg_stale_success"),
            Some(("running".into(), 2)),
            "a stale success touched the job under the current owner"
        );
        assert_eq!(
            embedding_rows(&path, "msg_stale_success"),
            0,
            "a stale success wrote a vector under the current owner"
        );

        // The current owner completes normally.
        release_b.send(()).unwrap();
        worker_b.join().unwrap();
        assert_eq!(
            job_state(&path, "msg_stale_success"),
            Some(("done".into(), 2))
        );
        assert_eq!(embedding_rows(&path, "msg_stale_success"), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recovery_resets_only_stale_running_jobs() {
        // A job another LIVE worker started moments ago must not be reset; one whose claim is
        // older than the lease (a crashed worker) is recovered.
        crate::zynk::embed::vec::register_sqlite_vec();
        std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
        let path = temp_db_path();
        crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_fresh").await;
            seed_message(&mut conn, "msg_stale").await;
            let embedder = FakeEmbedder::with_dim(8);
            let (model_id, _, _) = ensure_model_and_vec0(&mut conn, &embedder).await?;
            let stale_start = crate::zynk::message::rfc3339_utc(0); // 1970: long past the lease
            sqlx::query("UPDATE embedding_jobs SET status='running', started_at=? WHERE message_id='msg_fresh'")
                .bind(now_rfc3339())
                .execute(&mut conn)
                .await?;
            sqlx::query("UPDATE embedding_jobs SET status='running', started_at=? WHERE message_id='msg_stale'")
                .bind(&stale_start)
                .execute(&mut conn)
                .await?;
            assert_eq!(recover_running_jobs(&mut conn, &model_id).await?, 1, "only the stale one");
            assert_eq!(job_status(&mut conn, "msg_fresh").await?, "running");
            assert_eq!(job_status(&mut conn, "msg_stale").await?, "pending");
            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn a_joined_old_worker_hands_a_blocked_job_over_exactly_once() {
        // Codex Gate-2 round 8 (P2): the live-handoff ORDER — old workers joined BEFORE the
        // replacement recovers/polls — is what keeps a job single-owner. The old worker is blocked
        // inside its job; it is released and joined first; the replacement's startup then finds
        // nothing to recover or process: one attempt, one embedding row.
        crate::zynk::embed::vec::register_sqlite_vec();
        std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
        let path = temp_db_path();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let old = spawn_blocked_old_worker(path.clone(), release_rx);
        wait_for_job_state(&path, "msg_handover", ("running", 1));
        release_tx.send(()).unwrap();
        assert_eq!(
            old.join().unwrap(),
            1,
            "the old worker attempted its one job"
        );
        assert_eq!(
            job_state(&path, "msg_handover"),
            Some(("done".to_string(), 1))
        );
        assert_eq!(
            replacement_startup_sweep(&path),
            0,
            "nothing left for the replacement"
        );
        assert_eq!(
            job_state(&path, "msg_handover"),
            Some(("done".to_string(), 1))
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unjoined_old_worker_double_processes_a_blocked_job() {
        // The hazard the protocol ordering prevents (negative control): a replacement that starts
        // while an old worker still owns a `running` job re-runs it as soon as that claim looks
        // stale (older than RUNNING_LEASE) — two attempts for one job. Inside the lease the claim
        // is left alone (`recovery_resets_only_stale_running_jobs`), and the old worker's late
        // completion no longer lands on top of the new result (claim token, `attempts`).
        crate::zynk::embed::vec::register_sqlite_vec();
        std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
        let path = temp_db_path();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let old = spawn_blocked_old_worker(path.clone(), release_rx);
        wait_for_job_state(&path, "msg_handover", ("running", 1));
        crate::zynk::db::block_on(async {
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            sqlx::query("UPDATE embedding_jobs SET started_at='1970-01-01T00:00:00Z' WHERE message_id='msg_handover'")
                .execute(&mut conn)
                .await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        assert_eq!(
            replacement_startup_sweep(&path),
            1,
            "the replacement re-ran the stale-looking job"
        );
        assert_eq!(
            job_state(&path, "msg_handover"),
            Some(("done".to_string(), 2)),
            "two attempts for one job"
        );
        release_tx.send(()).unwrap();
        let _ = old.join().unwrap();
        assert_eq!(
            job_state(&path, "msg_handover"),
            Some(("done".to_string(), 2))
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn worker_embeds_pending_job_to_done() {
        crate::zynk::embed::vec::register_sqlite_vec();
        crate::zynk::db::block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            let rowid = seed_message(&mut conn, "msg_embed").await;

            // dim 8 fake; model_id is "fake@1" (matches active_model_id() default).
            let mut embedder = FakeEmbedder::with_dim(8);
            let (model_id, vec_table, dim) = ensure_model_and_vec0(&mut conn, &embedder).await?;
            assert_eq!(model_id, "fake@1");
            assert_eq!(dim, 8);

            let processed =
                process_pending_batch(&mut conn, &mut embedder, &model_id, &vec_table, dim, 32)
                    .await?;
            assert_eq!(processed, 1, "one runnable job processed");

            // Job is done.
            let status: String =
                sqlx::query("SELECT status FROM embedding_jobs WHERE message_id=? AND model_id=?")
                    .bind("msg_embed")
                    .bind(&model_id)
                    .fetch_one(&mut conn)
                    .await?
                    .try_get("status")?;
            assert_eq!(status, "done");

            // A vec0 row exists for that rowid.
            let vec_count: i64 = sqlx::query(&format!(
                "SELECT COUNT(*) AS c FROM {vec_table} WHERE message_rowid=?"
            ))
            .bind(rowid)
            .fetch_one(&mut conn)
            .await?
            .try_get("c")?;
            assert_eq!(vec_count, 1, "vec0 row exists for the message rowid");

            // message_embeddings row exists with the right model_id + vec_rowid.
            let me_row = sqlx::query(
                "SELECT model_id, vec_rowid FROM message_embeddings WHERE message_id=?",
            )
            .bind("msg_embed")
            .fetch_one(&mut conn)
            .await?;
            assert_eq!(me_row.try_get::<String, _>("model_id")?, "fake@1");
            assert_eq!(me_row.try_get::<i64, _>("vec_rowid")?, rowid);

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn worker_retries_failed_job() {
        crate::zynk::embed::vec::register_sqlite_vec();
        crate::zynk::db::block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_retry").await;

            // FakeEmbedder::failing_then_ok(1) is dim 384; align the vec0 table to it.
            let mut embedder = FakeEmbedder::failing_then_ok(1);
            let (model_id, vec_table, dim) = ensure_model_and_vec0(&mut conn, &embedder).await?;

            // First pass: the embed fails → job 'failed', attempts>=1, no embedding row.
            process_pending_batch(&mut conn, &mut embedder, &model_id, &vec_table, dim, 32).await?;
            let (status1, attempts1): (String, i64) = {
                let row =
                    sqlx::query("SELECT status, attempts FROM embedding_jobs WHERE message_id=?")
                        .bind("msg_retry")
                        .fetch_one(&mut conn)
                        .await?;
                (row.try_get("status")?, row.try_get("attempts")?)
            };
            assert_eq!(status1, "failed");
            assert!(attempts1 >= 1, "attempts incremented on the failed try");
            let me_count1: i64 =
                sqlx::query("SELECT COUNT(*) AS c FROM message_embeddings WHERE message_id=?")
                    .bind("msg_retry")
                    .fetch_one(&mut conn)
                    .await?
                    .try_get("c")?;
            assert_eq!(me_count1, 0, "no embedding row on the failed try");

            // Second pass: the same embedder now succeeds → 'done', attempts++, row present.
            process_pending_batch(&mut conn, &mut embedder, &model_id, &vec_table, dim, 32).await?;
            let (status2, attempts2): (String, i64) = {
                let row =
                    sqlx::query("SELECT status, attempts FROM embedding_jobs WHERE message_id=?")
                        .bind("msg_retry")
                        .fetch_one(&mut conn)
                        .await?;
                (row.try_get("status")?, row.try_get("attempts")?)
            };
            assert_eq!(status2, "done");
            assert!(
                attempts2 > attempts1,
                "attempts incremented again on the successful retry"
            );
            let me_count2: i64 =
                sqlx::query("SELECT COUNT(*) AS c FROM message_embeddings WHERE message_id=?")
                    .bind("msg_retry")
                    .fetch_one(&mut conn)
                    .await?
                    .try_get("c")?;
            assert_eq!(
                me_count2, 1,
                "embedding row present after the retry succeeds"
            );

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn crash_recovery_resets_running_to_pending() {
        crate::zynk::embed::vec::register_sqlite_vec();
        crate::zynk::db::block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_crash").await;

            // Force the job into 'running' with a started_at (simulate a dead process).
            sqlx::query(
                "UPDATE embedding_jobs SET status='running', started_at=? WHERE message_id=?",
            )
            .bind("2026-06-14T00:00:01Z")
            .bind("msg_crash")
            .execute(&mut conn)
            .await?;

            let reset = recover_running_jobs(&mut conn, "fake@1").await?;
            assert_eq!(reset, 1, "the running job was reset");

            let row =
                sqlx::query("SELECT status, started_at FROM embedding_jobs WHERE message_id=?")
                    .bind("msg_crash")
                    .fetch_one(&mut conn)
                    .await?;
            assert_eq!(row.try_get::<String, _>("status")?, "pending");
            assert!(
                row.try_get::<Option<String>, _>("started_at")?.is_none(),
                "started_at cleared on recovery"
            );

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn backfill_enqueues_jobs_for_messages_without_one() {
        crate::zynk::embed::vec::register_sqlite_vec();
        crate::zynk::db::block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_backfill").await;

            // Simulate a pre-worker message: delete its B3-enqueued job.
            sqlx::query("DELETE FROM embedding_jobs WHERE message_id=?")
                .bind("msg_backfill")
                .execute(&mut conn)
                .await?;
            let before: i64 =
                sqlx::query("SELECT COUNT(*) AS c FROM embedding_jobs WHERE message_id=?")
                    .bind("msg_backfill")
                    .fetch_one(&mut conn)
                    .await?
                    .try_get("c")?;
            assert_eq!(before, 0, "no job before backfill");

            let enqueued = backfill_missing(&mut conn, "fake@1", 256).await?;
            assert_eq!(enqueued, 1, "backfill enqueued one job");

            let row =
                sqlx::query("SELECT status FROM embedding_jobs WHERE message_id=? AND model_id=?")
                    .bind("msg_backfill")
                    .bind("fake@1")
                    .fetch_one(&mut conn)
                    .await?;
            assert_eq!(row.try_get::<String, _>("status")?, "pending");

            // Idempotent: a second pass finds nothing to enqueue.
            let again = backfill_missing(&mut conn, "fake@1", 256).await?;
            assert_eq!(again, 0, "backfill is idempotent");

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn process_pending_batch_leaves_no_job_running() {
        // The no-strand invariant (review #1): after a batch, EVERY job is in a
        // terminal-or-retryable state (done/failed/pending) — NEVER stuck `running`.
        // A `running` job is never re-selected, so a stranded one would block forever
        // until a process restart. We seed two jobs and drive a SINGLE batch with a
        // `failing_then_ok(1)` embedder so ONE job hits the recorded-failure path
        // (embed Err → marked `failed`) and the OTHER succeeds (→ `done`): both must
        // leave `running`. A transient DB error mid-step is hard to inject cleanly,
        // but the restructure routes it through the SAME `mark_job_failed` path (see
        // `process_one_job`'s `JobError::Db` arm), so asserting "no job is `running`"
        // after a batch that exercises the failure path covers the structural
        // guarantee both kinds of failure share.
        crate::zynk::embed::vec::register_sqlite_vec();
        crate::zynk::db::block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;

            // Two healthy messages → two pending jobs (enqueued at the same created_at;
            // batch order is by enqueued_at, so exactly one of them takes the injected
            // first-call failure and the other the subsequent success).
            seed_message(&mut conn, "msg_a").await;
            seed_message(&mut conn, "msg_b").await;

            let mut embedder = FakeEmbedder::failing_then_ok(1);
            let (model_id, vec_table, dim) = ensure_model_and_vec0(&mut conn, &embedder).await?;
            let processed =
                process_pending_batch(&mut conn, &mut embedder, &model_id, &vec_table, dim, 32)
                    .await?;
            assert_eq!(processed, 2, "both runnable jobs were attempted");

            // THE invariant: not a single job is left `running` after the batch.
            let running: i64 =
                sqlx::query("SELECT COUNT(*) AS c FROM embedding_jobs WHERE status='running'")
                    .fetch_one(&mut conn)
                    .await?
                    .try_get("c")?;
            assert_eq!(running, 0, "no job may be left in 'running' after a batch");

            // Both jobs reached a terminal-or-retryable state: exactly one done, one
            // failed (the injected first-call failure), neither stuck.
            let done: i64 =
                sqlx::query("SELECT COUNT(*) AS c FROM embedding_jobs WHERE status='done'")
                    .fetch_one(&mut conn)
                    .await?
                    .try_get("c")?;
            let failed: i64 =
                sqlx::query("SELECT COUNT(*) AS c FROM embedding_jobs WHERE status='failed'")
                    .fetch_one(&mut conn)
                    .await?
                    .try_get("c")?;
            assert_eq!(done, 1, "one job succeeded");
            assert_eq!(failed, 1, "one job recorded a failure (not stranded)");

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn vec_table_for_is_a_safe_identifier() {
        let table = vec_table_for("fake@1");
        assert_eq!(table, "message_vec_fake_1");
        assert!(
            !table.is_empty() && table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "vec_table must be a safe ascii alnum/underscore identifier: {table}"
        );
        // The real model id maps to a safe identifier too.
        let real = vec_table_for("multilingual-e5-small@1");
        assert_eq!(real, "message_vec_multilingual_e5_small_1");
        assert!(real.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }

    #[test]
    fn spawn_then_drop_is_clean() {
        // Smoke test of the thread + Drop-join: spawn and immediately drop. It must
        // return without panic or hang regardless of whether it found the live DB.
        let handle = spawn();
        drop(handle);
    }
}
