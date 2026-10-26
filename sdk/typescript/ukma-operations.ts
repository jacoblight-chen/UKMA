// GENERATED FILE — do not edit by hand.
// Source: contracts/api/operation-registry.yaml (spec §41.1).
// Regenerate: python scripts/gen_contracts.py

export type OperationId =
  | 'code.analyze_repository' |
  | 'code.get_analysis_job' |
  | 'code.cancel_analysis_job' |
  | 'code.get_symbol' |
  | 'code.find_symbols' |
  | 'code.find_references' |
  | 'code.query_callers' |
  | 'code.query_callees' |
  | 'code.query_dependencies' |
  | 'code.analyze_impact' |
  | 'code.get_edit_context' |
  | 'code.compare_revisions' |
  | 'code.invalidate_revision' |
  | 'document.ingest' |
  | 'document.get_ingest_job' |
  | 'document.cancel_ingest_job' |
  | 'document.get_document' |
  | 'document.parse_preview' |
  | 'document.search' |
  | 'document.extract_requirements' |
  | 'document.extract_relations' |
  | 'document.review_requirement' |
  | 'document.list_requirements' |
  | 'document.verify_consistency' |
  | 'document.compare_revisions' |
  | 'document.invalidate_revision' |
  | 'document.get_source_span' |
  | 'memory.open_agent_scope' |
  | 'memory.close_agent_scope' |
  | 'memory.put_working_block' |
  | 'memory.list_working_blocks' |
  | 'memory.remove_working_block' |
  | 'memory.store_episode' |
  | 'memory.search' |
  | 'memory.retrieve' |
  | 'memory.update_revision' |
  | 'memory.propose_fact' |
  | 'memory.verify_fact' |
  | 'memory.find_conflicts' |
  | 'memory.fold' |
  | 'memory.page_out' |
  | 'memory.page_in' |
  | 'memory.begin_reconsolidation' |
  | 'memory.commit_reconsolidation' |
  | 'memory.consolidate' |
  | 'memory.get_consolidation_job' |
  | 'memory.register_procedure_candidate' |
  | 'memory.review_procedure' |
  | 'memory.record_procedure_outcome' |
  | 'memory.forget' |
  | 'memory.evaluate_eviction' |
  | 'memory.get_budget' |
  | 'traceability.propose_link' |
  | 'traceability.validate_link' |
  | 'traceability.accept_link' |
  | 'traceability.revoke_link' |
  | 'traceability.get_link' |
  | 'traceability.list_links' |
  | 'traceability.query_coverage' |
  | 'traceability.analyze_change_impact' |
  | 'traceability.find_design_gaps' |
  | 'traceability.compare_revisions' |
  | 'traceability.explain_with_evidence' |
  | 'traceability.revalidate_stale' |
  | 'traceability.get_revalidation_job' |
  | 'system.capabilities' |
  | 'system.get_operation' |
  | 'system.get_job' |
  | 'system.cancel_job' |
  | 'code.get_ai_context' |
  | 'code.get_curated_context' |
  | 'code.analyze_complexity' |
  | 'code.find_circular_deps' |
  | 'code.find_hot_paths' |
  | 'code.find_dead_imports' |
  | 'code.get_module_summary' |
  | 'code.search_by_pattern' |
  | 'code.search_by_error' |
  | 'code.find_by_signature' |
  | 'code.find_entry_points' |
  | 'code.find_related_tests' |
  | 'code.index_files' |
  | 'code.pr_context' |
  | 'document.list_doc_sources' |
  | 'document.generate_architecture_doc' |
  | 'pro.analyze_coupling' |
  | 'pro.find_unused_code' |
  | 'pro.find_duplicates' |
  | 'pro.find_similar' |
  | 'pro.cluster_symbols' |
  | 'pro.compare_symbols' |
  | 'pro.cross_project_search' |
  | 'pro.mine_git_history' |
  | 'pro.mine_git_history_for_file' |
  | 'pro.search_git_history' |
  | 'pro.scan_security' |
  | 'pro.security_control_flow' |
  | 'pro.security_trace_data_flow' |
  | 'pro.security_generate_sbom' |
  | 'pro.security_audit_deps' |
  | 'pro.security_check_unchecked_returns' |
  | 'pro.security_check_resource_leaks' |
  | 'pro.security_check_misconfig' |
  | 'pro.security_check_input_validation' |
  | 'pro.security_check_error_exposure' |
  | 'pro.security_scan_iac' |
  | 'pro.security_check_licenses' |
  | 'pro.security_check_secrets_entropy' |
  | 'pro.security_detect_injection' |
  | 'pro.security_check_search_path' |
  | 'pro.security_check_crypto' |
  | 'pro.security_export_sarif' |
  | 'pro.security_orchestrated_scan' |
  | 'pro.security_codeql_warmup' |
  | 'code.find_implementors' |
  | 'code.traverse' |
  | 'code.get_detailed_symbol' |
  | 'code.index_directory' |
  | 'code.export_graph' |
  | 'code.find_importers' |
  | 'document.remove_source' |
  | 'memory.context' |
  | 'memory.list' |
  | 'memory.stats' |
  | 'system.metrics';

