//! Coverage, impact, gap and evidence analysis (§35.1, §40.3, TAPI-07..TAPI-11).
//!
//! Every analysis is explicit about what it could not decide: `unknown` is never
//! counted as covered, an empty denominator has no ratio at all, and a traversal
//! that hits a stale link reports `unknown_due_to_stale` instead of guessing.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    Completeness, Conclusion, CoverageReport, CurrentApplicability, Evidence, EvidenceReport,
    GapReport, IndexFreshness, Ref, Result, TraceError, TraceImpact, TraceKind, TraceLink,
    TraceState, ValidityVector,
};

/// Largest number of traversal candidates a single analysis may examine.
pub const MAX_TRAVERSAL_CANDIDATES: usize = 5_000;
/// Largest traversal depth an impact analysis accepts.
pub const MAX_IMPACT_DEPTH: u32 = 10;

/// One requirement registered for coverage analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementEntry {
    /// Requirement reference.
    pub reference: Ref,
    /// Whether the caller is authorised to see it.
    pub authorised: bool,
    /// Whether it is inside the queried scope.
    pub in_scope: bool,
}

/// Inputs of a coverage analysis.
#[derive(Debug, Clone)]
pub struct CoverageInputs<'a> {
    /// In-scope requirement candidates (already filtered by the caller).
    pub requirements: &'a [RequirementEntry],
    /// Every link the caller may read.
    pub links: &'a [TraceLink],
    /// Kinds that count as implementation.
    pub implementation_kinds: Vec<TraceKind>,
    /// Only count accepted links.
    pub accepted_only: bool,
    /// Count links whose index is lagging.
    pub include_unindexed: bool,
    /// Snapshot the report is computed against.
    pub snapshot_id: String,
}

fn state_counts(link: &TraceLink, accepted_only: bool) -> bool {
    if accepted_only {
        link.state == TraceState::Accepted
    } else {
        // A stale link was validated or accepted before its sources moved on, so
        // it still counts as evidence that the requirement *was* implemented.
        matches!(
            link.state,
            TraceState::Accepted | TraceState::Validated | TraceState::Stale
        )
    }
}

fn index_is_usable(validity: &ValidityVector, include_unindexed: bool) -> bool {
    if include_unindexed {
        return true;
    }
    matches!(
        validity.index_freshness,
        IndexFreshness::Current | IndexFreshness::Unknown
    )
}

/// Compute the coverage report (§35.1, TAPI-07).
///
/// The denominator is the authorised in-scope requirements. `unknown` is never
/// counted as covered, and a zero denominator yields `coverage_ratio = None`.
pub fn coverage(inputs: &CoverageInputs<'_>) -> CoverageReport {
    let mut covered = Vec::new();
    let mut uncovered = Vec::new();
    let mut unknown = Vec::new();
    let mut excluded_count = 0u32;
    let mut denominator = 0u32;

    for requirement in inputs.requirements {
        if !requirement.authorised || !requirement.in_scope {
            excluded_count += 1;
            continue;
        }
        denominator += 1;
        let mut qualifying: Vec<&TraceLink> = inputs
            .links
            .iter()
            .filter(|link| link.target.id == requirement.reference.id)
            .filter(|link| inputs.implementation_kinds.contains(&link.kind))
            .filter(|link| state_counts(link, inputs.accepted_only))
            .collect();
        qualifying.sort_by(|left, right| left.reference.id.cmp(&right.reference.id));

        if qualifying.is_empty() {
            uncovered.push(requirement.reference.clone());
            continue;
        }
        let decidable = qualifying.iter().find(|link| {
            link.validity.current_applicability == CurrentApplicability::Applicable
                && index_is_usable(&link.validity, inputs.include_unindexed)
        });
        let lagging = qualifying.iter().any(|link| {
            !index_is_usable(&link.validity, inputs.include_unindexed)
                || link.validity.current_applicability != CurrentApplicability::Applicable
        });
        match decidable {
            Some(_) => covered.push(requirement.reference.clone()),
            None if lagging => unknown.push(requirement.reference.clone()),
            None => uncovered.push(requirement.reference.clone()),
        }
    }

    let denominator_u32 = denominator;
    let coverage_ratio = if denominator_u32 == 0 {
        None
    } else {
        Some(f64::from(covered.len() as u32) / f64::from(denominator_u32))
    };
    let coverage_incomplete = excluded_count > 0 || !unknown.is_empty();
    CoverageReport {
        numerator: covered.len() as u32,
        denominator: denominator_u32,
        coverage_ratio,
        covered,
        uncovered,
        unknown,
        excluded_count,
        coverage_incomplete,
        snapshot_id: inputs.snapshot_id.clone(),
    }
}

