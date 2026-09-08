//! zynk fork: App-owned receipt DB worker (M3a §D6).
//!
//! The receipt handler runs synchronously inside the server/App API path. It must
//! NOT call `crate::zynk::db::block_on()` (which builds a fresh Tokio runtime per
//! call) from inside the server's multi-thread runtime, and `block_in_place` would
//! deadlock under the current-thread runtimes some tests use. So receipt DB writes
//! are delegated to a dedicated `std::thread` that owns ONE current-thread Tokio
//! runtime and a single reused SQLite connection. The synchronous handler sends a
//! work item over a bounded channel and waits for the result with a bounded
//! timeout; a timeout surfaces as `receipt_result_unknown` (the commit may or may
//! not have landed — a retry resolves the true state via idempotency).
//!
//! The worker only ever opens the native zynk DB; it never touches live Zynk
//! state. Dropping the handle stops the worker and joins the thread.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::zynk::db::DbError;
use crate::zynk::receipt::{
    append_received_event, AuthoritativeReceiver, ReceiptAccepted, ReceiptRequest,
};

/// Default bound on how long the synchronous handler blocks waiting for the
/// worker. Normal receipt writes complete in well under a millisecond; this caps
/// the worst case (a wedged worker) without blocking the server loop indefinitely.
pub const DEFAULT_RECEIPT_TIMEOUT: Duration = Duration::from_secs(5);

/// Env override (milliseconds, > 0) for how long the API handler waits for a receipt result
/// (mirrors the other env-tunable worker knobs).
pub const ZYNK_RECEIPT_TIMEOUT_MS_ENV: &str = "ZYNK_RECEIPT_TIMEOUT_MS";

/// Test hook: when set, the worker blocks before EVERY job until the file at this path exists —
/// lets a test build a receipt backlog deterministically. Never used in production.
pub const ZYNK_TEST_RECEIPT_BLOCK_FILE_ENV: &str = "ZYNK_TEST_RECEIPT_BLOCK_FILE";

/// The API handler's receipt wait: `ZYNK_RECEIPT_TIMEOUT_MS` or `DEFAULT_RECEIPT_TIMEOUT`.
pub fn receipt_timeout() -> Duration {
    std::env::var(ZYNK_RECEIPT_TIMEOUT_MS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_RECEIPT_TIMEOUT)
}

struct ReceiptJob {
    request: ReceiptRequest,
    receiver: AuthoritativeReceiver,
    current_socket_namespace: String,
    current_runtime_id: String,
    now: String,
    respond_to: SyncSender<Result<ReceiptAccepted, DbError>>,
}

enum WorkerMessage {
    Job(Box<ReceiptJob>),
    Shutdown,
}

/// Handle to the App-owned receipt DB worker thread. Owned by the headless server;
/// `None` in CLI / unit-test App constructors.
pub struct ReceiptWorkerHandle {
    sender: Option<SyncSender<WorkerMessage>>,
    join: Option<JoinHandle<()>>,
    control: std::sync::Arc<crate::zynk::worker_pause::PauseControl>,
}

// The live handoff (the only caller) is Unix-only.
impl ReceiptWorkerHandle {
    /// Stop ACCEPTING receipt jobs and wait (bounded) until the in-flight one and every job already
    /// queued have finished. `false` = still draining past `deadline` (the worker keeps running;
    /// call `resume` to accept jobs again). Live-handoff quiescence (Codex Gate-2 rounds 9-11).
    pub fn pause(&self, deadline: Duration) -> bool {
        self.control.pause(deadline)
    }

    pub fn resume(&self) {
        self.control.resume();
    }
}

