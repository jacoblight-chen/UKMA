//! Deterministic hybrid search: keyword (BM25) + static-semantic (static
//! embedding cosine), fused by `max`.
//!
//! `StaticEmbedder` is a model2vec-style static embedding: each token is
//! hashed (SHA-256) into a 256-dimensional vector; a text vector is the
//! mean pool of its token vectors, L2-normalised. It is a deterministic
//! stand-in for a neural embedding model — no model weights, no RAM gate,
//! fully reproducible (golden-safe), recorded in ADR-058. The BM25
//! constants (`k1=1.5`, `b=0.75`) and the cosine formula are the registered
//! retrieval-engine defaults (禮29.2 / 禮25.3), so the keyword channel is the
//! exact registered behaviour.

use std::sync::Arc;

use embedding_engine::{BackendMode, EmbeddingPort};
use retrieval_engine::Bm25Index;
use sha2::{Digest, Sha256};

/// Embedding dimension (static profile; recorded default, 禮29.2 analogue).
pub const EMBED_DIM: usize = 256;

/// Search / embedding policy (CodeGraph `--split-identifiers`,
/// `--full-body-embedding`, `--graph-only` parity).
///
/// Defaults preserve the existing static behaviour: identifiers are split on
/// camelCase boundaries, the corpus is name/qualified-name/signature (no body),
/// and ranking fuses keyword + static-semantic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPolicy {
    /// Split camelCase identifiers into parts (`getUserById` ->
    /// `get user by id`). CodeGraph default `true`; UKMA default `true`
    /// (preserves the existing tokeniser).
    pub split_identifiers: bool,
    /// Include the symbol's source body in the corpus. CodeGraph
    /// `--full-body-embedding` defaults to `true`; UKMA defaults to `false`
    /// to preserve the existing corpus.
    pub full_body: bool,
    /// Structural-only: rank by the keyword channel alone, skipping the
    /// static-semantic channel. CodeGraph `--graph-only` default `false`.
    pub graph_only: bool,
}

impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            split_identifiers: true,
            full_body: false,
            graph_only: false,
        }
    }
}

/// Tokeniser: lowercase, split on non-alphanumerics and at camelCase
/// boundaries (`getUserById` -> `get user by id`), so the keyword and
/// semantic channels see the same vocabulary.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut prev_alpha = false;
    let mut prev_lower = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if prev_alpha && prev_lower && ch.is_ascii_uppercase() && !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            current.extend(ch.to_lowercase());
            prev_alpha = true;
            prev_lower = !ch.is_ascii_uppercase();
        } else if prev_alpha {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            prev_alpha = false;
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Tokeniser without the camelCase split (CodeGraph `--split-identifiers
/// off` parity): lowercase, split on non-alphanumerics only
/// (`getUserById` -> `getuserbyid`).
pub fn tokenize_raw(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Tokenise according to a policy: split camelCase when
/// `split_identifiers` is set, otherwise keep identifiers whole.
pub fn tokenize_with(text: &str, policy: SearchPolicy) -> Vec<String> {
    if policy.split_identifiers {
        tokenize(text)
    } else {
        tokenize_raw(text)
    }
}

/// One token vector: the SHA-256 digest stretched to `EMBED_DIM`
/// coordinates through a 64-bit mixing (SplitMix64), mapped to [-1, 1].
fn token_vector(token: &str) -> [f32; EMBED_DIM] {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = [0.0f32; EMBED_DIM];
    for chunk in 0..4 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest[chunk * 8..chunk * 8 + 8]);
        let seed = u64::from_le_bytes(bytes);
        for dim in 0..64 {
            let mut x = seed ^ (dim as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            x ^= x >> 30;
            x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x ^= x >> 27;
            x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^= x >> 31;
            out[chunk * 64 + dim] = ((x & 0xFFFF_FFFF) as f32 / 0xFFFF_FFFFu32 as f32) * 2.0 - 1.0;
        }
    }
    out
}

/// Static embedder (no model, deterministic).
pub struct StaticEmbedder;

impl StaticEmbedder {
    /// Mean-pool + L2-normalise over a token list; empty -> zero vector.
    pub fn embed(tokens: &[String]) -> Vec<f32> {
        if tokens.is_empty() {
            return vec![0.0; EMBED_DIM];
        }
        let mut acc = [0.0f32; EMBED_DIM];
        for token in tokens {
            let tv = token_vector(token);
            for (i, v) in tv.iter().enumerate() {
                acc[i] += *v;
            }
        }
        let n = tokens.len() as f32;
        for v in acc.iter_mut() {
            *v /= n;
        }
        let norm = acc.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in acc.iter_mut() {
                *v /= norm;
            }
        }
        acc.to_vec()
    }
}

