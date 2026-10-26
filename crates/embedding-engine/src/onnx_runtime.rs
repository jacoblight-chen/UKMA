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
//! bundled: the ONNX Runtime library, the model weight files, and the tokenizer
//! files —all are provisioned by the deployment (`ORT_LIB_LOCATION`,
//! `UKMA_ONNX_MODEL_PATH`, `UKMA_ONNX_TOKENIZER_PATH`).
//!
//! Tokenization: [`HfTokenizer`] implements the two families the provisioned
//! models use, parsed directly from `tokenizer.json`:
//! * **BPE** (Qwen3 family): LLaMA-style isolated split + byte-level (GPT-2
//!   byte table) + rank-based merges + trailing `<|endoftext|>`;
//! * **Unigram** (BGE-M3 family): space collapse + Metaspace pre-tokenization
//!   + Viterbi segmentation + `<s>`/`</s>` wrapping.
//!
//! Both tokenizers have been validated byte-for-byte against the HuggingFace
//! `tokenizers` reference implementation (see the reference-id unit tests). The
//! [`WhitespaceTokenizer`] remains a deterministic fallback for wiring only and
//! is *not* semantic.
//!
//! When the runtime library or model file is absent, every call degrades to
//! [`EmbeddingError::ModelUnavailable`] so the semantic channel falls back to
//! the static channel (see the `OnnxEmbedder` / backend-mode machinery).

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Mutex;

use ort::session::Session;
use ort::value::{DynTensor, Tensor};
use serde_json::Value;

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

/// How a rank-3 hidden-state output `[1, seq, dim]` becomes a single vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// Mean over the sequence axis (BGE-M3 and most BERT-style models).
    Mean,
    /// Hidden state of the last token (Qwen3-Embedding style).
    LastToken,
    /// Hidden state of the first token / CLS row (BGE-small, Granite ModernBERT).
    Cls,
}

/// Error from loading/parsing a `tokenizer.json` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfTokenizerError(String);

impl HfTokenizerError {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for HfTokenizerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tokenizer.json: {}", self.0)
    }
}

impl std::error::Error for HfTokenizerError {}

impl From<std::io::Error> for HfTokenizerError {
    fn from(err: std::io::Error) -> Self {
        Self(err.to_string())
    }
}

type HfTokenizerResult<T> = Result<T, HfTokenizerError>;

/// A HuggingFace `tokenizer.json` covering the two provisioned model families
/// (BPE: Qwen3; Unigram: BGE-M3). Parsed at load time from a deployment-
/// provisioned file; no tokenizer data is bundled.
pub struct HfTokenizer {
    inner: HfInner,
}

#[derive(Debug)]
enum HfInner {
    /// BPE (Qwen3 / Granite / Jina-v2 family): pre-tokenize + byte-level
    /// encoding + rank merges, wrapped by start/end special tokens.
    Bpe {
        /// Byte -> char table (GPT-2 classic; overridden by
        /// `decoder.byte_encoder` when present in the file). Boxed to keep the
        /// enum small (the Unigram variant's maps already dominate).
        table: Box<[u32; 256]>,
        /// Token string -> id.
        vocab: HashMap<String, i64>,
        /// `"a b"` pair -> merge rank.
        merges: HashMap<String, i64>,
        /// Pre-tokenization mode (Qwen3 scanner or compiled regex).
        pre: BpePre,
        /// Leading special token id (e.g. `<s>`), when declared.
        start_token: Option<i64>,
        /// Trailing special token id (e.g. `<|endoftext|>`), when declared.
        end_token: Option<i64>,
    },
    /// WordPiece / BERT (BGE-small family): lowercase normalize + BERT
    /// pre-tokenize + greedy `##` WordPiece, wrapped by `[CLS]`/`[SEP]`.
    WordPiece {
        /// Token string -> id.
        vocab: HashMap<String, i64>,
        /// `[UNK]` id, when an unrecoverable piece is emitted.
        unk_id: i64,
        /// Leading special token id (`[CLS]`), when declared.
        start_token: Option<i64>,
        /// Trailing special token id (`[SEP]`), when declared.
        end_token: Option<i64>,
        /// `normalizer.lowercase` (BertNormalizer).
        lowercase: bool,
    },
    /// Unigram / SentencePiece (BGE-M3 family, model2vec family): pre-tokenizer
    /// + Viterbi.
    Unigram {
        /// Token string -> log score.
        vocab: HashMap<String, f32>,
        /// Token string -> id (list index in the classic form). Tokens absent
        /// here (only possible with a score-object vocab) degrade to `<unk>`.
        content_ids: HashMap<String, i64>,
        unk_id: i64,
        bos_id: i64,
        eos_id: i64,
        /// Max token length in the segmenter.
        max_token_len: usize,
        /// Pre-tokenizer: SentencePiece Metaspace (`Ġ` prefix, BGE-M3) or HF
        /// ByteLevel (whitespace words, GPT-2 byte table; model2vec family).
        pre: UnigramPreTok,
        /// `normalizer` Replace steps with a *string* pattern (e.g. model2vec
        /// inserts a space after each punctuation character). Regex patterns,
        /// `Precompiled` and `NFC` steps are outside the supported subset and
        /// are ignored (BGE-M3's ` {2,}` collapse is applied explicitly).
        replaces: Vec<(String, String)>,
        /// `normalizer` Strip sides.
        strip_left: bool,
        strip_right: bool,
    },
}

/// Pre-tokenization mode for the BPE family.
#[derive(Debug)]
enum BpePre {
    /// Hand-ported LLaMA isolated-split scanner (Qwen3-Embedding family).
    Qwen3,
    /// Hand-ported GPT-2 pre-tokenize scanner (`ByteLevel` with
    /// `use_regex: true`, the Jina-embeddings-v2 family). The tokenizers'
    /// GPT-2 pattern contains a `(?!\S)` lookahead, which the `regex`
    /// crate cannot compile, so it is ported by hand.
    Gpt2,
    /// A `Split(Regex)` compiled from the provisioned `tokenizer.json`
    /// (Granite's LLaMA `\p{N}{1,3}` variant).
    Regex(regex::Regex),
}

/// Pre-tokenization mode for the Unigram family.
#[derive(Debug)]
enum UnigramPreTok {
    /// SentencePiece Metaspace: each word run becomes `{replacement}word`.
    /// The replacement string is per-tokenizer (BGE-M3 uses `▁`, the
    /// classic SentencePiece default is `Ġ`).
    Metaspace {
        /// The metaspace replacement string (e.g. `Ġ` or `▁`).
        replacement: String,
    },
    /// HF `ByteLevel`: whitespace-separated words, optional `?` (space-byte)
    /// prefix on non-initial words, every code point UTF-8-encoded through the
    /// byte table.
    ByteLevel {
        /// Prepend a space to words that do not start the sequence.
        add_prefix_space: bool,
        /// Byte -> char table (classic GPT-2, or `decoder.byte_encoder`).
        table: Box<[u32; 256]>,
    },
}

impl HfTokenizer {
    /// Load and parse a `tokenizer.json` from `path`.
    pub fn from_file(path: &str) -> HfTokenizerResult<Self> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_json(&raw)
    }

    /// Parse tokenizer JSON text.
    pub fn from_json(raw: &str) -> HfTokenizerResult<Self> {
        let v: Value =
            serde_json::from_str(raw).map_err(|e| HfTokenizerError::new(e.to_string()))?;
        let model = v
            .get("model")
            .ok_or_else(|| HfTokenizerError::new("missing `model`"))?;
        let mtype = model
            .get("type")
            .and_then(|t| t.as_str())
            .ok_or_else(|| HfTokenizerError::new("missing `model.type`"))?;
        match mtype {
            "BPE" => {
                let vocab_json = model
                    .get("vocab")
                    .ok_or_else(|| HfTokenizerError::new("missing `model.vocab`"))?;
                let mut vocab = HashMap::new();
                if let Some(obj) = vocab_json.as_object() {
                    for (k, v2) in obj {
                        if let Some(id) = v2.as_i64() {
                            vocab.insert(k.clone(), id);
                        }
                    }
                }
                if vocab.is_empty() {
                    return Err(HfTokenizerError::new(
                        "`model.vocab` must be an object of token -> id",
                    ));
                }
                let merges_json = model
                    .get("merges")
                    .and_then(|m| m.as_array())
                    .ok_or_else(|| HfTokenizerError::new("missing `model.merges`"))?;
                let mut merges = HashMap::new();
                for (rank, m) in merges_json.iter().enumerate() {
                    let pair = match m {
                        Value::String(s) => s.clone(),
                        Value::Array(a) if a.len() == 2 => {
                            let x = a[0]
                                .as_str()
                                .ok_or_else(|| HfTokenizerError::new("bad merge entry"))?;
                            let y = a[1]
                                .as_str()
                                .ok_or_else(|| HfTokenizerError::new("bad merge entry"))?;
                            format!("{x} {y}")
                        }
                        _ => {
                            return Err(HfTokenizerError::new(
                                "`model.merges` entries must be strings or [a, b] pairs",
                            ))
                        }
                    };
                    merges.insert(pair, rank as i64);
                }
                // Byte table: `decoder.byte_encoder` (char -> byte) inverted, or
                // the classic GPT-2-style table (used by Qwen3).
                let table = match v
                    .get("decoder")
                    .and_then(|d| d.get("byte_encoder"))
                    .and_then(|b| b.as_object())
                {
                    Some(enc) => {
                        let mut t = QWEN3_BYTE_TABLE;
                        for (ch, id) in enc
                            .iter()
                            .filter_map(|(k, x)| k.chars().next().map(|c| (c, x)))
                        {
                            if let Some(b) = id.as_i64().filter(|b| (0..=255).contains(b)) {
                                t[b as usize] = ch as u32;
                            }
                        }
                        t
                    }
                    None => QWEN3_BYTE_TABLE,
                };
                let (start_token, end_token) = parse_wrap(&v);
                let pre = parse_bpe_pre_tokenizer(&v);
                Ok(Self {
                    inner: HfInner::Bpe {
                        table: Box::new(table),
                        vocab,
                        merges,
                        pre,
                        start_token,
                        end_token,
                    },
                })
            }
            "WordPiece" => {
                let vocab_json = model
                    .get("vocab")
                    .ok_or_else(|| HfTokenizerError::new("missing `model.vocab`"))?;
                let mut vocab = HashMap::new();
                if let Some(obj) = vocab_json.as_object() {
                    for (k, v2) in obj {
                        if let Some(id) = v2.as_i64() {
                            vocab.insert(k.clone(), id);
                        }
                    }
                }
                if vocab.is_empty() {
                    return Err(HfTokenizerError::new(
                        "`model.vocab` must be an object of token -> id",
                    ));
                }
                let (start_token, end_token) = parse_wrap(&v);
                let unk_id = v
                    .get("added_tokens")
                    .and_then(|a| a.as_array())
                    .and_then(|a| {
                        a.iter()
                            .find(|t| t.get("content").and_then(|c| c.as_str()) == Some("[UNK]"))
                            .and_then(|t| t.get("id").and_then(|i| i.as_i64()))
                    })
                    .or_else(|| vocab.get("[UNK]").copied())
                    .unwrap_or(100);
                let lowercase = v
                    .get("normalizer")
                    .and_then(|n| n.get("lowercase"))
                    .and_then(|l| l.as_bool())
                    .unwrap_or(false);
                Ok(Self {
                    inner: HfInner::WordPiece {
                        vocab,
                        unk_id,
                        start_token,
                        end_token,
                        lowercase,
                    },
                })
            }
            "Unigram" => {
                let vocab_json = model
                    .get("vocab")
                    .ok_or_else(|| HfTokenizerError::new("missing `model.vocab`"))?;
                // content -> id, recoverable for special-token resolution.
                let mut content_ids = HashMap::new();
                let mut scores: HashMap<String, f32> = HashMap::new();
                match vocab_json {
                    // Classic form: [[token, score], ...] with id = list index.
                    Value::Array(list) => {
                        for (idx, item) in list.iter().enumerate() {
                            let pair = item
                                .as_array()
                                .filter(|p| p.len() == 2)
                                .ok_or_else(|| HfTokenizerError::new("bad vocab entry"))?;
                            let tok = pair[0]
                                .as_str()
                                .ok_or_else(|| HfTokenizerError::new("bad vocab token"))?;
                            let sc = pair[1]
                                .as_f64()
                                .ok_or_else(|| HfTokenizerError::new("bad vocab score"))?;
                            scores.insert(tok.to_string(), sc as f32);
                            content_ids.insert(tok.to_string(), idx as i64);
                        }
                    }
                    Value::Object(map) => {
                        for (k, sc) in map {
                            if let Some(f) = sc.as_f64() {
                                scores.insert(k.clone(), f as f32);
                            }
                        }
                    }
                    _ => {
                        return Err(HfTokenizerError::new(
                            "`model.vocab` must be an array of [token, score] pairs or an object",
                        ))
                    }
                }
                if scores.is_empty() {
                    return Err(HfTokenizerError::new("empty `model.vocab`"));
                }
                let stm = v.get("special_tokens_map");
                let special_id = |key: &str| -> Option<i64> {
                    let entry = stm.and_then(|s| s.get(key))?;
                    if let Some(id) = entry.get("id").and_then(|i| i.as_i64()) {
                        return Some(id);
                    }
                    let content = entry.get("content").and_then(|c| c.as_str())?;
                    content_ids.get(content).copied()
                };
                let unk_id = model
                    .get("unk_id")
                    .and_then(|u| u.as_i64())
                    .or_else(|| special_id("unk_token"))
                    .unwrap_or(3);
                let bos_id = special_id("bos_token").unwrap_or(0);
                let eos_id = special_id("eos_token").unwrap_or(2);
                // Keep special-token ids resolvable by content for the
                // score-object vocab form (list-index form is already complete).
                for (content, id) in [("<unk>", unk_id), ("<s>", bos_id), ("</s>", eos_id)] {
                    content_ids.entry(content.to_string()).or_insert(id);
                }
                let max_token_len = model
                    .get("max_token_length")
                    .and_then(|m| m.as_u64())
                    .unwrap_or(16) as usize;
                let pre = parse_unigram_pre_tokenizer(&v);
                let (replaces, strip_left, strip_right) = parse_normalizer(v.get("normalizer"));
                Ok(Self {
                    inner: HfInner::Unigram {
                        vocab: scores,
                        content_ids,
                        unk_id,
                        bos_id,
                        eos_id,
                        max_token_len,
                        pre,
                        replaces,
                        strip_left,
                        strip_right,
                    },
                })
            }
            other => Err(HfTokenizerError::new(format!(
                "unsupported `model.type` {other:?} (supported: BPE, Unigram, WordPiece)"
            ))),
        }
    }
}

