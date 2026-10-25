# UKMA Phase 7 S0 — Upstream Baseline Report

**Status: COMPLETE — all S0 deliverables produced; official full-workspace upstream test BLOCKED
(rocksdb/MinGW, root-caused); feasible graph-engine subset PASSED (40/40).**
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
- Workspace: version 0.21.0, resolver 2, edition 2021, **47 crates**
  (1 graph engine, 1 parser API, 37 language parsers, 1 memory, 1 server, 1 harness)
- Graph engine crate `codegraph`: 42 rs files; storage impls Memory / Namespaced / RocksDB (feature-gated)

## 5. Upstream baseline test — final results (executed 2026-10-25)

**Environment repairs (root-caused, executed):**
- Run 1 BLOCKED: `error calling dlltool 'dlltool.exe': program not found`
  (GNU toolchain needs MinGW binutils; host had none).
  → Repair: installed WinLibs POSIX/UCRT via winget (`BrechtSanders.WinLibs.POSIX.UCRT`).
- Run 2 BLOCKED: `zstd-sys v2.0.16+zstd.1.5.7` build script — `Unable to find libclang`
  (zstd-sys compiles C via clang-sys; host had no LLVM).
  → Repair: installed LLVM 23.1.4 via winget (`LLVM.LLVM`).
- Run 3 (official `cargo test --workspace`): **BLOCKED** — `librocksdb-sys` (rocksdb 8.10)
  C++ fails under MinGW g++ (`uint64_t` undeclared in `blob_file_meta.h`;
  `ROCKSDB_PRIszt` format mismatch in `transaction_db_mutex_impl.cc`).
  Root cause: upstream pins MSVC (rust-toolchain 1.94.1); this host has no MSVC linker.
  Recorded as BLOCKED per plan — not marked as success.
- `cargo test -p codegraph-parser-api`: **BLOCKED** — transitively pulls `codegraph`
  (default features ⇒ rocksdb-backend) → same rocksdb/MinGW blocker.
  (The 37 language parser crates share this transitive dependency.)

**Feasible subset — PASSED (actual result):**
- `cargo +stable-x86_64-pc-windows-gnu test -p codegraph --no-default-features --lib`
  → **40 passed / 0 failed** (graph engine core, non-RocksDB; unit tests incl. storage trait)

**Upstream packaging defect found (observed, recorded — not modified upstream):**
`crates/codegraph/tests/**` (`export_test.rs`, `crash_recovery_test.rs`, `rocksdb_backend_test.rs`)
and all 6 examples call `CodeGraph::open` unconditionally with no
`#[cfg(feature = "rocksdb-backend")]` gate ⇒ the crate's test suite cannot build
without RocksDB. Relevant to S1: UKMA's test suite must gate persistence-backend tests.

Log: `f:\codegraph-workspace\upstream-baseline-test.log`

## 6. Deliverables

- `docs/migration/upstream-lock.yaml` — SHA, license, deps, toolchain, reproducibility
- `docs/migration/source-map.yaml` — observed paths only (no invented paths)
- `docs/migration/ledger/s0-upstream-baseline.yaml` — ledger entry
- `docs/migration/s1-contract-proposal.md` — S1 contract proposal + risks (pending architecture approval)
- `tests/golden/graph-basic-ops.golden.json` — one Graph golden fixture (from observed upstream test semantics)

## 7. Result

- Phase 6R gate: PASSED (13/13 tests, all four checks exit 0).
- Mirror + reference checkout: preserved outside the UKMA repo at locked SHA
  `590c2ca7dd7b37bd1d41caf45fac535b6522f969` (verified in 3 places; clean tree).
- License audit: Apache-2.0 + NOTICE identified and recorded.
- Upstream baseline: **feasible subset PASSED (40/40, graph engine non-RocksDB)**;
  **official full `cargo test --workspace` BLOCKED** (rocksdb 8.10 C++ incompatible with
  MinGW on this host; upstream pin is MSVC). Blocker root-caused and documented;
  to be re-run on an MSVC host/CI before S1 merge.
- Source map: generated from observed paths only (47 crates; graph engine file-level).
- Golden fixture: `tests/golden/graph-basic-ops.golden.json` (from observed upstream test semantics).
- S1 contract: PROPOSAL only — pending architecture approval; no S1 code written.
- No upstream source has been migrated into UKMA.