/// Hybrid index over a fixed, already-tokenised corpus (one document per
/// corpus entry, in the same order as the embeddings).
pub struct HybridIndex {
    bm25: Bm25Index,
    embeddings: Vec<Vec<f32>>,
    graph_only: bool,
    embedder: Option<(Arc<dyn EmbeddingPort>, String)>,
    backend_mode: BackendMode,
}

impl HybridIndex {
    /// Build the keyword + semantic channels from a tokenised corpus
    /// (fused ranking, the default behaviour).
    pub fn build(corpus: &[Vec<String>]) -> Self {
        Self::build_with(corpus, SearchPolicy::default())
    }

    /// Build with an explicit policy: `graph_only` switches ranking to the
    /// keyword channel alone.
    pub fn build_with(corpus: &[Vec<String>], policy: SearchPolicy) -> Self {
        Self::build_with_embedder(corpus, policy, None, BackendMode::WithModel)
    }

    /// Build with an explicit policy and an optional embedding port
    /// (CodeGraph `--embedding-model` parity). When the mode is
    /// `WithModel` and the selected profile is available, the semantic
    /// channel uses the port; otherwise it falls back to the static embedder
    /// (or the keyword channel alone under `GraphOnly`).
    pub fn build_with_embedder(
        corpus: &[Vec<String>],
        policy: SearchPolicy,
        embedder: Option<(Arc<dyn EmbeddingPort>, String)>,
        backend_mode: BackendMode,
    ) -> Self {
        let partial = Self {
            bm25: Bm25Index::build(corpus),
            embeddings: Vec::new(),
            graph_only: policy.graph_only,
            embedder,
            backend_mode,
        };
        let embeddings = corpus
            .iter()
            .map(|doc| partial.embed_tokens(doc, false))
            .collect();
        Self {
            embeddings,
            ..partial
        }
    }

    /// Embed a token list: use the selected port when it is available and the
    /// mode is `WithModel`; otherwise fall back to the static embedder.
    /// `GraphOnly` mode (RAM-gate degradation) skips the semantic channel.
    fn embed_tokens(&self, tokens: &[String], skip: bool) -> Vec<f32> {
        if skip || self.backend_mode == BackendMode::GraphOnly {
            return Vec::new();
        }
        if let Some((port, profile_id)) = &self.embedder {
            if self.backend_mode == BackendMode::WithModel && port.model_available(profile_id) {
                let joined = tokens.join(" ");
                if let Ok(vectors) = port.embed(profile_id, &[joined]) {
                    if let Some(vector) = vectors.into_iter().next() {
                        return vector;
                    }
                }
            }
        }
        StaticEmbedder::embed(tokens)
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.embeddings.len()
    }

    /// Empty check.
    pub fn is_empty(&self) -> bool {
        self.embeddings.is_empty()
    }

    /// Rank all documents for a text query; scores are `max(bm25_norm,
    /// cosine)`, normalised so the best match is 1.0 and the rest are in
    /// [0, 1]. Documents scoring 0 are omitted.
    pub fn rank(&self, query: &str) -> Vec<(usize, f64)> {
        self.rank_with(query, SearchPolicy::default())
    }

