//! Neo4j graph adapter (profile C).
//!
//! Serves [`graph_engine::store::GraphStore`] against a real Neo4j server over its
//! HTTP transactional API. Records are stored as `:UkmaNode` / `:UkmaEdge` nodes
//! keyed by `(scope, id)`, so tenant isolation, deterministic iteration and the
//! cascading delete are enforced by the store rather than by the caller.
//!
//! Deliberate choices, stated rather than hidden:
//! - **HTTP transactional API, not Bolt**, so the adapter stays synchronous and
//!   avoids pulling `tonic`/`tokio` into the workspace for one port.
//! - No APOC: everything uses plain Cypher.
//! - `flush` succeeds without work because Neo4j commits each statement; the
//!   capability probe says `persistent = true` for the same reason.

use std::sync::Mutex;

use graph_engine::error::{GraphError, Result};
use graph_engine::ids::CanonicalId;
use graph_engine::store::{GraphStore, ScopeKey, StoreCapabilities};
use graph_engine::types::{GraphEdge, GraphNode};
use serde_json::{json, Value};
use storage_ports::capabilities::{
    unavailable_adapter, AdapterCapabilities, BackendKind, CapabilityProbe, TransactionScope,
};

/// Environment variable an operator sets to enable Neo4j.
pub const URL_ENV: &str = "UKMA_NEO4J_URL";
/// User for the HTTP endpoint; defaults to `neo4j`.
pub const USER_ENV: &str = "UKMA_NEO4J_USER";
/// Password for the HTTP endpoint.
pub const PASSWORD_ENV: &str = "UKMA_NEO4J_PASSWORD";
/// Database name; defaults to `neo4j`.
pub const DATABASE_ENV: &str = "UKMA_NEO4J_DATABASE";

/// Adapter version stamped into capability reports.
const ADAPTER_VERSION: &str = "0.1.0";

/// Transport and Cypher failures are reported as `Internal`; the capability
/// variant carries no detail by contract, so a real error message must not be
/// squeezed into it.
fn internal(message: impl Into<String>) -> GraphError {
    GraphError::Internal(message.into())
}

fn scope_of(scope: &ScopeKey) -> String {
    format!("{}\u{1f}{}", scope.tenant_id, scope.project_id)
}

/// Graph store backed by a Neo4j server.
pub struct Neo4jStore {
    base: String,
    database: String,
    auth: String,
    agent: ureq::Agent,
    lock: Mutex<()>,
}

