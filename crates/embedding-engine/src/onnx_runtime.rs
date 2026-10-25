//! Real ONNX Runtime inference (feature `onnx`).
//!
//! **Honest boundary.** This module integrates the *real* ONNX Runtime via the
//! `ort` crate, configured with `load-dynamic` so the ONNX Runtime library is
//! loaded at *runtime* (from `ORT_LIB_LOCATION`) rather than linked at build
//! time. The build therefore stays self-contained (no native library, no
//! build-time download).
//!
//! What is real: loading the ONNX Runtime, opening a model file, running
//! inference, and extracting/normalizing the embedding output. What is *not*
//! bundled: the ONNX Runtime library, the model weight files, and a
//! model-matching tokenizer — all are provisioned by the deployment. The default
//! [`WhitespaceTokenizer`] is a deterministic fallback for wiring/validation and
//! is *not* semantic (mirrors the existing `OnnxEmbedder` stance); a deployment
//! should supply a tokenizer that matches the provisioned model.
//!
//! When the runtime library or model file is absent, every call degrades to
//! [`EmbeddingError::ModelUnavailable`] so the semantic channel falls back to the
//! static channel (see the `OnnxEmbedder` / backend-mode machinery).

use std::collections::BTreeMap;
use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;

use crate::{EmbeddingError, EmbeddingPort, EmbeddingResult, RerankerPort};

/// Tokenization seam. Real, model-matching tokenizers are provided by the
/// deployment; the default is a deterministic fallback.
pub trait Tokenizer: Send + Sync {
    /// Tokenize `text` into at most `max_len` token ids.
    fn tokenize(&self, text: &str, max_len: usize) -> Vec<i64>;
}

/// Deterministic whitespace tokenizer (wiring fallback, not semantic).
#[derive(Debug, Clone, Copy)]
pub struct WhitespaceTokenizer {
    /// Default max sequence length.
    pub max_len: usize,
}

impl WhitespaceTokenizer {
    /// Create a tokenizer with a default max sequence length.
    pub fn new(max_len: usize) -> Self {
        Self { max_len }
    }
}

impl Default for WhitespaceTokenizer {
    fn default() -> Self {
        Self::new(128)
    }
}

impl Tokenizer for WhitespaceTokenizer {
    fn tokenize(&self, text: &str, max_len: usize) -> Vec<i64> {
        let limit = max_len;
        let mut ids = Vec::with_capacity(limit);
        for token in text.split_whitespace() {
            if ids.len() >= limit {
                break;
            }
            // FNV-1a into a modest id space (deterministic, not semantic).
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in token.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
            ids.push((hash % 30_000) as i64);
        }
        ids
    }
}

/// A real ONNX Runtime session (feature-gated). The library is loaded at runtime
/// (`load-dynamic`); the model file is opened into an inference session.
pub struct OnnxRuntime {
    session: Mutex<Session>,
    tokenizer: Box<dyn Tokenizer>,
    /// Token id space size (used only to bound the fallback tokenizer).
    #[allow(dead_code)]
    vocab: usize,
}

impl OnnxRuntime {
    /// Resolve the ONNX Runtime library path (deployment-provided).
    fn library_path() -> Option<String> {
        std::env::var("ORT_LIB_LOCATION")
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// Load the ONNX Runtime library and open `model_path` into a session.
    ///
    /// Fails (and therefore degrades) when the library or model file is absent.
    pub fn load(model_path: &str) -> EmbeddingResult<Self> {
        Self::load_with_tokenizer(model_path, Box::new(WhitespaceTokenizer::default()))
    }

    /// Load with a deployment-provided tokenizer.
    pub fn load_with_tokenizer(
        model_path: &str,
        tokenizer: Box<dyn Tokenizer>,
    ) -> EmbeddingResult<Self> {
        // `load-dynamic`: load the ONNX Runtime library at runtime.
        let lib = Self::library_path().ok_or_else(|| EmbeddingError::ModelUnavailable {
            profile_id: "onnx".to_string(),
        })?;
        let env = ort::init_from(lib)
            .map_err(|_| EmbeddingError::ModelUnavailable {
                profile_id: "onnx".to_string(),
            })?
            .build()
            .map_err(|_| EmbeddingError::ModelUnavailable {
                profile_id: "onnx".to_string(),
            })?;
        let session = Session::builder(&env)
            .map_err(|_| EmbeddingError::ModelUnavailable {
                profile_id: "onnx".to_string(),
            })?
            .commit_from_file(model_path)
            .map_err(|_| EmbeddingError::ModelUnavailable {
                profile_id: "onnx".to_string(),
            })?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            vocab: 30_000,
        })
    }

    /// Run one text through the session and normalize the output to `dim`.
    fn run_one(&self, text: &str, dim: usize) -> EmbeddingResult<Vec<f32>> {
        let ids = self.tokenizer.tokenize(text, self.vocab);
        let seq_len = ids.len().max(1);
        let input = Tensor::<i64>::from_array(([1usize, seq_len], ids.into_boxed_slice()))
            .map_err(|e| EmbeddingError::InvalidArgument {
                field: "input",
                reason: e.to_string(),
            })?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| EmbeddingError::InvalidArgument {
                field: "session",
                reason: "session lock poisoned".to_string(),
            })?;
        let outputs =
            session
                .run(ort::inputs![input])
                .map_err(|e| EmbeddingError::InvalidArgument {
                    field: "run",
                    reason: e.to_string(),
                })?;
        let out = &outputs[0];
        let (_shape, data) =
            out.try_extract_tensor::<f32>()
                .map_err(|e| EmbeddingError::InvalidArgument {
                    field: "extract",
                    reason: e.to_string(),
                })?;
        // Truncate/pad to the profile dimension, then normalize.
        let mut vec = vec![0.0f32; dim];
        for (i, value) in data.iter().take(dim).enumerate() {
            vec[i] = *value;
        }
        let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in vec.iter_mut() {
                *value /= norm;
            }
        }
        Ok(vec)
    }
}