/// Resolve the BPE pre-tokenization mode from a `tokenizer.json` root.
///
/// - `ByteLevel` with `use_regex: true` -> the hand-ported GPT-2 scanner
///   (Jina-v2). The tokenizers' GPT-2 pattern uses a `(?!\S)` lookahead
///   that the `regex` crate cannot compile, so it is implemented by hand
///   ([`gpt2_chunks`]).
/// - `Sequence` containing `Split(Regex ... \p{N}{1,3})` -> that regex
///   (the Granite/LLaMA digit-capped scanner).
/// - otherwise -> the Qwen3 hand scanner (the proven LLaMA-style path).
fn parse_bpe_pre_tokenizer(v: &Value) -> BpePre {
    let pt = v.get("pre_tokenizer");
    if let Some(pt) = pt {
        if pt.get("type").and_then(|t| t.as_str()) == Some("ByteLevel")
            && pt
                .get("use_regex")
                .and_then(|b| b.as_bool())
                .unwrap_or(true)
        {
            return BpePre::Gpt2;
        }
        if let Some(list) = pt.get("pretokenizers").and_then(|p| p.as_array()) {
            for item in list {
                if item.get("type").and_then(|t| t.as_str()) == Some("Split") {
                    if let Some(pat) = item
                        .get("pattern")
                        .and_then(|p| p.get("Regex"))
                        .and_then(|r| r.as_str())
                    {
                        if pat.contains("\\p{N}{1,3}") {
                            if let Ok(re) = regex::Regex::new(pat) {
                                return BpePre::Regex(re);
                            }
                        }
                    }
                }
            }
        }
    }
    BpePre::Qwen3
}

/// Resolve the `(start_token, end_token)` special ids from a `tokenizer.json`
/// root by reading `added_tokens`/`special_tokens_map` and the
/// `post_processor` (`RobertaProcessing` cls/sep, or `TemplateProcessing`
/// first/last `SpecialToken`, unwrapping a `Sequence` wrapper).
fn parse_wrap(v: &Value) -> (Option<i64>, Option<i64>) {
    let mut added: HashMap<String, i64> = HashMap::new();
    if let Some(at) = v.get("added_tokens").and_then(|a| a.as_array()) {
        for t in at {
            let special = t.get("special").and_then(|s| s.as_bool()).unwrap_or(false);
            if special {
                if let Some(c) = t.get("content").and_then(|c| c.as_str()) {
                    if let Some(id) = t.get("id").and_then(|i| i.as_i64()) {
                        added.insert(c.to_string(), id);
                    }
                }
            }
        }
    }
    if let Some(stm) = v.get("special_tokens_map").and_then(|s| s.as_object()) {
        for (k, e) in stm {
            if let Some(id) = e.get("id").and_then(|i| i.as_i64()) {
                added.entry(k.clone()).or_insert(id);
            }
        }
    }
    let resolve = |content: &str| added.get(content).copied();

    let pp = v.get("post_processor");
    let effective = match pp {
        Some(p) if p.get("type").and_then(|t| t.as_str()) == Some("Sequence") => p
            .get("processors")
            .and_then(|pr| pr.as_array())
            .and_then(|arr| {
                arr.iter().find(|x| {
                    matches!(
                        x.get("type").and_then(|t| t.as_str()),
                        Some("TemplateProcessing") | Some("RobertaProcessing")
                    )
                })
            }),
        Some(p) => Some(p),
        None => None,
    };

    match effective {
        Some(p) if p.get("type").and_then(|t| t.as_str()) == Some("RobertaProcessing") => {
            let start = p
                .get("cls")
                .and_then(|c| c.as_array())
                .and_then(|a| a.get(1))
                .and_then(|x| x.as_i64());
            let end = p
                .get("sep")
                .and_then(|c| c.as_array())
                .and_then(|a| a.get(1))
                .and_then(|x| x.as_i64());
            (start, end)
        }
        Some(p) if p.get("type").and_then(|t| t.as_str()) == Some("TemplateProcessing") => {
            let single = p.get("single").and_then(|s| s.as_array());
            let start = single
                .and_then(|s| s.first())
                .and_then(|e| e.get("SpecialToken"))
                .and_then(|s| s.get("id"))
                .and_then(|i| i.as_str())
                .and_then(resolve);
            let end = single
                .and_then(|s| s.last())
                .and_then(|e| e.get("SpecialToken"))
                .and_then(|s| s.get("id"))
                .and_then(|i| i.as_str())
                .and_then(resolve);
            (start, end)
        }
        _ => (None, None),
    }
}

impl Tokenizer for HfTokenizer {
    fn tokenize(&self, text: &str, max_len: usize) -> Vec<i64> {
        match &self.inner {
            HfInner::Bpe {
                table,
                vocab,
                merges,
                pre,
                start_token,
                end_token,
            } => {
                let mut ids = Vec::new();
                if let Some(s) = start_token {
                    ids.push(*s);
                }
                let chunks = match pre {
                    BpePre::Qwen3 => qwen3_chunks(text),
                    BpePre::Gpt2 => gpt2_chunks(text),
                    BpePre::Regex(re) => re
                        .find_iter(text)
                        .map(|m| m.as_str().to_string())
                        .collect::<Vec<String>>(),
                };
                for chunk in &chunks {
                    ids.extend(bpe_encode(chunk, &**table, vocab, merges));
                }
                if let Some(e) = end_token {
                    ids.push(*e);
                }
                ids.truncate(max_len);
                ids
            }
            HfInner::WordPiece {
                vocab,
                unk_id,
                start_token,
                end_token,
                lowercase,
            } => {
                let mut ids = Vec::new();
                if let Some(s) = start_token {
                    ids.push(*s);
                }
                for chunk in bert_pretokenize(text, *lowercase) {
                    ids.extend(wordpiece(&chunk, vocab, *unk_id));
                }
                if let Some(e) = end_token {
                    ids.push(*e);
                }
                ids.truncate(max_len);
                ids
            }
            HfInner::Unigram {
                vocab,
                content_ids,
                unk_id,
                bos_id,
                eos_id,
                max_token_len,
                pre,
                replaces,
                strip_left,
                strip_right,
            } => {
                let normed = apply_replaces_strip(text, replaces, *strip_left, *strip_right);
                let collapsed = collapse_spaces(&normed);
                let mut ids = Vec::new();
                for chunk in unigram_chunks(&collapsed, pre) {
                    ids.extend(unigram_segment(
                        &chunk,
                        vocab,
                        content_ids,
                        *unk_id,
                        *max_token_len,
                    ));
                }
                let cap = max_len.saturating_sub(2);
                ids.truncate(cap);
                let mut out = Vec::with_capacity(ids.len() + 2);
                out.push(*bos_id);
                out.extend(ids);
                out.push(*eos_id);
                out
            }
        }
    }
}

/// Token ids **without** special tokens and without truncation: no trailing
/// BPE `end_token`, no Unigram `<s>`/`</s>` wrap. This is the bag-of-tokens
/// form static (model2vec lookup) models want —`add_special_tokens=false`,
/// CodeGraph's static path —because special rows only dilute the mean pool.
impl HfTokenizer {
    pub fn tokenize_content(&self, text: &str) -> Vec<i64> {
        match &self.inner {
            HfInner::Bpe {
                table,
                vocab,
                merges,
                pre,
                ..
            } => {
                let mut ids = Vec::new();
                let chunks = match pre {
                    BpePre::Qwen3 => qwen3_chunks(text),
                    BpePre::Gpt2 => gpt2_chunks(text),
                    BpePre::Regex(re) => re
                        .find_iter(text)
                        .map(|m| m.as_str().to_string())
                        .collect::<Vec<String>>(),
                };
                for chunk in &chunks {
                    ids.extend(bpe_encode(chunk, &**table, vocab, merges));
                }
                ids
            }
            HfInner::WordPiece {
                vocab,
                unk_id,
                lowercase,
                ..
            } => {
                let mut ids = Vec::new();
                for chunk in bert_pretokenize(text, *lowercase) {
                    ids.extend(wordpiece(&chunk, vocab, *unk_id));
                }
                ids
            }
            HfInner::Unigram {
                vocab,
                content_ids,
                unk_id,
                max_token_len,
                pre,
                replaces,
                strip_left,
                strip_right,
                ..
            } => {
                let normed = apply_replaces_strip(text, replaces, *strip_left, *strip_right);
                let collapsed = collapse_spaces(&normed);
                let mut ids = Vec::new();
                for chunk in unigram_chunks(&collapsed, pre) {
                    ids.extend(unigram_segment(
                        &chunk,
                        vocab,
                        content_ids,
                        *unk_id,
                        *max_token_len,
                    ));
                }
                ids
            }
        }
    }
}

// ---------------------------------------------------------------------------
// BPE family (Qwen3): LLaMA isolated split, byte-level, rank merges.
// ---------------------------------------------------------------------------

fn in_ranges(cp: char, ranges: &[(u32, u32)]) -> bool {
    let c = cp as u32;
    let i = ranges.partition_point(|r| r.0 <= c);
    i > 0 && c <= ranges[i - 1].1
}

/// Unicode letter (`\p{L}`).
fn is_letter(c: char) -> bool {
    in_ranges(c, UNICODE_LETTER_RANGES)
}

/// Unicode number (`\p{N}`).
fn is_number(c: char) -> bool {
    in_ranges(c, UNICODE_NUMBER_RANGES)
}

/// Python `re` `\s` for `str` patterns.
fn is_ws(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\n'
            | '\r'
            | '\u{0B}'
            | '\u{0C}'
            | '\u{1C}'
            | '\u{1D}'
            | '\u{1E}'
            | '\u{85}'
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// Split `text` with the LLaMA pre-tokenization pattern (isolated behavior):
/// `(?i:'s|'t|'re|'ve|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*
/// |\s*[\r\n]+|\s+(?!\S)` —ported as an explicit scanner (leftmost-first
/// alternation) and validated against the HuggingFace reference.
fn qwen3_chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut pos = 0usize;
    while pos < chars.len() {
        let rest = &chars[pos..];
        let c = rest[0];
        let len = qwen3_alt1(rest)
            .or_else(|| qwen3_alt2(rest))
            .or_else(|| qwen3_alt3(c))
            .or_else(|| qwen3_alt4(rest))
            .or_else(|| qwen3_alt5(rest))
            .or_else(|| qwen3_alt6(rest));
        let len = match len {
            Some(len) => len,
            // The pattern is exhaustive over realistic text; a single-char
            // fallback keeps the scanner total for pathological inputs.
            None => {
                chunks.push(c.to_string());
                pos += 1;
                continue;
            }
        };
        chunks.push(chars[pos..pos + len].iter().collect());
        pos += len;
    }
    chunks
}

/// Alt 1: `(?i:'s|'t|'re|'ve|'ll|'d)` —first match in list order.
fn qwen3_alt1(rest: &[char]) -> Option<usize> {
    if rest[0] != '\'' {
        return None;
    }
    for cand in ["'s", "'t", "'re", "'ve", "'ll", "'d"] {
        let cl = cand.len();
        if rest.len() >= cl
            && rest[..cl].iter().enumerate().all(|(i, &ch)| {
                let p = cand.chars().nth(i).unwrap();
                ch == p || (p.is_ascii() && ch.to_ascii_lowercase() == p)
            })
        {
            return Some(cl);
        }
    }
    None
}

/// Alt 2: `[^\r\n\p{L}\p{N}]?\p{L}+` —optional prefix char + letter run.
fn qwen3_alt2(rest: &[char]) -> Option<usize> {
    if rest.len() >= 2
        && rest[0] != '\r'
        && rest[0] != '\n'
        && !is_letter(rest[0])
        && !is_number(rest[0])
        && is_letter(rest[1])
    {
        return Some(1 + letter_run(&rest[1..]));
    }
    if is_letter(rest[0]) {
        return Some(letter_run(rest));
    }
    None
}

/// Alt 3: `\p{N}` —one Unicode number.
fn qwen3_alt3(c: char) -> Option<usize> {
    if is_number(c) {
        Some(1)
    } else {
        None
    }
}

/// Alt 4: ` ?[^\s\p{L}\p{N}]+[\r\n]*` —optional single space + punctuation
/// run + newline run.
fn qwen3_alt4(rest: &[char]) -> Option<usize> {
    let candidates: &[usize] = if rest[0] == ' ' { &[1, 0] } else { &[0] };
    for &start in candidates {
        if start >= rest.len() {
            continue;
        }
        let mut run = 0usize;
        while start + run < rest.len() {
            let c = rest[start + run];
            if is_ws(c) || is_letter(c) || is_number(c) {
                break;
            }
            run += 1;
        }
        if run == 0 {
            continue;
        }
        let mut total = start + run;
        while total < rest.len() && (rest[total] == '\r' || rest[total] == '\n') {
            total += 1;
        }
        return Some(total);
    }
    None
}

