//! Retrieval core: RRF fusion, PPR graph ranking, recency, language match and
//! the five-strategy query planner (§7.3, §24).
//!
//! Every constant here is a registered default of §29.2. Raw channel scores are
//! never summed directly across channels (RE-003): fusion happens on ranks.

pub mod hnsw;
pub mod vector_port;

use std::collections::BTreeMap;

/// RRF constant `k` (§29.2 `RRF_k=60`).
pub const RRF_K: u32 = 60;
/// PPR damping factor (§24.2 `a=0.6`).
pub const PPR_ALPHA: f64 = 0.6;
/// PPR iteration cap (§24.2).
pub const PPR_MAX_ITER: usize = 200;
/// PPR L1 convergence threshold (§24.2).
pub const PPR_L1_EPSILON: f64 = 1e-6;
/// Hub dampening limit (§24.2 `hub_limit=50`).
pub const HUB_LIMIT: f64 = 50.0;
/// Recency decay per day (§29.2 `recency_lambda=.05/day`).
pub const RECENCY_LAMBDA: f64 = 0.05;
/// Cross-lingual alignment threshold (§25.4 `theta_align=.85`).
pub const ALIGN_THETA: f64 = 0.85;
/// BM25 term-frequency saturation (§29.2 `BM25_k1=1.5`).
pub const BM25_K1: f64 = 1.5;
/// BM25 length normalisation (§29.2 `BM25_b=.75`).
pub const BM25_B: f64 = 0.75;

/// BM25 over a fixed, already-tokenised corpus (§24.2).
///
/// `score(D,Q) = sum IDF(q_i) * f_i*(k1+1) / (f_i + k1*(1-b+b*|D|/avgdl))`.
/// The IDF variant is the standard non-negative `ln(1 + (N-n+0.5)/(n+0.5))`;
/// §24.2 registers only `k1` and `b`, so the variant is recorded in ADR-051.
#[derive(Debug, Clone, Default)]
pub struct Bm25Index {
    docs: Vec<Vec<String>>,
    doc_freq: BTreeMap<String, usize>,
    avgdl: f64,
}

impl Bm25Index {
    /// Build an index over the corpus.
    pub fn build(corpus: &[Vec<String>]) -> Self {
        let mut doc_freq: BTreeMap<String, usize> = BTreeMap::new();
        let mut total = 0usize;
        for doc in corpus {
            let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
            for term in doc {
                seen.insert(term.as_str(), ());
            }
            for term in seen.keys() {
                *doc_freq.entry((*term).to_string()).or_insert(0) += 1;
            }
            total += doc.len();
        }
        let avgdl = if corpus.is_empty() {
            0.0
        } else {
            total as f64 / corpus.len() as f64
        };
        Self {
            docs: corpus.to_vec(),
            doc_freq,
            avgdl,
        }
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether the corpus is empty.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Inverse document frequency of one term.
    pub fn idf(&self, term: &str) -> f64 {
        let total = self.docs.len() as f64;
        let frequency = *self.doc_freq.get(term).unwrap_or(&0) as f64;
        (1.0 + (total - frequency + 0.5) / (frequency + 0.5)).ln()
    }

    /// Score one document; `doc_index` out of range is an error.
    pub fn score(&self, query: &[String], doc_index: usize) -> Result<f64, String> {
        let doc = self
            .docs
            .get(doc_index)
            .ok_or_else(|| format!("document index {doc_index} is out of range"))?;
        if self.avgdl <= 0.0 {
            return Ok(0.0);
        }
        let dl = doc.len() as f64;
        let mut total = 0.0;
        for term in query {
            let tf = doc.iter().filter(|token| *token == term).count();
            if tf == 0 {
                continue;
            }
            let f = tf as f64;
            let denominator = f + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / self.avgdl);
            if denominator <= 0.0 {
                continue;
            }
            total += self.idf(term) * f * (BM25_K1 + 1.0) / denominator;
        }
        if total.is_finite() {
            Ok(total)
        } else {
            Err("non-finite BM25 score".to_string())
        }
    }

    /// Rank all documents, best first, ties broken by document index.
    pub fn rank(&self, query: &[String]) -> Result<Vec<(usize, f64)>, String> {
        let mut scored: Vec<(usize, f64)> = Vec::with_capacity(self.docs.len());
        for index in 0..self.docs.len() {
            scored.push((index, self.score(query, index)?));
        }
        scored.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(scored)
    }
}