fn pushes_over_limit(seen: usize) -> Result<()> {
    if seen > MAX_TRAVERSAL_CANDIDATES {
        return Err(TraceError::ResourceLimit {
            detail: format!("traversal examined more than {MAX_TRAVERSAL_CANDIDATES} candidates"),
        });
    }
    Ok(())
}

/// Analyse the impact of changed endpoints (§35.1, TAPI-08).
pub fn impact(
    changed: &[Ref],
    links: &[TraceLink],
    max_depth: u32,
    kinds: &[TraceKind],
) -> Result<TraceImpact> {
    if changed.is_empty() {
        return Err(TraceError::InvalidArgument {
            field: "changed".to_string(),
            reason: "at least one changed endpoint is required".to_string(),
        });
    }
    if max_depth == 0 || max_depth > MAX_IMPACT_DEPTH {
        return Err(TraceError::InvalidArgument {
            field: "max_depth".to_string(),
            reason: format!("must be in 1..={MAX_IMPACT_DEPTH}"),
        });
    }
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut frontier: Vec<(Ref, Vec<Ref>)> = Vec::new();
    for reference in changed {
        if visited.insert(reference.id.clone()) {
            frontier.push((reference.clone(), vec![reference.clone()]));
        }
    }
    let mut impacted: Vec<Ref> = Vec::new();
    let mut used_links: BTreeMap<String, TraceLink> = BTreeMap::new();
    let mut paths: Vec<Vec<Ref>> = Vec::new();
    let mut unknown_due_to_stale: Vec<Ref> = Vec::new();

    for _ in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let mut next: Vec<(Ref, Vec<Ref>)> = Vec::new();
        for (node, path) in frontier {
            for link in links {
                if !kinds.is_empty() && !kinds.contains(&link.kind) {
                    continue;
                }
                let other = if link.source.id == node.id {
                    Some(&link.target)
                } else if link.target.id == node.id {
                    Some(&link.source)
                } else {
                    None
                };
                let Some(other) = other else { continue };
                pushes_over_limit(visited.len() + next.len())?;
                used_links.insert(link.reference.id.clone(), link.clone());
                if visited.contains(&other.id) {
                    continue;
                }
                if link.validity.current_applicability != CurrentApplicability::Applicable {
                    if !unknown_due_to_stale.iter().any(|seen| seen.id == other.id) {
                        unknown_due_to_stale.push(other.clone());
                    }
                    continue;
                }
                visited.insert(other.id.clone());
                let mut candidate_path = path.clone();
                candidate_path.push(other.clone());
                impacted.push(other.clone());
                paths.push(candidate_path.clone());
                next.push((other.clone(), candidate_path));
            }
        }
        frontier = next;
    }

    let completeness = if unknown_due_to_stale.is_empty() {
        Completeness::Complete
    } else {
        Completeness::Partial
    };
    Ok(TraceImpact {
        impacted,
        links: used_links.into_values().collect(),
        paths,
        unknown_due_to_stale,
        completeness,
    })
}

