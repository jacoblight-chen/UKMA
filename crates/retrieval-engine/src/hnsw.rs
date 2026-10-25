//! HNSW approximate nearest neighbour index (§25.2/§25.3 defaults:
//! `M=16`, `ef_construction=200`, `ef_search=64`).
//!
//! Deterministic by construction: level assignment uses a seeded LCG, so index
//! shape is reproducible for a given insertion order (required for golden tests
//! and for `ML-003` index-generation switches).
//!
//! Deletions are tombstones: a deleted point stops appearing in results but keeps
//! its graph edges, so deletion never destroys reachability for other points.

use storage_ports::Metric;

/// HNSW build/search parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswConfig {
    /// Neighbours per node on layers above 0 (§29.2 `M=16`).
    pub m: usize,
    /// Neighbours on layer 0 (conventionally `2*M`).
    pub m0: usize,
    /// Candidate list size during construction (§29.2 `ef_construction=200`).
    pub ef_construction: usize,
    /// Candidate list size during search (§29.2 `ef_search=64`).
    pub ef_search: usize,
    /// Seed for deterministic level assignment.
    pub seed: u64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 200,
            ef_search: 64,
            seed: 0x5EED_1234_ABCD_0001,
        }
    }
}

impl HnswConfig {
    /// Validate parameters; zero neighbours or candidate lists are rejected.
    pub fn validate(&self) -> Result<(), String> {
        if self.m == 0 || self.m0 == 0 {
            return Err("m and m0 must be >= 1".into());
        }
        if self.ef_construction < self.m {
            return Err("ef_construction must be >= m".into());
        }
        if self.ef_search == 0 {
            return Err("ef_search must be >= 1".into());
        }
        Ok(())
    }

    /// Normalisation factor for exponential level sampling.
    fn level_factor(&self) -> f64 {
        1.0 / (self.m as f64).ln()
    }
}

#[derive(Debug, Clone)]
struct Node {
    id: String,
    vector: Vec<f32>,
    /// Highest layer this node participates in.
    level: usize,
    /// `neighbours[layer]` = node indices.
    neighbours: Vec<Vec<usize>>,
    deleted: bool,
    metadata: Vec<(String, String)>,
}

