//! Embedding and reranker ports plus the model profile registry (§7.4).
//!
//! **No model is bundled.** The default embedder refuses with
//! `MODEL_UNAVAILABLE`; a deterministic test double exists for wiring tests and
//! is explicitly *not* semantic — it must never be used for retrieval quality
//! claims or as `ML-003` evidence. The `static` channel (aliases `static-code`
//! / `model2vec`) additionally uses a provisioned model2vec lookup-table
//! directory (`CODEGRAPH_STATIC_MODEL` or the CodeGraph-compatible
//! `~/.codegraph/static_models/jina-code-static-256`) when the `onnx`
//! feature is enabled and the directory is present; otherwise it degrades to
//! the deterministic channel.
//!
//! Profile identity is what makes vectors comparable (PROP-07, `DB-004`): model,
//! revision, digest, tokenizer, dimension, metric, normalization and language
//! support are all part of the profile.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use storage_ports::{EmbeddingProfile, Metric};

#[cfg(feature = "onnx")]
mod onnx_proto;
#[cfg(feature = "onnx")]
mod onnx_runtime;
#[cfg(feature = "onnx")]
pub use onnx_runtime::{
    HfTokenizer, HfTokenizerError, OnnxRuntime, Pooling, RealOnnxEmbedder, Tokenizer,
    WhitespaceTokenizer,
};
#[cfg(feature = "onnx")]
mod static_model;
#[cfg(feature = "onnx")]
pub use static_model::{
    resolve_static_model_dir, Model2VecEmbedder, StaticConfig, StaticModel,
    DEFAULT_STATIC_MODEL_NAME,
};

/// Embedding/reranker failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingError {
    /// No model is available for this profile.
    ModelUnavailable {
        /// Profile id.
        profile_id: String,
    },
    /// Two profiles disagree on dimension or metric for the same id.
    ModelProfileMismatch {
        /// Detail.
        detail: String,
    },
    /// Unknown profile.
    UnknownProfile {
        /// Profile id.
        profile_id: String,
    },
    /// Malformed argument.
    InvalidArgument {
        /// Offending field.
        field: &'static str,
        /// Why.
        reason: String,
    },
    /// A model artifact (config, tokenizer or weight file) is malformed or
    /// uses a format this build cannot read.
    UnsupportedFormat {
        /// Artifact kind (e.g. `safetensors`, `static_config`).
        kind: &'static str,
        /// Why.
        reason: String,
    },
}

impl EmbeddingError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            EmbeddingError::ModelUnavailable { .. } => "MODEL_UNAVAILABLE",
            EmbeddingError::ModelProfileMismatch { .. } => "MODEL_PROFILE_MISMATCH",
            EmbeddingError::UnknownProfile { .. } => "INVALID_ARGUMENT",
            EmbeddingError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            EmbeddingError::UnsupportedFormat { .. } => "INVALID_ARGUMENT",
        }
    }
}

impl std::fmt::Display for EmbeddingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbeddingError::ModelUnavailable { profile_id } => {
                write!(f, "model unavailable for profile `{profile_id}`")
            }
            EmbeddingError::ModelProfileMismatch { detail } => {
                write!(f, "profile mismatch: {detail}")
            }
            EmbeddingError::UnknownProfile { profile_id } => {
                write!(f, "unknown profile `{profile_id}`")
            }
            EmbeddingError::InvalidArgument { field, reason } => write!(f, "{field}: {reason}"),
            EmbeddingError::UnsupportedFormat { kind, reason } => {
                write!(f, "unsupported {kind} format: {reason}")
            }
        }
    }
}

impl std::error::Error for EmbeddingError {}

/// Result alias.
pub type EmbeddingResult<T> = Result<T, EmbeddingError>;

/// Privacy handling of a model provider (§7.4/§14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyPolicy {
    /// Runs locally; no data leaves the process.
    LocalOnly,
    /// Remote call allowed only under an explicit egress policy.
    RemoteWithEgressPolicy,
}

/// Complete model profile (§7.4 registry fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfile {
    /// Stable profile id.
    pub profile_id: String,
    /// Model identity.
    pub model_id: String,
    /// Model revision.
    pub model_revision: String,
    /// Weight digest (`sha256:<hex>`) when known.
    pub digest: String,
    /// Tokenizer revision.
    pub tokenizer_revision: String,
    /// Vector dimension.
    pub dimension: usize,
    /// Metric.
    pub metric: Metric,
    /// Normalization policy.
    pub normalization: String,
    /// Supported language tags.
    pub language_support: Vec<String>,
    /// Privacy handling.
    pub privacy_policy: PrivacyPolicy,
}

