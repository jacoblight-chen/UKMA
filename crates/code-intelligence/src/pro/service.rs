//! `ProService` — the 27 CodeGraph Pro operations (compatible, not ported).
//!
//! Each operation is a focused, real detector over the indexed corpus
//! (files + symbols), sharing the scanner / suppression / SARIF foundation.
//! Security detectors are heuristic (line / token based), which is the honest
//! boundary for a static, offline analysis — they flag candidates, they do not
//! prove vulnerabilities. Every operation requires `code:read`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use ukma_contracts::RequestContext;

use super::finding::{to_sarif, Finding, Severity};
use super::scanner::{ProScanner, ScanConfig};
use super::suppress::filter_suppressed;
use super::vuln::{ecosystem_for, DependencyRef, OfflineVulnerabilitySource, VulnerabilitySource};
use crate::service::{CodeAuthorizer, ScopeAuthorizer, SCOPE_READ};
use crate::{CodeError, CodeResult, Symbol};

/// The per-revision corpus the Pro operations run against.
#[derive(Debug, Clone, Default)]
pub struct RevisionCorpus {
    /// File path -> content.
    pub files: BTreeMap<String, String>,
    /// Indexed symbols.
    pub symbols: Vec<Symbol>,
}

/// Pro state (per-revision corpora).
#[derive(Debug, Default)]
pub struct ProState {
    revisions: HashMap<String, RevisionCorpus>,
}

/// The 27 Pro operations.
pub struct ProService {
    authorizer: Arc<dyn CodeAuthorizer>,
    state: Mutex<ProState>,
    vulnerability_source: Arc<dyn VulnerabilitySource>,
}

impl Default for ProService {
    fn default() -> Self {
        Self::new()
    }
}

impl ProService {
    /// A service with the default scope authorizer and the offline
    /// vulnerability source (no findings; the manifest is reported).
    pub fn new() -> Self {
        Self {
            authorizer: Arc::new(ScopeAuthorizer),
            state: Mutex::new(ProState::default()),
            vulnerability_source: Arc::new(OfflineVulnerabilitySource),
        }
    }

    /// Replace the vulnerability source (for example the OSV online adapter,
    /// enabled by `--online`).
    pub fn with_vulnerability_source(mut self, source: Arc<dyn VulnerabilitySource>) -> Self {
        self.vulnerability_source = source;
        self
    }

