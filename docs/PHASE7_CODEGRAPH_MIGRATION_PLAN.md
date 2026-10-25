# UKMA Phase 7 — CodeGraph 原始碼移植與同步重構執行規格

**狀態：工程實施計畫（未開始移植）**  
**對應基準：UKMA v1.2.3，ADR-021～025；Phase 6R Rust Workspace**  
**語言：Rust；IDE：VS Code + GitHub Copilot**

## 1. 目標與邊界

Phase 7 將 CodeGraph 的實際 Rust 原始碼，依已鎖定的上游 Commit 逐個 Vertical Slice 搬入 UKMA，**搬移當下直接重構**為 UKMA 的 Domain Ports、Database-Agnostic Storage Ports 與型別契約，並使用 Golden / Contract / Regression Tests 驗證。Phase 7 不允許將 Python 參考 Adapter 當成正式 Runtime，也不允許宣稱尚未測試的功能已完成。

**前置 Gate：** Phase 6R 的 `cargo fmt --all -- --check`、`cargo check --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace --all-targets` 必須由 VS Code 本機或 CI 真正執行成功；否則 Phase 7 只能進行上游稽核、來源映射、準備 fixtures，**不得合併** Rust runtime 移植成果。Phase 6R 尚未被確認通過 Cargo 驗收。

## 2. 不可違反的設計限制

1. **雙 Repository**：CodeGraph 的唯讀 Git mirror/reference 與全新 `ukma` Git history 分離；原始碼來源保留完整 SHA、相對路徑、授權與測試對照。
2. **Rust-native**：Production Runtime、Storage Adapter、Graph、Parser、Retrieval 與測試均使用 Rust；Python 只能做非必要輔助驗證。
3. **Database-Agnostic**：Domain Crates 不得匯入 RocksDB SDK / vendor schema；任何儲存依賴透過 Storage Port 注入。RocksDB 只能是可選 Adapter。
4. **單一權威與版本**：程式碼與文件以 SourceRevision 為準，記憶以 Memory Domain 為政策權威；TTL/Folding 不可刪除 Knowledge Source 或 Evidence。
5. **契約優先**：CAPI-01～13、65 操作目錄、ErrorCode、ACL、Job/Commit/Projection 與事件語意以 `docs/UKMA_v1.2.3_Full_Spec.md` 為準。Schema 未閉合者必須記錄待決，不可自行定為正式標準。
6. **可重現**：提交 lockfile、toolchain、配置、fixtures 與輸出報告；基準使用完整 Git SHA，不得以浮動 `main` 取代。
7. **授權合規**：對每份移植檔案查證 License、Copyright、NOTICE、第三方依賴；未完成檢查不得合併或發行。

## 3. 推薦工作目錄

```text
workspace/
├── codegraph-upstream.git/      # mirror，唯讀，不在 UKMA Git Repo 內
├── codegraph-reference/         # 上游來源分析與 golden tests
└── ukma/                         # 獨立 Git Repo，從 Phase 6R 工作區開始
    ├── crates/ukma-contracts/
    ├── crates/ukma-code-runtime/
    ├── crates/ukma-graph-engine/ # Phase 7 S1 達到契約門檻後新增
    ├── crates/ukma-parser-api/   # Phase 7 S2（候選）
    ├── docs/migration/
    └── tests/golden/
```

以下是在 `workspace/` 中執行的 Linux/Git Bash 範例（Windows 可用 Git Bash）。執行前請確認網路、磁碟空間及授權：

```bash
git clone --mirror https://github.com/codegraph-ai/CodeGraph.git codegraph-upstream.git
git clone https://github.com/codegraph-ai/CodeGraph.git codegraph-reference
cd codegraph-reference
git rev-parse HEAD             # 記錄完整 SHA，不可跳過
git status --porcelain
# 使用經審核的 SHA：git checkout --detach <VERIFIED_SHA>
# 在 UKMA 根目錄獨立 git init；不要將 mirror 納入 UKMA git
```

不應事先臆造 CodeGraph 的原始碼檔案對映；必須依實際上游 SHA 查證 Cargo workspace / crate graph 後產生 `source-map.yaml`。

## 4. 階段與 Gate（與 UKMA v1.2.3 §4.2 對齊）

| Slice | 交付 | Golden/Contract 驗收 | Gate |
|---|---|---|---|
| S0 Upstream Baseline | Mirror、ref checkout、SHA、License Audit、Cargo dependency tree、上游測試結果 | SHA 可重現、fixtures 可重跑、必要授權文件保存 | 無 S0 不得進 S1 |
| S1 Graph / Storage | Canonical Node/Edge、GraphStore Port、至少兩個 Adapter 測試 | ID、edge、traversal、ACL、資料版本及重啟；原始演算法對照 | 無 backend 直接耦合 |
| S2 Parser / Symbols | Parser API、語言解析器、symbol/references | AST / Symbol spans、錯誤處理與版本一致 | CAPI 相容 |
| S3 Retrieval / Index | BM25、Vector、Hybrid、Incremental Index | 查詢語意、ranking、ACL、freshness、retry | 索引模型版本化 |
| S4 Document | Markdown 既有能力搬移、DIE Adapter | heading/source span、版本與 evidence | 不以 memory TTL 清除文件 |
| S5 Memory | CodeGraph memory 移植並納入 UKMA memory policy | WM/EM/SM 隔離、no-loss migration | 無雙權威 |
| S6 Gateway | MCP / CLI / server 封裝 | 對外 API Schema 與舊工具 regression | Breaking API 版本化 |
| S7 Traceability | Code ↔ Document ↔ Tests ↔ Memory | 追溯有效性、歷史版本與權限 | 跨領域 E2E |

