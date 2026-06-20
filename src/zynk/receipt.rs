//! zynk fork: native receipt acceptance (M3a).
//!
//! Implements the binding ADR 0002 §Decision 4 receipt-acceptance invariants and
//! the M3a plan §D3/§D5. A `zynk.message_received` report advances delivery to
//! `received` ONLY when, inside one `BEGIN IMMEDIATE` transaction:
//!   1. the message exists and its stored message-triple + runtime namespace match
//!      the receipt, and the current server socket namespace matches too;
//!   2. the AUTHORITATIVE (hook-derived, never detection) receiver identity equals
//!      the stored target participant's `agent_label`, and the receiver is not the
//!      message's own sender (self-receipt is rejected);
//!   3. the latest delivery state is `submitted` (idempotent `already_received`
//!      once a `received` event exists; `drafted`/`failed`/orphan rejected).
//!
//! The wire header is NOT proof and is not parsed here. Receiver identity is resolved
//! by the App API handler from live hook-authority state; the integration-supplied
//! `receiver_agent_session` is a debugging hint only and is never trusted for any
//! control-path decision.

use sqlx::{Executor, Row, SqliteConnection};

use crate::zynk::db::DbError;
use crate::zynk::persistence::{
    append_delivery_event_in_transaction, DeliveryEventInput, DeliveryEventType,
};

/// Authoritative receiver identity resolved by the App API handler from live
/// hook-authority terminal state (never `effective_agent_label()`'s detection
/// fallback). This is the only identity trusted for control-path decisions.
#[derive(Clone, Debug)]
pub struct AuthoritativeReceiver {
    pub pane_id: String,
    pub agent_label: String,
    pub agent_session: Option<serde_json::Value>,
}

/// A decoded receipt request. The protocol triple + runtime namespace are quoted
/// by the receiver (from the sender's F4 response / the wire header); they are
/// validated against the authoritative stored message row, never trusted blindly.
#[derive(Clone, Debug)]
pub struct ReceiptRequest {
    pub message_id: String,
    pub conversation_id: String,
    pub conversation_seq: i64,
    pub runtime_session_id: String,
    pub socket_namespace: String,
    /// Integration audit/debug metadata only; never used as `delivery_events.seq`.
    pub receiver_seq: Option<i64>,
    pub timestamp: Option<String>,
    pub status: Option<String>,
    /// Integration-supplied hint; NEVER trusted for control-path decisions.
    pub receiver_agent_session_hint: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    Received,
    AlreadyReceived,
}

impl ReceiptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::AlreadyReceived => "already_received",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReceiptAccepted {
    pub status: ReceiptStatus,
    pub message_id: String,
    pub conversation_id: String,
    pub conversation_seq: i64,
    pub receiver_pane_id: String,
    pub receiver_agent_label: String,
}

/// Validate a receipt and, if valid and not already received, append exactly one
/// `received` delivery event in a single `BEGIN IMMEDIATE` transaction.
///
/// `current_socket_namespace` is the server's active socket path; receipts never
/// cross sockets (dev/live isolation). `current_runtime_id` is recorded in the
/// payload for audit but is deliberately NOT a rejection condition when it differs
/// from the message's stored runtime (M3a allows post-restart receipts on the same
/// socket namespace). All rejections fail closed with structured `DbError` codes
/// and append nothing.
pub async fn append_received_event(
    conn: &mut SqliteConnection,
    request: &ReceiptRequest,
    receiver: &AuthoritativeReceiver,
    current_socket_namespace: &str,
    current_runtime_id: &str,
    now: &str,
) -> Result<ReceiptAccepted, DbError> {
    // Dev/live isolation: the current server socket namespace must match the
    // receipt's quoted namespace before we touch the DB.
    if request.socket_namespace != current_socket_namespace {
        return Err(DbError::new(
            "socket_namespace_mismatch",
            format!(
                "receipt socket namespace {} does not match the current server socket {}",
                request.socket_namespace, current_socket_namespace
            ),
        ));
    }

    conn.execute("BEGIN IMMEDIATE").await?;
    let result =
        append_received_event_in_tx(conn, request, receiver, current_runtime_id, now).await;
    match result {
        Ok(accepted) => {
            conn.execute("COMMIT").await?;
            Ok(accepted)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK").await;
            Err(err)
        }
    }
}

