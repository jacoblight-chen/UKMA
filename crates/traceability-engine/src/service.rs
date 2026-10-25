//! The traceability service: TAPI-01..13 (§35.2).
//!
//! Endpoint heads, requirement authorisation and revision snapshots arrive through
//! explicit seams (`observe_endpoint`, `register_requirement`, `record_snapshot`)
//! because they belong to ingestion and policy, not to this domain. Nothing here
//! invents a head revision or an authorisation decision.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use ukma_contracts::RequestContext;

use crate::analysis::{self, CoverageInputs, RequirementEntry};
use crate::domain::{
    AcceptDecision, AcceptLink, Completeness, Conclusion, CoverageQuery, CoverageReport,
    CurrentApplicability, EndpointKind, Evidence, EvidenceReport, ExplainEvidence, GapQuery,
    GapReport, GetLink, GetTraceJob, IndexFreshness, JobHandle, JobView, ListLinks, Page,
    PageRequest, ProposeLink, Ref, Result, RevalidateStale, RevokeLink, TraceError, TraceImpact,
    TraceImpactQuery, TraceKind, TraceLink, TraceLinkInput, TraceRevisionDiff,
    TraceRevisionDiffResult, TraceState, ValidateLink,
};
use crate::link::EndpointHeads;

/// Scope needed to propose a link.
pub const SCOPE_WRITE: &str = "trace:write";
/// Scope needed to verify a link.
pub const SCOPE_VERIFY: &str = "trace:verify";
/// Scope needed to accept, reject or revoke a link.
pub const SCOPE_APPROVE: &str = "trace:approve";
/// Scope needed to read links.
pub const SCOPE_READ: &str = "trace:read";
/// Scope needed to register endpoints and snapshots.
pub const SCOPE_ADMIN: &str = "trace:admin";

/// Authorization decisions.
pub trait TraceAuthorizer: Send + Sync {
    /// Require `scope` on the context.
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> Result<()>;
}

/// Default authorizer: the delegated scope must be present.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScopeAuthorizer;

impl TraceAuthorizer for ScopeAuthorizer {
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> Result<()> {
        if ctx.scopes.iter().any(|granted| granted == scope) {
            Ok(())
        } else {
            Err(TraceError::Forbidden {
                required_scope: scope.to_string(),
            })
        }
    }
}

/// Time source.
pub trait TraceClock: Send + Sync {
    /// Current instant as an RFC 3339 UTC string.
    fn now(&self) -> String;
}

/// Clock backed by the fixed simulation instant used by tests and fixtures.
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: Arc<Mutex<i64>>,
}

impl Default for FixedClock {
    fn default() -> Self {
        Self {
            now: Arc::new(Mutex::new(1_000_000_000)),
        }
    }
}

impl FixedClock {
    /// Clock pinned at an instant.
    pub fn new(seconds: i64) -> Self {
        Self {
            now: Arc::new(Mutex::new(seconds)),
        }
    }

    /// Advance the clock.
    pub fn advance(&self, seconds: i64) {
        let mut guard = self.now.lock().expect("clock poisoned");
        *guard += seconds;
    }
}

impl TraceClock for FixedClock {
    fn now(&self) -> String {
        let seconds = *self.now.lock().expect("clock poisoned");
        crate::time::format_rfc3339(seconds)
    }
}

/// Wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl TraceClock for SystemClock {
    fn now(&self) -> String {
        crate::time::format_rfc3339(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(0),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    scope: (String, String),
    kind: EndpointKind,
    head_revision: Option<String>,
    authorised: bool,
    in_scope: bool,
    index: IndexFreshness,
}

#[derive(Debug, Clone)]
struct Snapshot {
    /// Link id to (etag, state) at snapshot time.
    links: BTreeMap<String, (String, TraceState)>,
}

#[derive(Debug, Clone)]
struct RevalidationJob {
    job_id: String,
    stage: String,
    diagnostics: Vec<String>,
    committed: bool,
}

#[derive(Debug, Default)]
struct ServiceState {
    links: BTreeMap<String, TraceLink>,
    endpoints: BTreeMap<String, Endpoint>,
    snapshots: BTreeMap<String, Snapshot>,
    jobs: BTreeMap<String, RevalidationJob>,
    sequence: u64,
}

impl ServiceState {
    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
}

/// Reference implementation of the traceability domain service.
pub struct TraceabilityService {
    authorizer: Arc<dyn TraceAuthorizer>,
    clock: Arc<dyn TraceClock>,
    state: Mutex<ServiceState>,
}

impl std::fmt::Debug for TraceabilityService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceabilityService").finish()
    }
}

impl Default for TraceabilityService {
    fn default() -> Self {
        Self::new()
    }
}

fn link_etag(id: &str, revision: u32) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update([0u8]);
    hasher.update(revision.to_le_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    format!("\"{out}\"")
}

impl TraceabilityService {
    /// New service with the default authorizer and a wall clock.
    pub fn new() -> Self {
        Self {
            authorizer: Arc::new(ScopeAuthorizer),
            clock: Arc::new(SystemClock),
            state: Mutex::new(ServiceState::default()),
        }
    }

