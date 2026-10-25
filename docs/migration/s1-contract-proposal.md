# UKMA Phase 7 — S1 Contract Proposal (Graph / Storage)

**Status: APPROVED via ADR-031 (ACCEPTED, 2026-10-25) — the three pending
decisions (§6) are resolved in `docs/adr/ADR-031-s1-graph-contract-decisions.md`;
S1 implementation is authorized by owner instruction. Wire-level public API
semantics remain locked per v1.2.3 (this proposal is an addition, not a
modification; 65/65 closure NOT claimed).**

Source basis: upstream SHA `590c2ca7dd7b37bd1d41caf45fac535b6522f969`
(observed only; nothing invented).

## 1. Upstream API observed (for mapping reference)

- Core types (`crates/codegraph/src/graph/types.rs`):
  - `NodeId = u64`, `EdgeId = u64` (monotonic counters, vendor-internal)
  - `NodeType` (8 variants), `EdgeType` (12 variants), `Direction` (3 variants)
  - `Node { id, node_type, properties: PropertyMap }`, `Edge { id, source_id, target_id, edge_type, properties }`
- Graph API (`crates/codegraph/src/graph/codegraph.rs`, 34 pub methods):
  constructors `in_memory()` / `with_backend(Box<dyn StorageBackend>)`;
  CRUD `add_node/get_node/get_node_mut/update_node_properties/delete_node/add_edge/get_edge/delete_edge`;
  queries `get_neighbors/get_neighbors_by_edge_type/get_edges_between`;
  batch `add_nodes_batch/add_edges_batch`;
  iteration `node_count/edge_count/nodes_iter/iter_nodes/iter_edges`;
  lifecycle `clear/flush/close/detach_storage/persist_to`;
  algorithms `bfs/dfs/find_strongly_connected_components/find_all_paths`;
  export `export_dot/export_dot_styled/export_json/export_json_filtered/export_csv_*/export_triples`.
- Storage (`crates/codegraph/src/storage/mod.rs`): `StorageBackend` trait
  (`put/get/delete/exists/scan_prefix/scan_prefix_keys/write_batch/flush`, `KeyValue = (Vec<u8>, Vec<u8>)`);
  impls: `MemoryBackend`, `NamespacedBackend`, `RocksDBBackend` (feature-gated `rocksdb-backend`, default ON upstream).

## 2. Proposed UKMA S1 mapping (proposal, not yet approved)

| Upstream | UKMA target | Notes |
|---|---|---|
| `CodeGraph` | `ukma-graph-engine` (new crate, created only after S1 gate) | algorithms (bfs/dfs/SCC/paths) ported per plan §6 |
| `Node/Edge` | UKMA canonical `GraphNode/GraphEdge` | property bag preserved; boundary validation added |
| `NodeId/EdgeId = u64` | **NOT exposed** as canonical IDs | S1 rule: canonical IDs stable across adapter substitution; vendor u64 counters stay internal |
| `StorageBackend` (KV) | UKMA Storage Port (Domain trait) | upstream trait is a good reference; UKMA port must be database-agnostic |
| `MemoryBackend` | UKMA Memory Adapter (test + dev) | one of the two required adapters |
| `RocksDBBackend` | optional adapter only, feature-gated | never imported by Domain crates (ADR-021..024) |
| `NodeType/EdgeType` | UKMA enums (versioned) | wire form to be fixed in contract; not finalized in S0 |
| thiserror errors | UKMA ErrorContract codes | NotFound/Conflict/CAPABILITY_UNAVAILABLE per v1.2.3 registry |

## 3. S1 semantics UKMA MUST add (upstream has none of these)

1. **ACL filtering before results**: traversal returns only nodes/edges visible to `AuthContext`;
   relationship existence must not leak cross-project data.
2. **Canonical IDs stable across adapter substitution** (backend replacement does not change external IDs).
3. **Commit + Outbox single authority**: one update = one commit + outbox event;
   projection failure marks projection lag, never rolls committed data to failed.
4. **Idempotency**: same key + same payload fingerprint replays the same result;
   same key + different payload → Conflict (mirrors Phase 6R JsonFileAdapter semantics).
5. **Capability negotiation**: non-portable native query features declared explicitly;
   unavailable → `CAPABILITY_UNAVAILABLE` (exact wire error per v1.2.3 registry — registry value pending approval).
6. **ETag / optimistic concurrency** on graph updates (Phase 6R pattern).

## 4. Risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | Upstream `NodeId/EdgeId` u64 counters leak as external IDs | High | Canonical ID layer in S1; golden test asserts stability across backend swap |
| R2 | `rocksdb` default feature ON upstream — accidental vendor coupling | High | Port without the feature; Domain crates never `dep:rocksdb`; CI fails on SDK import in Domain |
| R3 | Upstream has no ACL/tenant model — porting as-is ships a privacy hole | High | UKMA AuthContext mandatory in S1 contract before any query API exposure |
| R4 | Toolchain deviation: upstream pin 1.94.1 (msvc) vs executed 1.99.1-gnu (host has no MSVC linker) | Medium | Record deviation in upstream-lock; re-run pinned toolchain in CI before S1 merge |
| R5 | `PropertyMap` untyped bag vs UKMA versioned schema | Medium | Boundary validation + versioned schema in contract; bag preserved internally |
| R6 | Upstream `StorageBackend.write_batch` atomicity guarantees differ from UKMA outbox/transaction model | Medium | S1 contract defines transaction boundary; adapters declare capability |
| R7 | 38 language parsers (S2) share `codegraph-parser-api`; parser API churn could break S1 assumptions | Low | S1 depends only on graph/storage crates, not parser crates |
| R8 | Workspace comment says 27 parsers, actual 38 — stale doc at locked SHA | Low | Recorded as observed fact in source-map; no correction applied upstream |

## 5. Acceptance (S1 gate, per plan §6)

- `cargo fmt --all -- --check` / `cargo check --workspace --all-targets` /
  `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace --all-targets`
  actually executed and green.
- ID/traversal parity vs upstream (golden fixture `tests/golden/graph-basic-ops.golden.json`),
  ACL isolation, revision invalidation, ETag, outbox replay, backend substitution.
- **A single Memory Adapter + JSON dev adapter does NOT prove cross-production-DB portability** — stated, not claimed.

## 6. Decisions — RESOLVED by ADR-031 (ACCEPTED, 2026-10-25)

1. Canonical ID format → structured string `graph.node.<ULID>` / `graph.edge.<ULID>` (see ADR-031 D1).
2. `CAPABILITY_UNAVAILABLE` → spec-registered ErrorCode used as-is (ADR-031 D2).
3. `NodeType/EdgeType` wire names → new addition in snake_case v1, upstream semantics adopted (ADR-031 D3).

Full text: `docs/adr/ADR-031-s1-graph-contract-decisions.md`.
