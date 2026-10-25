//! Pro (`ProService`) 5-path contract matrix.
//!
//! Each representative operation is exercised through the sub-cases the
//! custom rules require: `valid` (positive), `invalid_schema` (negative /
//! control), `cross_scope` (authorization), `revision_conflict` (version
//! binding), and `failure` (error paths). The 27 operations share these
//! paths; the security aggregate plus the code / SBOM / SARIF operations are
//! the representative set.

#![cfg(feature = "parser-rust")]

use std::collections::BTreeMap;

use code_intelligence::pro::{DependencyRef, OsVulnerability, ProService, VulnerabilitySource};
use code_intelligence::{CodeError, CodeLocation, CodeResult, Resolution, Symbol, SymbolKind};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const REV_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REV_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

const VULN_RS: &str = "\
fn main() {
    let x = eval(input);
    let h = md5(data);
    let key = \"aB3xY7zQ9mK2pL5vW8cR1dF4gH6jN0sT\";
    // let suppressed = \"bC4yZ8aR0nL3qM6uX9dS2eG5hJ7kO1vW\"; // nosec
}
debug = true
let a = 1;
let a = 1;
";

const CLEAN_RS: &str = "\
fn hello() -> i32 {
    42
}
";

const B_ONLY_RS: &str = "\
// zebra_unique marker for the B revision
fn beta() -> i32 {
    1
}
";

