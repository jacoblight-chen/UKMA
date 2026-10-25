//! Multilingual ingestion, cross-lingual gates, the translation cache and the
//! embedding-profile generation rule (§25, ML-001..004).
//!
//! Segments keep their original byte offsets: normalisation, translation and
//! stop-word removal never destroy the position of the source characters. This
//! module identifies **scripts**, not languages; a script tag such as `und-Hans`
//! is a candidate, not a language decision.

use std::collections::BTreeMap;

use retrieval_engine;
use storage_ports::Metric;

use crate::domain::{MemoryError, Result};
pub use crate::facts::{THETA_ALIGN, THETA_CROSS};

/// Source-language weighting for an untranslated chain (§25.5 `omega_lang_src=.95`).
pub const OMEGA_SOURCE: f64 = 0.95;
/// Weighting for an unverified translation chain (§25.5 `omega_trans=.90`).
pub const OMEGA_TRANS: f64 = 0.9;
/// Translation cache lifetime in hours (§25.4 `cache_ttl=24h`).
pub const TRANSLATION_CACHE_TTL_HOURS: f64 = 24.0;
/// Laplace prior for the language preference (§25.4 `alpha=1.0`).
pub const LANGUAGE_PRIOR_ALPHA: f64 = 1.0;
/// Reference number of logical language shards (§25.3).
pub const LANGUAGE_SHARD_COUNT: usize = 4;

/// Writing system of a character run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Script {
    /// Latin.
    Latin,
    /// Han.
    Han,
    /// Hiragana.
    Hiragana,
    /// Katakana.
    Katakana,
    /// Hangul.
    Hangul,
    /// Cyrillic.
    Cyrillic,
    /// Arabic.
    Arabic,
    /// Devanagari.
    Devanagari,
    /// Thai.
    Thai,
    /// Anything else, including punctuation and digits.
    Other,
}

impl Script {
    /// BCP-47-style script subtag used as a candidate tag.
    pub fn candidate_tag(self) -> &'static str {
        match self {
            Script::Latin => "und-Latn",
            Script::Han => "und-Hans",
            Script::Hiragana => "und-Hira",
            Script::Katakana => "und-Kana",
            Script::Hangul => "und-Hang",
            Script::Cyrillic => "und-Cyrl",
            Script::Arabic => "und-Arab",
            Script::Devanagari => "und-Deva",
            Script::Thai => "und-Thai",
            Script::Other => "und-Zyyy",
        }
    }

    /// Whether the script is a CJK one that needs a dedicated analyser (§25.1).
    pub fn needs_cjk_analyzer(self) -> bool {
        matches!(self, Script::Han | Script::Hiragana | Script::Katakana)
    }
}

/// Script of one character.
pub fn script_of(character: char) -> Script {
    let code = character as u32;
    match code {
        0x0041..=0x024F | 0x1E00..=0x1EFF => Script::Latin,
        0x3040..=0x309F => Script::Hiragana,
        0x30A0..=0x30FF => Script::Katakana,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => Script::Han,
        0xAC00..=0xD7AF | 0x1100..=0x11FF => Script::Hangul,
        0x0400..=0x04FF => Script::Cyrillic,
        0x0600..=0x06FF | 0x0750..=0x077F => Script::Arabic,
        0x0900..=0x097F => Script::Devanagari,
        0x0E00..=0x0E7F => Script::Thai,
        _ => Script::Other,
    }
}

/// One script run with its original byte span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Script of the run.
    pub script: Script,
    /// Original text, unchanged.
    pub text: String,
    /// Inclusive byte offset of the first character.
    pub start: usize,
    /// Exclusive byte offset after the last character.
    pub end: usize,
}

/// Split text into script runs without losing offsets (§25.1).
pub fn segment(text: &str) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for (offset, character) in text.char_indices() {
        let script = script_of(character);
        let end = offset + character.len_utf8();
        match segments.last_mut() {
            Some(last) if last.script == script => {
                last.text.push(character);
                last.end = end;
            }
            _ => segments.push(Segment {
                script,
                text: character.to_string(),
                start: offset,
                end,
            }),
        }
    }
    segments
}

