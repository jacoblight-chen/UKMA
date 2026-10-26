# UKMA Phase 8 — CodeGraph 功能缺口補足實作計畫

**狀態：執行中** · **對應基準：UKMA v1.2.3** · **語言：Rust**

本計畫補足 Phase 7 覆蓋度審查所識別的 CodeGraph 功能缺口。所有變更皆為
**additive-only**（新增操作／模組；不修改任何既有 v1.2.3 語意），並依
`agent-rules` 規則：每個新行為都附 positive / negative / authorization /
version / failure 五類 Rust 測試；`ukma-contracts` 不新增任何資料庫依賴；
Tenant/Project 隔離、optimistic concurrency、idempotency 與 outbox 語意保留。

> 授權說明：本計畫的新增 wire 操作已依使用者明確指示（「請補足所有的缺口，
> 並依據計畫完整實作」）作為架構批准執行；僅新增，不改變既有操作語意。

## 1. 缺口清單與對應

| # | 缺口（CodeGraph 功能） | UKMA 補足 | 性質 |
|---|---|---|---|
| G1 | `find_implementors` | `code.find_implementors` | 新 wire 操作 |
| G2 | `traverse_graph`（獨立自由遍歷） | `code.traverse` | 新 wire 操作 |
| G3 | `get_detailed_symbol`（單一綜合工具） | `code.get_detailed_symbol` | 新 wire 操作（組合） |
| G4 | `index_directory`（公開 wire 操作） | `code.index_directory` | 新 wire 操作 |
| G5 | 圖匯出格式（csv/dot/json/triples） | `graph-engine::export` + `code.export_graph` | 新 domain 模組 + 新 wire 操作 |
| G6 | `remove_doc_source` | `document.remove_source` | 新 wire 操作 |
| G7 | `memory_context` | `memory.context` | 新 wire 操作 |
| G8 | `memory_list` | `memory.list` | 新 wire 操作 |
| G9 | `memory_stats` | `memory.stats` | 新 wire 操作 |
| G10 | telemetry/metrics | `system.metrics` | 新 wire 操作 |
| G11 | LSP 傳輸 + `custom_requests`（LSP 自訂請求分派器） | `ukma-mcp::lsp`（stdio JSON-RPC，`ukma/<operation>` 自訂請求）+ CLI `--lsp` | 新傳輸層 |

共 **10 個新 wire 操作**（code +5、document +1、memory +3、system +1），
註冊表由 112 → **122** 操作；domain（非 system）108 → 117。

## 2. 新操作契約（15 欄位，§41.1）

| 操作 | state_machine | scopes | idempotency | concurrency | events | 主要 error codes | 測試 id |
|---|---|---|---|---|---|---|---|
| `code.find_implementors` | read_only | code:read | not_required | immutable_snapshot | [] | INVALID_SCHEMA, UNAUTHENTICATED, FORBIDDEN, INTERNAL, NOT_FOUND, SOURCE_UNAVAILABLE, CAPABILITY_UNAVAILABLE | CAPI-28 |
| `code.traverse` | read_only | code:read | not_required | immutable_snapshot | [] | … + RESOURCE_LIMIT | CAPI-29 |
| `code.get_detailed_symbol` | read_only | code:read | not_required | immutable_snapshot | [] | 同 G1 | CAPI-30 |
| `code.index_directory` | create | code:index | required | not_applicable | code.index.partial_committed | 同 `code.index_files` + NOT_FOUND | CAPI-31 |
| `code.export_graph` | read_only | code:read | not_required | immutable_snapshot | [] | … + RESOURCE_LIMIT | CAPI-32 |
| `document.remove_source` | delete_tombstone | document:index_admin | required | not_applicable | [] | INVALID_SCHEMA, UNAUTHENTICATED, FORBIDDEN, INTERNAL, NOT_FOUND, ALREADY_DELETED, DELETION_BLOCKED | DAPI-17 |
| `memory.context` | read_only | memory:read | not_required | immutable_snapshot | [] | … + INVALID_ARGUMENT, NOT_FOUND | MAPI-26 |
| `memory.list` | read_only | memory:read | not_required | immutable_snapshot | [] | … + CURSOR_EXPIRED | MAPI-27 |
| `memory.stats` | read_only | memory:read | not_required | immutable_snapshot | [] | 基本四碼 | MAPI-28 |
| `system.metrics` | read_only | system:read | not_required | immutable_snapshot | [] | 基本四碼 | SYS-05 |

