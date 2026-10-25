# UKMA Contracts Engineering — Phase 2 report

**Scope**: Source-grounded schema refinement, event registry, API-to-auth/storage/test matrix, provisional MCP tooling manifest, additive Rust shared types and executable verification.

## Verified inventory

- Domain operations: **65** ({'code': 13, 'document': 14, 'memory': 25, 'traceability': 13}).
- Source-listed distinct event types: **30**, envelopes specified; payload contracts **unresolved**.
- Operation request/response schemas: **130 provisional**, shared JSON Schema enhanced with `Hash`, `ISODate`, `Span`, `Evidence`, `PageRequest`, `JobHandle`, `ValidityVector`, and `OperationOutcome`.
- MCP operation mappings: **65 provisional**, *not deployable*.

## Critical integrity restrictions

1. The v1.2.3 Markdown often provides response shapes only as references (e.g. `Symbol` or `JobView`). These have **not** been invented or marked closed.
2. `JobView` in shared `$defs` is only a marker. Its full inherited shape cannot be faithfully represented by the placeholder in this phase; **do not treat it as a complete schema**.
3. Event envelopes can be checked, but no event-specific payload has been fabricated. All events remain release-blocking.
4. Access-control matrix quotes explicit scopes; the rest of policy remains runtime work. All per-operation storage owners remain unresolved.
5. `ukma-contracts` Rust code is uncompiled in this execution environment (no Cargo binary found); no Rust compilation claim.
6. Any migration of CodeGraph source, adapters or actual domain engines is **out of scope** for this phase.

## Verification and release gate

```bash
python scripts/verify_contracts.py
python scripts/verify_phase2.py
python scripts/verify_phase2.py --strict   # MUST FAIL with exit code 2
cargo test --workspace               # Run in CI with Rust installed
```

## Phase 3 blocking tasks

- Convert every domain type in specification §§32–35 to a single canonical machine-readable IDL.
- Instantiate 65 fully typed request and response schemas with field-level invariants, plus positive/negative examples.
- Specify event payload schema for all 30 event types; cross-check event-to-operation mapping.
- Define the exact permission decision model, transaction/ownership model, state transitions, and corresponding executable tests.
- Cross-check Rust serde serialization against generated JSON Schemas and compile every public API method.
- Implement and certify at least two storage backends in the required deployment profile, and enforce source snapshot and scope behavior.
