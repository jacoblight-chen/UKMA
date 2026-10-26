//! Static model2vec lookup-table embedder (the CodeGraph `static` /
//! `static-code` / `model2vec` channel).
//!
//! A static model directory contains a HuggingFace `tokenizer.json`, a small
//! `config.json` (`normalize`, `hidden_dim`) and a `model.safetensors` file
//! holding the per-token embedding rows (`embeddings` tensor, shape
//! `[vocab, dim]`) plus optional per-token `weights` (sublinear
//! frequency / SIF). Embedding a text is a table lookup, not a neural
//! network pass: tokenize without special tokens or truncation, gather the
//! token rows, weight them, average and L2-normalize.
//!
//! The model is provisioned out of band; the canonical directory is
//! `~/.codegraph/static_models/jina-code-static-256` (a CodeGraph-compatible
//! location) or a directory named by `CODEGRAPH_STATIC_MODEL` (also a
//! CodeGraph-compatible convention), both recorded in the migration
//! ledger per the spec. When no directory is present the caller must fall
//! back to the deterministic static-256 channel.

use std::path::{Path, PathBuf};

use crate::onnx_runtime::HfTokenizer;
use crate::{EmbeddingError, EmbeddingPort, EmbeddingResult, RerankerPort};

/// Default location of the static model when `CODEGRAPH_STATIC_MODEL` is not
/// set: `~/.codegraph/static_models/jina-code-static-256` (`HOME`, falling
/// back to `USERPROFILE` on Windows). CodeGraph-compatible behavior.
pub const DEFAULT_STATIC_MODEL_NAME: &str = "jina-code-static-256";

/// Resolve the static model directory: `CODEGRAPH_STATIC_MODEL` if set and
/// non-empty, else the default `~/.codegraph/static_models/...` location.
/// The directory may not exist; callers check before loading.
pub fn resolve_static_model_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CODEGRAPH_STATIC_MODEL") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".codegraph")
            .join("static_models")
            .join(DEFAULT_STATIC_MODEL_NAME),
    )
}

/// Parsed `config.json` for a static model.
#[derive(Clone, Debug)]
pub struct StaticConfig {
    pub normalize: bool,
    pub hidden_dim: Option<usize>,
}

impl StaticConfig {
    fn from_json(raw: &str) -> EmbeddingResult<Self> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| EmbeddingError::UnsupportedFormat {
                kind: "static_config",
                reason: e.to_string(),
            })?;
        let normalize = value
            .get("normalize")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let hidden_dim = value
            .get("hidden_dim")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        Ok(Self {
            normalize,
            hidden_dim,
        })
    }
}

struct StaticTensor {
    dtype: String,
    shape: Vec<usize>,
    data: Vec<u8>,
}

/// A loaded static model2vec.
pub struct StaticModel {
    pub config: StaticConfig,
    pub tokenizer: HfTokenizer,
    vocab: usize,
    dim: usize,
    rows: Vec<f32>,
    weights: Vec<f32>,
}

impl StaticModel {
    /// Load `config.json`, `tokenizer.json` and `model.safetensors` from
    /// `dir`.
    pub fn from_dir(dir: &Path) -> EmbeddingResult<Self> {
        let read = |name: &str| -> EmbeddingResult<Vec<u8>> {
            let path = dir.join(name);
            std::fs::read(&path).map_err(|_| EmbeddingError::ModelUnavailable {
                profile_id: format!("static:{name}"),
            })
        };
        let config = StaticConfig::from_json(&String::from_utf8_lossy(&read("config.json")?))?;
        let tokenizer = HfTokenizer::from_json(&String::from_utf8_lossy(&read("tokenizer.json")?))
            .map_err(|e| EmbeddingError::UnsupportedFormat {
                kind: "tokenizer",
                reason: e.to_string(),
            })?;
        let tensors = parse_safetensors(&read("model.safetensors")?)?;
        let embeddings = tensors
            .get("embeddings")
            .ok_or(EmbeddingError::UnsupportedFormat {
                kind: "static_model",
                reason: "missing `embeddings` tensor".to_string(),
            })?;
        if embeddings.shape.len() != 2 {
            return Err(EmbeddingError::UnsupportedFormat {
                kind: "static_model",
                reason: format!(
                    "`embeddings` must be 2-D, got {} dims",
                    embeddings.shape.len()
                ),
            });
        }
        let vocab = embeddings.shape[0];
        let dim = embeddings.shape[1];
        if config.hidden_dim.is_some_and(|h| h != dim) {
            return Err(EmbeddingError::InvalidArgument {
                field: "static_model",
                reason: format!(
                    "config hidden_dim {:?} != tensor dim {dim}",
                    config.hidden_dim
                ),
            });
        }
        let rows = tensor_as_f32(embeddings)?;
        if rows.len() != vocab * dim {
            return Err(EmbeddingError::UnsupportedFormat {
                kind: "static_model",
                reason: "truncated `embeddings` data".to_string(),
            });
        }
        let weights = match tensors.get("weights") {
            Some(t) => {
                let w = tensor_as_f32(t)?;
                if w.len() != vocab {
                    return Err(EmbeddingError::UnsupportedFormat {
                        kind: "static_model",
                        reason: "truncated `weights` data".to_string(),
                    });
                }
                w
            }
            None => vec![1.0f32; vocab],
        };
        Ok(Self {
            config,
            tokenizer,
            vocab,
            dim,
            rows,
            weights,
        })
    }

