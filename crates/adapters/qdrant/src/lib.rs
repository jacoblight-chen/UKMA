//! Qdrant vector adapter (profile C).
//!
//! Serves [`VectorPort`] against a real Qdrant server over its REST API: one
//! collection per (scope, embedding profile), which is how tenant and profile
//! isolation are enforced by construction — a query can only ever reach the
//! collection it names.
//!
//! Deliberate choices, stated rather than hidden:
//! - **REST, not gRPC**, so the adapter stays synchronous and avoids pulling an
//!   async runtime into the workspace for one port.
//! - The **profile registry is process-local**: Qdrant stores the collection's
//!   dimension and distance (the authoritative part) but has nowhere to keep
//!   `model_id`/`model_revision`/`normalization`, so `declare_profile` must run
//!   in each process that writes vectors. `profile()` therefore returns `None`
//!   after a restart instead of inventing an identity.
//! - Point ids are derived deterministically from the logical id, and the logical
//!   id plus the source revision travel in the payload, so a round trip returns
//!   what was stored.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use storage_ports::capabilities::{
    unavailable_adapter, AdapterCapabilities, BackendKind, CapabilityProbe, TransactionScope,
};
use storage_ports::ports::{
    validate_scope, EmbeddingProfile, FullTextDoc, FullTextHit, FullTextPort, MetadataPort, Metric,
    VectorHit, VectorItem, VectorPort,
};
use storage_ports::{PortResult, StorageError};
use ukma_contracts::ScopeRef;

/// Environment variable an operator sets to enable Qdrant.
pub const URL_ENV: &str = "UKMA_QDRANT_URL";

/// Adapter version stamped into capability reports.
const ADAPTER_VERSION: &str = "0.1.0";

fn internal(message: impl Into<String>) -> StorageError {
    StorageError::Internal(message.into())
}

fn scope_key(scope: &ScopeRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        scope.tenant_id,
        scope.project_id,
        scope.repo_id.as_deref().unwrap_or(""),
        scope.workspace_id.as_deref().unwrap_or(""),
        scope.agent_id.as_deref().unwrap_or(""),
        scope.session_id.as_deref().unwrap_or("")
    )
}

fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Collection name for one (scope, profile): isolation by construction.
fn collection_name(scope: &ScopeRef, profile_id: &str) -> String {
    format!(
        "ukma_{}_{}",
        short_hash(&scope_key(scope)),
        short_hash(profile_id)
    )
}

/// Deterministic point id from a logical id (Qdrant accepts UUIDs or u64).
fn point_id(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        hex(&digest[0..4]),
        hex(&digest[4..6]),
        hex(&digest[6..8]),
        hex(&digest[8..10]),
        hex(&digest[10..16])
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Payload keys may not contain dots, so metadata is stored with a prefix.
fn payload_key(key: &str) -> String {
    format!("meta_{}", key.replace('.', "_"))
}

fn distance(metric: Metric) -> &'static str {
    match metric {
        Metric::Cosine => "Cosine",
        Metric::Dot => "Dot",
        Metric::L2 => "Euclid",
    }
}

/// Vector store backed by a Qdrant server.
pub struct QdrantStore {
    base: String,
    api_key: Option<String>,
    agent: ureq::Agent,
    profiles: Mutex<BTreeMap<(String, String), EmbeddingProfile>>,
    fulltext: storage_ports::testing::InMemoryFullText,
}

