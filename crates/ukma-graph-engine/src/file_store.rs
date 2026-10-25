//! JSON file-backed `GraphStore` adapter (development-only).
//!
//! **Guarantees (honest, per ADR-022)**: single-process, single-writer.
//! No cross-process safety and no crash-durability guarantee between
//! operations — durability is obtained by calling `flush()` (atomic
//! write-temp-then-rename). `capabilities()` reports `persistent = true`
//! because a flushed state survives process restart, not because every
//! write is durable.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{GraphError, Result};
use crate::ids::CanonicalId;
use crate::state::{Snapshot, StoreInner};
use crate::store::{GraphStore, ScopeKey, StoreCapabilities};
use crate::types::{GraphEdge, GraphNode};

/// JSON file-backed graph store (development-only, single-writer).
pub struct FileGraphStore {
    path: PathBuf,
    inner: Mutex<StoreInner>,
}

impl FileGraphStore {
    /// Open (create if absent) a store backed by a JSON file.
    ///
    /// Loads the existing file when present; a corrupt or unreadable file
    /// is a typed error (never a silent empty state).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let inner = if path.exists() {
            let raw = std::fs::read(&path).map_err(|e| io_err(&path, e))?;
            let snap: Snapshot = serde_json::from_slice(&raw).map_err(|e| {
                GraphError::Internal(format!("corrupt store file {}: {e}", path.display()))
            })?;
            if snap.format != crate::state::SNAPSHOT_FORMAT_V1 {
                return Err(GraphError::Internal(format!(
                    "unsupported store format {} at {} (expected v{})",
                    snap.format,
                    path.display(),
                    crate::state::SNAPSHOT_FORMAT_V1
                )));
            }
            let mut inner = StoreInner::default();
            inner.load(&snap);
            inner
        } else {
            StoreInner::default()
        };
        Ok(Self {
            path,
            inner: Mutex::new(inner),
        })
    }

    /// Backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of nodes in scope (test helper).
    pub fn node_count(&self, scope: &ScopeKey) -> usize {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .node_count(scope)
    }

    /// Number of edges in scope (test helper).
    pub fn edge_count(&self, scope: &ScopeKey) -> usize {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .edge_count(scope)
    }
}

fn io_err(path: &Path, e: std::io::Error) -> GraphError {
    GraphError::Internal(format!("io error on {}: {e}", path.display()))
}

impl GraphStore for FileGraphStore {
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            persistent: true,
            atomic_batches: true,
        }
    }

    fn put_node(&self, scope: &ScopeKey, node: &GraphNode) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .put_node(scope, node);
        Ok(())
    }

    fn get_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphNode>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .get_node(scope, id))
    }

    fn delete_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .delete_node(scope, id);
        Ok(())
    }

    fn node_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .node_ids(scope))
    }

    fn put_edge(&self, scope: &ScopeKey, edge: &GraphEdge) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .put_edge(scope, edge);
        Ok(())
    }

    fn get_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphEdge>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .get_edge(scope, id))
    }

    fn delete_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .delete_edge(scope, id);
        Ok(())
    }

    fn edge_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .edge_ids(scope))
    }

    fn outgoing_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .outgoing_edge_ids(scope, node))
    }

    fn incoming_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .incoming_edge_ids(scope, node))
    }

    /// Atomic durable flush: serialize to `<path>.tmp`, then rename over
    /// the target (the rename is atomic on both NTFS and POSIX).
    fn flush(&self) -> Result<()> {
        let snap = self.inner.lock().expect("store lock poisoned").snapshot();
        let bytes = serde_json::to_vec_pretty(&snap)
            .map_err(|e| GraphError::Internal(format!("snapshot serialization failed: {e}")))?;
        let tmp = self.path.with_file_name(format!(
            "{}.tmp",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("graph-store")
        ));
        std::fs::write(&tmp, &bytes).map_err(|e| io_err(&tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| io_err(&self.path, e))?;
        Ok(())
    }
}
