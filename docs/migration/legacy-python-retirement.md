# Legacy Python harness retirement (P0-WP7)

**Decision:** ADR-048 / `.github/copilot-instructions.md` — the UKMA runtime MUST be
Rust. The Python files under `scripts/` and `tests/` are **pre-implementation
scaffolding from phases 2–6**, not part of the runtime and not gate evidence.

## Status

| Path | Role | Status | Successor |
|---|---|---|---|
| `scripts/verify_contracts.py` | early schema/fixture checks | **RETIRED** | `cargo run -p ukma-contract-check` |
| `scripts/verify_phase2.py` … `verify_phase5.py` | phase acceptance scripts | **RETIRED** | `cargo test --workspace --all-targets` |
| `scripts/code_runtime_model.py`, `scripts/reference_adapter.py` | prototype reference model | **RETIRED** | `crates/ukma-contracts` + `crates/adapters/*` |
| `tests/test_registry.py`, `tests/test_phase5_runtime.py`, `tests/test_phase6_adapter.py` | prototype tests | **RETIRED** | Rust tests in `crates/*/tests/` |
| `scripts/gen_contracts.py` | codegen: registry → Rust/TS/MCP | **ACTIVE (dev tooling only)** | n/a — it generates code, it is not runtime |
| `scripts/verify_rust.ps1`, `verify_rust.sh` | Cargo gate wrappers | **ACTIVE** | n/a |

## Rules

1. **No release gate may cite a Python result.** Gate evidence is produced only by
   the `cargo` commands listed in `docs/UKMA_v1.2.3_IMPLEMENTATION_PLAN.md` §7.2.
2. **CI does not execute Python tests.** `.github/workflows/contract-closure.yml` runs
   only Cargo (plus the codegen reproducibility step). The Python files remain in the
   tree for historical traceability until the slices they served are fully superseded
   by Rust equivalents, then they are deleted.
3. **`scripts/gen_contracts.py` is exempt** because it is a *generator*: it reads
   `contracts/api/operation-registry.yaml` and emits Rust/TypeScript/MCP artifacts.
   It never runs in production, and its output is verified by the Rust checker, so a
   generator bug cannot silently pass (drift check + schema validation fail).
4. Deleting the retired files is a cleanup item; it is safe only after the
   corresponding Rust replacements are green (they are, as of P0 completion).

## Verification that the gate is Python-free

```powershell
# The only accepted gate commands:
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo run -p ukma-contract-check
```

Any other command (including `pytest`) is **not** acceptance evidence.
