//! zynk fork (M5a): BM25 runner over the external-content `messages_fts`.
//!
//! Prefilters (plan §8) are bound in the `WHERE` clause so they restrict the
//! candidate set BEFORE ranking; ordering is `bm25(messages_fts) ASC` (SQLite's
//! `bm25()` is lower-is-better). Joins read `messages` by the same `rowid` the FTS
//! external-content table uses, plus `conversation_participants` for from/to labels
//! and the latest `delivery_events` row for `delivery_status`. Reads body/FTS only.

use sqlx::{Row, SqliteConnection};

use super::{QueryFilters, QueryHit};
use crate::zynk::db::DbError;

/// Classify an error from the BM25 SELECT. The ONLY user-variable input in this
/// fixed query is the FTS5 `MATCH` expression (every filter + the limit are bound
/// literals), so `SQLITE_ERROR` (primary code 1) is a malformed caller query
/// (`fts_query_error` → `invalid_query`), NOT an infra failure. `SQLITE_BUSY`
/// (code 5) is a concurrent-writer lock (`persistence_busy`); anything else maps
/// via the default. This routes on the structured code instead of sniffing the
/// human message (the bundled SQLite emits FTS5 parse errors as "unterminated
/// string" / "no such column: X" with no "fts5"/"syntax error" substring).
fn classify_fts_error(err: sqlx::Error) -> DbError {
    if let sqlx::Error::Database(db) = &err {
        match db.code().as_deref() {
            Some("5") => return DbError::new("persistence_busy", err.to_string()),
            Some("1") => return DbError::new("fts_query_error", err.to_string()),
            _ => {}
        }
    }
    DbError::from(err)
}

const BM25_SQL: &str = "\
SELECT \
  m.id AS id, \
  m.conversation_id AS conversation_id, \
  m.conversation_seq AS conversation_seq, \
  m.type AS type, \
  m.created_at AS created_at, \
  m.workspace_id AS workspace_id, \
  m.tab_id AS tab_id, \
  m.branch AS branch, \
  m.cwd AS cwd, \
  fp.agent_label AS from_agent, \
  tp.agent_label AS to_agent, \
  json_extract(m.meta_json, '$.trace_id') AS trace_id, \
  bm25(messages_fts) AS bm25_score, \
  snippet(messages_fts, 0, '[', ']', '…', 12) AS snippet, \
  (SELECT de.event_type FROM delivery_events de WHERE de.message_id = m.id ORDER BY de.seq DESC LIMIT 1) AS delivery_status \
FROM messages_fts \
JOIN messages m ON m.rowid = messages_fts.rowid \
JOIN conversation_participants fp ON fp.id = m.from_participant_id \
JOIN conversation_participants tp ON tp.id = m.to_participant_id \
WHERE messages_fts MATCH ? \
  AND (? IS NULL OR m.workspace_id = ?) \
  AND (? IS NULL OR m.conversation_id = ?) \
  AND (? IS NULL OR m.created_at >= ?) \
  AND (? IS NULL OR m.type = ?) \
  AND (? IS NULL OR m.branch = ?) \
  AND (? IS NULL OR m.cwd = ?) \
  AND (? IS NULL OR fp.agent_label = ? OR tp.agent_label = ?) \
  AND (? IS NULL OR json_extract(m.meta_json, '$.trace_id') = ?) \
ORDER BY bm25(messages_fts) ASC \
LIMIT ?";

