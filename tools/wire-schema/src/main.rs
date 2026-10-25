//! Emits the wire payload JSON Schemas for the four domain crates.
//!
//! The schemas in `contracts/schema/v1/wire.schema.json` are *generated* from
//! the Rust request/response types that the MCP gateway actually decodes and
//! encodes, so the published contract cannot drift from the implementation
//! (ADR-058). Run `cargo run -p ukma-wire-schema` to regenerate and
//! `cargo run -p ukma-wire-schema -- --check` to verify.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use schemars::{schema_for, JsonSchema};
use serde_json::{json, Map, Value};

use code_intelligence as ci;
use document_intelligence as di;
use memory_domain as mem;
use traceability_engine as tr;

/// Registry path, relative to the crate directory (used by the tests).
#[cfg(test)]
const REGISTRY: &str = "../../contracts/api/operation-registry.yaml";
/// Output path, relative to the crate directory when run through `cargo run`.
const OUTPUT: &str = "contracts/schema/v1/wire.schema.json";

macro_rules! wire_table {
    ($m:ident) => {
        $m! {
        ("code.analyze_complexity", "request", ci::domain::ComplexityQuery),
        ("code.analyze_complexity", "response", Vec<ci::domain::ComplexityReport>),
        ("code.analyze_impact", "request", ci::domain::ImpactQuery),
        ("code.analyze_impact", "response", ci::domain::ImpactReport),
        ("code.analyze_repository", "request", ci::domain::AnalyzeRepository),
        ("code.analyze_repository", "response", ci::domain::JobHandle),
        ("code.find_by_signature", "request", ci::domain::SignatureQuery),
        ("code.find_by_signature", "response", ci::domain::Page<ci::Symbol>),
        ("code.find_circular_deps", "request", ci::domain::CircularDepsQuery),
        ("code.find_circular_deps", "response", ci::domain::Page<ci::domain::CircularDependency>),
        ("code.find_dead_imports", "request", ci::domain::DeadImportsQuery),
        ("code.find_dead_imports", "response", ci::domain::Page<ci::domain::DeadImport>),
        ("code.find_entry_points", "request", ci::domain::EntryPointsQuery),
        ("code.find_entry_points", "response", ci::domain::Page<ci::domain::EntryPoint>),
        ("code.find_hot_paths", "request", ci::domain::HotPathsQuery),
        ("code.find_hot_paths", "response", Vec<ci::domain::HotPath>),
        ("code.find_related_tests", "request", ci::domain::RelatedTestsQuery),
        ("code.find_related_tests", "response", ci::domain::Page<ci::domain::RelatedTest>),
        ("code.get_ai_context", "request", ci::domain::AiContextQuery),
        ("code.get_ai_context", "response", ci::domain::AiContextResult),
        ("code.get_curated_context", "request", ci::domain::CuratedContextQuery),
        ("code.get_curated_context", "response", ci::domain::CuratedContext),
        ("code.get_module_summary", "request", ci::domain::ModuleSummaryQuery),
        ("code.get_module_summary", "response", ci::domain::Page<ci::domain::ModuleSummary>),
        ("code.index_files", "request", ci::domain::IndexFiles),
        ("code.index_files", "response", ci::domain::IndexedRevision),
        ("code.pr_context", "request", ci::domain::PrContextQuery),
        ("code.pr_context", "response", ci::domain::PrContextReport),
        ("code.search_by_error", "request", ci::domain::ErrorSearchQuery),
        ("code.search_by_error", "response", ci::domain::Page<ci::domain::ErrorMatch>),
        ("code.search_by_pattern", "request", ci::domain::PatternSearchQuery),
        ("code.search_by_pattern", "response", ci::domain::Page<ci::domain::PatternMatch>),
        ("code.cancel_analysis_job", "request", ci::domain::CancelJob),
        ("code.cancel_analysis_job", "response", ci::domain::JobView),
        ("code.compare_revisions", "request", ci::domain::CodeRevisionDiff),
        ("code.compare_revisions", "response", ci::domain::Page<ci::domain::CodeChange>),
        ("code.find_references", "request", ci::domain::ReferenceQuery),
        ("code.find_references", "response", ci::domain::Page<ci::domain::ReferenceLocation>),
        ("code.find_symbols", "request", ci::domain::SymbolQuery),
        ("code.find_symbols", "response", ci::domain::Page<ci::Symbol>),
        ("code.get_analysis_job", "request", ci::domain::GetJob),
        ("code.get_analysis_job", "response", ci::domain::JobView),
        ("code.get_edit_context", "request", ci::domain::EditContextQuery),
        ("code.get_edit_context", "response", ci::domain::EditContextResult),
        ("code.get_symbol", "request", ci::domain::GetSymbol),
        ("code.get_symbol", "response", ci::Symbol),
        ("code.invalidate_revision", "request", ci::domain::InvalidateCodeRevision),
        ("code.invalidate_revision", "response", ci::domain::ChangeReceipt),
        ("code.query_callees", "request", ci::domain::TraversalQuery),
        ("code.query_callees", "response", ci::domain::Page<ci::CodeEdge>),
        ("code.query_callers", "request", ci::domain::TraversalQuery),
        ("code.query_callers", "response", ci::domain::Page<ci::CodeEdge>),
        ("code.query_dependencies", "request", ci::domain::TraversalQuery),
        ("code.query_dependencies", "response", ci::domain::Page<ci::CodeEdge>),
        ("document.cancel_ingest_job", "request", di::domain::CancelJob),
        ("document.cancel_ingest_job", "response", di::domain::JobView),
        ("document.compare_revisions", "request", di::domain::DocumentRevisionDiff),
        ("document.compare_revisions", "response", di::domain::DocDiff),
        ("document.extract_relations", "request", di::domain::ExtractRelations),
        ("document.extract_relations", "response", di::domain::Page<di::domain::ExtractedRelation>),
        ("document.extract_requirements", "request", di::domain::ExtractRequirements),
        ("document.extract_requirements", "response", di::domain::Page<di::domain::RequirementCandidate>),
        ("document.get_document", "request", di::domain::GetDocument),
        ("document.get_document", "response", di::domain::DocumentView),
        ("document.get_ingest_job", "request", di::domain::GetJob),
        ("document.get_ingest_job", "response", di::domain::JobView),
        ("document.get_source_span", "request", di::domain::GetSourceSpan),
        ("document.get_source_span", "response", di::domain::SourceSpanView),
        ("document.generate_architecture_doc", "request", di::domain::GenerateArchitectureDoc),
        ("document.generate_architecture_doc", "response", di::domain::ArchitectureDoc),
        ("document.ingest", "request", di::domain::DocumentIngest),
        ("document.ingest", "response", di::domain::JobHandle),
        ("document.invalidate_revision", "request", di::domain::InvalidateDocumentRevision),
        ("document.invalidate_revision", "response", di::domain::ChangeReceipt),
        ("document.list_doc_sources", "request", di::domain::ListDocSources),
        ("document.list_doc_sources", "response", di::domain::Page<di::domain::DocSourceSummary>),
        ("document.list_requirements", "request", di::domain::ListRequirements),
        ("document.list_requirements", "response", di::domain::Page<di::domain::RequirementCandidate>),
        ("document.parse_preview", "request", di::domain::ParsePreview),
        ("document.parse_preview", "response", di::domain::ParsedDocument),
        ("document.review_requirement", "request", di::domain::ReviewRequirement),
        ("document.review_requirement", "response", di::domain::RequirementReviewResult),
        ("document.search", "request", di::domain::SearchDocument),
        ("document.search", "response", di::domain::Page<di::domain::SearchHit>),
        ("document.verify_consistency", "request", di::domain::VerifyConsistency),
        ("document.verify_consistency", "response", di::domain::ConsistencyReport),
        ("memory.begin_reconsolidation", "request", mem::domain::BeginReconsolidation),
        ("memory.begin_reconsolidation", "response", mem::domain::ReconsolidationWindow),
        ("memory.close_agent_scope", "request", mem::domain::CloseAgentScope),
        ("memory.close_agent_scope", "response", mem::domain::CloseScopeResult),
        ("memory.commit_reconsolidation", "request", mem::domain::CommitReconsolidation),
        ("memory.commit_reconsolidation", "response", mem::domain::ChangeReceipt),
        ("memory.consolidate", "request", mem::domain::Consolidate),
        ("memory.consolidate", "response", mem::domain::JobHandle),
        ("memory.evaluate_eviction", "request", mem::domain::EvaluateEviction),
        ("memory.evaluate_eviction", "response", mem::domain::EvictionPlan),
        ("memory.find_conflicts", "request", mem::domain::FindConflicts),
        ("memory.find_conflicts", "response", mem::domain::Page<mem::domain::MemoryView>),
        ("memory.fold", "request", mem::domain::FoldMemory),
        ("memory.fold", "response", mem::domain::FoldedResult),
        ("memory.forget", "request", mem::domain::ForgetMemory),
        ("memory.forget", "response", mem::domain::DeletionReceipt),
        ("memory.get_budget", "request", mem::domain::GetMemoryBudget),
        ("memory.get_budget", "response", mem::domain::BudgetView),
        ("memory.get_consolidation_job", "request", mem::domain::GetMemoryJob),
        ("memory.get_consolidation_job", "response", mem::domain::JobView),
        ("memory.list_working_blocks", "request", mem::domain::ListWorkingBlocks),
        ("memory.list_working_blocks", "response", mem::domain::Page<mem::domain::MemoryView>),
        ("memory.open_agent_scope", "request", mem::domain::OpenAgentScope),
        ("memory.open_agent_scope", "response", mem::domain::AgentScopeView),
        ("memory.page_in", "request", mem::domain::PageIn),
        ("memory.page_in", "response", mem::domain::PageInResult),
        ("memory.page_out", "request", mem::domain::PageOut),
        ("memory.page_out", "response", mem::domain::PagingReceipt),
        ("memory.propose_fact", "request", mem::domain::ProposeFact),
        ("memory.propose_fact", "response", mem::domain::Ref),
        ("memory.put_working_block", "request", mem::domain::PutWorkingBlock),
        ("memory.put_working_block", "response", mem::domain::Ref),
        ("memory.record_procedure_outcome", "request", mem::domain::RecordProcedureOutcome),
        ("memory.record_procedure_outcome", "response", mem::domain::ChangeReceipt),
        ("memory.register_procedure_candidate", "request", mem::domain::RegisterProcedure),
        ("memory.register_procedure_candidate", "response", mem::domain::ProcedureView),
        ("memory.remove_working_block", "request", mem::domain::RemoveWorkingBlock),
        ("memory.remove_working_block", "response", mem::domain::DeletionReceipt),
        ("memory.retrieve", "request", mem::domain::RetrieveMemory),
        ("memory.retrieve", "response", mem::domain::MemoryView),
        ("memory.review_procedure", "request", mem::domain::ReviewProcedure),
        ("memory.review_procedure", "response", mem::domain::ProcedureView),
        ("memory.search", "request", mem::domain::SearchMemory),
        ("memory.search", "response", mem::domain::Page<mem::domain::MemoryView>),
        ("memory.store_episode", "request", mem::domain::StoreEpisode),
        ("memory.store_episode", "response", mem::domain::ChangeReceipt),
        ("memory.update_revision", "request", mem::domain::UpdateMemory),
        ("memory.update_revision", "response", mem::domain::ChangeReceipt),
        ("memory.verify_fact", "request", mem::domain::VerifyFact),
        ("memory.verify_fact", "response", mem::domain::ChangeReceipt),
        ("traceability.accept_link", "request", tr::domain::AcceptLink),
        ("traceability.accept_link", "response", tr::domain::TraceLink),
        ("traceability.analyze_change_impact", "request", tr::domain::TraceImpactQuery),
        ("traceability.analyze_change_impact", "response", tr::domain::TraceImpact),
        ("traceability.compare_revisions", "request", tr::domain::TraceRevisionDiff),
        ("traceability.compare_revisions", "response", tr::domain::TraceRevisionDiffResult),
        ("traceability.explain_with_evidence", "request", tr::domain::ExplainEvidence),
        ("traceability.explain_with_evidence", "response", tr::domain::EvidenceReport),
        ("traceability.find_design_gaps", "request", tr::domain::GapQuery),
        ("traceability.find_design_gaps", "response", tr::domain::GapReport),
        ("traceability.get_link", "request", tr::domain::GetLink),
        ("traceability.get_link", "response", tr::domain::TraceLink),
        ("traceability.get_revalidation_job", "request", tr::domain::GetTraceJob),
        ("traceability.get_revalidation_job", "response", tr::domain::JobView),
        ("traceability.list_links", "request", tr::domain::ListLinks),
        ("traceability.list_links", "response", tr::domain::Page<tr::domain::TraceLink>),
        ("traceability.propose_link", "request", tr::domain::ProposeLink),
        ("traceability.propose_link", "response", tr::domain::TraceLink),
        ("traceability.query_coverage", "request", tr::domain::CoverageQuery),
        ("traceability.query_coverage", "response", tr::domain::CoverageReport),
        ("traceability.revalidate_stale", "request", tr::domain::RevalidateStale),
        ("traceability.revalidate_stale", "response", tr::domain::JobHandle),
        ("traceability.revoke_link", "request", tr::domain::RevokeLink),
        ("traceability.revoke_link", "response", tr::domain::TraceLink),
        ("traceability.validate_link", "request", tr::domain::ValidateLink),
        ("traceability.validate_link", "response", tr::domain::TraceLink),
        // CodeGraph Pro (27 analyzers; 10 code + 17 security).
        ("pro.analyze_coupling", "request", ci::pro::ProRevisionRequest),
        ("pro.analyze_coupling", "response", ci::pro::ProReport),
        ("pro.find_unused_code", "request", ci::pro::ProRevisionRequest),
        ("pro.find_unused_code", "response", ci::pro::ProReport),
        ("pro.find_duplicates", "request", ci::pro::ProRevisionRequest),
        ("pro.find_duplicates", "response", ci::pro::ProReport),
        ("pro.find_similar", "request", ci::pro::ProRevisionRequest),
        ("pro.find_similar", "response", ci::pro::ProReport),
        ("pro.cluster_symbols", "request", ci::pro::ProRevisionRequest),
        ("pro.cluster_symbols", "response", ci::pro::ProReport),
        ("pro.compare_symbols", "request", ci::pro::ProCompareSymbolsRequest),
        ("pro.compare_symbols", "response", ci::pro::ProReport),
        ("pro.cross_project_search", "request", ci::pro::ProQueryRequest),
        ("pro.cross_project_search", "response", ci::pro::ProReport),
        ("pro.mine_git_history", "request", ci::pro::ProRevisionRequest),
        ("pro.mine_git_history", "response", ci::pro::ProReport),
        ("pro.mine_git_history_for_file", "request", ci::pro::ProFileRequest),
        ("pro.mine_git_history_for_file", "response", ci::pro::ProReport),
        ("pro.search_git_history", "request", ci::pro::ProQueryRequest),
        ("pro.search_git_history", "response", ci::pro::ProReport),
        ("pro.scan_security", "request", ci::pro::ProRevisionRequest),
        ("pro.scan_security", "response", ci::pro::ProReport),
        ("pro.security_control_flow", "request", ci::pro::ProRevisionRequest),
        ("pro.security_control_flow", "response", ci::pro::ProReport),
        ("pro.security_trace_data_flow", "request", ci::pro::ProRevisionRequest),
        ("pro.security_trace_data_flow", "response", ci::pro::ProReport),
        ("pro.security_generate_sbom", "request", ci::pro::ProRevisionRequest),
        ("pro.security_generate_sbom", "response", ci::pro::ProReport),
        ("pro.security_audit_deps", "request", ci::pro::ProRevisionRequest),
        ("pro.security_audit_deps", "response", ci::pro::ProReport),
        ("pro.security_check_unchecked_returns", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_unchecked_returns", "response", ci::pro::ProReport),
        ("pro.security_check_resource_leaks", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_resource_leaks", "response", ci::pro::ProReport),
        ("pro.security_check_misconfig", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_misconfig", "response", ci::pro::ProReport),
        ("pro.security_check_input_validation", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_input_validation", "response", ci::pro::ProReport),
        ("pro.security_check_error_exposure", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_error_exposure", "response", ci::pro::ProReport),
        ("pro.security_scan_iac", "request", ci::pro::ProRevisionRequest),
        ("pro.security_scan_iac", "response", ci::pro::ProReport),
        ("pro.security_check_licenses", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_licenses", "response", ci::pro::ProReport),
        ("pro.security_check_secrets_entropy", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_secrets_entropy", "response", ci::pro::ProReport),
        ("pro.security_detect_injection", "request", ci::pro::ProRevisionRequest),
        ("pro.security_detect_injection", "response", ci::pro::ProReport),
        ("pro.security_check_search_path", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_search_path", "response", ci::pro::ProReport),
        ("pro.security_check_crypto", "request", ci::pro::ProRevisionRequest),
        ("pro.security_check_crypto", "response", ci::pro::ProReport),
        ("pro.security_export_sarif", "request", ci::pro::ProRevisionRequest),
        ("pro.security_export_sarif", "response", ci::pro::ProReport),
        }
    };
}

macro_rules! collect {
    ($(($op:literal, $role:literal, $ty:ty)),* $(,)?) => {
        /// Every `(operation, role, type)` triple the gateway serves.
        fn wire_types() -> Vec<(&'static str, &'static str, String, Value)> {
            vec![
                $((
                    $op,
                    $role,
                    <$ty as JsonSchema>::schema_name().to_string(),
                    serde_json::to_value(schema_for!($ty)).expect("wire schema serialises"),
                )),*
            ]
        }
    };
}

wire_table!(collect);

fn rewrite_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get("$ref") {
                if let Some(rest) = text.strip_prefix("#/definitions/") {
                    map.insert("$ref".to_string(), Value::String(format!("#/$defs/{rest}")));
                }
            }
            for child in map.values_mut() {
                rewrite_refs(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

/// Builds the generated schema document.
///
/// # Errors
/// Returns an error when two distinct Rust types claim the same definition
/// name, which would silently overwrite one of the two schemas.
pub fn build() -> Result<Value, String> {
    let mut defs: BTreeMap<String, Value> = BTreeMap::new();
    let mut operations = Map::new();
    for (operation, role, name, mut root) in wire_types() {
        let definitions = root
            .get("definitions")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(object) = root.as_object_mut() {
            object.remove("definitions");
            object.remove("$schema");
        }
        let mut candidates: Vec<(String, Value)> = definitions.into_iter().collect();
        candidates.push((name.clone(), root));
        for (def_name, mut schema) in candidates {
            rewrite_refs(&mut schema);
            if let Some(object) = schema.as_object_mut() {
                // `title` is present on nested definitions but not on roots; the
                // key already carries the type name, so drop it for a stable shape.
                object.remove("title");
            }
            if let Some(existing) = defs.get(&def_name) {
                if existing != &schema {
                    return Err(format!(
                        "definition `{def_name}` is claimed by two different Rust types\n  existing: {}\n  incoming: {}",
                        serde_json::to_string(existing).unwrap(),
                        serde_json::to_string(&schema).unwrap(),
                    ));
                }
            }
            defs.insert(def_name, schema);
        }
        let entry = operations
            .entry(operation.to_string())
            .or_insert_with(|| json!({}));
        entry[role] = Value::String(name);
    }
    Ok(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://schemas.ukma.dev/v1/wire.schema.json",
        "title": "UKMA v1 wire payload types",
        "$comment": "Generated from the Rust domain types by tools/wire-schema. Do not edit by hand.",
        "$defs": defs,
        "x-operations": operations,
    }))
}

fn main() -> ExitCode {
    let value = match build() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let path = Path::new(OUTPUT);
    let check = std::env::args().any(|arg| arg == "--check");
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&value).expect("schema renders")
    );
    if check {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        let same = serde_json::from_str::<Value>(&existing).is_ok_and(|parsed| parsed == value);
        if same {
            println!("wire schema up to date ({path:?})");
            ExitCode::SUCCESS
        } else {
            eprintln!("wire schema drifted; run `cargo run -p ukma-wire-schema`");
            ExitCode::FAILURE
        }
    } else {
        match std::fs::write(path, rendered) {
            Ok(()) => {
                println!("wrote {path:?}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("cannot write {path:?}: {error}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn registry_ids() -> Vec<String> {
        let raw = std::fs::read_to_string(REGISTRY).expect("operation registry is readable");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&raw).expect("registry parses");
        parsed
            .get("operations")
            .and_then(|ops| ops.as_sequence())
            .expect("registry has operations")
            .iter()
            .filter_map(|op| op.get("operation_id").and_then(|id| id.as_str()))
            .map(str::to_string)
            .filter(|id| !id.starts_with("system."))
            .collect()
    }

    #[test]
    fn checked_in_schema_matches_the_rust_types() {
        let generated = build().expect("wire schema builds");
        let raw = std::fs::read_to_string("../../contracts/schema/v1/wire.schema.json")
            .expect("wire schema is checked in");
        let checked_in: Value = serde_json::from_str(&raw).expect("wire schema parses");
        assert_eq!(
            generated, checked_in,
            "contracts/schema/v1/wire.schema.json drifted; run `cargo run -p ukma-wire-schema`"
        );
    }

    #[test]
    fn every_domain_operation_has_both_wire_types() {
        let document = build().expect("wire schema builds");
        let operations = document
            .get("x-operations")
            .and_then(Value::as_object)
            .expect("x-operations present");
        let ids = registry_ids();
        assert_eq!(ids.len(), 108, "registry carries 108 domain operations");
        for id in ids {
            let entry = operations
                .get(&id)
                .unwrap_or_else(|| panic!("{id} has no wire types"));
            for role in ["request", "response"] {
                let name = entry
                    .get(role)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{id}: missing {role}"));
                assert!(
                    document["$defs"].get(name).is_some(),
                    "{id}: {role} type `{name}` is not in $defs"
                );
            }
        }
    }

    #[test]
    fn generated_schemas_carry_no_placeholders() {
        let rendered = serde_json::to_string(&build().expect("wire schema builds")).unwrap();
        for needle in ["UNRESOLVED", "NOT CLOSED", "PROVISIONAL"] {
            assert!(
                !rendered.contains(needle),
                "generated schema still says {needle}"
            );
        }
        assert!(
            !rendered.contains("#/definitions/"),
            "unrewritten definition ref"
        );
    }

    fn collect_refs(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::String(reference)) = map.get("$ref") {
                    if let Some(name) = reference.strip_prefix("#/$defs/") {
                        out.insert(name.to_string());
                    }
                }
                for child in map.values() {
                    collect_refs(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
            _ => {}
        }
    }

    #[test]
    fn every_definition_is_reachable_from_an_operation() {
        let document = build().expect("wire schema builds");
        let defs = document["$defs"].as_object().expect("$defs present");
        let mut pending: Vec<String> = Vec::new();
        for entry in document["x-operations"]
            .as_object()
            .expect("x-operations present")
            .values()
        {
            for role in ["request", "response"] {
                if let Some(name) = entry.get(role).and_then(Value::as_str) {
                    pending.push(name.to_string());
                }
            }
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(schema) = defs.get(&name) {
                let mut refs = BTreeSet::new();
                collect_refs(schema, &mut refs);
                pending.extend(refs);
            }
        }
        for name in defs.keys() {
            assert!(seen.contains(name), "definition `{name}` is unreachable");
        }
    }
}
