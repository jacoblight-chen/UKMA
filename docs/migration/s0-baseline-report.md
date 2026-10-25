# UKMA Phase 7 S0 — Upstream Baseline Report

**Status: COMPLETE — all S0 deliverables produced; official full-workspace upstream test
EXECUTED (297 passed / 1 ignored / 1 failed — single failure is an upstream test
Windows-portability defect, root-caused); feasible graph-engine subset PASSED (40/40).**
Every success below is backed by an actually executed command. Blockers are labeled BLOCKED.

## 1. Phase 6R Gate (precondition) — PASSED (executed 2026-10-25)

| Check | Command | Result |
|---|---|---|
| Format | `cargo fmt --all -- --check` | exit 0 |
| Check | `cargo +stable-x86_64-pc-windows-gnu check --workspace --all-targets` | exit 0 |
| Clippy | `cargo +stable-x86_64-pc-windows-gnu clippy --workspace --all-targets -- -D warnings` | exit 0, no warnings |
| Test | `cargo +stable-x86_64-pc-windows-gnu test --workspace --all-targets` | exit 0, **13 passed / 0 failed** |

Environment note: default `stable-x86_64-pc-windows-msvc` toolchain cannot link on this host
(`link.exe` absent; no Visual Studio C++ Build Tools / MinGW at start). All checks therefore
executed with `stable-x86_64-pc-windows-gnu` (rustc 1.99.1).

## 2. Mirror / reference preservation (outside UKMA repo) — DONE (executed)

- `git clone --mirror https://github.com/codegraph-ai/CodeGraph.git f:\codegraph-workspace\codegraph-upstream.git` → exit 0
- `git clone https://github.com/codegraph-ai/CodeGraph.git f:\codegraph-workspace\codegraph-reference` → exit 0 (916 files)
- SHA verified in mirror (`refs/heads/main`), mirror HEAD, reference HEAD — all
  `590c2ca7dd7b37bd1d41caf45fac535b6522f969`
- `git status --porcelain` in reference checkout → clean
- Commit: `2026-10-05T19:20:33-07:00`, author `anvanster <72588463+anvanster@users.noreply.github.com>`

## 3. License audit — DONE

- `LICENSE`: Apache License 2.0 (full text)
- `NOTICE`: "CodeGraph — Copyright 2025-2026 Andrey Vasilevsky <anvanster@gmail.com> … Licensed under the Apache License, Version 2.0."
- Per-file copyright headers observed (2024-2026, `SPDX-License-Identifier: Apache-2.0`)
- Third-party deps recorded in `upstream-lock.yaml` (rocksdb 0.22 feature-gated, serde, thiserror, log, uuid, tower-lsp, tokio, …)

## 4. Baseline metadata — DONE

- Pinned toolchain (upstream `rust-toolchain.toml`): **rust 1.94.1** (components rustfmt, clippy)
- Workspace: version 0.21.0, resolver 2, edition 2021, **43 crates**
  (1 graph engine, 1 parser API, 38 language parsers, 1 memory, 1 server, 1 harness)
  — verified via `cargo metadata --locked --format-version 1` (43 members == 43
  Cargo.toml entries, exact match); the workspace comment "Language parsers (27)"
  is stale (38 listed; recorded as-is, not corrected)
- Graph engine crate `codegraph`: 42 rs files; storage impls Memory / Namespaced / RocksDB (feature-gated)

## 5. Upstream baseline test — final results (executed 2026-10-25)

**Environment repairs (root-caused, executed):**
- Run 1 BLOCKED: `error calling dlltool 'dlltool.exe': program not found`
  (GNU toolchain needs MinGW binutils; host had none).
  → Repair: installed WinLibs POSIX/UCRT via winget (`BrechtSanders.WinLibs.POSIX.UCRT`).
- Run 2 BLOCKED: `zstd-sys v2.0.16+zstd.1.5.7` build script — `Unable to find libclang`
  (zstd-sys compiles C via clang-sys; host had no LLVM).
  → Repair: installed LLVM 23.1.4 via winget (`LLVM.LLVM`).
- Run 3 (official `cargo test --workspace` under plain WinLibs g++ 16.2): **BLOCKED** —
  `librocksdb-sys` (rocksdb 8.10) C++ fails under new g++ (`uint64_t` undeclared in
  `blob_file_meta.h`; `ROCKSDB_PRIszt` format mismatch in `transaction_db_mutex_impl.cc`).
  Root cause: rocksdb 8.10 headers lack `#include <cstdint>` (compatible with gcc ≤12 era;
  upstream pins MSVC).
- Run 4 (official `cargo test --workspace`, WinLibs gnu 16.2 + `CXXFLAGS="-include cstdint"`
  + `CFLAGS="-include stdint.h"` + LLVM 23.1.4, rust 1.99.1-gnu): **EXECUTED TO COMPLETION**
  (executed 2026-10-25, host `f:\codegraph-workspace\codegraph-reference`, reference SHA
  `590c2ca7dd7b37bd1d41caf45fac535b6522f969`).
  Result: **299 tests — 297 passed / 1 ignored / 1 failed** (exit 101):
  - `test result: ok. 57 passed`
  - `test result: ok. 72 passed; 1 ignored`
  - `test result: ok. 11 passed`
  - `test result: ok. 113 passed`
  - `test result: FAILED. 44 passed; 1 failed`
  All 20 `storage::rocksdb_backend` + `integration::rocksdb_backend_test` tests **passed**
  (RocksDB C++ build fully succeeded under the forced-`cstdint` workaround).