impl QdrantStore {
    /// Connect to `url` (for example `http://127.0.0.1:6333`).
    ///
    /// # Errors
    /// Fails when the server does not answer its version endpoint.
    pub fn connect(url: &str, api_key: Option<String>) -> PortResult<Self> {
        let base = url.trim_end_matches('/').to_string();
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30))
            .build();
        let store = Self {
            base,
            api_key,
            agent,
            profiles: Mutex::new(BTreeMap::new()),
            fulltext: storage_ports::testing::InMemoryFullText::new(),
        };
        let root = store.get("/")?;
        if root.get("version").and_then(Value::as_str).is_none() {
            return Err(internal("the endpoint did not answer like Qdrant"));
        }
        Ok(store)
    }

    /// Connect using [`URL_ENV`] and an optional `UKMA_QDRANT_API_KEY`.
    ///
    /// # Errors
    /// Returns [`StorageError::CapabilityUnavailable`] when the URL is unset.
    pub fn from_env() -> PortResult<Self> {
        match std::env::var(URL_ENV) {
            Ok(url) if !url.trim().is_empty() => {
                Self::connect(&url, std::env::var("UKMA_QDRANT_API_KEY").ok())
            }
            _ => Err(StorageError::CapabilityUnavailable {
                capability: "qdrant",
            }),
        }
    }

    /// Base URL this store was opened with.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> PortResult<Value> {
        let url = format!("{}{}", self.base, path);
        let mut request = match method {
            "GET" => self.agent.get(&url),
            "PUT" => self.agent.put(&url),
            "POST" => self.agent.post(&url),
            "DELETE" => self.agent.delete(&url),
            other => return Err(internal(format!("unsupported method {other}"))),
        };
        if let Some(key) = &self.api_key {
            request = request.set("api-key", key);
        }
        let response = match body {
            Some(body) => request.send_json(body.clone()),
            None => request.call(),
        };
        match response {
            Ok(response) => response
                .into_json::<Value>()
                .map_err(|error| internal(format!("qdrant returned non-JSON: {error}"))),
            Err(ureq::Error::Status(status, response)) => {
                let detail = response
                    .into_string()
                    .unwrap_or_else(|_| "<no body>".to_string());
                Err(internal(format!("qdrant {status}: {detail}")))
            }
            Err(error) => Err(internal(format!("qdrant transport error: {error}"))),
        }
    }

    fn get(&self, path: &str) -> PortResult<Value> {
        self.request("GET", path, None)
    }

    fn collect(&self, name: &str, dimension: usize, metric: Metric) -> PortResult<()> {
        let body = json!({"vectors": {"size": dimension, "distance": distance(metric)}});
        // Creating an existing collection with the same shape is a no-op.
        match self.request("PUT", &format!("/collections/{name}"), Some(&body)) {
            Ok(_) => Ok(()),
            Err(error) => {
                let text = error.safe_message();
                if text.contains("already exists") {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn exists(&self, name: &str) -> PortResult<bool> {
        match self.get(&format!("/collections/{name}")) {
            Ok(_) => Ok(true),
            Err(error) if error.safe_message().contains("404") => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn profile_of(
        &self,
        scope: &ScopeRef,
        profile_id: &str,
    ) -> PortResult<Option<EmbeddingProfile>> {
        let guard = self
            .profiles
            .lock()
            .map_err(|_| internal("qdrant profile lock poisoned"))?;
        Ok(guard
            .get(&(scope_key(scope), profile_id.to_string()))
            .cloned())
    }
}

impl VectorPort for QdrantStore {
    fn declare_profile(&self, scope: &ScopeRef, profile: &EmbeddingProfile) -> PortResult<()> {
        validate_scope(scope)?;
        if profile.dimension == 0 {
            return Err(StorageError::InvalidArgument {
                field: "dimension",
                reason: "must be >= 1".into(),
            });
        }
        let name = collection_name(scope, &profile.profile_id);
        if self.exists(&name)? {
            // Re-declaring with a different shape is a contract violation, not a
            // silent change: the stored vectors would stop being comparable.
            let info = self.get(&format!("/collections/{name}"))?;
            let size = info
                .pointer("/result/config/params/vectors/size")
                .and_then(Value::as_u64);
            if let Some(size) = size {
                if size as usize != profile.dimension {
                    return Err(StorageError::ModelProfileMismatch {
                        detail: format!(
                            "profile `{}` is stored with dimension {size}, not {}",
                            profile.profile_id, profile.dimension
                        ),
                    });
                }
            }
        } else {
            self.collect(&name, profile.dimension, profile.metric)?;
        }
        let mut guard = self
            .profiles
            .lock()
            .map_err(|_| internal("qdrant profile lock poisoned"))?;
        guard.insert(
            (scope_key(scope), profile.profile_id.clone()),
            profile.clone(),
        );
        Ok(())
    }

    fn profile(&self, scope: &ScopeRef, profile_id: &str) -> PortResult<Option<EmbeddingProfile>> {
        validate_scope(scope)?;
        self.profile_of(scope, profile_id)
    }

    fn upsert(&self, scope: &ScopeRef, profile_id: &str, items: &[VectorItem]) -> PortResult<()> {
        validate_scope(scope)?;
        let profile = self.profile_of(scope, profile_id)?.ok_or_else(|| {
            StorageError::ModelProfileMismatch {
                detail: format!("profile `{profile_id}` is not declared in this process"),
            }
        })?;
        for item in items {
            if item.vector.len() != profile.dimension {
                return Err(StorageError::ModelProfileMismatch {
                    detail: format!(
                        "item `{}` has dimension {}, profile expects {}",
                        item.id,
                        item.vector.len(),
                        profile.dimension
                    ),
                });
            }
        }
        let name = collection_name(scope, profile_id);
        if !self.exists(&name)? {
            return Err(StorageError::ModelProfileMismatch {
                detail: format!("profile `{profile_id}` has no collection"),
            });
        }
        let points: Vec<Value> = items
            .iter()
            .map(|item| {
                let mut payload = serde_json::Map::new();
                payload.insert("id".into(), json!(item.id));
                payload.insert("revision".into(), json!(item.revision));
                for (key, value) in &item.metadata {
                    payload.insert(payload_key(key), json!(value));
                }
                json!({
                    "id": point_id(&item.id),
                    "vector": item.vector,
                    "payload": payload,
                })
            })
            .collect();
        self.request(
            "PUT",
            &format!("/collections/{name}/points?wait=true"),
            Some(&json!({"points": points})),
        )?;
        Ok(())
    }

    fn delete(&self, scope: &ScopeRef, profile_id: &str, id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let name = collection_name(scope, profile_id);
        if !self.exists(&name)? {
            return Ok(());
        }
        self.request(
            "POST",
            &format!("/collections/{name}/points/delete?wait=true"),
            Some(&json!({"points": [point_id(id)]})),
        )?;
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
        let profile = self.profile_of(scope, profile_id)?.ok_or_else(|| {
            StorageError::ModelProfileMismatch {
                detail: format!("profile `{profile_id}` is not declared in this process"),
            }
        })?;
        if query.len() != profile.dimension {
            return Err(StorageError::ModelProfileMismatch {
                detail: format!(
                    "query dimension {} does not match profile dimension {}",
                    query.len(),
                    profile.dimension
                ),
            });
        }
        let name = collection_name(scope, profile_id);
        if !self.exists(&name)? {
            return Ok(Vec::new());
        }
        let mut body = json!({
            "vector": query,
            "limit": k,
            "with_payload": true,
        });
        if let Some(filter) = filter {
            if !filter.is_empty() {
                let must: Vec<Value> = filter
                    .iter()
                    .map(|(key, value)| json!({"key": payload_key(key), "match": {"value": value}}))
                    .collect();
                body["filter"] = json!({"must": must});
            }
        }
        let response = self.request(
            "POST",
            &format!("/collections/{name}/points/search"),
            Some(&body),
        )?;
        let mut hits = Vec::new();
        for point in response
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let payload = point.get("payload").cloned().unwrap_or(Value::Null);
            let Some(id) = payload.get("id").and_then(Value::as_str) else {
                continue;
            };
            hits.push(VectorHit {
                id: id.to_string(),
                score: point.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                revision: payload
                    .get("revision")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        Ok(hits)
    }
}

/// Qdrant serves vectors; full text stays in the canonical adapter.
impl FullTextPort for QdrantStore {
    fn index(&self, scope: &ScopeRef, analyzer: &str, doc: &FullTextDoc) -> PortResult<()> {
        self.fulltext.index(scope, analyzer, doc)
    }

    fn delete(&self, scope: &ScopeRef, analyzer: &str, id: &str) -> PortResult<()> {
        self.fulltext.delete(scope, analyzer, id)
    }

    fn search(
        &self,
        scope: &ScopeRef,
        analyzer: &str,
        field: Option<&str>,
        query: &str,
        limit: usize,
    ) -> PortResult<Vec<FullTextHit>> {
        self.fulltext.search(scope, analyzer, field, query, limit)
    }

    fn analyzers(&self) -> Vec<String> {
        self.fulltext.analyzers()
    }
}

impl CapabilityProbe for QdrantStore {
    fn capabilities(&self) -> AdapterCapabilities {
        capabilities_with(Some(&self.base))
    }
}

/// Capabilities as the router sees them: available only with a real URL.
pub fn capabilities_with(url: Option<&str>) -> AdapterCapabilities {
    match url {
        Some(url) if !url.trim().is_empty() => AdapterCapabilities {
            adapter_id: "qdrant".to_string(),
            adapter_version: ADAPTER_VERSION.to_string(),
            backend_kind: BackendKind::Hybrid,
            logical_contract_version: "1.2.3".to_string(),
            adapter_schema_version: "1".to_string(),
            supported_ports: vec!["VectorPort".to_string()],
            transaction_scope: TransactionScope::SingleKey,
            atomic_batch: false,
            compare_and_swap: false,
            snapshot_reads: false,
            acl_filtering: true,
            stable_cursor: false,
            native_graph_traversal: false,
            vector_filters: true,
            fulltext_analyzers: Vec::new(),
            durable_outbox: false,
            backup_restore: true,
            max_batch_size: 1000,
            available: true,
            unavailable_reason: String::new(),
        },
        _ => unavailable_adapter(
            "qdrant",
            BackendKind::Hybrid,
            "no Qdrant endpoint configured: set UKMA_QDRANT_URL to enable profile C's vector port",
        ),
    }
}

/// Capabilities from the environment.
pub fn capabilities() -> AdapterCapabilities {
    let url = std::env::var(URL_ENV).ok();
    capabilities_with(url.as_deref())
}

/// Whether [`URL_ENV`] is set.
pub fn is_configured() -> bool {
    std::env::var(URL_ENV)
        .map(|url| !url.trim().is_empty())
        .unwrap_or(false)
}

/// Metadata is not Qdrant's job; the canonical adapter owns it.
pub fn metadata_unavailable() -> PortResult<()> {
    Err(StorageError::CapabilityUnavailable {
        capability: "MetadataPort",
    })
}

/// Kept so the signature of a future metadata delegate is obvious.
pub fn metadata_port() -> Option<&'static dyn MetadataPort> {
    None
}