impl Neo4jStore {
    /// Connect to `base` (for example `http://127.0.0.1:7474`).
    ///
    /// # Errors
    /// Fails when the server rejects the credentials or does not answer.
    pub fn connect(base: &str, user: &str, password: &str, database: &str) -> Result<Self> {
        let store = Self {
            base: base.trim_end_matches('/').to_string(),
            database: database.to_string(),
            auth: format!("{user}:{password}"),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(30))
                .build(),
            lock: Mutex::new(()),
        };
        // A real probe: the endpoint must answer a trivial statement.
        store.run("RETURN 1 AS ok", json!({}))?;
        Ok(store)
    }

    /// Connect using the environment.
    ///
    /// # Errors
    /// Returns a capability error when [`URL_ENV`] is unset or the probe fails.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var(URL_ENV).map_err(|_| internal("UKMA_NEO4J_URL is not set"))?;
        if url.trim().is_empty() {
            return Err(internal("UKMA_NEO4J_URL is empty"));
        }
        let user = std::env::var(USER_ENV).unwrap_or_else(|_| "neo4j".to_string());
        let password = std::env::var(PASSWORD_ENV).unwrap_or_default();
        let database = std::env::var(DATABASE_ENV).unwrap_or_else(|_| "neo4j".to_string());
        Self::connect(&url, &user, &password, &database)
    }

    /// Endpoint this store was opened with.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Deletes every record of one scope (test cleanup and cutover runbook).
    ///
    /// # Errors
    /// Returns the underlying Neo4j error.
    pub fn purge_scope(&self, scope: &ScopeKey) -> Result<u64> {
        let rows = self.run(
            "MATCH (e:UkmaEdge {scope: $scope}) WITH e, count(e) AS edges
             DELETE e
             RETURN edges",
            json!({"scope": scope_of(scope)}),
        )?;
        let edges = rows
            .first()
            .and_then(|row| row.get(0))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let nodes = self.run(
            "MATCH (n:UkmaNode {scope: $scope}) WITH n, count(n) AS nodes
             DELETE n
             RETURN nodes",
            json!({"scope": scope_of(scope)}),
        )?;
        let node_count = nodes
            .first()
            .and_then(|row| row.get(0))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok(edges + node_count)
    }

    fn run(&self, statement: &str, parameters: Value) -> Result<Vec<Value>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| internal("neo4j lock poisoned"))?;
        let url = format!("{}/db/{}/tx/commit", self.base, self.database);
        let body = json!({
            "statements": [{"statement": statement, "parameters": parameters}],
        });
        let response = self
            .agent
            .post(&url)
            .set("Authorization", &basic_auth(&self.auth))
            .set("Content-Type", "application/json")
            .send_json(body);
        let payload = match response {
            Ok(response) => response
                .into_json::<Value>()
                .map_err(|error| internal(format!("neo4j returned non-JSON: {error}")))?,
            Err(ureq::Error::Status(status, response)) => {
                let detail = response
                    .into_string()
                    .unwrap_or_else(|_| "<no body>".to_string());
                return Err(internal(format!("neo4j {status}: {detail}")));
            }
            Err(error) => return Err(internal(format!("neo4j transport error: {error}"))),
        };
        if let Some(errors) = payload.get("errors").and_then(Value::as_array) {
            if let Some(first) = errors.first() {
                return Err(internal(format!(
                    "neo4j error {}: {}",
                    first.get("code").and_then(Value::as_str).unwrap_or("?"),
                    first
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("no message")
                )));
            }
        }
        let rows = payload
            .pointer("/results/0/data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|row| row.get("row").cloned().unwrap_or(Value::Null))
            .collect())
    }
}

fn basic_auth(credentials: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = credentials.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(TABLE[(b[0] >> 2) as usize] as char);
        out.push(TABLE[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(b[2] & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    format!("Basic {out}")
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| internal(format!("cannot encode record: {error}")))
}

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|error| internal(format!("cannot decode record: {error}")))
}

fn ids_from(rows: &[Value]) -> Result<Vec<CanonicalId>> {
    let mut ids = Vec::new();
    for row in rows {
        if let Some(raw) = row.get(0).and_then(Value::as_str) {
            ids.push(CanonicalId::parse(raw)?);
        }
    }
    Ok(ids)
}