impl ModelProfile {
    /// Validate the fields that make vectors comparable.
    pub fn validate(&self) -> EmbeddingResult<()> {
        if self.profile_id.trim().is_empty() {
            return Err(EmbeddingError::InvalidArgument {
                field: "profile_id",
                reason: "must be non-empty".into(),
            });
        }
        if self.dimension == 0 {
            return Err(EmbeddingError::InvalidArgument {
                field: "dimension",
                reason: "must be >= 1".into(),
            });
        }
        if self.language_support.is_empty() {
            return Err(EmbeddingError::InvalidArgument {
                field: "language_support",
                reason: "must list at least one language".into(),
            });
        }
        Ok(())
    }

    /// Convert to the vector port's profile so the same identity is used by the
    /// store (no second source of truth).
    pub fn to_vector_profile(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            profile_id: self.profile_id.clone(),
            model_id: self.model_id.clone(),
            model_revision: self.model_revision.clone(),
            dimension: self.dimension,
            metric: self.metric,
            normalization: self.normalization.clone(),
        }
    }

    /// Whether two profiles may share a vector space (PROP-07).
    pub fn comparable_with(&self, other: &ModelProfile) -> bool {
        self.dimension == other.dimension
            && self.metric == other.metric
            && self.model_id == other.model_id
            && self.model_revision == other.model_revision
            && self.normalization == other.normalization
    }
}

/// Registry of declared model profiles.
#[derive(Debug, Default)]
pub struct ModelRegistry {
    profiles: BTreeMap<String, ModelProfile>,
}

impl ModelRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a profile; re-registering the same id with a different
    /// dimension/metric/model is rejected (`MODEL_PROFILE_MISMATCH`).
    pub fn register(&mut self, profile: ModelProfile) -> EmbeddingResult<()> {
        profile.validate()?;
        if let Some(existing) = self.profiles.get(&profile.profile_id) {
            if !existing.comparable_with(&profile) {
                return Err(EmbeddingError::ModelProfileMismatch {
                    detail: format!(
                        "profile `{}` already registered as {}/{} dim {}",
                        profile.profile_id,
                        existing.model_id,
                        existing.model_revision,
                        existing.dimension
                    ),
                });
            }
        }
        self.profiles.insert(profile.profile_id.clone(), profile);
        Ok(())
    }

    /// Look up a profile.
    pub fn get(&self, profile_id: &str) -> Option<&ModelProfile> {
        self.profiles.get(profile_id)
    }

    /// All registered profiles.
    pub fn all(&self) -> Vec<&ModelProfile> {
        self.profiles.values().collect()
    }

    /// Profiles that can serve a language tag.
    pub fn for_language(&self, language: &str) -> Vec<&ModelProfile> {
        self.profiles
            .values()
            .filter(|profile| {
                profile
                    .language_support
                    .iter()
                    .any(|supported| supported == language || supported == "*")
            })
            .collect()
    }
}

/// The embedding port (§7.4).
pub trait EmbeddingPort: Send + Sync {
    /// Embed a batch of texts under one profile.
    fn embed(&self, profile_id: &str, texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>>;

    /// Whether the model for the profile is available (runtime + weights
    /// present). Defaults to `true` for embedders that are always usable.
    fn model_available(&self, _profile_id: &str) -> bool {
        true
    }
}

/// The reranker port (§7.4).
pub trait RerankerPort: Send + Sync {
    /// Rerank candidate ids against a query; returns `(id, score)` best first.
    fn rerank(
        &self,
        profile_id: &str,
        query: &str,
        candidates: &[(String, String)],
    ) -> EmbeddingResult<Vec<(String, f64)>>;
}

/// Default embedder: no model wired, every call is `MODEL_UNAVAILABLE`.
#[derive(Debug, Default)]
pub struct UnavailableEmbedder;

impl EmbeddingPort for UnavailableEmbedder {
    fn embed(&self, profile_id: &str, _texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Err(EmbeddingError::ModelUnavailable {
            profile_id: profile_id.to_string(),
        })
    }