/// Whether the text mixes scripts (ML-001).
pub fn is_mixed_script(text: &str) -> bool {
    let mut seen: Vec<Script> = Vec::new();
    for segment in segment(text) {
        if segment.script == Script::Other || segment.script == Script::Latin {
            continue;
        }
        if !seen.contains(&segment.script) {
            seen.push(segment.script);
        }
    }
    seen.len() > 1 || (seen.len() == 1 && segment(text).iter().any(|s| s.script == Script::Latin))
}

/// Candidate tags of every script seen, in first-appearance order.
pub fn candidate_tags(text: &str) -> Vec<&'static str> {
    let mut tags: Vec<&'static str> = Vec::new();
    for segment in segment(text) {
        if segment.script == Script::Other {
            continue;
        }
        let tag = segment.script.candidate_tag();
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// Language relationship of a hit to the query (§24.2, §25.4).
pub use retrieval_engine::LangMatch as LanguageRelation;

/// Ranking multiplier: `1.0` same language, `0.85` verified translation, `0.425`
/// otherwise (§24.2).
pub fn lang_match_factor(relation: LanguageRelation) -> f64 {
    relation.factor()
}

/// Alignment-weighted similarity: `min(1, align/0.85)` (§24.2).
pub fn omega_align(align_score: f64) -> f64 {
    retrieval_engine::omega_align(align_score)
}

/// Result of the cross-lingual merge gate (§25.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossLingualGate {
    /// Whether the claims may be merged as one concept.
    pub merge_allowed: bool,
    /// Why the merge was refused, empty when allowed.
    pub reasons: Vec<&'static str>,
}

/// Cross-lingual merge gate: entity match, calibrated similarity, compatible
/// qualifiers and valid provenance are all required (§25.4).
pub fn cross_lingual_gate(
    entity_match: bool,
    calibrated_similarity: f64,
    qualifiers_compatible: bool,
    provenance_valid: bool,
) -> CrossLingualGate {
    let mut reasons = Vec::new();
    if !entity_match {
        reasons.push("entity_mismatch");
    }
    if !calibrated_similarity.is_finite() || calibrated_similarity < THETA_ALIGN {
        reasons.push("similarity_below_theta_align");
    }
    if !qualifiers_compatible {
        reasons.push("incompatible_qualifier");
    }
    if !provenance_valid {
        reasons.push("invalid_provenance");
    }
    CrossLingualGate {
        merge_allowed: reasons.is_empty(),
        reasons,
    }
}

/// Key of a translation-cache entry (§25.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationCacheKey {
    /// `sha256:` hash of the source text.
    pub text_hash: String,
    /// Source language.
    pub source_lang: String,
    /// Target language.
    pub target_lang: String,
    /// Translator version.
    pub translator_version: String,
    /// ACL scope, so tenants never share a cache entry.
    pub acl_scope: String,
    /// Source revision.
    pub source_revision: String,
}

impl TranslationCacheKey {
    /// Validate every component is present.
    pub fn new(
        text_hash: impl Into<String>,
        source_lang: impl Into<String>,
        target_lang: impl Into<String>,
        translator_version: impl Into<String>,
        acl_scope: impl Into<String>,
        source_revision: impl Into<String>,
    ) -> Result<Self> {
        let key = Self {
            text_hash: text_hash.into(),
            source_lang: source_lang.into(),
            target_lang: target_lang.into(),
            translator_version: translator_version.into(),
            acl_scope: acl_scope.into(),
            source_revision: source_revision.into(),
        };
        if key.text_hash.trim().is_empty()
            || key.source_lang.trim().is_empty()
            || key.target_lang.trim().is_empty()
            || key.translator_version.trim().is_empty()
            || key.acl_scope.trim().is_empty()
            || key.source_revision.trim().is_empty()
        {
            return Err(MemoryError::InvalidArgument {
                field: "translation_cache_key".to_string(),
                reason: "every component is required (§25.4)".to_string(),
            });
        }
        Ok(key)
    }

    /// Stable cache key string.
    pub fn cache_key(&self) -> String {
        format!(
            "tc:{}:{}:{}:{}:{}:{}",
            self.text_hash,
            self.source_lang,
            self.target_lang,
            self.translator_version,
            self.acl_scope,
            self.source_revision
        )
    }

