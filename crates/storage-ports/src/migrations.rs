//! The shipped canonical migrations.
//!
//! Two steps exist today, and both are real transformations of canonical data:
//!
//! - **v1** is the baseline. It writes nothing; reaching v1 means "this store is
//!   a v1 canonical store", which is what a freshly created store already is.
//! - **v2** stamps `schema_version` into every metadata record so a later reader
//!   can tell records written before the migration from records written after
//!   it. It pages with the store's own cursor and CAS-writes each record, so a
//!   concurrently updated record fails loudly instead of being overwritten.

use serde_json::{json, Value};
use ukma_contracts::ScopeRef;

use crate::error::{PortResult, StorageError};
use crate::migration::Migration;
use crate::ports::MetadataPort;

/// Page size used while walking canonical records.
const PAGE: usize = 128;

/// Canonical schema version of a store written before versioning existed.
///
/// Such a store already satisfies the v1 contract (records carry no
/// `schema_version` field), so the runner starts from v1, not v0.
pub const fn legacy_canonical_version() -> u32 {
    1
}

/// Canonical schema version 1 (baseline).
pub struct Baseline;
impl Migration for Baseline {
    fn version(&self) -> u32 {
        1
    }

    fn name(&self) -> &'static str {
        "baseline"
    }

    fn apply(&self, _store: &dyn MetadataPort, _scope: &ScopeRef) -> PortResult<()> {
        Ok(())
    }
}

/// Canonical schema version 2: stamp `schema_version: 2` into every record.
pub struct StampSchemaVersion;

impl Migration for StampSchemaVersion {
    fn version(&self) -> u32 {
        2
    }

    fn name(&self) -> &'static str {
        "stamp-schema-version"
    }

    fn apply(&self, store: &dyn MetadataPort, scope: &ScopeRef) -> PortResult<()> {
        let mut cursor: Option<String> = None;
        loop {
            let (page, next) = store.list(scope, "", PAGE, cursor.as_deref())?;
            for entry in page {
                let Value::Object(mut object) = entry.value.clone() else {
                    return Err(StorageError::InvalidArgument {
                        field: "canonical value",
                        reason: format!("`{}` is not a JSON object", entry.key),
                    });
                };
                if object.get("schema_version").and_then(Value::as_u64) == Some(2) {
                    continue;
                }
                object.insert("schema_version".into(), json!(2));
                store.put_cas(
                    scope,
                    &entry.key,
                    Value::Object(object),
                    Some(entry.revision),
                )?;
            }
            match next {
                Some(cursor_next) => cursor = Some(cursor_next),
                None => return Ok(()),
            }
        }
    }
}

/// Every migration this build ships, in ascending version order.
pub fn canonical_migrations() -> Vec<&'static dyn Migration> {
    static BASELINE: Baseline = Baseline;
    static STAMP: StampSchemaVersion = StampSchemaVersion;
    vec![&BASELINE, &STAMP]
}