    /// Replace the authorizer.
    pub fn with_authorizer(mut self, authorizer: Arc<dyn TraceAuthorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    /// Replace the clock.
    pub fn with_clock(mut self, clock: Arc<dyn TraceClock>) -> Self {
        self.clock = clock;
        self
    }

    fn lock(&self) -> MutexGuard<'_, ServiceState> {
        self.state
            .lock()
            .expect("traceability service state poisoned")
    }

    fn scope_of(ctx: &RequestContext) -> (String, String) {
        (ctx.scope.tenant_id.clone(), ctx.scope.project_id.clone())
    }

    fn check_endpoint_scope(
        state: &ServiceState,
        ctx: &RequestContext,
        reference: &Ref,
    ) -> Result<()> {
        match state.endpoints.get(&reference.id) {
            Some(endpoint) => {
                let requested = Self::scope_of(ctx);
                if endpoint.scope == requested {
                    Ok(())
                } else {
                    Err(TraceError::CrossScopeDenied {
                        requested: format!("{}/{}", requested.0, requested.1),
                        actual: format!("{}/{}", endpoint.scope.0, endpoint.scope.1),
                    })
                }
            }
            None => Ok(()),
        }
    }

    fn require_scope_match(&self, ctx: &RequestContext, reference: &Ref) -> Result<()> {
        let state = self.lock();
        Self::check_endpoint_scope(&state, ctx, reference)
    }