    fn model_available(&self, _profile_id: &str) -> bool {
        false
    }
}

impl RerankerPort for UnavailableEmbedder {
    fn rerank(
        &self,
        profile_id: &str,
        _query: &str,
        _candidates: &[(String, String)],
    ) -> EmbeddingResult<Vec<(String, f64)>> {
        Err(EmbeddingError::ModelUnavailable {
            profile_id: profile_id.to_string(),
        })
    }
}

/// **Test double only.** Deterministic hashed bag-of-words embedding.
///
/// This is *not* a semantic model: cosine similarities carry no meaning beyond
/// lexical overlap. It exists to exercise wiring (dimension checks, profile
/// identity, index generation) and MUST NOT be used for retrieval quality claims
/// or cited as `ML-003` evidence.
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dimensions: usize,
}

impl HashEmbedder {
    /// New double with a fixed output dimension.
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }

    fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0f32; self.dimensions];
        for token in text.split_whitespace() {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in token.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
            let index = (hash % self.dimensions as u64) as usize;
            vector[index] += 1.0;
        }
        let norm: f32 = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in vector.iter_mut() {
                *value /= norm;
            }
        }
        vector
    }
}

impl EmbeddingPort for HashEmbedder {
    fn embed(&self, _profile_id: &str, texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| self.embed_one(text)).collect())
    }
}

/// Named selectable embedding models (CodeGraph `--embedding-model` parity).
///
/// The default model is [`Qwen3Embedding06bInt8`]
/// (`qwen3-embedding-0.6b-int8`, 1024 dimensions, INT8-quantized ONNX);
/// see [`EmbeddingModel::default`] / [`EmbeddingModel::DEFAULT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingModel {
    /// bge-small: 384 dimensions, fast ONNX transformer.
    BgeSmall,
    /// jina-code-v2: 768 dimensions.
    JinaCodeV2,
    /// granite-97m: 384 dimensions, 32K context.
    Granite97m,
    /// static: model2vec 256 dimensions, no ONNX runtime. Selectable as
    /// `static` (or the CodeGraph-compatible aliases `static-code` /
    /// `model2vec`); when a static model directory is provisioned the real
    /// model2vec lookup table is used, otherwise the deterministic
    /// token-statistics channel answers.
    Static,
    /// qwen3-embedding-0.6b-int8: 1024 dimensions, INT8-quantized ONNX.
    /// This is the **default** selectable model.
    Qwen3Embedding06bInt8,
    /// bge-m3-int8: 1024 dimensions, INT8-quantized ONNX, multilingual.
    BgeM3Int8,
}

impl EmbeddingModel {
    /// The default selectable model: `qwen3-embedding-0.6b-int8` (1024
    /// dimensions, INT8-quantized ONNX). Used when no model is explicitly
    /// selected (CodeGraph `--embedding-model` parity).
    pub const DEFAULT: Self = Self::Qwen3Embedding06bInt8;

    /// The six selectable model names.
    pub const NAMES: &'static [&'static str] = &[
        "bge-small",
        "jina-code-v2",
        "granite-97m",
        "static",
        "qwen3-embedding-0.6b-int8",
        "bge-m3-int8",
    ];

    /// Parse a model name; `None` when unrecognised. `static-code` and
    /// `model2vec` are CodeGraph-compatible aliases of `static` (the same
    /// model2vec lookup-table channel).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "bge-small" => Some(EmbeddingModel::BgeSmall),
            "jina-code-v2" => Some(EmbeddingModel::JinaCodeV2),
            "granite-97m" => Some(EmbeddingModel::Granite97m),
            "static" | "static-code" | "model2vec" => Some(EmbeddingModel::Static),
            "qwen3-embedding-0.6b-int8" => Some(EmbeddingModel::Qwen3Embedding06bInt8),
            "bge-m3-int8" => Some(EmbeddingModel::BgeM3Int8),
            _ => None,
        }
    }

    /// The registry / embedder profile id.
    pub fn profile_id(self) -> &'static str {
        match self {
            EmbeddingModel::BgeSmall => "bge-small-384",
            EmbeddingModel::JinaCodeV2 => "jina-code-v2-768",
            EmbeddingModel::Granite97m => "granite-97m-384",
            EmbeddingModel::Static => "static-256",
            EmbeddingModel::Qwen3Embedding06bInt8 => "qwen3-embedding-0.6b-int8-1024",
            EmbeddingModel::BgeM3Int8 => "bge-m3-int8-1024",
        }
    }

    /// The vector dimension.
    pub fn dimension(self) -> usize {
        match self {
            EmbeddingModel::BgeSmall => 384,
            EmbeddingModel::JinaCodeV2 => 768,
            EmbeddingModel::Granite97m => 384,
            EmbeddingModel::Static => 256,
            EmbeddingModel::Qwen3Embedding06bInt8 => 1024,
            EmbeddingModel::BgeM3Int8 => 1024,
        }
    }

    /// Whether the model requires the ONNX runtime (false for `static`).
    pub fn requires_onnx(self) -> bool {
        !matches!(self, EmbeddingModel::Static)
    }
}

