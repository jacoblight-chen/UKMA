# UKMA Phase 7 S1 — Contract Gap Report

**Date:** 2026-10-25 · **Status:** gaps resolved by design (additions only; no established
v1.2.3 semantics modified; 65/65 closure NOT claimed)

## Gap 1 — No domain ports in `ukma-contracts` yet

**Observation:** `ukma-contracts` (phase 1/2) contains only wire-level types
(`RequestEnvelope`/`ResponseEnvelope`/`ErrorDetail`/`ScopeRef`/`RequestContext` +
phase 2 commit/projection/validity types). The spec's Storage-Port concepts
(ADR-021..024, six Storage Ports + capability negotiation) have **no Rust trait
implementations yet** — no `GraphStore`, no Entity/Relation/Provenance domain
types exist in the workspace.

**Resolution (S1):** the `GraphStore` port is defined **inside the new
`graph-engine` crate** (per plan: the crate is created only after the S1
contract gate). It *uses* the established wire types as-is (`ScopeRef`-shaped
scope keys, `ErrorDetail` for error mapping, `RequestContext`'s
`idempotency_key`/`expected_etag` fields as the semantics reference) and adds no
new wire types to `ukma-contracts`. `ukma-contracts` is **not modified** in S1.

## Gap 2 — No graph operations in the 65-operation registry

**Observation:** the preliminary 65-operation registry contains no `graph.*`
operations; the spec's `ID = string` and ADR-023/PROP-02/PROP-03 are the only
normative graph-relevant rules.

**Resolution (S1):** the graph engine API is an **internal domain API** (not a
public wire API) — no wire semantics are added or changed. Public wire exposure
is deferred to S6 (gateway) with versioned operations. Resolved decisions per
ADR-031 (canonical string IDs; `CAPABILITY_UNAVAILABLE` as registered; graph
type wire names as a future S6 addition, snake_case v1).

## Gap 3 — Error-code coverage for graph failures

**Observation:** the registered `ErrorCode` union (spec §) covers all needed
failure classes; no new codes are required:

| S1 failure class | Registered code |
|---|---|
| entity not in scope | `NOT_FOUND` (never leaks existence) |
| optimistic concurrency mismatch | `ETAG_MISMATCH` |
| same key + different payload | `IDEMPOTENCY_CONFLICT` |
| adapter cannot provide capability | `CAPABILITY_UNAVAILABLE` |
| invalid argument / malformed ID | `INVALID_ARGUMENT` |
| cross-scope write attempt | `CROSS_SCOPE_DENIED` |
| traversal bound exceeded | `RESOURCE_LIMIT` |
| unexpected internal failure | `INTERNAL` |

## Gap 4 — Capability negotiation has no Rust surface

**Observation:** ADR-022 requires capability probing (never config claims).

**Resolution (S1):** `GraphStore::capabilities() -> StoreCapabilities`
(persistent, atomic_batches) is the probe surface; the engine returns
`CAPABILITY_UNAVAILABLE` for operations the active store cannot provide
(e.g., durable `flush` on a non-persistent store).

**Non-gaps (explicitly verified):** RocksDB is feature-gated upstream and stays
an optional adapter outside Domain (never a Domain dependency); no Python test
substitution (all S1 contract tests are Rust).
