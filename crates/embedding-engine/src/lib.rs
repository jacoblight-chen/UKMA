//! Embedding and reranker ports plus the model profile registry (§7.4).
//!
//! **No model is bundled.** The default embedder refuses with
//! `MODEL_UNAVAILABLE`; a deterministic test double exists for wiring tests and
//! is explicitly *not* semantic — it must never be used for retrieval quality
//! claims or as `ML-003` evidence.
//!
//! Profile identity is what makes vectors comparable (PROP-07, `DB-004`): model,
//! revision, digest, tokenizer, dimension, metric, normalization and language
//! support are all part of the profile.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use storage_ports::{EmbeddingProfile, Metric};

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
}

impl EmbeddingError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            EmbeddingError::ModelUnavailable { .. } => "MODEL_UNAVAILABLE",
            EmbeddingError::ModelProfileMismatch { .. } => "MODEL_PROFILE_MISMATCH",
            EmbeddingError::UnknownProfile { .. } => "INVALID_ARGUMENT",
            EmbeddingError::InvalidArgument { .. } => "INVALID_ARGUMENT",
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
}
