//! zynk fork: embedding provider seam (M5b embedding plan §4.2).
//
// The `Embedder` trait is the single seam the embedding worker (B4) and retrieval
// ranking (B5) build on. B1 ships only the deterministic, std-only `FakeEmbedder`
// (`fake.rs`) and the `ZYNK_EMBED_PROVIDER` selector; the real `fastembed` arm
// lands in B5. `EmbedError` is internal — it is NOT serialized to the F4 envelope.
//
// B1 lands the seam ahead of its callers: nothing in the bin build constructs an
// `Embedder` yet (B3 enqueues jobs, B4's worker calls `embed`, B5 ranks on it). The
// public surface is exercised by this module's tests; the module-level allow keeps the
// strict `-D warnings` gate green until those callers land (cf. `message.rs::human`).
#![allow(dead_code)]

pub mod fake;
pub mod vec;

// zynk M5b B5 (ADR 0006): the real fastembed-backed embedder compiles ONLY behind the
// opt-in `fastembed` feature — the whole module (and its heavy ort/fastembed deps) is
// absent from the DEFAULT build graph, keeping `just test`/`just check` hermetic.
#[cfg(feature = "fastembed")]
pub mod fastembed;

pub use fake::FakeEmbedder;

/// Produces fixed-dimension embedding vectors for input texts.
///
/// `embed` returns one `Vec<f32>` per input text, in input order, each exactly
/// `dim()` long. `&mut self` so a provider may hold a stateful model handle (the
/// real B5 embedder) or a deterministic test counter (`FakeEmbedder`).
pub trait Embedder: Send {
    /// The fixed embedding dimension every returned vector has.
    fn dim(&self) -> usize;
    /// A stable identifier for the model/provider (persisted alongside vectors so
    /// the `vec0` table dim stays model-driven and consistent across runs).
    fn model_id(&self) -> &str;
    /// Embed `texts`, returning one `dim()`-length vector per input, in order.
    fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// Internal embedding error (NOT serialized to the F4 envelope).
#[derive(Debug, Clone)]
pub enum EmbedError {
    /// The provider failed to produce an embedding (transient/runtime failure).
    Provider(String),
    /// The model is not loaded/provisioned (used by the real embedder in B5, and
    /// by the selector for the not-yet-built `fastembed` arm / unknown providers).
    ModelUnavailable(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::Provider(msg) => write!(f, "embed_provider: {msg}"),
            EmbedError::ModelUnavailable(msg) => write!(f, "model_unavailable: {msg}"),
        }
    }
}

impl std::error::Error for EmbedError {}

/// The env var selecting the embedding provider. `"fake"` (the default) is a
/// deterministic, network-free provider; `"fastembed"` is the real model arm (B5).
pub const ZYNK_EMBED_PROVIDER_ENV: &str = "ZYNK_EMBED_PROVIDER";

/// Build an [`Embedder`] from `ZYNK_EMBED_PROVIDER` (default `"fake"` when unset/empty).
///
/// - `"fake"` → a deterministic [`FakeEmbedder`] (default dim 384).
/// - `"fastembed"` → `Err(ModelUnavailable)` until B5 wires the real arm.
/// - anything else → `Err(ModelUnavailable)` naming the unknown value.
///
/// This is the ONLY place embed-provider env is read, so the default-fake-in-tests
/// discipline stays explicit and grep-able.
pub fn embedder_from_env() -> Result<Box<dyn Embedder>, EmbedError> {
    let provider = std::env::var(ZYNK_EMBED_PROVIDER_ENV).unwrap_or_default();
    let provider = provider.trim();
    match provider {
        "" | "fake" => Ok(Box::new(FakeEmbedder::new())),
        "fastembed" => {
            #[cfg(feature = "fastembed")]
            {
                fastembed::RealEmbedder::new().map(|e| Box::new(e) as Box<dyn Embedder>)
            }
            #[cfg(not(feature = "fastembed"))]
            {
                Err(EmbedError::ModelUnavailable(
                    "fastembed feature not compiled; rebuild with --features fastembed and \
                     provision ORT + the model (see ADR 0006)"
                        .into(),
                ))
            }
        }
        other => Err(EmbedError::ModelUnavailable(format!(
            "unknown ZYNK_EMBED_PROVIDER: {other}"
        ))),
    }
}

