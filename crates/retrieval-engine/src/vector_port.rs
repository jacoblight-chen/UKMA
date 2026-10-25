//! `VectorPort` adapter backed by the HNSW index.
//!
//! This proves the port contract can be served by an approximate index without
//! leaking implementation detail: profile identity, dimension/metric rejection
//! and scope isolation all come from the port contract, not from HNSW.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use storage_ports::{
    validate_scope, EmbeddingProfile, Metric, PortResult, StorageError, VectorHit, VectorItem,
    VectorPort,
};
use ukma_contracts::ScopeRef;

use crate::hnsw::{Hnsw, HnswConfig};

fn scope_key(scope: &ScopeRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        scope.tenant_id,
        scope.project_id,
        scope.repo_id.as_deref().unwrap_or("")
    )
}

#[derive(Default)]
struct Bucket {
    profile: Option<EmbeddingProfile>,
    index: Option<Hnsw>,
    /// id -> revision, kept so hits report the right revision after re-upsert.
    revisions: BTreeMap<String, String>,
}

/// Vector store that serves the port with HNSW under the hood.
pub struct HnswVectorStore {
    config: HnswConfig,
    buckets: Mutex<HashMap<(String, String), Bucket>>,
}

impl Default for HnswVectorStore {
    fn default() -> Self {
        Self::new(HnswConfig::default())
    }
}

impl HnswVectorStore {
    /// New store using the given HNSW parameters.
    pub fn new(config: HnswConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Number of live vectors in a profile (test helper).
    pub fn len(&self, scope: &ScopeRef, profile_id: &str) -> usize {
        self.buckets
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .get(&(scope_key(scope), profile_id.to_string()))
                    .and_then(|bucket| bucket.index.as_ref().map(|index| index.len()))
            })
            .unwrap_or(0)
    }

    /// Whether the profile has no vectors.
    pub fn is_empty(&self, scope: &ScopeRef, profile_id: &str) -> bool {
        self.len(scope, profile_id) == 0
    }
}

fn lock_error() -> StorageError {
    StorageError::Internal("vector store lock poisoned".into())
}

impl VectorPort for HnswVectorStore {
    fn declare_profile(&self, scope: &ScopeRef, profile: &EmbeddingProfile) -> PortResult<()> {
        validate_scope(scope)?;
        if profile.dimension == 0 {
            return Err(StorageError::InvalidArgument {
                field: "dimension",
                reason: "must be >= 1".into(),
            });
        }
        let mut guard = self.buckets.lock().map_err(|_| lock_error())?;
        let bucket = guard
            .entry((scope_key(scope), profile.profile_id.clone()))
            .or_default();
        if let Some(existing) = &bucket.profile {
            if existing.dimension != profile.dimension || existing.metric != profile.metric {
                return Err(StorageError::ModelProfileMismatch {
                    detail: format!(
                        "profile `{}` already declared with dimension {} / {:?}",
                        profile.profile_id, existing.dimension, existing.metric
                    ),
                });
            }
        } else {
            bucket.index = Some(
                Hnsw::new(self.config, profile.metric, profile.dimension)
                    .map_err(StorageError::Internal)?,
            );
        }
        bucket.profile = Some(profile.clone());
        Ok(())
    }

    fn profile(&self, scope: &ScopeRef, profile_id: &str) -> PortResult<Option<EmbeddingProfile>> {
        validate_scope(scope)?;
        let guard = self.buckets.lock().map_err(|_| lock_error())?;
        Ok(guard
            .get(&(scope_key(scope), profile_id.to_string()))
            .and_then(|bucket| bucket.profile.clone()))
    }