    /// Register an endpoint with its current head revision and visibility.
    ///
    /// Ingestion calls this; on the wire the head comes from the code/document
    /// analysis jobs, not from this domain.
    pub fn register_endpoint(
        &self,
        ctx: &RequestContext,
        reference: &Ref,
        kind: EndpointKind,
        authorised: bool,
        in_scope: bool,
        index: IndexFreshness,
    ) -> Result<()> {
        self.authorizer.require_scope(ctx, SCOPE_ADMIN)?;
        if reference.id.trim().is_empty() {
            return Err(TraceError::InvalidArgument {
                field: "endpoint.id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let mut state = self.lock();
        state.endpoints.insert(
            reference.id.clone(),
            Endpoint {
                scope: Self::scope_of(ctx),
                kind,
                head_revision: Some(reference.revision.clone()),
                authorised,
                in_scope,
                index,
            },
        );
        Ok(())
    }

    /// Record a new head revision for an endpoint.
    ///
    /// Every link that asserted the previous revision becomes `stale` in the
    /// workflow while its historical validity is untouched (§40.3).
    pub fn observe_endpoint(&self, ctx: &RequestContext, endpoint: &Ref) -> Result<u32> {
        self.authorizer.require_scope(ctx, SCOPE_ADMIN)?;
        let instant = self.clock.now();
        let mut state = self.lock();
        let requested = Self::scope_of(ctx);
        let record = state
            .endpoints
            .get_mut(&endpoint.id)
            .ok_or_else(|| TraceError::NotFound {
                resource: format!("endpoint {}", endpoint.id),
            })?;
        if record.scope != requested {
            return Err(TraceError::CrossScopeDenied {
                requested: format!("{}/{}", requested.0, requested.1),
                actual: format!("{}/{}", record.scope.0, record.scope.1),
            });
        }
        record.head_revision = Some(endpoint.revision.clone());
        let mut affected = 0u32;
        let endpoints = state.endpoints.clone();
        let links = std::mem::take(&mut state.links);
        let mut updated: BTreeMap<String, TraceLink> = BTreeMap::new();
        for (id, mut link) in links {
            let asserted = if link.source.id == endpoint.id {
                link.source_revision.clone()
            } else if link.target.id == endpoint.id {
                link.target_revision.clone()
            } else {
                updated.insert(id, link);
                continue;
            };
            if asserted == endpoint.revision {
                updated.insert(id, link);
                continue;
            }
            if crate::link::can_transition(link.state, TraceState::Stale) {
                crate::link::transition(&mut link, TraceState::Stale, &instant)?;
                affected += 1;
            }
            let heads = self.heads_for(&endpoints, &link);
            link.validity = crate::link::assess(&link, &heads, IndexFreshness::Current, &instant);
            updated.insert(id, link);
        }
        state.links = updated;
        Ok(affected)
    }

    /// Record the current link set as a revision snapshot (TAPI-10 input).
    pub fn record_snapshot(&self, ctx: &RequestContext, revision_id: &str) -> Result<usize> {
        self.authorizer.require_scope(ctx, SCOPE_ADMIN)?;
        if revision_id.trim().is_empty() {
            return Err(TraceError::InvalidArgument {
                field: "revision_id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let mut state = self.lock();
        let scope = Self::scope_of(ctx);
        let links: BTreeMap<String, (String, TraceState)> = state
            .links
            .iter()
            .filter(|(_, link)| {
                state
                    .endpoints
                    .get(&link.source.id)
                    .map(|endpoint| endpoint.scope == scope)
                    .unwrap_or(true)
            })
            .map(|(id, link)| (id.clone(), (link.etag.clone(), link.state)))
            .collect();
        let count = links.len();
        state
            .snapshots
            .insert(revision_id.to_string(), Snapshot { links });
        Ok(count)
    }

    fn heads_for(&self, endpoints: &BTreeMap<String, Endpoint>, link: &TraceLink) -> EndpointHeads {
        EndpointHeads {
            source: endpoints
                .get(&link.source.id)
                .and_then(|endpoint| endpoint.head_revision.clone()),
            target: endpoints
                .get(&link.target.id)
                .and_then(|endpoint| endpoint.head_revision.clone()),
        }
    }

    fn reindex(&self, state: &mut ServiceState, instant: &str) {
        let endpoints = state.endpoints.clone();
        for link in state.links.values_mut() {
            let heads = self.heads_for(&endpoints, link);
            let index = endpoints
                .get(&link.source.id)
                .map(|endpoint| endpoint.index)
                .unwrap_or(IndexFreshness::Unknown);
            link.validity = crate::link::assess(link, &heads, index, instant);
        }
    }

    fn lookup(&self, state: &ServiceState, id: &str) -> Result<TraceLink> {
        state
            .links
            .get(id)
            .cloned()
            .ok_or_else(|| TraceError::NotFound {
                resource: format!("link {id}"),
            })
    }

    fn check_etag(expected: &str, link: &TraceLink) -> Result<()> {
        if expected == link.etag {
            Ok(())
        } else {
            Err(TraceError::EtagMismatch {
                expected: expected.to_string(),
                actual: link.etag.clone(),
            })
        }
    }

    // -- TAPI-01 -------------------------------------------------------------

    /// `traceability.propose_link` — record a candidate link.
    pub fn propose_link(&self, ctx: &RequestContext, request: &ProposeLink) -> Result<TraceLink> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let input: &TraceLinkInput = &request.link;
        if input.evidence.is_empty() {
            return Err(TraceError::EvidenceRequired {
                operation: "traceability.propose_link".to_string(),
            });
        }
        if input.source.id == input.target.id {
            return Err(TraceError::InvalidArgument {
                field: "target".to_string(),
                reason: "a link cannot point at itself".to_string(),
            });
        }
        self.require_scope_match(ctx, &input.source)?;
        self.require_scope_match(ctx, &input.target)?;
        let mut state = self.lock();
        for endpoint in [&input.source, &input.target] {
            if !state.endpoints.contains_key(&endpoint.id) {
                return Err(TraceError::NotFound {
                    resource: format!("endpoint {}", endpoint.id),
                });
            }
        }
        let sequence = state.next_sequence();
        let id = format!(
            "link:{}:{}:{}",
            ctx.scope.tenant_id, ctx.scope.project_id, sequence
        );
        let instant = self.clock.now();
        let mut link = TraceLink {
            reference: Ref::new(id.clone(), "trace_link", link_etag(&id, 1)),
            source: input.source.clone(),
            target: input.target.clone(),
            kind: input.kind,
            state: TraceState::Proposed,
            evidence: Vec::new(),
            rationale: input.rationale.clone(),
            created_at: instant.clone(),
            verified_at: None,
            source_revision: input.source.revision.clone(),
            target_revision: input.target.revision.clone(),
            etag: link_etag(&id, 1),
            validity: crate::domain::ValidityVector {
                historical_validity: crate::domain::HistoricalValidity::Unverified,
                current_applicability: CurrentApplicability::Unknown,
                index_freshness: IndexFreshness::Unknown,
                asserted_source_revision: input.source.revision.clone(),
                observed_head_revision: None,
                assessed_at: instant.clone(),
                evidence_refs: Vec::new(),
            },
        };
        crate::link::append_evidence(&mut link, &input.evidence)?;
        let heads = self.heads_for(&state.endpoints, &link);
        let index = state
            .endpoints
            .get(&link.source.id)
            .map(|endpoint| endpoint.index)
            .unwrap_or(IndexFreshness::Unknown);
        link.validity = crate::link::assess(&link, &heads, index, &instant);
        state.links.insert(id, link.clone());
        Ok(link)
    }

    // -- TAPI-02 -------------------------------------------------------------

    /// `traceability.validate_link` — verify or reject a proposed link.
    pub fn validate_link(&self, ctx: &RequestContext, request: &ValidateLink) -> Result<TraceLink> {
        self.authorizer.require_scope(ctx, SCOPE_VERIFY)?;
        if request.evidence_ids.is_empty() {
            return Err(TraceError::EvidenceRequired {
                operation: "traceability.validate_link".to_string(),
            });
        }
        let instant = self.clock.now();
        let mut state = self.lock();
        let mut link = self.lookup(&state, &request.link.id)?;
        Self::check_endpoint_scope(&state, ctx, &link.source)?;
        Self::check_etag(&request.expected_etag, &link)?;
        for id in &request.evidence_ids {
            if !link.evidence.iter().any(|item| &item.evidence_id == id) {
                return Err(TraceError::NotFound {
                    resource: format!("evidence {id}"),
                });
            }
        }
        // Both asserted revisions must still be the observed heads.
        for (endpoint, asserted) in [
            (&link.source, &link.source_revision),
            (&link.target, &link.target_revision),
        ] {
            if let Some(record) = state.endpoints.get(&endpoint.id) {
                if let Some(head) = &record.head_revision {
                    if head != asserted {
                        return Err(TraceError::SourceRevisionMismatch {
                            endpoint: endpoint.id.clone(),
                            asserted: asserted.clone(),
                            observed: head.clone(),
                        });
                    }
                }
            }
        }
        crate::link::transition(&mut link, TraceState::Validated, &instant)?;
        let previous = state.links.get(&request.link.id).cloned();
        let revision = previous.map(|value| value.etag.len() as u32).unwrap_or(1) + 1;
        link.etag = link_etag(&link.reference.id, revision);
        link.reference.revision = link.etag.clone();
        let heads = self.heads_for(&state.endpoints, &link);
        let index = state
            .endpoints
            .get(&link.source.id)
            .map(|endpoint| endpoint.index)
            .unwrap_or(IndexFreshness::Unknown);
        link.validity = crate::link::assess(&link, &heads, index, &instant);
        state.links.insert(request.link.id.clone(), link.clone());
        Ok(link)
    }

    // -- TAPI-03 -------------------------------------------------------------

    /// `traceability.accept_link` — accept or reject a validated link.
    pub fn accept_link(&self, ctx: &RequestContext, request: &AcceptLink) -> Result<TraceLink> {
        self.authorizer.require_scope(ctx, SCOPE_APPROVE)?;
        let instant = self.clock.now();
        let mut state = self.lock();
        let mut link = self.lookup(&state, &request.link.id)?;
        Self::check_endpoint_scope(&state, ctx, &link.source)?;
        Self::check_etag(&request.expected_etag, &link)?;
        if link.state != TraceState::Validated {
            return Err(TraceError::LinkStateConflict {
                link: link.reference.id.clone(),
                state: link.state.as_str().to_string(),
                transition: "be reviewed".to_string(),
            });
        }
        let target = match request.decision {
            AcceptDecision::Accept => TraceState::Accepted,
            AcceptDecision::Reject => TraceState::Rejected,
        };
        crate::link::transition(&mut link, target, &instant)?;
        let revision = state.next_sequence() as u32 + 1;
        link.etag = link_etag(&link.reference.id, revision);
        link.reference.revision = link.etag.clone();
        let heads = self.heads_for(&state.endpoints, &link);
        let index = state
            .endpoints
            .get(&link.source.id)
            .map(|endpoint| endpoint.index)
            .unwrap_or(IndexFreshness::Unknown);
        link.validity = crate::link::assess(&link, &heads, index, &instant);
        state.links.insert(request.link.id.clone(), link.clone());
        Ok(link)
    }

    // -- TAPI-04 -------------------------------------------------------------

    /// `traceability.revoke_link` — withdraw a link, keeping its history.
    pub fn revoke_link(&self, ctx: &RequestContext, request: &RevokeLink) -> Result<TraceLink> {
        self.authorizer.require_scope(ctx, SCOPE_APPROVE)?;
        if request.reason.trim().is_empty() {
            return Err(TraceError::InvalidArgument {
                field: "reason".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let instant = self.clock.now();
        let mut state = self.lock();
        let mut link = self.lookup(&state, &request.link.id)?;
        Self::check_endpoint_scope(&state, ctx, &link.source)?;
        Self::check_etag(&request.expected_etag, &link)?;
        crate::link::transition(&mut link, TraceState::Revoked, &instant)?;
        let revision = state.next_sequence() as u32 + 1;
        link.etag = link_etag(&link.reference.id, revision);
        link.reference.revision = link.etag.clone();
        link.rationale = Some(format!(
            "{} (revoked: {})",
            link.rationale.clone().unwrap_or_default(),
            request.reason
        ));
        let heads = self.heads_for(&state.endpoints, &link);
        let index = state
            .endpoints
            .get(&link.source.id)
            .map(|endpoint| endpoint.index)
            .unwrap_or(IndexFreshness::Unknown);
        link.validity = crate::link::assess(&link, &heads, index, &instant);
        state.links.insert(request.link.id.clone(), link.clone());
        Ok(link)
    }

    // -- TAPI-05 -------------------------------------------------------------

    /// `traceability.get_link` — read one link.
    pub fn get_link(&self, ctx: &RequestContext, request: &GetLink) -> Result<TraceLink> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let link = self.lookup(&state, &request.link.id)?;
        let requested = Self::scope_of(ctx);
        for endpoint in [&link.source, &link.target] {
            if let Some(record) = state.endpoints.get(&endpoint.id) {
                if record.scope != requested {
                    return Err(TraceError::CrossScopeDenied {
                        requested: format!("{}/{}", requested.0, requested.1),
                        actual: format!("{}/{}", record.scope.0, record.scope.1),
                    });
                }
                if !record.authorised {
                    return Err(TraceError::Forbidden {
                        required_scope: format!("read access to {}", endpoint.id),
                    });
                }
            }
        }
        Ok(link)
    }

    // -- TAPI-06 -------------------------------------------------------------

    /// `traceability.list_links` — links around one endpoint.
    pub fn list_links(&self, ctx: &RequestContext, request: &ListLinks) -> Result<Page<TraceLink>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let requested = Self::scope_of(ctx);
        let mut links: Vec<TraceLink> = state
            .links
            .values()
            .filter(|link| {
                link.source.id == request.endpoint.id || link.target.id == request.endpoint.id
            })
            .filter(|link| request.kinds.is_empty() || request.kinds.contains(&link.kind))
            .filter(|link| request.states.is_empty() || request.states.contains(&link.state))
            .filter(|link| {
                state
                    .endpoints
                    .get(&link.source.id)
                    .map(|endpoint| endpoint.scope == requested && endpoint.authorised)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        links.sort_by(|left, right| left.reference.id.cmp(&right.reference.id));
        Page::of(links, &request.page)
    }

    // -- TAPI-07 -------------------------------------------------------------

    /// `traceability.query_coverage` — coverage over authorised requirements.
    pub fn query_coverage(
        &self,
        ctx: &RequestContext,
        request: &CoverageQuery,
    ) -> Result<CoverageReport> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let mut state = self.lock();
        let instant = self.clock.now();
        self.reindex(&mut state, &instant);
        let requested = Self::scope_of(ctx);
        if request.requirement_scope.tenant_id != requested.0
            || request.requirement_scope.project_id != requested.1
        {
            return Err(TraceError::CrossScopeDenied {
                requested: format!(
                    "{}/{}",
                    request.requirement_scope.tenant_id, request.requirement_scope.project_id
                ),
                actual: format!("{}/{}", requested.0, requested.1),
            });
        }
        let explicit: BTreeSet<&str> = request
            .requirement_refs
            .iter()
            .map(|reference| reference.id.as_str())
            .collect();
        let requirements: Vec<RequirementEntry> = state
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.kind == EndpointKind::Requirement)
            .filter(|(id, _)| explicit.is_empty() || explicit.contains(id.as_str()))
            .map(|(id, endpoint)| RequirementEntry {
                reference: Ref::new(
                    id.clone(),
                    "requirement",
                    endpoint.head_revision.clone().unwrap_or_default(),
                ),
                authorised: endpoint.authorised,
                in_scope: endpoint.in_scope,
            })
            .collect();
        let links: Vec<TraceLink> = state.links.values().cloned().collect();
        let snapshot_id = format!("snapshot-live-{}", state.sequence);
        Ok(analysis::coverage(&CoverageInputs {
            requirements: &requirements,
            links: &links,
            implementation_kinds: if request.implementation_kinds.is_empty() {
                vec![TraceKind::Implements]
            } else {
                request.implementation_kinds.clone()
            },
            accepted_only: request.accepted_only,
            include_unindexed: request.include_unindexed,
            snapshot_id,
        }))
    }

    // -- TAPI-08 -------------------------------------------------------------

    /// `traceability.analyze_change_impact` — bounded impact traversal.
    pub fn analyze_change_impact(
        &self,
        ctx: &RequestContext,
        request: &TraceImpactQuery,
    ) -> Result<TraceImpact> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let mut state = self.lock();
        let instant = self.clock.now();
        self.reindex(&mut state, &instant);
        for reference in &request.changed {
            Self::check_endpoint_scope(&state, ctx, reference)?;
        }
        let links: Vec<TraceLink> = state.links.values().cloned().collect();
        let mut impact =
            analysis::impact(&request.changed, &links, request.max_depth, &request.kinds)?;
        let page = Page::of(impact.links.clone(), &request.page)?;
        impact.links = page.items;
        Ok(impact)
    }

    // -- TAPI-09 -------------------------------------------------------------

    /// `traceability.find_design_gaps` — missing implementation and tests.
    pub fn find_design_gaps(&self, ctx: &RequestContext, request: &GapQuery) -> Result<GapReport> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let mut state = self.lock();
        let instant = self.clock.now();
        self.reindex(&mut state, &instant);
        let requested = Self::scope_of(ctx);
        if request.scope.tenant_id != requested.0 || request.scope.project_id != requested.1 {
            return Err(TraceError::CrossScopeDenied {
                requested: format!("{}/{}", request.scope.tenant_id, request.scope.project_id),
                actual: format!("{}/{}", requested.0, requested.1),
            });
        }
        let explicit: BTreeSet<&str> = request
            .requirement_refs
            .iter()
            .map(|reference| reference.id.as_str())
            .collect();
        let requirements: Vec<RequirementEntry> = state
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.kind == EndpointKind::Requirement)
            .filter(|(id, _)| explicit.is_empty() || explicit.contains(id.as_str()))
            .map(|(id, endpoint)| RequirementEntry {
                reference: Ref::new(
                    id.clone(),
                    "requirement",
                    endpoint.head_revision.clone().unwrap_or_default(),
                ),
                authorised: endpoint.authorised,
                in_scope: endpoint.in_scope,
            })
            .collect();
        let links: Vec<TraceLink> = state.links.values().cloned().collect();
        Ok(analysis::gaps(&requirements, &links))
    }

    // -- TAPI-10 -------------------------------------------------------------

    /// `traceability.compare_revisions` — diff two recorded snapshots.
    pub fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &TraceRevisionDiff,
    ) -> Result<TraceRevisionDiffResult> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let requested = Self::scope_of(ctx);
        if request.scope.tenant_id != requested.0 || request.scope.project_id != requested.1 {
            return Err(TraceError::CrossScopeDenied {
                requested: format!("{}/{}", request.scope.tenant_id, request.scope.project_id),
                actual: format!("{}/{}", requested.0, requested.1),
            });
        }
        let from = state.snapshots.get(&request.from_revision).ok_or_else(|| {
            TraceError::SourceUnavailable {
                reference: format!("snapshot {}", request.from_revision),
            }
        })?;
        let to = state.snapshots.get(&request.to_revision).ok_or_else(|| {
            TraceError::SourceUnavailable {
                reference: format!("snapshot {}", request.to_revision),
            }
        })?;
        let mut added = Vec::new();
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for (id, (etag, state_token)) in &to.links {
            match from.links.get(id) {
                None => {
                    if let Some(link) = state.links.get(id) {
                        added.push(link.clone());
                    }
                }
                Some((previous_etag, previous_state)) => {
                    if previous_etag != etag || previous_state != state_token {
                        if let Some(link) = state.links.get(id) {
                            changed.push(link.clone());
                        }
                    }
                }
            }
        }
        for id in from.links.keys() {
            if !to.links.contains_key(id) {
                if let Some(link) = state.links.get(id) {
                    removed.push(link.clone());
                }
            }
        }
        added.sort_by(|left, right| left.reference.id.cmp(&right.reference.id));
        changed.sort_by(|left, right| left.reference.id.cmp(&right.reference.id));
        removed.sort_by(|left, right| left.reference.id.cmp(&right.reference.id));
        Ok(TraceRevisionDiffResult {
            added,
            changed,
            removed,
        })
    }

    // -- TAPI-11 -------------------------------------------------------------

    /// `traceability.explain_with_evidence` — explain a claim from links.
    pub fn explain_with_evidence(
        &self,
        ctx: &RequestContext,
        request: &ExplainEvidence,
    ) -> Result<EvidenceReport> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let requested = Self::scope_of(ctx);
        let readable: BTreeSet<String> = state
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.scope == requested && endpoint.authorised)
            .map(|(id, _)| id.clone())
            .collect();
        let links: Vec<TraceLink> = state.links.values().cloned().collect();
        analysis::explain(
            &request.claim,
            &request.refs,
            &links,
            &readable,
            request.max_tokens,
        )
    }

    // -- TAPI-12 -------------------------------------------------------------

    /// `traceability.revalidate_stale` — queue stale links for revalidation.
    ///
    /// A link whose sources are not available stays `stale`: the job never
    /// auto-accepts anything (§35.2).
    pub fn revalidate_stale(
        &self,
        ctx: &RequestContext,
        request: &RevalidateStale,
    ) -> Result<JobHandle> {
        self.authorizer.require_scope(ctx, SCOPE_VERIFY)?;
        if !request.asynchronous {
            return Err(TraceError::InvalidArgument {
                field: "async".to_string(),
                reason: "revalidation is asynchronous; pass `async: true` (§35.2)".to_string(),
            });
        }
        if request.link_refs.is_empty() {
            return Err(TraceError::InvalidArgument {
                field: "link_refs".to_string(),
                reason: "at least one link is required".to_string(),
            });
        }
        if request.policy_id.trim().is_empty() {
            return Err(TraceError::InvalidArgument {
                field: "policy_id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let instant = self.clock.now();
        let mut state = self.lock();
        let sequence = state.next_sequence();
        let job_id = format!(
            "job:trace-revalidation:{}:{}",
            ctx.scope.tenant_id, sequence
        );
        let mut revalidated = 0u32;
        let mut still_stale = 0u32;
        let mut diagnostics = Vec::new();
        for reference in &request.link_refs {
            let Some(link) = state.links.get(&reference.id).cloned() else {
                still_stale += 1;
                diagnostics.push(format!("{} is unavailable and stays stale", reference.id));
                continue;
            };
            if link.state != TraceState::Stale {
                diagnostics.push(format!(
                    "{} is {} and was not revalidated",
                    reference.id,
                    link.state.as_str()
                ));
                continue;
            }
            let mut available = true;
            for (endpoint, asserted) in [
                (&link.source, &link.source_revision),
                (&link.target, &link.target_revision),
            ] {
                match state.endpoints.get(&endpoint.id) {
                    None => available = false,
                    Some(record) => match &record.head_revision {
                        None => available = false,
                        Some(head) => {
                            if head != asserted {
                                // The head moved on: the link needs a new revision,
                                // not a silent revalidation.
                                available = false;
                                diagnostics.push(format!(
                                    "{} asserted {} but {} is at {}",
                                    reference.id, asserted, endpoint.id, head
                                ));
                            }
                        }
                    },
                }
            }
            if !available {
                still_stale += 1;
                continue;
            }
            let mut updated = link;
            crate::link::transition(&mut updated, TraceState::Validated, &instant)?;
            let revision = state.next_sequence() as u32 + 1;
            updated.etag = link_etag(&updated.reference.id, revision);
            updated.reference.revision = updated.etag.clone();
            let heads = self.heads_for(&state.endpoints, &updated);
            updated.validity =
                crate::link::assess(&updated, &heads, IndexFreshness::Current, &instant);
            state.links.insert(reference.id.clone(), updated);
            revalidated += 1;
        }
        let stage = if still_stale > 0 {
            "partial"
        } else {
            "committed"
        };
        diagnostics.push(format!(
            "{revalidated} revalidated, {still_stale} still stale"
        ));
        state.jobs.insert(
            job_id.clone(),
            RevalidationJob {
                job_id: job_id.clone(),
                stage: stage.to_string(),
                diagnostics,
                committed: still_stale == 0,
            },
        );
        Ok(JobHandle {
            job_id,
            kind: "traceability.revalidate_stale".to_string(),
            stage: stage.to_string(),
        })
    }

    // -- TAPI-13 -------------------------------------------------------------

    /// `traceability.get_revalidation_job` — live view of a revalidation job.
    pub fn get_revalidation_job(
        &self,
        ctx: &RequestContext,
        request: &GetTraceJob,
    ) -> Result<JobView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let expected = format!("job:trace-revalidation:{}:", ctx.scope.tenant_id);
        if !request.job_id.starts_with(&expected) {
            return Err(TraceError::JobNotFound {
                job_id: request.job_id.clone(),
            });
        }
        let state = self.lock();
        let job = state
            .jobs
            .get(&request.job_id)
            .ok_or_else(|| TraceError::JobNotFound {
                job_id: request.job_id.clone(),
            })?;
        Ok(JobView {
            job_id: job.job_id.clone(),
            kind: "traceability.revalidate_stale".to_string(),
            stage: job.stage.clone(),
            canonical_committed: job.committed,
            diagnostics: job.diagnostics.clone(),
        })
    }

    /// Number of links currently stored (maintenance visibility).
    pub fn link_count(&self) -> usize {
        self.lock().links.len()
    }
}

/// The §37 `TraceabilityPort`.
#[allow(async_fn_in_trait)]
pub trait TraceabilityPort: Send + Sync {
    /// TAPI-01.
    async fn propose_link(&self, ctx: &RequestContext, request: &ProposeLink) -> Result<TraceLink>;
    /// TAPI-02.
    async fn validate_link(
        &self,
        ctx: &RequestContext,
        request: &ValidateLink,
    ) -> Result<TraceLink>;
    /// TAPI-03.
    async fn accept_link(&self, ctx: &RequestContext, request: &AcceptLink) -> Result<TraceLink>;
    /// TAPI-04.
    async fn revoke_link(&self, ctx: &RequestContext, request: &RevokeLink) -> Result<TraceLink>;
    /// TAPI-05.
    async fn get_link(&self, ctx: &RequestContext, request: &GetLink) -> Result<TraceLink>;
    /// TAPI-06.
    async fn list_links(
        &self,
        ctx: &RequestContext,
        request: &ListLinks,
    ) -> Result<Page<TraceLink>>;
    /// TAPI-07.
    async fn query_coverage(
        &self,
        ctx: &RequestContext,
        request: &CoverageQuery,
    ) -> Result<CoverageReport>;
    /// TAPI-08.
    async fn analyze_change_impact(
        &self,
        ctx: &RequestContext,
        request: &TraceImpactQuery,
    ) -> Result<TraceImpact>;
    /// TAPI-09.
    async fn find_design_gaps(&self, ctx: &RequestContext, request: &GapQuery)
        -> Result<GapReport>;
    /// TAPI-10.
    async fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &TraceRevisionDiff,
    ) -> Result<TraceRevisionDiffResult>;
    /// TAPI-11.
    async fn explain_with_evidence(
        &self,
        ctx: &RequestContext,
        request: &ExplainEvidence,
    ) -> Result<EvidenceReport>;
    /// TAPI-12.
    async fn revalidate_stale(
        &self,
        ctx: &RequestContext,
        request: &RevalidateStale,
    ) -> Result<JobHandle>;
    /// TAPI-13.
    async fn get_revalidation_job(
        &self,
        ctx: &RequestContext,
        request: &GetTraceJob,
    ) -> Result<JobView>;
}

