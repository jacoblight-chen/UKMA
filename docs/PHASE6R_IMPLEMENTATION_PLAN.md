# Phase 6R — Rust Native VS Code delivery

## What exists

- Existing Rust crates: `ukma-contracts`, `ukma-code-runtime`.
- Rust `GraphStore` and `EventOutbox` traits and development-only `JsonFileAdapter`.
- API registry of 65 operations and schema artifacts retained from Phase 1–6.
- Rust integration tests and a runnable example for the current adapter.
- VS Code tasks, debugger config, Copilot repository instructions.

## Important limitations

- **No CodeGraph source migrated yet.** No production database adapter implemented.
- JSON adapter is for development only, lacks cross-process coordination and power-loss durability guarantees.
- 65 APIs remain **not fully contract-closed**.
- The code was packaged without `cargo`/`rustc` installed in this environment; compile/test in VS Code or CI.

## Engineering work order

1. Run Cargo check/test/clippy and fix any compiler/formatter issues.
2. Replace `JsonFileAdapter` with a Rust SQLite adapter behind ports, with atomic symbol/outbox operations.
3. Introduce a second Rust adapter and run the same contract test suite against both.
4. Pin original CodeGraph upstream SHA and capture golden results. Migrate parser/symbol vertical slice and compare output.
5. Close remaining Code API Request/Response and Event payload schemas.
6. Only then enable gated runtime releases.

## Contract checklist

- authorization: tenant/project boundary, wrong-scope results
- idempotency: same key same payload vs conflicting payload
- optimistic concurrency: ETag enforcement
- outbox: replay, delivery acknowledgement, committed-state semantics
- durability: restart, partial writes and fault injection
- interface: no direct DB dependency above adapter