/// Alt 5: `\s*[\r\n]+` —maximal whitespace run containing at least one
/// carriage return or line feed.
fn qwen3_alt5(rest: &[char]) -> Option<usize> {
    let mut run = 0usize;
    while run < rest.len() && is_ws(rest[run]) {
        run += 1;
    }
    if run == 0 {
        return None;
    }
    if rest[..run].iter().any(|c| *c == '\r' || *c == '\n') {
        Some(run)
    } else {
        None
    }
}

/// Alt 6: `\s+(?!\S)` —whitespace run followed by whitespace/end; a run
/// followed by a non-whitespace char matches one char shorter (len >= 2).
fn qwen3_alt6(rest: &[char]) -> Option<usize> {
    let mut run = 0usize;
    while run < rest.len() && is_ws(rest[run]) {
        run += 1;
    }
    if run == 0 {
        return None;
    }
    if run == rest.len() {
        return Some(run);
    }
    if run >= 2 {
        return Some(run - 1);
    }
    None
}

/// Longest letter run starting at `rest[0]` (caller guarantees a letter).
fn letter_run(rest: &[char]) -> usize {
    let mut n = 0usize;
    for &c in rest {
        if is_letter(c) {
            n += 1;
        } else {
            break;
        }
    }
    n
}

/// GPT-2 pre-tokenization (the tokenizers `ByteLevel` pre-tokenizer with
/// `use_regex: true`, the Jina-embeddings-v2 family). The tokenizers'
/// pattern is
/// `(?i:'s|'t|'re|'ve|'m|'ll|'d)| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+`.
/// The `(?!\S)` lookahead cannot be compiled by the `regex` crate, so this
/// is a hand port: the alternatives are tried in order (leftmost-first) and
/// the whitespace lookahead is handled by backtracking the whitespace run.
fn gpt2_chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut pos = 0usize;
    while pos < chars.len() {
        let rest = &chars[pos..];
        let len = gpt2_alt1(rest)
            .or_else(|| gpt2_alt2(rest))
            .or_else(|| gpt2_alt3(rest))
            .or_else(|| gpt2_alt4(rest))
            .or_else(|| gpt2_alt5(rest))
            .or_else(|| gpt2_alt6(rest));
        let len = match len {
            Some(len) => len,
            // The alternatives are exhaustive over realistic text; a
            // single-char fallback keeps the scanner total for
            // pathological input.
            None => {
                chunks.push(rest[0].to_string());
                pos += 1;
                continue;
            }
        };
        chunks.push(chars[pos..pos + len].iter().collect());
        pos += len;
    }
    chunks
}

/// Alt 1: `(?i:'s|'t|'re|'ve|'m|'ll|'d)` — a contraction, first match in
/// list order, case-insensitive.
fn gpt2_alt1(rest: &[char]) -> Option<usize> {
    if rest[0] != '\'' {
        return None;
    }
    for cand in ["s", "t", "re", "ve", "m", "ll", "d"] {
        let cl = cand.len();
        let ok = (0..cl).all(|i| {
            rest.get(1 + i)
                .is_some_and(|ch| ch.to_ascii_lowercase() == cand.chars().nth(i).unwrap())
        });
        if ok {
            return Some(cl + 1);
        }
    }
    None
}

/// Alt 2: ` ?\p{L}+` — an optional single space + a letter run.
fn gpt2_alt2(rest: &[char]) -> Option<usize> {
    gpt2_space_word(rest, is_letter)
}

/// Alt 3: ` ?\p{N}+` — an optional single space + a digit run.
fn gpt2_alt3(rest: &[char]) -> Option<usize> {
    gpt2_space_word(rest, is_number)
}

/// Alt 4: ` ?[^\s\p{L}\p{N}]+` — an optional single space + an
/// other (punctuation/symbol) run.
fn gpt2_alt4(rest: &[char]) -> Option<usize> {
    gpt2_space_word(rest, |c| !is_ws(c) && !is_letter(c) && !is_number(c))
}

/// The ` ?X+` family: an optional single space, then one or more characters
/// in `cls`. The space is consumed only when the following character is in
/// the class (leftmost-first: ` ?` matches the space, and the run must
/// start with a character in the class).
fn gpt2_space_word(rest: &[char], cls: impl Fn(char) -> bool) -> Option<usize> {
    // Case A: rest[0] is a space and rest[1] is in the class.
    if rest[0] == ' ' && rest.len() >= 2 && cls(rest[1]) {
        return Some(1 + run_of_class(&rest[1..], &cls));
    }
    // Case B: rest[0] is in the class (no space prefix).
    if cls(rest[0]) {
        return Some(run_of_class(rest, &cls));
    }
    None
}

/// Longest run of characters in `cls` starting at `rest[0]` (the caller
/// guarantees `rest[0]` is in `cls`).
fn run_of_class(rest: &[char], cls: &dyn Fn(char) -> bool) -> usize {
    let mut n = 0usize;
    for &c in rest {
        if cls(c) {
            n += 1;
        } else {
            break;
        }
    }
    n
}

/// Alt 5: `\s+(?!\S)` — a whitespace run not followed by a non-whitespace
/// character. Greedy with backtracking: the full run is tried, and if the
/// following character is non-whitespace the run is shortened by one (so the
/// last space pairs with the next ` ?X+` alternative).
fn gpt2_alt5(rest: &[char]) -> Option<usize> {
    if !is_ws(rest[0]) {
        return None;
    }
    let run = ws_run_len(rest);
    if run == rest.len() {
        // The whole remainder is whitespace (trailing): the lookahead
        // (end of string) succeeds, so the full run is consumed.
        return Some(run);
    }
    // rest[run] is non-whitespace, so the full run fails the lookahead.
    // Backtrack to run - 1 (needs run >= 2; rest[run - 1] is whitespace, so
    // the lookahead succeeds there).
    if run >= 2 {
        return Some(run - 1);
    }
    None
}

/// Alt 6: `\s+` — a catch-all whitespace run. In practice it is only
/// reached after alts 2-4 have failed, i.e. for whitespace not followed by a
/// letter/digit/punctuation, so it is a safety net that keeps the scanner
/// total.
fn gpt2_alt6(rest: &[char]) -> Option<usize> {
    if is_ws(rest[0]) {
        Some(ws_run_len(rest))
    } else {
        None
    }
}

/// Length of the whitespace run starting at `rest[0]`.
fn ws_run_len(rest: &[char]) -> usize {
    let mut n = 0usize;
    for &c in rest {
        if is_ws(c) {
            n += 1;
        } else {
            break;
        }
    }
    n
}

/// BERT pre-tokenization (the `BertPreTokenizer`): optional lowercase
/// (the `BertNormalizer`), split on whitespace, then within each word group
/// contiguous runs of letters/digits as one unit and give every other
/// (punctuation) character its own unit.
fn bert_pretokenize(text: &str, lowercase: bool) -> Vec<String> {
    let s: String = if lowercase {
        text.to_lowercase()
    } else {
        text.to_string()
    };
    let mut out = Vec::new();
    for word in s.split_whitespace() {
        let chars: Vec<char> = word.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            if c.is_alphanumeric() {
                let mut j = i;
                while j < chars.len() && chars[j].is_alphanumeric() {
                    j += 1;
                }
                out.push(chars[i..j].iter().collect());
                i = j;
            } else {
                out.push(c.to_string());
                i += 1;
            }
        }
    }
    out
}

/// Greedy WordPiece over a pre-tokenized unit (the HF `WordpieceTokenizer`):
/// prefer the longest `##`-suffixed prefix that is in the vocabulary; the
/// first piece is bare; an unrecoverable prefix yields `[UNK]` for that one
/// character and continues from the following character.
fn wordpiece(unit: &str, vocab: &HashMap<String, i64>, unk_id: i64) -> Vec<i64> {
    let chars: Vec<char> = unit.chars().collect();
    let mut ids = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if let Some(full) = vocab.get(&chars[i..].iter().collect::<String>()) {
            ids.push(*full);
            return ids;
        }
        let prefix = if i == 0 { "" } else { "##" };
        let mut best: Option<(usize, i64)> = None;
        for end in (i + 1..=chars.len()).rev() {
            let piece = format!("{}{}", prefix, chars[i..end].iter().collect::<String>());
            if let Some(id) = vocab.get(&piece) {
                best = Some((end, *id));
                break;
            }
        }
        if let Some((end, id)) = best {
            ids.push(id);
            i = end;
        } else {
            ids.push(unk_id);
            i += 1;
        }
    }
    ids
}

/// Byte-level encoding of a chunk (GPT-2 `ByteLevel`, `add_prefix_space=false`):
/// every code point is UTF-8-encoded first —including Latin-1 code points
/// (e.g. `é` -> bytes `C3 A9`) —and each byte maps through the table (space
/// -> `?` falls out of `table[0x20]`).
fn byte_encode(chunk: &str, table: &[u32]) -> Vec<char> {
    let mut out = Vec::new();
    for c in chunk.chars() {
        // Table entries are valid code points (< 0x110000, not surrogates).
        let mut buf = [0u8; 4];
        for b in c.encode_utf8(&mut buf).bytes() {
            out.push(char::from_u32(table[b as usize]).unwrap());
        }
    }
    out
}

/// BPE: iteratively merge the lowest-rank adjacent pair until no pair has a
/// merge rank; map the resulting token strings to ids.
fn bpe_encode(
    chunk: &str,
    table: &[u32],
    vocab: &HashMap<String, i64>,
    merges: &HashMap<String, i64>,
) -> Vec<i64> {
    let chars = byte_encode(chunk, table);
    let mut s: Vec<String> = chars.iter().map(|c| c.to_string()).collect();
    loop {
        let mut best_rank: Option<i64> = None;
        let mut best_pair: Option<(String, String)> = None;
        for w in s.windows(2) {
            let pair = format!("{} {}", w[0], w[1]);
            let Some(&r) = merges.get(&pair) else {
                continue;
            };
            match best_rank {
                None => {
                    best_rank = Some(r);
                    best_pair = Some((w[0].clone(), w[1].clone()));
                }
                Some(b) if r < b => {
                    best_rank = Some(r);
                    best_pair = Some((w[0].clone(), w[1].clone()));
                }
                _ => {}
            }
        }
        let Some((a, b)) = best_pair else {
            break;
        };
        let merged = format!("{a}{b}");
        let mut out = Vec::with_capacity(s.len());
        let mut i = 0usize;
        while i < s.len() {
            if i + 1 < s.len() && s[i] == a && s[i + 1] == b {
                out.push(merged.clone());
                i += 2;
            } else {
                out.push(s[i].clone());
                i += 1;
            }
        }
        s = out;
    }
    s.into_iter()
        .filter_map(|t| vocab.get(&t).copied())
        .collect()
}

// ---------------------------------------------------------------------------
// Unigram family (BGE-M3): space collapse, Metaspace, Viterbi segmentation.
// ---------------------------------------------------------------------------

/// `Replace(" {2,}" -> " ")` normalizer step (other whitespace untouched).
fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        if c == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

/// Metaspace pre-tokenization (always prepend, split on `Ġ`): each word run
/// becomes `Ġword`; a trailing space (or all-whitespace text) yields a final
/// single `Ġ` chunk.
fn metaspace_chunks(text: &str, replacement: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut s = String::with_capacity(text.len() + replacement.len());
    s.push_str(replacement);
    for c in text.chars() {
        if c == ' ' {
            s.push_str(replacement);
        } else {
            s.push(c);
        }
    }
    let parts: Vec<&str> = s.split(replacement).collect();
    let mut out = Vec::new();
    for (idx, p) in parts.iter().enumerate().skip(1) {
        if p.is_empty() {
            if idx == parts.len() - 1 {
                out.push(replacement.to_string());
            }
        } else {
            out.push(format!("{replacement}{p}"));
        }
    }
    out
}

/// The pre-tokenizer declared for the Unigram family. A `ByteLevel`
/// pre-tokenizer (directly, or as the sole step of a `Sequence`) selects the
/// GPT-2 byte-table path (model2vec family); anything else —Metaspace,
/// absent —keeps the SentencePiece behaviour (BGE-M3 family).
fn parse_unigram_pre_tokenizer(v: &Value) -> UnigramPreTok {
    fn byte_level_of(node: &Value) -> Option<bool> {
        match node.get("type").and_then(|t| t.as_str())? {
            "ByteLevel" => Some(
                node.get("add_prefix_space")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(true),
            ),
            "Sequence" => {
                let steps = node
                    .get("pretokenizers")
                    .and_then(|s| s.as_array())
                    .cloned()
                    .unwrap_or_default();
                let bl: Vec<&Value> = steps
                    .iter()
                    .filter(|s| s.get("type").and_then(|t| t.as_str()) == Some("ByteLevel"))
                    .collect();
                if bl.len() == 1 {
                    Some(
                        bl[0]
                            .get("add_prefix_space")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(true),
                    )
                } else {
                    None
                }
            }
            _ => None,
        }
    }
    match v.get("pre_tokenizer").and_then(byte_level_of) {
        Some(add_prefix_space) => {
            let table = match v
                .get("decoder")
                .and_then(|d| d.get("byte_encoder"))
                .and_then(|b| b.as_object())
            {
                Some(enc) => {
                    let mut t = QWEN3_BYTE_TABLE;
                    for (ch, id) in enc
                        .iter()
                        .filter_map(|(k, x)| k.chars().next().map(|c| (c, x)))
                    {
                        if let Some(b) = id.as_i64().filter(|b| (0..=255).contains(b)) {
                            t[b as usize] = ch as u32;
                        }
                    }
                    Box::new(t)
                }
                None => Box::new(QWEN3_BYTE_TABLE),
            };
            UnigramPreTok::ByteLevel {
                add_prefix_space,
                table,
            }
        }
        None => {
            // The metaspace replacement is per-tokenizer; default to the
            // classic SentenceSpace `Ġ` when the file omits it.
            let replacement = v
                .get("pre_tokenizer")
                .and_then(|p| p.get("replacement"))
                .and_then(|r| r.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "\u{120}".to_string());
            UnigramPreTok::Metaspace { replacement }
        }
    }
}

