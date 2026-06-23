//! zynk fork (M6 / ADR 0007 §2): read-only `thread` + `inbox` queries.
//!
//! Both open the global DB via [`crate::zynk::db::open_query_readonly`]
//! (`PRAGMA query_only=1`), so a read can NEVER synthesize a delivery/recovery event
//! (the read-only + receipts-server-authoritative invariant). Both are RUNTIME-SCOPED
//! on the active `socket_namespace` (never conflate runtimes). They read
//! `messages.body` + participant labels + the latest `delivery_events` row only — no
//! header pollution. Responses are F4-enveloped (`result`/`command` first; failures
//! carry `code`/`message`/`context`; success carries the payload).

use sqlx::{Row, SqliteConnection};

use crate::zynk::db::{self, DbError};

/// One transcript/inbox row (the honest, body-pure projection of a message).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ThreadMessage {
    pub message_id: String,
    pub conversation_id: String,
    pub conversation_seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_parent_id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    pub from: String,
    pub to: String,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub created_at: String,
    pub body: String,
    /// Feature #107 (IM2): the per-message trace id (`meta_json.$.trace_id`), surfaced
    /// when present and omitted when the message carries no trace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// The honest latest delivery state (the most-recent `delivery_events` row), or
    /// `None` when no event has landed yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<String>,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadStatus {
    Ok,
    Failed,
}

/// The F4-enveloped `thread` response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ThreadResponse {
    pub result: ReadStatus,
    pub command: &'static str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub response_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<ThreadMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    pub next: String,
}

const THREAD_COMMAND: &str = "zynk thread";
const THREAD_RESPONSE_TYPE: &str = "zynk_thread_result";

impl ThreadResponse {
    fn ok(conversation_id: String, socket_namespace: String, messages: Vec<ThreadMessage>) -> Self {
        let count = messages.len();
        let next = if count == 0 {
            "no messages in this conversation for the active runtime scope".to_string()
        } else {
            "ordered transcript (conversation_seq ascending; follow derived_parent_id)".to_string()
        };
        ThreadResponse {
            result: ReadStatus::Ok,
            command: THREAD_COMMAND,
            response_type: Some(THREAD_RESPONSE_TYPE),
            conversation_id: Some(conversation_id),
            socket_namespace: Some(socket_namespace),
            count: Some(count),
            messages: Some(messages),
            code: None,
            message: None,
            context: None,
            next,
        }
    }

    fn failed(
        code: &str,
        message: impl Into<String>,
        context: serde_json::Value,
        next: &str,
    ) -> Self {
        ThreadResponse {
            result: ReadStatus::Failed,
            command: THREAD_COMMAND,
            response_type: None,
            conversation_id: None,
            socket_namespace: None,
            count: None,
            messages: None,
            code: Some(code.to_string()),
            message: Some(message.into()),
            context: Some(context),
            next: next.to_string(),
        }
    }

    /// A missing/empty selector (the conversation or message id).
    pub fn missing_selector() -> Self {
        Self::failed(
            "invalid_filter",
            "thread requires a <conversation_id> or <message_id>",
            serde_json::json!({}),
            "pass a conversation id or a message id",
        )
    }

    /// The selector did not resolve to any conversation in the active runtime scope.
    pub fn not_found(selector: &str) -> Self {
        Self::failed(
            "thread_not_found",
            format!("no conversation or message '{selector}' in the active runtime scope"),
            serde_json::json!({ "selector": selector }),
            "check the id, or that it belongs to this runtime (socket_namespace)",
        )
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.result, ReadStatus::Failed)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"result\":\"failed\"}".to_string())
    }

    pub fn to_human(&self) -> String {
        if self.is_failed() {
            return format!(
                "thread failed [{}]: {}",
                self.code.as_deref().unwrap_or("error"),
                self.message.as_deref().unwrap_or(""),
            );
        }
        let msgs = self.messages.as_deref().unwrap_or(&[]);
        let mut out = String::new();
        out.push_str(&format!(
            "conversation {}\n",
            self.conversation_id.as_deref().unwrap_or("?")
        ));
        for m in msgs {
            out.push_str(&format!(
                "#{}  {}  {}→{}  [{}]  {}\n    {}\n",
                m.conversation_seq,
                m.message_id,
                m.from,
                m.to,
                m.delivery_status.as_deref().unwrap_or("?"),
                m.message_type.as_deref().unwrap_or("-"),
                m.body,
            ));
        }
        out.push_str(&format!("{} message(s)", msgs.len()));
        out
    }
}