    /// Whether an entry created at `created_at` is still fresh at `now`.
    pub fn is_fresh(created_at: i64, now: i64) -> bool {
        now.saturating_sub(created_at) < (TRANSLATION_CACHE_TTL_HOURS * 3_600.0) as i64
    }
}

/// Language preference with a Laplace prior (§25.4).
///
/// An explicit user setting wins over the learned prior.
pub fn preferred_language(
    counts: &BTreeMap<String, u32>,
    explicit: Option<&str>,
) -> Option<String> {
    if let Some(explicit) = explicit {
        if !explicit.trim().is_empty() {
            return Some(explicit.to_string());
        }
    }
    let total: f64 = counts.values().map(|count| f64::from(*count)).sum();
    let denominator = total + LANGUAGE_PRIOR_ALPHA * counts.len() as f64;
    if denominator <= 0.0 {
        return None;
    }
    counts
        .iter()
        .map(|(language, count)| {
            (
                language.clone(),
                (f64::from(*count) + LANGUAGE_PRIOR_ALPHA) / denominator,
            )
        })
        .max_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|(language, _)| language)
}

/// Identity of an embedding profile (§21.5, §25.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingProfileKey {
    /// Profile id.
    pub profile_id: String,
    /// Model id.
    pub model_id: String,
    /// Model revision.
    pub model_revision: String,
    /// Vector dimension.
    pub dimension: usize,
    /// Distance metric.
    pub metric: Metric,
    /// Tokenizer revision; it can change token boundaries.
    pub tokenizer_revision: String,
    /// Normalisation.
    pub normalization: String,
    /// Quantisation.
    pub quantization: String,
}

impl EmbeddingProfileKey {
    /// Whether two profiles can share one vector space.
    pub fn comparable_with(&self, other: &Self) -> bool {
        self.model_id == other.model_id
            && self.model_revision == other.model_revision
            && self.dimension == other.dimension
            && self.metric == other.metric
            && self.tokenizer_revision == other.tokenizer_revision
            && self.normalization == other.normalization
            && self.quantization == other.quantization
    }
}

/// Whether moving from `previous` to `next` requires a new index generation and
/// an atomic alias switch (§25.2, ML-003).
pub fn requires_new_generation(previous: &EmbeddingProfileKey, next: &EmbeddingProfileKey) -> bool {
    !previous.comparable_with(next)
}

/// Status of Matryoshka-dimensional truncation (§25.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationStatus {
    /// The full documented dimension.
    Full,
    /// Truncated; requires a local quality test this build cannot run (§25.2).
    RequiresLocalQualityTest,
}

