//! Cross-domain test harness (P9).
//!
//! Everything here drives the **public API**: tests build an envelope, dispatch it
//! through the gateway and read the response envelope back. Nothing reaches into a
//! domain service directly except the two destinations that have no wire operation
//! (storage ports and the graph query facade), which is why those scenarios are
//! recorded as adapter-level E2E.
//!
//! The harness deliberately owns no policy: scopes come from the registry, the
//! gateway decides authorization, and the domain services decide semantics.

use std::sync::Arc;

use serde_json::{json, Value};
use ukma_contracts::generated::OPERATIONS;
use ukma_contracts::{
    Consistency, RequestContext, RequestEnvelope, ResponseEnvelope, ResponseStatus, ScopeRef,
};
use ukma_mcp::gateway::StaticScopes;
use ukma_mcp::{build, Gateway, JobIndex, Services};

use code_intelligence::domain::SourceFile;
use code_intelligence::parsers::engine::UnifiedParser;
use code_intelligence::pro::ProService;
use code_intelligence::service::{CodeIntelligenceService, InMemorySources};
use document_intelligence::parsers::DocumentParsers;
use document_intelligence::service::{DocumentService, InMemoryDocuments};
use memory_domain::service::{FixedClock, MemoryService};
use traceability_engine::service::TraceabilityService;

/// Default tenant used by the scenarios.
pub const TENANT: &str = "tenant-a";
/// Default project used by the scenarios.
pub const PROJECT: &str = "project-b";
/// Principal the gateway authenticates as.
pub const PRINCIPAL: &str = "principal";
/// Envelope version spoken by the scenarios.
pub const API_VERSION: &str = "1.2.3";

/// A gateway plus the services behind it.
pub struct Env {
    /// The dispatching gateway.
    pub gateway: Gateway,
    /// Domain services (needed only by the storage-level scenarios).
    pub services: Services,
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Every scope the registry mentions plus the domain-internal admin scopes the
/// seams need, sorted and de-duplicated.
pub fn all_scopes() -> Vec<String> {
    let mut scopes: Vec<String> = Vec::new();
    for spec in OPERATIONS {
        for scope in spec.required_scopes {
            if !scopes.iter().any(|existing| existing == scope) {
                scopes.push((*scope).to_string());
            }
        }
    }
    for extra in [
        // seams that seed authoritative heads / endpoints (ADR-053 D-13)
        "trace:admin",
        "code:index_admin",
        "document:index_admin",
    ] {
        if !scopes.iter().any(|existing| existing == extra) {
            scopes.push(extra.to_string());
        }
    }
    scopes.sort();
    scopes
}

fn grant(gateway: Gateway, scopes: &[String]) -> Gateway {
    let borrowed: Vec<&str> = scopes.iter().map(String::as_str).collect();
    gateway.with_scope_resolver(Arc::new(StaticScopes::new().grant(PRINCIPAL, &borrowed)))
}

fn services_with(code: CodeIntelligenceService, document: DocumentService) -> Services {
    Services {
        code: Arc::new(code),
        document: Arc::new(document),
        memory: Arc::new(MemoryService::new()),
        trace: Arc::new(TraceabilityService::new()),
        jobs: JobIndex::new(),
        pro: Arc::new(ProService::new()),
    }
}

fn empty_code() -> CodeIntelligenceService {
    CodeIntelligenceService::new(
        Arc::new(UnifiedParser::new()),
        Arc::new(InMemorySources::new()),
    )
}

fn empty_documents() -> DocumentService {
    DocumentService::new(
        Arc::new(DocumentParsers::new()),
        Arc::new(InMemoryDocuments::new()),
    )
}

fn env_of(services: Services) -> Env {
    let gateway = grant(build(&services).expect("handlers register"), &all_scopes());
    Env { gateway, services }
}

/// Environment with the in-memory defaults and full authority.
pub fn env() -> Env {
    env_of(services_with(empty_code(), empty_documents()))
}

/// Environment whose code service resolves `commit` to `files`.
pub fn env_with_code(commit: &str, files: Vec<SourceFile>) -> Env {
    let code = CodeIntelligenceService::new(
        Arc::new(UnifiedParser::new()),
        Arc::new(InMemorySources::new().with_commit(commit, files)),
    );
    env_of(services_with(code, empty_documents()))
}

/// Environment whose document service holds `artifacts`, plus their hashes.
pub fn env_with_documents(artifacts: Vec<(&str, Vec<u8>)>) -> (Env, Vec<String>) {
    let mut documents = InMemoryDocuments::new();
    let hashes: Vec<String> = artifacts
        .iter()
        .map(|(reference, bytes)| documents.put(reference, bytes))
        .collect();
    let document = DocumentService::new(Arc::new(DocumentParsers::new()), Arc::new(documents));
    (env_of(services_with(empty_code(), document)), hashes)
}

/// Environment with code sources, document artifacts and a memory service on
/// `clock` (the retention-isolation scenarios, DB-005/SR-001).
pub fn env_with_clock_and_sources(
    clock: Arc<FixedClock>,
    commit: &str,
    files: Vec<SourceFile>,
    artifacts: Vec<(&str, Vec<u8>)>,
) -> (Env, Vec<String>) {
    let code = CodeIntelligenceService::new(
        Arc::new(UnifiedParser::new()),
        Arc::new(InMemorySources::new().with_commit(commit, files)),
    );
    let mut documents = InMemoryDocuments::new();
    let hashes: Vec<String> = artifacts
        .iter()
        .map(|(reference, bytes)| documents.put(reference, bytes))
        .collect();
    let document = DocumentService::new(Arc::new(DocumentParsers::new()), Arc::new(documents));
    let memory = MemoryService::new().with_clock(clock);
    let services = Services {
        code: Arc::new(code),
        document: Arc::new(document),
        memory: Arc::new(memory),
        trace: Arc::new(TraceabilityService::new()),
        jobs: JobIndex::new(),
        pro: Arc::new(ProService::new()),
    };
    (env_of(services), hashes)
}

impl Env {
    /// Dispatch one operation.
    ///
    /// The harness fills the two envelope preconditions the registry declares for
    /// most mutations (`idempotency_key`, `expected_etag`) so a scenario only has to
    /// state them when the *value* matters (replay, conflict, CAS). A derived key is
    /// unique per call, so an unrelated scenario can never collide with it.
    pub fn dispatch(
        &self,
        operation: &str,
        context: &RequestContext,
        input: Value,
    ) -> ResponseEnvelope<Value> {
        let mut context = context.clone();
        if context.idempotency_key.is_none() {
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            context.idempotency_key = Some(format!("auto:{operation}:{sequence}"));
        }
        if context.expected_etag.is_none() {
            context.expected_etag = Some("\"auto-etag\"".to_string());
        }
        self.gateway.dispatch(RequestEnvelope {
            api_version: API_VERSION.to_string(),
            operation: operation.to_string(),
            context,
            input,
        })
    }

