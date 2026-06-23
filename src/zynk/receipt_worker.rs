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
                return Err(DbError::new(
                    "receipt_worker_busy",
                    "receipt worker queue is full; the receipt was not enqueued — retry to resolve the true state",
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
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
        // drains in-flight work — so `join` is bounded by the current job (SQLite
        // busy_timeout), never an indefinite block on a full queue. Never touches
        // live Zynk state.
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
    let join = std::thread::Builder::new()
        .name("zynk-receipt-worker".to_string())
        .spawn(move || worker_loop(receiver))
        .expect("spawn zynk receipt worker thread");
    ReceiptWorkerHandle {
        sender: Some(sender),
        join: Some(join),
    }
}

fn worker_loop(receiver: Receiver<WorkerMessage>) {
    // One current-thread runtime owns all DB work for this worker — safe because
    // this is a plain std::thread with no ambient Tokio runtime.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            // Cannot build a runtime: drain and fail every job so callers don't hang.
            while let Ok(message) = receiver.recv() {
                match message {
                    WorkerMessage::Shutdown => break,
                    WorkerMessage::Job(job) => {
                        let _ = job.respond_to.send(Err(DbError::new(
                            "tokio_runtime_failed",
                            "receipt worker could not build its Tokio runtime",
                        )));
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

    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Shutdown => break,
            WorkerMessage::Job(job) => {
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
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        drop(_receiver);
    }

    #[test]
    fn submit_without_sender_is_unavailable() {
        let handle = ReceiptWorkerHandle {
            sender: None,
            join: None,
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
}