所有操作的 `consistency_modes`、`storage_ports` 依同域既有操作模式
（read-only → MetadataStore；index → ArtifactStore/MetadataStore/GraphStore；
delete → +OutboxPort）。error code 僅使用 `error-registry.yaml` 已註冊值；
events 僅使用 `event-registry.yaml` 已註冊值（`document.remove_source` 無
已註冊事件，故 `events: []`，不新增事件型別）。

## 3. 實作切面（依序執行，每切面獨立可驗證）

### S8-1 圖匯出（domain，無 wire）
- `crates/graph-engine/src/export.rs`：`ExportFormat`（Dot/Json/Csv/Triples）+
  `export_graph(&scope_nodes, &scope_edges, format) -> String`。
- 語義：輸入為**已依 scope 過濾**的節點/邊集合（export 本身不跨 scope 讀取，
  與引擎其余 API 一致）；輸出**決定性**（依 canonical id 排序）；dot 輸出
  轉義 label；triples 使用 `subject predicate object` 行格式；csv 含表頭。
- 上游 `crates/codegraph/src/export/*.rs` 作為語義參考記錄於 ledger
  （本切面為 UKMA-native 重寫，不複製原始碼）。

### S8-2 新 domain 型別與服務實作
- `code-intelligence`：`FindImplementorsQuery/ImplementorMatch`、
  `TraverseQuery/TraverseResult`、`GetDetailedSymbolQuery/DetailedSymbol`、
  `IndexDirectory`（reuse `IndexedRevision`）、`ExportGraphQuery/GraphExport`。
- `document-intelligence`：`RemoveDocSource/DocDeletionReceipt`（tombstone +
  `ALREADY_DELETED` + `DELETION_BLOCKED` 語意；刪除所有 indexed blocks 與
  來源登記，保留 tombstone 使重複刪除可判別）。
- `memory-domain`：`MemoryContextQuery`（path/symbol 至少其一，否則
  `INVALID_ARGUMENT`；依 evidence `source_ref` 前綴或文本包含匹配，trust 排序）、
  `ListMemory`（kinds 過濾 + 分頁）、`GetMemoryStats/MemoryStats/KindCount`。
- 每個服務方法附 domain 層測試（positive / negative / authorization /
  cross-scope / failure）。

### S8-3 wire / 契約接線（10 操作）
1. `contracts/api/operation-registry.yaml`：10 個新條目 + `counts`
   （code 32、document 17、memory 28、traceability 13、system 5、pro 27、total 122）。
2. `scripts/gen_contracts.py`：assert 112 → 122；執行後再生成
   `ukma-contracts/src/generated/operations.rs`、`tool-manifest.json`、
   10 份 `contracts/mcp/tools/ukma_*_v1.input.schema.json`、`sdk/typescript/ukma-operations.ts`。
3. 每操作 request/response schema（`contracts/schema/v1/<op>.*.schema.json`）
   以既有模板生成（`scripts/gen_parity_schemas.py` 同構）。
4. Rust wire 型別 + `tools/wire-schema/src/main.rs` wire_table 10 組；
   測試常數 108 → 117；`cargo run -p ukma-wire-schema` 再生成 wire schema。
5. `contracts/fixtures/v1/<op>/`：每操作 ≥1 valid + ≥3 invalid
   （missing-request-id / bad-api-version / additional-property）。
6. `crates/ukma-mcp/src/manifest.rs`：`DOMAIN_OPERATION_COUNT` 108 → 117。
7. `crates/ukma-contract-check`：`expected_counts` 更新。
8. `contracts/matrices/api-contract-matrix.yaml`：10 個新條目。

