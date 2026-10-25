# UKMA ADR 索引

**唯一權威**：`docs/UKMA_v1.2.3_Full_Spec.md` §18（ADR-021…030）、§39（ADR-031/032）、§40.1（ADR-033…040）
**編號治理**：ADR-046（規格內三處 ADR 表存在編號衝突的裁決程序）

每份 ADR 一檔、編號唯一。檔案內容為規格決策的落地記錄（decision / verifiable invariants / consequence / spec refs）。

| ADR | 主題 | 狀態 | 規格來源 |
|---|---|---|---|
| [ADR-021](./ADR-021.md) | Database-Agnostic & Pluggable Persistence 是必須，不是選配 | ACCEPTED | §18, §6 |
| [ADR-022](./ADR-022.md) | 六種 Storage Ports 與能力協商 | ACCEPTED | §18, §6.2/§6.3 |
| [ADR-023](./ADR-023.md) | Logical IDs / Provenance 與 DB Schema 解耦 | ACCEPTED | §18, §5 |
| [ADR-024](./ADR-024.md) | Authoritative writes + durable outbox + generation publication | ACCEPTED | §18, §6.4 |
| [ADR-025](./ADR-025.md) | 雙 Repository + 每 Slice 同步重構 + Contract Tests | ACCEPTED | §18, §4 |
| [ADR-026](./ADR-026.md) | UKMA v1.2.3 為獨立規格，Memory Domain 定義內建 | ACCEPTED | §18, §10/§20 |
| [ADR-027](./ADR-027.md) | Memory Consolidation 採證據先於提升的九步 DAG | ACCEPTED | §18, §10.7/§27.1 |
| [ADR-028](./ADR-028.md) | Knowledge Authority 與 Memory Authority 互不代管刪除權 | ACCEPTED | §18, §10.8/§23 |
| [ADR-029](./ADR-029.md) | Public API 版本化；Private CodeGraph API 可重構 | ACCEPTED | §18, §31.2 |
| [ADR-030](./ADR-030.md) | 初版 Modular Monolith，分散部署按實測決策 | ACCEPTED | §18, §3.1 |
| [ADR-031](./ADR-031.md) | Schema-as-Code and Contract Test Gate | ACCEPTED | §39, §41 |
| [ADR-031a](./ADR-031a-s1-graph-contract-decisions.md) | S1 Graph Contract Decisions（canonical IDs / capability error / wire names） | ACCEPTED | 專案決策（重編自 ADR-031） |
| [ADR-032](./ADR-032.md) | FMA Full Coverage Preservation Inside UKMA | ACCEPTED | §39, §29 |
| [ADR-033](./ADR-033.md) | Context budget 唯一算法 | ACCEPTED | §40.1, §22.1 |
| [ADR-034](./ADR-034.md) | 唯一公開 API Registry | ACCEPTED | §40.1, §32–§35 |
| [ADR-035](./ADR-035.md) | 封閉錯誤碼 | ACCEPTED | §40.1, §31.3 |
| [ADR-036](./ADR-036.md) | 歷史與目前適用性拆分 | ACCEPTED | §40.1, §40.3 |
| [ADR-037](./ADR-037.md) | Canonical Commit / Projection 分離 | ACCEPTED | §40.1, §40.4 |
| [ADR-038](./ADR-038.md) | 最小後端能力 Gate | ACCEPTED | §40.1, §6.3 |
| [ADR-039](./ADR-039.md) | 操作、事件、狀態、Schema、測試單一 Registry | ACCEPTED | §40.1, §40.6 |
| [ADR-040](./ADR-040.md) | Schema Closed / Implementation Compliant 分級 | ACCEPTED | §40.1, §38.3 |
| [ADR-041](./ADR-041.md) | Unified Envelope and Versioned API v1 | ACCEPTED | §39（原 ADR-027，重編） |
| [ADR-042](./ADR-042.md) | Formal 4-Domain Port Contracts | ACCEPTED | §39（原 ADR-028，重編） |
| [ADR-043](./ADR-043.md) | Immutable Revision, Evidence and Stateful Trace Verification | ACCEPTED | §39（原 ADR-029，重編） |
| [ADR-044](./ADR-044.md) | Job, Idempotency, Outbox and Generation Semantics | ACCEPTED | §39（原 ADR-030，重編） |
| [ADR-045](./ADR-045.md) | Crate 命名與目錄對照（§3.1 為權威命名） | ACCEPTED | §3.1 / §39.2 |
| [ADR-046](./ADR-046.md) | ADR 編號治理與衝突裁決記錄 | ACCEPTED | §18 / §39 / §40.1 |
| [ADR-047](./ADR-047.md) | System API Minimal Contract（4 個 system operation 的 wire 形狀） | ACCEPTED | §36.4 |
| [ADR-048](./ADR-048.md) | 實作範圍與 Profile 決策（語言集 / PDF-OCR / DB Profile / Embedding / legacy runtime） | ACCEPTED | §8.1 / §9.1 / §25.2 / §28.2 |
| [ADR-049](./ADR-049.md) | Code Intelligence 服務實作決策（同步核心 + §37 async port、derived projection、version-scoped id、FSM 對照、impact completeness） | ACCEPTED | §7.1 / §32.1 / §32.3 / §37 / §40.4 |
| [ADR-050](./ADR-050.md) | Document Intelligence 實作決策（14 ops、no-OCR 結構性強制、rule-based 需求抽取、span 語意、redaction） | ACCEPTED | §9.1 / §31.4 / §33.1 / §33.2 / §33.3 / §37 / §40.4 / §41.4 |
| [ADR-051](./ADR-051.md) | Fractal Memory 核心實作（P5）：reuse context/retrieval engine、抽取式 fold、FSRS/TTL、gzip 熵、BM25+RRF 檢索、sandbox 隔離、tombstone 與投影撤銷、CAPABILITY_UNAVAILABLE 邊界 | ACCEPTED | §21 / §22 / §23 / §26.4 / §34.1 / §34.2 / §37 / §40.2 |
| [ADR-052](./ADR-052.md) | Memory Advanced 實作決策（P6）：四欄 trust＋Beta/Kalman、fact 三態與 promotion gate、九階段 DAG（Promote 為 commit point）、procedure 生命週期、poisoning quarantine、script 級多語 gate、embedding generation、maintenance jobs；supersedes ADR-051 D-09 | ACCEPTED | §23.3 / §25 / §26.1-§26.3 / §27.1-§27.3 / §29.4 / §34.2 |
| [ADR-053](./ADR-053.md) | Traceability 實作決策（P7）：TraceState 僅流程狀態＋三維有效性、11 條狀態轉移與終態、accepted 後 evidence immutable、coverage 的 unknown/denominator=0 語意、有界 impact、revalidation 永不自動 accept | ACCEPTED | §35.1 / §35.2 / §40.3 / §29.4 |
| [ADR-054](./ADR-054.md) | Gateway 實作決策（P8）：registry-driven manifest/dispatch/rest、ScopeResolver 注入、前置條件不正規化、未宣告碼降級、§40.7 NOT_FOUND 落地、JobIndex 租戶隔離、SDK generic typed、alias inert | ACCEPTED | §31.2 / §31.4 / §31.5 / §36.4 / §40.7 |

## 編號衝突與對照（摘要）

- §39 表的 `ADR-027..030` 與 §18 的 `ADR-027..030` 內容不同 → 重編為 **ADR-041..044**。
- repo 原 `ADR-031`（S1 graph 決策）與規格 `ADR-031`（Schema-as-Code）衝突 → repo 版重編為 **ADR-031a**。
- 新增 **ADR-045**（crate 命名）、**ADR-046**（本治理程序）、**ADR-047**（system API 形狀）、**ADR-048**（範圍與 profile 決策）。

## 新增 ADR 的規則

1. 先確認編號未被佔用（本索引為準）。
2. 若與規格條文衝突，MUST 新增 Superseding ADR 並記錄資料遷移與相容性影響；不得直接無記錄修改強制契約（§19.3）。
3. 每次新增／改狀態，同步更新本索引與 `docs/implementation-status.yaml`（若涉及 gate）。