    /// Embedding dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Embed one text: weighted mean of the token rows (unknown token ids
    /// are skipped; weights default to 1), L2-normalized when configured.
    /// Empty input or all-OOV input yields the zero vector.
    pub fn encode(&self, text: &str) -> EmbeddingResult<Vec<f32>> {
        let ids = self.tokenizer.tokenize_content(text);
        let mut acc = vec![0.0f32; self.dim];
        let mut used = 0usize;
        for &id in &ids {
            if id < 0 {
                continue;
            }
            let i = id as usize;
            if i >= self.vocab {
                continue; // OOV: skip like the CodeGraph model2vec path
            }
            let w = self.weights[i];
            let row = &self.rows[i * self.dim..(i + 1) * self.dim];
            for k in 0..self.dim {
                acc[k] += row[k] * w;
            }
            used += 1;
        }
        if used > 0 {
            let inv = 1.0 / used as f32;
            for v in &mut acc {
                *v *= inv;
            }
        }
        if self.config.normalize {
            let norm = acc.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 0.0 {
                for v in &mut acc {
                    *v /= norm;
                }
            }
        }
        Ok(acc)
    }

    /// Cosine similarity (dot product of unit vectors; `0.0` when either
    /// side is degenerate).
    fn cosine(a: &[f32], b: &[f32]) -> f64 {
        let (na, nb) = (
            a.iter().map(|v| v * v).sum::<f32>().sqrt(),
            b.iter().map(|v| v * v).sum::<f32>().sqrt(),
        );
        if na == 0.0 || nb == 0.0 {
            return 0.0;
        }
        a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() as f64 / (na * nb) as f64
    }
}

/// Minimal safetensors reader: a little-endian `u64` header length, a UTF-8
/// JSON header mapping tensor names to `{dtype, shape, data_offsets}`, then
/// the packed data. Supports `F32`/`F64`/`F16` tensors.
fn parse_safetensors(
    bytes: &[u8],
) -> EmbeddingResult<std::collections::HashMap<String, StaticTensor>> {
    if bytes.len() < 8 {
        return Err(EmbeddingError::UnsupportedFormat {
            kind: "safetensors",
            reason: "file shorter than the 8-byte header length".to_string(),
        });
    }
    let header_len = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
    if header_len == 0 || 8 + header_len > bytes.len() {
        return Err(EmbeddingError::UnsupportedFormat {
            kind: "safetensors",
            reason: "invalid header length".to_string(),
        });
    }
    let header_json = std::str::from_utf8(&bytes[8..8 + header_len]).map_err(|_| {
        EmbeddingError::UnsupportedFormat {
            kind: "safetensors",
            reason: "header is not valid UTF-8".to_string(),
        }
    })?;
    let header: serde_json::Value =
        serde_json::from_str(header_json).map_err(|e| EmbeddingError::UnsupportedFormat {
            kind: "safetensors",
            reason: format!("header JSON: {e}"),
        })?;
    let data_start = 8 + header_len;
    let mut out = std::collections::HashMap::new();
    for (name, value) in header
        .as_object()
        .ok_or(EmbeddingError::UnsupportedFormat {
            kind: "safetensors",
            reason: "header is not an object".to_string(),
        })?
    {
        if name.starts_with("__metadata__") {
            continue;
        }
        let dtype = value.get("dtype").and_then(|v| v.as_str()).ok_or(
            EmbeddingError::UnsupportedFormat {
                kind: "safetensors",
                reason: format!("tensor {name} has no dtype"),
            },
        )?;
        let shape: Vec<usize> = value
            .get("shape")
            .and_then(|v| v.as_array())
            .and_then(|a| a.iter().map(|d| d.as_u64().map(|d| d as usize)).collect())
            .ok_or(EmbeddingError::UnsupportedFormat {
                kind: "safetensors",
                reason: format!("tensor {name} has no shape"),
            })?;
        let offsets = value.get("data_offsets").and_then(|v| v.as_array());
        let (Some(start), Some(end)) = (
            offsets.and_then(|a| a.first()).and_then(|v| v.as_u64()),
            offsets.and_then(|a| a.get(1)).and_then(|v| v.as_u64()),
        ) else {
            return Err(EmbeddingError::UnsupportedFormat {
                kind: "safetensors",
                reason: format!("tensor {name} has no data_offsets"),
            });
        };
        let (start, end) = (start as usize, end as usize);
        if start >= end || end > bytes.len() - data_start {
            return Err(EmbeddingError::UnsupportedFormat {
                kind: "safetensors",
                reason: format!("tensor {name} offsets out of range"),
            });
        }
        if !matches!(dtype, "F32" | "F64" | "F16") {
            return Err(EmbeddingError::UnsupportedFormat {
                kind: "safetensors",
                reason: format!("tensor {name} has unsupported dtype {dtype}"),
            });
        }
        let expected = shape.iter().product::<usize>() * dtype_bytes(dtype);
        if end - start != expected {
            return Err(EmbeddingError::UnsupportedFormat {
                kind: "safetensors",
                reason: format!("tensor {name} size {end:?} does not match its dtype/shape"),
            });
        }
        out.insert(
            name.clone(),
            StaticTensor {
                dtype: dtype.to_string(),
                shape,
                data: bytes[data_start + start..data_start + end].to_vec(),
            },
        );
    }
    Ok(out)
}

