//! zynk fork (M5a): `zynk query` lexical/BM25 retrieval. In-process, read-only.
//!
//! M5a is the **F3 lexical/BM25 path ONLY** — FTS5 BM25 over the existing
//! `messages_fts`, metadata prefilters before ranking, F4-enveloped response. NO
//! embeddings / sqlite-vec / RRF (those are M5b/M5c). The read path opens the DB
//! via `db::open_query_readonly` (= `open_migrated_for_append` + `PRAGMA
//! query_only=1`): the connection is write-incapable for queries, so a query NEVER
//! synthesizes a recovery/delivery event. (The opener runs `MIGRATOR` first, so it
//! MAY write `_sqlx_migrations`/schema on a first/upgrade open — but never a
//! delivery or recovery event.) Reads `messages.body`/FTS only — no header pollution.
//!
//! Plan: `docs/zynk/plans/2026-06-14-m5-query-retrieval.md` §3/§7/§8 (M5a).

pub mod fts;
pub mod rrf;
pub mod vector;

/// Filters that prefilter the candidate set BEFORE ranking (plan §8).
#[derive(Debug, Clone)]
pub struct QueryFilters {
    pub workspace: Option<String>,
    pub conversation: Option<String>,
    pub agent: Option<String>,
    pub since: Option<String>, // RFC3339; the CLI validates before calling
    pub message_type: Option<String>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    /// Feature #107 (IM2): prefilter to a single trace id
    /// (`json_extract(meta_json, '$.trace_id') = ?`). Validated at the CLI via
    /// `validate_trace_id`. Old rows (no trace_id) are excluded by the equality.
    pub trace_id: Option<String>,
    pub limit: usize,
    pub exact: bool, // --exact => quote the query as an FTS5 phrase
}

impl Default for QueryFilters {
    fn default() -> Self {
        QueryFilters {
            workspace: None,
            conversation: None,
            agent: None,
            since: None,
            message_type: None,
            branch: None,
            cwd: None,
            trace_id: None,
            limit: 20,
            exact: false,
        }
    }
}

const MAX_LIMIT: usize = 200;

impl QueryFilters {
    fn effective_limit(&self) -> i64 {
        self.limit.clamp(1, MAX_LIMIT) as i64
    }

    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "workspace": self.workspace,
            "conversation": self.conversation,
            "agent": self.agent,
            "since": self.since,
            "type": self.message_type,
            "branch": self.branch,
            "cwd": self.cwd,
            "trace": self.trace_id,
            "limit": self.limit.clamp(1, MAX_LIMIT),
            "exact": self.exact,
        })
    }
}

/// One ranked result row (provenance per plan §7).
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryHit {
    pub message_id: String,
    pub conversation_id: String,
    pub conversation_seq: i64,
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
    /// Feature #107 (IM2): the per-message trace id (`meta_json.$.trace_id`), surfaced
    /// when present and omitted when the message carries no trace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub score: f64,
    pub matched_modes: Vec<String>,
    /// 1-based rank in the BM25/lexical list (`None` for a vector-only hit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_rank: Option<i64>,
    /// 1-based rank in the vector/ANN list (`None` for a BM25-only hit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<String>,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryStatus {
    Ok,
    Failed,
}

/// The F4-enveloped `zynk query` response (matches the `SendOutcome` contract:
/// leads with `result` + `command`; failures carry `code`/`message`/`context`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryResponse {
    pub result: QueryStatus,
    pub command: &'static str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub response_type: Option<&'static str>,
    // success payload (omitted on failure):
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking: Option<&'static str>,
    /// Hybrid-retrieval vector-index health, present on success (omitted on failure):
    /// `{ "ready": bool, "pending_jobs": i64, "model_id": <string|null> }`. `ready`
    /// is honest — `false` when the vector path degraded or jobs are still landing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_index: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<QueryHit>>,
    // failure payload (omitted on success):
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    pub next: String,
}

const COMMAND: &str = "zynk query";
const RESPONSE_TYPE: &str = "zynk_query_result";

