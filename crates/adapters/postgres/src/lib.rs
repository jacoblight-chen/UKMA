//! PostgreSQL adapter — **NOT IMPLEMENTED** (profile B, `sql`).
//!
//! Status: this crate exists so the topology can be declared and so the router
//! fails closed instead of silently degrading (ADR-038, §3.1 "缺少可工作的
//! Adapter 時 MUST UNAVAILABLE_ADAPTER").
//!
//! What is intentionally absent:
//! - no `postgres`/`sqlx` dependency (ADR-021: no vendor SDK outside adapters,
//!   and none is added before the adapter is actually implemented);
//! - no schema DDL, no migration runner, no connection handling.
//!
//! Acceptance criteria before this adapter may report `available = true`
//! (see `docs/UKMA_v1.2.3_IMPLEMENTATION_PLAN.md` P1-WP4):
//! 1. implement `MetadataPort`, `OutboxPort`, `FullTextPort` (and graph
//!    traversal or an explicit `CAPABILITY_UNAVAILABLE` for it);
//! 2. pass the whole `crates/storage-ports/tests/port_contract.rs` suite;
//! 3. pass three-way parity with the embedded adapter (`DB-002`);
//! 4. never load RocksDB anywhere in the process (`DB-008`).

use storage_ports::capabilities::{AdapterCapabilities, BackendKind};

/// Probed capabilities: unavailable, with a reason an operator can act on.
pub fn capabilities() -> AdapterCapabilities {
    storage_ports::capabilities::unavailable_adapter(
        "postgres",
        BackendKind::Sql,
        "adapter not implemented: no client dependency, no DDL, no contract tests yet",
    )
}
