//! Memory retrieval: strict filtering, BM25 + optional vector channels, RRF
//! fusion and the memory-domain final score (§24.1–§24.2).
//!
//! A strategy that could not run is reported in `skipped`, never disguised as a
//! zero-result strategy, and a missing embedding model makes the response
//! `partial` (§28.3 fault table).

use std::collections::BTreeMap;

use embedding_engine::EmbeddingPort;
use retrieval_engine::{Bm25Index, Fused, LangMatch};

use crate::domain::{DetailLevel, MemoryError, MemoryKind, MemoryQuery, Result};
use crate::time::format_rfc3339;

/// Largest number of candidates a single search scans (§31.4 resource limit).
pub const MAX_CANDIDATES: usize = 10_000;
/// Largest `max_results` a caller may ask for.
pub const MAX_RESULTS: u32 = 200;
/// Token ceiling applied to a response.
pub const MAX_RESPONSE_TOKENS: u32 = 100_000;

/// A memory document prepared for retrieval.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchDocument {
    /// Memory id.
    pub id: String,
    /// Store the document lives in.
    pub kind: MemoryKind,
    /// Searchable text (summary plus body).
    pub text: String,
    /// Creation instant.
    pub created_at: i64,
    /// Ranking trust in `[0,1]`.
    pub trust: f64,
    /// Importance in `[0,1]`.
    pub importance: f64,
    /// BCP-47 language.
    pub language: String,
    /// Whether the document is a folded derivation.
    pub folded: bool,
    /// Token count of the text.
    pub token_count: u32,
}

/// What the search actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelReport {
    /// Strategies that ran.
    pub executed: Vec<&'static str>,
    /// Strategies that could not run, with the reason.
    pub skipped: Vec<(&'static str, String)>,
    /// Factors that replaced a missing input, with the value used.
    pub defaults: Vec<(&'static str, String)>,
}

impl ChannelReport {
    /// Whether any strategy was skipped.
    pub fn is_partial(&self) -> bool {
        !self.skipped.is_empty()
    }
}

/// One ranked hit.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedHit {
    /// Memory id.
    pub id: String,
    /// Final score.
    pub score: f64,
    /// Rank per contributing channel.
    pub channel_ranks: BTreeMap<String, usize>,
}

/// Outcome of a search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    /// Hits, best first.
    pub hits: Vec<RankedHit>,
    /// Channel report.
    pub report: ChannelReport,
    /// Ids dropped because the token budget ran out.
    pub omitted_for_budget: usize,
}