impl QueryResponse {
    /// Build a successful F4-enveloped response. `ranking` ("rrf" | "bm25") + the
    /// `vector_index` JSON are HONEST: the caller (`run_query`/`fuse_results`) sets
    /// `ranking="rrf"` ONLY when the vector index was actually used, and reports
    /// `vector_index.ready=false` whenever the vector path degraded or jobs are
    /// still landing — never claiming hybrid it didn't run.
    fn ok(
        query: &str,
        filters: &QueryFilters,
        hits: Vec<QueryHit>,
        ranking: &'static str,
        vector_index: serde_json::Value,
    ) -> Self {
        let count = hits.len();
        // Guidance is gated on the ACTUAL `ranking` FIRST (ARB-M5C-001), never on
        // `vector_index.ready` alone: a partial-freshness RRF response (`ranking="rrf"`
        // with `ready=false` because some embedding jobs are still pending) IS hybrid —
        // it must NOT be described as "BM25-only". `ranking="rrf"` always implies
        // `count>=1` (fusion includes the non-empty vector list), so a zero-result query
        // is necessarily `ranking="bm25"`.
        let vector_ready = vector_index
            .get("ready")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let next = if ranking == "rrf" {
            if vector_ready {
                "hybrid (BM25 ∥ vector → RRF) results".to_string()
            } else {
                // RRF over the LANDED vectors, but some embeddings are still pending —
                // honest: it IS hybrid now, with fuller vector coverage to come.
                "hybrid (BM25 ∥ vector → RRF) results using the landed vectors; some \
                 embeddings are still pending — re-run as they complete for fuller vector \
                 coverage"
                    .to_string()
            }
        } else if count == 0 {
            "no matches — broaden the query tokens or relax --filters".to_string()
        } else if !vector_ready {
            // BM25-only because the vector index is not yet landed / unavailable.
            "vectors are still landing (or unavailable) — results are BM25-only for now; \
             re-run shortly for full hybrid (RRF) ranking"
                .to_string()
        } else {
            // Functional vector index that is ready, but no vector hits to fuse → BM25-only.
            "BM25-only results (vector index ready but no vector hits to fuse)".to_string()
        };
        QueryResponse {
            result: QueryStatus::Ok,
            command: COMMAND,
            response_type: Some(RESPONSE_TYPE),
            query: Some(query.to_string()),
            filters: Some(filters.as_json()),
            ranking: Some(ranking),
            vector_index: Some(vector_index),
            count: Some(count),
            results: Some(hits),
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
        QueryResponse {
            result: QueryStatus::Failed,
            command: COMMAND,
            response_type: None,
            query: None,
            filters: None,
            ranking: None,
            vector_index: None,
            count: None,
            results: None,
            code: Some(code.to_string()),
            message: Some(message.into()),
            context: Some(context),
            next: next.to_string(),
        }
    }

    /// CLI-side filter-validation failure (`invalid_filter`) — a bad `--since`,
    /// `--limit`, etc. is rejected before any DB access.
    pub fn invalid_filter(message: impl Into<String>, context: serde_json::Value) -> Self {
        Self::failed(
            "invalid_filter",
            message,
            context,
            "correct the filter value (--since must be RFC3339; --limit a non-negative integer)",
        )
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.result, QueryStatus::Failed)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"result\":\"failed\"}".to_string())
    }

    /// Concise human output (one block per hit).
    pub fn to_human(&self) -> String {
        if self.is_failed() {
            return format!(
                "query failed [{}]: {}",
                self.code.as_deref().unwrap_or("error"),
                self.message.as_deref().unwrap_or(""),
            );
        }
        let hits = self.results.as_deref().unwrap_or(&[]);
        if hits.is_empty() {
            return format!(
                "0 results for {:?}\n{}",
                self.query.as_deref().unwrap_or(""),
                self.next
            );
        }
        let mut out = String::new();
        for (i, h) in hits.iter().enumerate() {
            let status = h.delivery_status.as_deref().unwrap_or("?");
            let kind = h.message_type.as_deref().unwrap_or("-");
            // Render `bm25#N` / `vec#N` only for the modes this hit actually matched.
            let mut ranks = Vec::with_capacity(2);
            if let Some(r) = h.bm25_rank {
                ranks.push(format!("bm25#{r}"));
            }
            if let Some(r) = h.vector_rank {
                ranks.push(format!("vec#{r}"));
            }
            out.push_str(&format!(
                "#{}  {}  {}→{}  {}  [{}]  {}\n",
                i + 1,
                h.message_id,
                h.from,
                h.to,
                kind,
                status,
                ranks.join(" "),
            ));
            if let Some(s) = &h.snippet {
                out.push_str(&format!("    {s}\n"));
            }
            out.push_str(&format!(
                "    {}#{} · {} · {}\n",
                h.conversation_id,
                h.conversation_seq,
                h.branch.as_deref().unwrap_or("-"),
                h.created_at,
            ));
        }
        out.push_str(&format!("{} result(s)", hits.len()));
        out
    }
}