/// The F4-enveloped `inbox` response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InboxResponse {
    pub result: ReadStatus,
    pub command: &'static str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub response_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<ThreadMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    pub next: String,
}

const INBOX_COMMAND: &str = "zynk inbox";
const INBOX_RESPONSE_TYPE: &str = "zynk_inbox_result";

impl InboxResponse {
    fn ok(agent: String, socket_namespace: String, messages: Vec<ThreadMessage>) -> Self {
        let count = messages.len();
        let next = if count == 0 {
            "no messages addressed to this agent in the active runtime scope".to_string()
        } else {
            "messages addressed to you (most recent first) with their honest delivery_status"
                .to_string()
        };
        InboxResponse {
            result: ReadStatus::Ok,
            command: INBOX_COMMAND,
            response_type: Some(INBOX_RESPONSE_TYPE),
            agent: Some(agent),
            socket_namespace: Some(socket_namespace),
            count: Some(count),
            messages: Some(messages),
            code: None,
            message: None,
            context: None,
            next,
        }
    }

    fn failed(
        code: &str,
        message: impl Into<String>,
        context: serde_json::Value,
        next: &str,
    ) -> Self {
        InboxResponse {
            result: ReadStatus::Failed,
            command: INBOX_COMMAND,
            response_type: None,
            agent: None,
            socket_namespace: None,
            count: None,
            messages: None,
            code: Some(code.to_string()),
            message: Some(message.into()),
            context: Some(context),
            next: next.to_string(),
        }
    }

    /// The caller could not be identified (no `--agent` and no live pane identity).
    pub fn unidentified_caller(detail: impl Into<String>) -> Self {
        Self::failed(
            "caller_unidentified",
            detail,
            serde_json::json!({}),
            "pass --agent <me>, or run from an agent pane so ZYNK_PANE_ID resolves your identity",
        )
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.result, ReadStatus::Failed)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"result\":\"failed\"}".to_string())
    }

    pub fn to_human(&self) -> String {
        if self.is_failed() {
            return format!(
                "inbox failed [{}]: {}",
                self.code.as_deref().unwrap_or("error"),
                self.message.as_deref().unwrap_or(""),
            );
        }
        let msgs = self.messages.as_deref().unwrap_or(&[]);
        let mut out = String::new();
        out.push_str(&format!(
            "inbox for {}\n",
            self.agent.as_deref().unwrap_or("?")
        ));
        for m in msgs {
            out.push_str(&format!(
                "{}  {}→{}  [{}]  {}\n    {}\n",
                m.message_id,
                m.from,
                m.to,
                m.delivery_status.as_deref().unwrap_or("?"),
                m.message_type.as_deref().unwrap_or("-"),
                m.body,
            ));
        }
        out.push_str(&format!("{} message(s)", msgs.len()));
        out
    }
}

/// The F4-enveloped `trace` response (Feature #107 IM2). Mirrors `InboxResponse`
/// but keyed on a `trace` id: it lists every message carrying that trace id across
/// conversations in the active runtime scope, ordered oldest-first.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceResponse {
    pub result: ReadStatus,
    pub command: &'static str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub response_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<ThreadMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    pub next: String,
}

const TRACE_COMMAND: &str = "zynk trace";
const TRACE_RESPONSE_TYPE: &str = "zynk_trace_result";