impl TraceabilityPort for TraceabilityService {
    async fn propose_link(&self, ctx: &RequestContext, request: &ProposeLink) -> Result<TraceLink> {
        TraceabilityService::propose_link(self, ctx, request)
    }
    async fn validate_link(
        &self,
        ctx: &RequestContext,
        request: &ValidateLink,
    ) -> Result<TraceLink> {
        TraceabilityService::validate_link(self, ctx, request)
    }
    async fn accept_link(&self, ctx: &RequestContext, request: &AcceptLink) -> Result<TraceLink> {
        TraceabilityService::accept_link(self, ctx, request)
    }
    async fn revoke_link(&self, ctx: &RequestContext, request: &RevokeLink) -> Result<TraceLink> {
        TraceabilityService::revoke_link(self, ctx, request)
    }
    async fn get_link(&self, ctx: &RequestContext, request: &GetLink) -> Result<TraceLink> {
        TraceabilityService::get_link(self, ctx, request)
    }
    async fn list_links(
        &self,
        ctx: &RequestContext,
        request: &ListLinks,
    ) -> Result<Page<TraceLink>> {
        TraceabilityService::list_links(self, ctx, request)
    }
    async fn query_coverage(
        &self,
        ctx: &RequestContext,
        request: &CoverageQuery,
    ) -> Result<CoverageReport> {
        TraceabilityService::query_coverage(self, ctx, request)
    }
    async fn analyze_change_impact(
        &self,
        ctx: &RequestContext,
        request: &TraceImpactQuery,
    ) -> Result<TraceImpact> {
        TraceabilityService::analyze_change_impact(self, ctx, request)
    }
    async fn find_design_gaps(
        &self,
        ctx: &RequestContext,
        request: &GapQuery,
    ) -> Result<GapReport> {
        TraceabilityService::find_design_gaps(self, ctx, request)
    }
    async fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &TraceRevisionDiff,
    ) -> Result<TraceRevisionDiffResult> {
        TraceabilityService::compare_revisions(self, ctx, request)
    }
    async fn explain_with_evidence(
        &self,
        ctx: &RequestContext,
        request: &ExplainEvidence,
    ) -> Result<EvidenceReport> {
        TraceabilityService::explain_with_evidence(self, ctx, request)
    }
    async fn revalidate_stale(
        &self,
        ctx: &RequestContext,
        request: &RevalidateStale,
    ) -> Result<JobHandle> {
        TraceabilityService::revalidate_stale(self, ctx, request)
    }
    async fn get_revalidation_job(
        &self,
        ctx: &RequestContext,
        request: &GetTraceJob,
    ) -> Result<JobView> {
        TraceabilityService::get_revalidation_job(self, ctx, request)
    }
}

