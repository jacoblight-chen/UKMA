# 測試覆蓋矩陣（P9 建立，P10／ADR-057 收尾）

**規格版本**：1.2.3　**機器可讀來源**：`contracts/testing/test-coverage.json`
**驗證方式**：`crates/ukma-integration/tests/coverage_matrix.rs`

本表列出 §17.1（29 項強制測試）與 §29.4（40 項 fault matrix）的每一項處置
（disposition）。三種處置的定義：

| Disposition | 意義 |
|---|---|
| `covered` | 有實際可執行的 Rust 測試，且該測試名稱由機器驗證存在 |
| `partial` | 有實際測試覆蓋核心語意，但有具體未覆蓋面（見 note） |
| `gap` | 尚無測試；必須寫明原因，不得省略 |

**合計 69 項，100% 已分類**：`covered` **64**、`partial` **5**、`gap` **0**（ADR-057 之後）。
（「100% disposition」指每一項都有處置，**不等於**全部通過。）

## 缺口（不得宣稱完成）

| ID | 原因 |
|---|---|

## 部分覆蓋（有測試，但有已知未覆蓋面）

| ID | 未覆蓋面 |
|---|---|
| DB-005 | memory TTL/hold 有測試，但沒有跨域測試證明 code/document artifact 未被刪 |
| MEM-006 | script 分段與跨語矛盾有測試，但沒有 zh/en/ja 同義查詢的端到端測試 |
| FO-005 | 跨語 gate 有測試，但多語 fold 保留原文命題無專屬測試 |
| ML-002 | analyzer id 有記錄、BM25 有測試，但無 ICU/CJK analyzer 可重現性測試（未引入 ICU） |
| SR-001 | memory hold 阻止自身刪除有測試，但未證明 code/document 來源不受 memory TTL/RL 影響 |

## 已收尾的項目

| ID | 由 | 到 | 依據 |
|---|---|---|---|
| REP-003 | gap | **covered** | `LICENSE` ＋ `NOTICE` ＋ `contracts/testing/third-party-inventory.json` ＋ `release_evidence.rs`（ADR-056） |
| OPS-001 | gap | **covered** | `disaster_recovery.rs`：backup / restore / re-index / rollback ＋ 排程備份（ADR-057） |
| FM-006 | gap | **covered** | `storage-ports` migration runner （流程式鏈、fail-closed、版本後寫）＋`ukma-integration/tests/schema_migration.rs` 對 embedded 與 SQLite 同一契約（ADR-059） |
| FM-010 | gap→partial | **covered** | `ukma-bench` 三份報告；**1M episodes 實測 0 失敗**（ADR-057） |
| SR-004 | gap | **covered** | `ukma-adapter-sqlite` 跑同一契約＋兩後端等價（ADR-057） |
| DB-002 | covered | covered（範圍擴大） | 兩個 canonical 後端 ＋ 兩個 graph store 跑同一契約 |
| DB-010 | covered | covered（範圍擴大） | 九步 cutover ＋ fencing ＋ rollback ＋ 排程備份還原 |



## E2E-API-01..12（§38.1）

全部 12 個 scenario 在 `crates/ukma-integration/tests/e2e_api.rs` 有對應測試；
崩潰重播（E2E-API-06/09/12 的 chaos 面）在 `tests/chaos.rs`。

| Scenario | 測試 |
|---|---|
| E2E-API-01 | `e2e_api_01_commit_analysis_and_symbol_search_are_bound_to_one_commit` |
| E2E-API-02 | `e2e_api_02_document_ingest_keeps_source_coordinates_and_reports_partial` |
| E2E-API-03 | `e2e_api_03_accepted_link_carries_verifiable_evidence_into_coverage` |
| E2E-API-04 | `e2e_api_04_a_new_head_makes_the_link_stale_and_revalidation_restores_it` |
| E2E-API-05 | `e2e_api_05_a_parent_sees_the_folded_result_not_the_child_transcript` |
| E2E-API-06 | `e2e_api_06_paging_protects_blocks_and_reconsolidation_uses_the_etag` |
| E2E-API-07 | `e2e_api_07_consolidation_promotes_only_at_the_commit_stage_and_replays` |
| E2E-API-08 | `e2e_api_08_a_connected_cross_tenant_edge_never_leaks_between_tenants` |
| E2E-API-09 | `e2e_api_09_a_generation_is_refused_until_the_projection_is_published` |
| E2E-API-10 | `e2e_api_10_the_shipped_adapters_need_no_rocksdb_and_meet_the_minimum_profile` |
| E2E-API-11 | `e2e_api_11_a_store_copy_preserves_logical_ids_revisions_and_scope` |
| E2E-API-12 | `e2e_api_12_a_lost_response_is_resolved_by_the_idempotency_receipt` |

## PROP-01..10（§38.2）

| Property | 測試 |
|---|---|
| PROP-01 | `prop_01_a_hidden_object_is_indistinguishable_from_an_absent_one` |
| PROP-02 | `prop_02_symbol_identity_does_not_depend_on_file_order` |
| PROP-03 | `prop_03_graph_cycles_and_bounds_terminate_deterministically` |
| PROP-04 | `prop_04_repeated_mutations_converge_on_one_canonical_version` |
| PROP-05 | `prop_05_eviction_never_touches_protected_or_task_state_blocks` |
| PROP-06 | `prop_06_an_accepted_link_never_outlives_its_evidence_revision` |
| PROP-07 | `prop_07_vectors_from_different_profiles_cannot_be_compared` |
| PROP-08 | `prop_08_pagination_is_stable_and_duplicate_free` |
| PROP-09 | `prop_09_cancelling_a_job_keeps_the_committed_data_and_the_final_state` |
| PROP-10 | `prop_10_every_artefact_is_projected_from_one_registry` |

## Chaos / 崩潰重播（§38.1 的 chaos 面、P9-WP5）

| 測試 | 崩潰點 |
|---|---|
| `chaos_after_commit_before_publish_replays_to_exactly_one_ready_generation` | canonical commit 後、projection publish 前 |
| `chaos_a_crashed_projection_never_publishes_a_partial_index` | 半套 projection 落地、ack 前 |
| `chaos_a_job_cancelled_before_the_commit_leaves_no_canonical_data` | commit 前的 cancel 與 commit 後的 cancel 對照 |

## 為何缺口不是「通過」

`coverage_matrix.rs` 會拒絕：重複分類、未知 ID、未分類的強制測試、`gap` 沒有原因、
`covered`/`partial` 引用不存在的測試函式。因此這份矩陣本身是一個 gate：把某項從
`gap` 改成 `covered` 而不寫測試會直接讓 build 變紅。