fn dtype_bytes(dtype: &str) -> usize {
    match dtype {
        "F32" => 4,
        "F64" => 8,
        "F16" => 2,
        _ => 0,
    }
}

/// Convert a dtype-checked tensor (`F32`/`F64`/`F16`) to f32 values.
fn tensor_as_f32(t: &StaticTensor) -> EmbeddingResult<Vec<f32>> {
    let count = t.shape.iter().product::<usize>();
    let elem = dtype_bytes(&t.dtype);
    if t.data.len() != count * elem {
        return Err(EmbeddingError::UnsupportedFormat {
            kind: "safetensors",
            reason: "tensor data size mismatch".to_string(),
        });
    }
    let mut out = Vec::with_capacity(count);
    for c in 0..count {
        match elem {
            4 => {
                let v = u32::from_le_bytes(t.data[c * 4..c * 4 + 4].try_into().unwrap());
                out.push(f32::from_bits(v));
            }
            8 => {
                let v = u64::from_le_bytes(t.data[c * 8..c * 8 + 8].try_into().unwrap());
                out.push(f64::from_bits(v) as f32);
            }
            2 => {
                let v = u16::from_le_bytes(t.data[c * 2..c * 2 + 2].try_into().unwrap());
                out.push(f16_to_f32(v));
            }
            _ => {
                return Err(EmbeddingError::UnsupportedFormat {
                    kind: "safetensors",
                    reason: format!("unsupported dtype {}", t.dtype),
                })
            }
        }
    }
    Ok(out)
}

/// IEEE-754 half to single precision (including subnormals).
fn f16_to_f32(h: u16) -> f32 {
    let sign = (h >> 15) & 1;
    let exp = (h >> 10) & 0x1f;
    let mant = h & 0x3ff;
    let value = if exp == 0 {
        if mant == 0 {
            0.0f64
        } else {
            // Subnormal half: value = mant * 2^-24, exact in f64.
            (mant as f64) * 2f64.powi(-24)
        }
    } else if exp == 0x1f {
        if mant == 0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        let e = (exp as i32) - 15 + 127;
        (mant as f64 / 1024.0 + 1.0) * 2f64.powi(e - 127)
    };
    if sign == 1 {
        -value as f32
    } else {
        value as f32
    }
}

/// Static model2vec embedder: a `None` model degrades to
/// `MODEL_UNAVAILABLE` exactly like the ONNX runtime does.
pub struct Model2VecEmbedder {
    model: Option<StaticModel>,
    profile_id: &'static str,
}

impl Model2VecEmbedder {
    pub fn unavailable(profile_id: &'static str) -> Self {
        Self {
            model: None,
            profile_id,
        }
    }

    /// Load the static model from `dir` for `profile`/`dimension`. If the
    /// tensor dimension does not match `dimension`, the model is treated as
    /// unavailable so the caller can degrade to the deterministic channel.
    pub fn load(dir: &Path, profile_id: &'static str, dimension: usize) -> EmbeddingResult<Self> {
        let model = StaticModel::from_dir(dir)?;
        if model.dim() != dimension {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        }
        Ok(Self {
            model: Some(model),
            profile_id,
        })
    }