/// Cosine similarity of two vectors (§24.2 `score_vec`).
///
/// Mismatched dimensions are an error: vectors from different embedding
/// profiles are never compared directly.
pub fn cosine(left: &[f32], right: &[f32]) -> Result<f64, String> {
    if left.len() != right.len() {
        return Err(format!(
            "dimension mismatch: {} vs {}",
            left.len(),
            right.len()
        ));
    }
    if left.is_empty() {
        return Err("empty vectors".to_string());
    }
    let mut dot = 0.0f64;
    let mut left_norm = 0.0f64;
    let mut right_norm = 0.0f64;
    for (a, b) in left.iter().zip(right.iter()) {
        let a = f64::from(*a);
        let b = f64::from(*b);
        if !a.is_finite() || !b.is_finite() {
            return Err("non-finite vector component".to_string());
        }
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return Ok(0.0);
    }
    let similarity = dot / (left_norm.sqrt() * right_norm.sqrt());
    Ok(similarity.clamp(-1.0, 1.0))
}

/// One fused result.
#[derive(Debug, Clone, PartialEq)]
pub struct Fused {
    /// Document/entity id.
    pub id: String,
    /// Reciprocal-rank fusion score.
    pub score: f64,
    /// Rank in each contributing channel (1-based).
    pub channel_ranks: BTreeMap<String, usize>,
}

fn accumulate(channels: &[(String, Vec<String>, f64)]) -> Result<Vec<Fused>, String> {
    let mut accumulator: BTreeMap<String, (f64, BTreeMap<String, usize>)> = BTreeMap::new();
    for (name, ranked, weight) in channels {
        if !weight.is_finite() || *weight < 0.0 {
            return Err(format!("channel `{name}` has invalid weight {weight}"));
        }
        for (position, id) in ranked.iter().enumerate() {
            let rank = position + 1;
            let contribution = weight / (f64::from(RRF_K) + rank as f64);
            let entry = accumulator
                .entry(id.clone())
                .or_insert_with(|| (0.0, BTreeMap::new()));
            entry.0 += contribution;
            entry.1.insert(name.clone(), rank);
        }
    }
    let mut fused: Vec<Fused> = accumulator
        .into_iter()
        .map(|(id, (score, channel_ranks))| Fused {
            id,
            score,
            channel_ranks,
        })
        .collect();
    fused.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(fused)
}

/// Reciprocal-rank fusion with one unweighted channel per slice.
///
/// `RRF(d) = sum 1/(k + rank(d))`, ranks start at 1, absent channels contribute
/// nothing (§24.2).
pub fn rrf(channels: &[(String, Vec<String>)]) -> Vec<Fused> {
    let weighted: Vec<(String, Vec<String>, f64)> = channels
        .iter()
        .map(|(name, ranked)| (name.clone(), ranked.clone(), 1.0))
        .collect();
    accumulate(&weighted).unwrap_or_default()
}

/// Weighted RRF; a negative or non-finite weight is a caller bug and is
/// reported rather than silently clamped.
pub fn weighted_rrf(channels: &[(String, Vec<String>, f64)]) -> Result<Vec<Fused>, String> {
    accumulate(channels)
}

/// Recency multiplier: `exp(-0.05 * max(0, delta_days))` (§24.2).
pub fn recency(delta_days: f64) -> f64 {
    let days = if delta_days.is_finite() {
        delta_days.max(0.0)
    } else {
        0.0
    };
    (-RECENCY_LAMBDA * days).exp()
}

/// Language relationship of a hit to the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangMatch {
    /// Same language (`1.0`).
    Same,
    /// Verified cross-lingual translation (`0.85`).
    VerifiedTranslation,
    /// Other (`0.425`).
    Other,
}

impl LangMatch {
    /// Multiplier per §24.2.
    pub fn factor(self) -> f64 {
        match self {
            LangMatch::Same => 1.0,
            LangMatch::VerifiedTranslation => 0.85,
            LangMatch::Other => 0.425,
        }
    }
}

/// Cross-lingual alignment weight: `min(1, align_score / 0.85)` (§24.2).
pub fn omega_align(align_score: f64) -> f64 {
    if !align_score.is_finite() || align_score <= 0.0 {
        return 0.0;
    }
    (align_score / ALIGN_THETA).min(1.0)
}