impl ReceiptWorkerHandle {
    /// Submit a receipt for processing and block (bounded) for the result.
    /// `receipt_worker_unavailable` if the worker is gone; `receipt_result_unknown`
    /// on timeout (do not claim failure — the DB write may have landed).
    pub fn submit(
        &self,
        request: ReceiptRequest,
        receiver: AuthoritativeReceiver,
        current_socket_namespace: String,
        current_runtime_id: String,
        now: String,
        timeout: Duration,
    ) -> Result<ReceiptAccepted, DbError> {
        let Some(sender) = self.sender.as_ref() else {
            return Err(DbError::new(
                "receipt_worker_unavailable",
                "receipt worker is not running",
            ));
        };
        // A paused worker (live handoff quiescing) accepts nothing new: the receipt is NOT enqueued
        // and thus NOT written — a transient, retryable `receipt_worker_busy`. Admission is counted
        // under the pause mutex BEFORE the channel hand-off, so a pause sees this job as outstanding
        // from this point until the worker takes it into flight.
        if !self.control.try_enqueue() {
            return Err(DbError::new(
                "receipt_worker_busy",
                "a live handoff is quiescing the receipt worker; the receipt was not enqueued — retry to resolve the true state",
            ));
        }
        let (respond_to, response) = std::sync::mpsc::sync_channel(1);
        let job = ReceiptJob {
            request,
            receiver,
            current_socket_namespace,
            current_runtime_id,
            now,
            respond_to,
        };

        // NON-blocking enqueue: a blocking `SyncSender::send` would wait for queue
        // capacity on a full/wedged channel BEFORE `recv_timeout` ever starts,
        // hanging the synchronous API handler past the bounded timeout. `try_send`
        // never blocks — a saturated queue means the receipt was NOT enqueued (and
        // thus NOT written), so it is a transient, retryable `receipt_worker_busy`.
        match sender.try_send(WorkerMessage::Job(Box::new(job))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.control.unenqueue();
                return Err(DbError::new(
                    "receipt_worker_busy",
                    "receipt worker queue is full; the receipt was not enqueued — retry to resolve the true state",
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                self.control.unenqueue();
                return Err(DbError::new(
                    "receipt_worker_unavailable",
                    "receipt worker is not running",
                ));
            }
        }
        match response.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(DbError::new(
                "receipt_result_unknown",
                "receipt worker did not respond within the timeout; the DB write may or may not have landed — retry to resolve the true state",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(DbError::new(
                "receipt_worker_unavailable",
                "receipt worker disconnected before responding",
            )),
        }
    }
}

impl Drop for ReceiptWorkerHandle {
    fn drop(&mut self) {
        // NON-blocking shutdown: `try_send` can't hang on a full queue. Then drop
        // the sender so the worker's `recv()` disconnects and the loop exits once it
        // has drained what was already queued (ordinary shutdown may drain several
        // jobs, each bounded by its DB timeouts). A live handoff never reaches this
        // drop with a non-empty queue: its pause was acknowledged only once nothing
        // was in flight or queued, so that join is prompt. Never touches live Zynk state.
        self.control.stop();
        if let Some(sender) = self.sender.take() {
            let _ = sender.try_send(WorkerMessage::Shutdown);
            drop(sender);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Spawn the receipt DB worker thread. Call once at server startup; install the
/// returned handle on `App`.
pub fn spawn() -> ReceiptWorkerHandle {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<WorkerMessage>(64);
    let control = std::sync::Arc::new(crate::zynk::worker_pause::PauseControl::new());
    let loop_control = std::sync::Arc::clone(&control);
    let join = std::thread::Builder::new()
        .name("zynk-receipt-worker".to_string())
        .spawn(move || worker_loop(receiver, loop_control))
        .expect("spawn zynk receipt worker thread");
    ReceiptWorkerHandle {
        sender: Some(sender),
        join: Some(join),
        control,
    }
}

fn worker_loop(
    receiver: Receiver<WorkerMessage>,
    control: std::sync::Arc<crate::zynk::worker_pause::PauseControl>,
) {
    // Startup is WORK (Codex Gate-2 round 10): the loop reports idle only once its initial DB open
    // completed or failed (a failed open is retried per job), so a live-handoff pause is never
    // acknowledged while this worker still waits on the init lock or a busy database.
    // One current-thread runtime owns all DB work for this worker — safe because
    // this is a plain std::thread with no ambient Tokio runtime.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            control.set_idle();
            // Cannot build a runtime: drain and fail every job so callers don't hang.
            while let Ok(message) = receiver.recv() {
                match message {
                    WorkerMessage::Shutdown => break,
                    WorkerMessage::Job(job) => {
                        control.begin_work();
                        let _ = job.respond_to.send(Err(DbError::new(
                            "tokio_runtime_failed",
                            "receipt worker could not build its Tokio runtime",
                        )));
                        control.end_work();
                    }
                }
            }
            return;
        }
    };

    // Open the native DB once and reuse the connection for all receipt writes
    // (the worker processes jobs serially). Open without recovery — the receipt
    // path never runs orphan recovery (M3a §D5).
    let mut conn = rt
        .block_on(crate::zynk::db::open_migrated_for_append())
        .ok();
    control.set_idle();

    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Shutdown => break,
            WorkerMessage::Job(job) => {
                // Test hook: hold the job here — taken from the queue but not yet in flight (the
                // state a live-handoff pause must still count as outstanding work).
                if let Some(block_file) = std::env::var_os(ZYNK_TEST_RECEIPT_BLOCK_FILE_ENV) {
                    while !std::path::Path::new(&block_file).exists() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                // Take the job into flight (the pause accounting drops it from "queued" here).
                // Already-accepted jobs DRAIN even while paused: a live-handoff pause is
                // acknowledged only once nothing is in flight and nothing is queued, and a paused
                // handle accepts nothing new (`submit` → receipt_worker_busy).
                control.begin_work();
                if conn.is_none() {
                    conn = rt
                        .block_on(crate::zynk::db::open_migrated_for_append())
                        .ok();
                }
                let result = match conn.as_mut() {
                    Some(c) => rt.block_on(append_received_event(
                        c,
                        &job.request,
                        &job.receiver,
                        &job.current_socket_namespace,
                        &job.current_runtime_id,
                        &job.now,
                    )),
                    None => Err(DbError::new(
                        "db_error",
                        "receipt worker could not open the native database",
                    )),
                };
                let _ = job.respond_to.send(result);
                control.end_work();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!(
            "zynk-receipt-{tag}-{}-{}",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("t")
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("ZYNK_SQLITE_HOME", &home);
        home
    }

    #[test]
    fn receipt_worker_startup_is_busy_until_its_db_open_completes() {
        // Codex Gate-2 round 10 (P2): the startup DB open is work — a pause must not be acknowledged
        // while the worker is still opening/migrating (here: waiting on a held init lock), or a
        // "quiescent" drop would block on that startup. Once the lock is released the open completes,
        // the worker idles, and the drop is prompt.
        let home = temp_home("startup-busy");
        let db = home.join("zynk.db");
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.join("zynk.db.init-lock"))
            .unwrap();
        holder.lock().unwrap();
        let worker = spawn();
        assert!(
            !worker.pause(Duration::from_millis(300)),
            "startup must count as busy while the DB open waits on the init lock"
        );
        holder.unlock().unwrap();
        assert!(
            worker.pause(Duration::from_secs(15)),
            "idle once the open completed"
        );
        assert!(
            db.exists(),
            "the worker initialized its database after the lock was released"
        );
        let started = std::time::Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an idle worker joins promptly"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn receipt_worker_startup_error_path_reaches_idle() {
        // Startup error path: a foreign database makes the initial open fail fast; the worker then
        // idles (it will retry per job) and is pausable/droppable promptly.
        let home = temp_home("startup-error");
        let db = home.join("zynk.db");
        crate::zynk::db::block_on(async {
            use sqlx::{Connection, Executor};
            let mut conn = sqlx::SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&db)
                    .create_if_missing(true),
            )
            .await?;
            conn.execute("CREATE TABLE projects (id TEXT PRIMARY KEY)")
                .await?;
            conn.close().await?;
            Ok::<(), DbError>(())
        })
        .unwrap();
        let before = std::fs::read(&db).unwrap();
        let worker = spawn();
        assert!(
            worker.pause(Duration::from_secs(10)),
            "idle after the failed startup open"
        );
        let started = std::time::Instant::now();
        drop(worker);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            std::fs::read(&db).unwrap(),
            before,
            "the foreign database was touched"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    fn submit_in_background(
        worker: &std::sync::Arc<ReceiptWorkerHandle>,
        timeout: Duration,
    ) -> std::thread::JoinHandle<Result<ReceiptAccepted, DbError>> {
        let worker = std::sync::Arc::clone(worker);
        std::thread::spawn(move || {
            worker.submit(
                dummy_request(),
                dummy_receiver(),
                "s".into(),
                "rt".into(),
                "now".into(),
                timeout,
            )
        })
    }

    #[test]
    fn receipt_worker_pause_counts_queued_and_prefetched_jobs() {
        // Codex Gate-2 round 11 (P2): receipts whose submitters already timed out can still sit in
        // the queue (or be taken from it but not yet in flight) while the loop is idle. A pause
        // must not be acknowledged until they drained (bounded), a paused worker accepts nothing
        // new, and resume reopens admission — so the commit-time join never starts work after
        // service was withdrawn. The block hook holds each job between "taken" and "in flight".
        let home = temp_home("queue-drain");
        let db = home.join("zynk.db");
        crate::zynk::db::block_on(crate::zynk::db::open_migrated_at_without_recovery(&db)).unwrap();
        let release = home.join("release-receipts");
        std::env::set_var(ZYNK_TEST_RECEIPT_BLOCK_FILE_ENV, &release);
        let worker = std::sync::Arc::new(spawn());
        assert!(worker.pause(Duration::from_secs(10)), "idle after startup");
        worker.resume();
        // Three receipts whose submitters give up quickly: one is taken and held before it becomes
        // in flight, two stay queued — all three are outstanding work.
        let submitters: Vec<_> = (0..3)
            .map(|_| submit_in_background(&worker, Duration::from_millis(300)))
            .collect();
        for submitter in submitters {
            let err = submitter.join().unwrap().unwrap_err();
            assert_eq!(err.code, "receipt_result_unknown", "{}", err.message);
        }
        assert!(
            !worker.pause(Duration::from_millis(500)),
            "queued/prefetched receipts are not quiescent"
        );
        // Paused: nothing new is accepted ...
        let refused = submit_in_background(&worker, Duration::from_secs(5))
            .join()
            .unwrap()
            .unwrap_err();
        assert_eq!(refused.code, "receipt_worker_busy", "{}", refused.message);
        assert!(refused.message.contains("quiescing"), "{}", refused.message);
        // ... and once released the backlog drains within the bound.
        std::fs::write(&release, b"go").unwrap();
        assert!(worker.pause(Duration::from_secs(30)), "drained: quiescent");
        worker.resume();
        let after = submit_in_background(&worker, Duration::from_secs(10))
            .join()
            .unwrap()
            .map(|_| "ok".to_string())
            .unwrap_or_else(|err| err.code.to_string());
        assert_ne!(after, "receipt_worker_busy", "accepting again after resume");
        let worker = std::sync::Arc::try_unwrap(worker).ok().expect("sole owner");
        let started = std::time::Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an idle worker joins promptly"
        );
        std::env::remove_var(ZYNK_TEST_RECEIPT_BLOCK_FILE_ENV);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn receipt_worker_pauses_when_idle_and_drops_promptly() {
        // Live-handoff quiescence: an idle receipt worker acknowledges a pause immediately and a
        // paused handle still joins promptly when dropped (bounded commit-time join).
        let home = std::env::temp_dir().join(format!(
            "zynk-receipt-pause-{}-{}",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("t")
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("ZYNK_SQLITE_HOME", &home);
        let worker = spawn();
        assert!(
            worker.pause(Duration::from_secs(5)),
            "an idle worker is idle"
        );
        let started = std::time::Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "paused drop must not hang"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
    use std::time::Instant;

    fn dummy_request() -> ReceiptRequest {
        ReceiptRequest {
            message_id: "m".into(),
            conversation_id: "c".into(),
            conversation_seq: 1,
            runtime_session_id: "rt".into(),
            socket_namespace: "s".into(),
            receiver_seq: None,
            timestamp: None,
            status: None,
            receiver_agent_session_hint: None,
        }
    }

    fn dummy_receiver() -> AuthoritativeReceiver {
        AuthoritativeReceiver {
            pane_id: "p".into(),
            terminal_id: "t".into(),
            agent_label: "a".into(),
            agent_session: None,
        }
    }

    #[test]
    fn submit_does_not_block_when_queue_is_full() {
        // A handle whose channel is already full, with no worker draining it: the
        // enqueue must fail FAST (`receipt_worker_busy`), never block past the
        // timeout. We pass a long 30s timeout and require a near-instant return,
        // proving enqueue does not wait on queue capacity (the ARB-M3A-001 fix).
        let (sender, _receiver) = std::sync::mpsc::sync_channel::<WorkerMessage>(1);
        sender.try_send(WorkerMessage::Shutdown).unwrap(); // saturate the capacity-1 channel
        let handle = ReceiptWorkerHandle {
            sender: Some(sender),
            join: None,
            control: std::sync::Arc::new(crate::zynk::worker_pause::PauseControl::new()),
        };

        let start = Instant::now();
        let err = handle
            .submit(
                dummy_request(),
                dummy_receiver(),
                "s".into(),
                "rt".into(),
                "now".into(),
                Duration::from_secs(30),
            )
            .unwrap_err();
        let elapsed = start.elapsed();
        assert_eq!(err.code, "receipt_worker_busy");
        assert!(
            elapsed < Duration::from_secs(1),
            "submit blocked on a full queue instead of returning fast: {elapsed:?}"
        );
        // The rejected job must not leave a reservation behind: with the (imaginary) loop idle,
        // a pause is acknowledged at once.
        handle.control.set_idle();
        assert!(
            handle.pause(Duration::from_millis(10)),
            "a Full rejection leaked a queued reservation"
        );
        drop(_receiver);
    }

    #[test]
    fn submit_without_sender_is_unavailable() {
        let handle = ReceiptWorkerHandle {
            sender: None,
            join: None,
            control: std::sync::Arc::new(crate::zynk::worker_pause::PauseControl::new()),
        };
        let err = handle
            .submit(
                dummy_request(),
                dummy_receiver(),
                "s".into(),
                "rt".into(),
                "now".into(),
                Duration::from_millis(10),
            )
            .unwrap_err();
        assert_eq!(err.code, "receipt_worker_unavailable");
    }

    #[test]
    fn submit_when_worker_disconnected_is_unavailable() {
        // Receiver dropped → the channel is disconnected → `try_send` returns
        // Disconnected → `receipt_worker_unavailable`, fast and non-blocking.
        let (sender, receiver) = std::sync::mpsc::sync_channel::<WorkerMessage>(1);
        drop(receiver);
        let handle = ReceiptWorkerHandle {
            sender: Some(sender),
            join: None,
            control: std::sync::Arc::new(crate::zynk::worker_pause::PauseControl::new()),
        };
        let err = handle
            .submit(
                dummy_request(),
                dummy_receiver(),
                "s".into(),
                "rt".into(),
                "now".into(),
                Duration::from_millis(10),
            )
            .unwrap_err();
        assert_eq!(err.code, "receipt_worker_unavailable");
        handle.control.set_idle();
        assert!(
            handle.pause(Duration::from_millis(10)),
            "a Disconnected rejection leaked a queued reservation"
        );
    }
}