/// Compose BM25 hits + the vector outcome into the final ranked result list, the
/// `ranking` discriminator, and the `vector_index` JSON. PURE (no DB / no I/O), so
/// it is the unit-testable heart of the hybrid pipeline.
///
/// - **Hybrid (RRF):** when the vector index was `functional` AND returned ≥1 hit,
///   `ranking = "rrf"` and the two id lists are fused via [`rrf::rrf_fuse`]. The
///   per-id provenance prefers the BM25 entry (it carries the snippet); each output
///   hit reports `matched_modes` per membership (bm25 first if both), the 1-based
///   `bm25_rank`/`vector_rank` for the lists it appears in, and the fused score.
/// - **Degraded (BM25-only):** when the vector index was NOT functional (any
///   degradation), or functional-but-empty, `ranking = "bm25"` and the BM25 hits
///   pass through unchanged (truncated to the effective limit).
/// - `vector_index` is ALWAYS reported honestly:
///   `{ ready: functional && pending_jobs == 0, pending_jobs, model_id }`.
fn fuse_results(
    bm25_hits: Vec<QueryHit>,
    vec: vector::VectorOutcome,
    filters: &QueryFilters,
) -> (Vec<QueryHit>, &'static str, serde_json::Value) {
    let limit = filters.effective_limit() as usize;
    let vector_index = serde_json::json!({
        "ready": vec.functional && vec.pending_jobs == 0,
        "pending_jobs": vec.pending_jobs,
        "model_id": vec.model_id,
    });

    // Degrade to BM25-only: vector not functional, OR functional-but-empty.
    if !vec.functional || vec.hits.is_empty() {
        let mut results = bm25_hits;
        results.truncate(limit);
        return (results, "bm25", vector_index);
    }

    // Hybrid: fuse the two ranked id lists with RRF.
    let bm25_ids: Vec<String> = bm25_hits.iter().map(|h| h.message_id.clone()).collect();
    let vec_ids: Vec<String> = vec.hits.iter().map(|h| h.message_id.clone()).collect();

    // Position lookups for the 1-based per-list ranks.
    let bm25_pos: std::collections::HashMap<&str, i64> = bm25_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), (i as i64) + 1))
        .collect();
    let vec_pos: std::collections::HashMap<&str, i64> = vec_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), (i as i64) + 1))
        .collect();

    // Provenance map id -> QueryHit, preferring the BM25 entry (it has the snippet).
    let mut provenance: std::collections::HashMap<String, QueryHit> =
        std::collections::HashMap::new();
    for hit in vec.hits {
        provenance.entry(hit.message_id.clone()).or_insert(hit);
    }
    for hit in bm25_hits {
        provenance.insert(hit.message_id.clone(), hit); // BM25 wins (snippet)
    }

    let fused = rrf::rrf_fuse(
        &[(1.0, bm25_ids.clone()), (1.0, vec_ids.clone())],
        rrf::RRF_K,
    );

    let mut results = Vec::with_capacity(limit.min(fused.len()));
    for (id, fused_score) in fused.into_iter().take(limit) {
        let Some(mut hit) = provenance.remove(&id) else {
            continue;
        };
        let in_bm25 = bm25_pos.get(id.as_str()).copied();
        let in_vec = vec_pos.get(id.as_str()).copied();
        // matched_modes: bm25 first when both.
        let mut modes = Vec::with_capacity(2);
        if in_bm25.is_some() {
            modes.push("bm25".to_string());
        }
        if in_vec.is_some() {
            modes.push("vector".to_string());
        }
        hit.matched_modes = modes;
        hit.bm25_rank = in_bm25;
        hit.vector_rank = in_vec;
        hit.score = fused_score;
        results.push(hit);
    }

    (results, "rrf", vector_index)
}