impl Default for EmbeddingModel {
    /// The default model is `qwen3-embedding-0.6b-int8` (see
    /// [`EmbeddingModel::DEFAULT`]).
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Build the standard `ModelRegistry` pre-populated with the four selectable
/// models (CodeGraph `--embedding-model` parity). Built from a trusted, distinct
/// set, so registration cannot fail.
pub fn standard_registry() -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    for model in [
        EmbeddingModel::BgeSmall,
        EmbeddingModel::JinaCodeV2,
        EmbeddingModel::Granite97m,
        EmbeddingModel::Static,
        EmbeddingModel::Qwen3Embedding06bInt8,
        EmbeddingModel::BgeM3Int8,
    ] {
        let profile = ModelProfile {
            profile_id: model.profile_id().to_string(),
            model_id: model.profile_id().to_string(),
            model_revision: "1".into(),
            digest: String::new(),
            tokenizer_revision: "tok-1".into(),
            dimension: model.dimension(),
            metric: Metric::Cosine,
            normalization: "l2".into(),
            language_support: vec!["*".into()],
            privacy_policy: PrivacyPolicy::LocalOnly,
        };
        let _ = registry.register(profile);
    }
    registry
}

/// ONNX-backed embedder (CodeGraph ONNX model parity).
///
/// Honest capability boundary: `static` is always available (model2vec-style,
/// no ONNX runtime). The ONNX models (`bge-small`, `jina-code-v2`,
/// `granite-97m`, `qwen3-embedding-0.6b-int8`, `bge-m3-int8`) are available
/// only when their runtime + weight files have been provisioned; otherwise
/// every call is `MODEL_UNAVAILABLE`. A provisioned
/// model yields a deterministic embedding at the profile dimension so wiring
/// (dimension checks, profile identity) can be exercised without a real model.
#[derive(Debug, Clone, Default)]
pub struct OnnxEmbedder {
    available: std::collections::BTreeSet<String>,
}

impl OnnxEmbedder {
    /// No model provisioned (only `static` is usable).
    pub fn new() -> Self {
        Self {
            available: Default::default(),
        }
    }

    /// Mark a model's runtime + weights as provisioned.
    pub fn with_provisioned(mut self, profile_id: &str) -> Self {
        self.available.insert(profile_id.to_string());
        self
    }

    /// Whether the model's runtime + weights are present (`static` always).
    pub fn model_available(&self, profile_id: &str) -> bool {
        profile_id == "static-256" || self.available.contains(profile_id)
    }

    fn dimension_for(profile_id: &str) -> usize {
        match profile_id {
            "bge-small-384" => 384,
            "jina-code-v2-768" => 768,
            "granite-97m-384" => 384,
            "qwen3-embedding-0.6b-int8-1024" => 1024,
            "bge-m3-int8-1024" => 1024,
            _ => 256,
        }
    }

    fn embed_one(&self, text: &str, dimension: usize) -> Vec<f32> {
        let mut vector = vec![0.0f32; dimension];
        for token in text.split_whitespace() {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in token.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
            let index = (hash % dimension as u64) as usize;
            vector[index] += 1.0;
        }
        let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in vector.iter_mut() {
                *value /= norm;
            }
        }
        vector
    }
}

impl EmbeddingPort for OnnxEmbedder {
    fn model_available(&self, profile_id: &str) -> bool {
        profile_id == "static-256" || self.available.contains(profile_id)
    }