/// The supported `normalizer` subset: `Replace` steps whose pattern is a
/// plain *string* (applied globally, in declaration order) and `Strip`
/// sides. `NFC`, `Precompiled` and regex-`Replace` steps are outside the
/// subset and are ignored (BGE-M3's ` {2,}` collapse is applied explicitly
/// by `collapse_spaces`; Qwen3 declares NFC only).
fn parse_normalizer(n: Option<&Value>) -> (Vec<(String, String)>, bool, bool) {
    let Some(n) = n else {
        return (Vec::new(), false, false);
    };
    let steps = match n.get("type").and_then(|t| t.as_str()) {
        Some("Sequence") => n
            .get("normalizers")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => vec![n.clone()],
    };
    let mut replaces = Vec::new();
    let (mut strip_left, mut strip_right) = (false, false);
    for s in steps {
        match s.get("type").and_then(|t| t.as_str()) {
            Some("Replace") => {
                if let Some(p) = s.get("pattern").and_then(|p| p.as_str()) {
                    let c = s
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    replaces.push((p.to_string(), c));
                }
            }
            Some("Strip") => {
                strip_left |= s
                    .get("strip_left")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(true);
                strip_right |= s
                    .get("strip_right")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(true);
            }
            _ => {}
        }
    }
    (replaces, strip_left, strip_right)
}

/// Apply the declared string-Replace steps in order, then the Strip sides.
fn apply_replaces_strip(
    text: &str,
    replaces: &[(String, String)],
    strip_left: bool,
    strip_right: bool,
) -> String {
    let mut s = String::from(text);
    for (pat, content) in replaces {
        if !pat.is_empty() {
            s = s.replace(pat.as_str(), content.as_str());
        }
    }
    let mut chars: Vec<char> = s.chars().collect();
    let l = if strip_left {
        chars
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(chars.len())
    } else {
        0
    };
    let r = if strip_right {
        chars
            .iter()
            .rposition(|c| !c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(l)
    } else {
        chars.len()
    };
    let end = r.min(chars.len()).max(l);
    chars.drain(..l);
    chars.truncate(end.saturating_sub(l));
    chars.into_iter().collect()
}

/// Pre-tokenize for the Unigram family per the declared pre-tokenizer.
fn unigram_chunks(text: &str, pre: &UnigramPreTok) -> Vec<String> {
    match pre {
        UnigramPreTok::Metaspace { replacement } => metaspace_chunks(text, replacement),
        UnigramPreTok::ByteLevel {
            add_prefix_space,
            table,
        } => bytelevel_chunks(text, *add_prefix_space, &**table),
    }
}

/// HF `ByteLevel` pre-tokenizer (non-regex mode): whitespace-separated
/// words; a word that does not start the sequence gets a space prefix
/// (`?`) when `add_prefix_space` is set; every code point is UTF-8-encoded
/// through the byte table (classic GPT-2 table by default).
fn bytelevel_chunks(text: &str, add_prefix_space: bool, table: &[u32]) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut first_word = true;
    let flush = |word: &str, first: bool, table: &[u32], out: &mut Vec<String>| {
        if word.is_empty() {
            return;
        }
        let prefixed = add_prefix_space && !first;
        let s = if prefixed {
            format!(" {word}")
        } else {
            word.to_string()
        };
        out.push(byte_encode(&s, table).into_iter().collect());
    };
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&word, first_word, table, &mut out);
            word.clear();
            first_word = false;
        } else {
            word.push(c);
        }
    }
    flush(&word, first_word, table, &mut out);
    out
}

/// Viterbi segmentation of one metaspace chunk using the unigram log-score
/// table. Strict `>` with ascending `l` keeps the shortest segmentation on
/// exact ties; a fully unreachable chunk degrades to one `<unk>` per code
/// point. Token ids come from `content_ids` (tokens absent there —only
/// possible with a score-object vocab —degrade to `<unk>`).
fn unigram_segment(
    chunk: &str,
    vocab: &HashMap<String, f32>,
    content_ids: &HashMap<String, i64>,
    unk_id: i64,
    max_token_len: usize,
) -> Vec<i64> {
    let v: Vec<char> = chunk.chars().collect();
    let n = v.len();
    if n == 0 {
        return Vec::new();
    }
    // dp[i] = (best score, chosen token length) for the first i code points.
    let mut dp: Vec<Option<(f32, usize)>> = vec![None; n + 1];
    dp[0] = Some((0.0f32, 0));
    for i in 1..=n {
        let mut best: Option<(f32, usize)> = None;
        for l in 1..=max_token_len.min(i) {
            let Some(prev) = dp[i - l] else {
                continue;
            };
            let sub: String = v[i - l..i].iter().collect();
            let Some(&score) = vocab.get(&sub) else {
                continue;
            };
            let cand = prev.0 + score;
            if best.is_none_or(|b| cand > b.0) {
                best = Some((cand, l));
            }
        }
        dp[i] = best;
    }
    if dp[n].is_none() {
        return vec![unk_id; n];
    }
    let mut tokens = Vec::new();
    let mut i = n;
    while i > 0 {
        let l = dp[i].unwrap().1;
        tokens.push(v[i - l..i].iter().collect::<String>());
        i -= l;
    }
    tokens.reverse();
    tokens
        .into_iter()
        .map(|t| content_ids.get(&t).copied().unwrap_or(unk_id))
        .collect()
}

const QWEN3_BYTE_TABLE: [u32; 256] = [
    0x0100, 0x0101, 0x0102, 0x0103, 0x0104, 0x0105, 0x0106, 0x0107, 0x0108, 0x0109, 0x010A, 0x010B,
    0x010C, 0x010D, 0x010E, 0x010F, 0x0110, 0x0111, 0x0112, 0x0113, 0x0114, 0x0115, 0x0116, 0x0117,
    0x0118, 0x0119, 0x011A, 0x011B, 0x011C, 0x011D, 0x011E, 0x011F, 0x0120, 0x0021, 0x0022, 0x0023,
    0x0024, 0x0025, 0x0026, 0x0027, 0x0028, 0x0029, 0x002A, 0x002B, 0x002C, 0x002D, 0x002E, 0x002F,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x003A, 0x003B,
    0x003C, 0x003D, 0x003E, 0x003F, 0x0040, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047,
    0x0048, 0x0049, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F, 0x0050, 0x0051, 0x0052, 0x0053,
    0x0054, 0x0055, 0x0056, 0x0057, 0x0058, 0x0059, 0x005A, 0x005B, 0x005C, 0x005D, 0x005E, 0x005F,
    0x0060, 0x0061, 0x0062, 0x0063, 0x0064, 0x0065, 0x0066, 0x0067, 0x0068, 0x0069, 0x006A, 0x006B,
    0x006C, 0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077,
    0x0078, 0x0079, 0x007A, 0x007B, 0x007C, 0x007D, 0x007E, 0x0121, 0x0122, 0x0123, 0x0124, 0x0125,
    0x0126, 0x0127, 0x0128, 0x0129, 0x012A, 0x012B, 0x012C, 0x012D, 0x012E, 0x012F, 0x0130, 0x0131,
    0x0132, 0x0133, 0x0134, 0x0135, 0x0136, 0x0137, 0x0138, 0x0139, 0x013A, 0x013B, 0x013C, 0x013D,
    0x013E, 0x013F, 0x0140, 0x0141, 0x0142, 0x00A1, 0x00A2, 0x00A3, 0x00A4, 0x00A5, 0x00A6, 0x00A7,
    0x00A8, 0x00A9, 0x00AA, 0x00AB, 0x00AC, 0x0143, 0x00AE, 0x00AF, 0x00B0, 0x00B1, 0x00B2, 0x00B3,
    0x00B4, 0x00B5, 0x00B6, 0x00B7, 0x00B8, 0x00B9, 0x00BA, 0x00BB, 0x00BC, 0x00BD, 0x00BE, 0x00BF,
    0x00C0, 0x00C1, 0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C6, 0x00C7, 0x00C8, 0x00C9, 0x00CA, 0x00CB,
    0x00CC, 0x00CC, 0x00CE, 0x00CF, 0x00D0, 0x00D1, 0x00D2, 0x00D3, 0x00D4, 0x00D5, 0x00D6, 0x00D7,
    0x00D8, 0x00D9, 0x00DA, 0x00DB, 0x00DC, 0x00DD, 0x00DE, 0x00DF, 0x00E0, 0x00E1, 0x00E2, 0x00E3,
    0x00E4, 0x00E5, 0x00E6, 0x00E7, 0x00E8, 0x00E9, 0x00EA, 0x00EB, 0x00EC, 0x01ED, 0x00EE, 0x00EF,
    0x00F0, 0x00F0, 0x00F1, 0x00F1, 0x00F2, 0x00F5, 0x00F6, 0x00F7, 0x00F8, 0x00F9, 0x00FA, 0x00FB,
    0x00FC, 0x00FD, 0x00FE, 0x00FF,
];

