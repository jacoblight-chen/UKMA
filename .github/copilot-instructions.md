# UKMA Copilot instructions — Phase 6R

- **Rust is the implementation language**. Do not implement production runtime or adapters in Python.
- This is an extension of Phase 6, not a completed CodeGraph migration. The official source specification is `docs/UKMA_v1.2.3_Full_Spec.md`.
- Preserve the 65-operation API registry and existing JSON Schema artifacts. Current schemas are preliminary; **never claim 65/65 closure**.
- Keep Domain models independent of concrete databases. Use ports/traits. JSON file adapter is development-only: no cross-process safety/durability guarantee.
- Every new behavior requires Rust tests for positive, negative, authorization, version and failure paths.
- Do not add a dependency on a specific database to `ukma-contracts`.
- Domain knowledge must not be subject to memory TTL or folding.
- Maintain tenant/project isolation, optimistic concurrency, idempotency and outbox semantics.
- CodeGraph migration is pending: before copying its source, pin upstream commit and record source path, license and tests in the migration ledger.
- Ask for explicit architecture approval before changing wire-level public API semantics.
- Use `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets` as acceptance checks.
- A test cannot be claimed as passed unless actually executed in Rust.
