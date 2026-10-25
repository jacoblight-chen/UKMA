//! Canonical schema versioning and migration (FM-006, §29.4).
//!
//! The canonical store is the authority for metadata and manifests, so its
//! on-disk shape may only change through a versioned, forward-only migration.
//! This module owns the *contract*: the schema version port, the migration
//! step trait, and a runner that fails closed.
//!
//! Invariants enforced by [`migrate`]:
//! - a store newer than the code understands is refused, never downgraded;
//! - the chain must be contiguous from the stored version to the target, so a
//!   missing step cannot be skipped silently;
//! - the version is advanced only *after* a step succeeds, so a crashed run
//!   resumes at the failed step instead of replaying a half-applied one;
//! - re-running against an up-to-date store is a no-op (`skipped`).

use serde::{Deserialize, Serialize};
use ukma_contracts::ScopeRef;

use crate::error::{PortResult, StorageError};
use crate::ports::MetadataPort;

/// Highest canonical schema version this build understands.
pub const CANONICAL_SCHEMA_VERSION: u32 = 2;

/// Persisted canonical schema version of a store.
pub trait SchemaVersionPort: Send + Sync {
    /// Version currently recorded by the store.
    fn schema_version(&self) -> PortResult<u32>;

    /// Record a new version durably. Only the migration runner should call this.
    fn set_schema_version(&self, version: u32) -> PortResult<()>;
}

/// One forward-only migration step.
pub trait Migration: Send + Sync {
    /// Version the store reaches after this step succeeds.
    fn version(&self) -> u32;

    /// Stable step name, recorded in the report.
    fn name(&self) -> &'static str;

    /// Transform the canonical data of one scope.
    fn apply(&self, store: &dyn MetadataPort, scope: &ScopeRef) -> PortResult<()>;
}

/// What a migration run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    /// Version the store had when the run started.
    pub from: u32,
    /// Version the store has when the run finished.
    pub to: u32,
    /// Steps applied, in order, as `v<version>:<name>`.
    pub applied: Vec<String>,
    /// True when the store was already at the target version.
    pub skipped: bool,
}

/// Migration failure that is not a port error, e.g. a step that refuses.
pub fn chain_broken(from: u32, next: u32) -> StorageError {
    StorageError::MigrationChainBroken { from, next }
}

