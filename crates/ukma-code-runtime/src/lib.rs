//! Phase 6 executable-target prototype. No assertion of full CAPI closure.
//! Filesystem adapter is a working development backend, not production durability proof.
//!
//! **DEPRECATED (ADR-048, O-07, 2026-10-25).** This crate is superseded by
//! `crates/code-intelligence` (parsing/indexing) and `crates/adapters/*`
//! (persistence). It MUST NOT gain new functionality, and MUST be removed once
//! phase P3 completes. Its tests are NOT evidence for any release gate.
//!
//! Note: a crate-level `#![deprecated]` is intentionally NOT used because it
//! would also flag this crate's own internal use sites and break
//! `clippy -D warnings`; deprecation is recorded in `Cargo.toml`,
//! `docs/adr/ADR-048.md` and `docs/implementation-status.yaml` instead.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolRecord {
    pub id: String,
    pub tenant_id: String,
    pub project_id: String,
    pub repo_id: String,
    pub revision: String,
    pub name: String,
    pub kind: String,
    pub etag: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxEvent {
    pub event_id: u64,
    pub event_type: String,
    pub aggregate_id: String,
    pub revision: String,
    pub delivered: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdempotencyRecord {
    aggregate_id: String,
    payload_fingerprint: String,
    result: SymbolRecord,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    symbols: BTreeMap<String, SymbolRecord>,
    idempotency: BTreeMap<String, IdempotencyRecord>,
    outbox: Vec<OutboxEvent>,
    next_event_id: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Io(String),
    Serialization(String),
    NotFound,
    PermissionDenied,
    Conflict,
    Invalid(String),
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}
#[derive(Debug, Clone)]
pub struct WriteContext {
    pub tenant_id: String,
    pub project_id: String,
    pub idempotency_key: String,
    pub expected_etag: Option<u64>,
    pub can_index: bool,
}
#[derive(Debug, Clone)]
pub struct ReadContext {
    pub tenant_id: String,
    pub project_id: String,
    pub can_read: bool,
}

pub trait GraphStore: Send + Sync {
    fn upsert_symbol(
        &self,
        ctx: &WriteContext,
        symbol: SymbolRecord,
    ) -> Result<SymbolRecord, StoreError>;
    fn get_symbol(&self, ctx: &ReadContext, symbol_id: &str) -> Result<SymbolRecord, StoreError>;
}
pub trait EventOutbox: Send + Sync {
    fn pending(&self) -> Result<Vec<OutboxEvent>, StoreError>;
    fn mark_delivered(&self, event_id: u64) -> Result<(), StoreError>;
}
/// Atomic same-filesystem replace of one canonical state file; no cross-process lock.
/// Filesystem synchronization, NFS semantics and power-loss durability are NOT guaranteed.
#[derive(Clone)]
pub struct JsonFileAdapter {
    path: PathBuf,
    state: Arc<Mutex<Snapshot>>,
}
impl JsonFileAdapter {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let state = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            Snapshot::default()
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
        })
    }
    fn persist(&self, next: &Snapshot) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("pending");
        fs::write(&tmp, serde_json::to_vec_pretty(next)?)?;
        fs::rename(tmp, &self.path)?;
        Ok(())
    }
    pub fn all_events(&self) -> Result<Vec<OutboxEvent>, StoreError> {
        let guard = self
            .state
            .lock()
            .map_err(|_| StoreError::Io("poisoned".into()))?;
        Ok(guard.outbox.clone())
    }
}
impl GraphStore for JsonFileAdapter {
    fn upsert_symbol(
        &self,
        ctx: &WriteContext,
        mut symbol: SymbolRecord,
    ) -> Result<SymbolRecord, StoreError> {
        if !ctx.can_index
            || ctx.tenant_id != symbol.tenant_id
            || ctx.project_id != symbol.project_id
        {
            return Err(StoreError::PermissionDenied);
        }
        if ctx.idempotency_key.is_empty() || symbol.id.is_empty() || symbol.revision.is_empty() {
            return Err(StoreError::Invalid(
                "missing idempotency key, id or revision".into(),
            ));
        }
        let mut guard = self
            .state
            .lock()
            .map_err(|_| StoreError::Io("poisoned".into()))?;
        let key = format!(
            "{}:{}:{}",
            ctx.tenant_id, ctx.project_id, ctx.idempotency_key
        );
        symbol.etag = 0;
        let fingerprint = serde_json::to_string(&symbol)?;
        // Idempotency keys bind to the original normalized payload and original result.
        if let Some(previous) = guard.idempotency.get(&key) {
            if previous.aggregate_id != symbol.id || previous.payload_fingerprint != fingerprint {
                return Err(StoreError::Conflict);
            }
            return Ok(previous.result.clone());
        }
        if let Some(old) = guard.symbols.get(&symbol.id) {
            if old.tenant_id != ctx.tenant_id || old.project_id != ctx.project_id {
                return Err(StoreError::PermissionDenied);
            }
            if ctx.expected_etag != Some(old.etag) {
                return Err(StoreError::Conflict);
            }
            symbol.etag = old.etag + 1;
        } else {
            if ctx.expected_etag.is_some() {
                return Err(StoreError::Conflict);
            }
            symbol.etag = 1;
        }
        let mut next = guard.clone();
        next.symbols.insert(symbol.id.clone(), symbol.clone());
        next.idempotency.insert(
            key,
            IdempotencyRecord {
                aggregate_id: symbol.id.clone(),
                payload_fingerprint: fingerprint,
                result: symbol.clone(),
            },
        );
        next.next_event_id += 1;
        next.outbox.push(OutboxEvent {
            event_id: next.next_event_id,
            event_type: "code.symbol.upserted".into(),
            aggregate_id: symbol.id.clone(),
            revision: symbol.revision.clone(),
            delivered: false,
        });
        self.persist(&next)?;
        *guard = next;
        Ok(symbol)
    }
    fn get_symbol(&self, ctx: &ReadContext, symbol_id: &str) -> Result<SymbolRecord, StoreError> {
        if !ctx.can_read {
            return Err(StoreError::PermissionDenied);
        }
        let guard = self
            .state
            .lock()
            .map_err(|_| StoreError::Io("poisoned".into()))?;
        let item = guard.symbols.get(symbol_id).ok_or(StoreError::NotFound)?;
        if item.tenant_id != ctx.tenant_id || item.project_id != ctx.project_id {
            return Err(StoreError::NotFound);
        }
        Ok(item.clone())
    }
}
impl EventOutbox for JsonFileAdapter {
    fn pending(&self) -> Result<Vec<OutboxEvent>, StoreError> {
        Ok(self
            .all_events()?
            .into_iter()
            .filter(|x| !x.delivered)
            .collect())
    }
    fn mark_delivered(&self, event_id: u64) -> Result<(), StoreError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| StoreError::Io("poisoned".into()))?;
        let mut next = guard.clone();
        let event = next
            .outbox
            .iter_mut()
            .find(|x| x.event_id == event_id)
            .ok_or(StoreError::NotFound)?;
        event.delivered = true;
        self.persist(&next)?;
        *guard = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persist_idempotency_and_acl() {
        let file = std::env::temp_dir().join(format!("ukma-phase6-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let store = JsonFileAdapter::open(&file).unwrap();
        let ctx = WriteContext {
            tenant_id: "t".into(),
            project_id: "p".into(),
            idempotency_key: "once".into(),
            expected_etag: None,
            can_index: true,
        };
        let item = SymbolRecord {
            id: "s".into(),
            tenant_id: "t".into(),
            project_id: "p".into(),
            repo_id: "r".into(),
            revision: "abc".into(),
            name: "fn".into(),
            kind: "function".into(),
            etag: 0,
        };
        assert_eq!(store.upsert_symbol(&ctx, item.clone()).unwrap().etag, 1);
        assert_eq!(store.upsert_symbol(&ctx, item).unwrap().etag, 1);
        assert_eq!(store.pending().unwrap().len(), 1);
        drop(store);
        let reopened = JsonFileAdapter::open(&file).unwrap();
        assert_eq!(
            reopened
                .get_symbol(
                    &ReadContext {
                        tenant_id: "t".into(),
                        project_id: "p".into(),
                        can_read: true
                    },
                    "s"
                )
                .unwrap()
                .etag,
            1
        );
        assert!(matches!(
            reopened.get_symbol(
                &ReadContext {
                    tenant_id: "other".into(),
                    project_id: "p".into(),
                    can_read: true
                },
                "s"
            ),
            Err(StoreError::NotFound)
        ));
        reopened.mark_delivered(1).unwrap();
        assert_eq!(reopened.pending().unwrap().len(), 0);
        let _ = fs::remove_file(file);
    }
}
