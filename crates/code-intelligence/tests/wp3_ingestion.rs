//! Ingestion pipeline tests (`P3-WP3`): `Git Rev → File Discovery → Parse →
//! Resolve → Graph Commit → Index Publish`, the job FSM (§32.2) and the
//! generation pointer.

use std::sync::Arc;

use code_intelligence::domain::{
    AnalyzeRepository, GetJob, PageRequest, RepoRevision, SourceFile, SymbolQuery,
};
use code_intelligence::parsers::rust::RustParser;
use code_intelligence::service::{
    CodeIntelligenceService, InMemorySources, SCOPE_INDEX, SCOPE_INDEX_ADMIN, SCOPE_READ,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const REPO: &str = "repo-1";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PROFILE: &str = "tree-sitter-rust-0.24.2/ts-0.25.10";

const LIB_RS: &str = "\
mod core {
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}

fn main() {
    let _ = core::add(1, 2);
}
";

fn ctx() -> RequestContext {
    RequestContext {
        request_id: "req-1".to_string(),
        trace_id: "trace-1".to_string(),
        scope: ScopeRef {
            tenant_id: "tenant-1".to_string(),
            project_id: "project-1".to_string(),
            repo_id: Some(REPO.to_string()),
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: "agent-1".to_string(),
        scopes: vec![
            SCOPE_READ.to_string(),
            SCOPE_INDEX.to_string(),
            SCOPE_INDEX_ADMIN.to_string(),
        ],
        deadline_at: None,
        consistency: Consistency::Eventual,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

fn sources() -> InMemorySources {
    InMemorySources::new()
        .with_commit(
            COMMIT_A,
            vec![
                SourceFile::new("src/lib.rs", LIB_RS),
                SourceFile::new("docs/notes.py", "def notes():\n    pass\n"),
            ],
        )
        .with_commit(COMMIT_B, vec![SourceFile::new("src/lib.rs", LIB_RS)])
}

fn service() -> CodeIntelligenceService {
    CodeIntelligenceService::new(Arc::new(RustParser::new()), Arc::new(sources()))
}

fn request(commit: &str) -> AnalyzeRepository {
    AnalyzeRepository {
        revision: RepoRevision::git(REPO, commit),
        parser_profile: PROFILE.to_string(),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        asynchronous: true,
    }
}

#[test]
fn submit_parks_the_job_and_run_publishes_it() {
    let svc = service();
    let ctx = ctx();
    let handle = svc.submit_analysis(&ctx, &request(COMMIT_A)).unwrap();
    assert_eq!(handle.stage, "queued");

    let before = svc
        .get_analysis_job(
            &ctx,
            &GetJob {
                job_id: handle.job_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(before.stage, "queued");
    assert!(!before.canonical_committed);
    assert!(
        before.result_ref.is_none(),
        "no manifest before the commit point"
    );

    let after = svc.run_analysis(&ctx, &handle.job_id).unwrap();
    assert_eq!(after.stage, "ready");
    assert!(after.canonical_committed);
    assert_eq!(
        after.result_ref.as_deref(),
        Some(format!("tenant-1/project-1/{REPO}/{COMMIT_A}").as_str())
    );

    // running twice is a no-op, never a second commit
    let again = svc.run_analysis(&ctx, &handle.job_id).unwrap();
    assert_eq!(again.stage, "ready");
}

#[test]
fn unparseable_files_are_skipped_and_make_the_manifest_partial() {
    let svc = service();
    let ctx = ctx();
    let handle = svc.analyze_repository(&ctx, &request(COMMIT_A)).unwrap();
    let manifest = svc.analysis_manifest(&ctx, &handle.job_id).unwrap();
    assert_eq!(manifest.indexed_files, 1);
    assert_eq!(manifest.skipped_files, 1);
    assert_eq!(
        manifest.completeness(),
        "partial",
        "a skipped file must never be reported as complete"
    );
    assert!(manifest
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "CAPABILITY_UNAVAILABLE"));
}

#[test]
fn include_and_exclude_globs_control_discovery() {
    let svc = service();
    let ctx = ctx();
    let mut narrowed = request(COMMIT_A);
    narrowed.include_globs = vec!["src/**".to_string()];
    let handle = svc.analyze_repository(&ctx, &narrowed).unwrap();
    let manifest = svc.analysis_manifest(&ctx, &handle.job_id).unwrap();
    assert_eq!(manifest.indexed_files, 1);
    assert_eq!(manifest.skipped_files, 0, "excluded files are not skipped");
    assert_eq!(manifest.completeness(), "complete");

    let excluded_everything = AnalyzeRepository {
        exclude_globs: vec!["**".to_string()],
        ..request(COMMIT_B)
    };
    assert_eq!(
        svc.analyze_repository(&ctx, &excluded_everything)
            .unwrap_err()
            .code(),
        "CAPABILITY_UNAVAILABLE",
        "an empty revision is not an analysis"
    );
}

#[test]
fn each_revision_gets_its_own_generation_and_symbol_ids() {
    let svc = service();
    let ctx = ctx();
    let first = svc.analyze_repository(&ctx, &request(COMMIT_A)).unwrap();
    let second = svc.analyze_repository(&ctx, &request(COMMIT_B)).unwrap();

    let manifest_a = svc.analysis_manifest(&ctx, &first.job_id).unwrap();
    let manifest_b = svc.analysis_manifest(&ctx, &second.job_id).unwrap();
    assert_ne!(
        manifest_a.graph_generation, manifest_b.graph_generation,
        "each published index needs its own generation"
    );

    let symbols = |revision: &str| {
        svc.find_symbols(
            &ctx,
            &SymbolQuery {
                repository_id: REPO.to_string(),
                revision: revision.to_string(),
                query: "add".to_string(),
                kinds: Vec::new(),
                path_glob: None,
                language: None,
                page: PageRequest::new(10),
            },
        )
        .unwrap()
    };
    let id_a = symbols(COMMIT_A).items[0].id.clone();
    let id_b = symbols(COMMIT_B).items[0].id.clone();
    assert_ne!(id_a, id_b, "ids are version scoped (§32.3)");
    assert!(id_a.starts_with("sym:v1:") && id_b.starts_with("sym:v1:"));
}

#[test]
fn required_generation_is_enforced_for_strict_readers() {
    let svc = service();
    let ctx = ctx();
    let handle = svc.analyze_repository(&ctx, &request(COMMIT_A)).unwrap();
    let generation = svc
        .analysis_manifest(&ctx, &handle.job_id)
        .unwrap()
        .graph_generation;

    let mut strict = ctx.clone();
    strict.consistency = Consistency::StrictGeneration;
    strict.required_generation = Some(generation.clone());
    assert!(svc
        .find_symbols(
            &strict,
            &SymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                query: "add".to_string(),
                kinds: Vec::new(),
                path_glob: None,
                language: None,
                page: PageRequest::new(10),
            }
        )
        .is_ok());

    strict.required_generation = Some("gen-does-not-exist".to_string());
    assert_eq!(
        svc.find_symbols(
            &strict,
            &SymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                query: "add".to_string(),
                kinds: Vec::new(),
                path_glob: None,
                language: None,
                page: PageRequest::new(10),
            }
        )
        .unwrap_err()
        .code(),
        "INDEX_STALE"
    );
}

#[test]
fn symbol_ids_are_stable_across_service_instances() {
    let first = service();
    let second = service();
    let ctx = ctx();
    let job = first.analyze_repository(&ctx, &request(COMMIT_A)).unwrap();
    let other_job = second.analyze_repository(&ctx, &request(COMMIT_A)).unwrap();

    let id = |svc: &CodeIntelligenceService, job_id: &str| {
        let manifest = svc.analysis_manifest(&ctx, job_id).unwrap();
        assert_eq!(manifest.commit_sha, COMMIT_A);
        svc.find_symbols(
            &ctx,
            &SymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                query: "add".to_string(),
                kinds: Vec::new(),
                path_glob: None,
                language: None,
                page: PageRequest::new(10),
            },
        )
        .unwrap()
        .items[0]
            .id
            .clone()
    };
    assert_eq!(id(&first, &job.job_id), id(&second, &other_job.job_id));
}
