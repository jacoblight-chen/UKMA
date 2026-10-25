# 效能基準（P10-WP3, FM-010）

**量測日期**：2026-10-25　**量測方式**：`ukma-bench`（`cargo run --release -p ukma-bench`）
**機器可讀報告**：[baseline.json](./baseline.json)、[scale-probe-20k.json](./scale-probe-20k.json)、[scale-probe-1m.json](./scale-probe-1m.json)

## 1. 量測內容

`ukma-bench` 透過 public API（envelope → gateway → memory service）跑確定性工作負載：

- **寫入**：`memory.store_episode` × N（可重現摘要，每個 episode 帶 `topic{index % 200}` 標記）
- **查詢**：`memory.search` 查 `topic137`（約選中 1/200 的語料）× M

報告欄位包含 `command`、`profile`、`hardware`、`workload`（語言 / backend / analyzer / embedding version /
查詢選擇性）、`results`（P50/P95/P99、吞吐、失敗率、失敗碼分佈）、`index`（on-disk 位元組數、後端）與
`memory`（process RSS）。

**已量測**：延遲百分位、吞吐、失敗率、process RSS（Windows `K32GetProcessMemoryInfo` / Linux `/proc/self/statm`）、
SQL backend 的 on-disk 大小（同一工作負載寫入 `ukma-adapter-sqlite` 後 checkpoint 量測檔案大小）。

**未量測（報告中明列）**：VRAM（此 profile 不用 GPU、無 embedding model，屬 *not applicable* 而非未量測）、
profile C 的伺服器後端（宣告 unavailable）、多行程並行。

## 2. 基準線（profile A / embedded in-memory、release、單執行緒）

2 000 episodes + 500 queries：

| 操作 | P50 | P95 | P99 | 吞吐 | 失敗率 |
|---|---:|---:|---:|---:|---:|
| `memory.store_episode` | **0.012 ms** | 0.019 ms | 0.037 ms | ~67 200 ops/s | 0 |
| `memory.search` | **15.0 ms** | 16.7 ms | 17.8 ms | ~66 ops/s | 0 |

RSS 21 MiB；SQL backend on-disk 452 KiB（2 000 筆 canonical 記錄）。

## 3. 規模探測

| 規模 | store P50 / 吞吐 | search P50 / P95 / P99 | search 吞吐 | 失敗率 | RSS | on-disk（SQL） |
|---|---:|---:|---:|---:|---:|---:|
| 20 000 | 0.011 ms / ~72 700 ops/s | **2.82** / 4.11 / 7.77 ms | ~318 ops/s | 0 | 78 MiB | 4.0 MiB |
| 1 000 000 | 0.011 ms / ~69 200 ops/s | **204** / 210 / 221 ms | ~4.4 ops/s | **0** | 3 721 MiB | 207 MiB |

**1M episodes 可查詢且零失敗**。兩個關鍵事實：

1. **寫入與規模無關**：P50 從 2k 到 1M 都是 0.011 ms，吞吐穩定在 ~69k ops/s。
2. **查詢是次線性的**：20k 時 P50 2.8 ms（比 2k 的 15 ms 更快，見下），1M 時 204 ms。
   其中約一半是每次查詢計算語料指紋（O(n) hashing）以偵測索引是否過期，屬已知可優化點
   （改為增量維護指紋即可降到 O(候選數)）。

### 為何 20k 比 2k 更快

2 000 筆走「直接掃描」路徑（每次查詢對全部可見文件重建 BM25），15 ms；
超過 `MAX_CANDIDATES = 10_000` 後走**候選索引**路徑（倒排 postings 只挑出含查詢詞的文件），
所以 20k 只要 2.8 ms。交叉點如實記錄：小語料的絕對延遲仍可接受，但索引路徑在 10k 以上明顯更快。

### 為何「匹配全部」的查詢仍會被拒

候選上限（10 000）現在套用在**匹配數**上：查 `shared`（每筆都有）在大語料下會回
`RESOURCE_LIMIT`，而不是靜默截斷或全表掃描。此行為由
`crates/memory-domain/tests/scale_contract.rs` 固化（有界工作量保證，§31.4）。

## 4. 重現方式

```powershell
cargo run --release -p ukma-bench -- --episodes 2000 --queries 500 --out docs/performance/baseline.json
cargo run --release -p ukma-bench -- --episodes 20000 --queries 200 --out docs/performance/scale-probe-20k.json
cargo run --release -p ukma-bench -- --episodes 1000000 --queries 100 --out docs/performance/scale-probe-1m.json
```

1M 執行在本機（32 GiB RAM）約需 15 分鐘、RSS 3.7 GiB。

## 5. 已知限制

- 只有 profile A（in-memory）與 profile B 的 **on-disk 大小**可量測；profile C 需要 PostgreSQL/Qdrant/Neo4j。
- 沒有 embedding / reranker，**向量與混合檢索延遲未被量測**。
- 單行程、單執行緒；沒有並發吞吐數字。
- **不宣稱任何 SLA**：本文件只記錄觀察值。
- 效能數字綁定機器（Windows / x86_64 / release）；跨機器比較前必須看 `hardware` 欄位。