/// Truncation status of a dimension for the reference 1024-dimension model.
pub fn truncation_status(dimension: usize) -> TruncationStatus {
    if dimension >= 1_024 {
        TruncationStatus::Full
    } else {
        TruncationStatus::RequiresLocalQualityTest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmentation_keeps_original_byte_offsets() {
        let text = "订单 shipped 已到";
        let segments = segment(text);
        let rebuilt: String = segments
            .iter()
            .map(|segment| segment.text.clone())
            .collect();
        assert_eq!(rebuilt, text);
        for segment in &segments {
            assert_eq!(&text[segment.start..segment.end], segment.text);
        }
        assert!(segments.iter().any(|segment| segment.script == Script::Han));
        assert!(segments
            .iter()
            .any(|segment| segment.script == Script::Latin));
    }

    #[test]
    fn mixed_scripts_are_detected_and_tagged() {
        assert!(is_mixed_script("订单 shipped"));
        assert!(!is_mixed_script("只是中文"));
        assert!(!is_mixed_script("only latin text"));
        let tags = candidate_tags("订单 shipped");
        assert_eq!(tags, vec!["und-Hans", "und-Latn"]);
        assert_eq!(Script::Han.candidate_tag(), "und-Hans");
        assert!(Script::Hiragana.needs_cjk_analyzer());
        assert!(!Script::Latin.needs_cjk_analyzer());
        assert_eq!(script_of('あ'), Script::Hiragana);
        assert_eq!(script_of('ア'), Script::Katakana);
        assert_eq!(script_of('한'), Script::Hangul);
        assert_eq!(script_of('Ж'), Script::Cyrillic);
        assert_eq!(script_of('!'), Script::Other);
    }

    #[test]
    fn language_match_factors_are_the_registered_values() {
        assert_eq!(lang_match_factor(LanguageRelation::Same), 1.0);
        assert_eq!(
            lang_match_factor(LanguageRelation::VerifiedTranslation),
            0.85
        );
        assert_eq!(lang_match_factor(LanguageRelation::Other), 0.425);
        assert_eq!(omega_align(0.85), 1.0);
        assert!((omega_align(0.425) - 0.5).abs() < 1e-12);
        assert_eq!(omega_align(-1.0), 0.0);
    }

    #[test]
    fn the_cross_lingual_gate_requires_every_condition() {
        let ok = cross_lingual_gate(true, 0.9, true, true);
        assert!(ok.merge_allowed);
        assert!(ok.reasons.is_empty());

        let negated = cross_lingual_gate(true, 0.95, false, true);
        assert!(!negated.merge_allowed);
        assert_eq!(negated.reasons, vec!["incompatible_qualifier"]);

        let weak = cross_lingual_gate(true, 0.8, true, true);
        assert_eq!(weak.reasons, vec!["similarity_below_theta_align"]);

        let all = cross_lingual_gate(false, f64::NAN, false, false);
        assert_eq!(all.reasons.len(), 4);
        assert_eq!(THETA_ALIGN, 0.85);
        assert_eq!(THETA_CROSS, 0.75);
    }

    #[test]
    fn a_translation_cache_key_carries_scope_and_revision_and_expires_in_a_day() {
        let key = TranslationCacheKey::new(
            "sha256:aa",
            "en",
            "zh-Hant",
            "translator-3",
            "tenant-a/project-b",
            "rev-7",
        )
        .unwrap();
        assert!(key.cache_key().contains("tenant-a/project-b"));
        assert!(key.cache_key().contains("rev-7"));
        assert!(TranslationCacheKey::is_fresh(0, 3_600));
        assert!(!TranslationCacheKey::is_fresh(0, 24 * 3_600));
        assert_eq!(
            TranslationCacheKey::new("", "en", "fr", "t", "a", "r")
                .unwrap_err()
                .code(),
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn the_language_prior_starts_uniform_and_prefers_explicit_settings() {
        let counts: BTreeMap<String, u32> = BTreeMap::new();
        assert_eq!(preferred_language(&counts, None), None);
        assert_eq!(
            preferred_language(&counts, Some("zh-Hant")),
            Some("zh-Hant".to_string())
        );

        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        counts.insert("en".into(), 3);
        counts.insert("zh-Hant".into(), 1);
        assert_eq!(preferred_language(&counts, None), Some("en".to_string()));
        assert_eq!(
            preferred_language(&counts, Some("zh-Hant")),
            Some("zh-Hant".to_string()),
            "the explicit preference wins"
        );
        assert_eq!(LANGUAGE_SHARD_COUNT, 4);
        assert_eq!(OMEGA_SOURCE, 0.95);
        assert_eq!(OMEGA_TRANS, 0.9);
    }

    #[test]
    fn an_incompatible_embedding_profile_requires_a_new_generation() {
        let base = EmbeddingProfileKey {
            profile_id: "bge-m3-1024".into(),
            model_id: "bge-m3".into(),
            model_revision: "1".into(),
            dimension: 1_024,
            metric: Metric::Cosine,
            tokenizer_revision: "tok-1".into(),
            normalization: "l2".into(),
            quantization: "f32".into(),
        };
        assert!(!requires_new_generation(&base, &base.clone()));

        let truncated = EmbeddingProfileKey {
            profile_id: "bge-m3-512".into(),
            dimension: 512,
            ..base.clone()
        };
        assert!(requires_new_generation(&base, &truncated));
        assert_eq!(truncation_status(1_024), TruncationStatus::Full);
        assert_eq!(
            truncation_status(512),
            TruncationStatus::RequiresLocalQualityTest
        );

        let retokenized = EmbeddingProfileKey {
            tokenizer_revision: "tok-2".into(),
            ..base.clone()
        };
        assert!(requires_new_generation(&base, &retokenized));

        let requantised = EmbeddingProfileKey {
            quantization: "q4".into(),
            ..base.clone()
        };
        assert!(requires_new_generation(&base, &requantised));
    }
}