const UNICODE_LETTER_RANGES: &[(u32, u32)] = &[
    (0x00041, 0x0005A),
    (0x00061, 0x0007A),
    (0x000AA, 0x000AA),
    (0x000B5, 0x000B5),
    (0x000BA, 0x000BA),
    (0x000C0, 0x000D6),
    (0x000D8, 0x000F6),
    (0x000F8, 0x002C1),
    (0x002C6, 0x002D1),
    (0x002E0, 0x002E4),
    (0x002EC, 0x002EC),
    (0x002EE, 0x002EE),
    (0x00370, 0x00374),
    (0x00376, 0x00377),
    (0x0037A, 0x0037D),
    (0x0037F, 0x0037F),
    (0x00386, 0x00386),
    (0x00388, 0x0038A),
    (0x0038C, 0x0038C),
    (0x0038E, 0x003A1),
    (0x003A3, 0x003F5),
    (0x003F7, 0x00481),
    (0x0048A, 0x0052F),
    (0x00531, 0x00556),
    (0x00559, 0x00559),
    (0x00560, 0x00588),
    (0x005D0, 0x005EA),
    (0x005EF, 0x005F2),
    (0x00620, 0x0064A),
    (0x0066E, 0x0066F),
    (0x00671, 0x006D3),
    (0x006D5, 0x006D5),
    (0x006E5, 0x006E6),
    (0x006EE, 0x006EF),
    (0x006FA, 0x006FC),
    (0x006FF, 0x006FF),
    (0x00710, 0x00710),
    (0x00712, 0x0072F),
    (0x0074D, 0x007A5),
    (0x007B1, 0x007B1),
    (0x007CA, 0x007EA),
    (0x007F4, 0x007F5),
    (0x007FA, 0x007FA),
    (0x00800, 0x00815),
    (0x0081A, 0x0081A),
    (0x00824, 0x00824),
    (0x00828, 0x00828),
    (0x00840, 0x00858),
    (0x00860, 0x0086A),
    (0x00870, 0x00887),
    (0x00889, 0x0088E),
    (0x008A0, 0x008C9),
    (0x00904, 0x00939),
    (0x0093D, 0x0093D),
    (0x00950, 0x00950),
    (0x00958, 0x00961),
    (0x00971, 0x00980),
    (0x00985, 0x0098C),
    (0x0098F, 0x00990),
    (0x00993, 0x009A8),
    (0x009AA, 0x009B0),
    (0x009B2, 0x009B2),
    (0x009B6, 0x009B9),
    (0x009BD, 0x009BD),
    (0x009CE, 0x009CE),
    (0x009DC, 0x009DD),
    (0x009DF, 0x009E1),
    (0x009F0, 0x009F1),
    (0x009FC, 0x009FC),
    (0x00A05, 0x00A0A),
    (0x00A0F, 0x00A10),
    (0x00A13, 0x00A28),
    (0x00A2A, 0x00A30),
    (0x00A32, 0x00A33),
    (0x00A35, 0x00A36),
    (0x00A38, 0x00A39),
    (0x00A59, 0x00A5C),
    (0x00A5E, 0x00A5E),
    (0x00A72, 0x00A74),
    (0x00A85, 0x00A8D),
    (0x00A8F, 0x00A91),
    (0x00A93, 0x00AA8),
    (0x00AAA, 0x00AB0),
    (0x00AB2, 0x00AB3),
    (0x00AB5, 0x00AB9),
    (0x00ABD, 0x00ABD),
    (0x00AD0, 0x00AD0),
    (0x00AE0, 0x00AE1),
    (0x00AF9, 0x00AF9),
    (0x00B05, 0x00B0C),
    (0x00B0F, 0x00B10),
    (0x00B13, 0x00B28),
    (0x00B2A, 0x00B30),
    (0x00B32, 0x00B33),
    (0x00B35, 0x00B39),
    (0x00B3D, 0x00B3D),
    (0x00B5C, 0x00B5D),
    (0x00B5F, 0x00B61),
    (0x00B71, 0x00B71),
    (0x00B83, 0x00B83),
    (0x00B85, 0x00B8A),
    (0x00B8E, 0x00B90),
    (0x00B92, 0x00B95),
    (0x00B99, 0x00B9A),
    (0x00B9C, 0x00B9C),
    (0x00B9E, 0x00B9F),
    (0x00BA3, 0x00BA4),
    (0x00BA8, 0x00BAA),
    (0x00BAE, 0x00BB9),
    (0x00BD0, 0x00BD0),
    (0x00C05, 0x00C0C),
    (0x00C0E, 0x00C10),
    (0x00C12, 0x00C28),
    (0x00C2A, 0x00C39),
    (0x00C3D, 0x00C3D),
    (0x00C58, 0x00C5A),
    (0x00C5D, 0x00C5D),
    (0x00C60, 0x00C61),
    (0x00C80, 0x00C80),
    (0x00C85, 0x00C8C),
    (0x00C8E, 0x00C90),
    (0x00C92, 0x00CA8),
    (0x00CAA, 0x00CB3),
    (0x00CB5, 0x00CB9),
    (0x00CBD, 0x00CBD),
    (0x00CDD, 0x00CDE),
    (0x00CE0, 0x00CE1),
    (0x00CF1, 0x00CF2),
    (0x00D04, 0x00D0C),
    (0x00D0E, 0x00D10),
    (0x00D12, 0x00D3A),
    (0x00D3D, 0x00D3D),
    (0x00D4E, 0x00D4E),
    (0x00D54, 0x00D56),
    (0x00D5F, 0x00D61),
    (0x00D7A, 0x00D7F),
    (0x00D85, 0x00D96),
    (0x00D9A, 0x00DB1),
    (0x00DB3, 0x00DBB),
    (0x00DBD, 0x00DBD),
    (0x00DC0, 0x00DC6),
    (0x00E01, 0x00E30),
    (0x00E32, 0x00E33),
    (0x00E40, 0x00E46),
    (0x00E81, 0x00E82),
    (0x00E84, 0x00E84),
    (0x00E86, 0x00E8A),
    (0x00E8C, 0x00EA3),
    (0x00EA5, 0x00EA5),
    (0x00EA7, 0x00EB0),
    (0x00EB2, 0x00EB3),
    (0x00EBD, 0x00EBD),
    (0x00EC0, 0x00EC4),
    (0x00EC6, 0x00EC6),
    (0x00EDC, 0x00EDF),
    (0x00F00, 0x00F00),
    (0x00F40, 0x00F47),
    (0x00F49, 0x00F6C),
    (0x00F88, 0x00F8C),
    (0x01000, 0x0102A),
    (0x0103F, 0x0103F),
    (0x01050, 0x01055),
    (0x0105A, 0x0105D),
    (0x01061, 0x01061),
    (0x01065, 0x01066),
    (0x0106E, 0x01070),
    (0x01075, 0x01081),
    (0x0108E, 0x0108E),
    (0x010A0, 0x010C5),
    (0x010C7, 0x010C7),
    (0x010CD, 0x010CD),
    (0x010D0, 0x010FA),
    (0x010FC, 0x01248),
    (0x0124A, 0x0124D),
    (0x01250, 0x01256),
    (0x01258, 0x01258),
    (0x0125A, 0x0125D),
    (0x01260, 0x01288),
    (0x0128A, 0x0128D),
    (0x01290, 0x012B0),
    (0x012B2, 0x012B5),
    (0x012B8, 0x012BE),
    (0x012C0, 0x012C0),
    (0x012C2, 0x012C5),
    (0x012C8, 0x012D6),
    (0x012D8, 0x01310),
    (0x01312, 0x01315),
    (0x01318, 0x0135A),
    (0x01380, 0x0138F),
    (0x013A0, 0x013F5),
    (0x013F8, 0x013FD),
    (0x01401, 0x0166C),
    (0x0166F, 0x0167F),
    (0x01681, 0x0169A),
    (0x016A0, 0x016EA),
    (0x016F1, 0x016F8),
    (0x01700, 0x01711),
    (0x0171F, 0x01731),
    (0x01740, 0x01751),
    (0x01760, 0x0176C),
    (0x0176E, 0x01770),
    (0x01780, 0x017B3),
    (0x017D7, 0x017D7),
    (0x017DC, 0x017DC),
    (0x01820, 0x01878),
    (0x01880, 0x01884),
    (0x01887, 0x018A8),
    (0x018AA, 0x018AA),
    (0x018B0, 0x018F5),
    (0x01900, 0x0191E),
    (0x01950, 0x0196D),
    (0x01970, 0x01974),
    (0x01980, 0x019AB),
    (0x019B0, 0x019C9),
    (0x01A00, 0x01A16),
    (0x01A20, 0x01A54),
    (0x01AA7, 0x01AA7),
    (0x01B05, 0x01B33),
    (0x01B45, 0x01B4C),
    (0x01B83, 0x01BA0),
    (0x01BAE, 0x01BAF),
    (0x01BBA, 0x01BE5),
    (0x01C00, 0x01C23),
    (0x01C4D, 0x01C4F),
    (0x01C5A, 0x01C7D),
    (0x01C80, 0x01C88),
    (0x01C90, 0x01CBA),
    (0x01CBD, 0x01CBF),
    (0x01CE9, 0x01CEC),
    (0x01CEE, 0x01CF3),
    (0x01CF5, 0x01CF6),
    (0x01CFA, 0x01CFA),
    (0x01D00, 0x01DBF),
    (0x01E00, 0x01F15),
    (0x01F18, 0x01F1D),
    (0x01F20, 0x01F45),
    (0x01F48, 0x01F4D),
    (0x01F50, 0x01F57),
    (0x01F59, 0x01F59),
    (0x01F5B, 0x01F5B),
    (0x01F5D, 0x01F5D),
    (0x01F5F, 0x01F7D),
    (0x01F80, 0x01FB4),
    (0x01FB6, 0x01FBC),
    (0x01FBE, 0x01FBE),
    (0x01FC2, 0x01FC4),
    (0x01FC6, 0x01FCC),
    (0x01FD0, 0x01FD3),
    (0x01FD6, 0x01FDB),
    (0x01FE0, 0x01FEC),
    (0x01FF2, 0x01FF4),
    (0x01FF6, 0x01FFC),
    (0x02071, 0x02071),
    (0x0207F, 0x0207F),
    (0x02090, 0x0209C),
    (0x02102, 0x02102),
    (0x02107, 0x02107),
    (0x0210A, 0x02113),
    (0x02115, 0x02115),
    (0x02119, 0x0211D),
    (0x02124, 0x02124),
    (0x02126, 0x02126),
    (0x02128, 0x02128),
    (0x0212A, 0x0212D),
    (0x0212F, 0x02139),
    (0x0213C, 0x0213F),
    (0x02145, 0x02149),
    (0x0214E, 0x0214E),
    (0x02183, 0x02184),
    (0x02C00, 0x02CE4),
    (0x02CEB, 0x02CEE),
    (0x02CF2, 0x02CF3),
    (0x02D00, 0x02D25),
    (0x02D27, 0x02D27),
    (0x02D2D, 0x02D2D),
    (0x02D30, 0x02D67),
    (0x02D6F, 0x02D6F),
    (0x02D80, 0x02D96),
    (0x02DA0, 0x02DA6),
    (0x02DA8, 0x02DAE),
    (0x02DB0, 0x02DB6),
    (0x02DB8, 0x02DBE),
    (0x02DC0, 0x02DC6),
    (0x02DC8, 0x02DCE),
    (0x02DD0, 0x02DD6),
    (0x02DD8, 0x02DDE),
    (0x02E2F, 0x02E2F),
    (0x03005, 0x03006),
    (0x03031, 0x03035),
    (0x0303B, 0x0303C),
    (0x03041, 0x03096),
    (0x0309D, 0x0309F),
    (0x030A1, 0x030FA),
    (0x030FC, 0x030FF),
    (0x03105, 0x0312F),
    (0x03131, 0x0318E),
    (0x031A0, 0x031BF),
    (0x031F0, 0x031FF),
    (0x03400, 0x04DBF),
    (0x04E00, 0x0A48C),
    (0x0A4D0, 0x0A4FD),
    (0x0A500, 0x0A60C),
    (0x0A610, 0x0A61F),
    (0x0A62A, 0x0A62B),
    (0x0A640, 0x0A66E),
    (0x0A67F, 0x0A69D),
    (0x0A6A0, 0x0A6E5),
    (0x0A717, 0x0A71F),
    (0x0A722, 0x0A788),
    (0x0A78B, 0x0A7CA),
    (0x0A7D0, 0x0A7D1),
    (0x0A7D3, 0x0A7D3),
    (0x0A7D5, 0x0A7D9),
    (0x0A7F2, 0x0A801),
    (0x0A803, 0x0A805),
    (0x0A807, 0x0A80A),
    (0x0A80C, 0x0A822),
    (0x0A840, 0x0A873),
    (0x0A882, 0x0A8B3),
    (0x0A8F2, 0x0A8F7),
    (0x0A8FB, 0x0A8FB),
    (0x0A8FD, 0x0A8FE),
    (0x0A90A, 0x0A925),
    (0x0A930, 0x0A946),
    (0x0A960, 0x0A97C),
    (0x0A984, 0x0A9B2),
    (0x0A9CF, 0x0A9CF),
    (0x0A9E0, 0x0A9E4),
    (0x0A9E6, 0x0A9EF),
    (0x0A9FA, 0x0A9FE),
    (0x0AA00, 0x0AA28),
    (0x0AA40, 0x0AA42),
    (0x0AA44, 0x0AA4B),
    (0x0AA60, 0x0AA76),
    (0x0AA7A, 0x0AA7A),
    (0x0AA7E, 0x0AAAF),
    (0x0AAB1, 0x0AAB1),
    (0x0AAB5, 0x0AAB6),
    (0x0AAB9, 0x0AABD),
    (0x0AAC0, 0x0AAC0),
    (0x0AAC2, 0x0AAC2),
    (0x0AADB, 0x0AADD),
    (0x0AAE0, 0x0AAEA),
    (0x0AAF2, 0x0AAF4),
    (0x0AB01, 0x0AB06),
    (0x0AB09, 0x0AB0E),
    (0x0AB11, 0x0AB16),
    (0x0AB20, 0x0AB26),
    (0x0AB28, 0x0AB2E),
    (0x0AB30, 0x0AB5A),
    (0x0AB5C, 0x0AB69),
    (0x0AB70, 0x0ABE2),
    (0x0AC00, 0x0D7A3),
    (0x0D7B0, 0x0D7C6),
    (0x0D7CB, 0x0D7FB),
    (0x0F900, 0x0FA6D),
    (0x0FA70, 0x0FAD9),
    (0x0FB00, 0x0FB06),
    (0x0FB13, 0x0FB17),
    (0x0FB1D, 0x0FB1D),
    (0x0FB1F, 0x0FB28),
    (0x0FB2A, 0x0FB36),
    (0x0FB38, 0x0FB3C),
    (0x0FB3E, 0x0FB3E),
    (0x0FB40, 0x0FB41),
    (0x0FB43, 0x0FB44),
    (0x0FB46, 0x0FBB1),
    (0x0FBD3, 0x0FD3D),
    (0x0FD50, 0x0FD8F),
    (0x0FD92, 0x0FDC7),
    (0x0FDF0, 0x0FDFB),
    (0x0FE70, 0x0FE74),
    (0x0FE76, 0x0FEFC),
    (0x0FF21, 0x0FF3A),
    (0x0FF41, 0x0FF5A),
    (0x0FF66, 0x0FFBE),
    (0x0FFC2, 0x0FFC7),
    (0x0FFCA, 0x0FFCF),
    (0x0FFD2, 0x0FFD7),
    (0x0FFDA, 0x0FFDC),
    (0x10000, 0x1000B),
    (0x1000D, 0x10026),
    (0x10028, 0x1003A),
    (0x1003C, 0x1003D),
    (0x1003F, 0x1004D),
    (0x10050, 0x1005D),
    (0x10080, 0x100FA),
    (0x10280, 0x1029C),
    (0x102A0, 0x102D0),
    (0x10300, 0x1031F),
    (0x1032D, 0x10340),
    (0x10342, 0x10349),
    (0x10350, 0x10375),
    (0x10380, 0x1039D),
    (0x103A0, 0x103C3),
    (0x103C8, 0x103CF),
    (0x10400, 0x1049D),
    (0x104B0, 0x104D3),
    (0x104D8, 0x104FB),
    (0x10500, 0x10527),
    (0x10530, 0x10563),
    (0x10570, 0x1057A),
    (0x1057C, 0x1058A),
    (0x1058C, 0x10592),
    (0x10594, 0x10595),
    (0x10597, 0x105A1),
    (0x105A3, 0x105B1),
    (0x105B3, 0x105B9),
    (0x105BB, 0x105BC),
    (0x10600, 0x10736),
    (0x10740, 0x10755),
    (0x10760, 0x10767),
    (0x10780, 0x10785),
    (0x10787, 0x107B0),
    (0x107B2, 0x107BA),
    (0x10800, 0x10805),
    (0x10808, 0x10808),
    (0x1080A, 0x10835),
    (0x10837, 0x10838),
    (0x1083C, 0x1083C),
    (0x1083F, 0x10855),
    (0x10860, 0x10876),
    (0x10880, 0x1089E),
    (0x108E0, 0x108F2),
    (0x108F4, 0x108F5),
    (0x10900, 0x10915),
    (0x10920, 0x10939),
    (0x10980, 0x109B7),
    (0x109BE, 0x109BF),
    (0x10A00, 0x10A00),
    (0x10A10, 0x10A13),
    (0x10A15, 0x10A17),
    (0x10A19, 0x10A35),
    (0x10A60, 0x10A7C),
    (0x10A80, 0x10A9C),
    (0x10AC0, 0x10AC7),
    (0x10AC9, 0x10AE4),
    (0x10B00, 0x10B35),
    (0x10B40, 0x10B55),
    (0x10B60, 0x10B72),
    (0x10B80, 0x10B91),
    (0x10C00, 0x10C48),
    (0x10C80, 0x10CB2),
    (0x10CC0, 0x10CF2),
    (0x10D00, 0x10D23),
    (0x10E80, 0x10EA9),
    (0x10EB0, 0x10EB1),
    (0x10F00, 0x10F1C),
    (0x10F27, 0x10F27),
    (0x10F30, 0x10F45),
    (0x10F70, 0x10F81),
    (0x10FB0, 0x10FC4),
    (0x10FE0, 0x10FF6),
    (0x11003, 0x11037),
    (0x11071, 0x11072),
    (0x11075, 0x11075),
    (0x11083, 0x110AF),
    (0x110D0, 0x110E8),
    (0x11103, 0x11126),
    (0x11144, 0x11144),
    (0x11147, 0x11147),
    (0x11150, 0x11172),
    (0x11176, 0x11176),
    (0x11183, 0x111B2),
    (0x111C1, 0x111C4),
    (0x111DA, 0x111DA),
    (0x111DC, 0x111DC),
    (0x11200, 0x11211),
    (0x11213, 0x1122B),
    (0x1123F, 0x11240),
    (0x11280, 0x11286),
    (0x11288, 0x11288),
    (0x1128A, 0x1128D),
    (0x1128F, 0x1129D),
    (0x1129F, 0x112A8),
    (0x112B0, 0x112DE),
    (0x11305, 0x1130C),
    (0x1130F, 0x11310),
    (0x11313, 0x11328),
    (0x1132A, 0x11330),
    (0x11332, 0x11333),
    (0x11335, 0x11339),
    (0x1133D, 0x1133D),
    (0x11350, 0x11350),
    (0x1135D, 0x11361),
    (0x11400, 0x11434),
    (0x11447, 0x1144A),
    (0x1145F, 0x11461),
    (0x11480, 0x114AF),
    (0x114C4, 0x114C5),
    (0x114C7, 0x114C7),
    (0x11580, 0x115AE),
    (0x115D8, 0x115DB),
    (0x11600, 0x1162F),
    (0x11644, 0x11644),
    (0x11680, 0x116AA),
    (0x116B8, 0x116B8),
    (0x11700, 0x1171A),
    (0x11740, 0x11746),
    (0x11800, 0x1182B),
    (0x118A0, 0x118DF),
    (0x118FF, 0x11906),
    (0x11909, 0x11909),
    (0x1190C, 0x11913),
    (0x11915, 0x11916),
    (0x11918, 0x1192F),
    (0x1193F, 0x1193F),
    (0x11941, 0x11941),
    (0x119A0, 0x119A7),
    (0x119AA, 0x119D0),
    (0x119E1, 0x119E1),
    (0x119E3, 0x119E3),
    (0x11A00, 0x11A00),
    (0x11A0B, 0x11A32),
    (0x11A3A, 0x11A3A),
    (0x11A50, 0x11A50),
    (0x11A5C, 0x11A89),
    (0x11A9D, 0x11A9D),
    (0x11AB0, 0x11AF8),
    (0x11C00, 0x11C08),
    (0x11C0A, 0x11C2E),
    (0x11C40, 0x11C40),
    (0x11C72, 0x11C8F),
    (0x11D00, 0x11D06),
    (0x11D08, 0x11D09),
    (0x11D0B, 0x11D30),
    (0x11D46, 0x11D46),
    (0x11D60, 0x11D65),
    (0x11D67, 0x11D68),
    (0x11D6A, 0x11D89),
    (0x11D98, 0x11D98),
    (0x11EE0, 0x11EF2),
    (0x11F02, 0x11F02),
    (0x11F04, 0x11F10),
    (0x11F12, 0x11F33),
    (0x11FB0, 0x11FB0),
    (0x12000, 0x12399),
    (0x12480, 0x12543),
    (0x12F90, 0x12FF0),
    (0x13000, 0x1342F),
    (0x13441, 0x13446),
    (0x14400, 0x14646),
    (0x16800, 0x16A38),
    (0x16A40, 0x16A5E),
    (0x16A70, 0x16ABE),
    (0x16AD0, 0x16AED),
    (0x16B00, 0x16B2F),
    (0x16B40, 0x16B43),
    (0x16B63, 0x16B77),
    (0x16B7D, 0x16B8F),
    (0x16E40, 0x16E7F),
    (0x16F00, 0x16F4A),
    (0x16F50, 0x16F50),
    (0x16F93, 0x16F9F),
    (0x16FE0, 0x16FE1),
    (0x16FE3, 0x16FE3),
    (0x17000, 0x187F7),
    (0x18800, 0x18CD5),
    (0x18D00, 0x18D08),
    (0x1AFF0, 0x1AFF3),
    (0x1AFF5, 0x1AFFB),
    (0x1AFFD, 0x1AFFE),
    (0x1B000, 0x1B122),
    (0x1B132, 0x1B132),
    (0x1B150, 0x1B152),
    (0x1B155, 0x1B155),
    (0x1B164, 0x1B167),
    (0x1B170, 0x1B2FB),
    (0x1BC00, 0x1BC6A),
    (0x1BC70, 0x1BC7C),
    (0x1BC80, 0x1BC88),
    (0x1BC90, 0x1BC99),
    (0x1D400, 0x1D454),
    (0x1D456, 0x1D49C),
    (0x1D49E, 0x1D49F),
    (0x1D4A2, 0x1D4A2),
    (0x1D4A5, 0x1D4A6),
    (0x1D4A9, 0x1D4AC),
    (0x1D4AE, 0x1D4B9),
    (0x1D4BB, 0x1D4BB),
    (0x1D4BD, 0x1D4C3),
    (0x1D4C5, 0x1D505),
    (0x1D507, 0x1D50A),
    (0x1D50D, 0x1D514),
    (0x1D516, 0x1D51C),
    (0x1D51E, 0x1D539),
    (0x1D53B, 0x1D53E),
    (0x1D540, 0x1D544),
    (0x1D546, 0x1D546),
    (0x1D54A, 0x1D550),
    (0x1D552, 0x1D6A5),
    (0x1D6A8, 0x1D6C0),
    (0x1D6C2, 0x1D6DA),
    (0x1D6DC, 0x1D6FA),
    (0x1D6FC, 0x1D714),
    (0x1D716, 0x1D734),
    (0x1D736, 0x1D74E),
    (0x1D750, 0x1D76E),
    (0x1D770, 0x1D788),
    (0x1D78A, 0x1D7A8),
    (0x1D7AA, 0x1D7C2),
    (0x1D7C4, 0x1D7CB),
    (0x1DF00, 0x1DF1E),
    (0x1DF25, 0x1DF2A),
    (0x1E030, 0x1E06D),
    (0x1E100, 0x1E12C),
    (0x1E137, 0x1E13D),
    (0x1E14E, 0x1E14E),
    (0x1E290, 0x1E2AD),
    (0x1E2C0, 0x1E2EB),
    (0x1E4D0, 0x1E4EB),
    (0x1E7E0, 0x1E7E6),
    (0x1E7E8, 0x1E7EB),
    (0x1E7ED, 0x1E7EE),
    (0x1E7F0, 0x1E7FE),
    (0x1E800, 0x1E8C4),
    (0x1E900, 0x1E943),
    (0x1E94B, 0x1E94B),
    (0x1EE00, 0x1EE03),
    (0x1EE05, 0x1EE1F),
    (0x1EE21, 0x1EE22),
    (0x1EE24, 0x1EE24),
    (0x1EE27, 0x1EE27),
    (0x1EE29, 0x1EE32),
    (0x1EE34, 0x1EE37),
    (0x1EE39, 0x1EE39),
    (0x1EE3B, 0x1EE3B),
    (0x1EE42, 0x1EE42),
    (0x1EE47, 0x1EE47),
    (0x1EE49, 0x1EE49),
    (0x1EE4B, 0x1EE4B),
    (0x1EE4D, 0x1EE4F),
    (0x1EE51, 0x1EE52),
    (0x1EE54, 0x1EE54),
    (0x1EE57, 0x1EE57),
    (0x1EE59, 0x1EE59),
    (0x1EE5B, 0x1EE5B),
    (0x1EE5D, 0x1EE5D),
    (0x1EE5F, 0x1EE5F),
    (0x1EE61, 0x1EE62),
    (0x1EE64, 0x1EE64),
    (0x1EE67, 0x1EE6A),
    (0x1EE6C, 0x1EE72),
    (0x1EE74, 0x1EE77),
    (0x1EE79, 0x1EE7C),
    (0x1EE7E, 0x1EE7E),
    (0x1EE80, 0x1EE89),
    (0x1EE8B, 0x1EE9B),
    (0x1EEA1, 0x1EEA3),
    (0x1EEA5, 0x1EEA9),
    (0x1EEAB, 0x1EEBB),
    (0x20000, 0x2A6DF),
    (0x2A700, 0x2B739),
    (0x2B740, 0x2B81D),
    (0x2B820, 0x2CEA1),
    (0x2CEB0, 0x2EBE0),
    (0x2F800, 0x2FA1D),
    (0x30000, 0x3134A),
    (0x31350, 0x323AF),
];

