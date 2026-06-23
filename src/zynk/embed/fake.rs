//! zynk fork: deterministic, network-free fake embedder (M5b embedding plan §4.2).
//
// `FakeEmbedder` produces stable unit-length vectors with ZERO network, ZERO model
// files, and the std lib only (no rand/math crates — matching the repo's roll-our-own
// discipline, cf. `message.rs::new_prefixed_id`). It is the default provider in tests
// (B3/B4/B5 lean on its determinism) and carries a deliberate failure-injection seam
// for B4's retry test.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::{EmbedError, Embedder};

/// Default embedding dimension — mirrors the real default model
/// `intfloat/multilingual-e5-small` (384) so the downstream `vec0` table dim
/// stays model-driven and consistent whichever provider is wired.
pub const FAKE_EMBED_DIM: usize = 384;

/// A deterministic, std-only embedder. For each text it derives each component by
/// hashing the `(component_index, text)` pair, then L2-normalizes the vector to unit
/// length. Same text → byte-identical vector, with no I/O — FOR A GIVEN TOOLCHAIN.
///
/// CAVEAT (load-bearing): the derivation uses `std::collections::hash_map::Default-
/// Hasher`, whose algorithm std does NOT guarantee stable across Rust releases. A
/// toolchain change that altered the hash would change every vector — so persisted
/// `vec0` vectors would silently mismatch newly-computed query vectors. This is the
/// fake provider (tests / offline default), not a model you migrate data under; the
/// `golden_vector_pins_hash_output` regression test below pins the current output so
/// such drift FAILS loudly instead of silently corrupting "embeddings".
pub struct FakeEmbedder {
    dim: usize,
    /// Number of remaining `embed` calls that must fail before succeeding. The
    /// failure-injection seam (B4 retry test): each `embed` call while this is > 0
    /// returns `Err` and decrements it; at 0, `embed` succeeds deterministically.
    fail_remaining: usize,
}

impl FakeEmbedder {
    /// A fake embedder at the default dim (384).
    pub fn new() -> Self {
        Self::with_dim(FAKE_EMBED_DIM)
    }

    /// A fake embedder at an arbitrary dim — handy for fast tests (e.g. dim 8).
    pub fn with_dim(dim: usize) -> Self {
        Self {
            dim,
            fail_remaining: 0,
        }
    }

    /// A fake embedder whose FIRST `fail_times` `embed` calls return
    /// `Err(EmbedError::Provider(..))`, and every call after that succeeds. The
    /// legitimate test seam B4's retry path drives; not for production use.
    pub fn failing_then_ok(fail_times: usize) -> Self {
        Self {
            dim: FAKE_EMBED_DIM,
            fail_remaining: fail_times,
        }
    }

    /// Build one deterministic unit vector for `text`.
    ///
    /// Component `i` = a stable `f32` in `[-1.0, 1.0)` derived by hashing `(i, text)`,
    /// then the whole vector is L2-normalized to unit length. If the raw norm is 0
    /// (only when `dim == 0`, or the astronomically unlikely all-zero hash), the
    /// vector is returned un-normalized rather than dividing by zero.
    fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut vec: Vec<f32> = Vec::with_capacity(self.dim);
        for i in 0..self.dim {
            let mut hasher = DefaultHasher::new();
            i.hash(&mut hasher);
            text.hash(&mut hasher);
            let h = hasher.finish();
            // Map the u64 hash into a centered, stable f32 range [-1.0, 1.0).
            let centered = (h as f64 / u64::MAX as f64) * 2.0 - 1.0;
            vec.push(centered as f32);
        }
        let norm = vec.iter().map(|c| c * c).sum::<f32>().sqrt();
        if norm > 0.0 {
            for c in vec.iter_mut() {
                *c /= norm;
            }
        }
        vec
    }
}