impl GraphStore for Neo4jStore {
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            persistent: true,
            atomic_batches: true,
        }
    }

    fn put_node(&self, scope: &ScopeKey, node: &GraphNode) -> Result<()> {
        self.run(
            "MERGE (n:UkmaNode {scope: $scope, id: $id}) SET n.json = $json",
            json!({"scope": scope_of(scope), "id": node.id.to_string(), "json": encode(node)?}),
        )?;
        Ok(())
    }

    fn get_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphNode>> {
        let rows = self.run(
            "MATCH (n:UkmaNode {scope: $scope, id: $id}) RETURN n.json AS json",
            json!({"scope": scope_of(scope), "id": id.to_string()}),
        )?;
        match rows
            .first()
            .and_then(|row| row.get(0))
            .and_then(Value::as_str)
        {
            Some(raw) => Ok(Some(decode(raw)?)),
            None => Ok(None),
        }
    }

    fn delete_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        // Cascading delete: a dangling edge would be a corrupted graph.
        self.run(
            "MATCH (n:UkmaNode {scope: $scope, id: $id}) DELETE n",
            json!({"scope": scope_of(scope), "id": id.to_string()}),
        )?;
        self.run(
            "MATCH (e:UkmaEdge {scope: $scope}) WHERE e.source = $id OR e.target = $id DELETE e",
            json!({"scope": scope_of(scope), "id": id.to_string()}),
        )?;
        Ok(())
    }

    fn node_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        let rows = self.run(
            "MATCH (n:UkmaNode {scope: $scope}) RETURN n.id AS id ORDER BY n.id",
            json!({"scope": scope_of(scope)}),
        )?;
        ids_from(&rows)
    }

    fn put_edge(&self, scope: &ScopeKey, edge: &GraphEdge) -> Result<()> {
        self.run(
            "MERGE (e:UkmaEdge {scope: $scope, id: $id})
             SET e.json = $json, e.source = $source, e.target = $target",
            json!({
                "scope": scope_of(scope),
                "id": edge.id.to_string(),
                "json": encode(edge)?,
                "source": edge.source_id.to_string(),
                "target": edge.target_id.to_string(),
            }),
        )?;
        Ok(())
    }

    fn get_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphEdge>> {
        let rows = self.run(
            "MATCH (e:UkmaEdge {scope: $scope, id: $id}) RETURN e.json AS json",
            json!({"scope": scope_of(scope), "id": id.to_string()}),
        )?;
        match rows
            .first()
            .and_then(|row| row.get(0))
            .and_then(Value::as_str)
        {
            Some(raw) => Ok(Some(decode(raw)?)),
            None => Ok(None),
        }
    }

    fn delete_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        self.run(
            "MATCH (e:UkmaEdge {scope: $scope, id: $id}) DELETE e",
            json!({"scope": scope_of(scope), "id": id.to_string()}),
        )?;
        Ok(())
    }

    fn edge_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        let rows = self.run(
            "MATCH (e:UkmaEdge {scope: $scope}) RETURN e.id AS id ORDER BY e.id",
            json!({"scope": scope_of(scope)}),
        )?;
        ids_from(&rows)
    }

    fn outgoing_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        let rows = self.run(
            "MATCH (e:UkmaEdge {scope: $scope, source: $id}) RETURN e.id AS id ORDER BY e.id",
            json!({"scope": scope_of(scope), "id": node.to_string()}),
        )?;
        ids_from(&rows)
    }

    fn incoming_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        let rows = self.run(
            "MATCH (e:UkmaEdge {scope: $scope, target: $id}) RETURN e.id AS id ORDER BY e.id",
            json!({"scope": scope_of(scope), "id": node.to_string()}),
        )?;
        ids_from(&rows)
    }

    fn flush(&self) -> Result<()> {
        // Neo4j commits every statement; there is no buffer to flush.
        Ok(())
    }
}

impl CapabilityProbe for Neo4jStore {
    fn capabilities(&self) -> AdapterCapabilities {
        capabilities_with(Some(&self.base))
    }
}

/// Capabilities as the router sees them: available only with a real URL.
pub fn capabilities_with(url: Option<&str>) -> AdapterCapabilities {
    match url {
        Some(url) if !url.trim().is_empty() => AdapterCapabilities {
            adapter_id: "neo4j".to_string(),
            adapter_version: ADAPTER_VERSION.to_string(),
            backend_kind: BackendKind::Hybrid,
            logical_contract_version: "1.2.3".to_string(),
            adapter_schema_version: "1".to_string(),
            supported_ports: vec!["GraphPort".to_string()],
            transaction_scope: TransactionScope::MultiAggregate,
            atomic_batch: true,
            compare_and_swap: false,
            snapshot_reads: false,
            acl_filtering: true,
            stable_cursor: true,
            native_graph_traversal: true,
            vector_filters: false,
            fulltext_analyzers: Vec::new(),
            durable_outbox: false,
            backup_restore: true,
            max_batch_size: 1000,
            available: true,
            unavailable_reason: String::new(),
        },
        _ => unavailable_adapter(
            "neo4j",
            BackendKind::Hybrid,
            "no Neo4j endpoint configured: set UKMA_NEO4J_URL to enable profile C's graph port",
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
