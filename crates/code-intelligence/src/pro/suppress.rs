//! Suppression markers (CodeGraph 25-marker parity).
//!
//! A finding on a line carrying one of the registered suppression markers is
//! suppressed (excluded from results). The check is case-insensitive and
//! substring-based, matching the common linter / security opt-out idioms.

/// The registered suppression markers (lower-cased; 25, CodeGraph parity).
pub const SUPPRESSION_MARKERS: &[&str] = &[
    "nosec",           // # nosec / // nosec / #nosec / <!-- nosec -->
    "nolint",          // // NOLINT / #nolint
    "noqa",            // # noqa / # flake8: noqa / # ruff: noqa
    "disable",         // rubocop / pylint / mypy / ruff / fmt disable
    "eslint-disable",  // // eslint-disable(-next-line)
    "ts-ignore",       // @ts-ignore
    "ts-expect-error", // @ts-expect-error
    "type: ignore",    // # type: ignore
    "pylint: disable",
    "mypy: disable",
    "rubocop: disable",
    "ruff: noqa",
    "flake8: noqa",
    "fmt: off",
    "spell:disable-next-line",
    "spell:disable",
    "suppressed",
    "checkstyle:off",
    "checkstyle: off",
    "cwe: suppress",
    "snyk: ignore",
    "dependabot: ignore",
    "audit: ignore",
    "cargo: audit-ignore",
    "go: security-ignore",
];

/// Whether a line of source carries a registered suppression marker.
pub fn is_suppressed(line: &str) -> bool {
    let lowered = line.to_lowercase();
    SUPPRESSION_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// Filter out suppressed findings: a finding is dropped when the source line
/// at its location carries a registered suppression marker.
pub fn filter_suppressed(
    findings: Vec<crate::pro::finding::Finding>,
    files: &std::collections::BTreeMap<String, String>,
) -> Vec<crate::pro::finding::Finding> {
    findings
        .into_iter()
        .filter(|f| {
            !files
                .get(&f.file)
                .and_then(|content| content.lines().nth(f.line.saturating_sub(1) as usize))
                .map(is_suppressed)
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_set_has_25_registered_markers() {
        assert_eq!(SUPPRESSION_MARKERS.len(), 25);
    }

    #[test]
    fn common_opt_out_idioms_are_recognised() {
        assert!(is_suppressed("let x = 1; // nosec"));
        assert!(is_suppressed("let y = 2; # noqa"));
        assert!(is_suppressed("let z = 3; // NOLINT"));
        assert!(is_suppressed("@ts-ignore\nlet w: any = 4;"));
        assert!(!is_suppressed("let plain = 5;"));
    }
}
