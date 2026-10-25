//! Neo4j adapter — **NOT IMPLEMENTED** (profile C, `hybrid`, graph engine).
//!
//! Status: declared-unavailable placeholder (ADR-038).
//!
//! Acceptance criteria before `available = true`:
//! 1. implement `GraphPort` (`graph_engine::GraphStore`) with per-hop ACL and
//!    bounded traversal (`PROP-03`);
//! 2. prove logical-ID stability across backends (`DB-003`, `PROP-02`);
//! 3. never let Cypher specifics leak into the domain (`ADR-021`).

use storage_ports::capabilities::{AdapterCapabilities, BackendKind};

/// Probed capabilities: unavailable.
pub fn capabilities() -> AdapterCapabilities {
    storage_ports::capabilities::unavailable_adapter(
        "neo4j",
        BackendKind::Hybrid,
        "adapter not implemented: no client dependency and no graph port tests yet",
    )
}