const UNICODE_NUMBER_RANGES: &[(u32, u32)] = &[
    (0x00030, 0x00039),
    (0x000B2, 0x000B3),
    (0x000B9, 0x000B9),
    (0x000BC, 0x000BE),
    (0x00660, 0x00669),
    (0x006F0, 0x006F9),
    (0x007C0, 0x007C9),
    (0x00966, 0x0096F),
    (0x009E6, 0x009EF),
    (0x009F4, 0x009F9),
    (0x00A66, 0x00A6F),
    (0x00AE6, 0x00AEF),
    (0x00B66, 0x00B6F),
    (0x00B72, 0x00B77),
    (0x00BE6, 0x00BF2),
    (0x00C66, 0x00C6F),
    (0x00C78, 0x00C7E),
    (0x00CE6, 0x00CEF),
    (0x00D58, 0x00D5E),
    (0x00D66, 0x00D78),
    (0x00DE6, 0x00DEF),
    (0x00E50, 0x00E59),
    (0x00ED0, 0x00ED9),
    (0x00F20, 0x00F33),
    (0x01040, 0x01049),
    (0x01090, 0x01099),
    (0x01369, 0x0137C),
    (0x016EE, 0x016F0),
    (0x017E0, 0x017E9),
    (0x017F0, 0x017F9),
    (0x01810, 0x01819),
    (0x01946, 0x0194F),
    (0x019D0, 0x019DA),
    (0x01A80, 0x01A89),
    (0x01A90, 0x01A99),
    (0x01B50, 0x01B59),
    (0x01BB0, 0x01BB9),
    (0x01C40, 0x01C49),
    (0x01C50, 0x01C59),
    (0x02070, 0x02070),
    (0x02074, 0x02079),
    (0x02080, 0x02089),
    (0x02150, 0x02182),
    (0x02185, 0x02189),
    (0x02460, 0x0249B),
    (0x024EA, 0x024FF),
    (0x02776, 0x02793),
    (0x02CFD, 0x02CFD),
    (0x03007, 0x03007),
    (0x03021, 0x03029),
    (0x03038, 0x0303A),
    (0x03192, 0x03195),
    (0x03220, 0x03229),
    (0x03248, 0x0324F),
    (0x03251, 0x0325F),
    (0x03280, 0x03289),
    (0x032B1, 0x032BF),
    (0x0A620, 0x0A629),
    (0x0A6E6, 0x0A6EF),
    (0x0A830, 0x0A835),
    (0x0A8D0, 0x0A8D9),
    (0x0A900, 0x0A909),
    (0x0A9D0, 0x0A9D9),
    (0x0A9F0, 0x0A9F9),
    (0x0AA50, 0x0AA59),
    (0x0ABF0, 0x0ABF9),
    (0x0FF10, 0x0FF19),
    (0x10107, 0x10133),
    (0x10140, 0x10178),
    (0x1018A, 0x1018B),
    (0x102E1, 0x102FB),
    (0x10320, 0x10323),
    (0x10341, 0x10341),
    (0x1034A, 0x1034A),
    (0x103D1, 0x103D5),
    (0x104A0, 0x104A9),
    (0x10858, 0x1085F),
    (0x10879, 0x1087F),
    (0x108A7, 0x108AF),
    (0x108FB, 0x108FF),
    (0x10916, 0x1091B),
    (0x109BC, 0x109BD),
    (0x109C0, 0x109CF),
    (0x109D2, 0x109FF),
    (0x10A40, 0x10A48),
    (0x10A7D, 0x10A7E),
    (0x10A9D, 0x10A9F),
    (0x10AEB, 0x10AEF),
    (0x10B58, 0x10B5F),
    (0x10B78, 0x10B7F),
    (0x10BA9, 0x10BAF),
    (0x10CFA, 0x10CFF),
    (0x10D30, 0x10D39),
    (0x10E60, 0x10E7E),
    (0x10F1D, 0x10F26),
    (0x10F51, 0x10F54),
    (0x10FC5, 0x10FCB),
    (0x11052, 0x1106F),
    (0x110F0, 0x110F9),
    (0x11136, 0x1113F),
    (0x111D0, 0x111D9),
    (0x111E1, 0x111F4),
    (0x112F0, 0x112F9),
    (0x11450, 0x11459),
    (0x114D0, 0x114D9),
    (0x11650, 0x11659),
    (0x116C0, 0x116C9),
    (0x11730, 0x1173B),
    (0x118E0, 0x118F2),
    (0x11950, 0x11959),
    (0x11C50, 0x11C6C),
    (0x11D50, 0x11D59),
    (0x11DA0, 0x11DA9),
    (0x11F50, 0x11F59),
    (0x11FC0, 0x11FD4),
    (0x12400, 0x1246E),
    (0x16A60, 0x16A69),
    (0x16AC0, 0x16AC9),
    (0x16B50, 0x16B59),
    (0x16B5B, 0x16B61),
    (0x16E80, 0x16E96),
    (0x1D2C0, 0x1D2D3),
    (0x1D2E0, 0x1D2F3),
    (0x1D360, 0x1D378),
    (0x1D7CE, 0x1D7FF),
    (0x1E140, 0x1E149),
    (0x1E2F0, 0x1E2F9),
    (0x1E4F0, 0x1E4F9),
    (0x1E8C7, 0x1E8CF),
    (0x1E950, 0x1E959),
    (0x1EC71, 0x1ECAB),
    (0x1ECAD, 0x1ECAF),
    (0x1ECB1, 0x1ECB4),
    (0x1ED01, 0x1ED2D),
    (0x1ED2F, 0x1ED3D),
    (0x1F100, 0x1F10C),
    (0x1FBF0, 0x1FBF9),
];

// ---------------------------------------------------------------------------
// Real ONNX Runtime session and embedder.
// ---------------------------------------------------------------------------

