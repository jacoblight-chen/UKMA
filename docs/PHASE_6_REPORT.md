# Phase 6 — Code Intelligence Executable Contract Integration (prototype)

## Delivered
- Rust crate `ukma-code-runtime`: typed `GraphStore` and `EventOutbox` ports, `SymbolRecord`, scope and ETag checks, file-backed JSON adapter with a canonical state/outbox file, and Rust unit test source.
- Python **independent reference** persistence adapter and 8 executable tests for restart, ACL, ETag, idempotency, replay and event acknowledgments.
- No direct CodeGraph source has been migrated; this is a contract integration prototype only.

## Important limitations / release blockers
1. **Cargo/rustc unavailable in execution environment**: Rust compilation and Rust tests are NOT verified. Python tests do not verify Rust behavior.
2. Rust adapter uses a single JSON snapshot and atomic rename, but does not ensure power-loss durability, cross-process locking, or cross-volume guarantees. Not production-ready.
3. Rust prototype now retains normalized payload fingerprints and original results for idempotency, but it remains uncompiled and its implementation is not independently verified against the Python reference.
4. No backend pair has undergone real Storage Adapter Contract Tests; database-agnostic portability is NOT VERIFIED.
5. The CodeGraph AST parsing, symbols, callgraph and indexer are not integrated; no source migration yet.
6. Full CAPI-01..13, permissions, read model snapshots and event payload closure remains unverified; all 65 API operations remain release-blocked.

## Commands
```bash
python -m unittest discover -s tests -p 'test_phase6_adapter.py' -v
cargo test --workspace  # requires a Rust toolchain; NOT RUN in this environment
python scripts/verify_contracts.py --strict  # expected release-blocking nonzero exit
```

## Next gate
- Validate Rust idempotency in Cargo tests; add crash-safety and cross-process locking, full typed CAPI ports.
- Run cargo fmt/check/test, then integrate CodeGraph symbol parser behind ports.
- Implement second independent real backend and run identical tests across both.
