# UKMA v1.2.3 — Rust 實作工作區

這是依 [`docs/UKMA_v1.2.3_Full_Spec.md`](docs/UKMA_v1.2.3_Full_Spec.md) 以 **Rust 實作** 的正式工作區：不是 Python runtime，也不是「已完成」的 CodeGraph 遷移（遷移狀態見 `docs/migration/ledger/`；MIG-01 已依 ADR-063 永久決策）。

## 目前狀態（2026-10-25，全部可機器驗證）

| 項目 | 狀態 | 驗證方式 |
|---|---|---|
| 85 個操作（code 27 / document 16 / memory 25 / traceability 13 / system 4） | 全部實作並註冊於 registry | `contracts/api/operation-registry.yaml`、`ukma-server --operations` |
| 37 個錯誤碼（封閉集） | registry 與實作一致 | `cargo run -p ukma-contract-check`（6579 checks / 0 failed） |
| §29.4 40 項 + §17.1 29 類行為 ID | 85/85 covered（0 partial / 0 gap） | `contracts/testing/test-coverage.json` + `coverage_matrix` gate |
| 聲明層級 | `implementation_compliant` 與 `production_ready` 由證據**推導**為 true | `claim_level` gate、`docs/implementation-status.yaml` |
| 儲存端口 | 6 個 trait + GraphStore；Profile A/B 已實作，Profile C adapter 真實且配置驅動 | `crates/storage-ports`、`crates/adapters/*`、ADR-057/061/062 |
| 測試 | 870 passed / 0 failed | `cargo test --workspace --all-targets` |

**誠實邊界**（已記錄的決策，不是缺口）：
- **JsonFileAdapter 僅供開發**：無跨進程安全/耐久性保證；持久化用 Profile B（SQLite）或 Profile C（PostgreSQL + Qdrant + Neo4j）。
- **Profile C 三方等價**建立在 ADR-062 一次運行的已檢入證據上（本機無 Docker/psql/Qdrant/Neo4j）。
- **8 種語言語法偏差**（7 個 pinned 非 baseline crate + kotlin ng fork）以 golden fixture 量測，非對照上游行為（ADR-063）。
- PDF 不含 OCR（ADR-048：明確 partial）；VRAM 未量測（已量測 RSS 與磁碟）。

## 環境要求