    /// Dispatch and require success, returning `data`.
    pub fn ok(&self, operation: &str, context: &RequestContext, input: Value) -> Value {
        let response = self.dispatch(operation, context, input);
        assert_eq!(
            response.status,
            ResponseStatus::Ok,
            "{operation} failed: {:?}",
            response.error
        );
        response.data.unwrap_or(Value::Null)
    }

    /// Dispatch and require an error, returning its code.
    pub fn error(&self, operation: &str, context: &RequestContext, input: Value) -> String {
        let response = self.dispatch(operation, context, input);
        assert_eq!(
            response.status,
            ResponseStatus::Error,
            "{operation} unexpectedly succeeded: {:?}",
            response.data
        );
        response.error.expect("an error envelope").code
    }
}

/// Context for the default tenant/project with every scope granted.
pub fn ctx() -> RequestContext {
    RequestContext {
        request_id: "req-e2e".into(),
        trace_id: "trace-e2e".into(),
        scope: ScopeRef {
            tenant_id: TENANT.into(),
            project_id: PROJECT.into(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: PRINCIPAL.into(),
        scopes: all_scopes(),
        deadline_at: None,
        consistency: Consistency::ReadYourWrites,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

/// Context in another tenant (same project).
pub fn foreign_ctx(tenant: &str) -> RequestContext {
    let mut context = ctx();
    context.scope.tenant_id = tenant.to_string();
    context
}

/// Bind a context to a session/agent sandbox.
pub fn sandbox(mut context: RequestContext, session: &str, agent: &str) -> RequestContext {
    context.scope.session_id = Some(session.to_string());
    context.scope.agent_id = Some(agent.to_string());
    context
}

/// Attach an idempotency key.
pub fn idempotent(mut context: RequestContext, key: &str) -> RequestContext {
    context.idempotency_key = Some(key.to_string());
    context
}

/// Build an episode body for `memory.store_episode`.
pub fn episode(session: &str, agent: &str, summary: &str) -> Value {
    json!({
        "input": {
            "session_id": session,
            "agent_id": agent,
            "agent_depth": 0,
            "episode_type": "conversation_turn",
            "summary": summary,
            "full_text_ref": null,
            "key_facts": [],
            "outcome": "success",
            "source_refs": ["artifact:sha256:aa"],
            "importance_score": 0.5,
            "language": "en",
            "evidence": [{
                "evidence_id": "ev-1",
                "source_ref": "artifact:sha256:aa",
                "confidence": 0.9,
                "source_group": "group-1"
            }],
        }
    })
}

/// A `Ref` as the wire encodes it.
pub fn reference(id: &str, kind: &str, revision: &str) -> Value {
    json!({ "id": id, "kind": kind, "revision": revision })
}

/// Working block body.
pub fn working_block(session: &str, agent: &str, content: &str, priority: f64) -> Value {
    json!({
        "input": {
            "session_id": session,
            "agent_id": agent,
            "block_type": "recent_message",
            "content": content,
            "language": "en",
            "priority": priority,
            "protected": false,
            "expires_after_turns": null,
        },
        "automatic": false,
    })
}

/// Deterministic incompressible payload.
pub fn payload(seed: usize, bytes: usize) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ (seed as u64);
    let mut text = String::with_capacity(bytes);
    while text.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        text.push((b'a' + (state % 26) as u8) as char);
    }
    text
}

/// The `etag` field of a receipt, as a string.
pub fn etag_of(value: &Value) -> String {
    value
        .get("etag")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