/// The model id of the currently-configured active embedder — the SINGLE source of
/// truth shared by the send-path enqueue (B3) and the embedding worker (B4), so an
/// `embedding_jobs.model_id` matches the model the worker actually embeds with.
/// Pure config lookup (NEVER constructs/loads an Embedder — the send path must stay cheap).
/// Reads `ZYNK_EMBED_PROVIDER` (default `fake`).
pub fn active_model_id() -> String {
    match std::env::var(ZYNK_EMBED_PROVIDER_ENV)
        .unwrap_or_default()
        .trim()
    {
        "fastembed" => "multilingual-e5-small@1".to_string(),
        _ => "fake@1".to_string(), // "" | "fake" | anything-else → the fake default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    // `embedder_from_env` reads a process-global env var; serialize the env-mutating
    // tests so they don't race each other (cargo/nextest may run tests in threads).
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn embed_error_display_distinguishes_variants() {
        let p = EmbedError::Provider("boom".into());
        let m = EmbedError::ModelUnavailable("not loaded".into());
        assert_eq!(p.to_string(), "embed_provider: boom");
        assert_eq!(m.to_string(), "model_unavailable: not loaded");
        // It is a std::error::Error (usable as Box<dyn Error>).
        let _boxed: Box<dyn std::error::Error> = Box::new(p);
    }

    #[test]
    fn default_unset_provider_is_a_working_fake_dim_384() {
        let _guard = env_lock();
        std::env::remove_var(ZYNK_EMBED_PROVIDER_ENV);
        let mut e = embedder_from_env().expect("default provider must be a working fake");
        assert_eq!(e.dim(), 384, "default fake mirrors the real e5-small dim");
        assert_eq!(e.model_id(), "fake@1");
        let out = e.embed(&["hello"]).expect("fake embed must succeed");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 384);
    }

    #[test]
    fn empty_provider_string_is_treated_as_fake() {
        let _guard = env_lock();
        std::env::set_var(ZYNK_EMBED_PROVIDER_ENV, "");
        let e = embedder_from_env().expect("empty provider must default to fake");
        assert_eq!(e.dim(), 384);
        std::env::remove_var(ZYNK_EMBED_PROVIDER_ENV);
    }

    #[test]
    fn explicit_fake_provider_selects_fake() {
        let _guard = env_lock();
        std::env::set_var(ZYNK_EMBED_PROVIDER_ENV, "fake");
        let e = embedder_from_env().expect("'fake' must select the fake embedder");
        assert_eq!(e.model_id(), "fake@1");
        std::env::remove_var(ZYNK_EMBED_PROVIDER_ENV);
    }

    // In the DEFAULT build (no `fastembed` feature) the `"fastembed"` arm reports
    // `ModelUnavailable` — the feature isn't compiled in. With `--features fastembed`
    // the arm builds a `RealEmbedder` whose `new()` ALSO returns `ModelUnavailable`
    // when the model dir is unprovisioned, so this assertion holds either way in a
    // hermetic (unprovisioned) test environment.
    #[test]
    fn fastembed_provider_is_model_unavailable_without_feature_or_provisioning() {
        let _guard = env_lock();
        std::env::set_var(ZYNK_EMBED_PROVIDER_ENV, "fastembed");
        // Ensure no stray provisioning env leaks a real model into a hermetic run.
        std::env::remove_var("ZYNK_EMBED_MODEL_DIR");
        // `Box<dyn Embedder>` is not Debug, so match rather than `expect_err`.
        let result = embedder_from_env();
        std::env::remove_var(ZYNK_EMBED_PROVIDER_ENV);
        match result {
            Err(EmbedError::ModelUnavailable(_)) => {}
            Err(other) => panic!("expected ModelUnavailable, got {other:?}"),
            Ok(_) => panic!("fastembed must not yield an embedder without provisioning"),
        }
    }

    #[test]
    fn active_model_id_tracks_provider_selection() {
        let _guard = env_lock();
        // default (unset) → the fake default, matching `FakeEmbedder::model_id()`.
        std::env::remove_var(ZYNK_EMBED_PROVIDER_ENV);
        assert_eq!(active_model_id(), "fake@1");
        // explicit "fake" → the fake default.
        std::env::set_var(ZYNK_EMBED_PROVIDER_ENV, "fake");
        assert_eq!(active_model_id(), "fake@1");
        // "fastembed" → the real model id the worker embeds with in B5.
        std::env::set_var(ZYNK_EMBED_PROVIDER_ENV, "fastembed");
        assert_eq!(active_model_id(), "multilingual-e5-small@1");
        // anything unknown → falls back to the fake default (no panic, send stays cheap).
        std::env::set_var(ZYNK_EMBED_PROVIDER_ENV, "totally-bogus");
        assert_eq!(active_model_id(), "fake@1");
        std::env::remove_var(ZYNK_EMBED_PROVIDER_ENV);
    }

    #[test]
    fn unknown_provider_is_an_error_naming_the_value() {
        let _guard = env_lock();
        std::env::set_var(ZYNK_EMBED_PROVIDER_ENV, "totally-bogus");
        let result = embedder_from_env();
        std::env::remove_var(ZYNK_EMBED_PROVIDER_ENV);
        match result {
            Err(EmbedError::ModelUnavailable(msg)) => {
                assert!(
                    msg.contains("totally-bogus"),
                    "error must name the unknown value: {msg}"
                );
            }
            Err(other) => panic!("expected ModelUnavailable, got {other:?}"),
            Ok(_) => panic!("unknown provider must not yield an embedder"),
        }
    }
}