/// Lower-cased tokenizer: alphanumeric runs plus single CJK characters.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if is_cjk(character) {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push(character.to_string());
        } else if character.is_alphanumeric() {
            for lowered in character.to_lowercase() {
                current.push(lowered);
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_cjk(character: char) -> bool {
    matches!(character as u32,
        0x3040..=0x30FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF)
}

/// Validate a query before doing any work.
pub fn validate_query(query: &MemoryQuery) -> Result<()> {
    if query.query.trim().is_empty() {
        return Err(MemoryError::InvalidArgument {
            field: "query".to_string(),
            reason: "must be non-empty".to_string(),
        });
    }
    if query.max_results == 0 || query.max_results > MAX_RESULTS {
        return Err(MemoryError::InvalidArgument {
            field: "max_results".to_string(),
            reason: format!("must be in 1..={MAX_RESULTS}"),
        });
    }
    if query.max_tokens == 0 {
        return Err(MemoryError::InvalidArgument {
            field: "max_tokens".to_string(),
            reason: "must be greater than zero".to_string(),
        });
    }
    if let Some(min_trust) = query.min_trust {
        if !min_trust.is_finite() || !(0.0..=1.0).contains(&min_trust) {
            return Err(MemoryError::InvalidArgument {
                field: "min_trust".to_string(),
                reason: format!("must be in [0,1], got {min_trust}"),
            });
        }
    }
    for text in [&query.after, &query.before].into_iter().flatten() {
        crate::time::parse_rfc3339(text)?;
    }
    Ok(())
}

fn passes_filter(
    query: &MemoryQuery,
    document: &SearchDocument,
    after: Option<i64>,
    before: Option<i64>,
) -> bool {
    if !query.kinds.is_empty() && !query.kinds.contains(&document.kind) {
        return false;
    }
    if let Some(language) = &query.language {
        if &document.language != language {
            return false;
        }
    }
    if let Some(after) = after {
        if document.created_at < after {
            return false;
        }
    }
    if let Some(before) = before {
        if document.created_at > before {
            return false;
        }
    }
    if let Some(min_trust) = query.min_trust {
        if document.trust < min_trust {
            return false;
        }
    }
    true
}

/// Run the query over the candidate documents.
pub fn search(
    query: &MemoryQuery,
    documents: &[SearchDocument],
    embedder: Option<&dyn EmbeddingPort>,
    profile_id: &str,
    now: i64,
) -> Result<SearchOutcome> {
    validate_query(query)?;
    let after = match &query.after {
        Some(text) => Some(crate::time::parse_rfc3339(text)?),
        None => None,
    };
    let before = match &query.before {
        Some(text) => Some(crate::time::parse_rfc3339(text)?),
        None => None,
    };

    let filtered: Vec<&SearchDocument> = documents
        .iter()
        .filter(|document| passes_filter(query, document, after, before))
        .collect();
    if filtered.len() > MAX_CANDIDATES {
        return Err(MemoryError::ResourceLimit {
            detail: format!(
                "{} candidates exceed the {MAX_CANDIDATES} candidate limit",
                filtered.len()
            ),
        });
    }
    if filtered.is_empty() {
        return Ok(SearchOutcome {
            hits: Vec::new(),
            report: ChannelReport {
                executed: vec!["lexical"],
                skipped: Vec::new(),
                defaults: Vec::new(),
            },
            omitted_for_budget: 0,
        });
    }

    let mut report = ChannelReport::default();
    let query_tokens = tokenize(&query.query);

    // Channel 1: BM25 over the filtered corpus.
    let corpus: Vec<Vec<String>> = filtered
        .iter()
        .map(|document| tokenize(&document.text))
        .collect();
    let index = Bm25Index::build(&corpus);
    let lexical_ranked =
        index
            .rank(&query_tokens)
            .map_err(|detail| MemoryError::InvalidArgument {
                field: "query".to_string(),
                reason: detail,
            })?;
    let lexical_ids: Vec<String> = lexical_ranked
        .iter()
        .filter(|(_, score)| *score > 0.0)
        .map(|(position, _)| filtered[*position].id.clone())
        .collect();
    report.executed.push("lexical");

    // Channel 2: vector similarity, only when a model profile is available.
    let mut vector_ids: Vec<String> = Vec::new();
    match embedder {
        None => report.skipped.push((
            "vector",
            "no embedding port configured; lexical channel is authoritative".to_string(),
        )),
        Some(port) => {
            let texts: Vec<String> = filtered
                .iter()
                .map(|document| document.text.clone())
                .collect();
            match port.embed(profile_id, &texts) {
                Ok(vectors) => {
                    let query_text = vec![query.query.clone()];
                    match port.embed(profile_id, &query_text) {
                        Ok(query_vectors) if query_vectors.len() == 1 => {
                            let query_vector = &query_vectors[0];
                            let mut scored: Vec<(usize, f64)> = Vec::new();
                            for (position, vector) in vectors.iter().enumerate() {
                                match retrieval_engine::cosine(query_vector, vector) {
                                    Ok(score) => scored.push((position, score)),
                                    Err(_) => {
                                        report.skipped.push((
                                            "vector",
                                            "embedding dimensions disagree with the query".into(),
                                        ));
                                        break;
                                    }
                                }
                            }
                            scored.sort_by(|left, right| {
                                right
                                    .1
                                    .partial_cmp(&left.1)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                                    .then_with(|| left.0.cmp(&right.0))
                            });
                            vector_ids = scored
                                .into_iter()
                                .filter(|(_, score)| *score > 0.0)
                                .map(|(position, _)| filtered[position].id.clone())
                                .collect();
                            report.executed.push("vector");
                        }
                        Ok(_) => report.skipped.push((
                            "vector",
                            "embedding port returned an unexpected number of query vectors".into(),
                        )),
                        Err(error) => report
                            .skipped
                            .push(("vector", format!("MODEL_UNAVAILABLE: {error}"))),
                    }
                }
                Err(error) => report
                    .skipped
                    .push(("vector", format!("MODEL_UNAVAILABLE: {error}"))),
            }
        }
    }

    let mut channels: Vec<(String, Vec<String>)> = vec![("lexical".to_string(), lexical_ids)];
    if !vector_ids.is_empty() {
        channels.push(("vector".to_string(), vector_ids));
    }
    let fused: Vec<Fused> = retrieval_engine::rrf(&channels);

    report.defaults.push((
        "novelty_bonus",
        "no novelty model in this build; factor recorded as 1.0".to_string(),
    ));
    let lang_match = match &query.language {
        Some(language)
            if filtered
                .iter()
                .any(|document| &document.language == language) =>
        {
            LangMatch::Same
        }
        Some(_) => LangMatch::Other,
        None => LangMatch::Same,
    };

    let mut ranked: Vec<RankedHit> = Vec::new();
    for hit in fused {
        let document = filtered
            .iter()
            .find(|document| document.id == hit.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("search result {}", hit.id),
            })?;
        let recency =
            retrieval_engine::recency(now.saturating_sub(document.created_at) as f64 / 86_400.0);
        let score = retrieval_engine::final_score(
            hit.score,
            document.importance,
            recency,
            document.trust,
            1.0,
            lang_match.factor(),
        )
        .map_err(|reason| MemoryError::InvalidArgument {
            field: "score".to_string(),
            reason,
        })?;
        ranked.push(RankedHit {
            id: hit.id,
            score,
            channel_ranks: hit.channel_ranks,
        });
    }
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });

    // Progressive detail: trim to the request's result and token ceilings.
    let token_ceiling = query.max_tokens.min(MAX_RESPONSE_TOKENS);
    let detail_multiplier = match query.detail {
        DetailLevel::Title => 1,
        DetailLevel::Facts => 2,
        DetailLevel::Summary => 4,
        DetailLevel::Full => 1,
    };
    let mut used = 0u32;
    let mut kept = Vec::new();
    let mut omitted = 0usize;
    for hit in ranked {
        let document = filtered
            .iter()
            .find(|document| document.id == hit.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("search result {}", hit.id),
            })?;
        let cost = match query.detail {
            DetailLevel::Full => document.token_count,
            _ => (document.token_count / detail_multiplier).max(1),
        };
        if used.saturating_add(cost) > token_ceiling {
            omitted += 1;
            continue;
        }
        used += cost;
        kept.push(hit);
        if kept.len() == query.max_results as usize {
            break;
        }
    }
    if omitted > 0 {
        report.skipped.push((
            "token_budget",
            format!("{omitted} hits omitted to stay inside {token_ceiling} tokens"),
        ));
    }
    Ok(SearchOutcome {
        hits: kept,
        report,
        omitted_for_budget: omitted,
    })
}

