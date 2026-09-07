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
    /// The durable terminal anchor (`TerminalId`), the same value `agent.get`/`pane.get` report as
    /// `terminal_id` and the sender persisted on the target participant. Pane ids rotate; this
    /// does not.
    pub terminal_id: String,
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

/// The durable identity a stored participant row offers, canonicalized: a COMPLETE hook session
/// triple, else a terminal id, else nothing. A partial triple (a value without its source or kind)
/// is treated as no session, so it never binds by value alone; combined with no terminal it is
/// `None` and fails closed at the target.
enum StoredAnchor<'a> {
    Session {
        source: &'a str,
        kind: &'a str,
        value: &'a str,
    },
    /// A session with a missing component: never an anchor (it must not bind by value alone).
    Partial,
    Terminal(&'a str),
    None,
}

fn canonical_anchor<'a>(
    source: &'a Option<String>,
    kind: &'a Option<String>,
    value: &'a Option<String>,
    terminal: &'a Option<String>,
) -> StoredAnchor<'a> {
    let present = |field: &'a Option<String>| field.as_deref().filter(|s| !s.is_empty());
    match (present(source), present(kind), present(value)) {
        (Some(source), Some(kind), Some(value)) => StoredAnchor::Session {
            source,
            kind,
            value,
        },
        (None, None, None) => match present(terminal) {
            Some(terminal) => StoredAnchor::Terminal(terminal),
            None => StoredAnchor::None,
        },
        _ => StoredAnchor::Partial,
    }
}

