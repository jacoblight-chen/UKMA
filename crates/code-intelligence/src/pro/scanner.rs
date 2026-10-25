//! Shared scanner + cross-cutting behaviour (CodeGraph Pro parity).
//!
//! [`ProScanner`] runs per-line detectors over the indexed corpus, honouring
//! the cross-cutting behaviour: the path filter (test / production scoping),
//! the compile gate (`check_compile_gates` / `DEFENSIVE_GATED_OFF`), and the
//! telemetry block (a `path_filter` + `compile_gate` pair).

use std::collections::BTreeMap;

use super::finding::{Finding, Severity};

/// Cross-cutting scan configuration (CodeGraph parity).
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanConfig {
    /// Skip test files (`include_tests == false`).
    pub skip_tests: bool,
    /// Treat test files as production (`treat_as_production == true`).
    pub treat_as_production: bool,
    /// The compile gate is off (`check_compile_gates == DEFENSIVE_GATED_OFF`).
    pub compile_gate_off: bool,
}

/// Whether the compile gate applies (a gate is enforced unless it is
/// `DEFENSIVE_GATED_OFF`).
pub fn compile_gate(config: &ScanConfig) -> bool {
    !config.compile_gate_off
}

/// Whether a path is a test / spec file.
pub fn is_test_path(path: &str) -> bool {
    let lowered = path.to_lowercase();
    lowered.contains("test")
        || lowered.contains("spec")
        || lowered.contains("_test")
        || lowered.contains(".test.")
        || lowered.contains(".spec.")
}

/// The path filter: a path is in scope unless it is a test file that should
/// be skipped (and is not treated as production).
pub fn path_filter(path: &str, config: &ScanConfig) -> bool {
    if is_test_path(path) {
        if config.treat_as_production {
            return true;
        }
        if config.skip_tests {
            return false;
        }
    }
    true
}

/// The telemetry block: a `path_filter` + `compile_gate` pair (CodeGraph
/// telemetry parity).
pub const TELEMETRY_BLOCK: (&str, &str) = ("path_filter", "compile_gate");

/// A shared scanner over the indexed corpus (one entry per file).
pub struct ProScanner {
    files: BTreeMap<String, String>,
    config: ScanConfig,
}

impl ProScanner {
    /// A scanner over the corpus with a cross-cutting configuration.
    pub fn new(files: BTreeMap<String, String>, config: ScanConfig) -> Self {
        Self { files, config }
    }

    /// The cross-cutting configuration.
    pub fn config(&self) -> &ScanConfig {
        &self.config
    }

    /// The in-scope files (after the path filter).
    pub fn scoped_files(&self) -> Vec<&String> {
        self.files
            .keys()
            .filter(|path| path_filter(path, &self.config))
            .collect()
    }

    /// The lines of a file.
    pub fn lines(&self, path: &str) -> Option<Vec<&str>> {
        self.files
            .get(path)
            .map(|content| content.lines().collect())
    }

    /// Run a per-line detector over the in-scope files, returning findings.
    /// The detector receives `(path, line, 1-based line number)` and returns
    /// an optional message (a `None` means no finding on that line).
    pub fn scan_lines<F>(&self, rule: &str, severity: Severity, mut detect: F) -> Vec<Finding>
    where
        F: FnMut(&str, &str, usize) -> Option<String>,
    {
        let mut findings = Vec::new();
        for path in self.scoped_files() {
            let lines = self.lines(path).unwrap_or_default();
            for (i, line) in lines.iter().enumerate() {
                if let Some(message) = detect(path, line, i + 1) {
                    findings.push(Finding::new(
                        rule,
                        severity,
                        path,
                        (i + 1) as u32,
                        &message,
                        Some(line.trim().to_string()),
                    ));
                }
            }
        }
        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("src/prod.rs".to_string(), "let a = 1;\n".to_string());
        m.insert(
            "tests/unit_test.rs".to_string(),
            "fn test_a() {}\n".to_string(),
        );
        m
    }

    #[test]
    fn the_path_filter_skips_tests_only_when_configured() {
        let config = ScanConfig {
            skip_tests: true,
            ..ScanConfig::default()
        };
        let scanner = ProScanner::new(files(), config);
        let scoped: Vec<&str> = scanner.scoped_files().iter().map(|s| s.as_str()).collect();
        assert_eq!(scoped, vec!["src/prod.rs"]);
    }

    #[test]
    fn treat_as_production_keeps_test_files_in_scope() {
        let config = ScanConfig {
            skip_tests: true,
            treat_as_production: true,
            ..ScanConfig::default()
        };
        let scanner = ProScanner::new(files(), config);
        assert_eq!(scanner.scoped_files().len(), 2);
    }

    #[test]
    fn the_compile_gate_reflects_the_defensive_gate_off() {
        assert!(compile_gate(&ScanConfig::default()));
        assert!(!compile_gate(&ScanConfig {
            compile_gate_off: true,
            ..ScanConfig::default()
        }));
    }

    #[test]
    fn scan_lines_reports_each_detected_line() {
        let scanner = ProScanner::new(files(), ScanConfig::default());
        let findings = scanner.scan_lines("marker", Severity::Warning, |_, line, _| {
            if line.contains("a") {
                Some("contains `a`".to_string())
            } else {
                None
            }
        });
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].rule, "marker");
    }
}
