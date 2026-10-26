//! Real static model2vec validation (environment-gated).
//!
//! Loads the deployment-provisioned model2vec lookup-table directory
//! (`config.json` + `tokenizer.json` + `model.safetensors`) and asserts
//! embedding-quality invariants on the *real* model (e.g. the CodeGraph
//! `jina-code-static-256` release):
//!
//! * `CODEGRAPH_STATIC_MODEL` — the static model directory (when unset, the
//!   CodeGraph-compatible default `~/.codegraph/static_models/...` is tried)
//!
//! When no directory is found the tests print SKIP and pass; the invariants
//! only run against the real artifacts.

#[cfg(feature = "onnx")]
mod static_model_tests {
    use embedding_engine::{
        resolve_static_model_dir, EmbeddingError, EmbeddingPort, Model2VecEmbedder, RerankerPort,
    };
    use std::path::Path;

    fn dir() -> Option<std::path::PathBuf> {
        resolve_static_model_dir().filter(|d| d.is_dir())
    }

    /// Static model invariants: dimensionality, L2 normalization,
    /// determinism, and code-semantic ordering on the real lookup table.
    #[test]
    fn static_model_embedding_invariants() {
        let Some(dir) = dir() else {
            eprintln!("SKIP static_model_embedding_invariants (set CODEGRAPH_STATIC_MODEL or provision ~/.codegraph/static_models/jina-code-static-256)");
            return;
        };
        let embedder = Model2VecEmbedder::load(dir.as_path(), "static-256", 256)
            .expect("static model must load");
        assert!(embedder.model_available("static-256"));

        let similar = [
            "connection pool",
            "database connection pooling configuration",
        ];
        let dissimilar = ["connection pool", "banana smoothie recipe"];
        let sims = embedder
            .embed(
                "static-256",
                &[similar[0].to_string(), similar[1].to_string()],
            )
            .expect("embed similar pair");
        let diss = embedder
            .embed(
                "static-256",
                &[dissimilar[0].to_string(), dissimilar[1].to_string()],
            )
            .expect("embed dissimilar pair");

        for v in sims.iter().chain(diss.iter()) {
            assert_eq!(v.len(), 256, "dimension mismatch");
            let n: f32 = v.iter().map(|x| x * x).sum();
            assert!((n.sqrt() - 1.0).abs() < 1e-3, "L2 norm not ~1: {n}");
        }
        // Determinism: identical inputs produce identical embeddings.
        let again = embedder
            .embed("static-256", &[similar[0].to_string()])
            .expect("embed again");
        assert_eq!(sims[0], again[0], "embeddings must be deterministic");

        let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() as f64;
        let c_sim = cosine(&sims[0], &sims[1]);
        let c_dis = cosine(&diss[0], &diss[1]);
        assert!(
            c_sim > c_dis,
            "code-semantic ordering violated: similar={c_sim} dissimilar={c_dis}"
        );
        eprintln!(
            "OK static_model_embedding_invariants: c_sim={c_sim:.4} c_dis={c_dis:.4} ({})",
            dir.display()
        );
    }

    /// Rerank on static embeddings orders the code-similar candidate first.
    #[test]
    fn static_model_rerank_orders_similar_first() {
        let Some(dir) = dir() else {
            eprintln!("SKIP static_model_rerank_orders_similar_first (static model dir not found)");
            return;
        };
        let embedder = Model2VecEmbedder::load(dir.as_path(), "static-256", 256)
            .expect("static model must load");
        let scored = embedder
            .rerank(
                "static-256",
                "connection pool",
                &[
                    (
                        "dissimilar".to_string(),
                        "banana smoothie recipe".to_string(),
                    ),
                    (
                        "similar".to_string(),
                        "database connection pooling configuration".to_string(),
                    ),
                ],
            )
            .expect("rerank");
        assert_eq!(
            scored.first().map(|(id, _)| id.as_str()),
            Some("similar"),
            "rerank order: {scored:?}"
        );
        eprintln!("OK static_model_rerank_orders_similar_first: {scored:?}");
    }

    /// Availability/authorization path: a profile other than the loaded one
    /// must be reported unavailable (no cross-profile inference).
    #[test]
    fn static_model_wrong_profile_is_unavailable() {
        let Some(dir) = dir() else {
            eprintln!(
                "SKIP static_model_wrong_profile_is_unavailable (static model dir not found)"
            );
            return;
        };
        let embedder = Model2VecEmbedder::load(dir.as_path(), "static-256", 256)
            .expect("static model must load");
        assert!(!embedder.model_available("bge-small-384"));
        assert!(
            matches!(
                embedder.embed("bge-small-384", &["x".to_string()]),
                Err(EmbeddingError::ModelUnavailable { .. })
            ),
            "cross-profile embed must be unavailable"
        );
        eprintln!("OK static_model_wrong_profile_is_unavailable");
    }

    /// The loader must reject a directory whose shape is not a static model
    /// (missing files / malformed weights) rather than inventing vectors.
    #[test]
    fn static_model_missing_dir_is_unavailable() {
        let probe = Path::new("definitely-not-a-static-model-dir");
        assert!(matches!(
            Model2VecEmbedder::load(probe, "static-256", 256),
            Err(EmbeddingError::ModelUnavailable { .. })
        ));
        eprintln!("OK static_model_missing_dir_is_unavailable");
    }
}

#[cfg(not(feature = "onnx"))]
mod no_onnx {
    #[test]
    fn static_model_tests_require_the_onnx_feature() {
        eprintln!("SKIP static model tests (build with --features onnx)");
    }
}