/// Final memory-domain ranking (§24.2):
/// `RRF * importance^1.0 * recency^0.5 * trust^0.8 * novelty^0.2 * lang^0.6`.
///
/// All multipliers must be finite and non-negative; an invalid input is an
/// error, never a silent `1.0` (avoids `0^0` and NaN propagation).
pub fn final_score(
    rrf_score: f64,
    importance: f64,
    recency_factor: f64,
    trust: f64,
    novelty: f64,
    lang: f64,
) -> Result<f64, String> {
    let inputs = [
        ("rrf", rrf_score),
        ("importance", importance),
        ("recency", recency_factor),
        ("trust", trust),
        ("novelty", novelty),
        ("lang", lang),
    ];
    for (name, value) in inputs {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{name} must be finite and >= 0, got {value}"));
        }
    }
    Ok(rrf_score
        * importance
        * recency_factor.powf(0.5)
        * trust.powf(0.8)
        * novelty.powf(0.2)
        * lang.powf(0.6))
}

/// Graph adjacency used by PPR.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    /// `edges[i]` = `(target_index, weight)`.
    pub edges: Vec<Vec<(usize, f64)>>,
}

impl Graph {
    /// Build from a unit-weight edge list; out-of-range edges are ignored.
    pub fn from_edges(node_count: usize, edges: &[(usize, usize)]) -> Self {
        let mut adjacency = vec![Vec::new(); node_count];
        for (from, to) in edges {
            if *from < node_count && *to < node_count {
                adjacency[*from].push((*to, 1.0));
            }
        }
        Self { edges: adjacency }
    }

    /// Number of nodes.
    pub fn node_count(&self) -> usize {
        self.edges.len()
    }
}

/// Result of a personalized-PageRank run.
#[derive(Debug, Clone, PartialEq)]
pub struct PprResult {
    /// Normalised score per node.
    pub scores: Vec<f64>,
    /// Iterations actually executed.
    pub iterations: usize,
    /// `true` when the L1 delta never reached the tolerance (§24.2).
    pub not_converged: bool,
}

/// Personalized PageRank with dangling-mass re-injection (§24.2).
///
/// Seeds are `max(cosine, 0)^2` normalised; hub out-edges are dampened by
/// `min(1, 50 / max(1, degree))`; convergence is `L1 < 1e-6`.
pub fn ppr(graph: &Graph, seeds: &[f64]) -> Result<PprResult, String> {
    let n = graph.node_count();
    if n == 0 {
        return Ok(PprResult {
            scores: Vec::new(),
            iterations: 0,
            not_converged: false,
        });
    }
    if seeds.len() != n {
        return Err(format!(
            "seed vector length {} != node count {n}",
            seeds.len()
        ));
    }
    let mut normalised = vec![0.0f64; n];
    let mut total = 0.0f64;
    for (index, seed) in seeds.iter().enumerate() {
        let value = if seed.is_finite() {
            seed.max(0.0).powi(2)
        } else {
            0.0
        };
        normalised[index] = value;
        total += value;
    }
    if total <= 0.0 {
        for value in normalised.iter_mut() {
            *value = 1.0 / n as f64;
        }
    } else {
        for value in normalised.iter_mut() {
            *value /= total;
        }
    }

    let mut rank = normalised.clone();
    let mut iterations = 0usize;
    let mut converged = false;
    while iterations < PPR_MAX_ITER {
        let mut next = vec![0.0f64; n];
        let mut dangling_mass = 0.0f64;
        for (node, neighbours) in graph.edges.iter().enumerate() {
            if neighbours.is_empty() {
                dangling_mass += rank[node];
                continue;
            }
            let out_degree = neighbours.len();
            let hub_factor = HUB_LIMIT / (out_degree as f64).max(1.0);
            let share = rank[node] * hub_factor.min(1.0) / out_degree as f64;
            for (target, weight) in neighbours {
                next[*target] += share * weight;
            }
        }
        for node in 0..n {
            next[node] = (1.0 - PPR_ALPHA) * normalised[node]
                + PPR_ALPHA * next[node]
                + PPR_ALPHA * dangling_mass * normalised[node];
        }
        let delta: f64 = next
            .iter()
            .zip(rank.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        rank = next;
        iterations += 1;
        if delta < PPR_L1_EPSILON {
            converged = true;
            break;
        }
    }
    Ok(PprResult {
        scores: rank,
        iterations,
        not_converged: !converged,
    })
}

/// A retrieval strategy (§24.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strategy {
    /// Dense vector search.
    Vector,
    /// Graph traversal / PPR.
    Graph,
    /// Time-ordered scan.
    Temporal,
    /// BM25 full text.
    Bm25,
    /// Cross-lingual lookup.
    CrossLingual,
}