impl TraceResponse {
    fn ok(trace: String, socket_namespace: String, messages: Vec<ThreadMessage>) -> Self {
        let count = messages.len();
        let next = if count == 0 {
            "no messages with this trace id in the active runtime scope".to_string()
        } else {
            "messages sharing this trace id across conversations (oldest first)".to_string()
        };
        TraceResponse {
            result: ReadStatus::Ok,
            command: TRACE_COMMAND,
            response_type: Some(TRACE_RESPONSE_TYPE),
            trace: Some(trace),
            socket_namespace: Some(socket_namespace),
            count: Some(count),
            messages: Some(messages),
            code: None,
            message: None,
            context: None,
            next,
        }
    }

    fn failed(
        code: &str,
        message: impl Into<String>,
        context: serde_json::Value,
        next: &str,
    ) -> Self {
        TraceResponse {
            result: ReadStatus::Failed,
            command: TRACE_COMMAND,
            response_type: None,
            trace: None,
            socket_namespace: None,
            count: None,
            messages: None,
            code: Some(code.to_string()),
            message: Some(message.into()),
            context: Some(context),
            next: next.to_string(),
        }
    }

    /// A missing/empty/invalid trace id (rejected before any DB access). `code` lets the
    /// CLI surface the same `invalid_trace_id` the send path uses on a malformed id.
    pub fn invalid_trace(
        code: &str,
        message: impl Into<String>,
        context: serde_json::Value,
    ) -> Self {
        Self::failed(
            code,
            message,
            context,
            "pass a printable trace id (see `zynk send --trace`)",
        )
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.result, ReadStatus::Failed)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"result\":\"failed\"}".to_string())
    }

    pub fn to_human(&self) -> String {
        if self.is_failed() {
            return format!(
                "trace failed [{}]: {}",
                self.code.as_deref().unwrap_or("error"),
                self.message.as_deref().unwrap_or(""),
            );
        }
        let msgs = self.messages.as_deref().unwrap_or(&[]);
        let mut out = String::new();
        out.push_str(&format!("trace {}\n", self.trace.as_deref().unwrap_or("?")));
        for m in msgs {
            out.push_str(&format!(
                "{}#{}  {}  {}→{}  [{}]  {}\n    {}\n",
                m.conversation_id,
                m.conversation_seq,
                m.message_id,
                m.from,
                m.to,
                m.delivery_status.as_deref().unwrap_or("?"),
                m.message_type.as_deref().unwrap_or("-"),
                m.body,
            ));
        }
        out.push_str(&format!("{} message(s)", msgs.len()));
        out
    }
}

/// Common SELECT projection for the body-pure transcript rows.
const MESSAGE_SELECT: &str = "\
SELECT \
  m.id AS id, \
  m.conversation_id AS conversation_id, \
  m.conversation_seq AS conversation_seq, \
  m.derived_parent_id AS derived_parent_id, \
  m.type AS type, \
  m.created_at AS created_at, \
  m.workspace_id AS workspace_id, \
  m.tab_id AS tab_id, \
  m.branch AS branch, \
  m.cwd AS cwd, \
  m.body AS body, \
  fp.agent_label AS from_agent, \
  tp.agent_label AS to_agent, \
  json_extract(m.meta_json, '$.trace_id') AS trace_id, \
  (SELECT de.event_type FROM delivery_events de WHERE de.message_id = m.id ORDER BY de.seq DESC LIMIT 1) AS delivery_status \
FROM messages m \
JOIN conversation_participants fp ON fp.id = m.from_participant_id \
JOIN conversation_participants tp ON tp.id = m.to_participant_id ";

fn row_to_message(row: &sqlx::sqlite::SqliteRow) -> Result<ThreadMessage, DbError> {
    Ok(ThreadMessage {
        message_id: row.try_get("id")?,
        conversation_id: row.try_get("conversation_id")?,
        conversation_seq: row.try_get("conversation_seq")?,
        derived_parent_id: row.try_get::<Option<String>, _>("derived_parent_id")?,
        message_type: row.try_get::<Option<String>, _>("type")?,
        from: row.try_get("from_agent")?,
        to: row.try_get("to_agent")?,
        workspace_id: row.try_get("workspace_id")?,
        tab_id: row.try_get("tab_id")?,
        branch: row.try_get::<Option<String>, _>("branch")?,
        cwd: row.try_get::<Option<String>, _>("cwd")?,
        created_at: row.try_get("created_at")?,
        body: row.try_get("body")?,
        trace_id: row.try_get::<Option<String>, _>("trace_id")?,
        delivery_status: row.try_get::<Option<String>, _>("delivery_status")?,
    })
}