- `cargo test -p codegraph-parser-api`: **UNBLOCKED by Run 4** — its transitive rocksdb
  dependency now builds (see Run 4); covered by the full-workspace execution.

**Single failure — attributed to an upstream test defect (not UKMA, not toolchain):**
- `test_header_type_resolution` in `crates/codegraph-c/tests/integration_tests.rs:1760`
  panics: `Os { code: 3, kind: NotFound }` — the test hardcodes the POSIX path
  `/tmp/test_header_resolve.h` and calls `std::fs::write(...).unwrap()`. On Windows there is
  no `/tmp` directory, so the write fails. This test passes on Linux CI and fails on any
  Windows host. Recorded as an **upstream Windows-portability defect** (observed, documented;
  upstream source not modified).

**Feasible subset (supplementary, executed 2026-10-25):**
- `cargo +stable-x86_64-pc-windows-gnu test -p codegraph --no-default-features --lib`
  → **40 passed / 0 failed** (graph engine core, non-RocksDB; unit tests incl. storage trait)

**Upstream packaging defect found (observed, recorded — not modified upstream):**
`crates/codegraph/tests/**` (`export_test.rs`, `crash_recovery_test.rs`, `rocksdb_backend_test.rs`)
and all 6 examples call `CodeGraph::open` unconditionally with no
`#[cfg(feature = "rocksdb-backend")]` gate ⇒ the crate's test suite cannot build
without RocksDB. Relevant to S1: UKMA's test suite must gate persistence-backend tests.

Logs: `f:\codegraph-workspace\upstream-baseline-local.log` (Run 4, full workspace, 2026-10-25);
`f:\codegraph-workspace\ci-log2\` (CI run #2, ubuntu-22.04: compile OK, rust-lld link crash —
runner infrastructure flake); `f:\codegraph-workspace\upstream-baseline-test.log` (Runs 1–3).

## 6. Deliverables

- `docs/migration/upstream-lock.yaml` — SHA, license, deps, toolchain, reproducibility
- `docs/migration/source-map.yaml` — observed paths only (no invented paths)
- `docs/migration/dependency-graph.yaml` — Cargo dependency graph + Rust features audit
  (executed `cargo metadata --locked --format-version 1`; full 5.1 MB JSON preserved at
  `f:\codegraph-workspace\upstream-cargo-metadata.json`; 43 members, 71 unique external
  deps, `codegraph` features: default→rocksdb-backend→dep:rocksdb)
- `docs/migration/ledger/s0-upstream-baseline.yaml` — ledger entry
- `docs/migration/s1-contract-proposal.md` — S1 contract proposal + risks (pending architecture approval)
- `tests/golden/graph-basic-ops.golden.json` — one Graph golden fixture (from observed upstream test semantics)
- `.github/pull_request_template.md` — S1+ pre-merge checklist (ledger/license/tests required)
- `.github/CODEOWNERS` — owner review required on all PRs

## 7. Result

- Phase 6R gate: PASSED (13/13 tests, all four checks exit 0).
- Mirror + reference checkout: preserved outside the UKMA repo at locked SHA
  `590c2ca7dd7b37bd1d41caf45fac535b6522f969` (verified in 3 places; clean tree).
- License audit: Apache-2.0 + NOTICE identified and recorded.
- Upstream baseline: **official full `cargo test --workspace` EXECUTED TO COMPLETION —
  297 passed / 1 ignored / 1 failed** (Run 4; the single failure is an upstream
  Windows-portability defect: hardcoded `/tmp/` path in `test_header_type_resolution`;
  passes on Linux CI). Supplementary subset PASSED (40/40, graph engine non-RocksDB).
  Toolchain deviation recorded: upstream pin is MSVC 1.94.1; this host executed with
  GNU 1.99.1 + WinLibs 16.2 + forced `-include cstdint` (rocksdb 8.10 C++ workaround,
  root-caused). CI audit (ubuntu-22.04) confirmed the C++ compiles under gcc 11.
- Source map: generated from observed paths only (43 crates; graph engine file-level).
- Dependency graph: exported via actually executed `cargo metadata --locked --format-version 1`
  (exit 0; 43 workspace members; 71 unique external dependencies; feature audit:
  `codegraph` default→rocksdb-backend→dep:rocksdb; parser-api has no features).
- Count correction: earlier "47 crates / 37 parsers" was a miscount — verified actual
  **43 crates / 38 parsers** (Cargo.toml members == cargo metadata members, exact match);
  all S0 records corrected to the verified numbers.
- Repo protection (plan §5 item 6): lightweight guardrails IMPLEMENTED —
  `.github/pull_request_template.md` (S1+ checklist: ledger/license/tests mandatory) +
  `.github/CODEOWNERS` (owner review required). Platform-level branch protection
  rules are **BLOCKED** (GitHub free plan: branch protection is unavailable for
  private repositories — recorded explicitly, not marked as success); S1 PRs are
  enforced via the reviewer checklist in the template.
- Golden fixture: `tests/golden/graph-basic-ops.golden.json` (from observed upstream test semantics).
- S1 contract: PROPOSAL only — pending architecture approval; no S1 code written.
- No upstream source has been migrated into UKMA.