impl Strategy {
    /// All strategies in planner order.
    pub const ALL: [Strategy; 5] = [
        Strategy::Vector,
        Strategy::Graph,
        Strategy::Temporal,
        Strategy::Bm25,
        Strategy::CrossLingual,
    ];

    /// Stable name used in responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Vector => "vector",
            Strategy::Graph => "graph",
            Strategy::Temporal => "temporal",
            Strategy::Bm25 => "bm25",
            Strategy::CrossLingual => "cross_language",
        }
    }
}

/// What the planner decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Strategies that will execute.
    pub executed: Vec<Strategy>,
    /// Strategies skipped, with a reason (never silently dropped, §24.1).
    pub skipped: Vec<(Strategy, &'static str)>,
    /// Whether the response must be marked `partial`.
    pub partial: bool,
}

impl Plan {
    /// Whether no strategy will run.
    pub fn is_empty(&self) -> bool {
        self.executed.is_empty()
    }
}

/// Query/adapter hints used by the planner.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryHints {
    /// The query contains an exact identifier (recency may be bypassed).
    pub exact_identifier: bool,
    /// A vector channel is available.
    pub vector_available: bool,
    /// A graph channel is available.
    pub graph_available: bool,
    /// A temporal index is available.
    pub temporal_available: bool,
    /// A BM25 index is available.
    pub bm25_available: bool,
    /// A cross-lingual index is available.
    pub cross_lingual_available: bool,
    /// The query mixes languages.
    pub multilingual: bool,
}

/// Plan which strategies run (§24.1): an unavailable channel is *reported*,
/// never disguised as an empty result.
pub fn plan(hints: QueryHints) -> Plan {
    let mut executed = Vec::new();
    let mut skipped = Vec::new();
    let mut consider = |strategy: Strategy, available: bool| {
        if available {
            executed.push(strategy);
        } else {
            skipped.push((strategy, "capability unavailable on the active adapter"));
        }
    };
    consider(Strategy::Vector, hints.vector_available);
    consider(Strategy::Graph, hints.graph_available);
    consider(Strategy::Temporal, hints.temporal_available);
    consider(Strategy::Bm25, hints.bm25_available);
    if hints.multilingual {
        consider(Strategy::CrossLingual, hints.cross_lingual_available);
    } else {
        skipped.push((Strategy::CrossLingual, "query is not multilingual"));
    }
    let partial = !skipped.is_empty();
    Plan {
        executed,
        skipped,
        partial,
    }
}

/// Progressive detail tier (§24.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTier {
    /// High relevance: full text.
    Full,
    /// Medium: summary + key facts.
    Summary,
    /// Low: principal facts.
    Facts,
    /// Marginal: title/date only.
    Identification,
}

impl DetailTier {
    /// Token target range per tier (§24.3).
    pub fn token_range(self) -> (u32, u32) {
        match self {
            DetailTier::Full => (500, 2000),
            DetailTier::Summary => (100, 300),
            DetailTier::Facts => (30, 80),
            DetailTier::Identification => (10, 20),
        }
    }
}