/// Find design gaps (§35.1, TAPI-09).
pub fn gaps(requirements: &[RequirementEntry], links: &[TraceLink]) -> GapReport {
    let mut missing_implementation = Vec::new();
    let mut missing_tests: Vec<Ref> = Vec::new();
    let mut unverifiable = Vec::new();
    let mut coverage_incomplete = false;

    for requirement in requirements {
        if !requirement.authorised || !requirement.in_scope {
            coverage_incomplete = true;
            continue;
        }
        let implements: Vec<&TraceLink> = links
            .iter()
            .filter(|link| link.kind == TraceKind::Implements)
            .filter(|link| link.target.id == requirement.reference.id)
            .filter(|link| !link.state.is_terminal())
            .collect();
        if implements.is_empty() {
            missing_implementation.push(requirement.reference.clone());
            continue;
        }
        for link in implements {
            let implementation = &link.source;
            let verified = links.iter().any(|candidate| {
                candidate.kind == TraceKind::Verifies
                    && candidate.target.id == implementation.id
                    && !candidate.state.is_terminal()
            });
            if !verified
                && !missing_tests
                    .iter()
                    .any(|seen| seen.id == implementation.id)
            {
                missing_tests.push(implementation.clone());
            }
        }
    }

    let mut stale_links = Vec::new();
    for link in links {
        if link.state == TraceState::Stale
            || link.validity.current_applicability == CurrentApplicability::RequiresRevalidation
        {
            stale_links.push(link.reference.clone());
        }
        if link.state == TraceState::Proposed || link.evidence.is_empty() {
            unverifiable.push(link.reference.clone());
        }
    }

    GapReport {
        missing_implementation,
        missing_tests,
        stale_links,
        unverifiable,
        coverage_incomplete,
    }
}