/// Whether a report's conclusion is decisive (§35.1).
pub fn is_decisive(conclusion: Conclusion) -> bool {
    matches!(conclusion, Conclusion::Supported | Conclusion::Contradicted)
}

/// Whether a report is complete (§35.1).
pub fn is_complete(completeness: Completeness) -> bool {
    completeness == Completeness::Complete
}

/// Convenience: an evidence list from raw tuples.
pub fn evidence(entries: &[(String, String, f64)]) -> Vec<Evidence> {
    entries
        .iter()
        .map(|(id, source, confidence)| Evidence {
            evidence_id: id.clone(),
            source_ref: source.clone(),
            confidence: *confidence,
        })
        .collect()
}

/// Convenience: an empty page request.
pub fn default_page() -> PageRequest {
    PageRequest::default()
}

/// Convenience: the link kind used for implementation coverage.
pub fn implements() -> TraceKind {
    TraceKind::Implements
}

/// Convenience: the wire token of a state.
pub fn state_token(state: TraceState) -> &'static str {
    state.as_str()
}

/// Convenience: whether an applicability value counts as covered (§40.3).
pub fn counts_as_covered(applicability: CurrentApplicability) -> bool {
    applicability == CurrentApplicability::Applicable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_predicates_match_the_spec() {
        assert!(is_decisive(Conclusion::Supported));
        assert!(is_decisive(Conclusion::Contradicted));
        assert!(!is_decisive(Conclusion::Insufficient));
        assert!(is_complete(Completeness::Complete));
        assert!(!is_complete(Completeness::Partial));
        assert_eq!(implements(), TraceKind::Implements);
        assert_eq!(state_token(TraceState::Stale), "stale");
        assert!(counts_as_covered(CurrentApplicability::Applicable));
        assert!(!counts_as_covered(
            CurrentApplicability::RequiresRevalidation
        ));
        assert_eq!(default_page().limit, 50);
        let list = evidence(&[("e1".into(), "artifact:sha256:aa".into(), 0.9)]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].evidence_id, "e1");
    }

    #[test]
    fn fixed_clock_is_deterministic_and_advances() {
        let clock = FixedClock::new(0);
        assert_eq!(clock.now(), "1970-01-01T00:00:00Z");
        clock.advance(3_600);
        assert_eq!(clock.now(), "1970-01-01T01:00:00Z");
    }
}