    fn embed(&self, profile_id: &str, texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>> {
        if !self.model_available(profile_id) {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        }
        let dimension = Self::dimension_for(profile_id);
        Ok(texts
            .iter()
            .map(|text| self.embed_one(text, dimension))
            .collect())
    }
}

impl RerankerPort for OnnxEmbedder {
    fn rerank(
        &self,
        profile_id: &str,
        query: &str,
        candidates: &[(String, String)],
    ) -> EmbeddingResult<Vec<(String, f64)>> {
        if !self.model_available(profile_id) {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        }
        // Deterministic lexical overlap rerank (not semantic).
        let query_tokens: Vec<String> =
            query.split_whitespace().map(|t| t.to_lowercase()).collect();
        let max = query_tokens.len() as f64;
        let mut scored: Vec<(String, f64)> = candidates
            .iter()
            .map(|(id, text)| {
                let lowered = text.to_lowercase();
                let overlap = query_tokens
                    .iter()
                    .filter(|q| lowered.contains(&**q))
                    .count() as f64;
                (id.clone(), if max > 0.0 { overlap / max } else { 0.0 })
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(scored)
    }
}

/// The ~1.5 GB RAM gate for loading the ONNX model (CodeGraph parity).
pub const RAM_GATE_BYTES: u64 = 1_500_000_000;

/// Read `CODEGRAPH_SKIP_MEMORY_CHECK` (accepts `1` / `true` / `yes`).
pub fn skip_memory_check() -> bool {
    std::env::var("CODEGRAPH_SKIP_MEMORY_CHECK")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
}

/// Whether the ONNX model may be loaded given available memory.
pub fn model_gate_passes(available_bytes: u64) -> bool {
    skip_memory_check() || available_bytes >= RAM_GATE_BYTES
}

/// The effective mode after the RAM gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendMode {
    /// Load the embedding model.
    WithModel,
    /// Degrade to graph-only (no embeddings).
    GraphOnly,
}

/// Probe available system memory.
pub trait MemoryProbe: Send + Sync {
    /// Available (free) bytes.
    fn available_bytes(&self) -> u64;
}

/// A fixed probe (tests / deterministic wiring).
#[derive(Debug, Clone, Copy)]
pub struct FixedMemoryProbe {
    bytes: u64,
}

/// Best-effort probe of the host's free RAM. On Windows this reads
/// `GlobalMemoryStatusEx`; elsewhere it falls back to a healthy-host
/// assumption (the production OS probe is an integration point).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemMemoryProbe;

impl SystemMemoryProbe {
    /// New probe.
    pub fn new() -> Self {
        Self
    }
}

impl MemoryProbe for SystemMemoryProbe {
    fn available_bytes(&self) -> u64 {
        #[cfg(target_os = "windows")]
        {
            #[repr(C)]
            #[allow(non_snake_case)]
            struct MemoryStatusEx {
                dwLength: u32,
                dwMemoryStatus: u32,
                ullTotalPhys: u64,
                ullAvailPhys: u64,
                ullTotalPageFile: u64,
                ullAvailPageFile: u64,
                ullTotalVirtual: u64,
                ullAvailVirtual: u64,
                ullAvailExtendedVirtual: u64,
            }
            extern "system" {
                fn GlobalMemoryStatusEx(lpBuffer: *mut MemoryStatusEx) -> i32;
            }
            let mut status = MemoryStatusEx {
                dwLength: std::mem::size_of::<MemoryStatusEx>() as u32,
                dwMemoryStatus: 0,
                ullTotalPhys: 0,
                ullAvailPhys: 0,
                ullTotalPageFile: 0,
                ullAvailPageFile: 0,
                ullTotalVirtual: 0,
                ullAvailVirtual: 0,
                ullAvailExtendedVirtual: 0,
            };
            unsafe {
                if GlobalMemoryStatusEx(&mut status) != 0 {
                    return status.ullAvailPhys;
                }
            }
            // Fallback if the API is unavailable: assume a healthy host.
            RAM_GATE_BYTES * 2
        }
        #[cfg(not(target_os = "windows"))]
        {
            RAM_GATE_BYTES * 2
        }
    }
}

impl FixedMemoryProbe {
    pub fn new(bytes: u64) -> Self {
        Self { bytes }
    }
}

impl MemoryProbe for FixedMemoryProbe {
    fn available_bytes(&self) -> u64 {
        self.bytes
    }
}

/// Decide the effective backend mode (RAM gate + env override).
pub fn effective_mode(probe: &dyn MemoryProbe) -> BackendMode {
    if model_gate_passes(probe.available_bytes()) {
        BackendMode::WithModel
    } else {
        BackendMode::GraphOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, dimension: usize, metric: Metric) -> ModelProfile {
        ModelProfile {
            profile_id: id.to_string(),
            model_id: "bge-m3".into(),
            model_revision: "1".into(),
            digest: "sha256:0000000000000000".into(),
            tokenizer_revision: "tok-1".into(),
            dimension,
            metric,
            normalization: "l2".into(),
            language_support: vec!["*".into()],
            privacy_policy: PrivacyPolicy::LocalOnly,
        }
    }

    #[test]
    fn profile_validation_rejects_empty_fields() {
        let mut invalid = profile("p", 1024, Metric::Cosine);
        invalid.dimension = 0;
        assert_eq!(invalid.validate().unwrap_err().code(), "INVALID_ARGUMENT");

        let mut no_language = profile("p", 1024, Metric::Cosine);
        no_language.language_support.clear();
        assert!(no_language.validate().is_err());

        let mut no_id = profile("", 1024, Metric::Cosine);
        no_id.profile_id.clear();
        assert!(no_id.validate().is_err());
    }

    #[test]
    fn bge_m3_profile_converts_to_the_vector_port_profile() {
        let bge = profile("bge-m3-1024", 1024, Metric::Cosine);
        let vector_profile = bge.to_vector_profile();
        assert_eq!(vector_profile.profile_id, "bge-m3-1024");
        assert_eq!(vector_profile.dimension, 1024);
        assert_eq!(vector_profile.metric, Metric::Cosine);
    }

    #[test]
    fn registry_rejects_dimension_or_metric_drift_for_the_same_id() {
        let mut registry = ModelRegistry::new();
        registry
            .register(profile("p", 1024, Metric::Cosine))
            .unwrap();
        // identical re-registration is idempotent
        registry
            .register(profile("p", 1024, Metric::Cosine))
            .unwrap();

        let error = registry
            .register(profile("p", 512, Metric::Cosine))
            .unwrap_err();
        assert_eq!(error.code(), "MODEL_PROFILE_MISMATCH");

        let error = registry
            .register(profile("p", 1024, Metric::L2))
            .unwrap_err();
        assert_eq!(error.code(), "MODEL_PROFILE_MISMATCH");
    }

    #[test]
    fn registry_lookup_and_language_filtering() {
        let mut registry = ModelRegistry::new();
        registry
            .register(profile("multi", 1024, Metric::Cosine))
            .unwrap();
        let mut english_only = profile("en", 384, Metric::Cosine);
        english_only.language_support = vec!["en".into()];
        registry.register(english_only).unwrap();

        assert!(registry.get("multi").is_some());
        assert!(registry.get("missing").is_none());
        assert_eq!(registry.all().len(), 2);
        assert_eq!(registry.for_language("zh").len(), 1);
        assert_eq!(registry.for_language("en").len(), 2);
    }

    #[test]
    fn profiles_are_only_comparable_when_fully_identical() {
        let base = profile("a", 1024, Metric::Cosine);
        assert!(base.comparable_with(&profile("b", 1024, Metric::Cosine)));

        let mut different_model = profile("b", 1024, Metric::Cosine);
        different_model.model_id = "other".into();
        assert!(!base.comparable_with(&different_model));

        let mut different_revision = profile("b", 1024, Metric::Cosine);
        different_revision.model_revision = "2".into();
        assert!(!base.comparable_with(&different_revision));

        assert!(!base.comparable_with(&profile("b", 512, Metric::Cosine)));
        assert!(!base.comparable_with(&profile("b", 1024, Metric::L2)));
    }

    #[test]
    fn default_embedder_refuses_instead_of_inventing_vectors() {
        let embedder = UnavailableEmbedder;
        let error = embedder.embed("bge", &["hello".into()]).unwrap_err();
        assert_eq!(error.code(), "MODEL_UNAVAILABLE");
        let error = embedder.rerank("bge", "q", &[]).unwrap_err();
        assert_eq!(error.code(), "MODEL_UNAVAILABLE");
    }

    #[test]
    fn test_double_is_deterministic_normalised_and_fixed_dimension() {
        let embedder = HashEmbedder::new(16);
        let first = embedder
            .embed("p", &["graph traversal".into(), "unrelated".into()])
            .unwrap();
        let second = embedder
            .embed("p", &["graph traversal".into(), "unrelated".into()])
            .unwrap();
        assert_eq!(first, second, "deterministic");
        assert!(first.iter().all(|vector| vector.len() == 16));
        let norm: f32 = first[0]
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "normalised: {norm}");
        // lexical overlap only: same tokens => same vector
        let repeat = embedder.embed("p", &["traversal graph".into()]).unwrap();
        assert_eq!(first[0], repeat[0], "bag of words ignores order");
    }

