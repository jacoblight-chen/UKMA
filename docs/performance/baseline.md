# 效能基準（P10-WP3, FM-010）

**量測日期**：2026-10-25　**量測方式**：`ukma-bench`（`cargo run --release -p ukma-bench`）
**機器可讀報告**：`docs/performance/baseline.json`、`docs/performance/scale-probe-20k.json`

## 1. 量測內容與**未**量測內容

`ukma-bench` 透過 public API（envelope → gateway → memory service）跑一個確定性工作負載：

- **寫入**：`memory.store_episode` × N（每個 episode 100% 可重現的摘要、固定 evidence）
- **查詢**：`memory.search`（BM25 詞彙檢索，`max_results=20`、`detail=title`）× M

報告中的 `not_measured` 欄位明列未量測項：**process RSS / VRAM**、**第二個後端**（profile B/C 宣告 unavailable）、
**1M episode 規模**。RAM/VRAM 需要另一個依賴或 OS API，本階段不引入；不以估計值充當量測值。

## 2. 基準線（profile A / embedded in-memory、release build、單執行緒）

工作負載：2 000 episodes + 500 queries = 2 500 operations。

| 操作 | P50 | P95 | P99 | max | 吞吐 | 失敗率 |
|---|---:|---:|---:|---:|---:|---:|
| `memory.store_episode` | **0.011 ms** | 0.019 ms | 0.036 ms | 0.26 ms | ~69 900 ops/s | 0 |
| `memory.search`（2 000 筆） | **18.7 ms** | 21.1 ms | 23.6 ms | 22.2 ms | ~52 ops/s | 0 |
| 合計 | — | — | — | — | — | **0.0 %**（0/2 500） |

環境：Windows / x86_64 / release / 單執行緒；無 embedding model；analyzer = BM25 詞彙。

## 3. 規模探測（20 000 episodes）

同一 harness，`--episodes 20000 --queries 100`：

| 操作 | P50 | P95 | P99 | 吞吐 | 失敗率 |
|---|---:|---:|---:|---:|---:|
| `memory.store_episode` | 0.011 ms | 0.016 ms | 0.046 ms | ~71 500 ops/s | 0 |
| `memory.search` | 98.4 ms | 103.5 ms | 108.0 ms | ~10 ops/s | **100 %（`RESOURCE_LIMIT`）** |

**這是有界的正確行為，不是崩潰**：`memory-domain/src/search.rs` 宣告
`MAX_CANDIDATES = 10_000`，候選超過上限時回 `RESOURCE_LIMIT`（§31.4 的有界工作量規則），
而不是無界掃描。因此：

1. 寫入路徑與規模無關（P50 幾乎不變）；
2. 詞彙檢索是 O(n) 且受 10 000 候選上限約束 → **目前無法在單一查詢內服務 1M episodes**；
3. FM-010（1M episode 基準）因此維持 **partial**：harness 與報告可重現，但 1M 規模未達標，
   需要 ANN 索引（HNSW）或分區查詢才可能通過。此為已知缺口，不宣稱通過。

## 4. 重現方式

```powershell
cargo run --release -p ukma-bench -- --episodes 2000 --queries 500 --out docs/performance/baseline.json
cargo run --release -p ukma-bench -- --episodes 20000 --queries 100 --out docs/performance/scale-probe-20k.json
```

輸出 JSON 內含 `command`、`profile`、`hardware`、`workload`（語言/backend/analyzer/embedding version）
與 `results`（各百分位、吞吐、失敗率、失敗碼分佈）——即 §P10-WP3 要求的「可重現配置」。

## 5. 已知限制

- 只有 profile A（in-memory）可量測；B/C 需要 PostgreSQL / Qdrant / Neo4j，本環境不存在。
- 沒有 embedding / reranker，因此向量檢索與混合檢索延遲**未被量測**。
- 沒有 RSS / VRAM / 索引大小（in-memory profile 不提供 on-disk 索引），`index.bytes` 為 `null` 並附註原因。
- 未設定 SLA：本文件只記錄觀察值，不宣稱任何延遲目標已達成。