/// Balance between similarity and distance so one comparison function works.
fn score(metric: Metric, left: &[f32], right: &[f32]) -> f64 {
    // Higher is always better here: for L2 we negate the distance.
    match metric {
        Metric::Cosine => {
            let mut dot = 0.0f64;
            let mut ln = 0.0f64;
            let mut rn = 0.0f64;
            for (a, b) in left.iter().zip(right.iter()) {
                dot += f64::from(*a) * f64::from(*b);
                ln += f64::from(*a) * f64::from(*a);
                rn += f64::from(*b) * f64::from(*b);
            }
            if ln == 0.0 || rn == 0.0 {
                0.0
            } else {
                dot / (ln.sqrt() * rn.sqrt())
            }
        }
        Metric::Dot => left
            .iter()
            .zip(right.iter())
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum(),
        Metric::L2 => {
            let sum: f64 = left
                .iter()
                .zip(right.iter())
                .map(|(a, b)| {
                    let delta = f64::from(*a) - f64::from(*b);
                    delta * delta
                })
                .sum();
            -sum.sqrt()
        }
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswHit {
    /// Logical id.
    pub id: String,
    /// Score (higher is better; L2 scores are negated distances).
    pub score: f64,
    /// Stored metadata.
    pub metadata: Vec<(String, String)>,
}

/// Hierarchical navigable small world index.
#[derive(Debug)]
pub struct Hnsw {
    config: HnswConfig,
    metric: Metric,
    dimensions: usize,
    nodes: Vec<Node>,
    entry_point: Option<usize>,
    max_level: usize,
    rng_state: u64,
}

impl Hnsw {
    /// Create an index for a fixed dimension/metric/profile.
    pub fn new(config: HnswConfig, metric: Metric, dimensions: usize) -> Result<Self, String> {
        config.validate()?;
        if dimensions == 0 {
            return Err("dimensions must be >= 1".into());
        }
        let rng_state = config.seed;
        Ok(Self {
            config,
            metric,
            dimensions,
            nodes: Vec::new(),
            entry_point: None,
            max_level: 0,
            rng_state,
        })
    }

    /// Configuration in use.
    pub fn config(&self) -> HnswConfig {
        self.config
    }

    /// Highest layer present in the graph (diagnostics/tests).
    pub fn max_level(&self) -> usize {
        self.max_level
    }

    /// Highest layer a given id participates in, if indexed.
    pub fn level_of(&self, id: &str) -> Option<usize> {
        self.nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| node.level)
    }

    /// Number of indexed (non-deleted) points.
    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|node| !node.deleted).count()
    }

    /// Whether the index holds no live points.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Deterministic uniform in (0,1).
    fn next_uniform(&mut self) -> f64 {
        // Numerical Recipes LCG constants.
        self.rng_state = self
            .rng_state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let value = (self.rng_state >> 11) as f64 / (1u64 << 53) as f64;
        value.max(f64::MIN_POSITIVE)
    }

    fn sample_level(&mut self) -> usize {
        let factor = self.config.level_factor();
        let uniform = self.next_uniform();
        (-uniform.ln() * factor).floor() as usize
    }

    fn capacity(&self, layer: usize) -> usize {
        if layer == 0 {
            self.config.m0
        } else {
            self.config.m
        }
    }

    fn greedy(&self, query: &[f32], entry: usize, layer: usize) -> usize {
        let mut current = entry;
        let mut current_score = score(self.metric, query, &self.nodes[current].vector);
        loop {
            let mut improved = false;
            if let Some(neighbours) = self.nodes[current].neighbours.get(layer) {
                for neighbour in neighbours.clone() {
                    let candidate = score(self.metric, query, &self.nodes[neighbour].vector);
                    if candidate > current_score {
                        current_score = candidate;
                        current = neighbour;
                        improved = true;
                    }
                }
            }
            if !improved {
                return current;
            }
        }
    }

    /// Best-first search restricted to one layer.
    fn search_layer(
        &self,
        query: &[f32],
        entry_points: &[usize],
        layer: usize,
        ef: usize,
    ) -> Vec<(f64, usize)> {
        use std::collections::BinaryHeap;

        #[derive(PartialEq)]
        struct Candidate(f64, usize);
        impl Eq for Candidate {}
        impl PartialOrd for Candidate {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for Candidate {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.0
                    .partial_cmp(&other.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        }

        let mut visited = vec![false; self.nodes.len()];
        let mut candidates: BinaryHeap<Candidate> = BinaryHeap::new();
        let mut results: BinaryHeap<std::cmp::Reverse<Candidate>> = BinaryHeap::new();

        for entry in entry_points {
            if visited[*entry] {
                continue;
            }
            visited[*entry] = true;
            let value = score(self.metric, query, &self.nodes[*entry].vector);
            candidates.push(Candidate(value, *entry));
            results.push(std::cmp::Reverse(Candidate(value, *entry)));
            if results.len() > ef {
                results.pop();
            }
        }

        while let Some(Candidate(best, node)) = candidates.pop() {
            let worst = results
                .peek()
                .map(|std::cmp::Reverse(Candidate(value, _))| *value)
                .unwrap_or(f64::NEG_INFINITY);
            if best < worst && results.len() >= ef {
                break;
            }
            if let Some(neighbours) = self.nodes[node].neighbours.get(layer) {
                for neighbour in neighbours.clone() {
                    if visited[neighbour] {
                        continue;
                    }
                    visited[neighbour] = true;
                    let value = score(self.metric, query, &self.nodes[neighbour].vector);
                    let worst_now = results
                        .peek()
                        .map(|std::cmp::Reverse(Candidate(value, _))| *value)
                        .unwrap_or(f64::NEG_INFINITY);
                    if results.len() < ef || value > worst_now {
                        candidates.push(Candidate(value, neighbour));
                        results.push(std::cmp::Reverse(Candidate(value, neighbour)));
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
        }

        let mut collected: Vec<(f64, usize)> = results
            .into_iter()
            .map(|std::cmp::Reverse(Candidate(value, node))| (value, node))
            .collect();
        collected.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.1.cmp(&right.1))
        });
        collected
    }

    fn connect(&mut self, layer: usize, node: usize, candidates: &[(f64, usize)]) {
        let capacity = self.capacity(layer);
        let mut chosen: Vec<usize> = candidates
            .iter()
            .map(|(_, index)| *index)
            .filter(|index| *index != node)
            .take(capacity)
            .collect();
        chosen.dedup();
        self.nodes[node].neighbours[layer] = chosen.clone();
        for neighbour in chosen {
            // A candidate may not have this layer (lower level); never index past
            // its neighbour vector.
            let Some(existing) = self.nodes[neighbour].neighbours.get(layer).cloned() else {
                continue;
            };
            // Work on a copy so reads of `self.nodes` do not conflict with the
            // mutable borrow of the neighbour list.
            let mut list = existing;
            if !list.contains(&node) {
                list.push(node);
            }
            if list.len() > capacity {
                let target = self.nodes[neighbour].vector.clone();
                let mut scored: Vec<(f64, usize)> = list
                    .iter()
                    .map(|index| {
                        (
                            score(self.metric, &target, &self.nodes[*index].vector),
                            *index,
                        )
                    })
                    .collect();
                scored.sort_by(|left, right| {
                    right
                        .0
                        .partial_cmp(&left.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                scored.truncate(capacity);
                list = scored.into_iter().map(|(_, index)| index).collect();
            }
            self.nodes[neighbour].neighbours[layer] = list;
        }
    }

    /// Insert or replace a point (replacing keeps the id unique).
    pub fn insert(
        &mut self,
        id: &str,
        vector: &[f32],
        metadata: Vec<(String, String)>,
    ) -> Result<(), String> {
        if vector.len() != self.dimensions {
            return Err(format!(
                "vector dimension {} != index dimension {}",
                vector.len(),
                self.dimensions
            ));
        }
        if let Some(existing) = self.nodes.iter().position(|node| node.id == id) {
            self.nodes[existing].vector = vector.to_vec();
            self.nodes[existing].metadata = metadata;
            self.nodes[existing].deleted = false;
            return Ok(());
        }

        let level = self.sample_level();
        let index = self.nodes.len();
        self.nodes.push(Node {
            id: id.to_string(),
            vector: vector.to_vec(),
            level,
            neighbours: vec![Vec::new(); level + 1],
            deleted: false,
            metadata,
        });

        let Some(mut entry) = self.entry_point else {
            self.entry_point = Some(index);
            self.max_level = level;
            return Ok(());
        };

        let mut current = entry;
        // Descend from the top to just above the new node's level.
        let mut layer = self.max_level;
        while layer > level {
            current = self.greedy(vector, current, layer);
            layer -= 1;
        }
        // Build connections from min(level, max_level) down to 0.
        let top = level.min(self.max_level);
        for layer in (0..=top).rev() {
            let candidates =
                self.search_layer(vector, &[current], layer, self.config.ef_construction);
            self.connect(layer, index, &candidates);
            // Continue from the best candidate that actually has the next layer.
            if let Some((_, best)) = candidates
                .iter()
                .find(|(_, candidate)| self.nodes[*candidate].neighbours.len() > layer)
            {
                current = *best;
            }
        }
        if level > self.max_level {
            self.max_level = level;
            entry = self.entry_point.unwrap_or(index);
            let _ = entry;
        }
        if self.max_level < level || self.entry_point.is_none() {
            self.entry_point = Some(index);
            self.max_level = level;
        }
        Ok(())
    }

    /// Tombstone a point; it stops appearing in results but keeps its edges.
    pub fn delete(&mut self, id: &str) -> bool {
        match self.nodes.iter_mut().find(|node| node.id == id) {
            Some(node) => {
                node.deleted = true;
                true
            }
            None => false,
        }
    }

    /// k-NN search with optional metadata filter.
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&[(String, String)]>,
    ) -> Result<Vec<HnswHit>, String> {
        if query.len() != self.dimensions {
            return Err(format!(
                "query dimension {} != index dimension {}",
                query.len(),
                self.dimensions
            ));
        }
        if k == 0 {
            return Err("k must be >= 1".into());
        }
        let Some(entry) = self.entry_point else {
            return Ok(Vec::new());
        };
        let mut current = entry;
        let mut layer = self.max_level;
        while layer > 0 {
            current = self.greedy(query, current, layer);
            layer -= 1;
        }
        let ef = self.config.ef_search.max(k);
        let candidates = self.search_layer(query, &[current], 0, ef);
        let mut hits: Vec<HnswHit> = candidates
            .into_iter()
            .filter(|(_, index)| !self.nodes[*index].deleted)
            .filter(|(_, index)| match filter {
                Some(required) => required.iter().all(|(key, value)| {
                    self.nodes[*index]
                        .metadata
                        .contains(&(key.clone(), value.clone()))
                }),
                None => true,
            })
            .map(|(value, index)| HnswHit {
                id: self.nodes[index].id.clone(),
                score: value,
                metadata: self.nodes[index].metadata.clone(),
            })
            .collect();
        hits.truncate(k);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deterministic_vector(seed: u64, dimensions: usize) -> Vec<f32> {
        let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (0..dimensions)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
                (unit * 2.0 - 1.0) as f32
            })
            .collect()
    }

    #[test]
    fn rejects_bad_parameters() {
        let no_neighbours = HnswConfig {
            m: 0,
            ..HnswConfig::default()
        };
        assert!(Hnsw::new(no_neighbours, Metric::Cosine, 4).is_err());

        let tiny_ef = HnswConfig {
            ef_construction: 1,
            ..HnswConfig::default()
        };
        assert!(Hnsw::new(tiny_ef, Metric::Cosine, 4).is_err());

        assert!(Hnsw::new(HnswConfig::default(), Metric::Cosine, 0).is_err());
        assert!(Hnsw::new(HnswConfig::default(), Metric::Cosine, 4).is_ok());
    }

    #[test]
    fn defaults_match_the_spec_profile() {
        let config = HnswConfig::default();
        assert_eq!(config.m, 16);
        assert_eq!(config.ef_construction, 200);
        assert_eq!(config.ef_search, 64);
    }

    #[test]
    fn finds_the_exact_nearest_neighbour_on_a_small_set() {
        let mut index = Hnsw::new(HnswConfig::default(), Metric::Cosine, 8).unwrap();
        let vectors: Vec<Vec<f32>> = (0..40).map(|i| deterministic_vector(i, 8)).collect();
        for (i, vector) in vectors.iter().enumerate() {
            index.insert(&format!("v{i}"), vector, Vec::new()).unwrap();
        }
        assert_eq!(index.len(), 40);

        let query = &vectors[7];
        let hits = index.search(query, 1, None).unwrap();
        assert_eq!(hits[0].id, "v7", "the point itself must be the closest");
        assert!((hits[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn recall_is_high_against_brute_force() {
        let dimensions = 8;
        let count = 240;
        let mut index = Hnsw::new(HnswConfig::default(), Metric::Cosine, dimensions).unwrap();
        let vectors: Vec<Vec<f32>> = (0..count)
            .map(|i| deterministic_vector(i * 31 + 5, dimensions))
            .collect();
        for (i, vector) in vectors.iter().enumerate() {
            index.insert(&format!("v{i}"), vector, Vec::new()).unwrap();
        }

        let mut total_expected = 0usize;
        let mut total_found = 0usize;
        for probe in 0..20u64 {
            let query = deterministic_vector(probe * 977 + 13, dimensions);
            let mut brute: Vec<(f64, usize)> = vectors
                .iter()
                .enumerate()
                .map(|(i, vector)| (score(Metric::Cosine, &query, vector), i))
                .collect();
            brute.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            let expected: Vec<usize> = brute.iter().take(10).map(|(_, i)| *i).collect();

            let hits = index.search(&query, 10, None).unwrap();
            let found: Vec<usize> = hits
                .iter()
                .map(|hit| hit.id.trim_start_matches('v').parse::<usize>().unwrap())
                .collect();
            total_expected += expected.len();
            total_found += expected.iter().filter(|i| found.contains(i)).count();
        }
        let recall = total_found as f64 / total_expected as f64;
        assert!(recall >= 0.90, "recall@10 was {recall}");
    }

    #[test]
    fn deleted_points_disappear_without_breaking_search() {
        let mut index = Hnsw::new(HnswConfig::default(), Metric::Cosine, 4).unwrap();
        for i in 0..30 {
            index
                .insert(&format!("v{i}"), &deterministic_vector(i, 4), Vec::new())
                .unwrap();
        }
        let query = deterministic_vector(5, 4);
        let before = index.search(&query, 3, None).unwrap();
        assert_eq!(before[0].id, "v5");

        assert!(index.delete("v5"));
        assert_eq!(index.len(), 29);
        let after = index.search(&query, 3, None).unwrap();
        assert!(!after.iter().any(|hit| hit.id == "v5"));
        assert!(!after.is_empty(), "other points remain reachable");
        assert!(!index.delete("missing"));
    }

    #[test]
    fn metadata_filter_is_applied_after_search() {
        let mut index = Hnsw::new(HnswConfig::default(), Metric::Cosine, 4).unwrap();
        for i in 0..20 {
            let language = if i % 2 == 0 { "en" } else { "zh" };
            index
                .insert(
                    &format!("v{i}"),
                    &deterministic_vector(i, 4),
                    vec![("lang".to_string(), language.to_string())],
                )
                .unwrap();
        }
        let filter = vec![("lang".to_string(), "zh".to_string())];
        let hits = index
            .search(&deterministic_vector(0, 4), 5, Some(&filter))
            .unwrap();
        assert!(!hits.is_empty());
        assert!(hits
            .iter()
            .all(|hit| hit.metadata.iter().any(|(k, v)| k == "lang" && v == "zh")));
    }

    #[test]
    fn dimension_and_k_are_validated() {
        let mut index = Hnsw::new(HnswConfig::default(), Metric::Cosine, 4).unwrap();
        assert!(index.insert("v0", &[0.0; 3], Vec::new()).is_err());
        index
            .insert("v0", &[1.0, 0.0, 0.0, 0.0], Vec::new())
            .unwrap();
        assert!(index.search(&[0.0; 3], 1, None).is_err());
        assert!(index.search(&[1.0, 0.0, 0.0, 0.0], 0, None).is_err());
    }

    #[test]
    fn reinserting_the_same_id_replaces_the_vector() {
        let mut index = Hnsw::new(HnswConfig::default(), Metric::Cosine, 4).unwrap();
        index
            .insert("v0", &[1.0, 0.0, 0.0, 0.0], vec![("v".into(), "1".into())])
            .unwrap();
        index
            .insert("v0", &[0.0, 1.0, 0.0, 0.0], vec![("v".into(), "2".into())])
            .unwrap();
        assert_eq!(index.len(), 1, "ids stay unique");
        let hits = index.search(&[0.0, 1.0, 0.0, 0.0], 1, None).unwrap();
        assert_eq!(hits[0].id, "v0");
        assert_eq!(hits[0].metadata, vec![("v".to_string(), "2".to_string())]);
    }

    #[test]
    fn l2_index_ranks_by_negated_distance() {
        let mut index = Hnsw::new(HnswConfig::default(), Metric::L2, 2).unwrap();
        index.insert("near", &[0.1, 0.0], Vec::new()).unwrap();
        index.insert("far", &[9.0, 0.0], Vec::new()).unwrap();
        let hits = index.search(&[0.0, 0.0], 2, None).unwrap();
        assert_eq!(hits[0].id, "near");
        assert!(
            hits[0].score > hits[1].score,
            "closer means a higher negation"
        );
    }

    #[test]
    fn empty_index_returns_no_hits() {
        let index = Hnsw::new(HnswConfig::default(), Metric::Cosine, 4).unwrap();
        assert!(index.is_empty());
        assert!(index
            .search(&[1.0, 0.0, 0.0, 0.0], 3, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn build_is_deterministic_for_the_same_insertion_order() {
        let build = || {
            let mut index = Hnsw::new(HnswConfig::default(), Metric::Cosine, 6).unwrap();
            for i in 0..50 {
                index
                    .insert(&format!("v{i}"), &deterministic_vector(i, 6), Vec::new())
                    .unwrap();
            }
            index
                .search(&deterministic_vector(11, 6), 5, None)
                .unwrap()
                .into_iter()
                .map(|hit| hit.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(build(), build(), "same seed and order => same results");
    }
}
