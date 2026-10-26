//! Real-model validation (environment-gated).
//!
//! Loads the deployment-provisioned ONNX Runtime model and tokenizer and
//! asserts embedding-quality invariants on *real* INT8 weights and the
//! model-matching tokenizer:
//!
//! * `ORT_LIB_LOCATION`        — ONNX Runtime library path
//! * `UKMA_ONNX_MODEL_PATH`    — `model_int8.onnx`
//! * `UKMA_ONNX_TOKENIZER_PATH`— `tokenizer.json`
//! * `UKMA_TEST_MODEL`         — `qwen3` or `bge-m3` (selects profile/pooling)
//!
//! When any variable is unset the tests print SKIP and pass; the invariants
//! only run against the real artifacts.

#[cfg(feature = "onnx")]
mod real_models {
    use embedding_engine::{
        EmbeddingError, EmbeddingPort, HfTokenizer, Pooling, RealOnnxEmbedder, RerankerPort,
    };

    struct ModelConfig {
        model_path: String,
        tokenizer_path: String,
        profile_id: &'static str,
        dimension: usize,
        pooling: Pooling,
        max_len: usize,
        /// A profile that is NOT the one under test (for the unavailable
        /// cross-profile check).
        other_profile: &'static str,
        /// Model-appropriate semantic pair (natural language for text models,
        /// code for the code-embedding model).
        similar: [&'static str; 2],
        dissimilar: [&'static str; 2],
    }

    fn config() -> Option<ModelConfig> {
        let c = match std::env::var("UKMA_TEST_MODEL").as_deref() {
            Ok("qwen3") => (
                "qwen3-embedding-0.6b-int8-1024",
                1024,
                Pooling::LastToken,
                8192,
                "bge-m3-int8-1024",
                ["the cat sat on the mat", "a cat sat on a mat"],
                [
                    "the cat sat on the mat",
                    "quarterly financial report on revenue growth",
                ],
            ),
            Ok("bge-m3") => (
                "bge-m3-int8-1024",
                1024,
                Pooling::Mean,
                8192,
                "qwen3-embedding-0.6b-int8-1024",
                ["the cat sat on the mat", "a cat sat on a mat"],
                [
                    "the cat sat on the mat",
                    "quarterly financial report on revenue growth",
                ],
            ),
            Ok("bge-small") => (
                "bge-small-384",
                384,
                Pooling::Cls,
                512,
                "jina-code-v2-768",
                ["the cat sat on the mat", "a cat sat on a mat"],
                [
                    "the cat sat on the mat",
                    "quarterly financial report on revenue growth",
                ],
            ),
            Ok("jina-v2") => (
                "jina-code-v2-768",
                768,
                Pooling::Mean,
                8192,
                "bge-small-384",
                [
                    "def foo(x: int) -> int { return x + 1; }",
                    "def bar(x: int) -> int { return x + 1; }",
                ],
                [
                    "def foo(x: int) -> int { return x + 1; }",
                    "SELECT * FROM users WHERE id = 1;",
                ],
            ),
            Ok("granite") => (
                "granite-97m-384",
                384,
                Pooling::Cls,
                32768,
                "bge-small-384",
                ["the cat sat on the mat", "a cat sat on a mat"],
                [
                    "the cat sat on the mat",
                    "quarterly financial report on revenue growth",
                ],
            ),
            _ => return None,
        };
        let (profile_id, dimension, pooling, max_len, other_profile, similar, dissimilar) = c;
        let model_path = std::env::var("UKMA_ONNX_MODEL_PATH").ok()?;
        let tokenizer_path = std::env::var("UKMA_ONNX_TOKENIZER_PATH").ok()?;
        Some(ModelConfig {
            model_path,
            tokenizer_path,
            profile_id,
            dimension,
            pooling,
            max_len,
            other_profile,
            similar,
            dissimilar,
        })
    }

    fn load(cfg: &ModelConfig) -> Result<RealOnnxEmbedder, String> {
        let tok =
            HfTokenizer::from_file(&cfg.tokenizer_path).map_err(|e| format!("tokenizer: {e}"))?;
        RealOnnxEmbedder::load_with_config(
            &cfg.model_path,
            cfg.profile_id,
            cfg.dimension,
            Box::new(tok),
            cfg.pooling,
            cfg.max_len,
        )
        .map_err(|e| format!("load: {e:?}"))
    }

    fn cosine(a: &[f32], b: &[f32]) -> f64 {
        a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() as f64
    }

    fn norm(v: &[f32]) -> f64 {
        v.iter().map(|x| x * x).sum::<f32>().sqrt() as f64
    }

    /// Real-model invariants: dimensionality, L2 normalization, determinism,
    /// and semantic ordering (similar > dissimilar) on real INT8 inference.
    #[test]
    fn real_model_embedding_invariants() {
        let Some(cfg) = config() else {
            eprintln!("SKIP real_model_embedding_invariants (set UKMA_TEST_MODEL, UKMA_ONNX_MODEL_PATH, UKMA_ONNX_TOKENIZER_PATH, ORT_LIB_LOCATION)");
            return;
        };
        let embedder = load(&cfg).expect("real model must load");
        assert!(embedder.is_loaded());
        assert!(embedder.model_available(cfg.profile_id));

        let similar: Vec<String> = cfg.similar.iter().map(|s| s.to_string()).collect();
        let dissimilar: Vec<String> = cfg.dissimilar.iter().map(|s| s.to_string()).collect();
        let sims = embedder
            .embed(cfg.profile_id, &similar)
            .expect("embed similar pair");
        let diss = embedder
            .embed(cfg.profile_id, &dissimilar)
            .expect("embed dissimilar pair");

        for v in sims.iter().chain(diss.iter()) {
            assert_eq!(v.len(), cfg.dimension, "dimension mismatch");
            assert!((norm(v) - 1.0).abs() < 1e-2, "L2 norm not ~1: {}", norm(v));
        }
        // Determinism: identical inputs produce identical embeddings.
        let again = embedder
            .embed(cfg.profile_id, &[similar[0].clone()])
            .expect("embed again");
        assert_eq!(sims[0], again[0], "embeddings must be deterministic");

        let c_sim = cosine(&sims[0], &sims[1]);
        let c_dis = cosine(&diss[0], &diss[1]);
        assert!(
            c_sim > c_dis,
            "semantic ordering violated: similar={c_sim} dissimilar={c_dis}"
        );
        assert!(c_sim > 0.4, "similar pair unexpectedly low: {c_sim}");
        eprintln!(
            "OK real_model_embedding_invariants ({}): c_sim={c_sim:.4} c_dis={c_dis:.4}",
            cfg.profile_id
        );
    }

    /// Rerank on real embeddings orders the semantically similar candidate
    /// first.
    #[test]
    fn real_model_rerank_orders_similar_first() {
        let Some(cfg) = config() else {
            eprintln!("SKIP real_model_rerank_orders_similar_first (env not set)");
            return;
        };
        let embedder = load(&cfg).expect("real model must load");
        let query = cfg.similar[0].to_string();
        let scored = embedder
            .rerank(
                cfg.profile_id,
                &query,
                &[
                    ("dissimilar".to_string(), cfg.dissimilar[1].to_string()),
                    ("similar".to_string(), cfg.similar[1].to_string()),
                ],
            )
            .expect("rerank");
        assert_eq!(
            scored.first().map(|(id, _)| id.as_str()),
            Some("similar"),
            "rerank order: {scored:?}"
        );
        eprintln!(
            "OK real_model_rerank_orders_similar_first ({}): {scored:?}",
            cfg.profile_id
        );
    }

    /// Authorization/availability path: a profile other than the loaded one
    /// must be reported unavailable (no cross-profile inference).
    #[test]
    fn real_model_wrong_profile_is_unavailable() {
        let Some(cfg) = config() else {
            eprintln!("SKIP real_model_wrong_profile_is_unavailable (env not set)");
            return;
        };
        let embedder = load(&cfg).expect("real model must load");
        let other = cfg.other_profile;
        assert!(
            !embedder.model_available(other),
            "a different profile must not be available: {other}"
        );
        assert!(
            matches!(
                embedder.embed(other, &["x".to_string()]),
                Err(EmbeddingError::ModelUnavailable { .. })
            ),
            "cross-profile embed must be unavailable"
        );
        eprintln!(
            "OK real_model_wrong_profile_is_unavailable ({})",
            cfg.profile_id
        );
    }
}

#[cfg(not(feature = "onnx"))]
mod no_onnx {
    #[test]
    fn real_model_tests_require_the_onnx_feature() {
        eprintln!("SKIP real model tests (build with --features onnx)");
    }
}
