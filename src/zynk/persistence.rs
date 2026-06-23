//! zynk fork: message persistence transactions (ADR 0003).

use sqlx::{Executor, Row, SqliteConnection};

use crate::zynk::db::DbError;
use crate::zynk::message::{
    body_hash, lowercase_hex_sha256, new_prefixed_id, Party, SendCommand, SendOutcome,
};

#[derive(Clone, Debug)]
pub struct PersistedSend {
    pub message_id: String,
    pub conversation_id: String,
    pub conversation_seq: i64,
    pub body_hash: String,
    pub runtime_session_id: String,
    pub socket_namespace: String,
    /// Feature #107 (IM1): the per-message `trace_id` carried on the record. `None` when
    /// this send had no `--trace`. Stored in `messages.meta_json` ONLY — never in
    /// `body`/`body_hash`/`protocol_json`. Read by the IM3 header v2 renderer
    /// (`render_header`) to emit the wire-only `trace:` line.
    pub trace_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SendAttempt<'a> {
    pub command: SendCommand,
    pub message_id: &'a str,
    pub target_arg: &'a str,
    pub from: &'a Party,
    pub to: &'a Party,
    pub message_type: Option<&'a str>,
    pub body: &'a str,
    pub created_at: &'a str,
    /// Feature #107 (IM1): optional per-message trace id. When `Some`, the INSERT
    /// writes `meta_json = {"trace_id": <id>}`; when `None`, `meta_json` stays `'{}'`.
    /// PRE-VALIDATED at the CLI (`validate_trace_id`); persistence trusts the cleaned
    /// value and never mutates `body`/`body_hash`/`protocol_json` for it.
    pub trace_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryEventType {
    Drafted,
    Submitted,
    Received,
    Failed,
}

impl DeliveryEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drafted => "drafted",
            Self::Submitted => "submitted",
            Self::Received => "received",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeliveryEventInput<'a> {
    pub message_id: &'a str,
    pub event_type: DeliveryEventType,
    pub proof_source: &'a str,
    pub timestamp: &'a str,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug)]
struct ParticipantFields {
    agent_label: String,
    pane_id: Option<String>,
    terminal_id: Option<String>,
    terminal_instance_id: Option<String>,
    agent_session_source: Option<String>,
    agent_session_kind: Option<String>,
    agent_session_value: Option<String>,
    participant_key: String,
}

pub fn begin_send_attempt(input: SendAttempt<'_>) -> Result<PersistedSend, DbError> {
    crate::zynk::db::block_on(async move {
        let runtime_session_id = crate::zynk::runtime::read_runtime_id()
            .map_err(|msg| DbError::new("runtime_identity_missing", msg))?;
        let socket_namespace = crate::zynk::runtime::socket_namespace();
        let mut conn = crate::zynk::db::open_migrated_for_append().await?;
        begin_send_attempt_async(&mut conn, input, runtime_session_id, socket_namespace).await
    })
}

pub fn attach_to_outcome(outcome: SendOutcome, record: &PersistedSend) -> SendOutcome {
    outcome.with_persistence(
        record.conversation_id.clone(),
        record.conversation_seq,
        record.body_hash.clone(),
        record.runtime_session_id.clone(),
        record.socket_namespace.clone(),
    )
}

/// Feature #107 (IM1): resolve the trace_id to INHERIT for a `--trace inherit` send.
///
/// The inherited trace comes from the SAME parent the persist path derives:
/// the latest message authored by the reply-TARGET agent (`to`) in the CURRENT active
/// conversation for this runtime/socket/workspace/tab scope. This is READ-ONLY
/// (`PRAGMA query_only`); it never writes and never opens/creates a conversation.
///
/// Returns:
/// - `Ok(Some(id))` — the parent exists and carries a `meta_json.trace_id`.
/// - `Ok(None)`     — there is no parent OR the parent has no trace_id (the caller
///   then sends WITHOUT a trace and notes it; NEVER an invented conversation trace).
/// - `Err(_)`       — the DB could not be read.
pub fn resolve_parent_trace_id(from: &Party, to: &Party) -> Result<Option<String>, DbError> {
    let workspace_id = scoped_field(to.workspace.as_ref().or(from.workspace.as_ref()));
    let tab_id = scoped_field(to.tab.as_ref().or(from.tab.as_ref()));
    let to_label = participant_fields(to).agent_label;
    crate::zynk::db::block_on(async move {
        let runtime_session_id = crate::zynk::runtime::read_runtime_id()
            .map_err(|msg| DbError::new("runtime_identity_missing", msg))?;
        let socket_namespace = crate::zynk::runtime::socket_namespace();
        let mut conn = crate::zynk::db::open_query_readonly().await?;
        parent_trace_id_async(
            &mut conn,
            &runtime_session_id,
            &socket_namespace,
            &workspace_id,
            &tab_id,
            &to_label,
        )
        .await
    })
}

pub(crate) async fn parent_trace_id_async(
    conn: &mut SqliteConnection,
    runtime_session_id: &str,
    socket_namespace: &str,
    workspace_id: &str,
    tab_id: &str,
    to_label: &str,
) -> Result<Option<String>, DbError> {
    // The active conversation for this scope (read-only; do NOT create one).
    let Some(conversation_row) = sqlx::query(
        "SELECT id FROM conversations WHERE status = 'active' AND runtime_session_id = ? AND socket_namespace = ? AND workspace_id = ? AND tab_id = ? LIMIT 1",
    )
    .bind(runtime_session_id)
    .bind(socket_namespace)
    .bind(workspace_id)
    .bind(tab_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let conversation_id = conversation_row.try_get::<String, _>("id")?;

    // The trace_id of the latest message from the reply-target agent — the same parent
    // `latest_message_from_agent` (used for `derived_parent_id`) selects. NULL-safe:
    // a parent with no `meta_json.trace_id` yields SQL NULL → `None`.
    let row = sqlx::query(
        "SELECT json_extract(m.meta_json, '$.trace_id') AS trace_id FROM messages m JOIN conversation_participants p ON p.id = m.from_participant_id WHERE m.conversation_id = ? AND p.agent_label = ? ORDER BY m.conversation_seq DESC LIMIT 1",
    )
    .bind(&conversation_id)
    .bind(to_label)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.and_then(|r| r.try_get::<Option<String>, _>("trace_id").ok().flatten()))
}

pub fn append_delivery_event(input: DeliveryEventInput<'_>) -> Result<(), DbError> {
    crate::zynk::db::block_on(async move {
        let mut conn = crate::zynk::db::open_migrated_for_append().await?;
        append_delivery_event_async(&mut conn, input).await
    })
}

pub(crate) async fn begin_send_attempt_async(
    conn: &mut SqliteConnection,
    input: SendAttempt<'_>,
    runtime_session_id: String,
    socket_namespace: String,
) -> Result<PersistedSend, DbError> {
    conn.execute("BEGIN IMMEDIATE").await?;
    let result = begin_send_attempt_in_transaction(
        conn,
        input,
        runtime_session_id.clone(),
        socket_namespace.clone(),
    )
    .await;
    match result {
        Ok(record) => {
            conn.execute("COMMIT").await?;
            Ok(record)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK").await;
            Err(err)
        }
    }
}

async fn begin_send_attempt_in_transaction(
    conn: &mut SqliteConnection,
    input: SendAttempt<'_>,
    runtime_session_id: String,
    socket_namespace: String,
) -> Result<PersistedSend, DbError> {
    let workspace_id = scoped_field(
        input
            .to
            .workspace
            .as_ref()
            .or(input.from.workspace.as_ref()),
    );
    let tab_id = scoped_field(input.to.tab.as_ref().or(input.from.tab.as_ref()));
    let conversation_id = ensure_active_conversation(
        conn,
        &runtime_session_id,
        &socket_namespace,
        &workspace_id,
        &tab_id,
        input.created_at,
    )
    .await?;

    let from_fields = participant_fields(input.from);
    let to_fields = participant_fields(input.to);
    let from_participant_id =
        ensure_participant(conn, &conversation_id, &from_fields, input.created_at).await?;
    let to_participant_id =
        ensure_participant(conn, &conversation_id, &to_fields, input.created_at).await?;

    let derived_parent_id =
        latest_message_from_agent(conn, &conversation_id, &to_fields.agent_label).await?;
    let seq = increment_conversation_seq(conn, &conversation_id, input.created_at).await?;
    let hash = body_hash(input.body);
    // zynk (ADR 0005): the `protocol_json` DB column carries the persisted protocol IDs
    // UNIFORMLY for every send command (incl. `pane send-text` drafts); only these
    // structured IDs are stored. `body`/`body_hash`/FTS below stay PURE. The
    // agent-VISIBLE wire HEADER (for agent targets) is PREPENDED to the transmitted text
    // at the CLI, never persisted here — body purity is preserved.
    let mut protocol_obj = crate::zynk::header::protocol_id_fields(
        input.message_id,
        &conversation_id,
        seq,
        &runtime_session_id,
        &socket_namespace,
        &hash,
        input.message_type,
    );
    if let Some(map) = protocol_obj.as_object_mut() {
        map.insert(
            "command".to_string(),
            serde_json::Value::String(input.command.as_str().to_string()),
        );
    }
    let protocol_json = serde_json::to_string(&protocol_obj)
        .map_err(|err| DbError::new("json_encode_failed", err.to_string()))?;

    // Feature #107 (IM1): the per-message `trace_id` lives in `meta_json` ONLY. Build it
    // with serde_json (escape-safe — NEVER string-concatenate user input into JSON). When
    // there is no trace, `meta_json` stays the canonical empty object `'{}'`. `body`/
    // `body_hash`/`protocol_json`/FTS are UNCHANGED by the trace — body purity is preserved.
    let meta_json = match input.trace_id.as_deref() {
        Some(id) => serde_json::json!({ "trace_id": id }).to_string(),
        None => "{}".to_string(),
    };

    sqlx::query(
        "INSERT INTO messages (id, conversation_id, conversation_seq, derived_parent_id, runtime_session_id, socket_namespace, created_at, target_arg, from_participant_id, to_participant_id, type, body, body_hash, workspace_id, tab_id, cwd, foreground_cwd, branch, git_sha, protocol_json, meta_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.message_id)
    .bind(&conversation_id)
    .bind(seq)
    .bind(derived_parent_id)
    .bind(&runtime_session_id)
    .bind(&socket_namespace)
    .bind(input.created_at)
    .bind(input.target_arg)
    .bind(&from_participant_id)
    .bind(&to_participant_id)
    .bind(input.message_type)
    .bind(input.body)
    .bind(&hash)
    .bind(&workspace_id)
    .bind(&tab_id)
    .bind(input.from.cwd.as_deref())
    .bind(input.to.cwd.as_deref())
    .bind(input.from.branch.as_deref())
    .bind(input.from.git_sha.as_deref())
    .bind(protocol_json)
    .bind(&meta_json)
    .execute(&mut *conn)
    .await?;

    let row = sqlx::query("SELECT rowid FROM messages WHERE id = ?")
        .bind(input.message_id)
        .fetch_one(&mut *conn)
        .await?;
    let rowid = row.try_get::<i64, _>("rowid")?;
    sqlx::query(
        "INSERT INTO messages_fts (rowid, body, type, branch, cwd, target_arg) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(rowid)
    .bind(input.body)
    .bind(input.message_type)
    .bind(input.from.branch.as_deref())
    .bind(input.from.cwd.as_deref())
    .bind(input.target_arg)
    .execute(&mut *conn)
    .await?;

    // zynk M5b (ADR 0006): enqueue a PENDING embedding job in the SAME transaction as the
    // message + FTS rows, so it commits/rolls back atomically (never an orphan job, never a
    // message without a job). This is a LOCAL INSERT ONLY — the embedding compute is out-of-band
    // in the embedding worker (B4); the send NEVER blocks on it. body/body_hash/FTS stay pure.
    sqlx::query(
        "INSERT INTO embedding_jobs (id, message_id, model_id, status, attempts, enqueued_at) VALUES (?, ?, ?, 'pending', 0, ?)",
    )
    .bind(new_prefixed_id("ejob"))
    .bind(input.message_id)
    .bind(crate::zynk::embed::active_model_id())
    .bind(input.created_at)
    .execute(&mut *conn)
    .await?;

    Ok(PersistedSend {
        message_id: input.message_id.to_string(),
        conversation_id,
        conversation_seq: seq,
        body_hash: hash,
        runtime_session_id,
        socket_namespace,
        // Carry the trace forward (IM3 header v2 reads this). `meta_json` is the storage
        // of record; this field mirrors it so callers need not re-query the row.
        trace_id: input.trace_id.clone(),
    })
}

pub(crate) async fn append_delivery_event_async(
    conn: &mut SqliteConnection,
    input: DeliveryEventInput<'_>,
) -> Result<(), DbError> {
    conn.execute("BEGIN IMMEDIATE").await?;
    let result = append_delivery_event_in_transaction(conn, input).await;
    match result {
        Ok(()) => {
            conn.execute("COMMIT").await?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK").await;
            Err(err)
        }
    }
}

pub(crate) async fn append_delivery_event_in_transaction(
    conn: &mut SqliteConnection,
    input: DeliveryEventInput<'_>,
) -> Result<(), DbError> {
    validate_delivery_transition(conn, input.message_id, input.event_type).await?;
    let seq_row = sqlx::query(
        "UPDATE messages SET delivery_seq = delivery_seq + 1 WHERE id = ? RETURNING delivery_seq",
    )
    .bind(input.message_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(seq_row) = seq_row else {
        return Err(DbError::new(
            "message_not_found",
            format!("message {} not found for delivery event", input.message_id),
        ));
    };
    let seq = seq_row.try_get::<i64, _>("delivery_seq")?;
    let payload = serde_json::to_string(&input.payload)
        .map_err(|err| DbError::new("json_encode_failed", err.to_string()))?;
    sqlx::query(
        "INSERT INTO delivery_events (id, message_id, event_type, proof_source, seq, timestamp, payload_json) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_prefixed_id("evt"))
    .bind(input.message_id)
    .bind(input.event_type.as_str())
    .bind(input.proof_source)
    .bind(seq)
    .bind(input.timestamp)
    .bind(payload)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn validate_delivery_transition(
    conn: &mut SqliteConnection,
    message_id: &str,
    next: DeliveryEventType,
) -> Result<(), DbError> {
    let latest = sqlx::query(
        "SELECT event_type FROM delivery_events WHERE message_id = ? ORDER BY seq DESC LIMIT 1",
    )
    .bind(message_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = latest else {
        // No prior delivery event: only an M2 first event is valid. A `received`
        // event can never be the first event — a receipt requires a prior `submitted`.
        return match next {
            DeliveryEventType::Received => Err(DbError::new(
                "invalid_delivery_transition",
                "cannot append received as the first delivery event; message was never submitted",
            )),
            DeliveryEventType::Drafted
            | DeliveryEventType::Submitted
            | DeliveryEventType::Failed => Ok(()),
        };
    };
    let current = row.try_get::<String, _>("event_type")?;
    let allowed = matches!(
        (current.as_str(), next),
        ("drafted", DeliveryEventType::Failed) | ("submitted", DeliveryEventType::Received)
    );
    if allowed {
        Ok(())
    } else {
        Err(DbError::new(
            "invalid_delivery_transition",
            format!("cannot append {} after {current}", next.as_str()),
        ))
    }
}

async fn ensure_active_conversation(
    conn: &mut SqliteConnection,
    runtime_session_id: &str,
    socket_namespace: &str,
    workspace_id: &str,
    tab_id: &str,
    at: &str,
) -> Result<String, DbError> {
    sqlx::query(
        "UPDATE conversations SET status = 'closed', last_message_at = ? WHERE status = 'active' AND socket_namespace = ? AND workspace_id = ? AND tab_id = ? AND runtime_session_id <> ?",
    )
    .bind(at)
    .bind(socket_namespace)
    .bind(workspace_id)
    .bind(tab_id)
    .bind(runtime_session_id)
    .execute(&mut *conn)
    .await?;

    if let Some(row) = sqlx::query(
        "SELECT id FROM conversations WHERE status = 'active' AND runtime_session_id = ? AND socket_namespace = ? AND workspace_id = ? AND tab_id = ? LIMIT 1",
    )
    .bind(runtime_session_id)
    .bind(socket_namespace)
    .bind(workspace_id)
    .bind(tab_id)
    .fetch_optional(&mut *conn)
    .await? {
        return row.try_get::<String, _>("id").map_err(DbError::from);
    }

    let id = new_prefixed_id("conv");
    sqlx::query(
        "INSERT INTO conversations (id, runtime_session_id, socket_namespace, workspace_id, tab_id, created_at, last_message_at, status, meta_json) VALUES (?, ?, ?, ?, ?, ?, ?, 'active', '{}')",
    )
    .bind(&id)
    .bind(runtime_session_id)
    .bind(socket_namespace)
    .bind(workspace_id)
    .bind(tab_id)
    .bind(at)
    .bind(at)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

async fn ensure_participant(
    conn: &mut SqliteConnection,
    conversation_id: &str,
    fields: &ParticipantFields,
    joined_at: &str,
) -> Result<String, DbError> {
    let id = new_prefixed_id("part");
    sqlx::query(
        "INSERT OR IGNORE INTO conversation_participants (id, conversation_id, agent_label, pane_id, terminal_id, terminal_instance_id, agent_session_source, agent_session_kind, agent_session_value, participant_key, joined_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(conversation_id)
    .bind(&fields.agent_label)
    .bind(fields.pane_id.as_deref())
    .bind(fields.terminal_id.as_deref())
    .bind(fields.terminal_instance_id.as_deref())
    .bind(fields.agent_session_source.as_deref())
    .bind(fields.agent_session_kind.as_deref())
    .bind(fields.agent_session_value.as_deref())
    .bind(&fields.participant_key)
    .bind(joined_at)
    .execute(&mut *conn)
    .await?;

    let row = sqlx::query(
        "SELECT id FROM conversation_participants WHERE conversation_id = ? AND participant_key = ?",
    )
    .bind(conversation_id)
    .bind(&fields.participant_key)
    .fetch_one(&mut *conn)
    .await?;
    row.try_get::<String, _>("id").map_err(DbError::from)
}

async fn latest_message_from_agent(
    conn: &mut SqliteConnection,
    conversation_id: &str,
    agent_label: &str,
) -> Result<Option<String>, DbError> {
    let row = sqlx::query(
        "SELECT m.id FROM messages m JOIN conversation_participants p ON p.id = m.from_participant_id WHERE m.conversation_id = ? AND p.agent_label = ? ORDER BY m.conversation_seq DESC LIMIT 1",
    )
    .bind(conversation_id)
    .bind(agent_label)
    .fetch_optional(&mut *conn)
    .await?;
    row.map(|r| r.try_get::<String, _>("id"))
        .transpose()
        .map_err(DbError::from)
}

async fn increment_conversation_seq(
    conn: &mut SqliteConnection,
    conversation_id: &str,
    at: &str,
) -> Result<i64, DbError> {
    let row = sqlx::query(
        "UPDATE conversations SET conversation_seq = conversation_seq + 1, last_message_at = ? WHERE id = ? RETURNING conversation_seq",
    )
    .bind(at)
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    row.try_get::<i64, _>("conversation_seq")
        .map_err(DbError::from)
}

fn scoped_field(value: Option<&String>) -> String {
    value
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "unknown".to_string())
}

fn participant_fields(party: &Party) -> ParticipantFields {
    let (agent_session_source, agent_session_kind, agent_session_value) = session_fields(party);
    let agent_label = party
        .agent
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .or_else(|| party.pane.as_ref().map(|pane| format!("pane:{pane}")))
        .unwrap_or_else(|| "unknown".to_string());
    let terminal = party.terminal_id.clone().unwrap_or_default();
    let session_source = agent_session_source.clone().unwrap_or_default();
    let session_kind = agent_session_kind.clone().unwrap_or_default();
    let session = agent_session_value.clone().unwrap_or_default();
    let participant_key = lowercase_hex_sha256(
        format!("agent={agent_label}\0terminal={terminal}\0source={session_source}\0kind={session_kind}\0session={session}").as_bytes(),
    );
    ParticipantFields {
        agent_label,
        pane_id: party.pane.clone(),
        terminal_id: party.terminal_id.clone(),
        terminal_instance_id: None,
        agent_session_source,
        agent_session_kind,
        agent_session_value,
        participant_key,
    }
}

fn session_fields(party: &Party) -> (Option<String>, Option<String>, Option<String>) {
    let Some(value) = party.agent_session.as_ref() else {
        return (None, None, None);
    };
    let source = value
        .get("source")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let session_value = value
        .get("value")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (source, kind, session_value)
}

pub fn failed_event_payload(message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "error": message.into() })
}

pub fn empty_event_payload() -> serde_json::Value {
    serde_json::json!({})
}

pub fn transport_effect_context(effect: &'static str, detail: String) -> serde_json::Value {
    serde_json::json!({
        "transport_effect": effect,
        "detail": detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "zynk-persistence-test-{}-{}.db",
            std::process::id(),
            new_prefixed_id("test")
        ))
    }

    async fn create_test_message_between(
        conn: &mut SqliteConnection,
        message_id: &str,
        command: SendCommand,
        from_label: &str,
        to_label: &str,
    ) -> PersistedSend {
        let from = Party {
            agent: Some(from_label.into()),
            workspace: Some("workspace".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        let to = Party {
            agent: Some(to_label.into()),
            workspace: Some("workspace".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        begin_send_attempt_async(
            conn,
            SendAttempt {
                command,
                message_id,
                target_arg: to_label,
                from: &from,
                to: &to,
                message_type: None,
                body: "body",
                created_at: "2026-06-14T00:00:00Z",
                trace_id: None,
            },
            "rt_test".into(),
            "socket_test".into(),
        )
        .await
        .unwrap()
    }

    async fn create_test_message(
        conn: &mut SqliteConnection,
        message_id: &str,
        command: SendCommand,
    ) -> PersistedSend {
        create_test_message_between(conn, message_id, command, "alice", "bob").await
    }

    #[test]
    fn delivery_transition_validation_allows_only_m2_transitions() {
        crate::zynk::db::block_on(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;

            let draft =
                create_test_message(&mut conn, "msg_draft", SendCommand::PaneSendText).await;
            append_delivery_event_async(
                &mut conn,
                DeliveryEventInput {
                    message_id: &draft.message_id,
                    event_type: DeliveryEventType::Drafted,
                    proof_source: "pane.send_text",
                    timestamp: "2026-06-14T00:00:01Z",
                    payload: serde_json::json!({}),
                },
            )
            .await?;
            append_delivery_event_async(
                &mut conn,
                DeliveryEventInput {
                    message_id: &draft.message_id,
                    event_type: DeliveryEventType::Failed,
                    proof_source: "pane.send_text",
                    timestamp: "2026-06-14T00:00:02Z",
                    payload: serde_json::json!({}),
                },
            )
            .await?;

            let submitted =
                create_test_message(&mut conn, "msg_submitted", SendCommand::PaneRun).await;
            append_delivery_event_async(
                &mut conn,
                DeliveryEventInput {
                    message_id: &submitted.message_id,
                    event_type: DeliveryEventType::Submitted,
                    proof_source: "pane.send_input",
                    timestamp: "2026-06-14T00:00:03Z",
                    payload: serde_json::json!({}),
                },
            )
            .await?;
            let duplicate = append_delivery_event_async(
                &mut conn,
                DeliveryEventInput {
                    message_id: &submitted.message_id,
                    event_type: DeliveryEventType::Submitted,
                    proof_source: "pane.send_input",
                    timestamp: "2026-06-14T00:00:04Z",
                    payload: serde_json::json!({}),
                },
            )
            .await
            .unwrap_err();
            assert_eq!(duplicate.code, "invalid_delivery_transition");

            let wrong_after_submitted = append_delivery_event_async(
                &mut conn,
                DeliveryEventInput {
                    message_id: &submitted.message_id,
                    event_type: DeliveryEventType::Drafted,
                    proof_source: "pane.send_text",
                    timestamp: "2026-06-14T00:00:05Z",
                    payload: serde_json::json!({}),
                },
            )
            .await
            .unwrap_err();
            assert_eq!(wrong_after_submitted.code, "invalid_delivery_transition");

            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn derived_parent_id_targets_latest_message_from_reply_target() {
        crate::zynk::db::block_on(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let first = create_test_message_between(
                &mut conn,
                "msg_alice",
                SendCommand::AgentSend,
                "alice",
                "bob",
            )
            .await;
            let reply = create_test_message_between(
                &mut conn,
                "msg_bob",
                SendCommand::AgentSend,
                "bob",
                "alice",
            )
            .await;
            assert_eq!(first.conversation_id, reply.conversation_id);
            assert_eq!(reply.conversation_seq, 2);
            let row = sqlx::query("SELECT derived_parent_id FROM messages WHERE id = ?")
                .bind(&reply.message_id)
                .fetch_one(&mut conn)
                .await?;
            let parent = row.try_get::<String, _>("derived_parent_id")?;
            assert_eq!(parent, first.message_id);
            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn participant_key_is_stable_for_same_party() {
        let party = Party {
            agent: Some("codex".into()),
            pane: Some("pane-1".into()),
            terminal_id: Some("term-1".into()),
            agent_session: Some(serde_json::json!({"source":"hook","kind":"id","value":"s1"})),
            workspace: None,
            tab: None,
            cwd: None,
            branch: None,
            git_sha: None,
        };
        let a = participant_fields(&party);
        let b = participant_fields(&party);
        assert_eq!(a.participant_key, b.participant_key);
        assert_eq!(a.agent_label, "codex");
        assert_eq!(a.agent_session_kind.as_deref(), Some("id"));
    }

    #[test]
    fn participant_key_ignores_pane_churn_for_same_agent_terminal_session() {
        let mut a = Party {
            agent: Some("codex".into()),
            pane: Some("pane-1".into()),
            terminal_id: Some("term-1".into()),
            agent_session: Some(serde_json::json!({"source":"hook","kind":"id","value":"s1"})),
            ..Party::default()
        };
        let mut b = a.clone();
        b.pane = Some("pane-2".into());
        assert_eq!(
            participant_fields(&a).participant_key,
            participant_fields(&b).participant_key
        );
        a.terminal_id = Some("term-2".into());
        assert_ne!(
            participant_fields(&a).participant_key,
            participant_fields(&b).participant_key
        );
    }

    #[test]
    fn shell_party_uses_pane_label() {
        let party = Party {
            pane: Some("pane-1".into()),
            ..Party::default()
        };
        assert_eq!(participant_fields(&party).agent_label, "pane:pane-1");
    }

    async fn append_event(
        conn: &mut SqliteConnection,
        message_id: &str,
        event_type: DeliveryEventType,
        proof_source: &str,
        timestamp: &str,
    ) -> Result<(), DbError> {
        append_delivery_event_async(
            conn,
            DeliveryEventInput {
                message_id,
                event_type,
                proof_source,
                timestamp,
                payload: serde_json::json!({}),
            },
        )
        .await
    }

    #[test]
    fn delivery_transition_validation_covers_m3a_received_matrix() {
        crate::zynk::db::block_on(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;

            // none -> received : rejected (a receipt requires a prior submission).
            let orphan = create_test_message(&mut conn, "msg_orphan", SendCommand::PaneRun).await;
            let none_to_received = append_event(
                &mut conn,
                &orphan.message_id,
                DeliveryEventType::Received,
                "integration",
                "2026-06-14T00:00:00Z",
            )
            .await
            .unwrap_err();
            assert_eq!(none_to_received.code, "invalid_delivery_transition");

            // submitted -> received : accepted.
            let m = create_test_message(&mut conn, "msg_recv", SendCommand::PaneRun).await;
            append_event(
                &mut conn,
                &m.message_id,
                DeliveryEventType::Submitted,
                "pane.send_input",
                "2026-06-14T00:00:01Z",
            )
            .await?;
            append_event(
                &mut conn,
                &m.message_id,
                DeliveryEventType::Received,
                "integration",
                "2026-06-14T00:00:02Z",
            )
            .await?;

            // received -> received : rejected at the validator (the receipt handler
            // returns already_received before reaching the validator; a direct append
            // must still reject the duplicate).
            let dup = append_event(
                &mut conn,
                &m.message_id,
                DeliveryEventType::Received,
                "integration",
                "2026-06-14T00:00:03Z",
            )
            .await
            .unwrap_err();
            assert_eq!(dup.code, "invalid_delivery_transition");

            // received -> submitted : rejected (no regression to an earlier state).
            let regress = append_event(
                &mut conn,
                &m.message_id,
                DeliveryEventType::Submitted,
                "pane.send_input",
                "2026-06-14T00:00:04Z",
            )
            .await
            .unwrap_err();
            assert_eq!(regress.code, "invalid_delivery_transition");

            // drafted -> received : rejected (a draft was never submitted).
            let d = create_test_message(&mut conn, "msg_draft", SendCommand::PaneSendText).await;
            append_event(
                &mut conn,
                &d.message_id,
                DeliveryEventType::Drafted,
                "pane.send_text",
                "2026-06-14T00:00:05Z",
            )
            .await?;
            let drafted_to_received = append_event(
                &mut conn,
                &d.message_id,
                DeliveryEventType::Received,
                "integration",
                "2026-06-14T00:00:06Z",
            )
            .await
            .unwrap_err();
            assert_eq!(drafted_to_received.code, "invalid_delivery_transition");

            // failed -> received : rejected (terminal; retry creates a new message).
            let f = create_test_message(&mut conn, "msg_failed", SendCommand::PaneSendText).await;
            append_event(
                &mut conn,
                &f.message_id,
                DeliveryEventType::Drafted,
                "pane.send_text",
                "2026-06-14T00:00:07Z",
            )
            .await?;
            append_event(
                &mut conn,
                &f.message_id,
                DeliveryEventType::Failed,
                "pane.send_text",
                "2026-06-14T00:00:08Z",
            )
            .await?;
            let failed_to_received = append_event(
                &mut conn,
                &f.message_id,
                DeliveryEventType::Received,
                "integration",
                "2026-06-14T00:00:09Z",
            )
            .await
            .unwrap_err();
            assert_eq!(failed_to_received.code, "invalid_delivery_transition");

            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn protocol_json_carries_protocol_ids_uniformly_incl_drafts() {
        crate::zynk::db::block_on(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let from = Party {
                agent: Some("alice".into()),
                workspace: Some("ws".into()),
                tab: Some("tab".into()),
                ..Party::default()
            };
            let to = Party {
                agent: Some("bob".into()),
                workspace: Some("ws".into()),
                tab: Some("tab".into()),
                ..Party::default()
            };
            // `pane send-text` DRAFT: protocol_json still carries protocol IDs (ADR 0005 uniformity).
            let rec = begin_send_attempt_async(
                &mut conn,
                SendAttempt {
                    command: SendCommand::PaneSendText,
                    message_id: "msg_protocol",
                    target_arg: "bob",
                    from: &from,
                    to: &to,
                    message_type: Some("review"),
                    body: "zbodysentinel hello world",
                    created_at: "2026-06-14T00:00:00Z",
                    trace_id: None,
                },
                "rt_test".into(),
                "socket_test".into(),
            )
            .await?;

            let row =
                sqlx::query("SELECT protocol_json, body, body_hash FROM messages WHERE id = ?")
                    .bind("msg_protocol")
                    .fetch_one(&mut conn)
                    .await?;
            let protocol: serde_json::Value =
                serde_json::from_str(&row.try_get::<String, _>("protocol_json")?).unwrap();
            assert_eq!(protocol["command"], "pane send-text");
            assert_eq!(protocol["v"], 1);
            assert_eq!(protocol["message_id"], "msg_protocol");
            assert_eq!(protocol["conversation_id"], rec.conversation_id);
            assert_eq!(protocol["conversation_seq"], rec.conversation_seq);
            assert_eq!(protocol["runtime_session_id"], "rt_test");
            assert_eq!(protocol["socket_namespace"], "socket_test");
            assert_eq!(protocol["body_hash"], rec.body_hash);
            assert_eq!(protocol["type"], "review");

            // body + body_hash stay PURE (no protocol fields in the persisted body).
            assert_eq!(
                row.try_get::<String, _>("body")?,
                "zbodysentinel hello world"
            );
            assert_eq!(row.try_get::<String, _>("body_hash")?, rec.body_hash);

            // FTS: the body token IS indexed; a protocol_json-only token (the body_hash
            // hex — a single unicode61 token) is NOT in the FTS index.
            let body_hits = sqlx::query(
                "SELECT COUNT(*) AS c FROM messages_fts WHERE messages_fts MATCH 'zbodysentinel'",
            )
            .fetch_one(&mut conn)
            .await?
            .try_get::<i64, _>("c")?;
            assert_eq!(body_hits, 1, "body token IS indexed");
            let protocol_hits =
                sqlx::query("SELECT COUNT(*) AS c FROM messages_fts WHERE messages_fts MATCH ?")
                    .bind(&rec.body_hash)
                    .fetch_one(&mut conn)
                    .await?
                    .try_get::<i64, _>("c")?;
            assert_eq!(protocol_hits, 0, "protocol_json body_hash is NOT in FTS");

            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn send_enqueues_one_pending_embedding_job() {
        crate::zynk::db::block_on(async {
            // Provider unset → the fake default; `active_model_id()` must yield "fake@1".
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            let sent = create_test_message(&mut conn, "msg_ejob", SendCommand::PaneRun).await;

            let row = sqlx::query(
                "SELECT id, model_id, status, attempts, enqueued_at FROM embedding_jobs WHERE message_id = ?",
            )
            .bind(&sent.message_id)
            .fetch_all(&mut conn)
            .await?;
            assert_eq!(row.len(), 1, "exactly one embedding job per sent message");
            let job = &row[0];
            assert!(
                job.try_get::<String, _>("id")?.starts_with("ejob_"),
                "job id uses the ejob prefix"
            );
            assert_eq!(job.try_get::<String, _>("model_id")?, "fake@1");
            // The send did ZERO embedding compute: the job is queued PENDING, never 'done'.
            // This is the by-construction "send never blocks on embedding" guarantee.
            assert_eq!(job.try_get::<String, _>("status")?, "pending");
            assert_ne!(job.try_get::<String, _>("status")?, "done");
            assert_eq!(job.try_get::<i64, _>("attempts")?, 0);
            // enqueued_at is the send's created_at (enqueued in the same txn as the message).
            assert_eq!(
                job.try_get::<String, _>("enqueued_at")?,
                "2026-06-14T00:00:00Z"
            );

            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn failed_send_leaves_no_orphan_embedding_job() {
        crate::zynk::db::block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;

            // First send of "dup" succeeds → 1 message, 1 job.
            create_test_message(&mut conn, "dup", SendCommand::PaneRun).await;

            // Second send with the SAME message_id violates the messages PK; the whole
            // txn (message + FTS + enqueue) rolls back atomically.
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
            let err = begin_send_attempt_async(
                &mut conn,
                SendAttempt {
                    command: SendCommand::PaneRun,
                    message_id: "dup",
                    target_arg: "bob",
                    from: &from,
                    to: &to,
                    message_type: None,
                    body: "body",
                    created_at: "2026-06-14T00:00:01Z",
                    trace_id: None,
                },
                "rt_test".into(),
                "socket_test".into(),
            )
            .await;
            assert!(err.is_err(), "re-sending the same message_id must fail");

            // The failed attempt created NO orphan: still EXACTLY 1 message + 1 job for "dup".
            let messages = sqlx::query("SELECT COUNT(*) AS c FROM messages WHERE id = ?")
                .bind("dup")
                .fetch_one(&mut conn)
                .await?
                .try_get::<i64, _>("c")?;
            assert_eq!(messages, 1, "exactly one message row for dup");
            let jobs = sqlx::query("SELECT COUNT(*) AS c FROM embedding_jobs WHERE message_id = ?")
                .bind("dup")
                .fetch_one(&mut conn)
                .await?
                .try_get::<i64, _>("c")?;
            assert_eq!(
                jobs, 1,
                "the rolled-back retry left no orphan embedding job"
            );

            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    // --- Feature #107 (IM1): per-message trace_id in meta_json ---

    /// Send `body` from `from_label` to `to_label` with an optional `trace_id`, scoped to
    /// the (ws, tab) so the active-conversation lookup the inherit path uses lines up.
    #[allow(clippy::too_many_arguments)]
    async fn send_with_trace(
        conn: &mut SqliteConnection,
        message_id: &str,
        command: SendCommand,
        from_label: &str,
        to_label: &str,
        body: &str,
        created_at: &str,
        trace_id: Option<&str>,
    ) -> PersistedSend {
        let from = Party {
            agent: Some(from_label.into()),
            workspace: Some("ws".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        let to = Party {
            agent: Some(to_label.into()),
            workspace: Some("ws".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        begin_send_attempt_async(
            conn,
            SendAttempt {
                command,
                message_id,
                target_arg: to_label,
                from: &from,
                to: &to,
                message_type: None,
                body,
                created_at,
                trace_id: trace_id.map(str::to_string),
            },
            "rt_test".into(),
            "socket_test".into(),
        )
        .await
        .unwrap()
    }

    #[test]
    fn trace_id_persists_in_meta_json_only_and_body_stays_pure() {
        crate::zynk::db::block_on(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;

            // Two sends with the IDENTICAL body: one WITH a trace, one WITHOUT.
            let with_trace = send_with_trace(
                &mut conn,
                "msg_trace",
                SendCommand::ZynkSend,
                "alice",
                "bob",
                "zbodysentinel hello",
                "2026-06-14T00:00:00Z",
                Some("trace-abc-123"),
            )
            .await;
            let no_trace = send_with_trace(
                &mut conn,
                "msg_notrace",
                SendCommand::ZynkSend,
                "carol",
                "dave",
                "zbodysentinel hello",
                "2026-06-14T00:00:01Z",
                None,
            )
            .await;

            // The trace is carried on the returned record (IM3 reads this).
            assert_eq!(with_trace.trace_id.as_deref(), Some("trace-abc-123"));
            assert_eq!(no_trace.trace_id, None);

            // trace_id lives in meta_json ONLY: json_extract returns it; the no-trace row's
            // meta_json has NO trace_id (the canonical empty object).
            let traced = sqlx::query(
                "SELECT json_extract(meta_json, '$.trace_id') AS t, meta_json, body, body_hash, protocol_json FROM messages WHERE id = ?",
            )
            .bind("msg_trace")
            .fetch_one(&mut conn)
            .await?;
            assert_eq!(
                traced.try_get::<String, _>("t")?,
                "trace-abc-123",
                "meta_json.trace_id is the sent id"
            );

            let untraced = sqlx::query(
                "SELECT json_extract(meta_json, '$.trace_id') AS t, meta_json, body, body_hash, protocol_json FROM messages WHERE id = ?",
            )
            .bind("msg_notrace")
            .fetch_one(&mut conn)
            .await?;
            assert!(
                untraced.try_get::<Option<String>, _>("t")?.is_none(),
                "a no-trace send has NO meta_json.trace_id"
            );
            assert_eq!(untraced.try_get::<String, _>("meta_json")?, "{}");

            // body + body_hash are BYTE-IDENTICAL with vs without a trace (FTS purity).
            assert_eq!(
                traced.try_get::<String, _>("body")?,
                untraced.try_get::<String, _>("body")?,
                "body is identical with or without a trace"
            );
            assert_eq!(
                traced.try_get::<String, _>("body_hash")?,
                untraced.try_get::<String, _>("body_hash")?,
                "body_hash is identical with or without a trace"
            );
            assert_eq!(with_trace.body_hash, no_trace.body_hash);

            // protocol_json does NOT carry the trace (it is meta_json-only).
            assert!(
                !traced
                    .try_get::<String, _>("protocol_json")?
                    .contains("trace-abc-123"),
                "protocol_json must NOT contain the trace id"
            );

            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn trace_id_is_json_escape_safe_for_quotes_and_unicode() {
        crate::zynk::db::block_on(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;
            // A trace id with a double-quote + backslash + unicode: serde_json must escape it
            // so meta_json is valid JSON and json_extract round-trips the EXACT bytes.
            let weird = r#"a"b\c—é"#;
            send_with_trace(
                &mut conn,
                "msg_weird",
                SendCommand::ZynkSend,
                "alice",
                "bob",
                "body",
                "2026-06-14T00:00:00Z",
                Some(weird),
            )
            .await;
            let row = sqlx::query(
                "SELECT json_extract(meta_json, '$.trace_id') AS t, meta_json FROM messages WHERE id = ?",
            )
            .bind("msg_weird")
            .fetch_one(&mut conn)
            .await?;
            assert_eq!(row.try_get::<String, _>("t")?, weird);
            // meta_json itself parses as valid JSON.
            let meta: serde_json::Value =
                serde_json::from_str(&row.try_get::<String, _>("meta_json")?).unwrap();
            assert_eq!(meta["trace_id"], weird);
            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn inherit_copies_parent_trace_and_is_null_safe() {
        crate::zynk::db::block_on(async {
            let path = temp_db_path();
            let mut conn = crate::zynk::db::open_migrated_at(&path).await?;

            // alice -> bob WITH a trace. The parent of a bob->alice reply (the latest message
            // FROM the reply-target `alice`... here we reply TO bob, so parent = latest from bob).
            // Model the realistic flow: bob sends to alice WITH a trace; alice replies to bob and
            // INHERITS bob's trace (parent = latest message from `bob`, the reply target).
            send_with_trace(
                &mut conn,
                "msg_bob1",
                SendCommand::ZynkSend,
                "bob",
                "alice",
                "from bob",
                "2026-06-14T00:00:00Z",
                Some("trace-parent-9"),
            )
            .await;

            // Resolve the inherited trace the way the CLI does: parent = latest message from
            // the reply TARGET (`bob`) in the active conversation for this scope.
            let from_alice = Party {
                agent: Some("alice".into()),
                workspace: Some("ws".into()),
                tab: Some("tab".into()),
                ..Party::default()
            };
            let to_bob = Party {
                agent: Some("bob".into()),
                workspace: Some("ws".into()),
                tab: Some("tab".into()),
                ..Party::default()
            };
            let inherited = parent_trace_id_async(
                &mut conn,
                "rt_test",
                "socket_test",
                "ws",
                "tab",
                &participant_fields(&to_bob).agent_label,
            )
            .await?;
            assert_eq!(
                inherited.as_deref(),
                Some("trace-parent-9"),
                "inherit copies the parent (latest-from-target) trace"
            );

            // Persist the inherited reply; it lands in meta_json like an explicit trace.
            let reply = send_with_trace(
                &mut conn,
                "msg_reply",
                SendCommand::ZynkReply,
                "alice",
                "bob",
                "reply body",
                "2026-06-14T00:00:02Z",
                inherited.as_deref(),
            )
            .await;
            assert_eq!(reply.trace_id.as_deref(), Some("trace-parent-9"));

            // NULL-safe: when the parent has NO trace, inherit yields None (no panic, no trace).
            let _ = &from_alice; // keep the realistic source party in view.
            send_with_trace(
                &mut conn,
                "msg_bob2",
                SendCommand::ZynkSend,
                "bob",
                "alice",
                "from bob untraced",
                "2026-06-14T00:00:03Z",
                None,
            )
            .await;
            let inherited_none = parent_trace_id_async(
                &mut conn,
                "rt_test",
                "socket_test",
                "ws",
                "tab",
                &participant_fields(&to_bob).agent_label,
            )
            .await?;
            assert_eq!(
                inherited_none, None,
                "parent with no trace → inherit yields None (sends without a trace)"
            );

            // No parent at all (a different agent target) also yields None, never a panic.
            let no_parent =
                parent_trace_id_async(&mut conn, "rt_test", "socket_test", "ws", "tab", "nobody")
                    .await?;
            assert_eq!(no_parent, None, "no parent message → None");

            let _ = std::fs::remove_file(path);
            Ok(())
        })
        .unwrap();
    }
}