- Rust stable（[rustup](https://rustup.rs)）＋ C/C++ 编译器與 linker。
- **Windows**：MSVC 工具鏈，必要時安裝 Visual Studio C++ Build Tools；若 PowerShell 腳本執行被管理員政策限制，可改由 CMD 或 VS Code task 直接執行 `cargo.exe`（無需啟用 PowerShell 腳本）。
- **Linux/macOS**：rustup 安裝 Rust stable，確認 C/C++ 編譯器與 linker 已安裝。
- 建構與運行 Rust **不需要 Python**；`scripts/gen_contracts.py` 僅 CI 的 codegen-reproducibility 步驟需要 python3。

## 快速開始

```shell
git clone https://github.com/jacoblight-chen/UKMA.git && cd UKMA
cargo check --workspace --all-targets   # 確認編譯
cargo run -p ukma-server -- --health    # 健康負載
cargo run -p ukma-cli -- tools          # 列出 85 個操作
```

### 驗收命令（與 CI `contract-closure.yml` 同款）

```shell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets            # 預期 870 passed / 0 failed
cargo run -p ukma-contract-check              # 預期 6579 checks / 0 failed
cargo test -p ukma-integration --test coverage_matrix --test claim_level
cargo test -p code-intelligence --test s2_coverage
```

`scripts/verify_rust.ps1` / `verify_rust.sh` 是上述 cargo gate 的包裝腳本。

## 啟動與執行

### 1. HTTP 伺服器（Profile A，開發預設）

無 async runtime：單執行緒、每連線一個請求、`Connection: close`。

```shell
cargo run -p ukma-server                        # 監聽 127.0.0.1:8080
cargo run -p ukma-server -- serve --addr 0.0.0.0:9090
cargo run -p ukma-server -- --manifest          # 輸出 tool-manifest.json
cargo run -p ukma-server -- --operations        # 列出暴露的操作 id
cargo run -p ukma-server -- --help
```

REST 面（spec §31.2）：
- 所有 domain 操作都是 `POST /api/v1/{domain}/{operation}`（讀取也是 POST）
- `GET /api/v1/health`、`GET /api/v1/jobs/{job_id}`
- 請求頭：`x-ukma-tenant-id`、`x-ukma-project-id`、`x-ukma-principal-id`、`x-ukma-scopes`（逗號分隔）、`x-ukma-trace-id`
- 完整的真實路由以 `cargo run -p ukma-cli -- routes` 輸出為準；每個操作的路徑、方法、scope 都由 registry 派生，伺服器不另行聲明

示例（`memory.store_episode`，MAPI-06；完整欄位契約以 `contracts/schema/v1/wire.schema.json` 為準）：

```shell
curl -s -X POST http://127.0.0.1:8080/api/v1/memory/store_episode \
  -H "x-ukma-tenant-id: tenant-dev" -H "x-ukma-project-id: project-dev" \
  -H "x-ukma-principal-id: principal-dev" \
  -H "content-type: application/json" \
  -d '{"input":{"session_id":"s-1","agent_id":"agent-dev","agent_depth":0,
       "episode_type":"ConversationTurn","summary":"hello ukma",
       "full_text_ref":null,"key_facts":["ukma is rust-first"],
       "outcome":"Success","source_refs":[],"importance_score":0.8,
       "language":"en","evidence":[]}}'
```

### 2. CLI（與伺服器走同一個 gateway）

CLI 已合併為單一 `ukma` 二進位（`cargo build -p ukma-cli --release` → `target/release/ukma`），與 `ukma-server` 走同一個 gateway：

```shell
cargo run -p ukma-cli -- health               # 健康負載
cargo run -p ukma-cli -- manifest            # tool manifest
cargo run -p ukma-cli -- capabilities        # system.capabilities
cargo run -p ukma-cli -- tools [--domain memory]
cargo run -p ukma-cli -- routes             # REST 路由表
cargo run -p ukma-cli -- describe memory.store_episode
cargo run -p ukma-cli -- call memory.search '{"query":{...}}' \
  [--tenant t] [--project p] [--principal u] [--scope s]
```

`describe` 與 `call` 的 JSON 結構以 `wire.schema.json` 為準。

### 3. CodeGraph 式執行（Tier 0–3）

與 CodeGraph 同款的執行路徑，四層全部實作：

| Tier | 能力 | 命令 |
|---|---|---|
| 0 | 單工具直跑（跑完即結束） | `ukma --run-tool <name> --tool-args '<json>'` |
| 1 | MCP stdio 伺服器（JSON-RPC 2.0、MCP `2024-11-05`、serverInfo `ukma`/`1.2.3`） | `ukma --mcp` |
| 2 | 自動索引工作目錄（內容位址 revision、`@cwd` 別名、dirty snapshot） | `ukma --index-cwd <path>`、`ukma-server --index-cwd <path>` |
| 2+ | 索引選項＋搜尋策略（多根、tool-surface profile、exclude、檔案上限、watch、split-identifiers、full-body、graph-only） | `ukma --workspace <path>` `--profile <all\|core\|graph\|memory\|security>` `--exclude <glob>` `--max-files <n>` `--watch` `--split-identifiers` `--full-body-embedding` `--graph-only` |
| 3 | VS Code 擴充＋ GitHub Action（PR 審查） | `vscode-extension/`、`github-action/` |

```shell
# Tier 0 + 2：索引目錄後以 @cwd 別名執行工具
ukma --index-cwd . --run-tool system.capabilities --tool-args '{}'
# stderr: ukma: indexed N files at revision <40-hex>
# stdout: 渲染的 envelope

# Tier 1：stdio MCP（initialize / tools/list / tools/call）
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' | ukma --mcp
```

`@cwd` 別名在**所有**表面都可用（HTTP、CLI、MCP）：source port 註冊別名後，`code.analyze_repository` 的 `revision.commit_sha` 可直接傳 `"@cwd"`（需 `"dirty_policy": "snapshot"`）。索引是當前樹的**dirty snapshot**（worktree 從不是 clean commit），內容位址（sha256 前 40 hex），不做耐久性/跨進程聲明；**內建排除集**（~47 目錄 ＋ 二進位/存檔/OS 金鑱/機密檔 glob）、隱藏目錄、>256KiB 檔案被略過（見上方 Tier 2+ 索引選項）。

**Tier 3 — VS Code 擴充**（`vscode-extension/`）：`npm install && npm run compile` 後以 Extensions 開發宿主開啟；擴充把 `ukma --mcp --index-cwd <workspace>` 以 `McpStdioServerDefinition` 釋出給編輯器，85 個工具全部進入 Copilot（二進位預設從 PATH 解析，可設 `ukma.serverPath`）。

**Tier 3 — GitHub Action**（`github-action/`）：composite action 建置 `ukma`、`--index-cwd` 索引 checkout、`--run-tool` 跑一個工具並把結果貼成 PR 評論（用法與範例 workflow 見 `github-action/README.md`）。

**Tier 2+ 索引選項**（CodeGraph 式索引 CLI，與 `--index-cwd` 可併用）：
```shell
ukma --index-cwd . --workspace <extra-root> \
     --exclude '**/*.md' --max-files 2000 --watch \
     --full-body-embedding --graph-only
```
- `--workspace <path>`（可重複）：額外根目錄，合併後以 path 去重（碰撞時第一個根勝出）。
- `--exclude <glob>`（可重複）：附加略過 glob（支援 `*`、`?`、`**`），疊加在**內建排除集**之上。內建排除集與 CodeGraph 對齊：~47 個 build/cache/IDE/IaC/sensitive 目錄（`target`、`node_modules`、`vendor`、`venv`、`dist`、`build`、`out`、`__pycache__`、`DerivedData`、`Pods`、`xcuserdata`、`cmake-build-*` 及 `.git`/`.venv`/`.tox`/`.mypy_cache` 等隱藏目錄），加上二進位/原生庫（`*.o *.a *.so *.dylib *.dll *.lib *.exe *.bin *.obj *.pdb`）、存檔（`*.zip *.tar *.gz *.jar *.class *.pyc *.wasm`）、OS 金鑱（`thumbs.db`、`desktop.ini`）與**機密檔**（`*.pem *.key *.p12 *.pfx *.crt *.gpg *.kdbx`、`id_rsa` 等）。
- `--max-files <n>`：索引檔案數上限（在內建 skip 之後截斷）。
- `--watch`：以 mtime 輪詢（2s）偵測變更並重新索引。**兩種語意**：（a）`--mcp --watch`（含根目錄）→ 建立**可熱切換** gateway，把重新索引**熱切換**到活著的 stdio MCP session（CodeGraph `--watch` parity，已升級為 live hot-swap）；（b）`--watch`（非 MCP，含根目錄）→ 一次指令跑完後輪詢重索引（落盤、非熱切換）。

**Tier 2+ 搜尋/嵌入策略**（CodeGraph `--split-identifiers` / `--full-body-embedding` / `--graph-only` 對齊，僅對索引後的樹生效）：
- `--split-identifiers` / `--no-split-identifiers`：camelCase 識別字切分（`getUserById` → `get user by id`）；**預設切分**（維持既有 tokeniser）。
- `--full-body-embedding` / `--no-full-body-embedding`：把符號的**原始碼函式體**納入語料庫再嵌入（依 `definition` 行範圍自索引來源擷取）；**預設關**（維持既有 name/qualified/signature 語料庫；與 CodeGraph 預設 `true` 不同，為保留既有行為）。
- `--graph-only`：僅結構（keyword/BM25）排序，略過 static-semantic 通道；**預設關**。

**`--profile` 工具表面過濾**（CodeGraph `--profile` 對齊，搭配 `--mcp` 過濾曝露的 MCP 工具；不是 exclude 預設集）：
- `all`（預設）：全部 85 個工具。
- `core`：8 個搜尋/上下文工具（`find_symbols`、`get_symbol`、`get_ai_context`、`get_edit_context`、`get_curated_context`、`search_by_pattern`、`search_by_error`、`find_by_signature`）。
- `graph`：17 個結構/圖/impact/PR 工具（callers/callees/deps/impact/compare/circular/hot/dead/entry/related/complexity/module/index_files/pr_context/analyze_repository/get_analysis_job/find_references）。
- `memory`：25 個 `memory.*` ＋ 16 個 `document.*` 工具。
- `security`：社群版**空**（UKMA 不實作 CodeGraph Pro 的 security 工具）。
- 不在 profile 內的工具：`tools/list` 不列出、`tools/call` 以 `-32602 not in active profile` 拒絕。

### CodeGraph 對照（parity）

UKMA 實作了 CodeGraph 的核心執行路徑（Tier 0–3）、16 個 CodeGraph parity 操作與 **27 個 CodeGraph Pro 相容 operations**（10 結構分析＋17 安全分析，相容而非移植），使 CodeGraph 的常用人格能在 UKMA 上運行。

**已覆蓋（16 個 parity 操作）**：

| 域 | 操作 | 對應 CodeGraph 能力 |
|---|---|---|
| code (CAPI-14..27) | `analyze_complexity` | 複雜度報告（圈複雜度熱點） |
| code | `find_circular_deps` | 循環依賴偵測 |
| code | `find_hot_paths` | 熱路徑（高扇入/扇出） |
| code | `find_dead_imports` | 死 import 偵測 |
| code | `get_module_summary` | 模組摘要 |
| code | `search_by_pattern` / `search_by_error` | 模式 / 錯誤訊息搜尋 |
| code | `find_by_signature` | 依簽名（參數數/回傳型別）搜尋 |
| code | `find_entry_points` | 進入點偵測 |
| code | `find_related_tests` | 相關測試聯結 |
| code | `index_files` | 增量檔案索引 |
| code | `pr_context` | PR 差異上下文（base→head） |
| code | `get_ai_context` / `get_edit_context` | AI/編輯上下文組裝 |
| code | `get_curated_context` | 策展上下文 |
| document (DAPI-15..16) | `list_doc_sources` | 文件來源清單 |
| document | `generate_architecture_doc` | 由代碼事實渲染架構文件 |

搜尋以**混合**方式實作：static-embedding（詞元統計向量）＋ BM25 全文 ＋ HNSW 近鄰（`retrieval-engine`），不依賴外部 embedding 模型或 ONNX 執行時。

**CLI 旗標 parity**（CodeGraph CLI 對齊，均已實作＋Rust 測試）：

| 旗標 | CodeGraph 語意 | UKMA 實作 |
|---|---|---|
| `--workspace` | 可重複多根 | 合併多根、path 去重（第一個根勝出） |
| `--exclude` | 可重複 glob | 附加在內建排除集之上 |
| `--max-files` | 檔案上限（預設 5000） | 索引上限 |
| `--watch` | mtime 輪詢重索引 | 2s 輪詢、重索引；`--mcp --watch`（含根）→ 活著 session 的 live hot-swap |
| `--profile` | **工具表面過濾**（all/core/graph/memory/security） | 同一語意：過濾 MCP 工具表面（core=8、graph=17、memory=41、security=空） |
| `--split-identifiers` | camelCase 切分（預設 on） | `tokenize`（切分）vs `tokenize_raw`（不切分） |
| `--full-body-embedding` | 函式體嵌入（CodeGraph 預設 on） | 依 `definition` 行範圍自來源擷取函式體納入語料庫（UKMA 預設 off，保留既有行為） |
| `--graph-only` | 僅結構、無嵌入 | 僅 keyword/BM25 通道排序 |
| 內建排除集 | ~47 目錄 ＋ 二進位/機密 glob | 同集（見 Tier 2+ 索引選項） |

**CodeGraph Pro 相容 operations（27 個，相容而非移植）**：

UKMA 以 `pro` domain 提供 27 個 read-only 的 CodeGraph Pro 相容 operations（`code:read` scope、`immutable_snapshot` 並發、`PRO-01...PRO-27` 測試集）。`--profile security` 暴露 17 個安全分析器；`ukma_system_capabilities` 回報 `pro: 27`。

| 類別 | 操作（10 結構分析 / 17 安全分析，registry `domain.operation`） |
|---|---|
| 結構分析 | `pro.analyze_coupling`、`pro.find_unused_code`、`pro.find_duplicates`、`pro.find_similar`、`pro.cluster_symbols`、`pro.compare_symbols`、`pro.cross_project_search`、`pro.mine_git_history`、`pro.mine_git_history_for_file`、`pro.search_git_history` |
| 安全分析 | `pro.scan_security`、`pro.security_control_flow`、`pro.security_trace_data_flow`、`pro.security_generate_sbom`、`pro.security_audit_deps`、`pro.security_check_unchecked_returns`、`pro.security_check_resource_leaks`、`pro.security_check_misconfig`、`pro.security_check_input_validation`、`pro.security_check_error_exposure`、`pro.security_scan_iac`、`pro.security_check_licenses`、`pro.security_check_secrets_entropy`、`pro.security_detect_injection`、`pro.security_check_search_path`、`pro.security_check_crypto`、`pro.security_export_sarif` |

**`--embedding-model` 與 memory gate（CodeGraph 對齊）**：CLI 接受 `static-256`（預設、無外部權重、離線）、`bge-small`（384d）、`jina-code-v2`（768d）、`granite-97m`（384d）。ONNX 為**真實 runtime**（feature `onnx`，`ort` `load-dynamic` 於運行時載入 ONNX Runtime 並推論），但 UKMA 不自帶 ONNX Runtime 庫與模型權重（由部署側提供）；ONNX 不可用時語意通道 degrade 至 static fallback。`CODEGRAPH_SKIP_MEMORY_CHECK` 為 ONNX ~1.5GB RAM gate 開關：未設定時啟動前探測可用記憶體，不足時 degrade 至 `GraphOnly`；設定後略過 gate（用於已知記憶體充足的部署）。

**AI agent 規則檔**（CodeGraph 對齊）：`agent-rules/` 提供 Claude、Cursor、Windsurf、Codex、Cline 五份預設規則檔（把自然語言意圖對應到 `ukma_*` 工具），`cp agent-rules/<agent>/UKMA.md ~/<agent>/` 即啟用；用法與 intent→tool 對照表見 `agent-rules/README.md`。

**誠實邊界（不宣稱超越）**：
- **CodeGraph Pro**：UKMA 提供 27 個 Pro **相容 operations（重新實作，非原始碼移植）**。安全分析為**啟發式偵測**（模式覆蓋較廣，標記候選模式、**不證明**漏洞存在），亦不取代 SAST/DAST/依賴掃瞄；`--online`（feature `osv`）執行 OSV/依賴遠端查詢（網路操作，不在離線範圍），transport 失敗時 degrade 為 manifest-only。UKMA 不實作 CodeGraph Pro 的商業訂閱能力。
- **ONNX embedding 模型**：**真實 runtime**（feature `onnx`，`ort` `load-dynamic` 於運行時載入 ONNX Runtime、開啟模型、推論、正規化 embedding）已實作，但 UKMA **不自帶** ONNX Runtime 庫、模型權重與 model-matching tokenizer——皆由部署側提供；不可用時 degrade 至 static fallback。`--embedding-model` 可選 `static-256`（預設）/ `bge-small`（384d）/ `jina-code-v2`（768d）/ `granite-97m`（384d）；`CODEGRAPH_SKIP_MEMORY_CHECK` 為 ONNX ~1.5GB RAM gate 開關。
- **JetBrains 外掛**：提供**完整、可建置結構**（startup activity、tool window、action、薄 REST client，皆註冊於 `plugin.xml`）；Gradle/IntelliJ build **未在本倉庫驗收 gate 執行**（需 IntelliJ SDK），不宣稱可分發 .zip 已完成。
- **耐久性/跨進程**：in-memory 索引是當前樹的 dirty snapshot（無耐久性聲明）；持久化走 Profile B（SQLite）或 Profile C（PostgreSQL＋Qdrant＋Neo4j）。
- **`--watch` 熱切換**：`--mcp --watch`（含根目錄）把重新索引**熱切換**到活著的 stdio MCP session；`--watch`（非 MCP）為落盤重索引（非熱切換）。

### 4. 範例與工具

```shell
cargo run -p ukma-code-runtime --example quickstart   # JsonFileAdapter 最小閉環
cargo run -p code-intelligence --example parsed       # 解析範例（另有 cst / nodes）
cargo run -p ukma-bench -- --episodes 2000 --queries 500 --out bench.json
cargo run -p ukma-wire-schema -- --check             # wire.schema.json 與 Rust 類型對齊
cargo run -p ukma-release-evidence -- --check        # SBOM/inventory 派生文件對齊
```

`ukma-bench` 是可重現的效能 harness（FM-010）：走公開 API 存 episodes、做查詢，報告明寫量測了什麼、**沒有**量測什麼（RSS/VRAM、第二個後端、embedding 模型）。

### 5. 儲存 Profile

| Profile | 後端 | 用途 |
|---|---|---|
| A（開發） | in-memory + `JsonFileAdapter`（僅開發） | 本地開發、測試預設 |
| B | SQLite（`rusqlite` bundled、WAL、真 SQL 交易，ADR-057） | 無需伺服器的單機持久化 |
| C | PostgreSQL + Qdrant + Neo4j（真實、配置驅動，ADR-061） | 生產混合部署 |

6 個 adapter（`crates/adapters/`）：`filesystem`（內容位址 artifact）、`embedded`（文件支撐 metadata + durable outbox）、`sqlite`、`postgres`、`qdrant`（REST）、`neo4j`（HTTP 交易 API）。端口在 `crates/storage-ports`：`MetadataPort`、`FullTextPort`、`VectorPort`、`ArtifactPort`、`OutboxPort` + `SchemaVersionPort`/`Migration`/`CapabilityProbe`，另加 `GraphStore`（`crates/graph-engine`）。

## Workspace 組成（30 個 members）

| 層 | Crate |
|---|---|
| 合約 | `ukma-contracts`（wire 類型 + 生成的 OPERATIONS）、`ukma-types` |
| 域引擎 | `code-intelligence`（38 語言註冊表 + parser 端口）、`document-intelligence`（MD/PDF 文本層/DOCX，16 操作）、`memory-domain`（Fractal Memory：FSRS/TTL、折疊、驅逐、分頁，MAPI-01..25）、`traceability-engine`（TraceLink 狀態機，13 操作） |
| 共享核心 | `graph-engine`、`retrieval-engine`（HNSW + vector port）、`embedding-engine`（+ RerankerPort）、`indexing-engine`、`context-engine`（預算/驅逐/折疊/分頁）、`intelligence-core` |
| 儲存 | `storage-ports`、`storage-router`、`adapters/{filesystem,embedded,sqlite,postgres,qdrant,neo4j}` |
| 運行時 | `ukma-code-runtime`（dev-only file adapter + outbox 整合測試）、`ukma-security` |
| 表面 | `ukma-mcp`（gateway＋MCP stdio 伺服器＋`@cwd` 工作目錄索引：scope/授權/冪等/錯誤碼全由 registry 派生）、`ukma-cli`（單一 `ukma` 二進位）、`ukma-server`（`apps/`）、`vscode-extension/`、`github-action/` |
| 驗證與工具 | `ukma-integration`（gate：coverage_matrix、claim_level、port_parity、release_evidence…）、`ukma-contract-check`、`ukma-bench`、`ukma-wire-schema`、`ukma-release-evidence` |

`code-intelligence` 特性：`parser-rust`（預設）、`parser-s2-batch1/2/4`（預設，共 37 個 tree-sitter 語法）、`schema`（schemars 輸出）。8 個偏差語言的 pinned crate 見 ADR-063。

## 合約與代碼生成

- `contracts/api/operation-registry.yaml` 是唯一事實來源；`scripts/gen_contracts.py` 由 CI（`contract-closure`）執行並 diff 生成物（`sdk/typescript/ukma-operations.ts` 等）。
- `wire.schema.json` 由 Rust 類型生成（ADR-058）：`cargo run -p ukma-wire-schema` 重新生成，`--check` 驗證。
- SBOM / 第三方 inventory 是**派生**文件（ADR-059）：`ukma-release-evidence` 重寫、`--check` 驗證、`sign`/`verify` 處理 Ed25519 摘要（金鑰在倉庫外）。

## 文檔地圖

- `docs/UKMA_v1.2.3_Full_Spec.md` — 權威規格
- `docs/UKMA_v1.2.3_IMPLEMENTATION_PLAN.md` — 計畫、缺口分析與 Gate 對照（§7.1 全綠）
- `docs/implementation-status.yaml` — 聲明層級與 gate（由 `claim_level` gate 機器校驗）
- `docs/adr/`（ADR-001…063）、`docs/testing/`（覆蓋矩陣）、`docs/performance/`（基線）、`docs/migration/`（S2 ledger + 狀態）
- `.github/copilot-instructions.md` — 項目級 Copilot 指引；`.vscode/` — tasks、debugger、擴充建議
- VS Code：開啟 `UKMA-Phase6R.code-workspace`（或 **File → Open Folder** 開工作區根），**Terminal → Run Task → Cargo: Check / Cargo: Test / Cargo: Run Example**；用 Copilot Chat 搭配 `.github/copilot-instructions.md` 做 scoped 變更

## 不變式（每個新行為都必須維持）

租戶/項目隔離、樂觀並發（etag CAS）、冪等鍵、outbox 語義、錯誤碼封閉集、domain 不依賴 backend SDK（ADR-021）、domain 知識不受記憶 TTL/折疊影響、每個新行為需附 Rust 正/負/授權/版本/失敗路徑測試。

