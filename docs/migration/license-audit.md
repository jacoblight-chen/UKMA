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
