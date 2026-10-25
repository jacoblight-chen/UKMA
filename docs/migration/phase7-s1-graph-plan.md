# UKMA Phase 7 — S1 Graph Engine Migration Plan (Source List + Contract Mapping)

**Status: PROPOSAL — requires explicit architecture approval before any S1 code is written.**
Wire-level public API semantics are locked per UKMA v1.2.3; the 65-operation registry is
preliminary — **65/65 closure is NOT claimed**.

Source basis: upstream SHA `590c2ca7dd7b37bd1d41caf45fac535b6522f969` (observed only,
nothing invented). See `s1-contract-proposal.md` (detailed mapping + risks),
`crate-dependency-analysis.md` (module boundaries), `source-map.yaml` (dispositions).

## 1. Mandatory constraints (inherited, non-negotiable)

- UKMA runtime is **Rust** only; no Python runtime; Rust contract tests (never Python-substituted).
- **DSH Core is not modified.**
- The upstream reference repository is **read-only** — no modifications.
- **No RocksDB physical schema is used as a UKMA Domain Contract** (RocksDB at most an
  optional, feature-gated adapter; Domain crates never `dep:rocksdb`).
- **No re-implementation of existing, portable, mature algorithms** — upstream
  bfs/dfs/SCC/all-paths are ported/reused per plan §6, not rewritten from scratch.
- **No changes to established UKMA API semantics** (v1.2.3 wire form is locked).
- Features that have not passed tests are **never marked complete** (every success must
  show the executed command + actual result).

## 2. S1 source file list (from `crates/codegraph` only — observed at locked SHA)

| File | Disposition | UKMA target |
|---|---|---|
| `crates/codegraph/src/graph/types.rs` | rewrite | canonical `GraphNode/GraphEdge/NodeType/EdgeType/Direction` (IDs decoupled from vendor u64 counters) |
| `crates/codegraph/src/graph/property.rs` | rewrite | `PropertyMap/PropertyValue` + boundary validation + versioned schema |
| `crates/codegraph/src/graph/codegraph.rs` | rewrite | `ukma-graph-engine` public API (34-method surface re-mapped to UKMA ports) |
| `crates/codegraph/src/graph/algorithms.rs` | **reuse (port, not reimplement)** | bfs/dfs/SCC/all-paths — mature algorithms, ported per plan §6 |
| `crates/codegraph/src/graph/mod.rs` | rewrite | module glue |
| `crates/codegraph/src/storage/mod.rs` | rewrite | **UKMA Storage Port (Domain trait)** — database-agnostic |
| `crates/codegraph/src/storage/memory.rs` | rewrite | **Memory Adapter** (required adapter #1) |
| `crates/codegraph/src/storage/namespaced.rs` | rewrite | namespaced wrapper (test/dev) |
| `crates/codegraph/src/storage/rocksdb_backend.rs` | **reference only** | optional adapter candidate — feature-gated, never imported by Domain |
| `crates/codegraph/src/error.rs` | rewrite | UKMA ErrorContract codes (NotFound/Conflict/CAPABILITY_UNAVAILABLE) |
| `crates/codegraph/tests/unit/graph_ops_test.rs` | rewrite | UKMA contract tests (ID/traversal parity vs golden) |
| `crates/codegraph/tests/integration/memory_backend_test.rs` | rewrite | Memory adapter integration tests |

**Explicitly out of S1 scope:** `src/query.rs`, `src/metadata.rs`, `src/export/` (S3),
all parser crates (S2), `codegraph-memory` (S5), `codegraph-server` (S6).
Golden parity fixture: `tests/golden/graph-basic-ops.golden.json` (S0-prepared, from
observed upstream test semantics).

## 3. Contract mapping (summary — full table in `s1-contract-proposal.md` §2)

| Upstream (observed) | UKMA target |
|---|---|
| `CodeGraph` (34 pub methods) | `ukma-graph-engine` (created only after S1 gate) |
| `Node/Edge` + `PropertyMap` | canonical `GraphNode/GraphEdge` + validated property bag |
| `NodeId/EdgeId = u64` counters | **NOT exposed** — canonical ID layer stable across adapter substitution (counters stay vendor-internal) |
| `StorageBackend` KV trait | UKMA Storage Port (Domain, database-agnostic) |
| `MemoryBackend` | UKMA Memory Adapter (test + dev) |
| `RocksDBBackend` (feature `rocksdb-backend`, default ON upstream) | optional adapter, feature-gated; **never** a Domain dependency |
| `NodeType` (8) / `EdgeType` (12) | versioned UKMA enums (wire form pending approval) |
| thiserror errors | UKMA ErrorContract codes |

## 4. Semantics UKMA MUST add (upstream has none)

1. ACL filtering before results (AuthContext; no cross-project existence leakage)
2. Canonical IDs stable across adapter substitution
3. Commit + Outbox single authority (projection lag never rolls back committed data)
4. Idempotency (same key+payload replay; different payload → Conflict)
5. Capability negotiation → `CAPABILITY_UNAVAILABLE` (wire code pending approval)
6. ETag / optimistic concurrency on graph updates

## 5. Acceptance gate (plan §6 — all must be actually executed and green)

- `cargo fmt --all -- --check` / `cargo check --workspace --all-targets` /
  `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace --all-targets`
- Parity vs upstream: golden `tests/golden/graph-basic-ops.golden.json`
  (ID/traversal), ACL isolation, revision invalidation, ETag, outbox replay,
  backend substitution (Memory ↔ second adapter).
- A Memory Adapter + JSON dev adapter does **not** prove cross-production-DB portability
  — stated, not claimed.

## 6. Risks (full table in `s1-contract-proposal.md` §4)

R1 u64 ID leak (High) · R2 rocksdb default-ON coupling (High) · R3 no upstream ACL
model (High) · R4 toolchain deviation 1.94.1-msvc vs 1.99.1-gnu (Medium) ·
R5 untyped PropertyMap (Medium) · R6 write_batch atomicity vs outbox model (Medium) ·
R7 parser churn (Low) · R8 stale "27 parsers" doc (Low; actual 38, verified).

## 7. Decisions — RESOLVED by ADR-031 (ACCEPTED, 2026-10-25)

1. **Canonical ID format** → structured string `graph.node.<ULID>` /
   `graph.edge.<ULID>`, engine-assigned at creation via injectable `IdGenerator`,
   persisted → stable across adapter substitution (ADR-023, PROP-02; spec
   `ID = string`; vendor u64 counters stay internal; golden parity via structure
   + deterministic mapping, never raw u64).
2. **CAPABILITY_UNAVAILABLE** → the spec-registered ErrorCode used as-is
   (non-retryable; adapter capability probes, never config claims). No new wire
   code invented.
3. **NodeType/EdgeType wire names** → new addition (no graph operations exist in
   the 65 registry — nothing established is modified): v1 adopts the upstream
   semantics in snake_case wire form (`code_file`…`generic`; `imports`…
   `runtime_calls`; `outgoing`/`incoming`/`both`), versioned, unknown values
   rejected. 65/65 closure NOT claimed.

See `docs/adr/ADR-031-s1-graph-contract-decisions.md`.

**Additional S1 constraints:**
- Ported tests use cross-platform temp paths (`std::env::temp_dir()`); the
  upstream `/tmp/` hardcoding is never carried over (accepted known upstream
  defect).
- **Deterministic bounded termination (PROP-03):** traversal APIs require
  `max_depth`/`max_nodes` bounds with hard limits and sorted-adjacency ordering.

**S1 code start is authorized by owner instruction with ADR-031 in force;
document delivery never substitutes for code acceptance.**
