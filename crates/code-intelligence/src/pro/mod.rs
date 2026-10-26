//! CodeGraph Pro — compatible (not ported) security + code analytics.
//!
//! A focused set of detectors over the indexed corpus, sharing a common
//! scanner, suppression, and SARIF foundation. Each operation is a real,
//! testable detector (not an empty stub) that honours the cross-cutting
//! behaviour: suppression markers, the compile gate, and test/production
//! scoping.

pub mod finding;
pub mod scanner;
pub mod service;
pub mod suppress;
pub mod vuln;
pub mod wire;

pub use finding::{to_sarif, Finding, Severity};
pub use scanner::{compile_gate, path_filter, ProScanner, ScanConfig, TELEMETRY_BLOCK};
pub use service::{ProService, ProState};
pub use suppress::{filter_suppressed, is_suppressed, SUPPRESSION_MARKERS};
#[cfg(feature = "osv")]
pub use vuln::OsVulnerabilitySource;
pub use vuln::{
    ecosystem_for, DependencyRef, OfflineVulnerabilitySource, OsVulnerability, VulnerabilitySource,
};
pub use wire::{
    ProCompareSymbolsRequest, ProFileRequest, ProOrchestratedScanRequest, ProQueryRequest,
    ProReport, ProRevisionRequest,
};