    #[test]
    fn test_double_handles_empty_text() {
        let embedder = HashEmbedder::new(8);
        let vectors = embedder.embed("p", &[String::new()]).unwrap();
        assert_eq!(vectors[0], vec![0.0; 8], "empty text yields a zero vector");
    }

    #[test]
    fn embedding_model_parses_names_and_dimensions() {
        assert_eq!(
            EmbeddingModel::from_name("bge-small").unwrap().dimension(),
            384
        );
        assert_eq!(
            EmbeddingModel::from_name("jina-code-v2")
                .unwrap()
                .dimension(),
            768
        );
        assert_eq!(
            EmbeddingModel::from_name("granite-97m")
                .unwrap()
                .dimension(),
            384
        );
        assert_eq!(
            EmbeddingModel::from_name("static").unwrap().dimension(),
            256
        );
        assert_eq!(
            EmbeddingModel::from_name("qwen3-embedding-0.6b-int8")
                .unwrap()
                .dimension(),
            1024
        );
        assert_eq!(
            EmbeddingModel::from_name("bge-m3-int8")
                .unwrap()
                .dimension(),
            1024
        );
        assert!(!EmbeddingModel::from_name("static").unwrap().requires_onnx());
        assert!(EmbeddingModel::from_name("bge-small")
            .unwrap()
            .requires_onnx());
        assert!(EmbeddingModel::from_name("qwen3-embedding-0.6b-int8")
            .unwrap()
            .requires_onnx());
        assert!(EmbeddingModel::from_name("bge-m3-int8")
            .unwrap()
            .requires_onnx());
        assert!(EmbeddingModel::from_name("bogus").is_none());
    }