    /// Replace the authorizer.
    pub fn with_authorizer(mut self, authorizer: Arc<dyn CodeAuthorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    /// Register a corpus for a revision (the scan target).
    pub fn index(
        &self,
        revision: &str,
        files: BTreeMap<String, String>,
        symbols: Vec<Symbol>,
    ) -> CodeResult<()> {
        let mut state = self.lock();
        state
            .revisions
            .insert(revision.to_string(), RevisionCorpus { files, symbols });
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProState> {
        self.state.lock().expect("pro state poisoned")
    }

    /// Require `code:read` and resolve a revision's corpus.
    fn corpus(&self, ctx: &RequestContext, revision: &str) -> CodeResult<RevisionCorpus> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        state
            .revisions
            .get(revision)
            .cloned()
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("revision {revision}"),
            })
    }

    fn scanner(&self, corpus: &RevisionCorpus, config: ScanConfig) -> ProScanner {
        ProScanner::new(corpus.files.clone(), config)
    }

    fn config(&self, _ctx: &RequestContext) -> ScanConfig {
        // The cross-cutting flags ride on the context's scopes / consistency;
        // the default is the inclusive, gate-on configuration.
        ScanConfig::default()
    }

    fn report(&self, operation: &str, revision: &str, findings: &[Finding]) -> serde_json::Value {
        serde_json::json!({
            "operation": operation,
            "revision": revision,
            "count": findings.len(),
            "findings": findings,
        })
    }

    // -- code operations ---------------------------------------------------

    /// `analyze_coupling`: for each symbol, how many distinct other files
    /// reference its name.
    pub fn analyze_coupling(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let rows: Vec<serde_json::Value> = corpus
            .symbols
            .iter()
            .map(|symbol| {
                let def_file = &symbol.definition.file_id;
                let mut referencing: BTreeMap<&str, ()> = BTreeMap::new();
                for (path, content) in &corpus.files {
                    if path == def_file {
                        continue;
                    }
                    if content.to_lowercase().contains(&symbol.name.to_lowercase()) {
                        referencing.insert(path, ());
                    }
                }
                serde_json::json!({
                    "symbol": symbol.name,
                    "file": def_file,
                    "referenced_in": referencing.len(),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "operation": "analyze_coupling",
            "revision": revision,
            "count": rows.len(),
            "coupling": rows,
        }))
    }

    /// `find_unused_code`: symbols whose name appears only at the definition
    /// (no other occurrence in the corpus).
    pub fn find_unused_code(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let mut findings = Vec::new();
        for symbol in &corpus.symbols {
            let mut occurrences = 0usize;
            for content in corpus.files.values() {
                let lowered = content.to_lowercase();
                let needle = symbol.name.to_lowercase();
                occurrences += lowered.matches(&needle).count();
            }
            if occurrences <= 1 {
                findings.push(Finding::new(
                    "unused-code",
                    Severity::Info,
                    &symbol.definition.file_id,
                    symbol.definition.line_start,
                    &format!("`{}` appears to be unused", symbol.name),
                    None,
                ));
            }
        }
        Ok(self.report("find_unused_code", revision, &findings))
    }

    /// `find_duplicates`: normalised lines that occur more than once in a file.
    pub fn find_duplicates(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let mut findings = Vec::new();
        for (path, content) in &corpus.files {
            let mut seen: HashMap<String, usize> = HashMap::new();
            for (i, line) in content.lines().enumerate() {
                let normalized = line.trim().to_lowercase();
                if normalized.is_empty() {
                    continue;
                }
                let count = seen.entry(normalized).or_insert(0);
                *count += 1;
                if *count == 2 {
                    findings.push(Finding::new(
                        "duplicate-code",
                        Severity::Info,
                        path,
                        (i + 1) as u32,
                        "duplicated line",
                        Some(line.trim().to_string()),
                    ));
                }
            }
        }
        Ok(self.report("find_duplicates", revision, &findings))
    }

    /// `find_similar`: symbols sharing a name prefix (first 4 chars).
    pub fn find_similar(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for symbol in &corpus.symbols {
            let prefix = symbol.name.get(..4).unwrap_or(&symbol.name).to_lowercase();
            groups.entry(prefix).or_default().push(symbol.name.clone());
        }
        let pairs: Vec<serde_json::Value> = groups
            .into_iter()
            .filter(|(_, names)| names.len() >= 2)
            .flat_map(|(_, names)| {
                let mut out = Vec::new();
                for i in 0..names.len() {
                    for j in (i + 1)..names.len() {
                        out.push(serde_json::json!({ "a": names[i], "b": names[j] }));
                    }
                }
                out
            })
            .collect();
        Ok(serde_json::json!({
            "operation": "find_similar",
            "revision": revision,
            "count": pairs.len(),
            "pairs": pairs,
        }))
    }

    /// `cluster_symbols`: symbols grouped by their definition file.
    pub fn cluster_symbols(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let mut clusters: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for symbol in &corpus.symbols {
            clusters
                .entry(symbol.definition.file_id.clone())
                .or_default()
                .push(symbol.name.clone());
        }
        let rows: Vec<serde_json::Value> = clusters
            .into_iter()
            .map(|(file, symbols)| {
                serde_json::json!({ "file": file, "count": symbols.len(), "symbols": symbols })
            })
            .collect();
        Ok(serde_json::json!({
            "operation": "cluster_symbols",
            "revision": revision,
            "count": rows.len(),
            "clusters": rows,
        }))
    }

    /// `compare_symbols`: compare two symbols by name.
    pub fn compare_symbols(
        &self,
        ctx: &RequestContext,
        revision: &str,
        a: &str,
        b: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let find = |name: &str| -> CodeResult<Option<&Symbol>> {
            Ok(corpus.symbols.iter().find(|s| s.name == name))
        };
        match (find(a)?, find(b)?) {
            (Some(sa), Some(sb)) => Ok(serde_json::json!({
                "operation": "compare_symbols",
                "revision": revision,
                "a": describe_symbol(sa),
                "b": describe_symbol(sb),
                "same_file": sa.definition.file_id == sb.definition.file_id,
                "same_kind": sa.kind == sb.kind,
            })),
            (Some(_), None) => Err(CodeError::NotFound {
                resource: format!("symbol {b}"),
            }),
            (None, Some(_)) => Err(CodeError::NotFound {
                resource: format!("symbol {a}"),
            }),
            (None, None) => Err(CodeError::NotFound {
                resource: format!("symbol {a}"),
            }),
        }
    }

    /// `cross_project_search`: search a query across all indexed revisions.
    pub fn cross_project_search(
        &self,
        ctx: &RequestContext,
        query: &str,
    ) -> CodeResult<serde_json::Value> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if query.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "query",
                reason: "must be non-empty".into(),
            });
        }
        let state = self.lock();
        let needle = query.to_lowercase();
        let mut matches = Vec::new();
        for (revision, corpus) in &state.revisions {
            for (path, content) in &corpus.files {
                for (i, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(&needle) {
                        matches.push(serde_json::json!({
                            "revision": revision,
                            "file": path,
                            "line": i + 1,
                            "snippet": line.trim(),
                        }));
                    }
                }
            }
        }
        Ok(serde_json::json!({
            "operation": "cross_project_search",
            "query": query,
            "count": matches.len(),
            "matches": matches,
        }))
    }

    /// `mine_git_history`: per-file line counts for a revision.
    pub fn mine_git_history(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let rows: Vec<serde_json::Value> = corpus
            .files
            .iter()
            .map(|(path, content)| {
                serde_json::json!({ "file": path, "lines": content.lines().count() })
            })
            .collect();
        Ok(serde_json::json!({
            "operation": "mine_git_history",
            "revision": revision,
            "count": rows.len(),
            "files": rows,
        }))
    }

    /// `mine_git_history_for_file`: a file's line count across all revisions.
    pub fn mine_git_history_for_file(
        &self,
        ctx: &RequestContext,
        file: &str,
    ) -> CodeResult<serde_json::Value> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let mut rows = Vec::new();
        for (revision, corpus) in &state.revisions {
            if let Some(content) = corpus.files.get(file) {
                rows.push(serde_json::json!({
                    "revision": revision,
                    "lines": content.lines().count(),
                }));
            }
        }
        if rows.is_empty() {
            return Err(CodeError::NotFound {
                resource: format!("file {file} in history"),
            });
        }
        Ok(serde_json::json!({
            "operation": "mine_git_history_for_file",
            "file": file,
            "count": rows.len(),
            "history": rows,
        }))
    }

    /// `search_git_history`: find a query in the line history across revisions.
    pub fn search_git_history(
        &self,
        ctx: &RequestContext,
        query: &str,
    ) -> CodeResult<serde_json::Value> {
        self.cross_project_search(ctx, query)
    }

    // -- security operations -----------------------------------------------

    /// `scan_security`: the aggregate security scan (all detectors).
    pub fn scan_security(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let config = self.config(ctx);
        let scanner = self.scanner(&corpus, config);
        let mut findings = Vec::new();
        findings.extend(Self::detect_control_flow(&scanner));
        findings.extend(Self::detect_data_flow(&scanner));
        findings.extend(Self::detect_injection(&scanner));
        findings.extend(Self::detect_crypto(&scanner));
        findings.extend(Self::detect_misconfig(&scanner));
        findings.extend(Self::detect_secrets_entropy(&scanner));
        findings.extend(Self::detect_unchecked_returns(&scanner));
        findings.extend(Self::detect_resource_leaks(&corpus));
        findings.extend(Self::detect_input_validation(&scanner));
        findings.extend(Self::detect_error_exposure(&scanner));
        findings.extend(Self::detect_search_path(&scanner));
        findings.extend(Self::detect_iac(&corpus));
        findings.extend(Self::detect_licenses(&corpus));
        let findings = filter_suppressed(findings, &corpus.files);
        Ok(self.report("scan_security", revision, &findings))
    }

    /// `security_control_flow`: dynamic / dangerous execution sinks.
    pub fn security_control_flow(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_control_flow(&scanner), &corpus.files);
        Ok(self.report("security_control_flow", revision, &findings))
    }

    /// `security_trace_data_flow`: input-to-sink on the same line.
    pub fn security_trace_data_flow(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_data_flow(&scanner), &corpus.files);
        Ok(self.report("security_trace_data_flow", revision, &findings))
    }

    /// `security_generate_sbom`: a CycloneDX SBOM from the lockfiles.
    pub fn security_generate_sbom(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let components = Self::dependencies(&corpus);
        Ok(serde_json::json!({
            "bomFormat": "CycloneDX",
            "specVersion": "1.5",
            "version": 1,
            "components": components,
        }))
    }

    /// `security_audit_deps`: the dependency list, plus an OSV audit. When the
    /// wired vulnerability source is online (feature `osv` + `--online`), each
    /// dependency is queried against OSV; on any transport failure it degrades
    /// to the offline manifest. Offline (the default) reports the manifest.
    pub fn security_audit_deps(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let components = Self::dependencies(&corpus);
        let online = self.vulnerability_source.is_online();
        let mut osv: Vec<serde_json::Value> = Vec::new();
        let mut online_available = online;
        if online {
            for (path, dep) in Self::dependency_refs(&corpus) {
                let dep = DependencyRef {
                    name: dep.name,
                    version: dep.version,
                    ecosystem: ecosystem_for(&path),
                };
                match self.vulnerability_source.audit(&dep) {
                    Ok(findings) => {
                        for v in findings {
                            osv.push(serde_json::json!({
                                "id": v.id,
                                "summary": v.summary,
                            }));
                        }
                    }
                    Err(_) => online_available = false,
                }
            }
        }
        let note = if !online {
            "OSV lookup is a network operation; the offline manifest list is reported".to_string()
        } else if !online_available {
            "OSV lookup unavailable (transport failure); the offline manifest list is reported"
                .to_string()
        } else if osv.is_empty() {
            "OSV lookup returned no known vulnerabilities".to_string()
        } else {
            "OSV lookup matched known vulnerabilities".to_string()
        };
        Ok(serde_json::json!({
            "operation": "security_audit_deps",
            "revision": revision,
            "count": components.as_array().map(|a| a.len()).unwrap_or(0),
            "dependencies": components,
            "vulnerabilities": osv,
            "vulnerability_count": osv.len(),
            "online_available": online_available,
            "note": note,
        }))
    }

    /// `security_check_unchecked_returns`: calls without visible error handling.
    pub fn security_check_unchecked_returns(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_unchecked_returns(&scanner), &corpus.files);
        Ok(self.report("security_check_unchecked_returns", revision, &findings))
    }

    /// `security_check_resource_leaks`: an open without a matching close.
    pub fn security_check_resource_leaks(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let findings = filter_suppressed(Self::detect_resource_leaks(&corpus), &corpus.files);
        Ok(self.report("security_check_resource_leaks", revision, &findings))
    }

    /// `security_check_misconfig`: insecure configuration values.
    pub fn security_check_misconfig(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_misconfig(&scanner), &corpus.files);
        Ok(self.report("security_check_misconfig", revision, &findings))
    }

    /// `security_check_input_validation`: unvalidated input reaching a sink.
    pub fn security_check_input_validation(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_input_validation(&scanner), &corpus.files);
        Ok(self.report("security_check_input_validation", revision, &findings))
    }

    /// `security_check_error_exposure`: internals leaking through errors.
    pub fn security_check_error_exposure(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_error_exposure(&scanner), &corpus.files);
        Ok(self.report("security_check_error_exposure", revision, &findings))
    }

    /// `security_scan_iac`: insecure IaC (Docker / K8s / Terraform).
    pub fn security_scan_iac(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let findings = filter_suppressed(Self::detect_iac(&corpus), &corpus.files);
        Ok(self.report("security_scan_iac", revision, &findings))
    }

    /// `security_check_licenses`: copyleft license markers.
    pub fn security_check_licenses(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let findings = filter_suppressed(Self::detect_licenses(&corpus), &corpus.files);
        Ok(self.report("security_check_licenses", revision, &findings))
    }

    /// `security_check_secrets_entropy`: high-entropy string candidates.
    pub fn security_check_secrets_entropy(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_secrets_entropy(&scanner), &corpus.files);
        Ok(self.report("security_check_secrets_entropy", revision, &findings))
    }

    /// `security_detect_injection`: concatenation into query / command sinks.
    pub fn security_detect_injection(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_injection(&scanner), &corpus.files);
        Ok(self.report("security_detect_injection", revision, &findings))
    }

    /// `security_check_search_path`: relative dynamic-load paths (CWE-426/427).
    pub fn security_check_search_path(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_search_path(&scanner), &corpus.files);
        Ok(self.report("security_check_search_path", revision, &findings))
    }

    /// `security_check_crypto`: weak / deprecated crypto primitives.
    pub fn security_check_crypto(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let findings = filter_suppressed(Self::detect_crypto(&scanner), &corpus.files);
        Ok(self.report("security_check_crypto", revision, &findings))
    }

    /// `security_export_sarif`: the SARIF 2.1.0 export of the aggregate scan.
    pub fn security_export_sarif(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let mut findings = Vec::new();
        findings.extend(Self::detect_control_flow(&scanner));
        findings.extend(Self::detect_data_flow(&scanner));
        findings.extend(Self::detect_injection(&scanner));
        findings.extend(Self::detect_crypto(&scanner));
        findings.extend(Self::detect_misconfig(&scanner));
        findings.extend(Self::detect_secrets_entropy(&scanner));
        findings.extend(Self::detect_unchecked_returns(&scanner));
        findings.extend(Self::detect_resource_leaks(&corpus));
        findings.extend(Self::detect_input_validation(&scanner));
        findings.extend(Self::detect_error_exposure(&scanner));
        findings.extend(Self::detect_search_path(&scanner));
        findings.extend(Self::detect_iac(&corpus));
        findings.extend(Self::detect_licenses(&corpus));
        let findings = filter_suppressed(findings, &corpus.files);
        let sarif = to_sarif(&findings, "ukma-pro");
        Ok(serde_json::json!({
            "operation": "security_export_sarif",
            "revision": revision,
            "count": findings.len(),
            "sarif": sarif,
        }))
    }

    /// `security_orchestrated_scan`: run the detector suite across the
    /// requested engines and roll up the findings (CodeGraph
    /// `security_orchestrated_scan` parity). UKMA has one built-in engine,
    /// `native` (the heuristic detector suite, reported as analyzer
    /// `codegraph`); `codegraph` is accepted as an alias for it.
    pub fn security_orchestrated_scan(
        &self,
        ctx: &RequestContext,
        revision: &str,
        engines: &[String],
        severity: Option<&str>,
        path_filter: Option<&str>,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let scanner = self.scanner(&corpus, self.config(ctx));
        let allowed = ["native", "codegraph"];
        for engine in engines {
            if !allowed.contains(&engine.as_str()) {
                return Err(CodeError::InvalidArgument {
                    field: "engines",
                    reason: format!("unknown engine `{engine}` (expected `native` or `codegraph`)"),
                });
            }
        }
        let min_severity = match severity {
            Some(raw) => Some(match raw {
                "info" => Severity::Info,
                "warning" => Severity::Warning,
                "error" => Severity::Error,
                "critical" => Severity::Critical,
                other => {
                    return Err(CodeError::InvalidArgument {
                        field: "severity",
                        reason: format!("unknown severity `{other}`"),
                    })
                }
            }),
            None => None,
        };
        // The raw detector output (pre-merge, pre-suppression).
        let mut raw = Vec::new();
        raw.extend(Self::detect_control_flow(&scanner));
        raw.extend(Self::detect_data_flow(&scanner));
        raw.extend(Self::detect_injection(&scanner));
        raw.extend(Self::detect_crypto(&scanner));
        raw.extend(Self::detect_misconfig(&scanner));
        raw.extend(Self::detect_secrets_entropy(&scanner));
        raw.extend(Self::detect_unchecked_returns(&scanner));
        raw.extend(Self::detect_resource_leaks(&corpus));
        raw.extend(Self::detect_input_validation(&scanner));
        raw.extend(Self::detect_error_exposure(&scanner));
        raw.extend(Self::detect_search_path(&scanner));
        raw.extend(Self::detect_iac(&corpus));
        raw.extend(Self::detect_licenses(&corpus));
        let findings_pre_merge = raw.len();
        let merged = filter_suppressed(raw, &corpus.files);
        let findings: Vec<Finding> = merged
            .into_iter()
            .filter(|f| {
                if let Some(min) = &min_severity {
                    f.severity >= *min
                } else {
                    true
                }
            })
            .filter(|f| match path_filter {
                Some(needle) if !needle.is_empty() => f.file.contains(needle),
                _ => true,
            })
            .collect();
        let actionable = findings
            .iter()
            .filter(|f| {
                matches!(
                    f.severity,
                    Severity::Warning | Severity::Error | Severity::Critical
                )
            })
            .count();
        let mut agreement: BTreeMap<String, u64> = BTreeMap::new();
        for finding in &findings {
            *agreement.entry(finding.rule.clone()).or_insert(0) += 1;
        }
        let reachability = findings
            .iter()
            .map(|f| f.file.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        Ok(serde_json::json!({
            "operation": "security_orchestrated_scan",
            "revision": revision,
            "analyzer": "orchestrated",
            "engines": [{ "name": "codegraph", "findings_pre_merge": findings_pre_merge }],
            "total": findings.len(),
            "actionable": actionable,
            "findings": findings,
            "agreement_distribution": agreement,
            "reachability": reachability,
            "path_filter": path_filter,
            "severity": severity,
        }))
    }

    /// `security_codeql_warmup`: pre-build the analysis corpus for a revision
    /// (the UKMA-native equivalent of warming a CodeQL database) and report
    /// its shape and readiness.
    pub fn security_codeql_warmup(
        &self,
        ctx: &RequestContext,
        revision: &str,
    ) -> CodeResult<serde_json::Value> {
        let corpus = self.corpus(ctx, revision)?;
        let line_count: u64 = corpus
            .files
            .values()
            .map(|content| content.lines().count() as u64)
            .sum();
        let languages: Vec<String> = corpus
            .files
            .keys()
            .filter_map(|path| path.rsplit_once('.').map(|(_, ext)| ext.to_string()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(serde_json::json!({
            "operation": "security_codeql_warmup",
            "revision": revision,
            "ready": true,
            "analyzer": "codeql",
            "engine": "native",
            "file_count": corpus.files.len(),
            "symbol_count": corpus.symbols.len(),
            "line_count": line_count,
            "languages": languages,
        }))
    }

    // -- detectors ---------------------------------------------------------

    fn detect_control_flow(scanner: &ProScanner) -> Vec<Finding> {
        let sinks = [
            "eval(",
            "exec(",
            "system(",
            "spawn(",
            "function(",
            "shell_execute(",
            "child_process",
            "subprocess",
            "runtime.getruntime().exec",
            "runtime.exec",
            "processbuilder",
            "exec_sync",
            "execsync(",
            "os.popen(",
            "popen(",
        ];
        scanner.scan_lines("control-flow", Severity::Warning, |_, line, _| {
            let lowered = line.to_lowercase();
            if sinks.iter().any(|s| lowered.contains(s)) {
                Some("dynamic / dangerous execution sink".to_string())
            } else {
                None
            }
        })
    }

    fn detect_data_flow(scanner: &ProScanner) -> Vec<Finding> {
        let sources = ["req.", "req ", "input", "args", "params", "query"];
        let sinks = ["query(", "exec(", "innerhtml", "write(", "system("];
        scanner.scan_lines("data-flow", Severity::Warning, |_, line, _| {
            let lowered = line.to_lowercase();
            if sources.iter().any(|s| lowered.contains(s))
                && sinks.iter().any(|s| lowered.contains(s))
            {
                Some("untrusted input reaches a sink on the same line".to_string())
            } else {
                None
            }
        })
    }

    fn detect_injection(scanner: &ProScanner) -> Vec<Finding> {
        let sinks = ["query", "sql", "exec", "system", "innerhtml", "template"];
        scanner.scan_lines("injection", Severity::Error, |_, line, _| {
            let lowered = line.to_lowercase();
            let in_sink = sinks.iter().any(|s| lowered.contains(s));
            let has_concat = line.contains('+') && (line.contains('"') || line.contains('\''));
            if has_concat && in_sink {
                return Some("string concatenation into a query / command sink".to_string());
            }
            let has_interpolation = (lowered.contains("format!(")
                || lowered.contains(".format(")
                || lowered.contains("f\"")
                || lowered.contains("f'"))
                && in_sink;
            if has_interpolation {
                Some("string interpolation into a query / command sink".to_string())
            } else {
                None
            }
        })
    }

    fn detect_crypto(scanner: &ProScanner) -> Vec<Finding> {
        let weak = [
            "md5(",
            "sha1(",
            "des ",
            "des(",
            "ecb",
            "arcfour",
            "rc4",
            "math.random",
            "insecure_random",
            "md5",
            "sha1",
            "sha224",
            "ripemd",
            "hmac-md5",
            "hmacmd5",
            "crypt(",
            "crypt_md5",
            "xor_cipher",
            "base64_decode_as_key",
        ];
        scanner.scan_lines("weak-crypto", Severity::Error, |_, line, _| {
            let lowered = line.to_lowercase();
            if weak.iter().any(|w| lowered.contains(w)) {
                Some("weak / deprecated crypto primitive".to_string())
            } else {
                None
            }
        })
    }

    fn detect_misconfig(scanner: &ProScanner) -> Vec<Finding> {
        let bad = [
            "debug = true",
            "debug=true",
            "0.0.0.0",
            "verify_tls = false",
            "verify_tls=false",
            "insecure",
            "allow_any",
            "password = \"",
            "******\"",
            "tls = false",
            "tls=false",
            "ssl = false",
            "ssl=false",
            "https = false",
            "https=false",
            "allow_unauthenticated",
            "permitall",
            "noauth",
            "auth_enabled = false",
            "auth_enabled=false",
            "cors: \"*\"",
        ];
        scanner.scan_lines("misconfig", Severity::Warning, |_, line, _| {
            let lowered = line.to_lowercase();
            if bad.iter().any(|b| lowered.contains(b)) {
                Some("insecure configuration value".to_string())
            } else {
                None
            }
        })
    }

    fn detect_secrets_entropy(scanner: &ProScanner) -> Vec<Finding> {
        scanner.scan_lines("secrets-entropy", Severity::Warning, |_, line, _| {
            let lowered = line.to_lowercase();
            if lowered.contains("private key-----") || lowered.contains("begin openpgp private key")
            {
                return Some("private key material present".to_string());
            }
            if line.contains("AKIA")
                || line.contains("xoxb-")
                || line.contains("xoxp-")
                || line.contains("ghp_")
                || line.contains("gho_")
                || line.contains("glpat-")
            {
                return Some("known secret / API-key prefix".to_string());
            }
            for quoted in extract_quoted(line) {
                if quoted.len() >= 16 && shannon_entropy(&quoted) > 4.0 {
                    return Some("high-entropy string (possible secret)".to_string());
                }
            }
            None
        })
    }

    fn detect_unchecked_returns(scanner: &ProScanner) -> Vec<Finding> {
        let handled = [
            "?", ".ok(", ".check", "assert", "expect(", "catch", "try ", "recover",
        ];
        let definitions = [
            "fn ", "pub fn", "impl ", "struct ", "trait ", "enum ", "type ",
        ];
        scanner.scan_lines("unchecked-return", Severity::Info, |_, line, _| {
            let trimmed = line.trim();
            let lowered = line.to_lowercase();
            let is_definition = definitions.iter().any(|d| lowered.starts_with(d));
            let has_call = trimmed.contains('(') && trimmed.contains(')');
            if has_call && !is_definition && !handled.iter().any(|h| lowered.contains(h)) {
                Some("call return value not visibly handled".to_string())
            } else {
                None
            }
        })
    }

    fn detect_resource_leaks(corpus: &RevisionCorpus) -> Vec<Finding> {
        let opens = [
            "open(",
            "fopen(",
            "connect(",
            "new file(",
            "create(",
            "socket(",
            "pipe(",
        ];
        let closes = [
            "close(",
            "fclose(",
            "disconnect(",
            "dispose(",
            "drop(",
            "shutdown(",
            "reset(",
        ];
        let mut findings = Vec::new();
        for (path, content) in &corpus.files {
            let lowered = content.to_lowercase();
            if opens.iter().any(|o| lowered.contains(o))
                && !closes.iter().any(|c| lowered.contains(c))
            {
                let line_no = content
                    .lines()
                    .enumerate()
                    .find(|(_, l)| {
                        let l = l.to_lowercase();
                        opens.iter().any(|o| l.contains(o))
                    })
                    .map(|(i, _)| (i + 1) as u32)
                    .unwrap_or(1);
                findings.push(Finding::new(
                    "resource-leak",
                    Severity::Warning,
                    path,
                    line_no,
                    "resource opened without a visible close",
                    None,
                ));
            }
        }
        findings
    }

    fn detect_input_validation(scanner: &ProScanner) -> Vec<Finding> {
        let inputs = ["req.", "req ", "args", "input", "params"];
        let sinks = ["query(", "exec(", "insert(", "update(", "write("];
        let guards = ["validate", "sanitize", "check", "assert", "escape", "trim"];
        scanner.scan_lines("input-validation", Severity::Warning, |_, line, _| {
            let lowered = line.to_lowercase();
            if inputs.iter().any(|i| lowered.contains(i))
                && sinks.iter().any(|s| lowered.contains(s))
                && !guards.iter().any(|g| lowered.contains(g))
            {
                Some("unvalidated input reaches a sink".to_string())
            } else {
                None
            }
        })
    }

    fn detect_error_exposure(scanner: &ProScanner) -> Vec<Finding> {
        let leak = [
            ".message",
            ".stack",
            "e.message",
            "error.message",
            "stacktrace",
            "traceback",
            "print_stack",
        ];
        let expose = ["throw", "log(", "return", "send(", "respond("];
        scanner.scan_lines("error-exposure", Severity::Warning, |_, line, _| {
            let lowered = line.to_lowercase();
            if leak.iter().any(|l| lowered.contains(l))
                && expose.iter().any(|e| lowered.contains(e))
            {
                Some("internal error details may be exposed".to_string())
            } else {
                None
            }
        })
    }

    fn detect_search_path(scanner: &ProScanner) -> Vec<Finding> {
        scanner.scan_lines("search-path", Severity::Warning, |_, line, _| {
            let lowered = line.to_lowercase();
            let relative_load = (lowered.contains("load(\"")
                || lowered.contains("require(\"./")
                || lowered.contains("dlopen(\""))
                && !line.contains("/");
            if relative_load {
                Some("relative dynamic-load path (CWE-426/427)".to_string())
            } else {
                None
            }
        })
    }

    fn detect_iac(corpus: &RevisionCorpus) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (path, content) in &corpus.files {
            if !is_iac_file(path) {
                continue;
            }
            let markers = [
                ("user root", "container runs as root"),
                ("--privileged", "privileged container"),
                (":latest", "unpinned image tag"),
                ("0.0.0.0", "service bound to all interfaces"),
            ];
            for (i, line) in content.lines().enumerate() {
                let l = line.to_lowercase();
                for (marker, message) in markers {
                    if l.contains(marker) {
                        findings.push(Finding::new(
                            "iac",
                            Severity::Warning,
                            path,
                            (i + 1) as u32,
                            message,
                            Some(line.trim().to_string()),
                        ));
                    }
                }
            }
        }
        findings
    }

    fn detect_licenses(corpus: &RevisionCorpus) -> Vec<Finding> {
        let copyleft = ["gpl", "agpl", "lgpl", "mpl"];
        let mut findings = Vec::new();
        for (path, content) in &corpus.files {
            if !path.to_lowercase().contains("license") && !path.to_lowercase().ends_with(".md") {
                continue;
            }
            for (i, line) in content.lines().enumerate() {
                let l = line.to_lowercase();
                if copyleft.iter().any(|c| l.contains(c)) {
                    findings.push(Finding::new(
                        "copyleft-license",
                        Severity::Warning,
                        path,
                        (i + 1) as u32,
                        "copyleft license marker",
                        Some(line.trim().to_string()),
                    ));
                }
            }
        }
        findings
    }

    /// Extract the dependency list from the known lockfiles (8, CodeGraph
    /// `security_generate_sbom` parity).
    fn dependencies(corpus: &RevisionCorpus) -> serde_json::Value {
        const LOCKFILES: &[&str] = &[
            "package-lock.json",
            "yarn.lock",
            "cargo.lock",
            "go.sum",
            "requirements.txt",
            "pom.xml",
            "package.json",
            ".npmrc",
        ];
        let mut components = Vec::new();
        for (path, content) in &corpus.files {
            if !LOCKFILES.iter().any(|lf| path.ends_with(lf)) {
                continue;
            }
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            components.push(serde_json::json!({
                "type": "application",
                "name": name,
                "purl": format!("pkg:generic/{name}"),
            }));
            // For the line-based manifests, also list each dependency line.
            for line in content.lines() {
                let dep = parse_dependency_line(path, line);
                if let Some((dep_name, version)) = dep {
                    components.push(serde_json::json!({
                        "type": "library",
                        "name": dep_name,
                        "version": version,
                    }));
                }
            }
        }
        serde_json::Value::Array(components)
    }

    /// The auditable dependencies (name + version) from the line-based
    /// manifests, paired with their source path (for ecosystem inference).
    fn dependency_refs(corpus: &RevisionCorpus) -> Vec<(String, DependencyRef)> {
        const LOCKFILES: &[&str] = &[
            "package-lock.json",
            "yarn.lock",
            "cargo.lock",
            "go.sum",
            "requirements.txt",
            "pom.xml",
            "package.json",
            ".npmrc",
        ];
        let mut refs = Vec::new();
        for (path, content) in &corpus.files {
            if !LOCKFILES.iter().any(|lf| path.ends_with(lf)) {
                continue;
            }
            for line in content.lines() {
                if let Some((dep_name, version)) = parse_dependency_line(path, line) {
                    refs.push((
                        path.clone(),
                        DependencyRef {
                            name: dep_name,
                            version,
                            ecosystem: String::new(),
                        },
                    ));
                }
            }
        }
        refs
    }
}

fn describe_symbol(symbol: &Symbol) -> serde_json::Value {
    serde_json::json!({
        "name": symbol.name,
        "kind": symbol.kind,
        "file": symbol.definition.file_id,
        "line_start": symbol.definition.line_start,
        "line_end": symbol.definition.line_end,
    })
}

fn is_iac_file(path: &str) -> bool {
    let lowered = path.to_lowercase();
    lowered.contains("dockerfile")
        || lowered.contains("docker-compose")
        || lowered.ends_with(".tf")
        || lowered.contains("k8s")
        || lowered.contains("kubernetes")
        || (lowered.ends_with(".yaml") && lowered.contains("kind:"))
        || lowered.ends_with(".yml") && lowered.contains("kind:")
}

fn parse_dependency_line(path: &str, line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
        return None;
    }
    // `name version` (requirements / go.sum style) or `name@version`.
    if path.ends_with("requirements.txt") || path.ends_with("go.sum") {
        let mut parts = trimmed.split_whitespace();
        let name = parts.next()?;
        let version = parts.next().unwrap_or("");
        if name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Some((name.to_string(), version.to_string()));
        }
        return None;
    }
    if trimmed.contains('@') {
        let (name, version) = trimmed.split_once('@')?;
        return Some((name.trim().to_string(), version.to_string()));
    }
    None
}

fn extract_quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            if j < bytes.len() {
                out.push(line[start..j].to_string());
                i = j + 1;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }
    out
}

fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut freq: HashMap<char, f64> = HashMap::new();
    for c in s.chars() {
        *freq.entry(c).or_insert(0.0) += 1.0;
    }
    let len = s.chars().count() as f64;
    freq.values().fold(0.0, |acc, &count| {
        let p = count / len;
        acc - p * p.log2()
    })
}
