# Phase 5 — Code Intelligence Runtime Contract Closure: semantic model

## Implementation
- Retains Phase 4 package with all 65 operations and 13 Code typed candidates.
- Adds 13-operation scoped policy matrix, three *proposed* code event payload schemas, deterministic reference runtime model, and runnable runtime tests.
- Tests cross-project isolation, job idempotency/cancellation, post-commit protection, failed projections, freshness gate, CAS, edge visibility and event dedup.

## Important limitations
- This is **contract-semantic verification**, not integration with CodeGraph source, a Rust binary, a database or an MCP host.
- Three event payload schemas are **proposals**, not yet approved normative contracts. Phase 4 event registry remains UNRESOLVED; no inaccurate closure status is asserted.
- Source spec does not specify every wire field or every downstream behavior. Rust/Cargo is unavailable locally. Status remains **0/65 fully closed**.
- Authorization is exercised using a deterministic model and does **not** establish real access-control enforcement. Backends still require at least two adapter implementations with contract parity testing.

## Commands
```bash
python scripts/verify_contracts.py
python scripts/verify_phase2.py
python scripts/verify_phase3.py
python scripts/verify_phase4.py
python scripts/verify_phase5.py
python scripts/verify_phase3.py --strict  # expected exit code 2
```

## Next release blockers
1. Approve JobHandle/JobView/ChangeReceipt final wire schemas and the three proposed payloads.
2. Implement typed Rust Port and compile in CI.
3. Exercise Job cancellation, outbox, CAS, index generation against two real database adapter profiles.
4. Test MCP schema parity and full request-to-response behavior.