impl Default for FakeEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for FakeEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &str {
        "fake@1"
    }

    fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if self.fail_remaining > 0 {
            self.fail_remaining -= 1;
            return Err(EmbedError::Provider("FakeEmbedder injected failure".into()));
        }
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L2 norm of a vector.
    fn l2(v: &[f32]) -> f32 {
        v.iter().map(|c| c * c).sum::<f32>().sqrt()
    }

    #[test]
    fn dim_and_model_id_are_stable() {
        let e = FakeEmbedder::new();
        assert_eq!(e.dim(), 384);
        assert_eq!(e.model_id(), "fake@1");
        let small = FakeEmbedder::with_dim(8);
        assert_eq!(small.dim(), 8);
        assert_eq!(small.model_id(), "fake@1");
    }

    #[test]
    fn embed_returns_one_vector_per_text_each_of_dim_length() {
        let mut e = FakeEmbedder::with_dim(8);
        let out = e.embed(&["a", "b", "c"]).expect("embed ok");
        assert_eq!(out.len(), 3, "one vector per input text");
        for v in &out {
            assert_eq!(v.len(), 8, "every vector has length dim()");
        }
    }

    #[test]
    fn empty_input_slice_yields_empty_output_no_panic() {
        let mut e = FakeEmbedder::with_dim(8);
        let out = e.embed(&[]).expect("empty input is Ok(vec![])");
        assert!(out.is_empty());
    }

    #[test]
    fn deterministic_within_one_instance_across_two_calls() {
        let mut e = FakeEmbedder::with_dim(16);
        let a = e.embed(&["the quick brown fox"]).expect("call 1");
        let b = e.embed(&["the quick brown fox"]).expect("call 2");
        // Byte-identical vectors across two calls on the same instance.
        assert_eq!(a, b);
    }

    #[test]
    fn deterministic_across_two_distinct_instances() {
        let mut e1 = FakeEmbedder::with_dim(384);
        let mut e2 = FakeEmbedder::with_dim(384);
        let a = e1.embed(&["hello world"]).expect("e1");
        let b = e2.embed(&["hello world"]).expect("e2");
        assert_eq!(a, b, "two instances must agree byte-for-byte");
    }

    #[test]
    fn golden_vector_pins_hash_output() {
        // Pins the EXACT current FakeEmbedder output for a fixed (dim, text) so a
        // future toolchain change that alters `DefaultHasher` fails LOUDLY here
        // (rather than silently changing every persisted/queried "embedding").
        // Captured from the current toolchain; update deliberately if std's hasher
        // ever changes (and treat that as a vec0 re-embed migration trigger).
        let mut e = FakeEmbedder::with_dim(4);
        let out = e.embed(&["golden"]).expect("embed ok");
        assert_eq!(out.len(), 1);
        let expected: [f32; 4] = [0.18544184, 0.50244355, -0.7467244, 0.3944165];
        assert_eq!(
            out[0], expected,
            "FakeEmbedder hash output drifted — DefaultHasher likely changed across \
             the toolchain; this invalidates persisted vec0 vectors"
        );
    }

    #[test]
    fn vectors_are_unit_l2_norm() {
        let mut e = FakeEmbedder::with_dim(384);
        let out = e
            .embed(&["alpha", "a longer piece of text with words", "z"])
            .expect("embed ok");
        for v in &out {
            let n = l2(v);
            assert!(
                (n - 1.0).abs() < 1e-5,
                "expected unit L2 norm, got {n} for a {}-dim vector",
                v.len()
            );
        }
    }

    #[test]
    fn distinct_texts_yield_distinct_vectors() {
        let mut e = FakeEmbedder::with_dim(64);
        let out = e.embed(&["cat", "dog", "bird"]).expect("embed ok");
        assert_ne!(out[0], out[1], "different texts must differ");
        assert_ne!(out[1], out[2]);
        assert_ne!(out[0], out[2]);
    }

    #[test]
    fn within_a_vector_components_are_not_all_equal() {
        // Guards against a degenerate hash that ignores the component index.
        let mut e = FakeEmbedder::with_dim(32);
        let out = e.embed(&["some representative text"]).expect("embed ok");
        let v = &out[0];
        let first = v[0];
        assert!(
            v.iter().any(|&c| (c - first).abs() > 1e-9),
            "components must vary across the vector, not all == {first}"
        );
    }

    #[test]
    fn failing_then_ok_one_fails_first_call_then_succeeds() {
        let mut e = FakeEmbedder::failing_then_ok(1);
        let first = e.embed(&["x"]);
        assert!(
            matches!(first, Err(EmbedError::Provider(_))),
            "first call must fail with Provider, got {first:?}"
        );
        let second = e.embed(&["x"]).expect("second call must succeed");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].len(), FAKE_EMBED_DIM);
    }

    #[test]
    fn failing_then_ok_zero_never_fails() {
        let mut e = FakeEmbedder::failing_then_ok(0);
        e.embed(&["x"])
            .expect("with 0 failures the first call succeeds");
    }

    #[test]
    fn failing_then_ok_two_fails_twice_then_succeeds() {
        let mut e = FakeEmbedder::failing_then_ok(2);
        assert!(matches!(e.embed(&["x"]), Err(EmbedError::Provider(_))));
        assert!(matches!(e.embed(&["x"]), Err(EmbedError::Provider(_))));
        e.embed(&["x"]).expect("third call succeeds");
    }

    #[test]
    fn dim_zero_does_not_panic_and_yields_empty_vectors() {
        // Edge case: a 0-dim embedder produces empty (length-0) vectors, no div-by-zero.
        let mut e = FakeEmbedder::with_dim(0);
        let out = e.embed(&["x", "y"]).expect("embed ok");
        assert_eq!(out.len(), 2);
        assert!(out[0].is_empty() && out[1].is_empty());
    }
}