/// Does the receiver's live hook session equal the stored complete triple? Every component must
/// be present on the receiver and equal — no wildcards.
fn receiver_matches_session(
    receiver: &AuthoritativeReceiver,
    stored_source: &str,
    stored_kind: &str,
    stored_value: &str,
) -> bool {
    let live = |key: &str| {
        receiver
            .agent_session
            .as_ref()
            .and_then(|session| session.get(key))
            .and_then(|component| component.as_str())
    };
    live("source") == Some(stored_source)
        && live("kind") == Some(stored_kind)
        && live("value") == Some(stored_value)
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
                fp.agent_label AS from_agent_label, \
                fp.terminal_id AS from_terminal_id, \
                fp.agent_session_source AS from_session_source, \
                fp.agent_session_kind AS from_session_kind, \
                fp.agent_session_value AS from_session_value, \
                m.from_participant_id AS from_participant_id, \
                m.to_participant_id AS to_participant_id, \
                tp.agent_label AS to_agent_label, \
                tp.terminal_id AS to_terminal_id, \
                tp.agent_session_value AS to_session_value, \
                tp.agent_session_source AS to_session_source, \
                tp.agent_session_kind AS to_session_kind \
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
    let from_agent_label = row.try_get::<String, _>("from_agent_label")?;
    let from_terminal_id = row.try_get::<Option<String>, _>("from_terminal_id")?;
    let from_session_source = row.try_get::<Option<String>, _>("from_session_source")?;
    let from_session_kind = row.try_get::<Option<String>, _>("from_session_kind")?;
    let from_session_value = row.try_get::<Option<String>, _>("from_session_value")?;
    let from_participant_id = row.try_get::<String, _>("from_participant_id")?;
    let to_participant_id = row.try_get::<String, _>("to_participant_id")?;
    let to_agent_label = row.try_get::<String, _>("to_agent_label")?;
    let to_terminal_id = row.try_get::<Option<String>, _>("to_terminal_id")?;
    let to_session_value = row.try_get::<Option<String>, _>("to_session_value")?;
    let to_session_source = row.try_get::<Option<String>, _>("to_session_source")?;
    let to_session_kind = row.try_get::<Option<String>, _>("to_session_kind")?;

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
    // The stored target participant is keyed by label + terminal + hook session — never by the
    // pane id, which rotates. The receiver must be THAT participant, not merely a same-label pane.
    // The stored anchor is canonicalized before authorization and fails closed (Gate-3 round 5,
    // ARCH-RECEIPT-ANCHOR-001): either a COMPLETE hook session triple (source, kind, value — the
    // durable anchor, which survives pane churn, a restart and a live handoff), or no session and
    // a terminal id (all a session-less target can offer, within this server's lifetime). A partial
    // triple, or a row with neither anchor, is not receipt-capable: no label-only fallback exists.
    match canonical_anchor(
        &to_session_source,
        &to_session_kind,
        &to_session_value,
        &to_terminal_id,
    ) {
        StoredAnchor::Session {
            source,
            kind,
            value,
        } => {
            if !receiver_matches_session(receiver, source, kind, value) {
                return Err(DbError::new(
                    "receiver_identity_mismatch",
                    "receiver agent session (source, kind, value) is not the session the message \
                     was addressed to",
                ));
            }
        }
        StoredAnchor::Terminal(stored) => {
            if receiver.terminal_id != stored {
                return Err(DbError::new(
                    "receiver_identity_mismatch",
                    format!(
                        "receiver terminal {} is not the terminal the message was addressed to ({})",
                        receiver.terminal_id, stored
                    ),
                ));
            }
        }
        StoredAnchor::Partial => {
            return Err(DbError::new(
                "receiver_identity_mismatch",
                "the stored target's hook session is incomplete (source, kind or value missing), so \
                 no receipt can be attributed to it",
            ));
        }
        StoredAnchor::None => {
            return Err(DbError::new(
                "receiver_identity_mismatch",
                "the stored target carries no durable identity (no complete hook session and no \
                 terminal), so no receipt can be attributed to it",
            ));
        }
    }
    // Self-receipt is decided by the SAME session-first logical identity as the target binding
    // (Gate-3 round 4, ARCH-RECEIPT-SELF-001; Codex Gate-2 R20): the sender's stored label plus its
    // hook session triple when it carried a session, else its terminal, else — for rows with no
    // durable anchor at all — its stored pane id. Row identity alone is too narrow (the same
    // session across a restore lives in two participant rows) and the stored pane id alone is both
    // too narrow (pane ids rotate) and too wide (another participant may later resume at that
    // pane). The pane id is otherwise audit metadata.
    let receiver_is_sender = from_participant_id == to_participant_id
        || (from_agent_label == receiver.agent_label
            && match canonical_anchor(
                &from_session_source,
                &from_session_kind,
                &from_session_value,
                &from_terminal_id,
            ) {
                StoredAnchor::Session {
                    source,
                    kind,
                    value,
                } => receiver_matches_session(receiver, source, kind, value),
                StoredAnchor::Terminal(stored) => receiver.terminal_id == stored,
                // A malformed sender row decides by its terminal, else its stored pane.
                StoredAnchor::Partial => {
                    match from_terminal_id.as_deref().filter(|t| !t.is_empty()) {
                        Some(stored) => receiver.terminal_id == stored,
                        None => from_pane_id.as_deref() == Some(receiver.pane_id.as_str()),
                    }
                }
                StoredAnchor::None => from_pane_id.as_deref() == Some(receiver.pane_id.as_str()),
            });
    if receiver_is_sender {
        return Err(DbError::new(
            "self_receipt_rejected",
            "the sender is the addressee of its own message and cannot report its receipt",
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

    /// A hook-less party on its own terminal (`term-<pane>`): the terminal is the durable anchor a
    /// receipt binds to when no session was stored (a stored target with neither anchor is refused).
    fn party(agent: &str, pane: &str) -> Party {
        Party {
            agent: Some(agent.into()),
            pane: Some(pane.into()),
            terminal_id: Some(format!("term-{pane}")),
            workspace: Some("ws".into()),
            tab: Some("tab".into()),
            ..Party::default()
        }
    }

    /// A party with NO durable anchor at all (no terminal, no session): a legacy/sparse row.
    fn unanchored_party(agent: &str, pane: &str) -> Party {
        Party {
            terminal_id: None,
            ..party(agent, pane)
        }
    }

    /// A party pinned to a terminal (and optionally a hook session) — the durable anchors of the
    /// participant key; the pane id is deliberately NOT one of them (pane ids rotate).
    fn party_on(agent: &str, pane: &str, terminal: &str, session: Option<&str>) -> Party {
        Party {
            terminal_id: Some(terminal.into()),
            agent_session: session
                .map(|value| serde_json::json!({"source": "hook", "kind": "id", "value": value})),
            ..party(agent, pane)
        }
    }

    /// A party whose hook session is given as the full triple the participant identity stores.
    fn party_with_session(agent: &str, pane: &str, source: &str, kind: &str, value: &str) -> Party {
        Party {
            terminal_id: Some("term-2".into()),
            agent_session: Some(
                serde_json::json!({"source": source, "kind": kind, "value": value}),
            ),
            ..party(agent, pane)
        }
    }

    fn receiver_with_session(
        agent: &str,
        pane: &str,
        source: &str,
        kind: &str,
        value: &str,
    ) -> AuthoritativeReceiver {
        AuthoritativeReceiver {
            pane_id: pane.into(),
            terminal_id: "term-2".into(),
            agent_label: agent.into(),
            agent_session: Some(serde_json::json!({
                "source": source, "agent": agent, "kind": kind, "value": value
            })),
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
        setup_submitted_between(conn, &from, &to, message_id).await
    }

    async fn setup_submitted_between(
        conn: &mut SqliteConnection,
        from: &Party,
        to: &Party,
        message_id: &str,
    ) -> PersistedSend {
        let target_arg = to.agent.clone().unwrap_or_default();
        let rec = begin_send_attempt_async(
            conn,
            SendAttempt {
                command: SendCommand::PaneRun,
                message_id,
                target_arg: &target_arg,
                from,
                to,
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
            terminal_id: format!("term-{pane}"),
            agent_label: agent.into(),
            agent_session: None,
        }
    }

    fn receiver_on(
        agent: &str,
        pane: &str,
        terminal: &str,
        session: Option<&str>,
    ) -> AuthoritativeReceiver {
        AuthoritativeReceiver {
            pane_id: pane.into(),
            terminal_id: terminal.into(),
            agent_label: agent.into(),
            agent_session: session.map(|value| {
                serde_json::json!({"source": "hook", "agent": agent, "kind": "id", "value": value})
            }),
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
    fn receipt_from_a_different_terminal_with_the_same_label_is_rejected() {
        // Gate-3 round 3 (AUD-310-RECEIPT-001): a receipt binds to the STORED target participant
        // (label + terminal + session = the participant key), never to an agent label alone. A
        // second hook-authoritative "codex" on another terminal cannot receipt this message.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_on("codex", "w-2", "term-2", None);
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_bound").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_bound"),
                &receiver_on("codex", "w-3", "term-3", None),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "receiver_identity_mismatch");
            assert_eq!(latest_event(&mut conn, "msg_bound").await.0, "submitted");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn receipt_from_the_same_terminal_under_another_session_is_rejected() {
        // Same label, same terminal, but the hook session the message was addressed to is gone:
        // the stored participant carried a session value, so the receiver must present it.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_on("codex", "w-2", "term-2", Some("sess-1"));
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_sess").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_sess"),
                &receiver_on("codex", "w-2", "term-2", Some("sess-2")),
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
    fn receipt_after_pane_churn_on_the_stored_terminal_is_accepted() {
        // Pane-churn policy: pane ids rotate (restart, layout moves) and are NOT part of the
        // participant key. The same terminal + session receipting from a different pane id is the
        // addressed participant and is accepted.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_on("codex", "w-2", "term-2", Some("sess-1"));
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_churn").await;
            let accepted = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_churn"),
                &receiver_on("codex", "w-9", "term-2", Some("sess-1")),
                "socket_test",
                "rt",
                "t",
            )
            .await?;
            assert!(matches!(accepted.status, ReceiptStatus::Received));
            assert_eq!(latest_event(&mut conn, "msg_churn").await.0, "received");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn receipt_after_a_restore_with_the_stored_session_is_accepted() {
        // Restart / live-handoff model: the restored terminal has a NEW terminal id (ids are
        // allocated per server lifetime) but keeps its persisted agent session. The session is the
        // durable anchor, so the addressed participant still receipts.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_on("codex", "w-2", "term-2", Some("sess-1"));
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_restore").await;
            let accepted = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_restore"),
                &receiver_on("codex", "w-2", "term-7", Some("sess-1")),
                "socket_test",
                "rt",
                "t",
            )
            .await?;
            assert!(matches!(accepted.status, ReceiptStatus::Received));
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn receipt_with_the_stored_session_value_but_another_source_is_rejected() {
        // Gate-3 round 3 pre-read (arbiter): the stored participant identity is the full session
        // triple (source, kind, value), not the value alone.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_with_session("pi", "w-2", "zynk:pi", "id", "sess-1");
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_src").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_src"),
                &receiver_with_session("pi", "w-2", "hook", "id", "sess-1"),
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
    fn receipt_with_the_stored_session_value_but_another_kind_is_rejected() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_with_session("pi", "w-2", "zynk:pi", "id", "sess-1");
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_kind").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_kind"),
                &receiver_with_session("pi", "w-2", "zynk:pi", "path", "sess-1"),
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
    fn receipt_with_the_full_stored_session_triple_is_accepted() {
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_with_session("pi", "w-2", "zynk:pi", "id", "sess-1");
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_triple").await;
            let accepted = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_triple"),
                &receiver_with_session("pi", "w-2", "zynk:pi", "id", "sess-1"),
                "socket_test",
                "rt",
                "t",
            )
            .await?;
            assert!(matches!(accepted.status, ReceiptStatus::Received));
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
    fn a_self_addressed_message_is_rejected_after_pane_churn() {
        // Gate-3 round 4 (ARCH-RECEIPT-SELF-001): the self-receipt guard compared pane ids, which
        // rotate and are not identity. The sender is the addressee here (same label, terminal and
        // session); a receipt from its new pane id is still a self-receipt.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let me = party_on("codex", "w-1", "term-1", Some("sess-1"));
            let rec = setup_submitted_between(&mut conn, &me, &me, "msg_self_churn").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_self_churn"),
                &receiver_on("codex", "w-9", "term-1", Some("sess-1")),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "self_receipt_rejected");
            assert_eq!(
                latest_event(&mut conn, "msg_self_churn").await.0,
                "submitted"
            );
            let seq: i64 = sqlx::query("SELECT delivery_seq FROM messages WHERE id = ?")
                .bind("msg_self_churn")
                .fetch_one(&mut conn)
                .await?
                .try_get("delivery_seq")?;
            assert_eq!(seq, 1, "delivery_seq and the event history are unchanged");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn a_stale_participant_pane_snapshot_does_not_enable_self_receipt() {
        // The participant row keeps its FIRST pane snapshot (INSERT OR IGNORE); a later message
        // from the same participant at another pane must still be a rejected self-receipt.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let first = party_on("codex", "w-1", "term-1", Some("sess-1"));
            let _ = setup_submitted_between(&mut conn, &first, &first, "msg_self_first").await;
            let later = party_on("codex", "w-5", "term-1", Some("sess-1"));
            let rec = setup_submitted_between(&mut conn, &later, &later, "msg_self_later").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_self_later"),
                &receiver_on("codex", "w-5", "term-1", Some("sess-1")),
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
    fn the_same_session_across_terminal_rows_cannot_receipt_its_own_message() {
        // Codex Gate-2 R20 (item 3): sender and addressee with the SAME label and coherent hook
        // session but different terminal ids (a restore in between) are different participant
        // rows; self-receipt is decided by the session-first logical identity, not row equality.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let before = party_on("codex", "w-1", "term-1", Some("sess-1"));
            let after = party_on("codex", "w-2", "term-2", Some("sess-1"));
            let rec = setup_submitted_between(&mut conn, &before, &after, "msg_self_rows").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_self_rows"),
                &receiver_on("codex", "w-9", "term-2", Some("sess-1")),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "self_receipt_rejected");
            assert_eq!(
                latest_event(&mut conn, "msg_self_rows").await.0,
                "submitted"
            );
            let seq: i64 = sqlx::query("SELECT delivery_seq FROM messages WHERE id = ?")
                .bind("msg_self_rows")
                .fetch_one(&mut conn)
                .await?
                .try_get("delivery_seq")?;
            assert_eq!(seq, 1);
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn a_distinct_participant_resuming_at_the_senders_old_pane_is_accepted() {
        // Codex Gate-2 R20 (item 4): the stored sender pane id is not a veto for anchored senders.
        // Claude (session a) sent from pane w-1; Codex (session b) later resumes at w-1 with a new
        // terminal — a valid receipt by the addressee, not a self-receipt.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let sender = party_on("claude", "w-1", "term-1", Some("sess-a"));
            let target = party_on("codex", "w-2", "term-2", Some("sess-b"));
            let rec = setup_submitted_between(&mut conn, &sender, &target, "msg_old_pane").await;
            let accepted = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_old_pane"),
                &receiver_on("codex", "w-1", "term-9", Some("sess-b")),
                "socket_test",
                "rt",
                "t",
            )
            .await?;
            assert!(matches!(accepted.status, ReceiptStatus::Received));
            assert_eq!(latest_event(&mut conn, "msg_old_pane").await.0, "received");
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn a_stored_target_with_no_durable_anchor_is_refused() {
        // Gate-3 round 5 (ARCH-RECEIPT-ANCHOR-001): a stored target with neither a session nor a
        // terminal used to degrade to a label-only check, so any same-label hook-authoritative pane
        // could receipt it. Such a row is not receipt-capable: fail closed.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = unanchored_party("codex", "w-2");
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_unanchored").await;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_unanchored"),
                &receiver("codex", "w-2"),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "receiver_identity_mismatch", "{}", err.message);
            assert_eq!(
                latest_event(&mut conn, "msg_unanchored").await.0,
                "submitted"
            );
            let _ = std::fs::remove_file(path);
            Ok(())
        });
    }

    #[test]
    fn a_partial_stored_session_triple_is_refused() {
        // A stored session missing its source or kind is not an anchor (absent components were
        // wildcards): the row fails closed even when the receiver presents the same value. Persist
        // never writes such a row any more (a partial triple is stored as no session), so the
        // legacy shape is planted directly on the stored participant.
        run(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = party("claude", "w-1");
            let to = party_on("codex", "w-2", "term-w-2", Some("sess-1"));
            let rec = setup_submitted_between(&mut conn, &from, &to, "msg_partial").await;
            sqlx::query(
                "UPDATE conversation_participants SET agent_session_source = NULL \
                 WHERE id = (SELECT to_participant_id FROM messages WHERE id = 'msg_partial')",
            )
            .execute(&mut conn)
            .await?;
            let err = append_received_event(
                &mut conn,
                &request_for(&rec, "msg_partial"),
                &receiver_on("codex", "w-2", "term-w-2", Some("sess-1")),
                "socket_test",
                "rt",
                "t",
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "receiver_identity_mismatch", "{}", err.message);
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
