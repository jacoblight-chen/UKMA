//! Storage router: the single place that decides which adapter serves a port.
//!
//! Rules (§6.3, §28.2, §40.5, ADR-038):
//! - every port the deployment requires MUST have an adapter;
//! - every such adapter MUST be `available`;
//! - adapters holding **canonical** data (metadata, outbox) MUST satisfy the
//!   minimum production profile, otherwise startup fails;
//! - unavailable adapters are never silently substituted.

pub mod cutover;

use std::sync::Arc;

use storage_ports::capabilities::{AdapterCapabilities, CapabilityProbe};
use storage_ports::error::{PortResult, StorageError};
use storage_ports::{ArtifactPort, FullTextPort, GraphPort, MetadataPort, OutboxPort, VectorPort};

/// Ports the deployment can require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Port {
    /// Graph storage (`graph_engine::GraphStore`).
    Graph,
    /// Canonical metadata.
    Metadata,
    /// Full-text projection.
    FullText,
    /// Vector projection.
    Vector,
    /// Immutable artifacts.
    Artifact,
    /// Durable outbox.
    Outbox,
}

impl Port {
    /// Stable name used in capability declarations.
    pub fn as_str(self) -> &'static str {
        match self {
            Port::Graph => "GraphPort",
            Port::Metadata => "MetadataPort",
            Port::FullText => "FullTextPort",
            Port::Vector => "VectorPort",
            Port::Artifact => "ArtifactPort",
            Port::Outbox => "OutboxPort",
        }
    }

    /// Whether the port holds canonical (non-rebuildable) data.
    pub fn is_canonical(self) -> bool {
        matches!(
            self,
            Port::Graph | Port::Metadata | Port::Artifact | Port::Outbox
        )
    }
}

/// All ports wired to concrete adapters.
pub struct StorageRouter {
    graph: Arc<dyn GraphPort>,
    metadata: Arc<dyn MetadataPort>,
    fulltext: Arc<dyn FullTextPort>,
    vector: Arc<dyn VectorPort>,
    artifact: Arc<dyn ArtifactPort>,
    outbox: Arc<dyn OutboxPort>,
    capabilities: Vec<AdapterCapabilities>,
}

impl StorageRouter {
    /// Build a router from concrete adapters.
    pub fn new(
        graph: Arc<dyn GraphPort>,
        metadata: Arc<dyn MetadataPort>,
        fulltext: Arc<dyn FullTextPort>,
        vector: Arc<dyn VectorPort>,
        artifact: Arc<dyn ArtifactPort>,
        outbox: Arc<dyn OutboxPort>,
    ) -> Self {
        Self {
            graph,
            metadata,
            fulltext,
            vector,
            artifact,
            outbox,
            capabilities: Vec::new(),
        }
    }

    /// Attach probed capabilities (last write wins per `adapter_id`).
    pub fn with_capabilities(mut self, capabilities: Vec<AdapterCapabilities>) -> Self {
        for capability in capabilities {
            self.capabilities
                .retain(|existing| existing.adapter_id != capability.adapter_id);
            self.capabilities.push(capability);
        }
        self
    }

    /// Probed capabilities of every attached adapter.
    pub fn capabilities(&self) -> &[AdapterCapabilities] {
        &self.capabilities
    }

