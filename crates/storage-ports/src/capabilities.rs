//! Adapter capability negotiation (§6.3) and the minimum backend gate (§40.5).
//!
//! Values MUST come from real probes, never from configuration claims
//! (ADR-022). A missing *required* capability fails startup
//! (`CAPABILITY_UNAVAILABLE`), it is never silently degraded (ADR-038).

use serde::{Deserialize, Serialize};

use crate::error::{PortResult, StorageError};

/// Deployment profile a backend belongs to (§28.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Embedded store: no cross-engine ACID, single authoritative writer.
    Embedded,
    /// SQL backend, optionally carrying graph traversal.
    Sql,
    /// Multiple specialist engines behind outbox projections.
    Hybrid,
}

/// How wide a transaction's guarantee is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionScope {
    /// No transactions at all.
    None,
    /// Single aggregate / single key.
    SingleKey,
    /// Multiple aggregates in one store.
    MultiAggregate,
    /// Across stores; only valid where the backend genuinely provides it.
    CrossStore,
}

/// Full adapter capability declaration (§6.3, 17 fields + availability).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterCapabilities {
    /// Stable adapter identity.
    pub adapter_id: String,
    /// Adapter version.
    pub adapter_version: String,
    /// Backend family.
    pub backend_kind: BackendKind,
    /// Version of the logical contract implemented.
    pub logical_contract_version: String,
    /// Backend schema version (evolves independently, §6.5).
    pub adapter_schema_version: String,
    /// Ports this adapter implements.
    pub supported_ports: Vec<String>,
    /// Transaction scope actually provided.
    pub transaction_scope: TransactionScope,
    /// Batches are applied atomically.
    pub atomic_batch: bool,
    /// Compare-and-swap is available.
    pub compare_and_swap: bool,
    /// Snapshot reads are available.
    pub snapshot_reads: bool,
    /// ACL filtering happens inside the store.
    pub acl_filtering: bool,
    /// Cursors are stable across calls.
    pub stable_cursor: bool,
    /// Native graph traversal exists.
    pub native_graph_traversal: bool,
    /// Vector search supports metadata filters.
    pub vector_filters: bool,
    /// Full-text analyzers available.
    pub fulltext_analyzers: Vec<String>,
    /// Outbox is durable.
    pub durable_outbox: bool,
    /// Backup/restore is supported.
    pub backup_restore: bool,
    /// Maximum batch size.
    pub max_batch_size: u32,
    /// Probe result: `false` means the adapter exists but cannot serve traffic.
    pub available: bool,
    /// Why the adapter is unavailable (empty when available).
    pub unavailable_reason: String,
}

impl AdapterCapabilities {
    /// Capability names required by the minimum production profile (§40.5).
    pub const MINIMUM_REQUIRED: [&'static str; 8] = [
        "durable_canonical_transaction",
        "write_idempotency",
        "compare_and_swap",
        "typed_key_equality",
        "revisioned_read",
        "delete_tombstone",
        "durable_outbox",
        "scoped_acl_enforcement",
    ];

    /// Names of the capabilities this adapter actually provides.
    pub fn provided(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.compare_and_swap {
            names.push("compare_and_swap");
            names.push("typed_key_equality");
        }
        if self.snapshot_reads {
            names.push("revisioned_read");
        }
        if self.durable_outbox {
            names.push("durable_outbox");
            names.push("durable_canonical_transaction");
            names.push("delete_tombstone");
        }
        if self.acl_filtering {
            names.push("scoped_acl_enforcement");
        }
        if self.atomic_batch {
            names.push("write_idempotency");
        }
        names
    }

    /// Capabilities required by the minimum profile that this adapter lacks.
    pub fn missing_required(&self) -> Vec<&'static str> {
        let provided = self.provided();
        Self::MINIMUM_REQUIRED
            .into_iter()
            .filter(|name| !provided.contains(name))
            .collect()
    }

    /// Fail-closed startup gate (§6.3, §40.5, ADR-038).
    pub fn assert_minimum_profile(&self) -> PortResult<()> {
        if !self.available {
            return Err(StorageError::CapabilityUnavailable {
                capability: "adapter_available",
            });
        }
        let missing = self.missing_required();
        if !missing.is_empty() {
            // The missing set is reported through the message so operators can act.
            return Err(StorageError::Internal(format!(
                "adapter {} lacks required capabilities: {}",
                self.adapter_id,
                missing.join(", ")
            )));
        }
        Ok(())
    }
}

/// An adapter declaration for a backend that exists in the target topology but
/// cannot serve traffic yet. `available = false` and an empty port list make
/// [`StorageRouter::assert_ready`] fail closed instead of silently degrading
/// (§3.1, §6.3, §40.5).
pub fn unavailable_adapter(
    adapter_id: &str,
    backend_kind: BackendKind,
    reason: &str,
) -> AdapterCapabilities {
    AdapterCapabilities {
        adapter_id: adapter_id.to_string(),
        adapter_version: "0.0.0".to_string(),
        backend_kind,
        logical_contract_version: "1.2.3".to_string(),
        adapter_schema_version: "0".to_string(),
        supported_ports: Vec::new(),
        transaction_scope: TransactionScope::None,
        atomic_batch: false,
        compare_and_swap: false,
        snapshot_reads: false,
        acl_filtering: false,
        stable_cursor: false,
        native_graph_traversal: false,
        vector_filters: false,
        fulltext_analyzers: Vec::new(),
        durable_outbox: false,
        backup_restore: false,
        max_batch_size: 0,
        available: false,
        unavailable_reason: reason.to_string(),
    }
}

/// An adapter that publishes its probed capabilities.
pub trait CapabilityProbe {
    /// Probed capabilities of this adapter instance.
    fn capabilities(&self) -> AdapterCapabilities;
}