/// Human-readable instant used in reports.
pub fn describe_instant(instant: i64) -> String {
    format_rfc3339(instant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedding_engine::HashEmbedder;

    fn document(id: &str, text: &str, created_at: i64) -> SearchDocument {
        SearchDocument {
            id: id.to_string(),
            kind: MemoryKind::Episodic,
            text: text.to_string(),
            created_at,
            trust: 0.8,
            importance: 0.7,
            language: "en".to_string(),
            folded: false,
            token_count: tokenize(text).len() as u32,
        }
    }

    fn query(text: &str) -> MemoryQuery {
        MemoryQuery {
            query: text.to_string(),
            kinds: Vec::new(),
            language: None,
            after: None,
            before: None,
            min_trust: None,
            max_results: 10,
            detail: DetailLevel::Full,
            max_tokens: 10_000,
        }
    }

    #[test]
    fn tokenizer_splits_words_and_single_cjk_characters() {
        assert_eq!(tokenize("Hello, World!"), vec!["hello", "world"]);
        assert_eq!(tokenize("訂單 Order"), vec!["訂", "單", "order"]);
        assert_eq!(tokenize("  "), Vec::<String>::new());
        assert_eq!(tokenize("A1_b2"), vec!["a1", "b2"]);
    }

    #[test]
    fn query_validation_rejects_unbounded_requests() {
        let mut bad = query("x");
        bad.max_results = 0;
        assert_eq!(validate_query(&bad).unwrap_err().code(), "INVALID_ARGUMENT");
        let mut bad = query("x");
        bad.max_results = MAX_RESULTS + 1;
        assert!(validate_query(&bad).is_err());
        let bad = query("  ");
        assert!(validate_query(&bad).is_err());
        let mut bad = query("x");
        bad.max_tokens = 0;
        assert!(validate_query(&bad).is_err());
        let mut bad = query("x");
        bad.min_trust = Some(1.5);
        assert!(validate_query(&bad).is_err());
        let mut bad = query("x");
        bad.after = Some("yesterday".into());
        assert!(validate_query(&bad).is_err());
    }

    #[test]
    fn lexical_channel_ranks_the_matching_document_first() {
        let documents = vec![
            document("m1", "alpha beta", 0),
            document("m2", "unrelated content", 0),
            document("m3", "alpha alpha", 0),
        ];
        let outcome = search(&query("alpha"), &documents, None, "profile", 0).unwrap();
        assert_eq!(outcome.hits[0].id, "m3");
        assert_eq!(outcome.hits[1].id, "m1");
        assert_eq!(
            outcome.hits.len(),
            2,
            "a document without the term is absent"
        );
        assert!(outcome
            .report
            .skipped
            .iter()
            .any(|(name, _)| *name == "vector"));
    }

    #[test]
    fn filters_run_before_ranking() {
        let documents = vec![
            document("in-scope", "alpha", 100),
            SearchDocument {
                kind: MemoryKind::Semantic,
                ..document("wrong-kind", "alpha alpha alpha", 100)
            },
            SearchDocument {
                trust: 0.1,
                ..document("low-trust", "alpha alpha", 100)
            },
            document("too-old", "alpha", 1),
        ];
        let mut request = query("alpha");
        request.kinds = vec![MemoryKind::Episodic];
        request.min_trust = Some(0.5);
        request.after = Some(format_rfc3339(50));
        let outcome = search(&request, &documents, None, "profile", 200).unwrap();
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].id, "in-scope");
    }

    #[test]
    fn a_language_filter_narrows_the_candidate_set() {
        let documents = vec![
            document("en", "alpha", 0),
            SearchDocument {
                language: "zh-Hant".to_string(),
                ..document("zh", "alpha", 0)
            },
        ];
        let all = search(&query("alpha"), &documents, None, "profile", 0).unwrap();
        let mut request = query("alpha");
        request.language = Some("en".to_string());
        let filtered = search(&request, &documents, None, "profile", 0).unwrap();
        assert_eq!(all.hits.len(), 2);
        assert_eq!(filtered.hits.len(), 1);
        assert_eq!(filtered.hits[0].id, "en");
    }

    #[test]
    fn a_missing_embedding_model_is_reported_as_partial() {
        let documents = vec![document("m1", "alpha", 0)];
        let outcome = search(&query("alpha"), &documents, None, "profile", 0).unwrap();
        assert!(outcome.report.is_partial());
        let reason = &outcome.report.skipped[0].1;
        assert!(reason.contains("lexical channel is authoritative"));
    }

    #[test]
    fn the_vector_channel_runs_when_a_profile_exists() {
        let embedder = HashEmbedder::new(64);
        let documents = vec![document("m1", "alpha beta", 0), document("m2", "gamma", 0)];
        let outcome = search(&query("alpha"), &documents, Some(&embedder), "p", 0).unwrap();
        assert!(outcome.report.executed.contains(&"vector"));
        assert!(!outcome.report.is_partial());
        assert_eq!(outcome.hits[0].id, "m1");
    }

    #[test]
    fn the_token_budget_trims_results_and_says_so() {
        let documents: Vec<SearchDocument> = (0..5)
            .map(|index| document(&format!("m{index}"), "alpha beta gamma", 0))
            .collect();
        let mut request = query("alpha");
        request.max_tokens = 4;
        let outcome = search(&request, &documents, None, "profile", 0).unwrap();
        assert!(outcome.hits.len() < documents.len());
        assert!(outcome.omitted_for_budget > 0);
        assert!(outcome
            .report
            .skipped
            .iter()
            .any(|(name, _)| *name == "token_budget"));
    }

    #[test]
    fn max_results_is_respected() {
        let documents: Vec<SearchDocument> = (0..6)
            .map(|index| document(&format!("m{index}"), "alpha", 0))
            .collect();
        let mut request = query("alpha");
        request.max_results = 2;
        let outcome = search(&request, &documents, None, "profile", 0).unwrap();
        assert_eq!(outcome.hits.len(), 2);
    }

    #[test]
    fn an_empty_filtered_set_returns_no_hits_without_error() {
        let documents = vec![document("m1", "alpha", 0)];
        let mut request = query("alpha");
        request.kinds = vec![MemoryKind::Semantic];
        let outcome = search(&request, &documents, None, "profile", 0).unwrap();
        assert!(outcome.hits.is_empty());
        assert!(outcome.report.executed.contains(&"lexical"));
    }

    #[test]
    fn novelty_default_is_recorded() {
        let documents = vec![document("m1", "alpha", 0)];
        let outcome = search(&query("alpha"), &documents, None, "profile", 0).unwrap();
        assert!(outcome
            .report
            .defaults
            .iter()
            .any(|(name, _)| *name == "novelty_bonus"));
        assert_eq!(describe_instant(0), "1970-01-01T00:00:00Z");
    }
}
