//! Filesystem artifact adapter (§6.2 Artifact, embedded profile).
//!
//! Immutable, content-addressed storage: bytes are written once under
//! `objects/<sha256>`; the content address **is** the integrity check, so
//! tampering is detectable ([`ArtifactPort::verify`]).
//!
//! Guarantees (honest):
//! - single-process, single-writer; no cross-process locking;
//! - writes are atomic (temp file + rename);
//! - retention is recorded as metadata only; expiry enforcement is a policy job.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use storage_ports::capabilities::{
    AdapterCapabilities, BackendKind, CapabilityProbe, TransactionScope,
};
use storage_ports::error::{io_error, PortResult, StorageError};
use storage_ports::{ArtifactPort, ArtifactRecord, Retention};
use ukma_contracts::ScopeRef;

/// Content-addressed artifact store rooted at a directory.
pub struct FilesystemArtifacts {
    root: PathBuf,
    /// Guards in-process writers; cross-process safety is explicitly not claimed.
    lock: Mutex<()>,
}

impl FilesystemArtifacts {
    /// Open (creating if needed) an artifact store rooted at `root`.
    pub fn open(root: impl AsRef<Path>) -> PortResult<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(root.join("objects")).map_err(|e| io_error(&root, e))?;
        std::fs::create_dir_all(root.join("metadata")).map_err(|e| io_error(&root, e))?;
        Ok(Self {
            root,
            lock: Mutex::new(()),
        })
    }

    /// Root directory of the store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn object_path(&self, hash: &str) -> PathBuf {
        let hex = hash.strip_prefix("sha256:").unwrap_or(hash);
        self.root.join("objects").join(hex)
    }

    fn metadata_path(&self, scope: &ScopeRef, hash: &str) -> PathBuf {
        let hex = hash.strip_prefix("sha256:").unwrap_or(hash);
        self.root
            .join("metadata")
            .join(format!("{}.{}.{}", scope.tenant_id, scope.project_id, hex))
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(71);
    hex.push_str("sha256:");
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

impl ArtifactPort for FilesystemArtifacts {
    fn put_immutable(
        &self,
        scope: &ScopeRef,
        bytes: &[u8],
        media_type: &str,
        retention: Retention,
    ) -> PortResult<ArtifactRecord> {
        storage_ports::validate_scope(scope)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| StorageError::Internal("artifact lock poisoned".into()))?;

        let hash = hash_bytes(bytes);
        let object_path = self.object_path(&hash);
        if !object_path.exists() {
            let temp = object_path.with_extension("tmp");
            std::fs::write(&temp, bytes).map_err(|e| io_error(&temp, e))?;
            std::fs::rename(&temp, &object_path).map_err(|e| io_error(&object_path, e))?;
        }

        let record = ArtifactRecord {
            hash: hash.clone(),
            media_type: media_type.to_string(),
            byte_len: bytes.len() as u64,
            retention,
        };
        let metadata_path = self.metadata_path(scope, &hash);
        let encoded = serde_json::to_vec_pretty(&record)
            .map_err(|e| StorageError::Internal(format!("artifact metadata encode: {e}")))?;
        std::fs::write(&metadata_path, encoded).map_err(|e| io_error(&metadata_path, e))?;
        Ok(record)
    }

    fn get(&self, scope: &ScopeRef, hash: &str) -> PortResult<Vec<u8>> {
        storage_ports::validate_scope(scope)?;
        let metadata_path = self.metadata_path(scope, hash);
        if !metadata_path.exists() {
            return Err(StorageError::NotFound {
                collection: "artifact",
                key: hash.to_string(),
            });
        }
        std::fs::read(self.object_path(hash)).map_err(|e| io_error(&self.object_path(hash), e))
    }

    fn verify(&self, scope: &ScopeRef, hash: &str) -> PortResult<bool> {
        let bytes = self.get(scope, hash)?;
        Ok(hash_bytes(&bytes) == hash)
    }

    fn exists(&self, scope: &ScopeRef, hash: &str) -> PortResult<bool> {
        storage_ports::validate_scope(scope)?;
        Ok(self.metadata_path(scope, hash).exists() && self.object_path(hash).exists())
    }
}

impl CapabilityProbe for FilesystemArtifacts {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter_id: "filesystem-artifacts".to_string(),
            adapter_version: "0.1.0".to_string(),
            backend_kind: BackendKind::Embedded,
            logical_contract_version: "1.2.3".to_string(),
            adapter_schema_version: "1".to_string(),
            supported_ports: vec!["ArtifactPort".to_string()],
            transaction_scope: TransactionScope::SingleKey,
            atomic_batch: false,
            compare_and_swap: false,
            snapshot_reads: true,
            acl_filtering: false,
            stable_cursor: false,
            native_graph_traversal: false,
            vector_filters: false,
            fulltext_analyzers: Vec::new(),
            durable_outbox: false,
            backup_restore: true,
            max_batch_size: 1,
            available: true,
            unavailable_reason: String::new(),
        }
    }
}