    pub fn model(&self) -> Option<&StaticModel> {
        self.model.as_ref()
    }
}

impl EmbeddingPort for Model2VecEmbedder {
    fn model_available(&self, profile_id: &str) -> bool {
        self.model.is_some() && profile_id == self.profile_id
    }

    fn embed(&self, profile_id: &str, texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let model = self
            .model
            .as_ref()
            .ok_or_else(|| EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            })?;
        if profile_id != self.profile_id {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        }
        texts.iter().map(|text| model.encode(text)).collect()
    }
}

impl RerankerPort for Model2VecEmbedder {
    fn rerank(
        &self,
        profile_id: &str,
        query: &str,
        candidates: &[(String, String)],
    ) -> EmbeddingResult<Vec<(String, f64)>> {
        let model = self
            .model
            .as_ref()
            .ok_or_else(|| EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            })?;
        if profile_id != self.profile_id {
            return Err(EmbeddingError::ModelUnavailable {
                profile_id: profile_id.to_string(),
            });
        }
        let q = model.encode(query)?;
        let mut scored: Vec<(String, f64)> = candidates
            .iter()
            .map(|(id, text)| {
                let e = model.encode(text).unwrap_or_default();
                (id.clone(), StaticModel::cosine(&q, &e))
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_fixture(name: &str) -> Vec<u8> {
        std::fs::read(fixture_dir().join(name)).expect("static_bpe fixture")
    }

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/static_bpe")
    }

    #[test]
    fn resolves_env_dir_when_set() {
        std::env::set_var("CODEGRAPH_STATIC_MODEL", "/tmp/static-model");
        assert_eq!(
            resolve_static_model_dir().as_deref(),
            Some(Path::new("/tmp/static-model"))
        );
        std::env::remove_var("CODEGRAPH_STATIC_MODEL");
    }

    #[test]
    fn safetensors_rejects_short_and_bad_files() {
        assert!(matches!(
            parse_safetensors(b"xx"),
            Err(EmbeddingError::UnsupportedFormat { .. })
        ));
        // header length points past EOF
        let mut bytes = vec![99u8, 0, 0, 0, 0, 0, 0, 0];
        bytes.extend_from_slice(br#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}"#);
        assert!(matches!(
            parse_safetensors(&bytes),
            Err(EmbeddingError::UnsupportedFormat { .. })
        ));
        // unsupported dtype
        let header = br#"{"a":{"dtype":"I32","shape":[1],"data_offsets":[0,4]}}"#;
        let mut bytes = vec![0u8; 8];
        bytes[0..8].copy_from_slice(&(header.len() as u64).to_le_bytes());
        bytes.extend_from_slice(header);
        bytes.extend_from_slice(&[0u8; 4]);
        assert!(matches!(
            parse_safetensors(&bytes),
            Err(EmbeddingError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn f16_roundtrips_normal_subnormal_and_infinity() {
        assert!((f16_to_f32(0x3C00) - 1.0).abs() < 1e-6);
        assert!(f16_to_f32(0x7C00).is_infinite());
        assert!(f16_to_f32(0xFC00).is_infinite()); // -inf, not NaN
        assert!(f16_to_f32(0xFC01).is_nan());
        assert!((f16_to_f32(0x0001) - 5.9604645e-8).abs() < 1e-12);
        assert!((f16_to_f32(0x8001) + 5.9604645e-8).abs() < 1e-12);
        assert!((f16_to_f32(0x7BFF) - 65504.0).abs() < 1.0);
    }

    fn assert_near(actual: &[f32], expect: [f32; 4], label: &str) {
        assert_eq!(actual.len(), 4, "{label}: length");
        for (got, want) in actual.iter().zip(expect) {
            assert!((got - want).abs() < 1e-6, "{label}: {got} vs {want}");
        }
    }

    /// Fixture: BPE `{"a":0,"b":1,"ab":2,"c":3," b":4}`, merge `a b`,
    /// rows a=[1,0,0,0] b=[0,1,0,0] ab=[0,0,1,1] c=[1,1,0,0] ` b`=[0,0,1,0],
    /// weights [1,1,1,2,1], normalize=true, hidden_dim=4.
    #[test]
    fn fixture_model_loads_and_embeds_by_hand_computed_values() {
        let model = StaticModel::from_dir(&fixture_dir()).expect("load fixture");
        assert_eq!(model.dim(), 4);

        // "ab" -> chunk "ab" -> ids [2] -> 1*[0,0,1,1] -> normalize.
        assert_near(
            &model.encode("ab").expect("ab"),
            [0.0, 0.0, 1.0 / 2f32.sqrt(), 1.0 / 2f32.sqrt()],
            "ab",
        );

        // "a b" -> chunks "a", " b"; the space token is not in the vocab
        // and is dropped -> ids [0, 1] -> [1,0,0,0] + [0,1,0,0] -> /2
        // -> [0.5,0.5,0,0] -> normalize (norm sqrt(0.5)).
        assert_near(
            &model.encode("a b").expect("a b"),
            [1.0 / 2f32.sqrt(), 1.0 / 2f32.sqrt(), 0.0, 0.0],
            "a b",
        );

        // "c" -> ids [3] -> weight 2 * [1,1,0,0] -> normalize.
        assert_near(
            &model.encode("c").expect("c"),
            [1.0 / 2f32.sqrt(), 1.0 / 2f32.sqrt(), 0.0, 0.0],
            "c",
        );

        // "z" -> token absent from the BPE vocab -> dropped -> zero vector.
        let v = model.encode("z").expect("z");
        assert!(v.iter().all(|x| *x == 0.0), "z drops to zero");

        // empty text -> zero vector, stays zero after normalize.
        let v = model.encode("").expect("empty");
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn model2vec_embedder_degrades_without_a_model() {
        let emb = Model2VecEmbedder::unavailable("static-256");
        assert!(!emb.model_available("static-256"));
        assert!(matches!(
            emb.embed("static-256", &["x".to_string()]),
            Err(EmbeddingError::ModelUnavailable { .. })
        ));
        assert!(matches!(
            emb.rerank("static-256", "q", &[("a".to_string(), "x".to_string())]),
            Err(EmbeddingError::ModelUnavailable { .. })
        ));
    }

    #[test]
    fn model2vec_embedder_rejects_other_profiles() {
        let emb = Model2VecEmbedder::load(&fixture_dir(), "static", 4).expect("load");
        assert!(emb.model_available("static"));
        assert!(!emb.model_available("static-256"));
        assert!(matches!(
            emb.embed("static-256", &["ab".to_string()]),
            Err(EmbeddingError::ModelUnavailable { .. })
        ));
    }

    #[test]
    fn model2vec_rerank_orders_by_cosine() {
        let emb = Model2VecEmbedder::load(&fixture_dir(), "static", 4).expect("load");
        let out = emb
            .rerank(
                "static",
                "ab",
                &[
                    ("close".to_string(), "ab".to_string()),
                    ("far".to_string(), "a b".to_string()),
                ],
            )
            .expect("rerank");
        assert_eq!(out[0].0, "close");
        assert!(out[0].1 > out[1].1);
    }

    #[test]
    fn load_rejects_dimension_mismatch() {
        assert!(matches!(
            Model2VecEmbedder::load(&fixture_dir(), "static", 128),
            Err(EmbeddingError::ModelUnavailable { .. })
        ));
        assert!(matches!(
            Model2VecEmbedder::load(Path::new("definitely-missing-dir"), "static", 4),
            Err(EmbeddingError::ModelUnavailable { .. })
        ));
    }

    #[test]
    fn config_parses_normalize_and_hidden_dim() {
        let c = StaticConfig::from_json(r#"{"normalize": false, "hidden_dim": 8}"#).expect("parse");
        assert!(!c.normalize);
        assert_eq!(c.hidden_dim, Some(8));
        let c = StaticConfig::from_json(r#"{}"#).expect("defaults");
        assert!(c.normalize);
        assert_eq!(c.hidden_dim, None);
        assert!(StaticConfig::from_json("not json").is_err());
    }

    fn encode_ab_without_normalization(dir: &Path) -> Vec<f32> {
        std::fs::write(dir.join("tokenizer.json"), read_fixture("tokenizer.json")).unwrap();
        std::fs::write(
            dir.join("model.safetensors"),
            read_fixture("model.safetensors"),
        )
        .unwrap();
        std::fs::write(dir.join("config.json"), r#"{"normalize": false}"#).unwrap();
        let model = StaticModel::from_dir(dir).expect("load");
        model.encode("ab").expect("encode")
    }

    #[test]
    fn normalize_off_keeps_raw_mean() {
        // No `tempfile` dependency: a pid-unique scratch dir under the
        // system temp, removed on the way out.
        let dir = std::env::temp_dir().join(format!("ukma_static_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let result = encode_ab_without_normalization(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        // no normalization: 1 * [0,0,1,1] / 1 = [0,0,1,1]
        assert_eq!(result, vec![0.0, 0.0, 1.0, 1.0]);
    }
}
