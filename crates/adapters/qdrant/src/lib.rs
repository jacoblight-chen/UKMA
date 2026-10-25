//! Qdrant adapter — **NOT IMPLEMENTED** (profile C, `hybrid`, vector engine).
//!
//! Status: declared-unavailable placeholder so the hybrid profile can be
//! described without pretending it works (ADR-038).
//!
//! Acceptance criteria before `available = true`:
//! 1. implement `VectorPort` with profile isolation, dimension/metric
//!    rejection (`MODEL_PROFILE_MISMATCH`) and metadata filters;
//! 2. pass `crates/storage-ports/tests/port_contract.rs` vector cases;
//! 3. report projection lag rather than violating §6.4 consistency semantics.

use storage_ports::capabilities::{AdapterCapabilities, BackendKind};

/// Probed capabilities: unavailable.
pub fn capabilities() -> AdapterCapabilities {
    storage_ports::capabilities::unavailable_adapter(
        "qdrant",
        BackendKind::Hybrid,
        "adapter not implemented: no client dependency and no vector contract tests yet",
    )
}