**規則：** 一次只實作一個可驗證 Slice；S1 最小範圍可先限定 Node/Edge 的 create/read 與 `traverse`，先測試再擴張。此表為順序與門檻，不表示已移植任何模組。

## 5. S0：第一個可執行工作包

- 建立 `docs/migration/upstream-lock.yaml`：Git URL、full SHA、license audit、toolchain、上游測試命令與結果。
- 用 `cargo metadata --locked --format-version 1` 或適當指令匯出上游 Cargo dependency graph；檢查 Rust Features。
- 跑 CodeGraph 上游可行的 `cargo test` / 官方建置指令；環境阻礙應作為 blocked 記錄，不可標記成功。
- 由實際檔案解析建立 `docs/migration/source-map.yaml`；標明可重用/需改寫/外部依賴。
- 選一個小型 Graph Fixture，保存為 `tests/golden/`；記錄輸入、輸出、來源 SHA 與合法預期差異。
- 設定 repo 保護規則，拒絕不含 migration ledger / license / tests 的 S1 pull request。

## 6. S1：Graph Engine 最小 Vertical Slice

**設計順序：** 先訂 `NodeId`、`EdgeId`、`GraphNode`、`GraphEdge`、`RevisionRef`、`AuthContext`、`GraphQuery` 的正式邏輯契約 → 定義 `GraphStore` Port → 搬移 CodeGraph 演算法與型別 → 實作可測的 Memory Adapter（用於測試）及第一個持久化 Adapter → 擴充第二個正式 backend 候選的 Adapter → 執行同一組 Contract Tests。

- Canonical IDs 在 Adapter 更換後不變；不可將 vendor-specific key 作為對外 ID。
- Graph traversal MUST 在過濾 ACL 後才回傳節點或邊，不能從關係存在與否洩漏跨 project 資料。
- 一個更新的 Commit + Outbox 同屬單一權威交易；若索引投影失敗，標記 projection lag，不能把 committed 資料回滾成失敗。
- Idempotency key 與 payload fingerprint 相同時重試返回相同結果；同 key 不同 payload 回傳衝突。
- Native graph query 的非可攜特性必須由 CapabilityNegotiation 明確聲明，無功能時回報 `CAPABILITY_UNAVAILABLE`（精確 wire error 需以 v1.2.3 Registry 核准值為準）。

**Acceptance（最低要求）：**

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

需有 ID/traversal parity、ACL isolation、revision invalidation、ETag、Outbox replay、backend substitution、fuzz/property-based 測試（具體測試成熟度應在 source-map 及 contract matrix 對照）。**單一 Memory Adapter + JSON 開發用 Adapter 不能證明跨生產 DB 可攜性。**

## 7. Migration Ledger（每個 Slice 必須填寫）

```yaml
slice_id: S1-graph-minimal
upstream:
  repository: https://github.com/codegraph-ai/CodeGraph
  commit: "<verified-full-sha>"
  paths: []   # 必須在實際 checkout 查證後填寫
target:
  repository: ukma
  crate: ukma-graph-engine
  files: []
license:
  identified: false
  notices_preserved: false
changes: []
behavior:
  preserved: []
  intentional_breaking: []
tests:
  golden: pending
  contract: pending
  integration: pending
  rust_cargo: pending
review:
  owner: "<reviewer>"
  status: pending
```

CI **MUST FAIL** 如果：source_sha 未鎖定、移植來源路徑空白、License 未審核、test status 偽造、Domain 有直接 vendor SDK import、Rust Cargo fail、Schema drift 未記錄、重大差異未核准。

## 8. GitHub Copilot 執行方式

先在 VS Code 中執行 **Phase 6R Cargo 驗證**；通過後開啟 Copilot Agent Mode，讀取：

- `docs/UKMA_v1.2.3_Full_Spec.md`（規範權威）
- `docs/PHASE7_CODEGRAPH_MIGRATION_PLAN.md`（本工程流程）
- `.github/copilot-instructions.md`
- `docs/migration/upstream-lock.yaml` 與 `source-map.yaml`（建立後）

建議使用 `.github/prompts/phase7-s0-upstream-baseline.prompt.md` 啟動第一個任務。**不可要求 Agent 在 S0 完成前一次生成整個 Graph Engine。** Copilot 對 shell 的使用權限和下載上游 repo 的授權，仍需依 VS Code 的設定及使用者確認。

## 9. Status 與交付定義

目前此套件只新增 Phase 7 **計畫文件與啟動範本**；並未下載 CodeGraph、未鎖定上游 SHA、未移植任何原始碼，也未通過 Rust 編譯。成功的 S0 應提交上游鎖定資料、來源映射、基準測試、稽核結果；成功的 S1 應另提交 Rust crate、兩組 Adapter 與 Golden/Contract Tests 結果。不得以文件交付代替程式碼驗收。