    /// Fail-closed readiness check for the ports a profile requires.
    pub fn assert_ready(&self, required: &[Port]) -> PortResult<()> {
        for port in required {
            let serving: Vec<&AdapterCapabilities> = self
                .capabilities
                .iter()
                .filter(|adapter| {
                    adapter
                        .supported_ports
                        .iter()
                        .any(|name| name == port.as_str())
                })
                .collect();
            if serving.is_empty() {
                return Err(StorageError::CapabilityUnavailable {
                    capability: port.as_str(),
                });
            }
            if !serving.iter().any(|adapter| adapter.available) {
                let reason = serving
                    .iter()
                    .map(|adapter| {
                        format!(
                            "{}: {}",
                            adapter.adapter_id,
                            adapter.unavailable_reason.as_str()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(StorageError::Internal(format!(
                    "no available adapter for {} ({reason})",
                    port.as_str()
                )));
            }
            if port.is_canonical() {
                let passing = serving
                    .iter()
                    .filter(|adapter| adapter.available)
                    .any(|adapter| adapter.assert_minimum_profile().is_ok());
                if !passing {
                    return Err(StorageError::Internal(format!(
                        "no adapter for {} satisfies the minimum production profile (§40.5)",
                        port.as_str()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Graph port.
    pub fn graph(&self) -> &Arc<dyn GraphPort> {
        &self.graph
    }

    /// Metadata port.
    pub fn metadata(&self) -> &Arc<dyn MetadataPort> {
        &self.metadata
    }

    /// Full-text port.
    pub fn fulltext(&self) -> &Arc<dyn FullTextPort> {
        &self.fulltext
    }

    /// Vector port.
    pub fn vector(&self) -> &Arc<dyn VectorPort> {
        &self.vector
    }

    /// Artifact port.
    pub fn artifact(&self) -> &Arc<dyn ArtifactPort> {
        &self.artifact
    }

    /// Outbox port.
    pub fn outbox(&self) -> &Arc<dyn OutboxPort> {
        &self.outbox
    }

    /// Adapter ids that advertise a port, available or not.
    pub fn adapters_for(&self, port: Port) -> Vec<&str> {
        self.capabilities
            .iter()
            .filter(|adapter| {
                adapter
                    .supported_ports
                    .iter()
                    .any(|name| name == port.as_str())
            })
            .map(|adapter| adapter.adapter_id.as_str())
            .collect()
    }
}

/// Collect capabilities from a heterogeneous set of probes.
pub fn probe_all(probes: &[&dyn CapabilityProbe]) -> Vec<AdapterCapabilities> {
    probes.iter().map(|probe| probe.capabilities()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage_ports::capabilities::BackendKind;
    use storage_ports::testing::{
        InMemoryArtifact, InMemoryFullText, InMemoryMetadata, InMemoryOutbox, InMemoryVector,
    };
    use storage_ports::TransactionScope;

    fn wired(capabilities: Vec<AdapterCapabilities>) -> StorageRouter {
        StorageRouter::new(
            Arc::new(graph_engine::InMemoryGraphStore::new()),
            Arc::new(InMemoryMetadata::new()),
            Arc::new(InMemoryFullText::new()),
            Arc::new(InMemoryVector::new()),
            Arc::new(InMemoryArtifact::new()),
            Arc::new(InMemoryOutbox::new()),
        )
        .with_capabilities(capabilities)
    }

    fn full_profile(adapter_id: &str) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter_id: adapter_id.to_string(),
            adapter_version: "0.1.0".into(),
            backend_kind: BackendKind::Embedded,
            logical_contract_version: "1.2.3".into(),
            adapter_schema_version: "1".into(),
            supported_ports: vec![
                "GraphPort".into(),
                "MetadataPort".into(),
                "FullTextPort".into(),
                "VectorPort".into(),
                "ArtifactPort".into(),
                "OutboxPort".into(),
            ],
            transaction_scope: TransactionScope::MultiAggregate,
            atomic_batch: true,
            compare_and_swap: true,
            snapshot_reads: true,
            acl_filtering: true,
            stable_cursor: true,
            native_graph_traversal: true,
            vector_filters: true,
            fulltext_analyzers: vec!["icu-v1".into()],
            durable_outbox: true,
            backup_restore: true,
            max_batch_size: 1000,
            available: true,
            unavailable_reason: String::new(),
        }
    }

    #[test]
    fn ready_when_every_required_port_is_served() {
        let router = wired(vec![full_profile("a")]);
        assert!(router
            .assert_ready(&[Port::Graph, Port::Metadata, Port::Outbox])
            .is_ok());
    }

    #[test]
    fn fails_when_a_required_port_has_no_adapter() {
        let mut partial = full_profile("a");
        partial.supported_ports = vec!["GraphPort".into()];
        let router = wired(vec![partial]);
        let error = router.assert_ready(&[Port::Vector]).unwrap_err();
        assert_eq!(error.code(), "CAPABILITY_UNAVAILABLE");
    }

    #[test]
    fn fails_when_the_only_adapter_is_unavailable() {
        let mut unavailable = full_profile("b");
        unavailable.available = false;
        unavailable.unavailable_reason = "no server configured".into();
        let router = wired(vec![unavailable]);
        let error = router.assert_ready(&[Port::Metadata]).unwrap_err();
        assert!(error.to_string().contains("no available adapter"));
    }

    #[test]
    fn canonical_port_requires_minimum_profile() {
        let mut weak = full_profile("weak");
        weak.durable_outbox = false; // canonical data would have no durable outbox
        let router = wired(vec![weak]);
        let error = router.assert_ready(&[Port::Outbox]).unwrap_err();
        assert!(error.to_string().contains("minimum production profile"));
    }

    #[test]
    fn derived_port_does_not_require_minimum_profile() {
        let mut weak = full_profile("weak");
        weak.durable_outbox = false;
        weak.compare_and_swap = false;
        weak.snapshot_reads = false;
        weak.acl_filtering = false;
        weak.atomic_batch = false;
        let router = wired(vec![weak]);
        assert!(router.assert_ready(&[Port::Vector]).is_ok());
    }

    #[test]
    fn graph_store_satisfies_the_graph_port() {
        // Compile-time proof that there is exactly one graph contract.
        let store: Arc<dyn GraphPort> = Arc::new(graph_engine::InMemoryGraphStore::new());
        assert!(!store.capabilities().persistent);
    }
}
