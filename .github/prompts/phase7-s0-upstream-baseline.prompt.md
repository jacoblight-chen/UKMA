# UKMA Phase 7 — S0 Upstream Baseline

Read `docs/UKMA_v1.2.3_Full_Spec.md`, `docs/PHASE7_CODEGRAPH_MIGRATION_PLAN.md` and `.github/copilot-instructions.md` before taking action.

First run actual Phase 6R Rust Cargo checks. If they do not pass, repair and document the failure before any migration merge.

Then preserve a read-only CodeGraph mirror and reference checkout *outside the UKMA git repository*, with user authorization for network/git operations. Record exact upstream SHA, license, dependencies, Rust toolchain, and reproducible baseline test results. Generate a source map from observed paths (never invent paths). Prepare one Graph golden fixture, migration ledger, proposed S1 contract and risks. Do not migrate all modules or invent upstream API signatures.

Every reported success must include the executed command and actual result. If blocked, say so explicitly.