/// Resolve a `<conversation_id | message_id>` selector to a conversation id WITHIN the
/// active runtime scope. A conversation id matches directly; otherwise we look up a
/// message by id and take its conversation. Returns `None` when nothing matches the
/// runtime scope (so the caller emits `thread_not_found`).
async fn resolve_conversation_id(
    conn: &mut SqliteConnection,
    selector: &str,
    socket_namespace: &str,
) -> Result<Option<String>, DbError> {
    if let Some(row) =
        sqlx::query("SELECT id FROM conversations WHERE id = ? AND socket_namespace = ? LIMIT 1")
            .bind(selector)
            .bind(socket_namespace)
            .fetch_optional(&mut *conn)
            .await?
    {
        return Ok(Some(row.try_get::<String, _>("id")?));
    }
    if let Some(row) = sqlx::query(
        "SELECT conversation_id FROM messages WHERE id = ? AND socket_namespace = ? LIMIT 1",
    )
    .bind(selector)
    .bind(socket_namespace)
    .fetch_optional(&mut *conn)
    .await?
    {
        return Ok(Some(row.try_get::<String, _>("conversation_id")?));
    }
    Ok(None)
}

/// Read the ordered transcript for a conversation/message selector, runtime-scoped.
/// READ-ONLY (`open_query_readonly`); writes nothing.
pub fn run_thread(selector: &str) -> ThreadResponse {
    let selector = selector.trim();
    if selector.is_empty() {
        return ThreadResponse::missing_selector();
    }
    let socket_namespace = crate::zynk::runtime::socket_namespace();
    let result = db::block_on(async {
        let mut conn = db::open_query_readonly().await?;
        let Some(conversation_id) =
            resolve_conversation_id(&mut conn, selector, &socket_namespace).await?
        else {
            return Ok(None);
        };
        let rows = sqlx::query(&format!(
            "{MESSAGE_SELECT} WHERE m.conversation_id = ? AND m.socket_namespace = ? ORDER BY m.conversation_seq ASC"
        ))
        .bind(&conversation_id)
        .bind(&socket_namespace)
        .fetch_all(&mut conn)
        .await?;
        let mut messages = Vec::with_capacity(rows.len());
        for row in &rows {
            messages.push(row_to_message(row)?);
        }
        Ok(Some((conversation_id, messages)))
    });

    match result {
        Ok(Some((conversation_id, messages))) => {
            ThreadResponse::ok(conversation_id, socket_namespace, messages)
        }
        Ok(None) => ThreadResponse::not_found(selector),
        Err(err) => ThreadResponse::failed(
            err.code,
            err.message,
            serde_json::json!({ "selector": selector }),
            "the global DB could not be read; check the DB path/permissions",
        ),
    }
}

/// List messages addressed to `agent` (the caller), most recent first, runtime-scoped.
/// READ-ONLY (`open_query_readonly`); writes nothing.
pub fn run_inbox(agent: &str, limit: usize) -> InboxResponse {
    let agent = agent.trim();
    if agent.is_empty() {
        return InboxResponse::unidentified_caller("the caller agent label is empty");
    }
    let socket_namespace = crate::zynk::runtime::socket_namespace();
    let limit = limit.clamp(1, 500) as i64;
    let result = db::block_on(async {
        let mut conn = db::open_query_readonly().await?;
        let rows = sqlx::query(&format!(
            "{MESSAGE_SELECT} WHERE m.socket_namespace = ? AND tp.agent_label = ? ORDER BY m.created_at DESC, m.conversation_seq DESC LIMIT ?"
        ))
        .bind(&socket_namespace)
        .bind(agent)
        .bind(limit)
        .fetch_all(&mut conn)
        .await?;
        let mut messages = Vec::with_capacity(rows.len());
        for row in &rows {
            messages.push(row_to_message(row)?);
        }
        Ok::<Vec<ThreadMessage>, DbError>(messages)
    });

    match result {
        Ok(messages) => InboxResponse::ok(agent.to_string(), socket_namespace, messages),
        Err(err) => InboxResponse::failed(
            err.code,
            err.message,
            serde_json::json!({ "agent": agent }),
            "the global DB could not be read; check the DB path/permissions",
        ),
    }
}