    /// The default model is `qwen3-embedding-0.6b-int8` (1024 dimensions,
    /// INT8 ONNX), reachable through both the `Default` impl and the
    /// `DEFAULT` constant, and resolvable by name.
    #[test]
    fn embedding_model_default_is_qwen3_embedding_06b_int8() {
        let default = EmbeddingModel::default();
        assert_eq!(default, EmbeddingModel::DEFAULT);
        assert_eq!(default, EmbeddingModel::Qwen3Embedding06bInt8);
        assert_eq!(
            default.profile_id(),
            "qwen3-embedding-0.6b-int8-1024",
            "the default resolves to the qwen3 INT8 profile"
        );
        assert_eq!(default.dimension(), 1024);
        assert!(
            default.requires_onnx(),
            "the default requires the ONNX runtime"
        );
        assert_eq!(
            EmbeddingModel::from_name("qwen3-embedding-0.6b-int8")
                .expect("the default name parses"),
            EmbeddingModel::default(),
            "the default name round-trips to the default"
        );
    }

    #[test]
    fn standard_registry_holds_the_six_selectable_models() {
        let registry = standard_registry();
        assert_eq!(registry.all().len(), 6);
        assert_eq!(registry.get("bge-small-384").unwrap().dimension, 384);
        assert_eq!(registry.get("jina-code-v2-768").unwrap().dimension, 768);
        assert_eq!(registry.get("granite-97m-384").unwrap().dimension, 384);
        assert_eq!(registry.get("static-256").unwrap().dimension, 256);
        assert_eq!(
            registry
                .get("qwen3-embedding-0.6b-int8-1024")
                .unwrap()
                .dimension,
            1024
        );
        assert_eq!(registry.get("bge-m3-int8-1024").unwrap().dimension, 1024);
    }