    /// Rank with an explicit policy. The query is tokenised the same way
    /// the corpus was; `graph_only` scores by the keyword channel alone.
    pub fn rank_with(&self, query: &str, policy: SearchPolicy) -> Vec<(usize, f64)> {
        let query_tokens = tokenize_with(query, policy);
        if query_tokens.is_empty() || self.is_empty() {
            return Vec::new();
        }
        let query_vector = self.embed_tokens(&query_tokens, self.graph_only);
        let keyword = self.bm25.rank(&query_tokens).unwrap_or_default();
        let max_keyword = keyword.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
        let max_keyword = if max_keyword > 0.0 { max_keyword } else { 1.0 };
        let mut results: Vec<(usize, f64)> = Vec::new();
        for (i, _) in self.embeddings.iter().enumerate() {
            let cos = if self.graph_only {
                0.0
            } else {
                retrieval_engine::cosine(&query_vector, &self.embeddings[i])
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0)
            };
            let kw = keyword
                .iter()
                .find(|(idx, _)| *idx == i)
                .map(|(_, s)| s / max_keyword)
                .unwrap_or(0.0);
            let score = if self.graph_only { kw } else { kw.max(cos) };
            if score > 0.0 {
                results.push((i, score));
            }
        }
        results.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenization_splits_camel_and_lowercase() {
        assert_eq!(tokenize("getUserById"), vec!["get", "user", "by", "id"]);
        // Only the lowercase->uppercase boundary splits; digit boundaries do
        // not (documented behaviour).
        assert_eq!(tokenize("SHA256digest()"), vec!["sha256digest"]);
        assert_eq!(tokenize("snake_case_name"), vec!["snake", "case", "name"]);
    }

    #[test]
    fn tokenize_raw_keeps_identifiers_whole() {
        assert_eq!(tokenize_raw("getUserById"), vec!["getuserbyid"]);
        assert_eq!(
            tokenize_raw("snake_case_name"),
            vec!["snake", "case", "name"]
        );
    }

    #[test]
    fn tokenize_with_switches_between_modes() {
        let split = SearchPolicy {
            split_identifiers: true,
            ..SearchPolicy::default()
        };
        let whole = SearchPolicy {
            split_identifiers: false,
            ..SearchPolicy::default()
        };
        assert_eq!(
            tokenize_with("getUserById", split),
            vec!["get", "user", "by", "id"]
        );
        assert_eq!(tokenize_with("getUserById", whole), vec!["getuserbyid"]);
    }

    #[test]
    fn graph_only_excludes_documents_without_keyword_overlap() {
        let corpus = vec![
            tokenize("authenticate user session token verification"),
            tokenize("render template into html output"),
        ];
        let index = HybridIndex::build_with(
            &corpus,
            SearchPolicy {
                graph_only: true,
                ..SearchPolicy::default()
            },
        );
        let ranked = index.rank_with(
            "user session token validation",
            SearchPolicy {
                graph_only: true,
                ..SearchPolicy::default()
            },
        );
        assert_eq!(ranked.len(), 1, "only the keyword-overlapping document");
        assert_eq!(ranked[0].0, 0);
    }

    #[test]
    fn embeddings_are_deterministic_and_normalised() {
        let tokens = vec!["hello".to_string(), "world".to_string()];
        let a = StaticEmbedder::embed(&tokens);
        let b = StaticEmbedder::embed(&tokens);
        assert_eq!(a, b);
        let norm = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hybrid_ranks_semantically_close_documents_first() {
        let corpus = vec![
            tokenize("authenticate user session token verification"),
            tokenize("render template into html output"),
            tokenize("validate the user session token before login"),
        ];
        let index = HybridIndex::build(&corpus);
        let ranked = index.rank("user session token validation");
        assert!(!ranked.is_empty());
        assert!(ranked[0].0 == 0 || ranked[0].0 == 2);
        assert!(ranked[0].1 >= ranked[ranked.len() - 1].1);
    }

    #[test]
    fn empty_corpus_and_empty_query_rank_to_nothing() {
        let index = HybridIndex::build(&[]);
        assert!(index.rank("anything").is_empty());
        let index = HybridIndex::build(&[tokenize("hello world")]);
        assert!(index.rank("   ").is_empty());
    }
}
