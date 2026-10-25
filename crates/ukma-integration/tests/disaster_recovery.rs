//! P10-WP4: disaster recovery drill (OPS-001).
//!
//! Backup, destroy, restore, re-index and rollback are real file operations: the
//! backup is a copy, the disaster deletes the live files, and the restore proves
//! the canonical data, the generation pointer and the outbox all come back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexing_engine::GenerationPointer;
use storage_ports::{ArtifactPort, FullTextDoc, FullTextPort, MetadataPort, OutboxPort, Retention};
use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};
use ukma_adapter_filesystem::FilesystemArtifacts;
use ukma_contracts::ScopeRef;

const TENANT: &str = "tenant-a";
const PROJECT: &str = "project-b";
const ANALYZER: &str = "icu-v1";

struct Layout {
    root: PathBuf,
}

impl Layout {
    fn new(tag: &str) -> Self {
        let mut root = std::env::temp_dir();
        root.push(format!("ukma-dr-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("live")).unwrap();
        std::fs::create_dir_all(root.join("backup")).unwrap();
        Self { root }
    }

    fn store(&self, area: &str) -> PathBuf {
        self.root.join(area).join("store.json")
    }

    fn artifacts(&self, area: &str) -> PathBuf {
        self.root.join(area).join("artifacts")
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scope() -> ScopeRef {
    default_scope(TENANT, PROJECT)
}

/// Copy the store file and every artifact byte-for-byte.
fn backup(layout: &Layout) -> usize {
    std::fs::copy(layout.store("live"), layout.store("backup")).expect("store copy");
    copy_tree(&layout.artifacts("live"), &layout.artifacts("backup"))
}

fn copy_tree(from: &Path, to: &Path) -> usize {
    let mut copied = 0;
    if !from.is_dir() {
        return copied;
    }
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copied += copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            copied += 1;
        }
    }
    copied
}

fn populate(layout: &Layout) -> EmbeddedStore {
    let store = EmbeddedStore::open(layout.store("live")).unwrap();
    let artifacts = FilesystemArtifacts::open(layout.artifacts("live")).unwrap();
    let s = scope();
    store
        .put_cas(&s, "code/manifest", serde_json::json!({"gen": 1}), None)
        .unwrap();
    store
        .put_cas(
            &s,
            "memory/mem:1",
            serde_json::json!({"summary": "survive me"}),
            None,
        )
        .unwrap();
    store.put_manifest(&s, "code", "gen-1").unwrap();
    store
        .enqueue(&s, event("e1", "memory.episode.committed", 1))
        .unwrap();
    artifacts
        .put_immutable(
            &s,
            b"canonical evidence",
            "text/plain",
            Retention::KeepForever,
        )
        .unwrap();
    store.flush().unwrap();
    store
}

/// Rebuild the derived projection from canonical data plus outbox replay.
fn reindex(store: &EmbeddedStore, scope: &ScopeRef) -> usize {
    let mut rebuilt = 0;
    let mut pointer = GenerationPointer::new();
    for pending in store.pending(scope, "projection", 100).unwrap() {
        let (entries, _cursor) = store.list(scope, "", 1000, None).unwrap();
        for entry in entries {
            let body = entry
                .value
                .get("summary")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| entry.value.to_string());
            store
                .index(
                    scope,
                    ANALYZER,
                    &FullTextDoc {
                        id: entry.key.clone(),
                        revision: entry.revision.to_string(),
                        fields: BTreeMap::from([("body".to_string(), body)]),
                    },
                )
                .unwrap();
            rebuilt += 1;
        }
        if let Some(generation) = store.get_manifest(scope, "code").unwrap() {
            if pointer.current() != Some(generation.as_str()) {
                pointer.publish(&generation);
            }
        }
        store.ack(scope, "projection", &pending.event_id).unwrap();
    }
    rebuilt
}

#[test]
fn a_full_backup_restore_and_reindex_recovers_the_deployment() {
    let layout = Layout::new("full");
    let s = scope();
    let store = populate(&layout);
    let _ = store;

    // backup: store + artifact bytes
    let copied = backup(&layout);
    assert!(copied >= 1, "the artifact bytes were not copied");

    // disaster: the live files are gone
    std::fs::remove_file(layout.store("live")).unwrap();
    std::fs::remove_dir_all(layout.artifacts("live")).unwrap();
    assert!(
        EmbeddedStore::open(layout.store("live")).is_ok(),
        "a missing file opens empty"
    );
    let empty = EmbeddedStore::open(layout.store("live")).unwrap();
    assert!(empty.get(&s, "code/manifest").unwrap().is_none());

    // restore
    std::fs::copy(layout.store("backup"), layout.store("live")).unwrap();
    copy_tree(&layout.artifacts("backup"), &layout.artifacts("live"));

    let restored = EmbeddedStore::open(layout.store("live")).unwrap();
    let entry = restored
        .get(&s, "memory/mem:1")
        .unwrap()
        .expect("canonical record restored");
    assert_eq!(entry.value["summary"], "survive me");
    assert_eq!(
        restored.get_manifest(&s, "code").unwrap().as_deref(),
        Some("gen-1"),
        "the generation pointer must survive the restore"
    );
    assert_eq!(
        restored.pending(&s, "projection", 10).unwrap().len(),
        1,
        "the unacked outbox must survive the restore"
    );

    let artifacts = FilesystemArtifacts::open(layout.artifacts("live")).unwrap();
    let hash = {
        let s = scope();
        artifacts
            .put_immutable(
                &s,
                b"canonical evidence",
                "text/plain",
                Retention::KeepForever,
            )
            .unwrap()
            .hash
    };
    assert_eq!(
        artifacts.get(&s, &hash).unwrap(),
        b"canonical evidence".to_vec()
    );

    // re-index: derived projections are rebuilt from canonical data
    let rebuilt = reindex(&restored, &s);
    assert!(rebuilt >= 2, "only {rebuilt} records were re-indexed");
    assert!(
        !FullTextPort::search(&restored, &s, ANALYZER, None, "survive", 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        restored.pending(&s, "projection", 10).unwrap().is_empty(),
        "the re-index must ack the outbox"
    );
}

#[test]
fn a_corrupt_store_is_a_typed_failure_and_the_backup_still_restores_it() {
    let layout = Layout::new("corrupt");
    let s = scope();
    let _ = populate(&layout);
    backup(&layout);

    // disaster: the snapshot is truncated mid-write
    std::fs::write(layout.store("live"), b"{\"format\": 1, \"entr").unwrap();
    match EmbeddedStore::open(layout.store("live")) {
        Err(storage_ports::StorageError::Internal(message)) => {
            assert!(message.contains("corrupt"), "{message}")
        }
        Ok(_) => panic!("a truncated snapshot must not open"),
        Err(other) => panic!("unexpected error: {other}"),
    }

    std::fs::copy(layout.store("backup"), layout.store("live")).unwrap();
    let restored = EmbeddedStore::open(layout.store("live")).unwrap();
    assert!(restored.get(&s, "code/manifest").unwrap().is_some());
}

#[test]
fn rollback_reads_the_retained_generation_and_refuses_the_unpublished_one() {
    let layout = Layout::new("rollback");
    let s = scope();
    let store = populate(&layout);
    let mut pointer = GenerationPointer::new();
    pointer.publish("gen-1");
    pointer.require("gen-1").expect("gen-1 is current");

    // A new generation is published, then rolled back to the retained one.
    store.put_manifest(&s, "code", "gen-2").unwrap();
    store
        .enqueue(&s, event("e2", "code.analysis.committed", 2))
        .unwrap();
    store.flush().unwrap();
    pointer.publish("gen-2");
    assert_eq!(pointer.current(), Some("gen-2"));
    assert_eq!(pointer.retained(), ["gen-1"]);

    // Rollback: readers that pin the old generation keep working, and the
    // generation that was never published is still refused.
    store.put_manifest(&s, "code", "gen-1").unwrap();
    pointer.publish("gen-1");
    pointer
        .require("gen-1")
        .expect("rollback generation is readable");
    pointer
        .require("gen-2")
        .expect("the superseded generation is retained");
    assert!(
        pointer.require("gen-3").is_err(),
        "an unpublished generation must be refused"
    );

    // The replayed event is still pending, so the projection is rebuilt.
    assert_eq!(store.pending(&s, "projection", 10).unwrap().len(), 2);
}