    #[test]
    fn re_registering_a_profile_with_a_different_dimension_is_rejected() {
        let mut registry = standard_registry();
        let mut altered = registry.get("bge-m3-int8-1024").unwrap().clone();
        altered.dimension = 384;
        let error = registry.register(altered).unwrap_err();
        assert_eq!(error.code(), "MODEL_PROFILE_MISMATCH");
        // The original registration is untouched.
        assert_eq!(registry.get("bge-m3-int8-1024").unwrap().dimension, 1024);
    }

    #[test]
    fn onnx_embedder_static_is_always_available_and_onnx_models_default_unavailable() {
        let embedder = OnnxEmbedder::new();
        // static always works (no ONNX runtime).
        let static_vectors = embedder
            .embed("static-256", &["hello world".into()])
            .unwrap();
        assert_eq!(static_vectors[0].len(), 256);
        // ONNX models are unavailable by default (no runtime / weights).
        for profile_id in [
            "bge-small-384",
            "jina-code-v2-768",
            "granite-97m-384",
            "qwen3-embedding-0.6b-int8-1024",
            "bge-m3-int8-1024",
        ] {
            let error = embedder
                .embed(profile_id, &["hello world".into()])
                .unwrap_err();
            assert_eq!(error.code(), "MODEL_UNAVAILABLE");
        }
        assert!(!embedder.model_available("bge-small-384"));
        assert!(embedder.model_available("static-256"));
    }

    #[test]
    fn onnx_embedder_provisioned_model_embeds_at_the_profile_dimension() {
        let embedder = OnnxEmbedder::new().with_provisioned("bge-small-384");
        assert!(embedder.model_available("bge-small-384"));
        let vectors = embedder
            .embed("bge-small-384", &["traversal graph".into()])
            .unwrap();
        assert_eq!(vectors[0].len(), 384, "384-dim bge-small");
        // Deterministic + normalised.
        let repeat = embedder
            .embed("bge-small-384", &["traversal graph".into()])
            .unwrap();
        assert_eq!(vectors[0], repeat[0]);
        let norm: f32 = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn onnx_embedder_provisioned_1024_models_embed_at_the_profile_dimension() {
        let embedder = OnnxEmbedder::new()
            .with_provisioned("qwen3-embedding-0.6b-int8-1024")
            .with_provisioned("bge-m3-int8-1024");
        for (profile_id, name) in [
            (
                "qwen3-embedding-0.6b-int8-1024",
                "qwen3-embedding-0.6b-int8",
            ),
            ("bge-m3-int8-1024", "bge-m3-int8"),
        ] {
            assert!(
                embedder.model_available(profile_id),
                "{name} is provisioned"
            );
            let vectors = embedder
                .embed(profile_id, &["traversal graph".into()])
                .unwrap();
            assert_eq!(vectors[0].len(), 1024, "{name} is 1024-dim");
            // Deterministic + normalised.
            let repeat = embedder
                .embed(profile_id, &["traversal graph".into()])
                .unwrap();
            assert_eq!(vectors[0], repeat[0]);
            let norm: f32 = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-5, "{name} is normalised");
        }
    }

    #[test]
    fn the_ram_gate_degrades_below_1_5gb_and_env_bypasses() {
        // Positive: enough memory loads the model.
        assert!(model_gate_passes(RAM_GATE_BYTES + 1));
        // Negative: below the gate (and no env override) degrades.
        assert!(!model_gate_passes(RAM_GATE_BYTES - 1));
        // effective_mode follows the probe.
        assert_eq!(
            effective_mode(&FixedMemoryProbe::new(RAM_GATE_BYTES + 1)),
            BackendMode::WithModel
        );
        assert_eq!(
            effective_mode(&FixedMemoryProbe::new(RAM_GATE_BYTES - 1)),
            BackendMode::GraphOnly
        );
    }

    #[test]
    fn the_ram_gate_env_override_bypasses_the_threshold() {
        std::env::set_var("CODEGRAPH_SKIP_MEMORY_CHECK", "yes");
        // With the override set, even a low-memory probe loads the model.
        assert!(model_gate_passes(1));
        assert_eq!(
            effective_mode(&FixedMemoryProbe::new(1)),
            BackendMode::WithModel
        );
        std::env::remove_var("CODEGRAPH_SKIP_MEMORY_CHECK");
        // After clearing, the gate is in force again.
        assert!(!model_gate_passes(1));
    }
}
