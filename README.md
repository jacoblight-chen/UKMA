# UKMA v1.2.3 — Phase 6R VS Code / GitHub Copilot Rust Workspace

This is the **Rust-first development workspace** built from the Phase 6 package, NOT a Python runtime and NOT a completed CodeGraph migration.

## Open with VS Code

1. Install Rust via [rustup](https://rustup.rs) (stable toolchain); verify `rustc --version` and `cargo --version` in an integrated terminal.
2. Install VS Code plus GitHub Copilot, GitHub Copilot Chat, rust-analyzer and CodeLLDB (recommended in `.vscode/extensions.json`). Sign into your Copilot account.
3. Open `UKMA-Phase6R.code-workspace` in VS Code, or use **File → Open Folder** on the workspace root.
4. Run **Terminal → Run Task → Cargo: Check**, then **Cargo: Test** and **Cargo: Run Example**.
5. Use Copilot Chat with `.github/copilot-instructions.md` and the `implement-next-slice` prompt for scoped changes.

## Windows notes

Install `rustup-init.exe` using the Rust site, choose the MSVC toolchain and install the Visual Studio C++ Build Tools if necessary. Restart VS Code after installation. If PowerShell script execution is restricted by administrator policy, run `cargo.exe` directly from CMD or the VS Code task (does not require enabling PowerShell scripts). No Python installation is required to build or run the Rust crates.

## Linux/macOS notes

Install Rust stable with rustup; ensure a C/C++ compiler and linker are installed. For Linux run `cargo test --workspace --all-targets` from the root.

## Source layout

- `crates/ukma-contracts` — shared Rust request/response wire type prototypes.
- `crates/ukma-code-runtime` — Rust storage ports, dev-only file adapter, outbox and integration tests.
- `contracts/` — 65 operation registry, preliminary schemas, MCP/event artifacts.
- `docs/UKMA_v1.2.3_Full_Spec.md` — complete standalone spec.
- `docs/PHASE6R_IMPLEMENTATION_PLAN.md` — migration and acceptance order.
- `.vscode/` — tasks, debugger and extension suggestions.
- `.github/copilot-instructions.md` — project-level instructions for Copilot.
- `scripts/` — `gen_contracts.py` (registry codegen; run by the CI `contract-closure` codegen-reproducibility step) and `verify_rust.ps1`/`verify_rust.sh` (cargo gate wrappers).

## Acceptance commands

```shell
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo run -p ukma-code-runtime --example quickstart
```

**Honest status:** Rust/Cargo was unavailable in the packaging environment, so these commands must be executed in VS Code or CI. The current JsonFileAdapter is not a production database. API closure remains 0/65 until full schema/runtime acceptance is satisfied.


## VS Code Workspace 專案顯示修正

請使用 **File → Open Workspace from File** 開啟 `UKMA-Phase6R.code-workspace`，左側 Explorer 將顯示 `UKMA (Workspace Root)`、`ukma-contracts`、`ukma-code-runtime` 三個資料夾。兩個 Crate 的原始位置分別為 `crates/ukma-contracts` 和 `crates/ukma-code-runtime`；它們仍是根目錄 `Cargo.toml` 的 Workspace members。開啟時若 VS Code 要求信任工作區，請確認內容後再信任。
