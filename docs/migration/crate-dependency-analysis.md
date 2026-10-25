# UKMA Phase 7 S0 — CodeGraph Crate & Module Dependency Analysis

**Source basis:** `cargo metadata --locked --format-version 1` (actually executed, exit 0) at
locked SHA `590c2ca7dd7b37bd1d41caf45fac535b6522f969`; source tree observed in
`f:\codegraph-workspace\codegraph-reference`. Nothing invented.
Full machine-readable data: `f:\codegraph-workspace\upstream-cargo-metadata.json` (5.1 MB).
Companion: `dependency-graph.yaml` (features + external deps summary).

## 1. Module → crate mapping (plan §5's six modules)

| Module | Upstream crate(s) | Source location (observed) | Notes |
|---|---|---|---|
| Graph | `codegraph` | `src/graph/` (types.rs, property.rs, codegraph.rs, algorithms.rs, mod.rs) | Node/Edge types, 34-method `CodeGraph` API, bfs/dfs/SCC/paths |
| Storage | `codegraph` | `src/storage/` (mod.rs, memory.rs, namespaced.rs, rocksdb_backend.rs) | `StorageBackend` trait (KV-level); RocksDB impl feature-gated `rocksdb-backend` (default ON) |
| Retrieval | `codegraph` | `src/query.rs`, `src/metadata.rs`, `src/export/` | query/metadata/export sit in the core crate, not a separate crate |
| Parser | `codegraph-parser-api` + 38 `codegraph-<lang>` | `crates/codegraph-parser-api`, `crates/codegraph-<lang>` | each parser = parser-api + its `tree-sitter-<lang>` grammar (optional dep per parser) |
| Memory | `codegraph-memory` | `src/embedding/` | **standalone crate — zero workspace dependencies**; fastembed/hf-hub/safetensors/tokenizers; **hard (non-optional) rocksdb dep** |
| Server | `codegraph-server` | `src/ai_query/`, `src/domain/`, `src/git_mining/`, `src/handlers/`, `src/mcp/` | hub crate: depends on core + all 38 parsers + parser-api + memory |

Supporting: `codegraph-harness` (test infrastructure; workspace deps: none — external only:
anyhow, clap, serde, serde_json, serde_yaml, tempfile, walkdir, which).

## 2. Intra-workspace dependency edges (complete, from cargo metadata)

```
codegraph                      <- no workspace deps (core; rocksdb OPTIONAL)
codegraph-parser-api           -> codegraph
codegraph-<lang> (×38)        -> codegraph, codegraph-parser-api   (+ tree-sitter-<lang> grammar, external)
codegraph-memory              -> (none)                            [hard rocksdb, external]
codegraph-harness             -> (none)
codegraph-server             -> codegraph, codegraph-parser-api, codegraph-memory, codegraph-<lang> (all 38)
```

Dependent counts: `codegraph` = 40 crates depend on it; `codegraph-parser-api` = 39.
All 38 parser crates have the identical edge shape (`codegraph` + `codegraph-parser-api`).

## 3. Key dependency findings (S1-relevant)

1. **rocksdb coupling is uneven** — `codegraph` gates rocksdb behind `dep:rocksdb`
   (feature `rocksdb-backend`, default ON upstream), but `codegraph-memory` declares
   rocksdb as a **hard, non-optional** dependency. UKMA S1 must not inherit the
   memory crate's hard coupling; the UKMA Storage Port stays database-agnostic.
2. **Retrieval lives inside the core crate** (query.rs/metadata.rs/export/) — there is
   no separate retrieval crate; S3 will re-map from within `codegraph`, not from a
   dedicated module boundary.
3. **`codegraph-memory` is isolated** (no workspace deps) — the S5 memory migration is
   a self-contained crate transplant with its own embedding stack; its hard rocksdb
   dep must be replaced by the UKMA Storage Port.
4. **`codegraph-server` is a fan-in hub** (42 workspace deps) — S6 gateway work touches
   the widest surface; keep S1 independent of it (S1 depends only on graph/storage
   surfaces, never on server or parser crates).
5. **Two tree-sitter versions locked** (0.20.10 + 0.25.10) — the parser layer mixes
   tree-sitter API generations; relevant to S2 (parser porting), not S1.
6. **The upstream packaging defect** (test suite + examples call `CodeGraph::open`
   unconditionally, no `#[cfg(feature = "rocksdb-backend")]` gate) means the upstream
   test suite cannot build without RocksDB — UKMA S1 tests must gate persistence
   backends explicitly (Memory adapter + feature-gated others).

## 4. UKMA slice mapping consequence

| Upstream crate | UKMA slice | S1 dependency? |
|---|---|---|
| `codegraph` (graph + storage + retrieval) | S1 (graph/storage), S3 (retrieval) | **Yes** — sole S1 source |
| `codegraph-parser-api` + 38 parsers | S2 | No |
| `codegraph-memory` | S5 | No |
| `codegraph-server` | S6 | No |
| `codegraph-harness` | test infrastructure | No (reference only) |

**S1 boundary rule:** `graph-engine` maps exclusively from `crates/codegraph`
(graph + storage surfaces). No S1 code path reaches parser, memory, or server crates.