async fn append_received_event_in_tx(
    conn: &mut SqliteConnection,
    request: &ReceiptRequest,
    receiver: &AuthoritativeReceiver,
    current_runtime_id: &str,
    now: &str,
) -> Result<ReceiptAccepted, DbError> {
    let row = sqlx::query(
        "SELECT m.conversation_id AS conversation_id, \
                m.conversation_seq AS conversation_seq, \
                m.runtime_session_id AS runtime_session_id, \
                m.socket_namespace AS socket_namespace, \
                fp.pane_id AS from_pane_id, \
                tp.agent_label AS to_agent_label \
         FROM messages m \
         JOIN conversation_participants fp ON fp.id = m.from_participant_id \
         JOIN conversation_participants tp ON tp.id = m.to_participant_id \
         WHERE m.id = ?",
    )
    .bind(&request.message_id)
    .fetch_optional(&mut *conn)
    .await?;

    let Some(row) = row else {
        return Err(DbError::new(
            "message_not_found",
            format!("no message {} in the native store", request.message_id),
        ));
    };

    let stored_conversation_id = row.try_get::<String, _>("conversation_id")?;
    let stored_conversation_seq = row.try_get::<i64, _>("conversation_seq")?;
    let stored_runtime = row.try_get::<String, _>("runtime_session_id")?;
    let stored_socket = row.try_get::<String, _>("socket_namespace")?;
    let from_pane_id = row.try_get::<Option<String>, _>("from_pane_id")?;
    let to_agent_label = row.try_get::<String, _>("to_agent_label")?;

    // 1. Message identity + stored runtime namespace.
    if stored_conversation_id != request.conversation_id
        || stored_conversation_seq != request.conversation_seq
    {
        return Err(DbError::new(
            "conversation_mismatch",
            "receipt conversation id/seq do not match the stored message",
        ));
    }
    if stored_runtime != request.runtime_session_id {
        return Err(DbError::new(
            "runtime_mismatch",
            "receipt runtime_session_id does not match the stored message runtime",
        ));
    }
    if stored_socket != request.socket_namespace {
        return Err(DbError::new(
            "socket_namespace_mismatch",
            "receipt socket namespace does not match the stored message socket namespace",
        ));
    }

    // 2. Authoritative receiver identity must equal the stored target; reject
    //    self-receipt (the sender cannot acknowledge its own message).
    if receiver.agent_label != to_agent_label {
        return Err(DbError::new(
            "receiver_identity_mismatch",
            format!(
                "receiver {} is not the message target {}",
                receiver.agent_label, to_agent_label
            ),
        ));
    }
    if from_pane_id.as_deref() == Some(receiver.pane_id.as_str()) {
        return Err(DbError::new(
            "self_receipt_rejected",
            "the sending pane cannot report receipt of its own message",
        ));
    }

    // 3. Latest delivery state: idempotent once received; only `submitted` advances.
    let latest = sqlx::query(
        "SELECT event_type FROM delivery_events WHERE message_id = ? ORDER BY seq DESC LIMIT 1",
    )
    .bind(&request.message_id)
    .fetch_optional(&mut *conn)
    .await?;
    let latest = latest
        .map(|r| r.try_get::<String, _>("event_type"))
        .transpose()?;
    match latest.as_deref() {
        // Idempotent: a prior valid receipt exists. Append nothing, do not bump
        // delivery_seq, report `already_received`.
        Some("received") => {
            return Ok(ReceiptAccepted {
                status: ReceiptStatus::AlreadyReceived,
                message_id: request.message_id.clone(),
                conversation_id: stored_conversation_id,
                conversation_seq: stored_conversation_seq,
                receiver_pane_id: receiver.pane_id.clone(),
                receiver_agent_label: receiver.agent_label.clone(),
            });
        }
        Some("submitted") => {}
        Some("drafted") => {
            return Err(DbError::new(
                "draft_not_submitted",
                "message is drafted, not submitted; it cannot be received",
            ));
        }
        Some("failed") => {
            return Err(DbError::new(
                "already_failed",
                "message delivery already failed; retry creates a new message",
            ));
        }
        _ => {
            return Err(DbError::new(
                "invalid_delivery_transition",
                "message has no submission to receive",
            ));
        }
    }

    let effective_timestamp = request
        .timestamp
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(now);

    let payload = serde_json::json!({
        "receiver_pane_id": receiver.pane_id,
        "receiver_agent_label": receiver.agent_label,
        "receiver_agent_session": receiver.agent_session,
        "receiver_agent_session_hint": request.receiver_agent_session_hint,
        "receiver_seq": request.receiver_seq,
        "supplied_timestamp": request.timestamp,
        "effective_timestamp": effective_timestamp,
        "status": request.status,
        "message_runtime_session_id": request.runtime_session_id,
        "recording_runtime_session_id": current_runtime_id,
        "socket_namespace": request.socket_namespace,
    });

    // Defense in depth: route the append through the shared validated path, which
    // re-checks `submitted -> received`, allocates the authoritative
    // `delivery_events.seq`, and inserts with `proof_source='integration'`.
    append_delivery_event_in_transaction(
        conn,
        DeliveryEventInput {
            message_id: &request.message_id,
            event_type: DeliveryEventType::Received,
            proof_source: "integration",
            timestamp: effective_timestamp,
            payload,
        },
    )
    .await?;

    Ok(ReceiptAccepted {
        status: ReceiptStatus::Received,
        message_id: request.message_id.clone(),
        conversation_id: stored_conversation_id,
        conversation_seq: stored_conversation_seq,
        receiver_pane_id: receiver.pane_id.clone(),
        receiver_agent_label: receiver.agent_label.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zynk::message::{new_prefixed_id, Party, SendCommand};
    use crate::zynk::persistence::{
        append_delivery_event_async, begin_send_attempt_async, PersistedSend, SendAttempt,
    };

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "zynk-receipt-test-{}-{}.db",
            std::process::id(),
            new_prefixed_id("test")
        ))
    }

    fn party(agent: &str, pane: &str) -> Party {
        Party {
            agent: Some(agent.into()),
            pane: Some(pane.into()),
            workspace: Some("ws".into()),
            tab: Some("tab".into()),
            ..Party::default()
        }
    }

    async fn setup_submitted(
        conn: &mut SqliteConnection,
        from_agent: &str,
        from_pane: &str,
        to_agent: &str,
        to_pane: &str,
        message_id: &str,
    ) -> PersistedSend {
        let from = party(from_agent, from_pane);
        let to = party(to_agent, to_pane);
        let rec = begin_send_attempt_async(
            conn,
            SendAttempt {
                command: SendCommand::PaneRun,
                message_id,
                target_arg: to_agent,
                from: &from,
                to: &to,
                message_type: None,
                body: "hi",
                created_at: "2026-06-14T00:00:00Z",
                trace_id: None,
            },
            "rt_test".into(),
            "socket_test".into(),
        )
        .await
        .unwrap();
        append_event(
            conn,
            message_id,
            DeliveryEventType::Submitted,
            "pane.send_input",
        )
        .await;
        rec
    }

    async fn append_event(
        conn: &mut SqliteConnection,
        message_id: &str,
        event_type: DeliveryEventType,
        proof_source: &str,
    ) {
        append_delivery_event_async(
            conn,
            DeliveryEventInput {
                message_id,
                event_type,
                proof_source,
                timestamp: "2026-06-14T00:00:01Z",
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    }

    fn request_for(rec: &PersistedSend, message_id: &str) -> ReceiptRequest {
        ReceiptRequest {
            message_id: message_id.into(),
            conversation_id: rec.conversation_id.clone(),
            conversation_seq: rec.conversation_seq,
            runtime_session_id: rec.runtime_session_id.clone(),
            socket_namespace: rec.socket_namespace.clone(),
            receiver_seq: None,
            timestamp: None,
            status: None,
            receiver_agent_session_hint: None,
        }
    }

    fn receiver(agent: &str, pane: &str) -> AuthoritativeReceiver {
        AuthoritativeReceiver {
            pane_id: pane.into(),
            agent_label: agent.into(),
            agent_session: None,
        }
    }

    async fn latest_event(conn: &mut SqliteConnection, message_id: &str) -> (String, String) {
        let row = sqlx::query(
            "SELECT event_type, proof_source FROM delivery_events WHERE message_id = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(message_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        (
            row.try_get::<String, _>("event_type").unwrap(),
            row.try_get::<String, _>("proof_source").unwrap(),
        )
    }

    async fn delivery_seq(conn: &mut SqliteConnection, message_id: &str) -> i64 {
        sqlx::query("SELECT delivery_seq FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_one(&mut *conn)
            .await
            .unwrap()
            .try_get::<i64, _>("delivery_seq")
            .unwrap()
    }

    async fn received_count(conn: &mut SqliteConnection, message_id: &str) -> i64 {
        sqlx::query(
            "SELECT COUNT(*) AS c FROM delivery_events WHERE message_id = ? AND event_type = 'received'",
        )
        .bind(message_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap()
        .try_get::<i64, _>("c")
        .unwrap()
    }

    fn run<F>(f: F)
    where
        F: std::future::Future<Output = Result<(), DbError>>,
    {
        crate::zynk::db::block_on(f).unwrap();
    }

    #[test]
    fn valid_receipt_records_received_with_integration_proof() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let rec = setup_submitted(&mut conn, "claude", "w-1", "codex", "w-2", "msg_v").await;
            let accepted = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_v"),
                &receiver("codex", "w-2"),
                "socket_test",
                "rt_recording",
                "2026-06-14T00:00:02Z",
            )
            .await
            .unwrap();
            assert_eq!(accepted.status, ReceiptStatus::Received);
            assert_eq!(
                latest_event(&mut conn, "msg_v").await,
                ("received".into(), "integration".into())
            );
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn duplicate_receipt_is_already_received_no_seq_bump() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let rec = setup_submitted(&mut conn, "claude", "w-1", "codex", "w-2", "msg_d").await;
            let req = request_for(&rec, "msg_d");
            let recv = receiver("codex", "w-2");
            append_received_event(
                &mut conn,
                &req,
                &recv,
                "socket_test",
                "rt_a",
                "2026-06-14T00:00:02Z",
            )
            .await
            .unwrap();
            let seq_after_first = delivery_seq(&mut conn, "msg_d").await;
            let second = append_received_event(
                &mut conn,
                &req,
                &recv,
                "socket_test",
                "rt_b",
                "2026-06-14T00:00:03Z",
            )
            .await
            .unwrap();
            assert_eq!(second.status, ReceiptStatus::AlreadyReceived);
            assert_eq!(delivery_seq(&mut conn, "msg_d").await, seq_after_first);
            assert_eq!(received_count(&mut conn, "msg_d").await, 1);
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn unknown_message_is_message_not_found() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let rec = setup_submitted(&mut conn, "claude", "w-1", "codex", "w-2", "msg_real").await;
            let mut req = request_for(&rec, "msg_real");
            req.message_id = "msg_ghost".into();
            let err = append_received_event(
                &mut conn,
                &req,
                &receiver("codex", "w-2"),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "message_not_found");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn conversation_and_runtime_mismatches_are_rejected() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let rec = setup_submitted(&mut conn, "claude", "w-1", "codex", "w-2", "msg_m").await;

            let mut wrong_conv = request_for(&rec, "msg_m");
            wrong_conv.conversation_id = "conv_other".into();
            assert_eq!(
                append_received_event(
                    &mut conn,
                    &wrong_conv,
                    &receiver("codex", "w-2"),
                    "socket_test",
                    "rt",
                    "t"
                )
                .await
                .unwrap_err()
                .code,
                "conversation_mismatch"
            );

            let mut wrong_seq = request_for(&rec, "msg_m");
            wrong_seq.conversation_seq = rec.conversation_seq + 99;
            assert_eq!(
                append_received_event(
                    &mut conn,
                    &wrong_seq,
                    &receiver("codex", "w-2"),
                    "socket_test",
                    "rt",
                    "t"
                )
                .await
                .unwrap_err()
                .code,
                "conversation_mismatch"
            );

            let mut wrong_rt = request_for(&rec, "msg_m");
            wrong_rt.runtime_session_id = "rt_other".into();
            assert_eq!(
                append_received_event(
                    &mut conn,
                    &wrong_rt,
                    &receiver("codex", "w-2"),
                    "socket_test",
                    "rt",
                    "t"
                )
                .await
                .unwrap_err()
                .code,
                "runtime_mismatch"
            );
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn socket_namespace_mismatch_rejected_for_current_and_stored() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let rec = setup_submitted(&mut conn, "claude", "w-1", "codex", "w-2", "msg_s").await;

            // current server socket != receipt socket (pre-tx guard).
            let err_current = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_s"),
                &receiver("codex", "w-2"),
                "socket_other",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err_current.code, "socket_namespace_mismatch");

            // receipt socket == current, but != the stored message socket.
            let mut wrong_stored = request_for(&rec, "msg_s");
            wrong_stored.socket_namespace = "socket_dev2".into();
            let err_stored = append_received_event(
                &mut conn,
                &wrong_stored,
                &receiver("codex", "w-2"),
                "socket_dev2",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err_stored.code, "socket_namespace_mismatch");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn wrong_receiver_identity_rejected() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let rec = setup_submitted(&mut conn, "claude", "w-1", "codex", "w-2", "msg_w").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_w"),
                &receiver("pi", "w-3"),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "receiver_identity_mismatch");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn self_receipt_is_rejected() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            // Self-addressed: codex@w-1 -> codex@w-1 (from and to resolve to one pane).
            let rec = setup_submitted(&mut conn, "codex", "w-1", "codex", "w-1", "msg_self").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_self"),
                &receiver("codex", "w-1"),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "self_receipt_rejected");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn drafted_and_failed_messages_cannot_be_received() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;

            // Drafted only.
            let draft = {
                let from = party("claude", "w-1");
                let to = party("codex", "w-2");
                let rec = begin_send_attempt_async(
                    &mut conn,
                    SendAttempt {
                        command: SendCommand::PaneSendText,
                        message_id: "msg_draft",
                        target_arg: "codex",
                        from: &from,
                        to: &to,
                        message_type: None,
                        body: "hi",
                        created_at: "2026-06-14T00:00:00Z",
                        trace_id: None,
                    },
                    "rt_test".into(),
                    "socket_test".into(),
                )
                .await
                .unwrap();
                append_event(
                    &mut conn,
                    "msg_draft",
                    DeliveryEventType::Drafted,
                    "pane.send_text",
                )
                .await;
                rec
            };
            assert_eq!(
                append_received_event(
                    &mut conn,
                    &request_for(&draft, "msg_draft"),
                    &receiver("codex", "w-2"),
                    "socket_test",
                    "rt",
                    "t"
                )
                .await
                .unwrap_err()
                .code,
                "draft_not_submitted"
            );

            // Drafted -> failed.
            let failed = {
                let from = party("claude", "w-1");
                let to = party("codex", "w-2");
                let rec = begin_send_attempt_async(
                    &mut conn,
                    SendAttempt {
                        command: SendCommand::PaneSendText,
                        message_id: "msg_failed",
                        target_arg: "codex",
                        from: &from,
                        to: &to,
                        message_type: None,
                        body: "hi",
                        created_at: "2026-06-14T00:00:00Z",
                        trace_id: None,
                    },
                    "rt_test".into(),
                    "socket_test".into(),
                )
                .await
                .unwrap();
                append_event(
                    &mut conn,
                    "msg_failed",
                    DeliveryEventType::Drafted,
                    "pane.send_text",
                )
                .await;
                append_event(
                    &mut conn,
                    "msg_failed",
                    DeliveryEventType::Failed,
                    "pane.send_text",
                )
                .await;
                rec
            };
            assert_eq!(
                append_received_event(
                    &mut conn,
                    &request_for(&failed, "msg_failed"),
                    &receiver("codex", "w-2"),
                    "socket_test",
                    "rt",
                    "t"
                )
                .await
                .unwrap_err()
                .code,
                "already_failed"
            );
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn receipt_after_restart_with_different_recording_runtime_is_accepted() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let rec =
                setup_submitted(&mut conn, "claude", "w-1", "codex", "w-2", "msg_restart").await;
            // Simulate a server restart: the message's stored runtime is "rt_test",
            // but the current recording runtime differs. Same socket namespace, so the
            // receipt is accepted (M3a post-restart liveness) and the payload records
            // the differing recording runtime for audit.
            let accepted = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_restart"),
                &receiver("codex", "w-2"),
                "socket_test",
                "rt_after_restart",
                "2026-06-14T01:00:00Z",
            )
            .await
            .unwrap();
            assert_eq!(accepted.status, ReceiptStatus::Received);
            let payload = sqlx::query(
                "SELECT payload_json FROM delivery_events WHERE message_id = ? AND event_type = 'received'",
            )
            .bind("msg_restart")
            .fetch_one(&mut conn)
            .await
            .unwrap()
            .try_get::<String, _>("payload_json")
            .unwrap();
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["message_runtime_session_id"], "rt_test");
            assert_eq!(payload["recording_runtime_session_id"], "rt_after_restart");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }
}