/// Run the BM25 query against `messages_fts` and map rows to `QueryHit`s, ranked.
pub async fn bm25_search(
    conn: &mut SqliteConnection,
    query: &str,
    filters: &QueryFilters,
) -> Result<Vec<QueryHit>, DbError> {
    // --exact => an FTS5 phrase (double-quote, doubling internal quotes); default
    // passes the user query through FTS5 syntax (advanced operators allowed).
    let match_query = if filters.exact {
        format!("\"{}\"", query.replace('"', "\"\""))
    } else {
        query.to_string()
    };
    let limit = filters.effective_limit();

    let rows = sqlx::query(BM25_SQL)
        .bind(&match_query)
        .bind(&filters.workspace)
        .bind(&filters.workspace)
        .bind(&filters.conversation)
        .bind(&filters.conversation)
        .bind(&filters.since)
        .bind(&filters.since)
        .bind(&filters.message_type)
        .bind(&filters.message_type)
        .bind(&filters.branch)
        .bind(&filters.branch)
        .bind(&filters.cwd)
        .bind(&filters.cwd)
        .bind(&filters.agent)
        .bind(&filters.agent)
        .bind(&filters.agent)
        .bind(&filters.trace_id)
        .bind(&filters.trace_id)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await
        .map_err(classify_fts_error)?;

    let mut hits = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        hits.push(QueryHit {
            message_id: row.try_get("id")?,
            conversation_id: row.try_get("conversation_id")?,
            conversation_seq: row.try_get("conversation_seq")?,
            message_type: row.try_get::<Option<String>, _>("type")?,
            from: row.try_get("from_agent")?,
            to: row.try_get("to_agent")?,
            workspace_id: row.try_get("workspace_id")?,
            tab_id: row.try_get("tab_id")?,
            branch: row.try_get::<Option<String>, _>("branch")?,
            cwd: row.try_get::<Option<String>, _>("cwd")?,
            created_at: row.try_get("created_at")?,
            trace_id: row.try_get::<Option<String>, _>("trace_id")?,
            score: row.try_get::<f64, _>("bm25_score")?,
            matched_modes: vec!["bm25".to_string()],
            bm25_rank: Some((i as i64) + 1),
            vector_rank: None,
            snippet: row.try_get::<Option<String>, _>("snippet")?,
            delivery_status: row.try_get::<Option<String>, _>("delivery_status")?,
        });
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zynk::db::{block_on, open_migrated_at_without_recovery, DbError};
    use crate::zynk::message::{new_prefixed_id, Party, SendCommand};
    use crate::zynk::persistence::{begin_send_attempt_async, SendAttempt};

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "zynk-fts-trace-test-{}-{}.db",
            std::process::id(),
            new_prefixed_id("test")
        ))
    }

    /// Seed a message via the real send path, carrying an optional `trace_id` (written
    /// to `meta_json.$.trace_id` by the persistence layer, IM1).
    async fn seed(
        conn: &mut SqliteConnection,
        message_id: &str,
        body: &str,
        trace_id: Option<&str>,
    ) {
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
                message_type: Some("review"),
                body,
                created_at: "2026-06-16T00:00:00Z",
                trace_id: trace_id.map(str::to_string),
            },
            "rt_test".into(),
            "socket_test".into(),
        )
        .await
        .unwrap();
    }

    /// `--trace <id>` prefilter: returns ONLY messages carrying that trace; excludes a
    /// different-trace row AND an old/no-trace row (even when all match the FTS term).
    #[test]
    fn trace_prefilter_returns_only_matching_trace() {
        block_on(async {
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;

            // All three share the FTS term "shared", so only the --trace filter differs.
            seed(&mut conn, "msg_t1", "shared body alpha", Some("trace-A")).await;
            seed(&mut conn, "msg_t2", "shared body beta", Some("trace-B")).await;
            seed(&mut conn, "msg_t3", "shared body gamma", None).await; // old/no-trace row

            // Baseline: no trace filter → all 3 match the term.
            let all = bm25_search(&mut conn, "shared", &QueryFilters::default())
                .await
                .unwrap();
            assert_eq!(all.len(), 3, "all three share the FTS term");

            // --trace trace-A → only msg_t1.
            let filters = QueryFilters {
                trace_id: Some("trace-A".into()),
                ..QueryFilters::default()
            };
            let hits = bm25_search(&mut conn, "shared", &filters).await.unwrap();
            assert_eq!(hits.len(), 1, "only the trace-A message survives");
            assert_eq!(hits[0].message_id, "msg_t1");
            assert_eq!(hits[0].trace_id.as_deref(), Some("trace-A"));

            // A trace nobody carries → empty (and never the no-trace row).
            let none = QueryFilters {
                trace_id: Some("trace-Z".into()),
                ..QueryFilters::default()
            };
            let empty = bm25_search(&mut conn, "shared", &none).await.unwrap();
            assert!(empty.is_empty(), "an absent trace yields no rows");

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    /// `QueryHit.trace_id` is surfaced from `meta_json.$.trace_id` when present, and is
    /// `None` (→ omitted from JSON) when the message carries no trace.
    #[test]
    fn query_hit_surfaces_trace_id_present_and_absent() {
        block_on(async {
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;

            seed(
                &mut conn,
                "msg_has_trace",
                "needle one",
                Some("trace-present"),
            )
            .await;
            seed(&mut conn, "msg_no_trace", "needle two", None).await;

            let hits = bm25_search(&mut conn, "needle", &QueryFilters::default())
                .await
                .unwrap();
            assert_eq!(hits.len(), 2);

            let traced = hits
                .iter()
                .find(|h| h.message_id == "msg_has_trace")
                .unwrap();
            assert_eq!(traced.trace_id.as_deref(), Some("trace-present"));
            // Serializes the trace_id field when present.
            let v = serde_json::to_value(traced).unwrap();
            assert_eq!(v["trace_id"], "trace-present");

            let untraced = hits
                .iter()
                .find(|h| h.message_id == "msg_no_trace")
                .unwrap();
            assert_eq!(untraced.trace_id, None, "no-trace row → None");
            // Omitted from JSON (skip_serializing_if).
            let v2 = serde_json::to_value(untraced).unwrap();
            assert!(
                v2.get("trace_id").is_none(),
                "absent trace_id must be omitted from JSON, got {v2:?}"
            );

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }
}