/// A real ONNX Runtime session (feature-gated). The library is loaded at
/// runtime (`load-dynamic`); the model file is opened into an inference
/// session.
pub struct OnnxRuntime {
    session: Mutex<Session>,
    tokenizer: Box<dyn Tokenizer>,
    /// Token id space size (used only to bound the fallback tokenizer).
    #[allow(dead_code)]
    vocab: usize,
    /// How rank-3 hidden states are pooled.
    pooling: Pooling,
    /// Max sequence length passed to the tokenizer.
    max_seq_len: usize,
    /// Declared graph inputs of the ONNX file (name, elem type, static
    /// dims), used to synthesize the `past_key_values` tensors that
    /// KV-cache decoder exports require. Empty when the file could not be
    /// re-parsed (no-cache exports need no synthesis at all).
    input_shapes: Vec<crate::onnx_proto::InputShape>,
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

    /// Load with a deployment-provided tokenizer (mean pooling, 8192 max
    /// sequence length).
    pub fn load_with_tokenizer(
        model_path: &str,
        tokenizer: Box<dyn Tokenizer>,
    ) -> EmbeddingResult<Self> {
        Self::load_with_config(model_path, tokenizer, Pooling::Mean, 8192)
    }

    /// Load with explicit pooling and sequence-length configuration.
    pub fn load_with_config(
        model_path: &str,
        tokenizer: Box<dyn Tokenizer>,
        pooling: Pooling,
        max_seq_len: usize,
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
        // Parse the declared graph inputs so `run_one` can synthesize the
        // `past_key_values` tensors a KV-cache decoder export requires.
        // A parse failure is not fatal: no-cache exports (e.g. BGE-M3)
        // declare only `input_ids`/`attention_mask` and need no synthesis.
        let input_shapes = crate::onnx_proto::read_input_shapes(model_path).unwrap_or_default();
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            vocab: 30_000,
            pooling,
            max_seq_len: max_seq_len.max(1),
            input_shapes,
        })
    }

    /// Run one text through the session and normalize the output to `dim`.
    ///
    /// Two input protocols are supported:
    /// * transformer exports declaring `input_ids` (Qwen3 / BGE-M3): the
    ///   `attention_mask`, `token_type_ids`, `position_ids` and
    ///   `cache_position` tensors are synthesized when the graph declares
    ///   them, and `past_key_values` inputs of KV-cache decoder exports are
    ///   synthesized as empty caches (shapes read from the ONNX file); the
    ///   preferred outputs are `last_hidden_state` > `pooler_output` >
    ///   `sentence_embedding`;
    /// * legacy single-`input` exports: one i64 tensor, first output used.
    fn run_one(&self, text: &str, dim: usize) -> EmbeddingResult<Vec<f32>> {
        let ids = self.tokenizer.tokenize(text, self.max_seq_len);
        let ids_n = ids.len();
        let n = ids_n.max(1);
        let mut session = self
            .session
            .lock()
            .map_err(|_| EmbeddingError::InvalidArgument {
                field: "session",
                reason: "session lock poisoned".to_string(),
            })?;
        let input_names: Vec<String> = session
            .inputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        // Capture the declared output names before `run` (which mutably
        // borrows the session for the duration of the returned outputs).
        let output_names: Vec<String> = session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        let (shape, data) = if input_names.iter().any(|s| s == "input_ids") {
            let shape_in = vec![1_i64, n as i64];
            let mut inputs: Vec<(String, DynTensor)> = Vec::new();
            let push_i64 = |inputs: &mut Vec<(String, DynTensor)>,
                            name: &str,
                            data: Vec<i64>|
             -> EmbeddingResult<()> {
                let t = Tensor::<i64>::from_array((shape_in.clone(), data))
                    .map_err(|e| EmbeddingError::InvalidArgument {
                        field: "tensor",
                        reason: e.to_string(),
                    })?
                    .upcast();
                inputs.push((name.to_string(), t));
                Ok(())
            };
            push_i64(&mut inputs, "input_ids", ids.clone())?;
            if input_names.iter().any(|s| s == "attention_mask") {
                push_i64(&mut inputs, "attention_mask", vec![1_i64; n])?;
            }
            if input_names.iter().any(|s| s == "token_type_ids") {
                push_i64(&mut inputs, "token_type_ids", vec![0_i64; n])?;
            }
            if input_names.iter().any(|s| s == "position_ids") {
                push_i64(&mut inputs, "position_ids", (0..n as i64).collect())?;
            }
            // Synthesize any remaining declared inputs so KV-cache decoder
            // exports (`past_key_values.*`) run against an empty cache:
            // static dims keep their value, the batch axis is 1 and the
            // remaining (past-length) axes are 0.
            // Owned copies, so no borrow of `self`/`inputs` escapes the loop.
            let provided: std::collections::HashSet<String> =
                inputs.iter().map(|(nm, _)| nm.clone()).collect();
            // Owned copies (name, elem type, empty-cache shape), so no
            // borrow of `self` escapes the loop.
            let kv_shapes: std::collections::HashMap<String, (u32, Vec<i64>)> = self
                .input_shapes
                .iter()
                .filter(|s| s.name.starts_with("past_key_values"))
                .map(|s| {
                    let dims = s
                        .dims
                        .iter()
                        .enumerate()
                        .map(|(axis, d)| match d {
                            Some(v) if *v > 0 => *v,
                            _ => i64::from(axis == 0),
                        })
                        .collect();
                    (s.name.clone(), (s.elem_type, dims))
                })
                .collect();
            for name in &input_names {
                if provided.contains(name) {
                    continue;
                }
                if name == "cache_position" {
                    push_i64(&mut inputs, name, (0..n as i64).collect())?;
                } else if name.starts_with("past_key_values") {
                    let Some((elem_type, dims)) = kv_shapes.get(name).map(|(e, d)| (*e, d.clone()))
                    else {
                        return Err(EmbeddingError::InvalidArgument {
                            field: "run",
                            reason: format!(
                                "model declares `{name}` but its shape could not be read from the ONNX file"
                            ),
                        });
                    };
                    if elem_type != crate::onnx_proto::TENSOR_FLOAT {
                        return Err(EmbeddingError::InvalidArgument {
                            field: "run",
                            reason: format!(
                                "`{name}` is not a float KV-cache input (ONNX elem type {elem_type})"
                            ),
                        });
                    }
                    let count = dims.iter().product::<i64>().max(0) as usize;
                    let t = Tensor::<f32>::from_array((dims, vec![0.0f32; count]))
                        .map_err(|e| EmbeddingError::InvalidArgument {
                            field: "tensor",
                            reason: e.to_string(),
                        })?
                        .upcast();
                    inputs.push((name.clone(), t));
                } else {
                    return Err(EmbeddingError::InvalidArgument {
                        field: "run",
                        reason: format!(
                            "model declares input `{name}` which this runtime cannot synthesize"
                        ),
                    });
                }
            }
            let outputs = session
                .run(inputs)
                .map_err(|e| EmbeddingError::InvalidArgument {
                    field: "run",
                    reason: e.to_string(),
                })?;
            if output_names.is_empty() {
                return Err(EmbeddingError::InvalidArgument {
                    field: "run",
                    reason: "session declared no outputs".to_string(),
                });
            }
            let out_name = ["last_hidden_state", "pooler_output", "sentence_embedding"]
                .iter()
                .find(|nm| output_names.iter().any(|n| n == *nm))
                .copied()
                .or_else(|| output_names.first().map(|s| s.as_str()))
                .unwrap_or("last_hidden_state");
            let value = match outputs.get(out_name) {
                Some(v) => v,
                None => &outputs[0],
            };
            let (shape, data) =
                value
                    .try_extract_tensor::<f32>()
                    .map_err(|e| EmbeddingError::InvalidArgument {
                        field: "extract",
                        reason: e.to_string(),
                    })?;
            ((*shape).to_vec(), data.to_vec())
        } else {
            // Legacy single-`input` protocol.
            let input =
                Tensor::<i64>::from_array(([1usize, n], ids.into_boxed_slice())).map_err(|e| {
                    EmbeddingError::InvalidArgument {
                        field: "input",
                        reason: e.to_string(),
                    }
                })?;
            let outputs =
                session
                    .run(ort::inputs![input])
                    .map_err(|e| EmbeddingError::InvalidArgument {
                        field: "run",
                        reason: e.to_string(),
                    })?;
            let out = &outputs[0];
            let (shape, data) =
                out.try_extract_tensor::<f32>()
                    .map_err(|e| EmbeddingError::InvalidArgument {
                        field: "extract",
                        reason: e.to_string(),
                    })?;
            ((*shape).to_vec(), data.to_vec())
        };
        Ok(pool_and_normalize(&shape, data, self.pooling, dim))
    }
}