/// Feature #107 (IM2): list every message carrying `trace_id`, across conversations,
/// in the active runtime scope (oldest first). The `trace_id` is validated up front
/// with the SAME gate the send path uses (`validate_trace_id`) — an explicit error on
/// bad input, never a silent strip. The lookup keys on
/// `json_extract(meta_json, '$.trace_id') = ?`, served by the partial index
/// `idx_messages_trace_id` (migration 0003). READ-ONLY (`open_query_readonly`).
pub fn run_trace(raw_trace: &str) -> TraceResponse {
    let trace_id = match crate::zynk::message::validate_trace_id(raw_trace) {
        Ok(cleaned) => cleaned,
        Err((code, message)) => {
            return TraceResponse::invalid_trace(
                code,
                message,
                serde_json::json!({ "trace": raw_trace }),
            );
        }
    };
    let socket_namespace = crate::zynk::runtime::socket_namespace();
    let result = db::block_on(async {
        let mut conn = db::open_query_readonly().await?;
        let rows = sqlx::query(&format!(
            "{MESSAGE_SELECT} WHERE m.socket_namespace = ? \
             AND json_extract(m.meta_json, '$.trace_id') = ? \
             ORDER BY m.created_at ASC, m.conversation_seq ASC"
        ))
        .bind(&socket_namespace)
        .bind(&trace_id)
        .fetch_all(&mut conn)
        .await?;
        let mut messages = Vec::with_capacity(rows.len());
        for row in &rows {
            messages.push(row_to_message(row)?);
        }
        Ok::<Vec<ThreadMessage>, DbError>(messages)
    });

    match result {
        Ok(messages) => TraceResponse::ok(trace_id, socket_namespace, messages),
        Err(err) => TraceResponse::failed(
            err.code,
            err.message,
            serde_json::json!({ "trace": trace_id }),
            "the global DB could not be read; check the DB path/permissions",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_missing_selector_is_invalid_filter() {
        let resp = ThreadResponse::missing_selector();
        assert!(resp.is_failed());
        assert_eq!(resp.code.as_deref(), Some("invalid_filter"));
        // F4 envelope: command first, failures carry code/message/context.
        let v: serde_json::Value = serde_json::from_str(&resp.to_json()).unwrap();
        assert_eq!(v["result"], "failed");
        assert_eq!(v["command"], "zynk thread");
        assert!(v["context"].is_object());
    }

    #[test]
    fn thread_not_found_names_the_selector() {
        let resp = ThreadResponse::not_found("conv-xyz");
        assert!(resp.is_failed());
        assert_eq!(resp.code.as_deref(), Some("thread_not_found"));
        assert!(resp.message.as_deref().unwrap().contains("conv-xyz"));
    }

    #[test]
    fn empty_thread_selector_short_circuits_without_db() {
        // An empty selector must not touch the DB (pure validation).
        let resp = run_thread("   ");
        assert!(resp.is_failed());
        assert_eq!(resp.code.as_deref(), Some("invalid_filter"));
    }

    #[test]
    fn inbox_envelope_ok_shape() {
        let resp = InboxResponse::ok("codex".into(), "/tmp/s.sock".into(), vec![]);
        let v: serde_json::Value = serde_json::from_str(&resp.to_json()).unwrap();
        assert_eq!(v["result"], "ok");
        assert_eq!(v["command"], "zynk inbox");
        assert_eq!(v["agent"], "codex");
        assert_eq!(v["count"], 0);
        assert!(v["next"].is_string());
    }

    #[test]
    fn inbox_unidentified_caller_is_failed() {
        let resp = InboxResponse::unidentified_caller("no pane");
        assert!(resp.is_failed());
        assert_eq!(resp.code.as_deref(), Some("caller_unidentified"));
    }
}