    fn upsert(&self, scope: &ScopeRef, profile_id: &str, items: &[VectorItem]) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self.buckets.lock().map_err(|_| lock_error())?;
        let key = (scope_key(scope), profile_id.to_string());
        let bucket = guard.entry(key).or_default();
        let profile = bucket
            .profile
            .clone()
            .ok_or_else(|| StorageError::ModelProfileMismatch {
                detail: format!("profile `{profile_id}` is not declared in scope"),
            })?;
        let index = bucket
            .index
            .as_mut()
            .ok_or_else(|| StorageError::Internal("profile declared without an index".into()))?;
        for item in items {
            if item.vector.len() != profile.dimension {
                return Err(StorageError::ModelProfileMismatch {
                    detail: format!(
                        "item `{}` has dimension {} but profile `{profile_id}` requires {}",
                        item.id,
                        item.vector.len(),
                        profile.dimension
                    ),
                });
            }
        }
        for item in items {
            index
                .insert(
                    &item.id,
                    &item.vector,
                    item.metadata
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                )
                .map_err(StorageError::Internal)?;
            bucket
                .revisions
                .insert(item.id.clone(), item.revision.clone());
        }
        Ok(())
    }

    fn delete(&self, scope: &ScopeRef, profile_id: &str, id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self.buckets.lock().map_err(|_| lock_error())?;
        if let Some(bucket) = guard.get_mut(&(scope_key(scope), profile_id.to_string())) {
            if let Some(index) = bucket.index.as_mut() {
                index.delete(id);
            }
            bucket.revisions.remove(id);
        }
        Ok(())
    }

    fn search(
        &self,
        scope: &ScopeRef,
        profile_id: &str,
        query: &[f32],
        k: usize,
        filter: Option<&BTreeMap<String, String>>,
    ) -> PortResult<Vec<VectorHit>> {
        validate_scope(scope)?;
        if k == 0 {
            return Err(StorageError::InvalidArgument {
                field: "k",
                reason: "must be >= 1".into(),
            });
        }
        let guard = self.buckets.lock().map_err(|_| lock_error())?;
        let Some(bucket) = guard.get(&(scope_key(scope), profile_id.to_string())) else {
            return Ok(Vec::new());
        };
        let profile =
            bucket
                .profile
                .as_ref()
                .ok_or_else(|| StorageError::ModelProfileMismatch {
                    detail: format!("profile `{profile_id}` is not declared in scope"),
                })?;
        if query.len() != profile.dimension {
            return Err(StorageError::ModelProfileMismatch {
                detail: format!(
                    "query dimension {} != profile `{profile_id}` dimension {}",
                    query.len(),
                    profile.dimension
                ),
            });
        }
        let Some(index) = bucket.index.as_ref() else {
            return Ok(Vec::new());
        };
        let filter_vec: Option<Vec<(String, String)>> =
            filter.map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let hits = index
            .search(query, k, filter_vec.as_deref())
            .map_err(StorageError::Internal)?;
        Ok(hits
            .into_iter()
            .map(|hit| VectorHit {
                revision: bucket.revisions.get(&hit.id).cloned().unwrap_or_default(),
                id: hit.id,
                // L2 scores are negated distances inside the index; expose the
                // distance so port semantics stay "lower is closer" for L2.
                score: if profile.metric == Metric::L2 {
                    -hit.score
                } else {
                    hit.score
                },
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> ScopeRef {
        ScopeRef {
            tenant_id: "t".into(),
            project_id: "p".into(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        }
    }

    fn profile(id: &str, dimension: usize, metric: Metric) -> EmbeddingProfile {
        EmbeddingProfile {
            profile_id: id.into(),
            model_id: "bge-m3".into(),
            model_revision: "1".into(),
            dimension,
            metric,
            normalization: "l2".into(),
        }
    }

    fn item(id: &str, vector: Vec<f32>) -> VectorItem {
        VectorItem {
            id: id.into(),
            revision: format!("rev-{id}"),
            vector,
            metadata: BTreeMap::from([("lang".to_string(), "en".to_string())]),
        }
    }

    #[test]
    fn serves_search_through_hnsw_with_revisions() {
        let store = HnswVectorStore::default();
        let s = scope();
        store
            .declare_profile(&s, &profile("p4", 4, Metric::Cosine))
            .unwrap();
        store
            .upsert(
                &s,
                "p4",
                &[
                    item("near", vec![1.0, 0.0, 0.0, 0.0]),
                    item("far", vec![0.0, 1.0, 0.0, 0.0]),
                ],
            )
            .unwrap();
        assert_eq!(store.len(&s, "p4"), 2);

        let hits = store
            .search(&s, "p4", &[1.0, 0.0, 0.0, 0.0], 2, None)
            .unwrap();
        assert_eq!(hits[0].id, "near");
        assert_eq!(hits[0].revision, "rev-near");
        assert!((hits[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn profile_and_dimension_guards_still_apply() {
        let store = HnswVectorStore::default();
        let s = scope();
        store
            .declare_profile(&s, &profile("p4", 4, Metric::Cosine))
            .unwrap();
        assert_eq!(
            store
                .declare_profile(&s, &profile("p4", 8, Metric::Cosine))
                .unwrap_err()
                .code(),
            "MODEL_PROFILE_MISMATCH"
        );
        assert_eq!(
            store
                .upsert(&s, "p4", &[item("bad", vec![1.0, 0.0])])
                .unwrap_err()
                .code(),
            "MODEL_PROFILE_MISMATCH"
        );
        assert_eq!(
            store
                .search(&s, "p4", &[1.0, 0.0], 1, None)
                .unwrap_err()
                .code(),
            "MODEL_PROFILE_MISMATCH"
        );
    }

    #[test]
    fn scopes_do_not_share_vectors() {
        let store = HnswVectorStore::default();
        let a = scope();
        let mut b = scope();
        b.tenant_id = "other".into();
        store
            .declare_profile(&a, &profile("p4", 4, Metric::Cosine))
            .unwrap();
        store
            .declare_profile(&b, &profile("p4", 4, Metric::Cosine))
            .unwrap();
        store
            .upsert(&a, "p4", &[item("secret", vec![1.0, 0.0, 0.0, 0.0])])
            .unwrap();
        assert!(store
            .search(&b, "p4", &[1.0, 0.0, 0.0, 0.0], 5, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn l2_scores_stay_lower_is_closer() {
        let store = HnswVectorStore::default();
        let s = scope();
        store
            .declare_profile(&s, &profile("l2", 2, Metric::L2))
            .unwrap();
        store
            .upsert(
                &s,
                "l2",
                &[
                    item("close", vec![0.1, 0.0]),
                    item("distant", vec![9.0, 0.0]),
                ],
            )
            .unwrap();
        let hits = store.search(&s, "l2", &[0.0, 0.0], 2, None).unwrap();
        assert_eq!(hits[0].id, "close");
        assert!(
            hits[0].score < hits[1].score,
            "L2 exposes distance: {} vs {}",
            hits[0].score,
            hits[1].score
        );
    }

    #[test]
    fn delete_removes_the_point_from_results() {
        let store = HnswVectorStore::default();
        let s = scope();
        store
            .declare_profile(&s, &profile("p4", 4, Metric::Cosine))
            .unwrap();
        store
            .upsert(
                &s,
                "p4",
                &[
                    item("a", vec![1.0, 0.0, 0.0, 0.0]),
                    item("b", vec![0.0, 1.0, 0.0, 0.0]),
                ],
            )
            .unwrap();
        store.delete(&s, "p4", "a").unwrap();
        let hits = store
            .search(&s, "p4", &[1.0, 0.0, 0.0, 0.0], 5, None)
            .unwrap();
        assert!(hits.iter().all(|hit| hit.id != "a"));
        assert_eq!(store.len(&s, "p4"), 1);
    }
}
