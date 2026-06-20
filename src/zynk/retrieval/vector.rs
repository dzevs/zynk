//! zynk fork (M5c): vec0 KNN vector runner — the vector half of the hybrid
//! `zynk query` pipeline.
//!
//! ## Candidate-pool + join-prefilter fallback (plan §6)
//!
//! The current vec0 table (`embedding_worker::ensure_model_and_vec0`) is declared
//! `vec0(message_rowid INTEGER PRIMARY KEY, embedding float[dim] ...)` — it has NO
//! auxiliary or partition columns, so the metadata prefilters (workspace /
//! conversation / since / type / branch / cwd / agent) CANNOT be expressed inside
//! the vec0 `MATCH ... AND k = ?` query. The runner therefore:
//!   - **Step A:** asks vec0 for a GENEROUS candidate pool (`k` ≫ the limit), in
//!     ascending distance order, with NO prefilters.
//!   - **Step B:** fetches provenance for those candidate rowids from `messages` /
//!     `conversation_participants` (the SAME join + the SAME prefilters `fts.rs`
//!     binds), keeping only candidates that survive the prefilters.
//!   - **Step C:** walks the Step-A distance order and emits the surviving hits,
//!     `vector_rank = position+1` among the survivors, up to the effective limit.
//!
//! ## Graceful-degradation contract (operator hard requirement)
//!
//! [`knn_search`] returns a [`VectorOutcome`] **directly** — it never returns a
//! `Result` and never panics. EVERY vector-side problem (no model row, no vec0
//! table, embedder won't build, embed error, dim mismatch, KNN error, provenance
//! error) is swallowed into `functional = false` + empty `hits`, so the caller can
//! degrade to BM25-only with an HONEST `vector_index.ready = false`. A vector
//! problem must NEVER fail the query or surface `db_unavailable` (a true DB-open
//! failure is handled in `run_query` BEFORE this runner is reached).

use sqlx::{Row, SqliteConnection};

use super::{QueryFilters, QueryHit};
use crate::zynk::embed::{active_model_id, embedder_from_env};

/// The outcome of a vector-side KNN attempt. `functional` is the honesty signal:
/// `true` ONLY when the vector index was actually usable end-to-end (model row +
/// vec0 table + embedder + KNN all OK). On any degradation `functional = false`
/// and `hits` is empty — never an error, never a panic.
pub struct VectorOutcome {
    /// Distance-ordered, prefiltered vector hits (`vector_rank = Some(i+1)`,
    /// `matched_modes = ["vector"]`). EMPTY when the vector index was unavailable.
    pub hits: Vec<QueryHit>,
    /// `true` iff the vector index was usable (model row + vec0 table + embedder).
    pub functional: bool,
    /// `embedding_jobs` for the active model not yet in `'done'` (best-effort).
    pub pending_jobs: i64,
    /// The active model id (`None` only if `active_model_id()` resolves empty).
    pub model_id: Option<String>,
}

impl VectorOutcome {
    /// A degraded outcome: no hits, not functional. Carries `pending_jobs`/`model_id`
    /// so the envelope can still report them honestly.
    fn degraded(pending_jobs: i64, model_id: Option<String>) -> Self {
        VectorOutcome {
            hits: Vec::new(),
            functional: false,
            pending_jobs,
            model_id,
        }
    }
}