/// Explain a claim from the links around `refs` (§35.1, TAPI-11).
pub fn explain(
    claim: &str,
    refs: &[Ref],
    links: &[TraceLink],
    readable: &BTreeSet<String>,
    max_tokens: u32,
) -> Result<EvidenceReport> {
    if claim.trim().is_empty() {
        return Err(TraceError::InvalidArgument {
            field: "claim".to_string(),
            reason: "must be non-empty".to_string(),
        });
    }
    if max_tokens == 0 {
        return Err(TraceError::InvalidArgument {
            field: "max_tokens".to_string(),
            reason: "must be greater than zero".to_string(),
        });
    }
    let mut omitted_due_to_access = 0u32;
    let mut relevant: Vec<&TraceLink> = Vec::new();
    for reference in refs {
        if !readable.contains(reference.id.as_str()) {
            omitted_due_to_access += 1;
            continue;
        }
        for link in links {
            if (link.source.id == reference.id || link.target.id == reference.id)
                && !relevant
                    .iter()
                    .any(|seen| seen.reference.id == link.reference.id)
            {
                relevant.push(link);
            }
        }
    }
    relevant.sort_by(|left, right| left.reference.id.cmp(&right.reference.id));

    let estimate = claim.split_whitespace().count() as u32
        + 20 * relevant.len() as u32
        + 10 * relevant
            .iter()
            .map(|link| link.evidence.len() as u32)
            .sum::<u32>();
    if estimate > max_tokens {
        return Err(TraceError::ContextBudgetExceeded {
            requested: estimate,
            remaining: max_tokens,
        });
    }

    let mut evidence: Vec<Evidence> = Vec::new();
    let mut contradicted = false;
    let mut supported = false;
    for link in &relevant {
        if link.evidence.is_empty() {
            continue;
        }
        let counted = matches!(link.state, TraceState::Accepted | TraceState::Validated);
        if !counted {
            continue;
        }
        if link.kind == TraceKind::Contradicts {
            contradicted = true;
        } else {
            supported = true;
        }
        for item in &link.evidence {
            if !evidence
                .iter()
                .any(|seen| seen.evidence_id == item.evidence_id)
            {
                evidence.push(item.clone());
            }
        }
    }

    let conclusion = if contradicted {
        Conclusion::Contradicted
    } else if supported {
        Conclusion::Supported
    } else {
        Conclusion::Insufficient
    };
    let completeness = if omitted_due_to_access > 0 {
        Completeness::Partial
    } else {
        Completeness::Complete
    };
    Ok(EvidenceReport {
        claim: claim.to_string(),
        links: relevant.into_iter().cloned().collect(),
        evidence,
        conclusion,
        omitted_due_to_access,
        completeness,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::HistoricalValidity;

    fn reference(id: &str, kind: &str) -> Ref {
        Ref::new(id, kind, "rev-1")
    }

    fn link(
        id: &str,
        source: &Ref,
        target: &Ref,
        kind: TraceKind,
        state: TraceState,
        applicability: CurrentApplicability,
        index: IndexFreshness,
    ) -> TraceLink {
        TraceLink {
            reference: Ref::new(id, "trace_link", "\"1\""),
            source: source.clone(),
            target: target.clone(),
            kind,
            state,
            evidence: vec![Evidence {
                evidence_id: format!("{id}-e"),
                source_ref: format!("artifact:sha256:{id}"),
                confidence: 0.9,
            }],
            rationale: None,
            created_at: "0".into(),
            verified_at: Some("0".into()),
            source_revision: source.revision.clone(),
            target_revision: target.revision.clone(),
            etag: "\"1\"".into(),
            validity: ValidityVector {
                historical_validity: HistoricalValidity::Valid,
                current_applicability: applicability,
                index_freshness: index,
                asserted_source_revision: source.revision.clone(),
                observed_head_revision: Some(source.revision.clone()),
                assessed_at: "0".into(),
                evidence_refs: Vec::new(),
            },
        }
    }

    fn requirement(id: &str, authorised: bool) -> RequirementEntry {
        RequirementEntry {
            reference: reference(id, "requirement"),
            authorised,
            in_scope: true,
        }
    }

    #[test]
    fn coverage_counts_only_applicable_verified_links() {
        let requirements = vec![
            requirement("req:covered", true),
            requirement("req:uncovered", true),
            requirement("req:hidden", false),
        ];
        let links = vec![link(
            "link:1",
            &reference("code:a", "code"),
            &reference("req:covered", "requirement"),
            TraceKind::Implements,
            TraceState::Accepted,
            CurrentApplicability::Applicable,
            IndexFreshness::Current,
        )];
        let report = coverage(&CoverageInputs {
            requirements: &requirements,
            links: &links,
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: true,
            include_unindexed: false,
            snapshot_id: "snap-1".into(),
        });
        assert_eq!(report.denominator, 2);
        assert_eq!(report.numerator, 1);
        assert_eq!(report.coverage_ratio, Some(0.5));
        assert_eq!(report.covered.len(), 1);
        assert_eq!(report.uncovered.len(), 1);
        assert_eq!(report.excluded_count, 1);
        assert!(report.coverage_incomplete);
        assert_eq!(report.snapshot_id, "snap-1");
    }

    #[test]
    fn an_empty_denominator_has_no_ratio() {
        let requirements = vec![requirement("req:hidden", false)];
        let report = coverage(&CoverageInputs {
            requirements: &requirements,
            links: &[],
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: true,
            include_unindexed: false,
            snapshot_id: "snap".into(),
        });
        assert_eq!(report.denominator, 0);
        assert_eq!(report.numerator, 0);
        assert_eq!(
            report.coverage_ratio, None,
            "an empty denominator must not report 100%"
        );
        assert!(report.coverage_incomplete);
    }

    #[test]
    fn stale_or_lagging_links_become_unknown_not_covered() {
        let requirements = vec![requirement("req:1", true)];
        let stale = link(
            "link:1",
            &reference("code:a", "code"),
            &reference("req:1", "requirement"),
            TraceKind::Implements,
            TraceState::Stale,
            CurrentApplicability::RequiresRevalidation,
            IndexFreshness::Current,
        );
        let report = coverage(&CoverageInputs {
            requirements: &requirements,
            links: std::slice::from_ref(&stale),
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: false,
            include_unindexed: false,
            snapshot_id: "snap".into(),
        });
        assert_eq!(report.covered.len(), 0);
        assert_eq!(report.unknown.len(), 1);
        assert!(report.coverage_incomplete);

        let lagging = link(
            "link:2",
            &reference("code:a", "code"),
            &reference("req:1", "requirement"),
            TraceKind::Implements,
            TraceState::Accepted,
            CurrentApplicability::Applicable,
            IndexFreshness::Lagging,
        );
        let report = coverage(&CoverageInputs {
            requirements: &requirements,
            links: std::slice::from_ref(&lagging),
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: true,
            include_unindexed: false,
            snapshot_id: "snap".into(),
        });
        assert_eq!(report.unknown.len(), 1);

        let report = coverage(&CoverageInputs {
            requirements: &requirements,
            links: &[lagging],
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: true,
            include_unindexed: true,
            snapshot_id: "snap".into(),
        });
        assert_eq!(report.covered.len(), 1, "lagging counts when allowed");
    }

    #[test]
    fn accepted_only_ignores_validated_links() {
        let requirements = vec![requirement("req:1", true)];
        let validated = link(
            "link:1",
            &reference("code:a", "code"),
            &reference("req:1", "requirement"),
            TraceKind::Implements,
            TraceState::Validated,
            CurrentApplicability::Applicable,
            IndexFreshness::Current,
        );
        let strict = coverage(&CoverageInputs {
            requirements: &requirements,
            links: std::slice::from_ref(&validated),
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: true,
            include_unindexed: false,
            snapshot_id: "snap".into(),
        });
        assert_eq!(strict.covered.len(), 0);
        let relaxed = coverage(&CoverageInputs {
            requirements: &requirements,
            links: &[validated],
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: false,
            include_unindexed: false,
            snapshot_id: "snap".into(),
        });
        assert_eq!(relaxed.covered.len(), 1);
    }

    #[test]
    fn impact_walks_the_graph_and_stops_at_stale_links() {
        let requirement = reference("req:1", "requirement");
        let code = reference("code:a", "code");
        let test = reference("test:1", "test");
        let links = vec![
            link(
                "link:1",
                &code,
                &requirement,
                TraceKind::Implements,
                TraceState::Accepted,
                CurrentApplicability::Applicable,
                IndexFreshness::Current,
            ),
            link(
                "link:2",
                &test,
                &code,
                TraceKind::Verifies,
                TraceState::Accepted,
                CurrentApplicability::RequiresRevalidation,
                IndexFreshness::Current,
            ),
        ];
        let result = impact(std::slice::from_ref(&requirement), &links, 3, &[]).unwrap();
        assert_eq!(result.impacted.len(), 1);
        assert_eq!(result.impacted[0].id, "code:a");
        assert_eq!(result.unknown_due_to_stale.len(), 1);
        assert_eq!(result.unknown_due_to_stale[0].id, "test:1");
        assert_eq!(result.completeness, Completeness::Partial);
        assert_eq!(result.paths.len(), 1);
        assert_eq!(result.paths[0].len(), 2);
    }

    #[test]
    fn impact_depth_and_arguments_are_bounded() {
        let requirement = reference("req:1", "requirement");
        let code = reference("code:a", "code");
        let test = reference("test:1", "test");
        let links = vec![
            link(
                "link:1",
                &code,
                &requirement,
                TraceKind::Implements,
                TraceState::Accepted,
                CurrentApplicability::Applicable,
                IndexFreshness::Current,
            ),
            link(
                "link:2",
                &test,
                &code,
                TraceKind::Verifies,
                TraceState::Accepted,
                CurrentApplicability::Applicable,
                IndexFreshness::Current,
            ),
        ];
        let shallow = impact(std::slice::from_ref(&requirement), &links, 1, &[]).unwrap();
        assert_eq!(shallow.impacted.len(), 1);
        assert_eq!(shallow.completeness, Completeness::Complete);
        let deep = impact(&[requirement], &links, 2, &[]).unwrap();
        assert_eq!(deep.impacted.len(), 2);

        assert_eq!(
            impact(&[], &links, 1, &[]).unwrap_err().code(),
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            impact(&[reference("req:1", "requirement")], &links, 0, &[])
                .unwrap_err()
                .code(),
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            impact(&[reference("req:1", "requirement")], &links, 99, &[])
                .unwrap_err()
                .code(),
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn gaps_report_missing_implementation_missing_tests_and_stale_links() {
        let requirements = vec![requirement("req:1", true), requirement("req:2", true)];
        let code = reference("code:a", "code");
        let links = vec![link(
            "link:1",
            &code,
            &reference("req:1", "requirement"),
            TraceKind::Implements,
            TraceState::Accepted,
            CurrentApplicability::RequiresRevalidation,
            IndexFreshness::Current,
        )];
        let report = gaps(&requirements, &links);
        assert_eq!(report.missing_implementation.len(), 1);
        assert_eq!(report.missing_implementation[0].id, "req:2");
        assert_eq!(report.missing_tests.len(), 1);
        assert_eq!(report.missing_tests[0].id, "code:a");
        assert_eq!(report.stale_links.len(), 1);
        assert!(!report.coverage_incomplete);
    }

    #[test]
    fn a_hidden_requirement_makes_the_gap_analysis_incomplete() {
        let requirements = vec![requirement("req:1", false)];
        let report = gaps(&requirements, &[]);
        assert!(report.coverage_incomplete);
    }

    #[test]
    fn explain_distinguishes_supported_contradicted_and_insufficient() {
        let requirement = reference("req:1", "requirement");
        let code = reference("code:a", "code");
        let readable: BTreeSet<String> = [requirement.id.clone(), code.id.clone()]
            .into_iter()
            .collect();

        let support = link(
            "link:1",
            &code,
            &requirement,
            TraceKind::Implements,
            TraceState::Accepted,
            CurrentApplicability::Applicable,
            IndexFreshness::Current,
        );
        let report = explain(
            "the requirement is implemented",
            std::slice::from_ref(&requirement),
            std::slice::from_ref(&support),
            &readable,
            10_000,
        )
        .unwrap();
        assert_eq!(report.conclusion, Conclusion::Supported);
        assert_eq!(report.completeness, Completeness::Complete);
        assert_eq!(report.omitted_due_to_access, 0);

        let contradict = link(
            "link:2",
            &code,
            &requirement,
            TraceKind::Contradicts,
            TraceState::Accepted,
            CurrentApplicability::Applicable,
            IndexFreshness::Current,
        );
        let report = explain(
            "claim",
            std::slice::from_ref(&requirement),
            std::slice::from_ref(&contradict),
            &readable,
            10_000,
        )
        .unwrap();
        assert_eq!(report.conclusion, Conclusion::Contradicted);

        let proposed = link(
            "link:3",
            &code,
            &requirement,
            TraceKind::Implements,
            TraceState::Proposed,
            CurrentApplicability::Applicable,
            IndexFreshness::Current,
        );
        let report = explain(
            "claim",
            &[requirement],
            std::slice::from_ref(&proposed),
            &readable,
            10_000,
        )
        .unwrap();
        assert_eq!(report.conclusion, Conclusion::Insufficient);
    }

    #[test]
    fn explain_reports_omitted_refs_and_token_budget() {
        let requirement = reference("req:1", "requirement");
        let hidden = reference("req:hidden", "requirement");
        let readable: BTreeSet<String> = [requirement.id.clone()].into_iter().collect();
        let report = explain("claim", &[requirement, hidden], &[], &readable, 10_000).unwrap();
        assert_eq!(report.omitted_due_to_access, 1);
        assert_eq!(report.completeness, Completeness::Partial);

        assert_eq!(
            explain("the requirement is implemented", &[], &[], &readable, 1)
                .unwrap_err()
                .code(),
            "CONTEXT_BUDGET_EXCEEDED"
        );
        assert_eq!(
            explain("  ", &[], &[], &readable, 10).unwrap_err().code(),
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            explain("claim", &[], &[], &readable, 0).unwrap_err().code(),
            "INVALID_ARGUMENT"
        );
    }
}
