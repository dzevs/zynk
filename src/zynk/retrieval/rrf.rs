//! zynk fork (M5c): pure Reciprocal Rank Fusion (RRF).
//
// RRF (Cormack, Clarke & Buettcher, SIGIR'09) fuses several independently ranked
// lists (here: lexical/BM25 and vector/ANN) into ONE ranked list by summing, per
// doc, a `weight / (k + rank)` contribution for each list the doc appears in. It is
// the deterministic core the M5c hybrid `zynk query` pipeline composes — no DB, no
// I/O, no async. C3 (the hybrid pipeline) is the only caller; until it lands, the
// `RRF_K` constant + `rrf_fuse` fn are unused in the bin build, so the module-level
// allow keeps the strict `-D warnings` gate green (cf. `src/zynk/embed/mod.rs`, which
// likewise lands its seam ahead of its callers).
#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// RRF default constant (the paper's optimum + the Elasticsearch/OpenSearch de-facto
/// default; the optimum is flat).
pub const RRF_K: f64 = 60.0;

/// Fuse several ranked lists of message ids into one ranked list by Reciprocal Rank Fusion.
/// `lists` is a slice of `(weight, ranked_ids)` — each `ranked_ids` is in best-first order.
/// RRFscore(d) = Σ over each list i in which d appears:  weight_i / (k + rank_i(d)),
/// where rank_i(d) is d's 1-BASED position in list i. A doc absent from a list contributes 0
/// for that list. Returns `(message_id, fused_score)` sorted by score DESCENDING, ties broken
/// by message_id ASCENDING (so the output is fully deterministic). Each id appears once (deduped).
pub fn rrf_fuse(lists: &[(f64, Vec<String>)], k: f64) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();

    for (weight, ranked_ids) in lists {
        // Guard against a duplicate WITHIN one list: count only its first (best) rank,
        // so a later worse rank in the same list does not double-count.
        let mut seen: HashSet<&str> = HashSet::new();
        for (i, id) in ranked_ids.iter().enumerate() {
            if !seen.insert(id.as_str()) {
                continue;
            }
            let rank = (i + 1) as f64; // 1-based
            let contribution = weight / (k + rank);
            *scores.entry(id.clone()).or_insert(0.0) += contribution;
        }
    }

    let mut fused: Vec<(String, f64)> = scores.into_iter().collect();
    // Primary: score DESC. Secondary (ties): id ASC. Total order that can't panic.
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-12;

    fn ids(fused: &[(String, f64)]) -> Vec<&str> {
        fused.iter().map(|(id, _)| id.as_str()).collect()
    }

    fn list(weight: f64, ids: &[&str]) -> (f64, Vec<String>) {
        (weight, ids.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn known_fusion_order_and_score() {
        // list1=[a,b,c], list2=[b,c,d], both weight 1.0, k=60.
        // a=1/61, b=1/62+1/61, c=1/63+1/62, d=1/63 → order [b, c, a, d].
        let lists = [list(1.0, &["a", "b", "c"]), list(1.0, &["b", "c", "d"])];
        let fused = rrf_fuse(&lists, 60.0);
        assert_eq!(ids(&fused), vec!["b", "c", "a", "d"]);

        // b's exact score == 1/62 + 1/61.
        let b_score = fused.iter().find(|(id, _)| id == "b").unwrap().1;
        assert!((b_score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < EPS);

        // a single-mode score == 1/61.
        let a_score = fused.iter().find(|(id, _)| id == "a").unwrap().1;
        assert!((a_score - (1.0 / 61.0)).abs() < EPS);
        // d single-mode score == 1/63.
        let d_score = fused.iter().find(|(id, _)| id == "d").unwrap().1;
        assert!((d_score - (1.0 / 63.0)).abs() < EPS);
    }

    #[test]
    fn both_list_doc_outranks_single_mode() {
        // b, c appear in BOTH lists; a, d in only one. Both-list docs rank above.
        let lists = [list(1.0, &["a", "b", "c"]), list(1.0, &["b", "c", "d"])];
        let fused = rrf_fuse(&lists, 60.0);
        let order = ids(&fused);
        let pos = |id: &str| order.iter().position(|x| *x == id).unwrap();
        assert!(pos("b") < pos("a") && pos("b") < pos("d"));
        assert!(pos("c") < pos("a") && pos("c") < pos("d"));
    }

    #[test]
    fn deterministic_tie_break_by_id_asc() {
        // x and y each score 1/61. Output must be [x, y] regardless of input order.
        let fused1 = rrf_fuse(&[list(1.0, &["x"]), list(1.0, &["y"])], 60.0);
        assert_eq!(ids(&fused1), vec!["x", "y"]);

        // Reverse the input order — still x before y (id ASC tiebreak).
        let fused2 = rrf_fuse(&[list(1.0, &["y"]), list(1.0, &["x"])], 60.0);
        assert_eq!(ids(&fused2), vec!["x", "y"]);

        // Equal scores within f64 epsilon.
        assert!((fused1[0].1 - fused1[1].1).abs() < EPS);
    }

    #[test]
    fn weight_raises_a_lists_contribution() {
        // Two disjoint single-element lists. Equal weight → tie broken by id ASC ([a, z]).
        let equal = rrf_fuse(&[list(1.0, &["z"]), list(1.0, &["a"])], 60.0);
        assert_eq!(ids(&equal), vec!["a", "z"]);

        // Give z's list weight 2.0 → z's contribution (2/61) beats a's (1/61) → z first.
        let weighted = rrf_fuse(&[list(2.0, &["z"]), list(1.0, &["a"])], 60.0);
        assert_eq!(ids(&weighted), vec!["z", "a"]);
        let z_score = weighted.iter().find(|(id, _)| id == "z").unwrap().1;
        assert!((z_score - (2.0 / 61.0)).abs() < EPS);
    }

    #[test]
    fn empty_inputs_return_empty() {
        assert!(rrf_fuse(&[], 60.0).is_empty());
        assert!(rrf_fuse(&[list(1.0, &[])], 60.0).is_empty());
        // Multiple all-empty lists.
        assert!(rrf_fuse(&[list(1.0, &[]), list(2.0, &[])], 60.0).is_empty());
    }

    #[test]
    fn dedup_within_a_list_counts_first_rank_only() {
        // list1=[a, a, b]: a's rank is 1 (not also counted at 3); b's rank is 3.
        let fused = rrf_fuse(&[list(1.0, &["a", "a", "b"])], 60.0);
        // Each id appears once.
        assert_eq!(fused.len(), 2);
        let a_score = fused.iter().find(|(id, _)| id == "a").unwrap().1;
        let b_score = fused.iter().find(|(id, _)| id == "b").unwrap().1;
        // a counted only at rank 1 → 1/61 (NOT 1/61 + 1/63).
        assert!((a_score - (1.0 / 61.0)).abs() < EPS);
        // b at rank 3 → 1/63.
        assert!((b_score - (1.0 / 63.0)).abs() < EPS);
        // a outranks b.
        assert_eq!(ids(&fused), vec!["a", "b"]);
    }

    #[test]
    fn rrf_k_default_is_sixty() {
        assert_eq!(RRF_K, 60.0);
    }
}