/// Run the hybrid `zynk query` and produce the F4 response. Pipeline (plan §6):
/// validate → register vec0 → open read-only → BM25 search → vector KNN (degrading)
/// → fuse. The read path is write-incapable (`query_only=1`) and never synthesizes a
/// recovery/delivery event. An FTS5 MATCH parse error (`fts_query_error`) is the
/// caller's malformed query (`invalid_query`); a true open/IO failure is
/// `db_unavailable`. The VECTOR side NEVER fails the query: any vector-side problem
/// degrades to BM25-only with an honest `vector_index.ready=false` (see
/// [`vector::knn_search`]) — never `vector_unavailable`, never a panic.
pub fn run_query(text: &str, filters: QueryFilters) -> QueryResponse {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return QueryResponse::failed(
            "invalid_query",
            "query text is empty",
            serde_json::json!({ "query": text }),
            "supply a non-empty query; quote a phrase for exact match",
        );
    }

    // Register vec0 process-globally BEFORE opening the read conn, so the conn sees
    // the `vec0` module (idempotent; the embedding worker also registers it).
    crate::zynk::embed::vec::register_sqlite_vec();

    let result = crate::zynk::db::block_on(async {
        let mut conn = crate::zynk::db::open_query_readonly().await?;
        // BM25 first: a malformed FTS query (`fts_query_error`) or a true DB error
        // surfaces here, BEFORE the vector path (which can only degrade, not error).
        let bm25_hits = fts::bm25_search(&mut conn, trimmed, &filters).await?;
        // The vector side swallows every error into a non-functional outcome.
        let vec = vector::knn_search(&mut conn, trimmed, &filters).await;
        Ok::<_, crate::zynk::db::DbError>((bm25_hits, vec))
    });

    match result {
        Ok((bm25_hits, vec)) => {
            let (hits, ranking, vector_index) = fuse_results(bm25_hits, vec, &filters);
            QueryResponse::ok(trimmed, &filters, hits, ranking, vector_index)
        }
        Err(err) if err.code == "fts_query_error" => QueryResponse::failed(
            "invalid_query",
            "malformed FTS5 query — balance quotes and check FTS5 operators",
            serde_json::json!({ "query": trimmed, "detail": err.message }),
            "fix the FTS5 syntax (balance quotes); or drop operators for a plain term search",
        ),
        Err(err) => QueryResponse::failed(
            "db_unavailable",
            "the zynk database could not be opened or queried",
            serde_json::json!({ "code": err.code, "detail": err.message }),
            "check ZYNK_SQLITE_HOME / the native zynk-v2 DB path",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_filters_default_limit_is_20() {
        let f = QueryFilters::default();
        assert_eq!(f.limit, 20);
        assert!(f.workspace.is_none() && f.agent.is_none() && !f.exact);
    }

    #[test]
    fn limit_is_clamped() {
        let high = QueryFilters {
            limit: 100_000,
            ..QueryFilters::default()
        };
        assert_eq!(high.effective_limit(), MAX_LIMIT as i64);
        let low = QueryFilters {
            limit: 0,
            ..QueryFilters::default()
        };
        assert_eq!(low.effective_limit(), 1);
    }

    fn hit() -> QueryHit {
        QueryHit {
            message_id: "msg_a".into(),
            conversation_id: "conv_x".into(),
            conversation_seq: 7,
            message_type: Some("review".into()),
            from: "claude".into(),
            to: "codex".into(),
            workspace_id: "w65".into(),
            tab_id: "w65:2".into(),
            branch: Some("zynk-fork".into()),
            cwd: Some("/repo".into()),
            created_at: "2026-06-14T00:00:00Z".into(),
            trace_id: Some("trace-x".into()),
            score: -1.2,
            matched_modes: vec!["bm25".into()],
            bm25_rank: Some(1),
            vector_rank: None,
            snippet: Some("the [footer]".into()),
            delivery_status: Some("received".into()),
        }
    }

    /// A BM25-only outcome equivalent: not functional, so `fuse_results` degrades.
    fn bm25_only_index() -> serde_json::Value {
        serde_json::json!({ "ready": false, "pending_jobs": 0, "model_id": "fake@1" })
    }

    #[test]
    fn success_response_is_f4_enveloped() {
        let resp = QueryResponse::ok(
            "footer",
            &QueryFilters::default(),
            vec![hit()],
            "bm25",
            bm25_only_index(),
        );
        let v: serde_json::Value = serde_json::from_str(&resp.to_json()).unwrap();
        assert_eq!(v["result"], "ok");
        assert_eq!(v["command"], "zynk query");
        assert_eq!(v["type"], "zynk_query_result");
        assert_eq!(v["ranking"], "bm25");
        assert_eq!(v["count"], 1);
        assert_eq!(v["results"][0]["message_id"], "msg_a");
        assert_eq!(v["results"][0]["from"], "claude");
        assert_eq!(v["results"][0]["bm25_rank"], 1);
        // vector_rank is None on a BM25-only hit → omitted.
        assert!(v["results"][0].get("vector_rank").is_none());
        // vector_index is always present on success, honest about readiness.
        assert_eq!(v["vector_index"]["ready"], false);
        assert_eq!(v["vector_index"]["pending_jobs"], 0);
        assert_eq!(v["vector_index"]["model_id"], "fake@1");
        assert!(v["next"].is_string());
        // failure-only fields absent on success:
        assert!(v.get("code").is_none());
    }

    /// FIX 2: a functional-but-empty vector index (`ready=true`, `ranking="bm25"`)
    /// must NOT claim a hybrid fuse in `next` — it never ran.
    #[test]
    fn next_does_not_claim_hybrid_for_functional_empty_index() {
        let functional_empty = serde_json::json!({
            "ready": true, "pending_jobs": 0, "model_id": "fake@1"
        });
        let resp = QueryResponse::ok(
            "footer",
            &QueryFilters::default(),
            vec![hit()],
            "bm25", // functional index, but no vector hits to fuse
            functional_empty,
        );
        assert!(
            !resp.next.contains("hybrid"),
            "ready-but-empty index must not claim a hybrid fuse: {:?}",
            resp.next
        );
        assert!(
            resp.next.contains("BM25-only"),
            "ready-but-empty index reports honest BM25-only guidance: {:?}",
            resp.next
        );

        // Sanity: the genuine RRF case DOES claim hybrid.
        let ready = serde_json::json!({ "ready": true, "pending_jobs": 0, "model_id": "fake@1" });
        let rrf = QueryResponse::ok(
            "footer",
            &QueryFilters::default(),
            vec![hit()],
            "rrf",
            ready,
        );
        assert!(
            rrf.next.contains("hybrid"),
            "the genuine rrf case claims hybrid: {:?}",
            rrf.next
        );
    }

    /// ARB-M5C-001: a partial-freshness RRF response (`ranking="rrf"` with
    /// `vector_index.ready=false` — some embedding jobs still pending) IS hybrid; `next`
    /// must say so (landed vectors + pending), NOT "BM25-only". This was the reachable
    /// state (`fuse_results_pending_jobs_makes_ready_false`) the old `!vector_ready`-first
    /// branch mislabeled.
    #[test]
    fn next_for_partial_freshness_rrf_says_hybrid_not_bm25() {
        let partial = serde_json::json!({
            "ready": false, "pending_jobs": 2, "model_id": "fake@1"
        });
        let resp = QueryResponse::ok(
            "footer",
            &QueryFilters::default(),
            vec![hit()],
            "rrf",
            partial,
        );
        assert!(
            !resp.next.contains("BM25-only"),
            "a partial-freshness RRF response must NOT claim BM25-only: {:?}",
            resp.next
        );
        assert!(
            resp.next.contains("hybrid") || resp.next.contains("RRF"),
            "a partial-freshness RRF response reports hybrid/RRF guidance: {:?}",
            resp.next
        );
        assert!(
            resp.next.contains("pending"),
            "it must mention the still-pending embeddings: {:?}",
            resp.next
        );
    }

    #[test]
    fn failure_response_is_f4_enveloped() {
        let resp = QueryResponse::failed(
            "invalid_query",
            "query text is empty",
            serde_json::json!({ "query": "" }),
            "supply a non-empty query",
        );
        assert!(resp.is_failed());
        let v: serde_json::Value = serde_json::from_str(&resp.to_json()).unwrap();
        assert_eq!(v["result"], "failed");
        assert_eq!(v["command"], "zynk query");
        assert_eq!(v["code"], "invalid_query");
        assert_eq!(v["message"], "query text is empty");
        assert_eq!(v["context"]["query"], "");
        assert!(v["next"].is_string());
        // success-only fields absent on failure:
        assert!(v.get("results").is_none());
        assert!(v.get("ranking").is_none());
    }

    #[test]
    fn empty_query_is_invalid_query_without_touching_db() {
        // run_query short-circuits on empty input before opening the DB.
        let resp = run_query("   ", QueryFilters::default());
        assert!(resp.is_failed());
        assert_eq!(resp.code.as_deref(), Some("invalid_query"));
    }

    #[test]
    fn human_output_non_empty_for_hits_and_empty_set() {
        let ok = QueryResponse::ok(
            "q",
            &QueryFilters::default(),
            vec![hit()],
            "bm25",
            bm25_only_index(),
        );
        assert!(ok.to_human().contains("msg_a"));
        assert!(ok.to_human().contains("claude→codex"));
        assert!(ok.to_human().contains("bm25#1"), "renders bm25 rank");
        let none = QueryResponse::ok(
            "q",
            &QueryFilters::default(),
            vec![],
            "bm25",
            bm25_only_index(),
        );
        assert!(none.to_human().contains("0 results"));
    }

    /// A minimal QueryHit with a given id + mode, for the pure `fuse_results` tests.
    fn hit_with(id: &str, mode: &str) -> QueryHit {
        QueryHit {
            message_id: id.into(),
            conversation_id: "conv".into(),
            conversation_seq: 1,
            message_type: Some("review".into()),
            from: "alice".into(),
            to: "bob".into(),
            workspace_id: "w".into(),
            tab_id: "w:1".into(),
            branch: None,
            cwd: None,
            created_at: "2026-06-14T00:00:00Z".into(),
            trace_id: None,
            score: 0.0,
            matched_modes: vec![mode.into()],
            bm25_rank: if mode == "bm25" { Some(1) } else { None },
            vector_rank: if mode == "vector" { Some(1) } else { None },
            snippet: if mode == "bm25" {
                Some("snip".into())
            } else {
                None
            },
            delivery_status: Some("received".into()),
        }
    }

    // Hybrid path: functional vector outcome with overlap → ranking="rrf", the
    // both-mode doc ranks first, and matched_modes/bm25_rank/vector_rank are set.
    #[test]
    fn fuse_results_hybrid_rrf_ranks_both_mode_doc_first() {
        // bm25 order: [a, b]; vector order: [b, c]. b is in BOTH → it must rank first.
        let bm25 = vec![hit_with("a", "bm25"), hit_with("b", "bm25")];
        let vec = vector::VectorOutcome {
            hits: vec![hit_with("b", "vector"), hit_with("c", "vector")],
            functional: true,
            pending_jobs: 0,
            model_id: Some("fake@1".into()),
        };
        let (results, ranking, vector_index) = fuse_results(bm25, vec, &QueryFilters::default());

        assert_eq!(ranking, "rrf", "functional vector with hits → hybrid RRF");
        assert_eq!(
            vector_index["ready"], true,
            "functional + 0 pending → ready"
        );

        // b first (both-mode).
        assert_eq!(results[0].message_id, "b");
        assert_eq!(
            results[0].matched_modes,
            vec!["bm25".to_string(), "vector".to_string()],
            "both-mode doc lists bm25 first then vector"
        );
        assert_eq!(results[0].bm25_rank, Some(2), "b is 2nd in the bm25 list");
        assert_eq!(
            results[0].vector_rank,
            Some(1),
            "b is 1st in the vector list"
        );
        // b's provenance is the BM25 entry (it has the snippet).
        assert_eq!(results[0].snippet.as_deref(), Some("snip"));
        // b's fused score is the RRF score: 1/(60+2) + 1/(60+1).
        assert!((results[0].score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);

        // a is bm25-only: bm25_rank set, vector_rank None, modes=["bm25"].
        let a = results.iter().find(|h| h.message_id == "a").unwrap();
        assert_eq!(a.bm25_rank, Some(1));
        assert_eq!(a.vector_rank, None);
        assert_eq!(a.matched_modes, vec!["bm25".to_string()]);
        // c is vector-only: vector_rank set, bm25_rank None, modes=["vector"].
        let c = results.iter().find(|h| h.message_id == "c").unwrap();
        assert_eq!(c.bm25_rank, None);
        assert_eq!(c.vector_rank, Some(2));
        assert_eq!(c.matched_modes, vec!["vector".to_string()]);

        // All three ids present, deduped.
        assert_eq!(results.len(), 3);
    }

    // Degraded path: a non-functional vector outcome → ranking="bm25",
    // vector_index.ready=false, BM25 results pass through unchanged.
    #[test]
    fn fuse_results_degrades_to_bm25_when_vector_not_functional() {
        let bm25 = vec![hit_with("a", "bm25"), hit_with("b", "bm25")];
        let vec = vector::VectorOutcome {
            hits: vec![],
            functional: false,
            pending_jobs: 3,
            model_id: Some("fake@1".into()),
        };
        let (results, ranking, vector_index) = fuse_results(bm25, vec, &QueryFilters::default());

        assert_eq!(ranking, "bm25", "non-functional vector → BM25-only");
        assert_eq!(vector_index["ready"], false, "honest: not ready");
        assert_eq!(vector_index["pending_jobs"], 3);
        assert_eq!(vector_index["model_id"], "fake@1");

        // BM25 results unchanged (same order, same per-hit ranks).
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message_id, "a");
        assert_eq!(results[0].bm25_rank, Some(1));
        assert_eq!(results[0].vector_rank, None);
        assert_eq!(results[0].matched_modes, vec!["bm25".to_string()]);
        assert_eq!(results[1].message_id, "b");
    }

    // Functional-but-empty vector outcome also degrades to BM25, but ready can still
    // be true (the index answered, just no stored vectors matched / pending = 0).
    #[test]
    fn fuse_results_functional_empty_vector_is_bm25_ranking() {
        let bm25 = vec![hit_with("a", "bm25")];
        let vec = vector::VectorOutcome {
            hits: vec![],
            functional: true,
            pending_jobs: 0,
            model_id: Some("fake@1".into()),
        };
        let (results, ranking, vector_index) = fuse_results(bm25, vec, &QueryFilters::default());
        assert_eq!(
            ranking, "bm25",
            "functional-but-empty vector → BM25 ranking (no vector hits to fuse)"
        );
        // ready is true: the index IS functional and nothing is pending.
        assert_eq!(vector_index["ready"], true);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message_id, "a");
    }

    // Honesty: a functional index with pending jobs reports ready=false.
    #[test]
    fn fuse_results_pending_jobs_makes_ready_false() {
        let bm25 = vec![hit_with("a", "bm25")];
        let vec = vector::VectorOutcome {
            hits: vec![hit_with("a", "vector")],
            functional: true,
            pending_jobs: 5,
            model_id: Some("fake@1".into()),
        };
        let (_results, ranking, vector_index) = fuse_results(bm25, vec, &QueryFilters::default());
        // It still fuses (ranking="rrf" — vectors that DID land are usable)...
        assert_eq!(ranking, "rrf");
        // ...but readiness is honest: jobs are still landing.
        assert_eq!(
            vector_index["ready"], false,
            "pending jobs → not fully ready"
        );
        assert_eq!(vector_index["pending_jobs"], 5);
    }
}