/// Tier from a **calibrated** relevance score in `[0,1]` (§24.3).
///
/// Raw RRF scores are not calibrated and MUST NOT be passed here (RE-003).
pub fn detail_tier(calibrated_relevance: f64) -> Result<DetailTier, String> {
    if !calibrated_relevance.is_finite() || !(0.0..=1.0).contains(&calibrated_relevance) {
        return Err(format!(
            "relevance must be a calibrated value in [0,1], got {calibrated_relevance}"
        ));
    }
    Ok(if calibrated_relevance > 0.90 {
        DetailTier::Full
    } else if calibrated_relevance >= 0.70 {
        DetailTier::Summary
    } else if calibrated_relevance >= 0.50 {
        DetailTier::Facts
    } else {
        DetailTier::Identification
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str, ids: &[&str]) -> (String, Vec<String>) {
        (
            name.to_string(),
            ids.iter().map(|id| id.to_string()).collect(),
        )
    }

    #[test]
    fn rrf_matches_the_formula() {
        let fused = rrf(&[channel("a", &["x", "y"]), channel("b", &["y", "x"])]);
        let expected = 1.0 / (60.0 + 1.0) + 1.0 / (60.0 + 2.0);
        assert_eq!(fused.len(), 2);
        for entry in &fused {
            assert!((entry.score - expected).abs() < 1e-12);
            assert_eq!(entry.channel_ranks.len(), 2);
        }
        assert_eq!(fused[0].id, "x", "ties break deterministically by id");
    }

    #[test]
    fn rrf_rewards_agreement_across_channels() {
        let fused = rrf(&[
            channel("a", &["both", "only-a"]),
            channel("b", &["both", "only-b"]),
        ]);
        assert_eq!(fused[0].id, "both");
        assert!(fused[0].score > fused[1].score);
    }

    #[test]
    fn weighted_rrf_rejects_negative_and_non_finite_weights() {
        assert!(weighted_rrf(&[("a".into(), vec!["x".into()], -1.0)])
            .unwrap_err()
            .contains("invalid weight"));
        assert!(weighted_rrf(&[("a".into(), vec!["x".into()], f64::NAN)])
            .unwrap_err()
            .contains("invalid weight"));
        assert_eq!(
            weighted_rrf(&[("a".into(), vec!["x".into()], 0.0)]).unwrap()[0].score,
            0.0
        );
    }

    #[test]
    fn recency_decays_and_clamps_future_timestamps() {
        assert!((recency(0.0) - 1.0).abs() < 1e-12);
        assert!((recency(10.0) - (-0.5f64).exp()).abs() < 1e-12);
        assert!(
            (recency(-5.0) - 1.0).abs() < 1e-12,
            "future timestamps clamp to 0 days"
        );
        assert!((recency(f64::NAN) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn language_multipliers_match_the_table() {
        assert_eq!(LangMatch::Same.factor(), 1.0);
        assert_eq!(LangMatch::VerifiedTranslation.factor(), 0.85);
        assert_eq!(LangMatch::Other.factor(), 0.425);
    }

    #[test]
    fn omega_align_is_capped_at_one() {
        assert!((omega_align(0.85) - 1.0).abs() < 1e-12);
        assert!((omega_align(1.0) - 1.0).abs() < 1e-12);
        assert!((omega_align(0.425) - 0.5).abs() < 1e-12);
        assert_eq!(omega_align(0.0), 0.0);
        assert_eq!(omega_align(f64::NAN), 0.0);
    }

    #[test]
    fn final_score_is_monotone_and_rejects_invalid_inputs() {
        let score = final_score(0.03, 0.5, 0.9, 0.8, 0.1, 1.0).unwrap();
        assert!(score > 0.0);
        let higher = final_score(0.03, 0.9, 0.9, 0.8, 0.1, 1.0).unwrap();
        assert!(higher > score);
        assert!(final_score(0.03, -0.1, 0.9, 0.8, 0.1, 1.0).is_err());
        assert!(final_score(0.03, 0.5, 0.9, f64::NAN, 0.1, 1.0).is_err());
        assert_eq!(final_score(0.03, 0.0, 0.9, 0.8, 0.1, 1.0).unwrap(), 0.0);
    }

    #[test]
    fn ppr_converges_and_stays_normalised() {
        let graph = Graph::from_edges(3, &[(0, 1), (1, 2), (2, 0)]);
        let result = ppr(&graph, &[1.0, 0.0, 0.0]).unwrap();
        assert!(!result.not_converged);
        assert!(result.iterations < PPR_MAX_ITER);
        let total: f64 = result.scores.iter().sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "scores stay normalised: {total}"
        );
        assert!(result.scores[0] > result.scores[2]);
    }

    #[test]
    fn ppr_redistributes_dangling_mass() {
        let graph = Graph::from_edges(2, &[(0, 1)]);
        let result = ppr(&graph, &[1.0, 0.0]).unwrap();
        let total: f64 = result.scores.iter().sum();
        assert!((total - 1.0).abs() < 1e-6, "no mass is lost: {total}");
    }

    #[test]
    fn ppr_validates_inputs() {
        let graph = Graph::from_edges(2, &[(0, 1)]);
        assert!(ppr(&graph, &[1.0]).is_err());
        let empty = Graph::default();
        assert!(ppr(&empty, &[]).unwrap().scores.is_empty());
        let zero_seeds = ppr(&graph, &[0.0, 0.0]).unwrap();
        assert!(zero_seeds.scores.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn planner_reports_unavailable_strategies_as_partial() {
        let plan = plan(QueryHints {
            vector_available: true,
            bm25_available: true,
            ..QueryHints::default()
        });
        assert_eq!(plan.executed, vec![Strategy::Vector, Strategy::Bm25]);
        assert!(plan.partial, "skipped channels must surface as partial");
        assert!(plan
            .skipped
            .iter()
            .any(|(strategy, _)| *strategy == Strategy::Graph));
    }

    #[test]
    fn planner_only_plans_cross_lingual_for_multilingual_queries() {
        let single = plan(QueryHints {
            multilingual: false,
            cross_lingual_available: true,
            bm25_available: true,
            ..QueryHints::default()
        });
        assert!(!single.executed.contains(&Strategy::CrossLingual));
        assert!(single
            .skipped
            .iter()
            .any(|(strategy, reason)| *strategy == Strategy::CrossLingual
                && reason.contains("not multilingual")));

        let multi = plan(QueryHints {
            multilingual: true,
            cross_lingual_available: true,
            bm25_available: true,
            ..QueryHints::default()
        });
        assert!(multi.executed.contains(&Strategy::CrossLingual));
    }

    #[test]
    fn detail_tiers_require_calibrated_relevance() {
        assert_eq!(detail_tier(0.95).unwrap(), DetailTier::Full);
        assert_eq!(detail_tier(0.90).unwrap(), DetailTier::Summary);
        assert_eq!(detail_tier(0.75).unwrap(), DetailTier::Summary);
        assert_eq!(detail_tier(0.60).unwrap(), DetailTier::Facts);
        assert_eq!(detail_tier(0.10).unwrap(), DetailTier::Identification);
        assert!(detail_tier(1.7).is_err(), "raw scores are out of range");
        assert!(detail_tier(f64::NAN).is_err());
        assert_eq!(DetailTier::Full.token_range(), (500, 2000));
    }

    fn tokens(text: &str) -> Vec<String> {
        text.split_whitespace().map(|t| t.to_lowercase()).collect()
    }

    #[test]
    fn bm25_ranks_the_matching_document_first() {
        let corpus = vec![
            tokens("alpha beta gamma"),
            tokens("delta epsilon zeta"),
            tokens("alpha alpha alpha"),
        ];
        let index = Bm25Index::build(&corpus);
        assert_eq!(index.len(), 3);
        let ranked = index.rank(&tokens("alpha")).unwrap();
        assert_eq!(ranked[0].0, 2, "the most frequent match ranks first");
        assert_eq!(ranked[1].0, 0);
        assert_eq!(ranked[2].1, 0.0, "a document without the term scores zero");
    }

    #[test]
    fn bm25_length_normalisation_discounts_longer_documents() {
        let corpus = vec![tokens("alpha"), tokens("alpha filler filler filler filler")];
        let index = Bm25Index::build(&corpus);
        let short = index.score(&tokens("alpha"), 0).unwrap();
        let long = index.score(&tokens("alpha"), 1).unwrap();
        assert!(short > long, "short={short} long={long}");
    }

    #[test]
    fn bm25_idf_is_never_negative_and_empty_corpus_scores_zero() {
        let corpus = vec![tokens("common term"), tokens("common term")];
        let index = Bm25Index::build(&corpus);
        assert!(index.idf("common") >= 0.0, "idf={}", index.idf("common"));
        assert_eq!(index.score(&tokens("absent"), 0).unwrap(), 0.0);
        let empty = Bm25Index::build(&[]);
        assert!(empty.is_empty());
        assert!(empty.rank(&tokens("anything")).unwrap().is_empty());
        let blank = Bm25Index::build(&[vec![], vec![]]);
        assert_eq!(blank.score(&tokens("anything"), 0).unwrap(), 0.0);
    }

    #[test]
    fn bm25_rejects_out_of_range_document_index() {
        let index = Bm25Index::build(&[tokens("alpha")]);
        assert!(index.score(&tokens("alpha"), 4).is_err());
    }

    #[test]
    fn cosine_is_bounded_and_rejects_mismatched_dimensions() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]).unwrap(), 1.0);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]).unwrap(), 0.0);
        assert_eq!(cosine(&[1.0, 0.0], &[-1.0, 0.0]).unwrap(), -1.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]).unwrap(), 0.0);
        assert!(cosine(&[1.0], &[1.0, 2.0]).is_err());
        assert!(cosine(&[], &[]).is_err());
        assert!(cosine(&[f32::NAN], &[1.0]).is_err());
    }
}
