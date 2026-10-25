# UKMA Phase 7 S0 — License Audit (Upstream + Third-Party)

**Source basis:** observed at locked SHA `590c2ca7dd7b37bd1d41caf45fac535b6522f969`
(`f:\codegraph-workspace\codegraph-reference`) + `cargo metadata --locked` (executed, exit 0;
per-package `license` fields, 499 external/transitive packages).

## 1. Upstream (CodeGraph)

| Item | Finding |
|---|---|
| `LICENSE` | Apache License 2.0 (full text present) |
| `NOTICE` | "CodeGraph — Copyright 2025-2026 Andrey Vasilevsky <anvanster@gmail.com> … This product includes software developed by Andrey Vasilevsky (https://github.com/codegraph-ai/codegraph). Licensed under the Apache License, Version 2.0." |
| Per-file headers | Observed on source files: `Copyright 2024-2026 Andrey Vasilevsky <anvanster@gmail.com>` + `SPDX-License-Identifier: Apache-2.0` |
| Commit provenance | `590c2ca7dd7b37bd1d41caf45fac535b6522f969` (2026-10-05T19:20:33-07:00, author anvanster) |

**Attribution requirement for any S1+ merge:** preserve the Apache-2.0 attribution
(copyright notice + SPDX headers + NOTICE-equivalent credit) in `graph-engine`
and the UKMA NOTICE/README. The upstream `NOTICE` text is recorded verbatim in
`upstream-lock.yaml` → `license_audit.notice`.

## 2. Third-party (499 external/transitive packages from cargo metadata)

**Zero packages without a declared license.** Family summary (declared SPDX strings):

| License family | Count |
|---|---|
| MIT OR Apache-2.0 (dual) | 310 |
| MIT (or MIT + other permissive) | 131 |
| Apache-2.0 | 20 |
| Unicode-3.0 (data license, e.g. unicode tables) | 18 |
| BSD (2/3-clause) | 10 |
| ISC | 5 |
| CDLA-Permissive-2.0 | 2 |
| 0BSD / Zlib | 1 |
| CC0-1.0 | 1 |
| MPL-2.0 (file-level copyleft) | 1 |

**Compatibility assessment:**
- All licenses are permissive or OSI-approved; **no GPL/AGPL/proprietary licenses found**
  among declared packages.
- The single **MPL-2.0** package is file-level copyleft: using it as a dependency is
  compatible; UKMA must not modify and redistribute MPL-2.0 file *sources* without
  their license terms (standard dependency usage is fine).
- **CDLA-Permissive-2.0** (2 packages): permissive source license (Apache-2.0-compatible
  for use; product-level patents granted) — no action needed for dependency use.
- Dual-licensed (MIT OR Apache-2.0) packages are compatible with UKMA (Apache-2.0).
- `rocksdb` (0.22.0 / librocksdb-sys 8.10.0, C++ RocksDB 8.10.0) is Apache-2.0 —
  and it is **feature-gated in `codegraph`** (never required by the Domain), consistent
  with the Database-Agnostic rule.

## 3. Conclusion

- **Upstream CodeGraph is Apache-2.0** — compatible with UKMA; NOTICE + per-file
  SPDX headers must be preserved on any migrated file (S1+ PR checklist enforces this).
- **All 499 third-party packages are permissive/OSI-approved; 0 unlicensed; no
  copyleft-via-linking risk** (the one MPL-2.0 package is file-level).
- No license blockers for S1. S1+ PRs are checked by `.github/pull_request_template.md`
  (license section) + reviewer (CODEOWNERS).

---

## 4. P10 final audit (UKMA workspace, executed 2026-10-25)

This section closes REP-003. It audits **the UKMA workspace itself** (the section
above audits the upstream reference project), and it is enforced by a test rather
than by this prose.

### 4.1 Artefacts

| Artefact | Purpose |
|---|---|
| `LICENSE` | Apache License 2.0, full canonical text (root) |
| `NOTICE` | UKMA copyright + CodeGraph/Andrey Vasilevsky attribution + pinned commit + pointer to the inventory |
| `contracts/testing/third-party-inventory.json` | machine-readable inventory of every package |
| `crates/ukma-integration/tests/release_evidence.rs` | the gate (6 tests) |

### 4.2 Method

```
cargo metadata --locked --format-version 1     # exit 0
```

All workspace members and every external/transitive package are recorded with the
declared licence string. The **decision is re-derived by the gate**, not trusted
from the file: a licence string is parsed as a disjunction (`OR`, `AND`, and the
legacy `MIT/Apache-2.0` slash form) and allowed when at least one option is in the
allowed list:

`MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`,
`BSD-3-Clause`, `ISC`, `0BSD`, `Zlib`, `Unicode-3.0`, `CC0-1.0`, `MIT-0`,
`BSL-1.0`, `CDLA-Permissive-2.0`, `Unlicense`, `MPL-2.0` (file-level copyleft,
dependency use only).

Blocked families: `GPL-2.0`, `GPL-3.0`, `AGPL`, `SSPL`, `BUSL`, `Elastic-2.0`,
`Commons-Clause` — allowed **only** when a permissive alternative is elected, and
the elected option is recorded in the entry's `reason`.

### 4.3 Result

| Measurement | Value |
|---|---|
| External / transitive packages | **307** |
| Workspace packages | **29** |
| Packages without a declared licence | **0** |
| Blocked licences | **0** |
| Backend SDK packages (rocksdb, sqlx, qdrant-client, neo4j, …) | **0** |
| CycloneDX components | **336** |

The counts were refreshed with the schema-closure work (ADR-058): `schemars`,
`schemars_derive`, `serde_derive_internals` and `dyn-clone` are MIT-licensed build
dependencies used only behind the opt-in `schema` feature that generates the wire
payload schemas.

Two packages offer `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; the permissive option
is elected, so the LGPL option is never taken. No package is copyleft-only.

### 4.4 Gate behaviour

- Adding a dependency without refreshing the inventory fails
  `the_inventory_covers_the_whole_dependency_tree` (it compares against `Cargo.lock`
  in both directions: missing **and** stale entries).
- Adding a copyleft-only dependency fails `every_recorded_decision_follows_the_policy`
  and `no_copyleft_or_backend_sdk_reaches_the_build` permanently.
- `NOTICE` is checked against `docs/migration/upstream-lock.yaml`, so the upstream
  attribution cannot drift from the pinned commit.

### 4.5 Remaining limitation

The inventory records **declared** licence metadata from the resolved crates; it does
not read each package's `LICENSE` files. A package that declares a licence but ships
different terms would not be caught here. This is recorded as a limitation, not a
pass.