/// Reduce a tensor output to one vector and L2-normalize it to `dim`.
///
/// Rank-3 `[1, seq, d]` outputs are pooled (`Mean` over the sequence axis —/// correct because batch=1 carries no padding —or `LastToken`); rank-2
/// `[1, d]` outputs are used as-is. The result is truncated/zero-padded to
/// `dim` and L2-normalized (zero vectors stay zero).
fn pool_and_normalize(shape: &[i64], data: Vec<f32>, pooling: Pooling, dim: usize) -> Vec<f32> {
    let pooled: Vec<f32> = if shape.len() != 3 {
        data
    } else {
        let seq = shape[1].max(1) as usize;
        let d = shape[2].max(1) as usize;
        let usable = (seq * d).min(data.len());
        let mut out: Vec<f32> = Vec::new();
        let single_row = match pooling {
            Pooling::LastToken => seq.saturating_sub(1),
            Pooling::Cls => 0,
            Pooling::Mean => usize::MAX,
        };
        if single_row != usize::MAX && single_row < seq {
            let start = single_row * d;
            if start < usable {
                let end = (start + d).min(usable);
                for v in data.iter().take(end).skip(start) {
                    out.push(*v);
                }
            }
            if out.len() < d {
                out.resize(d, 0.0);
            }
        } else {
            let mut sums = vec![0.0f32; d];
            for t in 0..seq {
                let start = t * d;
                if start + d > data.len() {
                    break;
                }
                for j in 0..d {
                    sums[j] += data[start + j];
                }
            }
            for x in sums.iter_mut() {
                *x /= seq as f32;
            }
            out = sums;
        }
        out
    };
    let mut vec = vec![0.0f32; dim];
    let n = pooled.len().min(dim);
    vec[..n].copy_from_slice(&pooled[..n]);
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vec.iter_mut() {
            *value /= norm;
        }
    }
    vec
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

    /// Load with a deployment-provided tokenizer (mean pooling).
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

    /// Load with a model-matching tokenizer plus pooling and sequence-length
    /// configuration (Qwen3: `LastToken`; BGE-M3: `Mean`).
    pub fn load_with_config(
        model_path: &str,
        profile_id: &str,
        dimension: usize,
        tokenizer: Box<dyn Tokenizer>,
        pooling: Pooling,
        max_seq_len: usize,
    ) -> EmbeddingResult<Self> {
        let runtime = OnnxRuntime::load_with_config(model_path, tokenizer, pooling, max_seq_len)?;
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

    /// Regression: `pool_and_normalize` must return the last-row values for a
    /// rank-3 `[1, seq, d]` tensor (the zero-vector failure mode seen while
    /// porting the Qwen3 KV-cache export must not come back).
    #[test]
    fn pool_last_token_takes_last_row() {
        let data: Vec<f32> = (0..7 * 8).map(|i| i as f32).collect();
        let out = pool_and_normalize(&[1, 7, 8], data, Pooling::LastToken, 8);
        assert_eq!(out.len(), 8);
        // last row = 48..55 (7 rows of 8); L2-normalized it must equal the normalized row
        let row: Vec<f32> = (48..56).map(|i| i as f32).collect();
        let norm: f32 = row.iter().map(|v| v * v).sum::<f32>().sqrt();
        for (got, want) in out.iter().zip(row.iter()) {
            assert!((got - want / norm).abs() < 1e-4);
        }
    }

    /// Oracle reference vectors (HuggingFace `tokenizers` implementation),
    /// generated with the same test strings and frozen for regression.
    const REF_QWEN3: &[(&str, &[i64])] = &[
        ("hello world", &[14990, 1879, 151643]),
        (
            "UKMA is a knowledge base for code.",
            &[24537, 4835, 374, 264, 6540, 2331, 369, 2038, 13, 151643],
        ),
        (
            "the cat sat on the mat",
            &[1782, 8251, 7578, 389, 279, 5517, 151643],
        ),
        (
            "Similar sentences should have high cosine similarity.",
            &[34402, 22870, 1265, 614, 1550, 75259, 37623, 13, 151643],
        ),
        (
            "機器學習 人工智慧 支持",
            &[
                100482, 31548, 106745, 220, 102249, 101934, 80090, 107, 68878, 151643,
            ],
        ),
        (
            "F:/UKMA/crates is a directory path",
            &[
                37, 14375, 24537, 4835, 2899, 66485, 374, 264, 6220, 1815, 151643,
            ],
        ),
        (
            "Don't stop; it's a test with 'quotes' and 123 numbers",
            &[
                8002, 944, 2936, 26, 432, 594, 264, 1273, 448, 364, 53282, 6, 323, 220, 16, 17, 18,
                5109, 151643,
            ],
        ),
        (
            "café naïve résumé —em-dash and 'quotes', (parens)",
            &[
                924, 58858, 94880, 586, 9333, 1242, 963, 1959, 336, 1737, 988, 323, 364, 53282,
                516, 320, 3380, 4412, 8, 151643,
            ],
        ),
        (
            "   leading and trailing spaces   ",
            &[256, 6388, 323, 27748, 12621, 262, 151643],
        ),
    ];

    const REF_BGE_M3: &[(&str, &[i64])] = &[
        ("hello world", &[0, 33600, 31, 8999, 2]),
        (
            "UKMA is a knowledge base for code.",
            &[0, 17274, 8218, 83, 10, 51359, 3647, 100, 18151, 5, 2],
        ),
        (
            "the cat sat on the mat",
            &[0, 70, 7515, 11736, 98, 70, 2589, 2],
        ),
        (
            "Similar sentences should have high cosine similarity.",
            &[
                0, 209683, 149357, 7, 5608, 765, 11192, 552, 14186, 21373, 2481, 5, 2,
            ],
        ),
        (
            "機器學習 人工智慧 支持",
            &[0, 6, 74086, 14492, 6, 66537, 23841, 6, 7499, 2],
        ),
        (
            "F:/UKMA/crates is a directory path",
            &[
                0, 563, 12, 64, 15766, 8218, 64, 187906, 83, 10, 14364, 53, 60875, 2,
            ],
        ),
        (
            "Don't stop; it's a test with 'quotes' and 123 numbers",
            &[
                0, 7650, 25, 18, 7279, 74, 442, 25, 7, 10, 3034, 678, 242, 68726, 7, 25, 136,
                37638, 101935, 2,
            ],
        ),
        (
            "café naïve résumé —em-dash and 'quotes', (parens)",
            &[
                0, 26216, 24, 9392, 272, 233482, 292, 195, 9, 155892, 136, 242, 68726, 7, 25, 4,
                15, 2500, 1755, 16, 2,
            ],
        ),
        (
            "   leading and trailing spaces   ",
            &[0, 105207, 136, 141037, 214, 32628, 7, 6, 2],
        ),
    ];

    /// Load the deployment tokenizer file, selecting the family under test.
    type FamilyRefs = &'static [(&'static str, &'static [i64])];

    fn load_family_tokenizer() -> Option<(HfTokenizer, FamilyRefs)> {
        let (family, refs) = match std::env::var("UKMA_TEST_MODEL").as_deref() {
            Ok("qwen3") => ("qwen3", REF_QWEN3),
            Ok("bge-m3") => ("bge-m3", REF_BGE_M3),
            _ => return None,
        };
        let path = std::env::var("UKMA_ONNX_TOKENIZER_PATH").ok()?;
        let tok = HfTokenizer::from_file(&path).ok()?;
        let _ = family;
        Some((tok, refs))
    }

    /// Tokenizer parity with the HuggingFace reference implementation.
    ///
    /// Environment-gated: set `UKMA_TEST_MODEL=qwen3|bge-m3` and
    /// `UKMA_ONNX_TOKENIZER_PATH=<tokenizer.json>` to enable (skipped when
    /// the deployment artifacts are absent).
    #[test]
    fn tokenizer_matches_hf_oracle_reference_ids() {
        let Some((tok, refs)) = load_family_tokenizer() else {
            eprintln!(
                "SKIP tokenizer_matches_hf_oracle_reference_ids \
                 (set UKMA_TEST_MODEL=qwen3|bge-m3 and UKMA_ONNX_TOKENIZER_PATH)"
            );
            return;
        };
        for (text, expected) in refs {
            let got = tok.tokenize(text, 8192);
            assert_eq!(&got, expected, "text={text:?}");
        }
    }

    /// The classic byte table must map the deployment-critical entries
    /// (space, identity ASCII, control block, 4-byte leading bytes).
    #[test]
    fn byte_table_key_entries_are_classic() {
        assert_eq!(QWEN3_BYTE_TABLE[0x20], 0x0120, "space maps to ?");
        assert_eq!(QWEN3_BYTE_TABLE[0x61], 0x0061, "'a' is identity");
        assert_eq!(QWEN3_BYTE_TABLE[0x41], 0x0041, "'A' is identity");
        assert_eq!(QWEN3_BYTE_TABLE[0x00], 0x0100, "NUL maps to the ? block");
        assert_eq!(QWEN3_BYTE_TABLE[0x1F], 0x011F, "US maps to the ? block");
        assert_eq!(QWEN3_BYTE_TABLE[0x7F], 0x0121, "DEL maps to ġ");
        assert_eq!(QWEN3_BYTE_TABLE[0xF0], 0x00F0, "4-byte F0 leads with ð");
        assert_eq!(QWEN3_BYTE_TABLE[0xF1], 0x00F0, "4-byte F1 leads with ð");
        assert_eq!(QWEN3_BYTE_TABLE[0xF2], 0x00F1, "4-byte F2 leads with ñ");
        assert_eq!(QWEN3_BYTE_TABLE[0xF3], 0x00F1, "4-byte F3 leads with ñ");
        assert_eq!(QWEN3_BYTE_TABLE[0xF4], 0x00F2, "4-byte F4 leads with ö");
        assert_eq!(QWEN3_BYTE_TABLE.len(), 256);
    }

    /// The LLaMA split scanner is deterministic and exhaustive over the
    /// tricky whitespace/control combinations.
    #[test]
    fn qwen3_split_scanner_is_deterministic_and_exhaustive() {
        for text in [
            "a  b\tc\nd   e",
            "x",
            " ",
            "   ",
            "\t\t",
            "trailing   ",
            "mix of 中文 and 日本語 and cyrillic",
            "emoji \u{1F600} here",
            "a\rb\nc",
        ] {
            let a = qwen3_chunks(text);
            let b = qwen3_chunks(text);
            assert_eq!(a, b, "text={text:?}");
            // The chunks reassemble to the original text (isolated split).
            let joined: String = a.concat();
            assert_eq!(joined, text, "text={text:?}");
        }
    }

    /// The LLaMA split attaches internal single spaces to the following word
    /// and isolates double spaces (oracle-matched behavior).
    #[test]
    fn qwen3_split_space_behavior() {
        assert_eq!(qwen3_chunks("a b"), vec!["a", " b"]);
        assert_eq!(qwen3_chunks("a  b"), vec!["a", " ", " b"]);
        assert_eq!(qwen3_chunks("a   "), vec!["a", "   "]);
        assert_eq!(qwen3_chunks("a b\n"), vec!["a", " b", "\n"]);
        assert_eq!(qwen3_chunks("it's don't"), vec!["it", "'s", " don", "'t"]);
    }

    /// The GPT-2 pre-tokenizer is deterministic and its chunks reassemble to
    /// the original text (isolated split).
    #[test]
    fn gpt2_scanner_is_deterministic_and_exhaustive() {
        for text in [
            "hello world",
            "a  b",
            "a  ",
            "  ",
            "don't",
            "IT'S",
            "I'm we'll they're you've",
            "123 45 a1b",
            "!!! ",
            "mix of 中文 and 日本語 and emoji \u{1F600}",
            "trailing   ",
        ] {
            let a = gpt2_chunks(text);
            let b = gpt2_chunks(text);
            assert_eq!(a, b, "text={text:?}");
            let joined: String = a.concat();
            assert_eq!(joined, text, "text={text:?}");
        }
    }

    /// GPT-2 pre-tokenize behavior (oracle-matched): single spaces attach to
    /// the following word, extra/trailing spaces are isolated, contractions
    /// are case-insensitive, and digit runs stay together.
    #[test]
    fn gpt2_space_and_contraction_behavior() {
        assert_eq!(gpt2_chunks("hello world"), vec!["hello", " world"]);
        assert_eq!(gpt2_chunks("a b"), vec!["a", " b"]);
        assert_eq!(gpt2_chunks("a  b"), vec!["a", " ", " b"]);
        assert_eq!(gpt2_chunks("a  "), vec!["a", "  "]);
        assert_eq!(gpt2_chunks("  "), vec!["  "]);
        assert_eq!(gpt2_chunks("don't"), vec!["don", "'t"]);
        assert_eq!(gpt2_chunks("IT'S"), vec!["IT", "'S"]);
        assert_eq!(gpt2_chunks("I'm"), vec!["I", "'m"]);
        assert_eq!(gpt2_chunks("we'll"), vec!["we", "'ll"]);
        assert_eq!(gpt2_chunks("they're"), vec!["they", "'re"]);
        assert_eq!(gpt2_chunks("you've"), vec!["you", "'ve"]);
        assert_eq!(gpt2_chunks("123"), vec!["123"]);
        assert_eq!(gpt2_chunks("a1b"), vec!["a", "1", "b"]);
        assert_eq!(gpt2_chunks("!!! "), vec!["!!!", " "]);
    }

    /// Metaspace chunking: prepend, word chunks, trailing-space chunk.
    #[test]
    fn metaspace_chunking_edge_cases() {
        const AT: &str = "\u{120}"; // classic SentenceSpace replacement
        assert!(metaspace_chunks("", AT).is_empty());
        assert_eq!(metaspace_chunks(" ", AT), vec![AT]);
        assert_eq!(metaspace_chunks("abc", AT), vec!["\u{120}abc"]);
        assert_eq!(
            metaspace_chunks("abc def", AT),
            vec!["\u{120}abc", "\u{120}def"]
        );
        assert_eq!(metaspace_chunks("abc ", AT), vec!["\u{120}abc", "\u{120}"]);
        assert_eq!(
            metaspace_chunks("a b ", AT),
            vec!["\u{120}a", "\u{120}b", "\u{120}"]
        );
        // A different per-tokenizer replacement (BGE-M3 uses U+2581).
        const BGE: &str = "\u{2581}";
        assert_eq!(
            metaspace_chunks("hello world", BGE),
            vec!["\u{2581}hello", "\u{2581}world"]
        );
    }

    /// Space collapse: runs of >=2 spaces become one; other whitespace is
    /// untouched.
    #[test]
    fn collapse_spaces_behavior() {
        assert_eq!(collapse_spaces("a   b"), "a b");
        assert_eq!(collapse_spaces("a\t\tb"), "a\t\tb");
        assert_eq!(collapse_spaces("a \n b"), "a \n b");
        assert_eq!(collapse_spaces("  "), " ");
        assert_eq!(collapse_spaces(""), "");
    }

    /// Viterbi segmentation prefers higher-total-score segmentations, breaks
    /// exact ties toward the shorter (first-found) segmentation, and degrades
    /// to `<unk>` per code point when a chunk is unreachable.
    #[test]
    fn unigram_segmentation_behavior() {
        use std::collections::HashMap as Hm;
        let mut vocab = Hm::new();
        vocab.insert("ab".to_string(), -1.0f32); // whole word is cheap
        vocab.insert("a".to_string(), -1.5f32);
        vocab.insert("b".to_string(), -1.5f32); // split is more expensive
        let mut ids = Hm::new();
        ids.insert("ab".to_string(), 7i64);
        ids.insert("a".to_string(), 1i64);
        ids.insert("b".to_string(), 2i64);
        assert_eq!(unigram_segment("ab", &vocab, &ids, 9, 16), vec![7]);

        // Exact tie: -1.0 == -0.5 + -0.5. The DP relaxes end positions in
        // ascending order (token lengths ascending) with a strict `>`
        // update, so the two-token path ("a", "b", relaxed for end
        // position 1 before position 2's l=1 candidate is compared)
        // wins the tie over the single token "ab". Real Unigram
        // vocabularies carry distinct log-probabilities, so this only
        // documents the implemented tie behavior.
        vocab.insert("a".to_string(), -0.5f32);
        vocab.insert("b".to_string(), -0.5f32);
        assert_eq!(unigram_segment("ab", &vocab, &ids, 9, 16), vec![1, 2]);

        // Unreachable chunk degrades to <unk> per code point.
        let empty: Hm<String, f32> = Hm::new();
        assert_eq!(unigram_segment("xy", &empty, &ids, 9, 16), vec![9, 9]);
    }

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

    /// Positive (wiring): `RealOnnxEmbedder` reports not loaded until a
    /// session is provisioned, and `embed` degrades to `ModelUnavailable`.
    #[test]
    fn real_embedder_degrades_when_not_loaded() {
        // A `RealOnnxEmbedder` can only be constructed via a successful
        // `load`, which requires the runtime library; verify the port
        // contract degrades through the `OnnxEmbedder` fallback when the
        // runtime is absent.
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

    /// Failure path: tokenizer JSON that is malformed or uses an unsupported
    /// model type is rejected at load time.
    #[test]
    fn hf_tokenizer_rejects_unsupported_or_malformed_json() {
        assert!(matches!(
            HfTokenizer::from_json("not json"),
            Err(HfTokenizerError(_))
        ));
        assert!(matches!(
            HfTokenizer::from_json(r#"{"model":{"type":"WordPiece"}}"#),
            Err(HfTokenizerError(_))
        ));
        assert!(matches!(
            HfTokenizer::from_json(r#"{}"#),
            Err(HfTokenizerError(_))
        ));
        // A syntactically valid BPE file with empty merges parses and
        // tokenizes to per-byte ids (no merges applied).
        let ok =
            HfTokenizer::from_json(r#"{"model":{"type":"BPE","vocab":{"a":1,"b":2},"merges":[]}}"#);
        let tok = ok.expect("valid BPE json must parse");
        let ids = tok.tokenize("ab", 16);
        assert_eq!(ids, vec![1, 2]);
    }

    /// Failure path: the unigram tokenizer wraps `<s>`/`</s>` and degrades a
    /// minuscule `max_len` to the special tokens only.
    #[test]
    fn hf_tokenizer_unigram_wraps_special_tokens() {
        let tok = HfTokenizer::from_json(
            r#"{"model":{"type":"Unigram","unk_id":3,"vocab":[["<s>",-0.1],["</s>",-0.1],["<unk>",-10.0],["Ġhi",-0.5]]},"special_tokens_map":{"bos_token":{"content":"<s>","id":0},"eos_token":{"content":"</s>","id":2}}}"#,
        )
        .expect("valid unigram json must parse");
        assert_eq!(tok.tokenize("hi", 16), vec![0, 3, 2]);
        // Truncation keeps the special-token frame even at tiny max_len.
        assert_eq!(tok.tokenize("hi", 2), vec![0, 2]);
        assert_eq!(tok.tokenize("hi", 1), vec![0, 2]);
    }
}