export interface OperationSpec {
  readonly id: OperationId;
  readonly uri: string;
  readonly mcp: string;
  readonly scopes: readonly string[];
  readonly idempotency: 'required' | 'not_required';
}

export const OPERATIONS: readonly OperationSpec[] = [
  { id: 'code.analyze_repository', uri: 'ukma.code.analyze_repository.v1', mcp: 'ukma_code_analyze_repository_v1', scopes: ['code:index'], idempotency: 'required' },
  { id: 'code.get_analysis_job', uri: 'ukma.code.get_analysis_job.v1', mcp: 'ukma_code_get_analysis_job_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.cancel_analysis_job', uri: 'ukma.code.cancel_analysis_job.v1', mcp: 'ukma_code_cancel_analysis_job_v1', scopes: ['code:index'], idempotency: 'required' },
  { id: 'code.get_symbol', uri: 'ukma.code.get_symbol.v1', mcp: 'ukma_code_get_symbol_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_symbols', uri: 'ukma.code.find_symbols.v1', mcp: 'ukma_code_find_symbols_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_references', uri: 'ukma.code.find_references.v1', mcp: 'ukma_code_find_references_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.query_callers', uri: 'ukma.code.query_callers.v1', mcp: 'ukma_code_query_callers_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.query_callees', uri: 'ukma.code.query_callees.v1', mcp: 'ukma_code_query_callees_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.query_dependencies', uri: 'ukma.code.query_dependencies.v1', mcp: 'ukma_code_query_dependencies_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.analyze_impact', uri: 'ukma.code.analyze_impact.v1', mcp: 'ukma_code_analyze_impact_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.get_edit_context', uri: 'ukma.code.get_edit_context.v1', mcp: 'ukma_code_get_edit_context_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.compare_revisions', uri: 'ukma.code.compare_revisions.v1', mcp: 'ukma_code_compare_revisions_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.invalidate_revision', uri: 'ukma.code.invalidate_revision.v1', mcp: 'ukma_code_invalidate_revision_v1', scopes: ['code:index_admin'], idempotency: 'required' },
  { id: 'document.ingest', uri: 'ukma.document.ingest.v1', mcp: 'ukma_document_ingest_v1', scopes: ['document:index'], idempotency: 'required' },
  { id: 'document.get_ingest_job', uri: 'ukma.document.get_ingest_job.v1', mcp: 'ukma_document_get_ingest_job_v1', scopes: ['document:read'], idempotency: 'not_required' },
  { id: 'document.cancel_ingest_job', uri: 'ukma.document.cancel_ingest_job.v1', mcp: 'ukma_document_cancel_ingest_job_v1', scopes: ['document:index'], idempotency: 'required' },
  { id: 'document.get_document', uri: 'ukma.document.get_document.v1', mcp: 'ukma_document_get_document_v1', scopes: ['document:read'], idempotency: 'not_required' },
  { id: 'document.parse_preview', uri: 'ukma.document.parse_preview.v1', mcp: 'ukma_document_parse_preview_v1', scopes: ['document:analyze'], idempotency: 'not_required' },
  { id: 'document.search', uri: 'ukma.document.search.v1', mcp: 'ukma_document_search_v1', scopes: ['document:read'], idempotency: 'not_required' },
  { id: 'document.extract_requirements', uri: 'ukma.document.extract_requirements.v1', mcp: 'ukma_document_extract_requirements_v1', scopes: ['document:analyze'], idempotency: 'required' },
  { id: 'document.extract_relations', uri: 'ukma.document.extract_relations.v1', mcp: 'ukma_document_extract_relations_v1', scopes: ['document:analyze'], idempotency: 'required' },
  { id: 'document.review_requirement', uri: 'ukma.document.review_requirement.v1', mcp: 'ukma_document_review_requirement_v1', scopes: ['document:review'], idempotency: 'required' },
  { id: 'document.list_requirements', uri: 'ukma.document.list_requirements.v1', mcp: 'ukma_document_list_requirements_v1', scopes: ['document:read'], idempotency: 'not_required' },
  { id: 'document.verify_consistency', uri: 'ukma.document.verify_consistency.v1', mcp: 'ukma_document_verify_consistency_v1', scopes: ['document:analyze'], idempotency: 'not_required' },
  { id: 'document.compare_revisions', uri: 'ukma.document.compare_revisions.v1', mcp: 'ukma_document_compare_revisions_v1', scopes: ['document:read'], idempotency: 'not_required' },
  { id: 'document.invalidate_revision', uri: 'ukma.document.invalidate_revision.v1', mcp: 'ukma_document_invalidate_revision_v1', scopes: ['document:index_admin'], idempotency: 'required' },
  { id: 'document.get_source_span', uri: 'ukma.document.get_source_span.v1', mcp: 'ukma_document_get_source_span_v1', scopes: ['document:read'], idempotency: 'not_required' },
  { id: 'memory.open_agent_scope', uri: 'ukma.memory.open_agent_scope.v1', mcp: 'ukma_memory_open_agent_scope_v1', scopes: ['memory:delegate'], idempotency: 'required' },
  { id: 'memory.close_agent_scope', uri: 'ukma.memory.close_agent_scope.v1', mcp: 'ukma_memory_close_agent_scope_v1', scopes: ['memory:delegate'], idempotency: 'required' },
  { id: 'memory.put_working_block', uri: 'ukma.memory.put_working_block.v1', mcp: 'ukma_memory_put_working_block_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.list_working_blocks', uri: 'ukma.memory.list_working_blocks.v1', mcp: 'ukma_memory_list_working_blocks_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.remove_working_block', uri: 'ukma.memory.remove_working_block.v1', mcp: 'ukma_memory_remove_working_block_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.store_episode', uri: 'ukma.memory.store_episode.v1', mcp: 'ukma_memory_store_episode_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.search', uri: 'ukma.memory.search.v1', mcp: 'ukma_memory_search_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.retrieve', uri: 'ukma.memory.retrieve.v1', mcp: 'ukma_memory_retrieve_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.update_revision', uri: 'ukma.memory.update_revision.v1', mcp: 'ukma_memory_update_revision_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.propose_fact', uri: 'ukma.memory.propose_fact.v1', mcp: 'ukma_memory_propose_fact_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.verify_fact', uri: 'ukma.memory.verify_fact.v1', mcp: 'ukma_memory_verify_fact_v1', scopes: ['memory:review'], idempotency: 'required' },
  { id: 'memory.find_conflicts', uri: 'ukma.memory.find_conflicts.v1', mcp: 'ukma_memory_find_conflicts_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.fold', uri: 'ukma.memory.fold.v1', mcp: 'ukma_memory_fold_v1', scopes: ['memory:fold'], idempotency: 'required' },
  { id: 'memory.page_out', uri: 'ukma.memory.page_out.v1', mcp: 'ukma_memory_page_out_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.page_in', uri: 'ukma.memory.page_in.v1', mcp: 'ukma_memory_page_in_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.begin_reconsolidation', uri: 'ukma.memory.begin_reconsolidation.v1', mcp: 'ukma_memory_begin_reconsolidation_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.commit_reconsolidation', uri: 'ukma.memory.commit_reconsolidation.v1', mcp: 'ukma_memory_commit_reconsolidation_v1', scopes: ['memory:write'], idempotency: 'required' },
  { id: 'memory.consolidate', uri: 'ukma.memory.consolidate.v1', mcp: 'ukma_memory_consolidate_v1', scopes: ['memory:consolidate'], idempotency: 'required' },
  { id: 'memory.get_consolidation_job', uri: 'ukma.memory.get_consolidation_job.v1', mcp: 'ukma_memory_get_consolidation_job_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.register_procedure_candidate', uri: 'ukma.memory.register_procedure_candidate.v1', mcp: 'ukma_memory_register_procedure_candidate_v1', scopes: ['memory:procedure_write'], idempotency: 'required' },
  { id: 'memory.review_procedure', uri: 'ukma.memory.review_procedure.v1', mcp: 'ukma_memory_review_procedure_v1', scopes: ['memory:procedure_review'], idempotency: 'required' },
  { id: 'memory.record_procedure_outcome', uri: 'ukma.memory.record_procedure_outcome.v1', mcp: 'ukma_memory_record_procedure_outcome_v1', scopes: ['memory:procedure_write'], idempotency: 'required' },
  { id: 'memory.forget', uri: 'ukma.memory.forget.v1', mcp: 'ukma_memory_forget_v1', scopes: ['memory:forget'], idempotency: 'required' },
  { id: 'memory.evaluate_eviction', uri: 'ukma.memory.evaluate_eviction.v1', mcp: 'ukma_memory_evaluate_eviction_v1', scopes: ['memory:manage'], idempotency: 'not_required' },
  { id: 'memory.get_budget', uri: 'ukma.memory.get_budget.v1', mcp: 'ukma_memory_get_budget_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'traceability.propose_link', uri: 'ukma.traceability.propose_link.v1', mcp: 'ukma_traceability_propose_link_v1', scopes: ['trace:write'], idempotency: 'required' },
  { id: 'traceability.validate_link', uri: 'ukma.traceability.validate_link.v1', mcp: 'ukma_traceability_validate_link_v1', scopes: ['trace:verify'], idempotency: 'required' },
  { id: 'traceability.accept_link', uri: 'ukma.traceability.accept_link.v1', mcp: 'ukma_traceability_accept_link_v1', scopes: ['trace:approve'], idempotency: 'required' },
  { id: 'traceability.revoke_link', uri: 'ukma.traceability.revoke_link.v1', mcp: 'ukma_traceability_revoke_link_v1', scopes: ['trace:approve'], idempotency: 'required' },
  { id: 'traceability.get_link', uri: 'ukma.traceability.get_link.v1', mcp: 'ukma_traceability_get_link_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'traceability.list_links', uri: 'ukma.traceability.list_links.v1', mcp: 'ukma_traceability_list_links_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'traceability.query_coverage', uri: 'ukma.traceability.query_coverage.v1', mcp: 'ukma_traceability_query_coverage_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'traceability.analyze_change_impact', uri: 'ukma.traceability.analyze_change_impact.v1', mcp: 'ukma_traceability_analyze_change_impact_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'traceability.find_design_gaps', uri: 'ukma.traceability.find_design_gaps.v1', mcp: 'ukma_traceability_find_design_gaps_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'traceability.compare_revisions', uri: 'ukma.traceability.compare_revisions.v1', mcp: 'ukma_traceability_compare_revisions_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'traceability.explain_with_evidence', uri: 'ukma.traceability.explain_with_evidence.v1', mcp: 'ukma_traceability_explain_with_evidence_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'traceability.revalidate_stale', uri: 'ukma.traceability.revalidate_stale.v1', mcp: 'ukma_traceability_revalidate_stale_v1', scopes: ['trace:verify'], idempotency: 'required' },
  { id: 'traceability.get_revalidation_job', uri: 'ukma.traceability.get_revalidation_job.v1', mcp: 'ukma_traceability_get_revalidation_job_v1', scopes: ['trace:read'], idempotency: 'not_required' },
  { id: 'system.capabilities', uri: 'ukma.system.capabilities.v1', mcp: 'ukma_system_capabilities_v1', scopes: ['system:read'], idempotency: 'not_required' },
  { id: 'system.get_operation', uri: 'ukma.system.get_operation.v1', mcp: 'ukma_system_get_operation_v1', scopes: ['system:read'], idempotency: 'not_required' },
  { id: 'system.get_job', uri: 'ukma.system.get_job.v1', mcp: 'ukma_system_get_job_v1', scopes: ['system:read'], idempotency: 'not_required' },
  { id: 'system.cancel_job', uri: 'ukma.system.cancel_job.v1', mcp: 'ukma_system_cancel_job_v1', scopes: ['system:manage'], idempotency: 'required' },
  { id: 'code.get_ai_context', uri: 'ukma.code.get_ai_context.v1', mcp: 'ukma_code_get_ai_context_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.get_curated_context', uri: 'ukma.code.get_curated_context.v1', mcp: 'ukma_code_get_curated_context_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.analyze_complexity', uri: 'ukma.code.analyze_complexity.v1', mcp: 'ukma_code_analyze_complexity_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_circular_deps', uri: 'ukma.code.find_circular_deps.v1', mcp: 'ukma_code_find_circular_deps_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_hot_paths', uri: 'ukma.code.find_hot_paths.v1', mcp: 'ukma_code_find_hot_paths_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_dead_imports', uri: 'ukma.code.find_dead_imports.v1', mcp: 'ukma_code_find_dead_imports_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.get_module_summary', uri: 'ukma.code.get_module_summary.v1', mcp: 'ukma_code_get_module_summary_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.search_by_pattern', uri: 'ukma.code.search_by_pattern.v1', mcp: 'ukma_code_search_by_pattern_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.search_by_error', uri: 'ukma.code.search_by_error.v1', mcp: 'ukma_code_search_by_error_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_by_signature', uri: 'ukma.code.find_by_signature.v1', mcp: 'ukma_code_find_by_signature_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_entry_points', uri: 'ukma.code.find_entry_points.v1', mcp: 'ukma_code_find_entry_points_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_related_tests', uri: 'ukma.code.find_related_tests.v1', mcp: 'ukma_code_find_related_tests_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.index_files', uri: 'ukma.code.index_files.v1', mcp: 'ukma_code_index_files_v1', scopes: ['code:index'], idempotency: 'required' },
  { id: 'code.pr_context', uri: 'ukma.code.pr_context.v1', mcp: 'ukma_code_pr_context_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'document.list_doc_sources', uri: 'ukma.document.list_doc_sources.v1', mcp: 'ukma_document_list_doc_sources_v1', scopes: ['document:read'], idempotency: 'not_required' },
  { id: 'document.generate_architecture_doc', uri: 'ukma.document.generate_architecture_doc.v1', mcp: 'ukma_document_generate_architecture_doc_v1', scopes: ['document:analyze'], idempotency: 'not_required' },
  { id: 'pro.analyze_coupling', uri: 'ukma.pro.analyze_coupling.v1', mcp: 'ukma_pro_analyze_coupling_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.find_unused_code', uri: 'ukma.pro.find_unused_code.v1', mcp: 'ukma_pro_find_unused_code_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.find_duplicates', uri: 'ukma.pro.find_duplicates.v1', mcp: 'ukma_pro_find_duplicates_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.find_similar', uri: 'ukma.pro.find_similar.v1', mcp: 'ukma_pro_find_similar_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.cluster_symbols', uri: 'ukma.pro.cluster_symbols.v1', mcp: 'ukma_pro_cluster_symbols_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.compare_symbols', uri: 'ukma.pro.compare_symbols.v1', mcp: 'ukma_pro_compare_symbols_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.cross_project_search', uri: 'ukma.pro.cross_project_search.v1', mcp: 'ukma_pro_cross_project_search_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.mine_git_history', uri: 'ukma.pro.mine_git_history.v1', mcp: 'ukma_pro_mine_git_history_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.mine_git_history_for_file', uri: 'ukma.pro.mine_git_history_for_file.v1', mcp: 'ukma_pro_mine_git_history_for_file_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.search_git_history', uri: 'ukma.pro.search_git_history.v1', mcp: 'ukma_pro_search_git_history_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.scan_security', uri: 'ukma.pro.scan_security.v1', mcp: 'ukma_pro_scan_security_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_control_flow', uri: 'ukma.pro.security_control_flow.v1', mcp: 'ukma_pro_security_control_flow_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_trace_data_flow', uri: 'ukma.pro.security_trace_data_flow.v1', mcp: 'ukma_pro_security_trace_data_flow_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_generate_sbom', uri: 'ukma.pro.security_generate_sbom.v1', mcp: 'ukma_pro_security_generate_sbom_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_audit_deps', uri: 'ukma.pro.security_audit_deps.v1', mcp: 'ukma_pro_security_audit_deps_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_unchecked_returns', uri: 'ukma.pro.security_check_unchecked_returns.v1', mcp: 'ukma_pro_security_check_unchecked_returns_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_resource_leaks', uri: 'ukma.pro.security_check_resource_leaks.v1', mcp: 'ukma_pro_security_check_resource_leaks_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_misconfig', uri: 'ukma.pro.security_check_misconfig.v1', mcp: 'ukma_pro_security_check_misconfig_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_input_validation', uri: 'ukma.pro.security_check_input_validation.v1', mcp: 'ukma_pro_security_check_input_validation_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_error_exposure', uri: 'ukma.pro.security_check_error_exposure.v1', mcp: 'ukma_pro_security_check_error_exposure_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_scan_iac', uri: 'ukma.pro.security_scan_iac.v1', mcp: 'ukma_pro_security_scan_iac_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_licenses', uri: 'ukma.pro.security_check_licenses.v1', mcp: 'ukma_pro_security_check_licenses_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_secrets_entropy', uri: 'ukma.pro.security_check_secrets_entropy.v1', mcp: 'ukma_pro_security_check_secrets_entropy_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_detect_injection', uri: 'ukma.pro.security_detect_injection.v1', mcp: 'ukma_pro_security_detect_injection_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_search_path', uri: 'ukma.pro.security_check_search_path.v1', mcp: 'ukma_pro_security_check_search_path_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_check_crypto', uri: 'ukma.pro.security_check_crypto.v1', mcp: 'ukma_pro_security_check_crypto_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_export_sarif', uri: 'ukma.pro.security_export_sarif.v1', mcp: 'ukma_pro_security_export_sarif_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_orchestrated_scan', uri: 'ukma.pro.security_orchestrated_scan.v1', mcp: 'ukma_pro_security_orchestrated_scan_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'pro.security_codeql_warmup', uri: 'ukma.pro.security_codeql_warmup.v1', mcp: 'ukma_pro_security_codeql_warmup_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_implementors', uri: 'ukma.code.find_implementors.v1', mcp: 'ukma_code_find_implementors_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.traverse', uri: 'ukma.code.traverse.v1', mcp: 'ukma_code_traverse_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.get_detailed_symbol', uri: 'ukma.code.get_detailed_symbol.v1', mcp: 'ukma_code_get_detailed_symbol_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.index_directory', uri: 'ukma.code.index_directory.v1', mcp: 'ukma_code_index_directory_v1', scopes: ['code:index'], idempotency: 'required' },
  { id: 'code.export_graph', uri: 'ukma.code.export_graph.v1', mcp: 'ukma_code_export_graph_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'code.find_importers', uri: 'ukma.code.find_importers.v1', mcp: 'ukma_code_find_importers_v1', scopes: ['code:read'], idempotency: 'not_required' },
  { id: 'document.remove_source', uri: 'ukma.document.remove_source.v1', mcp: 'ukma_document_remove_source_v1', scopes: ['document:index_admin'], idempotency: 'required' },
  { id: 'memory.context', uri: 'ukma.memory.context.v1', mcp: 'ukma_memory_context_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.list', uri: 'ukma.memory.list.v1', mcp: 'ukma_memory_list_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'memory.stats', uri: 'ukma.memory.stats.v1', mcp: 'ukma_memory_stats_v1', scopes: ['memory:read'], idempotency: 'not_required' },
  { id: 'system.metrics', uri: 'ukma.system.metrics.v1', mcp: 'ukma_system_metrics_v1', scopes: ['system:read'], idempotency: 'not_required' },
];
