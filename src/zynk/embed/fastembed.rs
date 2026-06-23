//! zynk fork: real `fastembed`-backed embedder (M5b B5, ADR 0006).
//
// This WHOLE module compiles only behind the opt-in `fastembed` feature
// (`#![cfg(feature = "fastembed")]`). It is absent from the DEFAULT build graph,
// so `just test`/`just check` never pull `fastembed`/`ort`/ONNX Runtime and stay
// 100% network-free and hermetic (ADR 0006 §D5/§D6).
//
// OFFLINE BY CONSTRUCTION. `RealEmbedder::new()` loads the model from a
// PRE-PROVISIONED local directory (`ZYNK_EMBED_MODEL_DIR`) via
// `TextEmbedding::try_new_from_user_defined`, which takes the ONNX bytes +
// tokenizer files we read off disk — there is NO implicit HuggingFace Hub fetch at
// construction or at `embed()` time. The `ort`/ONNX-Runtime build-time download is a
// separate, explicit provisioning concern (warm `~/.cache/ort.pyke.io` or a pinned
// local ORT lib path per ADR 0006), not handled here. If the model dir is unset or
// incomplete, `new()` returns `Err(ModelUnavailable)` — it NEVER panics and NEVER
// downloads.
#![cfg(feature = "fastembed")]

use std::path::PathBuf;

use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};

use super::{EmbedError, Embedder};

/// The stable model id persisted alongside vectors and returned by
/// [`super::active_model_id`] for the `fastembed` provider. Mirrors the default
/// model `intfloat/multilingual-e5-small` (dim 384).
pub const REAL_EMBED_MODEL_ID: &str = "multilingual-e5-small@1";

/// The fixed embedding dimension of the default model (`multilingual-e5-small`).
pub const REAL_EMBED_DIM: usize = 384;

/// The env var naming the PRE-PROVISIONED local model directory. The directory must
/// contain `model.onnx` (or `onnx/model.onnx`), `tokenizer.json`, `config.json`,
/// `special_tokens_map.json`, and `tokenizer_config.json` — the standard fastembed
/// user-defined layout. NOTHING is downloaded; the provisioning step stages these.
pub const ZYNK_EMBED_MODEL_DIR_ENV: &str = "ZYNK_EMBED_MODEL_DIR";

/// A real, fastembed-backed embedder loaded OFFLINE from a provisioned local dir.
pub struct RealEmbedder {
    model: TextEmbedding,
}

impl RealEmbedder {
    /// Build the embedder by loading the model OFFLINE from `ZYNK_EMBED_MODEL_DIR`.
    ///
    /// Returns `Err(EmbedError::ModelUnavailable)` if the env var is unset/empty or
    /// any required file is missing — never panics, never reaches the network.
    pub fn new() -> Result<Self, EmbedError> {
        let dir = std::env::var(ZYNK_EMBED_MODEL_DIR_ENV)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                EmbedError::ModelUnavailable(format!(
                    "{ZYNK_EMBED_MODEL_DIR_ENV} is unset; provision a local model dir \
                     (see ADR 0006) — no implicit download is performed"
                ))
            })?;
        let dir = PathBuf::from(dir);

        let onnx = read_required(&dir, &["model.onnx", "onnx/model.onnx"])?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read_one(&dir, "tokenizer.json")?,
            config_file: read_one(&dir, "config.json")?,
            special_tokens_map_file: read_one(&dir, "special_tokens_map.json")?,
            tokenizer_config_file: read_one(&dir, "tokenizer_config.json")?,
        };

        // `multilingual-e5-small` is a mean-pooled e5 model; declare the pooling so the
        // sentence embedding matches the upstream definition exactly.
        let user_model =
            UserDefinedEmbeddingModel::new(onnx, tokenizer_files).with_pooling(Pooling::Mean);

        let model =
            TextEmbedding::try_new_from_user_defined(user_model, InitOptionsUserDefined::default())
                .map_err(|e| {
                    EmbedError::ModelUnavailable(format!(
                        "fastembed failed to load model from {}: {e}",
                        dir.display()
                    ))
                })?;

        Ok(Self { model })
    }
}

impl Embedder for RealEmbedder {
    fn dim(&self) -> usize {
        REAL_EMBED_DIM
    }

    fn model_id(&self) -> &str {
        REAL_EMBED_MODEL_ID
    }

    fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // fastembed wants owned strings; the batch size `None` uses its default.
        let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        self.model
            .embed(owned, None)
            .map_err(|e| EmbedError::Provider(format!("fastembed embed failed: {e}")))
    }
}

/// Read the first file from `dir` that exists among `candidates`, as bytes.
fn read_required(dir: &std::path::Path, candidates: &[&str]) -> Result<Vec<u8>, EmbedError> {
    for cand in candidates {
        let p = dir.join(cand);
        if p.is_file() {
            return std::fs::read(&p).map_err(|e| {
                EmbedError::ModelUnavailable(format!("reading {}: {e}", p.display()))
            });
        }
    }
    Err(EmbedError::ModelUnavailable(format!(
        "none of {candidates:?} found under {}",
        dir.display()
    )))
}

/// Read exactly `name` from `dir` as bytes, mapping a missing file to `ModelUnavailable`.
fn read_one(dir: &std::path::Path, name: &str) -> Result<Vec<u8>, EmbedError> {
    let p = dir.join(name);
    std::fs::read(&p)
        .map_err(|e| EmbedError::ModelUnavailable(format!("reading {}: {e}", p.display())))
}

// NOTE: `EmbeddingModel` is imported so a future provider-selection switch (bge-m3,
// dim 1024) can name built-in model variants; it is intentionally referenced here to
// keep the import meaningful under `-D warnings` without an `allow`.
#[allow(dead_code)]
const _ASSERT_DEFAULT_MODEL: EmbeddingModel = EmbeddingModel::MultilingualE5Small;