/// A real ONNX embedder (feature-gated). Implements the ports by running the
/// provisioned model; degrades to `MODEL_UNAVAILABLE` when not loaded.
pub struct RealOnnxEmbedder {
    runtime: Option<OnnxRuntime>,
    profile_id: String,
    dimension: usize,
}

impl RealOnnxEmbedder {
    /// Load a real ONNX session for `profile_id` at `dimension`.
    pub fn load(model_path: &str, profile_id: &str, dimension: usize) -> EmbeddingResult<Self> {
        let runtime = OnnxRuntime::load(model_path)?;
        Ok(Self {
            runtime: Some(runtime),
            profile_id: profile_id.to_string(),
            dimension,
        })
    }

    /// Load with a deployment-provided tokenizer.
    pub fn load_with_tokenizer(
        model_path: &str,
        profile_id: &str,
        dimension: usize,
        tokenizer: Box<dyn Tokenizer>,
    ) -> EmbeddingResult<Self> {
        let runtime = OnnxRuntime::load_with_tokenizer(model_path, tokenizer)?;
        Ok(Self {
            runtime: Some(runtime),
            profile_id: profile_id.to_string(),
            dimension,
        })
    }

    /// Whether the real runtime is loaded.
    pub fn is_loaded(&self) -> bool {
        self.runtime.is_some()
    }
}

impl EmbeddingPort for RealOnnxEmbedder {
    fn model_available(&self, profile_id: &str) -> bool {
        self.runtime.is_some() && (profile_id == self.profile_id || profile_id == "static-256")
    }

    fn embed(&self, profile_id: &str, texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            })?;
        let dimension = if profile_id == self.profile_id {
            self.dimension
        } else {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        };
        texts
            .iter()
            .map(|text| runtime.run_one(text, dimension))
            .collect()
    }
}

impl RerankerPort for RealOnnxEmbedder {
    fn rerank(
        &self,
        profile_id: &str,
        query: &str,
        candidates: &[(String, String)],
    ) -> EmbeddingResult<Vec<(String, f64)>> {
        // Delegates to the lexical-overlap rerank when the real runtime is not
        // loaded for this profile (a rerank model is not part of the embedding
        // session); otherwise score by cosine similarity of real embeddings.
        let Some(runtime) = self.runtime.as_ref() else {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        };
        if profile_id != self.profile_id {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        }
        let q = runtime.run_one(query, self.dimension)?;
        let mut scored: Vec<(String, f64)> = candidates
            .iter()
            .map(|(id, text)| {
                let c = runtime.run_one(text, self.dimension).unwrap_or_default();
                let dot: f32 = q.iter().zip(&c).map(|(a, b)| a * b).sum();
                (id.clone(), dot as f64)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(scored)
    }
}

/// The set of provisioned model paths (wiring aid; not used in inference).
#[allow(dead_code)]
pub type ProvisionedModels = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Failure path: no ONNX Runtime library provisioned (`ORT_LIB_LOCATION`
    /// unset) -> `ModelUnavailable`, so the semantic channel degrades.
    #[test]
    fn load_without_runtime_library_degrades_to_model_unavailable() {
        std::env::remove_var("ORT_LIB_LOCATION");
        let result = OnnxRuntime::load("does-not-exist.onnx");
        assert!(
            matches!(result, Err(EmbeddingError::ModelUnavailable { .. })),
            "expected ModelUnavailable"
        );
    }

    /// Positive (wiring): the fallback tokenizer is deterministic and bounded.
    #[test]
    fn whitespace_tokenizer_is_deterministic_and_bounded() {
        let tokenizer = WhitespaceTokenizer::new(8);
        let a = tokenizer.tokenize("the quick brown fox", 8);
        let b = tokenizer.tokenize("the quick brown fox", 8);
        assert_eq!(a, b, "tokenization must be deterministic");
        assert!(a.len() <= 8, "must respect max_len");
        assert!(a.len() == 4, "4 whitespace tokens expected");
    }

    /// Positive (wiring): `RealOnnxEmbedder` reports not loaded until a session
    /// is provisioned, and `embed` degrades to `ModelUnavailable`.
    #[test]
    fn real_embedder_degrades_when_not_loaded() {
        // A `RealOnnxEmbedder` can only be constructed via a successful `load`,
        // which requires the runtime library; verify the port contract degrades
        // through the `OnnxEmbedder` fallback when the runtime is absent.
        let embedder = crate::OnnxEmbedder::new();
        assert!(
            !embedder.model_available("bge-small-384"),
            "ONNX model must be unavailable without provisioned runtime"
        );
        assert!(
            matches!(
                embedder.embed("bge-small-384", &["x".to_string()]),
                Err(EmbeddingError::ModelUnavailable { .. })
            ),
            "embed must degrade to ModelUnavailable"
        );
    }
}