### S8-4 傳輸與閘道
- `ukma-mcp/src/handlers.rs`：10 個 `bind!` 註冊（system.metrics 於
  `register_system`）。
- `ukma-mcp/src/gateway.rs`：`MetricsRegistry`（每操作 success/failure 計數 +
  job 統計）掛載於 gateway；`system.metrics` 回傳 `SystemMetrics`。
- `ukma-mcp/src/lsp.rs`：stdio JSON-RPC 2.0 LSP 傳輸：
  - `initialize` → serverInfo + capabilities（自訂請求清單 `ukma/<operation_id>`）；
  - `initialized` / `shutdown` / `exit` 生命週期；
  - 自訂請求 `ukma/<operation_id>`：params = operation input + context，
    分派至 gateway（與 MCP 同一 gateway，同一授權/計數路徑）；
  - 未知方法 / 未知操作 / 參數錯誤回 JSON-RPC 錯誤。
- `ukma-cli`：`--lsp` flag（與 `--mcp` 互斥）啟動 LSP 傳輸。
- 測試：handshake、initialize 能力宣告、自訂請求正路徑、未知方法／未知操作／
  參數錯誤負路徑、shutdown 生命週期。

### S8-5 E2E（公開 API）
- `crates/ukma-integration/tests/e2e_gap_closure.rs`：10 操作各至少一條
  經公開 API（`env.ok` / `env.dispatch`）的 E2E，含 authorization（缺 scope
  → FORBIDDEN）與 cross-scope（→ NOT_FOUND）代表樣例。

### S8-6 文件與 ledger
- `docs/migration/ledger/s8-gap-closure.yaml`：本切面 ledger
  （G5 記錄上游 export 參考路徑；其餘為 UKMA-native，無上游來源）。
- `docs/migration/s2-status.md`：修正過時狀態（38/38 ported，與 ledger /
  parity report 一致）。
- `docs/implementation-status.yaml`：notes 增補 Phase 8 一頁。

### S8-7 驗收 Gate（全數執行）
```
cargo fmt --all -- --check
cargo +stable-x86_64-pc-windows-gnu check --workspace --all-targets
cargo +stable-x86_64-pc-windows-gnu clippy --workspace --all-targets -- -D warnings
cargo +stable-x86_64-pc-windows-gnu test --workspace --all-targets
cargo run -p ukma-contract-check
cargo run -p ukma-wire-schema -- --check
```

## 4. 測試矩陣（每新行為五類）

| 切面 | positive | negative | authorization | version | failure |
|---|---|---|---|---|---|
| graph-engine export | 4 格式各一 | 空圖／未知格式 | scope 外不讀取（輸入即 scope 過濾集） | v1 wire name | 無（純函式） |
| 10 新操作 | domain + E2E | invalid input（含 schema 負） | 缺 scope → FORBIDDEN；cross-scope → NOT_FOUND | api_major / operation_uri | 缺資源 → NOT_FOUND；index 不存在 → SOURCE_UNAVAILABLE；重複刪除 → ALREADY_DELETED；空參數 → INVALID_ARGUMENT |
| system.metrics | 計數累計 | — | system:read | v1 | gateway 未初始化不適用 |
| LSP | initialize/自訂請求 | 未知方法/操作/參數 | 經 gateway 同路徑 | JSON-RPC 2.0 | 序列化失敗 |

## 5. 邊界與不作為

- 不新增 event 型別、不新增 error code、不新增 scope。
- 不改動既有 112 操作的任何語意；不宣稱 65/65 closure。
- RocksDB 不進入 Domain；graph export 為純函式，無 IO。
- Pro 表面不在本計畫範圍（上游 Pro 封閉，已於 Phase 7 審查記錄）。
- JetBrains 外掛維持 HTTP 傳輸（LSP 傳輸可供選用；外掛切換為後續獨立任務）。