/// Format a query vector as the JSON-array string vec0 binds — EXACTLY as the
/// embedding worker formats stored vectors (`embedding_worker::write_embedding_in_transaction`),
/// so the query vector lands in the same space the worker persisted.
fn vec_to_json(v: &[f32]) -> String {
    format!(
        "[{}]",
        v.iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// The provenance SELECT for a set of candidate rowids — mirrors `fts.rs`'s
/// `messages` / `conversation_participants` join and binds the SAME prefilters, but
/// keyed by `m.rowid IN (<candidates>)` instead of an FTS MATCH (no snippet/bm25).
/// `placeholders` is a comma-separated run of `?` for the candidate rowids.
fn provenance_sql(placeholders: &str) -> String {
    format!(
        "SELECT \
           m.rowid AS rowid, \
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
           (SELECT de.event_type FROM delivery_events de WHERE de.message_id = m.id ORDER BY de.seq DESC LIMIT 1) AS delivery_status \
         FROM messages m \
         JOIN conversation_participants fp ON fp.id = m.from_participant_id \
         JOIN conversation_participants tp ON tp.id = m.to_participant_id \
         WHERE m.rowid IN ({placeholders}) \
           AND (? IS NULL OR m.workspace_id = ?) \
           AND (? IS NULL OR m.conversation_id = ?) \
           AND (? IS NULL OR m.created_at >= ?) \
           AND (? IS NULL OR m.type = ?) \
           AND (? IS NULL OR m.branch = ?) \
           AND (? IS NULL OR m.cwd = ?) \
           AND (? IS NULL OR fp.agent_label = ? OR tp.agent_label = ?) \
           AND (? IS NULL OR json_extract(m.meta_json, '$.trace_id') = ?)"
    )
}

/// Vec0 KNN over the active model's vec0 table, with the candidate-pool +
/// join-prefilter fallback (see module docs). Returns a [`VectorOutcome`] directly
/// — it SWALLOWS every vector-side error into `functional = false` + empty hits, so
/// the caller can degrade to BM25-only honestly. NEVER errors out, NEVER panics.
pub async fn knn_search(
    conn: &mut SqliteConnection,
    query: &str,
    filters: &QueryFilters,
) -> VectorOutcome {
    let model_id = active_model_id();
    if model_id.is_empty() {
        return VectorOutcome::degraded(0, None);
    }

    // (1) pending_jobs: best-effort. On error, the vector index is not usable yet.
    let pending_jobs: i64 = match sqlx::query(
        "SELECT COUNT(*) AS c FROM embedding_jobs WHERE model_id = ? AND status <> 'done'",
    )
    .bind(&model_id)
    .fetch_one(&mut *conn)
    .await
    .and_then(|row| row.try_get::<i64, _>("c"))
    {
        Ok(count) => count,
        Err(_) => return VectorOutcome::degraded(0, Some(model_id)),
    };

    // (2) Resolve the model row (vec_table + dim). No row → not functional.
    let model_row = sqlx::query("SELECT vec_table, dim FROM embedding_models WHERE id = ?1")
        .bind(&model_id)
        .fetch_optional(&mut *conn)
        .await;
    let (vec_table, dim) = match model_row {
        Ok(Some(row)) => {
            let vec_table: String = match row.try_get("vec_table") {
                Ok(t) => t,
                Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
            };
            let dim: i64 = match row.try_get("dim") {
                Ok(d) => d,
                Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
            };
            (vec_table, dim)
        }
        // No model row (worker hasn't registered it) OR a query error → degrade.
        Ok(None) | Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
    };

    // (3) Confirm the vec0 table exists (the worker creates it lazily). 0 → degrade.
    let table_exists: i64 = match sqlx::query(
        "SELECT COUNT(*) AS c FROM sqlite_master WHERE type = 'table' AND name = ?1",
    )
    .bind(&vec_table)
    .fetch_one(&mut *conn)
    .await
    .and_then(|row| row.try_get::<i64, _>("c"))
    {
        Ok(count) => count,
        Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
    };
    if table_exists == 0 {
        return VectorOutcome::degraded(pending_jobs, Some(model_id));
    }

    // (4) Build the embedder + embed the QUERY (same model the worker used → same
    // space). A provider that won't build / an embed error / empty / dim mismatch
    // all degrade to BM25-only.
    let mut embedder = match embedder_from_env() {
        Ok(e) => e,
        Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
    };
    let query_vec = match embedder.embed(&[query]) {
        Ok(mut vectors) if !vectors.is_empty() => vectors.swap_remove(0),
        _ => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
    };
    if query_vec.len() as i64 != dim {
        // Dim mismatch (the stored vectors are a different model's space).
        return VectorOutcome::degraded(pending_jobs, Some(model_id));
    }

    // (5) Format the query vector exactly as the worker formats stored vectors.
    let json_vec = vec_to_json(&query_vec);

    // (6) Step A — vec0 KNN candidate pool (no prefilters; vec0 can't express them).
    let limit = filters.effective_limit();
    let candidate_k = (limit * 8).clamp(limit, 512);
    // `vec_table` is a validated identifier from `embedding_models` (NOT user input);
    // vec0 syntax needs the table name as a literal, so format it into the SQL.
    let knn_sql = format!(
        "SELECT message_rowid AS rowid, distance AS distance \
         FROM {vec_table} \
         WHERE embedding MATCH ?1 AND k = ?2 \
         ORDER BY distance"
    );
    let knn_rows = match sqlx::query(&knn_sql)
        .bind(&json_vec)
        .bind(candidate_k)
        .fetch_all(&mut *conn)
        .await
    {
        Ok(rows) => rows,
        Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
    };
    if knn_rows.is_empty() {
        // No stored vectors yet, but the index IS functional (it answered the KNN).
        return VectorOutcome {
            hits: Vec::new(),
            functional: true,
            pending_jobs,
            model_id: Some(model_id),
        };
    }

    // Candidate rowids in distance order (the order Step C walks).
    let mut candidate_rowids: Vec<i64> = Vec::with_capacity(knn_rows.len());
    let mut distance_by_rowid: std::collections::HashMap<i64, f64> =
        std::collections::HashMap::with_capacity(knn_rows.len());
    for row in &knn_rows {
        let rowid: i64 = match row.try_get("rowid") {
            Ok(r) => r,
            Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
        };
        // distance is a placeholder score C3 (`fuse_results`) overwrites with RRF.
        let distance: f64 = row.try_get("distance").unwrap_or(0.0);
        if distance_by_rowid.insert(rowid, distance).is_none() {
            candidate_rowids.push(rowid);
        }
    }

    // (6) Step B — provenance + prefilters for the candidate rowids (mirror fts.rs).
    let placeholders = std::iter::repeat_n("?", candidate_rowids.len())
        .collect::<Vec<_>>()
        .join(",");
    let prov_sql = provenance_sql(&placeholders);
    let mut prov_query = sqlx::query(&prov_sql);
    for rowid in &candidate_rowids {
        prov_query = prov_query.bind(rowid);
    }
    // Bind the SAME prefilters, in the SAME order/shape as `fts.rs`.
    prov_query = prov_query
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
        .bind(&filters.trace_id);
    let prov_rows = match prov_query.fetch_all(&mut *conn).await {
        Ok(rows) => rows,
        Err(_) => return VectorOutcome::degraded(pending_jobs, Some(model_id)),
    };

    // Map surviving rowid -> provenance QueryHit (snippet/bm25_rank None; the score
    // is the vec0 distance, a placeholder `fuse_results` overwrites with RRF).
    let mut hit_by_rowid: std::collections::HashMap<i64, QueryHit> =
        std::collections::HashMap::with_capacity(prov_rows.len());
    for row in &prov_rows {
        let rowid: i64 = match row.try_get("rowid") {
            Ok(r) => r,
            Err(_) => continue,
        };
        let hit = match build_hit(row, distance_by_rowid.get(&rowid).copied().unwrap_or(0.0)) {
            Some(h) => h,
            None => continue,
        };
        hit_by_rowid.insert(rowid, hit);
    }

    // (6) Step C — walk the KNN distance order; emit ALL surviving candidates (already
    // bounded by `candidate_k`, the generous pool) with a contiguous 1-based
    // `vector_rank` among the survivors. We do NOT pre-truncate to `limit` here: a
    // vector doc ranked past `limit` can still legitimately enter the fused top-`limit`
    // after BM25 reshuffles, so RRF must see the full surviving leg. The single
    // post-fusion `truncate(limit)` in `fuse_results` cuts the final list.
    let mut hits = Vec::with_capacity(hit_by_rowid.len());
    let mut rank: i64 = 0;
    for rowid in &candidate_rowids {
        if let Some(mut hit) = hit_by_rowid.remove(rowid) {
            rank += 1;
            hit.vector_rank = Some(rank);
            hits.push(hit);
        }
    }

    VectorOutcome {
        hits,
        functional: true,
        pending_jobs,
        model_id: Some(model_id),
    }
}

/// Map a provenance row to a vector `QueryHit`. `score` is the placeholder vec0
/// distance (the hybrid `fuse_results` overwrites it with the RRF score). Returns
/// `None` if a required column is missing (the candidate is dropped, not panicked).
fn build_hit(row: &sqlx::sqlite::SqliteRow, score: f64) -> Option<QueryHit> {
    Some(QueryHit {
        message_id: row.try_get("id").ok()?,
        conversation_id: row.try_get("conversation_id").ok()?,
        conversation_seq: row.try_get("conversation_seq").ok()?,
        message_type: row.try_get::<Option<String>, _>("type").ok()?,
        from: row.try_get("from_agent").ok()?,
        to: row.try_get("to_agent").ok()?,
        workspace_id: row.try_get("workspace_id").ok()?,
        tab_id: row.try_get("tab_id").ok()?,
        branch: row.try_get::<Option<String>, _>("branch").ok()?,
        cwd: row.try_get::<Option<String>, _>("cwd").ok()?,
        created_at: row.try_get("created_at").ok()?,
        trace_id: row.try_get::<Option<String>, _>("trace_id").ok()?,
        score,
        matched_modes: vec!["vector".to_string()],
        bm25_rank: None,
        vector_rank: None, // set by Step C among the survivors
        snippet: None,
        delivery_status: row.try_get::<Option<String>, _>("delivery_status").ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zynk::db::{block_on, open_migrated_at_without_recovery, DbError};
    use crate::zynk::embed::embedder_from_env;
    use crate::zynk::embed::vec::register_sqlite_vec;
    use crate::zynk::embedding_worker::ensure_model_and_vec0;
    use crate::zynk::message::{new_prefixed_id, Party, SendCommand};
    use crate::zynk::persistence::{begin_send_attempt_async, SendAttempt};
    use sqlx::{Executor, SqliteConnection};

    // The vector tests use the DEFAULT (dim-384) FakeEmbedder for BOTH the stored
    // vectors AND `knn_search`'s query embedder (which is `embedder_from_env()` =
    // dim 384). That keeps the spaces consistent so the dim check passes — the path
    // (a) the prompt calls out. We craft stored vectors via the SAME provider so a
    // query for a stored message's body embeds to the exact stored vector → distance 0.

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "zynk-vector-test-{}-{}.db",
            std::process::id(),
            new_prefixed_id("test")
        ))
    }

    /// Seed a message via the real send path (also enqueues a B3 pending job).
    async fn seed_message(
        conn: &mut SqliteConnection,
        message_id: &str,
        agent_to: &str,
        msg_type: Option<&str>,
        body: &str,
    ) -> i64 {
        let from = Party {
            agent: Some("alice".into()),
            workspace: Some("workspace".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        let to = Party {
            agent: Some(agent_to.into()),
            workspace: Some("workspace".into()),
            tab: Some("tab".into()),
            ..Party::default()
        };
        begin_send_attempt_async(
            conn,
            SendAttempt {
                command: SendCommand::PaneRun,
                message_id,
                target_arg: agent_to,
                from: &from,
                to: &to,
                message_type: msg_type,
                body,
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

    /// Manually insert a vec0 row + message_embeddings row with a CONTROLLED vector
    /// (the FakeEmbedder vector for `body`), so a query for `body` lands at distance 0.
    async fn insert_controlled_vector(
        conn: &mut SqliteConnection,
        vec_table: &str,
        rowid: i64,
        message_id: &str,
        model_id: &str,
        body: &str,
    ) {
        // Use the SAME provider knn_search uses (embedder_from_env, dim 384) so the
        // stored vector is in the exact space the query vector will be in.
        let mut embedder = embedder_from_env().expect("default fake embedder");
        let vector = embedder.embed(&[body]).expect("embed body").swap_remove(0);
        let json = super::vec_to_json(&vector);
        let insert_vec =
            format!("INSERT OR REPLACE INTO {vec_table} (message_rowid, embedding) VALUES (?, ?)");
        sqlx::query(&insert_vec)
            .bind(rowid)
            .bind(&json)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query(
            "INSERT OR REPLACE INTO message_embeddings (message_id, model_id, vec_rowid, text_hash, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(message_id)
        .bind(model_id)
        .bind(rowid)
        .bind("hash")
        .bind("2026-06-14T00:00:00Z")
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    #[test]
    fn knn_returns_matching_message_with_rank_one() {
        register_sqlite_vec();
        block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;

            let body = "the quick brown fox jumps";
            let rowid = seed_message(&mut conn, "msg_vec_a", "bob", Some("review"), body).await;
            // Also seed a DISTINCT-body message so the pool has > 1 candidate.
            let rowid2 = seed_message(
                &mut conn,
                "msg_vec_b",
                "bob",
                Some("review"),
                "totally other text",
            )
            .await;

            // Default (dim-384) fake embedder → model row + vec0 table.
            let embedder = embedder_from_env().unwrap();
            let (model_id, vec_table, _dim) =
                ensure_model_and_vec0(&mut conn, embedder.as_ref()).await?;

            insert_controlled_vector(&mut conn, &vec_table, rowid, "msg_vec_a", &model_id, body)
                .await;
            insert_controlled_vector(
                &mut conn,
                &vec_table,
                rowid2,
                "msg_vec_b",
                &model_id,
                "totally other text",
            )
            .await;

            // Query whose FakeEmbedder vector == the stored vector for msg_vec_a.
            let filters = QueryFilters::default();
            let outcome = knn_search(&mut conn, body, &filters).await;
            assert!(outcome.functional, "vector index must be functional");
            assert!(
                !outcome.hits.is_empty(),
                "at least the exact-match message must be returned"
            );
            assert_eq!(
                outcome.hits[0].message_id, "msg_vec_a",
                "the exact-match (distance 0) message must rank first"
            );
            assert_eq!(outcome.hits[0].vector_rank, Some(1));
            assert_eq!(outcome.hits[0].bm25_rank, None, "vector-only hit");
            assert_eq!(outcome.hits[0].matched_modes, vec!["vector".to_string()]);
            assert_eq!(outcome.model_id.as_deref(), Some("fake@1"));

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn prefilters_restrict_vector_hits() {
        register_sqlite_vec();
        block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;

            let body = "shared body token text";
            // Two messages with the SAME body (→ same stored vector) but different
            // type, so a --type prefilter restricts which survives.
            let rowid_r =
                seed_message(&mut conn, "msg_pf_review", "bob", Some("review"), body).await;
            let rowid_q =
                seed_message(&mut conn, "msg_pf_question", "bob", Some("question"), body).await;

            let embedder = embedder_from_env().unwrap();
            let (model_id, vec_table, _dim) =
                ensure_model_and_vec0(&mut conn, embedder.as_ref()).await?;
            insert_controlled_vector(
                &mut conn,
                &vec_table,
                rowid_r,
                "msg_pf_review",
                &model_id,
                body,
            )
            .await;
            insert_controlled_vector(
                &mut conn,
                &vec_table,
                rowid_q,
                "msg_pf_question",
                &model_id,
                body,
            )
            .await;

            let filters = QueryFilters {
                message_type: Some("review".into()),
                ..QueryFilters::default()
            };
            let outcome = knn_search(&mut conn, body, &filters).await;
            assert!(outcome.functional);
            assert_eq!(
                outcome.hits.len(),
                1,
                "the --type review prefilter must keep only the review message"
            );
            assert_eq!(outcome.hits[0].message_id, "msg_pf_review");

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn no_model_row_degrades_not_panics() {
        register_sqlite_vec();
        block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_nomodel", "bob", Some("review"), "some body").await;

            // NO ensure_model_and_vec0 → no embedding_models row, no vec0 table.
            let outcome = knn_search(&mut conn, "some body", &QueryFilters::default()).await;
            assert!(!outcome.functional, "no model row → not functional");
            assert!(outcome.hits.is_empty(), "degraded → empty hits");
            assert_eq!(
                outcome.model_id.as_deref(),
                Some("fake@1"),
                "model_id still reported honestly"
            );

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn model_row_but_no_vec0_table_degrades() {
        register_sqlite_vec();
        block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            seed_message(&mut conn, "msg_novec0", "bob", Some("review"), "some body").await;

            // Insert a model row whose vec_table does NOT exist as a table.
            sqlx::query(
                "INSERT INTO embedding_models (id, provider, model_name, dim, normalize, vec_table, created_at) \
                 VALUES ('fake@1', 'fake', 'fake@1', 384, 1, 'message_vec_fake_1', '2026-06-14T00:00:00Z')",
            )
            .execute(&mut conn)
            .await?;
            // (deliberately do NOT create message_vec_fake_1)

            let outcome = knn_search(&mut conn, "some body", &QueryFilters::default()).await;
            assert!(!outcome.functional, "missing vec0 table → not functional");
            assert!(outcome.hits.is_empty());

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }

    #[test]
    fn pending_jobs_reflects_non_done_jobs() {
        register_sqlite_vec();
        block_on(async {
            std::env::remove_var(crate::zynk::embed::ZYNK_EMBED_PROVIDER_ENV);
            let path = temp_db_path();
            let mut conn = open_migrated_at_without_recovery(&path).await?;
            // The send path enqueues a 'pending' embedding job for fake@1.
            seed_message(
                &mut conn,
                "msg_pending",
                "bob",
                Some("review"),
                "pending body",
            )
            .await;

            // No model row yet → degrades, but pending_jobs must still report the job.
            let outcome = knn_search(&mut conn, "pending body", &QueryFilters::default()).await;
            assert_eq!(
                outcome.pending_jobs, 1,
                "the seeded pending job must be counted"
            );

            // Mark it done; pending_jobs drops to 0.
            conn.execute("UPDATE embedding_jobs SET status='done' WHERE message_id='msg_pending'")
                .await?;
            let outcome2 = knn_search(&mut conn, "pending body", &QueryFilters::default()).await;
            assert_eq!(outcome2.pending_jobs, 0, "done jobs are not pending");

            let _ = std::fs::remove_file(&path);
            Ok::<(), DbError>(())
        })
        .unwrap();
    }
}