fn read_ctx() -> RequestContext {
    RequestContext {
        request_id: "req-1".into(),
        trace_id: "trace-1".into(),
        scope: ScopeRef {
            tenant_id: "tenant-1".into(),
            project_id: "project-1".into(),
            repo_id: Some("repo-1".into()),
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: "agent-1".into(),
        scopes: vec!["code:read".to_string()],
        deadline_at: None,
        consistency: Consistency::Eventual,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

fn no_scope_ctx() -> RequestContext {
    let mut ctx = read_ctx();
    ctx.scopes.clear();
    ctx
}

fn symbol(name: &str, file: &str, line: u32) -> Symbol {
    Symbol {
        id: format!("sym-{name}"),
        name: name.to_string(),
        qualified_name: name.to_string(),
        kind: SymbolKind::Function,
        definition: CodeLocation {
            file_id: file.to_string(),
            revision: REV_A.to_string(),
            line_start: line,
            line_end: line,
            qualified_name: Some(name.to_string()),
        },
        language: "rust".into(),
        resolution: Resolution::Resolved,
        signature: None,
        visibility: None,
        evidence_ids: Vec::new(),
    }
}

fn vuln_corpus() -> (BTreeMap<String, String>, Vec<Symbol>) {
    let mut files = BTreeMap::new();
    files.insert("src/vuln.rs".to_string(), VULN_RS.to_string());
    let symbols = vec![
        symbol("main", "src/vuln.rs", 1),
        symbol("unused_fn", "src/vuln.rs", 5),
    ];
    (files, symbols)
}

fn clean_corpus() -> (BTreeMap<String, String>, Vec<Symbol>) {
    let mut files = BTreeMap::new();
    files.insert("src/clean.rs".to_string(), CLEAN_RS.to_string());
    let symbols = vec![symbol("hello", "src/clean.rs", 1)];
    (files, symbols)
}

fn index_a(svc: &ProService) {
    let (files, symbols) = vuln_corpus();
    svc.index(REV_A, files, symbols).expect("index accepted");
}

fn index_clean(svc: &ProService) {
    let (files, symbols) = clean_corpus();
    svc.index(REV_A, files, symbols).expect("index accepted");
}

// -- positive --------------------------------------------------------------

/// Positive: the aggregate scan flags the seeded issues.
#[test]
fn scan_security_flags_the_seeded_issues() {
    let svc = ProService::new();
    index_a(&svc);
    let report = svc
        .scan_security(&read_ctx(), REV_A)
        .expect("scan must succeed");
    assert!(
        report["count"].as_u64().unwrap() > 0,
        "the seeded corpus must produce findings"
    );
    // `eval(` is a control-flow sink; `md5(` is weak crypto; `debug = true`
    // is a misconfig.
    let findings = report["findings"].as_array().unwrap();
    let rules: Vec<&str> = findings
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"control-flow"), "eval( must be flagged");
    assert!(rules.contains(&"weak-crypto"), "md5( must be flagged");
    assert!(rules.contains(&"misconfig"), "debug = true must be flagged");
}

/// Positive (suppression): the `// nosec` high-entropy line is suppressed, so
/// exactly one high-entropy finding remains (the un-suppressed `key`).
#[test]
fn the_nosec_marker_suppresses_the_finding() {
    let svc = ProService::new();
    index_a(&svc);
    let report = svc
        .scan_security(&read_ctx(), REV_A)
        .expect("scan must succeed");
    let findings = report["findings"].as_array().unwrap();
    let high_entropy: Vec<&serde_json::Value> = findings
        .iter()
        .filter(|f| f["rule"] == "secrets-entropy")
        .collect();
    assert_eq!(
        high_entropy.len(),
        1,
        "only the un-suppressed high-entropy string is reported"
    );
}

// -- negative (control) ----------------------------------------------------

/// Negative (control): a clean corpus produces no findings.
#[test]
fn a_clean_corpus_produces_no_findings() {
    let svc = ProService::new();
    index_clean(&svc);
    let report = svc
        .scan_security(&read_ctx(), REV_A)
        .expect("scan must succeed");
    assert_eq!(
        report["count"].as_u64().unwrap(),
        0,
        "clean corpus is clean"
    );
}

// -- authorization ---------------------------------------------------------

/// Authorization: a caller without `code:read` is rejected.
#[test]
fn pro_operations_require_the_read_scope() {
    let svc = ProService::new();
    index_a(&svc);
    let error = svc
        .scan_security(&no_scope_ctx(), REV_A)
        .expect_err("a read-less caller must be rejected");
    assert_eq!(error.code(), "FORBIDDEN");
}

// -- version (revision binding) --------------------------------------------

/// Version: a word present only in `REV_B` is found there, not in `REV_A`.
#[test]
fn the_scan_is_bound_to_the_indexed_revision() {
    let svc = ProService::new();
    index_a(&svc);
    // Register a B revision with a distinct marker.
    let mut files = BTreeMap::new();
    files.insert("src/b_only.rs".to_string(), B_ONLY_RS.to_string());
    svc.index(REV_B, files, Vec::new()).expect("B indexed");
    let b = svc
        .cross_project_search(&read_ctx(), "zebra_unique")
        .expect("search must succeed");
    assert_eq!(
        b["count"].as_u64().unwrap(),
        1,
        "the B-only marker is found"
    );
    // `mine_git_history` for A does not list the B-only file.
    let a = svc
        .mine_git_history(&read_ctx(), REV_A)
        .expect("history must succeed");
    let listed: Vec<&str> = a["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    assert!(
        !listed.contains(&"src/b_only.rs"),
        "A history excludes the B file"
    );
}

// -- failure ---------------------------------------------------------------

/// Failure: an unknown revision is a not-found; an empty query is an invalid
/// argument; an unknown symbol in `compare_symbols` is a not-found.
#[test]
fn pro_failure_paths() {
    let svc = ProService::new();
    index_a(&svc);
    let missing = svc
        .scan_security(&read_ctx(), "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        .expect_err("an unknown revision must be rejected");
    assert_eq!(missing.code(), "NOT_FOUND");

    let bad_query = svc
        .cross_project_search(&read_ctx(), "   ")
        .expect_err("an empty query is invalid");
    assert_eq!(bad_query.code(), "INVALID_ARGUMENT");

    let no_symbol = svc
        .compare_symbols(&read_ctx(), REV_A, "nope", "main")
        .expect_err("an unknown symbol is a not-found");
    assert_eq!(no_symbol.code(), "NOT_FOUND");
}

// -- representative operations ----------------------------------------------

/// Positive: `find_unused_code` flags the unused symbol.
#[test]
fn find_unused_code_flags_the_unused_symbol() {
    let svc = ProService::new();
    index_a(&svc);
    let report = svc
        .find_unused_code(&read_ctx(), REV_A)
        .expect("scan must succeed");
    let names: Vec<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["message"].as_str())
        .collect();
    assert!(
        names.iter().any(|m| m.contains("unused_fn")),
        "the unused symbol is flagged: {names:?}"
    );
}

/// Positive: `find_duplicates` detects the duplicated line.
#[test]
fn find_duplicates_detects_the_duplicated_line() {
    let svc = ProService::new();
    index_a(&svc);
    let report = svc
        .find_duplicates(&read_ctx(), REV_A)
        .expect("scan must succeed");
    assert!(
        report["count"].as_u64().unwrap() >= 1,
        "the duplicated `let a = 1;` line is detected"
    );
}

/// Positive: `security_generate_sbom` emits a CycloneDX document.
#[test]
fn the_sbom_is_a_cyclonedx_document() {
    let svc = ProService::new();
    let mut files = BTreeMap::new();
    files.insert("src/vuln.rs".to_string(), VULN_RS.to_string());
    files.insert("requirements.txt".to_string(), "flask 2.0.0\n".to_string());
    svc.index(REV_A, files, Vec::new()).expect("index accepted");
    let sbom = svc
        .security_generate_sbom(&read_ctx(), REV_A)
        .expect("sbom must succeed");
    assert_eq!(sbom["bomFormat"], "CycloneDX");
    assert!(
        sbom["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "requirements.txt"),
        "the lockfile is a component"
    );
}

/// Positive: `security_export_sarif` emits a valid SARIF 2.1.0 document.
#[test]
fn the_sarif_export_is_a_valid_2_1_0_document() {
    let svc = ProService::new();
    index_a(&svc);
    let report = svc
        .security_export_sarif(&read_ctx(), REV_A)
        .expect("sarif must succeed");
    let sarif = &report["sarif"];
    assert_eq!(sarif["version"], "2.1.0");
    assert!(!sarif["runs"][0]["results"].as_array().unwrap().is_empty());
}

/// Positive: `analyze_coupling` and `cluster_symbols` produce the expected rows.
#[test]
fn the_code_analytics_operations_produce_rows() {
    let svc = ProService::new();
    index_a(&svc);
    let coupling = svc
        .analyze_coupling(&read_ctx(), REV_A)
        .expect("coupling must succeed");
    assert!(
        coupling["count"].as_u64().unwrap() >= 1,
        "coupling rows exist"
    );
    let clusters = svc
        .cluster_symbols(&read_ctx(), REV_A)
        .expect("clusters must succeed");
    assert!(
        clusters["count"].as_u64().unwrap() >= 1,
        "symbol clusters exist"
    );
}

// -- OSV online dependency audit (5-path) -----------------------------------

/// A vulnerability source that always reports one finding (online).
#[derive(Clone)]
struct StubVulnSource {
    finding: OsVulnerability,
}

impl Default for StubVulnSource {
    fn default() -> Self {
        Self {
            finding: OsVulnerability {
                id: "GHSA-test".into(),
                summary: "simulated finding".into(),
            },
        }
    }
}

impl VulnerabilitySource for StubVulnSource {
    fn audit(&self, _dep: &DependencyRef) -> CodeResult<Vec<OsVulnerability>> {
        Ok(vec![self.finding.clone()])
    }
    fn is_online(&self) -> bool {
        true
    }
}

/// A vulnerability source that always fails (simulated transport failure).
struct FailingVulnSource;

impl VulnerabilitySource for FailingVulnSource {
    fn audit(&self, _dep: &DependencyRef) -> CodeResult<Vec<OsVulnerability>> {
        Err(CodeError::ResourceLimit {
            detail: "simulated transport failure".into(),
        })
    }
    fn is_online(&self) -> bool {
        true
    }
}

/// A corpus that contains a line-based manifest with auditable dependencies.
fn lockfile_corpus() -> (BTreeMap<String, String>, Vec<Symbol>) {
    let mut files = BTreeMap::new();
    files.insert(
        "requirements.txt".to_string(),
        "flask 2.0.0\nrequests 2.28.0\n".to_string(),
    );
    (files, Vec::new())
}

fn index_lockfile(svc: &ProService) {
    let (files, symbols) = lockfile_corpus();
    svc.index(REV_A, files, symbols).expect("index accepted");
}

/// Positive: a wired online source surfaces its findings in the report.
#[test]
fn osv_positive_a_wired_source_surfaces_findings() {
    let svc =
        ProService::new().with_vulnerability_source(std::sync::Arc::new(StubVulnSource::default()));
    index_lockfile(&svc);
    let report = svc
        .security_audit_deps(&read_ctx(), REV_A)
        .expect("audit must succeed");
    assert_eq!(report["online_available"], true, "online was wired");
    assert!(
        report["dependencies"].as_array().unwrap().len() >= 2,
        "the manifest dependencies are listed"
    );
    assert!(
        report["vulnerability_count"].as_u64().unwrap() >= 1,
        "the stub finding is reported"
    );
}

/// Negative (offline): the default service reports the manifest with no
/// online lookup.
#[test]
fn osv_negative_offline_reports_the_manifest_without_lookup() {
    let svc = ProService::new();
    index_lockfile(&svc);
    let report = svc
        .security_audit_deps(&read_ctx(), REV_A)
        .expect("audit must succeed");
    assert_eq!(report["online_available"], false, "offline by default");
    assert_eq!(report["vulnerability_count"], 0, "no findings when offline");
    assert!(
        report["dependencies"].as_array().unwrap().len() >= 2,
        "the manifest is still reported"
    );
    assert!(
        report["note"]
            .as_str()
            .unwrap()
            .contains("network operation"),
        "the note names the offline fallback"
    );
}

/// Authorization: `security_audit_deps` requires `code:read`.
#[test]
fn osv_authorization_requires_the_code_read_scope() {
    let svc = ProService::new();
    index_lockfile(&svc);
    assert!(
        svc.security_audit_deps(&no_scope_ctx(), REV_A).is_err(),
        "no scope -> denied"
    );
    assert!(
        svc.security_audit_deps(&read_ctx(), REV_A).is_ok(),
        "with the scope -> allowed"
    );
}

/// Version: the report is bound to the requested revision.
#[test]
fn osv_version_the_report_is_bound_to_the_revision() {
    let svc = ProService::new();
    index_lockfile(&svc);
    let report = svc
        .security_audit_deps(&read_ctx(), REV_A)
        .expect("audit must succeed");
    assert_eq!(report["revision"], REV_A, "bound to the requested revision");
}

/// Failure (degrade): a failing online source degrades to the manifest rather
/// than hard-failing the operation.
#[test]
fn osv_failure_a_failing_source_degrades_to_the_manifest() {
    let svc = ProService::new().with_vulnerability_source(std::sync::Arc::new(FailingVulnSource));
    index_lockfile(&svc);
    let report = svc
        .security_audit_deps(&read_ctx(), REV_A)
        .expect("degrades instead of failing");
    assert_eq!(report["online_available"], false, "degraded to unavailable");
    assert_eq!(
        report["vulnerability_count"], 0,
        "no findings after failure"
    );
    assert!(
        report["dependencies"].as_array().unwrap().len() >= 2,
        "the manifest is still reported"
    );
    assert!(
        report["note"].as_str().unwrap().contains("unavailable"),
        "the note names the transport failure"
    );
}

// -- richer security heuristics (5-path) ------------------------------------

/// A corpus that exercises only the *new* heuristic patterns (no legacy
/// pattern is present), so a positive result proves the extended detectors.
const RICH_RS: &str = "\
fn main() {
    let _p = subprocess.call(cmd);
    let _k = sha224(data);
    let _c = ripemd160(data);
    let _cfg = \"tls=false\";
    let _aws = \"AKIAIOSFODNN7EXAMPLE\";
    let _db = conn.query(format!(\"x\"));
    let _e = log(err.stacktrace);
}
";

fn rich_corpus() -> (BTreeMap<String, String>, Vec<Symbol>) {
    let mut files = BTreeMap::new();
    files.insert("src/rich.rs".to_string(), RICH_RS.to_string());
    let symbols = vec![symbol("main", "src/rich.rs", 1)];
    (files, symbols)
}

fn index_rich(svc: &ProService) {
    let (files, symbols) = rich_corpus();
    svc.index(REV_A, files, symbols).expect("index accepted");
}

fn rules_of(report: &serde_json::Value) -> Vec<&str> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect()
}

/// Positive: the extended detectors flag the new patterns.
#[test]
fn richer_heuristics_flag_the_new_patterns() {
    let svc = ProService::new();
    index_rich(&svc);
    let report = svc
        .scan_security(&read_ctx(), REV_A)
        .expect("scan succeeds");
    let rules = rules_of(&report);
    assert!(rules.contains(&"control-flow"), "subprocess (new) flagged");
    assert!(
        rules.contains(&"weak-crypto"),
        "sha224/ripemd (new) flagged"
    );
    assert!(rules.contains(&"misconfig"), "tls=false (new) flagged");
    assert!(
        rules.contains(&"secrets-entropy"),
        "AKIA prefix (new) flagged"
    );
    assert!(
        rules.contains(&"injection"),
        "format! interpolation (new) flagged"
    );
    assert!(
        rules.contains(&"error-exposure"),
        "stacktrace (new) flagged"
    );
}

/// Negative: a clean corpus produces none of the security rules.
#[test]
fn richer_heuristics_are_silent_on_clean() {
    let svc = ProService::new();
    index_clean(&svc);
    let report = svc
        .scan_security(&read_ctx(), REV_A)
        .expect("scan succeeds");
    let rules = rules_of(&report);
    for rule in [
        "control-flow",
        "weak-crypto",
        "misconfig",
        "secrets-entropy",
        "injection",
        "error-exposure",
    ] {
        assert!(
            !rules.contains(&rule),
            "{rule} must not fire on a clean corpus"
        );
    }
}

/// Authorization: the aggregate scan requires `code:read`.
#[test]
fn richer_heuristics_authorization_requires_code_read() {
    let svc = ProService::new();
    index_rich(&svc);
    let error = svc
        .scan_security(&no_scope_ctx(), REV_A)
        .expect_err("a read-less caller must be rejected");
    assert_eq!(error.code(), "FORBIDDEN");
}

/// Version: the report is bound to the requested revision.
#[test]
fn richer_heuristics_are_bound_to_the_revision() {
    let svc = ProService::new();
    index_rich(&svc);
    let report = svc
        .scan_security(&read_ctx(), REV_A)
        .expect("scan succeeds");
    assert_eq!(report["revision"], REV_A, "bound to the requested revision");
}

/// Failure: an unknown revision is a not-found.
#[test]
fn richer_heuristics_failure_unknown_revision() {
    let svc = ProService::new();
    index_rich(&svc);
    let missing = svc
        .scan_security(&read_ctx(), "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        .expect_err("an unknown revision must be rejected");
    assert_eq!(missing.code(), "NOT_FOUND");
}