/// Applies every pending step up to `target`.
///
/// # Errors
/// - [`StorageError::SchemaVersionUnsupported`] when the store is newer than `target`;
/// - [`StorageError::MigrationChainBroken`] when a needed step is missing;
/// - whatever the failing step returns (the version is then left untouched).
pub fn migrate(
    store: &(dyn MigrationStore + '_),
    scope: &ScopeRef,
    steps: &[&dyn Migration],
    target: u32,
) -> PortResult<MigrationReport> {
    let from = store.schema_version()?;
    if from > target {
        return Err(StorageError::SchemaVersionUnsupported {
            found: from,
            supported: target,
        });
    }
    if from == target {
        return Ok(MigrationReport {
            from,
            to: from,
            applied: Vec::new(),
            skipped: true,
        });
    }

    let mut planned: Vec<&dyn Migration> = steps
        .iter()
        .copied()
        .filter(|step| step.version() > from && step.version() <= target)
        .collect();
    planned.sort_by_key(|step| step.version());

    let mut expected = from + 1;
    for step in &planned {
        if step.version() != expected {
            return Err(chain_broken(from, step.version()));
        }
        expected += 1;
    }
    if expected != target + 1 {
        return Err(chain_broken(from, expected));
    }

    let mut applied = Vec::new();
    for step in planned {
        step.apply(store.metadata(), scope)?;
        store.set_schema_version(step.version())?;
        applied.push(format!("v{}:{}", step.version(), step.name()));
    }
    Ok(MigrationReport {
        from,
        to: target,
        applied,
        skipped: false,
    })
}

/// A store that can be migrated: metadata access plus a schema version.
pub trait MigrationStore: MetadataPort + SchemaVersionPort {
    /// Metadata view handed to each step.
    fn metadata(&self) -> &dyn MetadataPort;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;
    use crate::testing::InMemoryMetadata;

    fn scope() -> ScopeRef {
        ScopeRef {
            tenant_id: "tenant-a".into(),
            project_id: "project-a".into(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        }
    }

    struct CountingMigration {
        version: u32,
        name: &'static str,
        fail: bool,
        calls: AtomicU32,
    }

    impl Migration for CountingMigration {
        fn version(&self) -> u32 {
            self.version
        }

        fn name(&self) -> &'static str {
            self.name
        }

        fn apply(&self, store: &dyn MetadataPort, scope: &ScopeRef) -> PortResult<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(StorageError::Internal("step refused".into()));
            }
            store.put_cas(scope, "migrated", json!(self.version), None)?;
            Ok(())
        }
    }

    fn store_with_version(version: u32) -> InMemoryMetadata {
        let store = InMemoryMetadata::new();
        store.set_schema_version(version).unwrap();
        store
    }

    #[test]
    fn a_run_from_an_old_version_applies_every_step_in_order() {
        let store = store_with_version(0);
        let one = CountingMigration {
            version: 1,
            name: "one",
            fail: false,
            calls: AtomicU32::new(0),
        };
        let two = CountingMigration {
            version: 2,
            name: "two",
            fail: false,
            calls: AtomicU32::new(0),
        };
        let report = migrate(&store, &scope(), &[&one, &two], 2).unwrap();
        assert_eq!(report.from, 0);
        assert_eq!(report.to, 2);
        assert!(!report.skipped);
        assert_eq!(report.applied, vec!["v1:one", "v2:two"]);
        assert_eq!(one.calls.load(Ordering::SeqCst), 1);
        assert_eq!(two.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.schema_version().unwrap(), 2);
    }

    #[test]
    fn re_running_an_up_to_date_store_is_a_no_op() {
        let store = store_with_version(2);
        let two = CountingMigration {
            version: 2,
            name: "two",
            fail: false,
            calls: AtomicU32::new(0),
        };
        let report = migrate(&store, &scope(), &[&two], 2).unwrap();
        assert!(report.skipped);
        assert!(report.applied.is_empty());
        assert_eq!(two.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_newer_store_is_refused_instead_of_downgraded() {
        let store = store_with_version(3);
        let error = migrate(&store, &scope(), &[], 2).unwrap_err();
        assert_eq!(error.code(), "INVALID_SCHEMA");
        assert!(matches!(
            error,
            StorageError::SchemaVersionUnsupported {
                found: 3,
                supported: 2
            }
        ));
        assert_eq!(store.schema_version().unwrap(), 3);
    }

    #[test]
    fn a_missing_step_breaks_the_chain() {
        let store = store_with_version(0);
        let two = CountingMigration {
            version: 2,
            name: "two",
            fail: false,
            calls: AtomicU32::new(0),
        };
        let error = migrate(&store, &scope(), &[&two], 2).unwrap_err();
        assert_eq!(error.code(), "INVALID_SCHEMA");
        assert_eq!(store.schema_version().unwrap(), 0);
    }

    #[test]
    fn a_target_beyond_the_registered_steps_is_refused() {
        let store = store_with_version(0);
        let one = CountingMigration {
            version: 1,
            name: "one",
            fail: false,
            calls: AtomicU32::new(0),
        };
        let error = migrate(&store, &scope(), &[&one], 2).unwrap_err();
        assert_eq!(error.code(), "INVALID_SCHEMA");
    }

    #[test]
    fn a_failing_step_leaves_the_recorded_version_untouched() {
        let store = store_with_version(0);
        let failing = CountingMigration {
            version: 1,
            name: "failing",
            fail: true,
            calls: AtomicU32::new(0),
        };
        let error = migrate(&store, &scope(), &[&failing], 1).unwrap_err();
        assert_eq!(error.code(), "INTERNAL");
        assert_eq!(store.schema_version().unwrap(), 0);
    }
}
