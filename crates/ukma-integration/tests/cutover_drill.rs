//! P10-WP2: cutover drill (DB-010, E2E-API-11).
//!
//! `PLANNED -> SNAPSHOT -> EXPORT -> IMPORT -> VERIFY -> SHADOW_READ -> CUTOVER ->
//! MONITOR -> COMPLETE`, with a fencing token and a rollback drill. The two stores
//! are real embedded stores on disk: the export/import is actual data movement,
//! not a claimed one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use storage_ports::{MetadataPort, OutboxEvent, OutboxPort};
use storage_router::cutover::{Cutover, CutoverError, CutoverState, StepEvidence};
use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};
use ukma_contracts::ScopeRef;

const TENANT: &str = "tenant-a";
const PROJECT: &str = "project-b";

fn temp(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-cutover-{}-{tag}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn scope() -> ScopeRef {
    default_scope(TENANT, PROJECT)
}

/// A cutover moves two things: the canonical records **and** the published
/// generation pointers. A drill that moved only the records would lose the
/// generation and silently reset `strict_generation` reads.
struct Export {
    records: Vec<Value>,
    manifests: BTreeMap<String, String>,
    /// Pending events per consumer: the outbox is canonical, so a cutover that
    /// dropped it would lose every unacked projection on the target side.
    pending: BTreeMap<String, Vec<OutboxEvent>>,
}

/// The manifest names the target topology declares; a real cutover takes them
/// from the deployment profile rather than by guessing.
const MANIFESTS: [&str; 3] = ["code", "memory", "trace"];

/// Projection consumers the deployment declares (their ack state is part of the
/// cutover, because it decides what must be replayed afterwards).
const CONSUMERS: [&str; 2] = ["projection", "audit"];

/// Everything the source holds for this scope, in a stable order.
fn export(store: &EmbeddedStore, scope: &ScopeRef) -> Export {
    let (entries, _cursor) = store.list(scope, "", 1000, None).expect("list");
    let records = entries
        .into_iter()
        .map(|entry| {
            json!({
                "key": entry.key,
                "revision": entry.revision,
                "etag": entry.etag,
                "value": entry.value,
            })
        })
        .collect();
    let mut manifests = BTreeMap::new();
    for name in MANIFESTS {
        if let Some(generation) = store.get_manifest(scope, name).expect("manifest read") {
            manifests.insert(name.to_string(), generation);
        }
    }
    let mut pending = BTreeMap::new();
    for consumer in CONSUMERS {
        pending.insert(
            consumer.to_string(),
            store.pending(scope, consumer, 1000).expect("outbox read"),
        );
    }
    Export {
        records,
        manifests,
        pending,
    }
}

fn import(store: &EmbeddedStore, scope: &ScopeRef, export: &Export) -> usize {
    let mut moved = 0;
    for record in &export.records {
        store
            .put_cas(
                scope,
                record["key"].as_str().expect("key"),
                record["value"].clone(),
                None,
            )
            .expect("import write");
        moved += 1;
    }
    for (name, generation) in &export.manifests {
        store
            .put_manifest(scope, name, generation)
            .expect("manifest write");
    }
    // Every event is enqueued once; ack state is then restored per consumer.
    let mut all_events: BTreeMap<String, OutboxEvent> = BTreeMap::new();
    for events in export.pending.values() {
        for event in events {
            all_events.insert(event.event_id.clone(), event.clone());
        }
    }
    for event in all_events.into_values() {
        store.enqueue(scope, event).expect("outbox enqueue");
    }
    for consumer in CONSUMERS {
        let keep: Vec<&str> = export
            .pending
            .get(consumer)
            .map(|events| events.iter().map(|event| event.event_id.as_str()).collect())
            .unwrap_or_default();
        for event_id in export
            .pending
            .values()
            .flat_map(|events| events.iter().map(|event| event.event_id.clone()))
        {
            if !keep.contains(&event_id.as_str()) {
                store.ack(scope, consumer, &event_id).expect("outbox ack");
            }
        }
    }
    moved
}

fn checksum(export: &Export) -> String {
    // A real content checksum of the exported bytes, not a placeholder.
    let mut hasher = Sha256Like::default();
    for record in &export.records {
        hasher.update(record.to_string().as_bytes());
    }
    for (name, generation) in &export.manifests {
        hasher.update(name.as_bytes());
        hasher.update(generation.as_bytes());
    }
    for (consumer, events) in &export.pending {
        hasher.update(consumer.as_bytes());
        for event in events {
            hasher.update(event.event_id.as_bytes());
        }
    }
    format!("sha256:{}", hasher.finish())
}

/// Tiny deterministic digest (FNV-1a 128 bit spelled out as hex) so the drill does
/// not need another dependency; it is only used to prove the export is identical.
#[derive(Default)]
struct Sha256Like(u128);

impl Sha256Like {
    fn update(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for byte in bytes {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }

    fn finish(&self) -> String {
        format!("{:032x}", self.0)
    }
}

fn source_store(path: &Path) -> EmbeddedStore {
    let store = EmbeddedStore::open(path).unwrap();
    let s = scope();
    store
        .put_cas(&s, "code/manifest", json!({"gen": 1, "symbols": 3}), None)
        .unwrap();
    store
        .put_cas(&s, "memory/mem:1", json!({"summary": "kept"}), None)
        .unwrap();
    store
        .put_cas(&s, "trace/link:1", json!({"state": "accepted"}), None)
        .unwrap();
    store.put_manifest(&s, "code", "gen-1").unwrap();
    store
        .enqueue(&s, event("e1", "code.analysis.committed", 1))
        .unwrap();
    store.flush().unwrap();
    store
}

#[test]
fn the_nine_step_cutover_moves_real_data_and_keeps_identity() {
    let source_path = temp("source");
    let target_path = temp("target");
    let s = scope();
    let source = source_store(&source_path);

    let mut cutover = Cutover::new();
    cutover.require_generation("gen-1");
    let fence = cutover.fence();

    // SNAPSHOT: freeze the source and record its checksum.
    source.flush().unwrap();
    let snapshot = export(&source, &s);
    let snapshot_checksum = checksum(&snapshot);
    cutover
        .advance(
            CutoverState::Snapshot,
            fence,
            StepEvidence {
                checksum: Some(snapshot_checksum.clone()),
                records: Some(snapshot.records.len()),
                divergences: Some(0),
            },
        )
        .unwrap();

    // EXPORT: the canonical records leave the source unchanged.
    let exported = export(&source, &s);
    assert_eq!(checksum(&exported), snapshot_checksum);
    cutover
        .advance(
            CutoverState::Export,
            fence,
            StepEvidence {
                checksum: Some(checksum(&exported)),
                records: Some(exported.records.len()),
                divergences: Some(0),
            },
        )
        .unwrap();

    // IMPORT: the target accepts them.
    let target = EmbeddedStore::open(&target_path).unwrap();
    let moved = import(&target, &s, &exported);
    target.flush().unwrap();
    let imported = export(&target, &s);
    cutover
        .advance(
            CutoverState::Import,
            fence,
            StepEvidence {
                checksum: Some(checksum(&imported)),
                records: Some(moved),
                divergences: Some(0),
            },
        )
        .unwrap();

    // VERIFY: logical identity, revisions, etags, values and the manifest survive.
    let source_view = export(&source, &s);
    let target_view = export(&target, &s);
    assert_eq!(
        source_view.records, target_view.records,
        "logical identity changed across the cutover"
    );
    assert_eq!(
        source_view.manifests, target_view.manifests,
        "the published generation did not survive the cutover"
    );
    assert_eq!(
        source_view.pending, target_view.pending,
        "the unacked outbox did not survive the cutover"
    );
    assert_eq!(
        target.get_manifest(&s, "code").unwrap(),
        source.get_manifest(&s, "code").unwrap()
    );
    let foreign = default_scope("tenant-z", PROJECT);
    assert!(
        target.get(&foreign, "code/manifest").unwrap().is_none(),
        "ACL scope did not survive the cutover"
    );
    cutover
        .advance(
            CutoverState::Verify,
            fence,
            StepEvidence {
                checksum: Some(checksum(&target_view)),
                records: Some(target_view.records.len()),
                divergences: Some(0),
            },
        )
        .unwrap();

    // SHADOW_READ: reads agree before writers move.
    let mut divergences = 0;
    for record in &source_view.records {
        let key = record["key"].as_str().unwrap();
        let from_source = source.get(&s, key).unwrap().map(|entry| entry.value);
        let from_target = target.get(&s, key).unwrap().map(|entry| entry.value);
        if from_source != from_target {
            divergences += 1;
        }
    }
    assert_eq!(divergences, 0);
    cutover
        .advance(
            CutoverState::ShadowRead,
            fence,
            StepEvidence {
                checksum: None,
                records: Some(source_view.records.len()),
                divergences: Some(0),
            },
        )
        .unwrap();

    // CUTOVER / MONITOR / COMPLETE: ownership moves and the source goes read-only.
    let mut writable = cutover;
    writable
        .advance(CutoverState::Cutover, fence, StepEvidence::default())
        .unwrap();
    assert!(writable.state().target_writable());
    assert!(!writable.state().source_writable());
    let new_fence = writable.fence();
    assert!(
        new_fence > fence,
        "writer ownership moved without a fence bump"
    );
    writable
        .advance(CutoverState::Monitor, new_fence, StepEvidence::default())
        .unwrap();
    writable
        .advance(
            CutoverState::Complete,
            writable.fence(),
            StepEvidence::default(),
        )
        .unwrap();
    assert_eq!(writable.state(), CutoverState::Complete);
    assert_eq!(writable.steps().len(), 8);

    // The old fence is now useless: a superseded writer cannot commit.
    assert!(matches!(
        writable.advance(CutoverState::Complete, fence, StepEvidence::default()),
        Err(CutoverError::StaleFence { .. })
    ));

    // Outbox replay on the target still sees the pending event.
    assert_eq!(target.pending(&s, "projection", 10).unwrap().len(), 1);

    let report: Value = serde_json::from_str(&writable.report_json()).expect("report");
    assert_eq!(report["current"], "complete");
    assert_eq!(report["retained_generations"][0], "gen-1");

    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&target_path);
}

#[test]
fn a_failed_export_can_be_rolled_back_and_replayed() {
    let source_path = temp("rollback");
    let target_path = temp("rollback-target");
    let s = scope();
    let source = source_store(&source_path);
    let mut cutover = Cutover::new();
    let fence = cutover.fence();

    let records = export(&source, &s);
    cutover
        .advance(
            CutoverState::Snapshot,
            fence,
            StepEvidence {
                checksum: Some(checksum(&records)),
                records: Some(records.records.len()),
                divergences: Some(0),
            },
        )
        .unwrap();

    // The export is wrong (a truncated record set) and the drill stops.
    let truncated = Export {
        records: records.records.iter().take(1).cloned().collect(),
        manifests: BTreeMap::new(),
        pending: BTreeMap::new(),
    };
    assert_ne!(checksum(&truncated), checksum(&records));
    let state = cutover.rollback("export was truncated").unwrap();
    assert_eq!(state, CutoverState::Planned);
    assert!(
        cutover.fence() > fence,
        "a rollback must fence the old writer"
    );

    // Replay from the snapshot with the full record set, then import for real.
    let target = EmbeddedStore::open(&target_path).unwrap();
    let moved = import(&target, &s, &records);
    assert_eq!(moved, records.records.len());
    let exported_again = export(&target, &s);
    assert_eq!(exported_again.records, records.records);
    assert_eq!(exported_again.manifests, records.manifests);
    // A relative order check: keys are stable, so the export is reproducible.
    let keys: Vec<&str> = records
        .records
        .iter()
        .filter_map(|r| r["key"].as_str())
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);

    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&target_path);
}

#[test]
fn the_machine_refuses_to_skip_a_step_or_a_clean_shadow_read() {
    let mut cutover = Cutover::new();
    let fence = cutover.fence();
    assert!(matches!(
        cutover.advance(CutoverState::Import, fence, StepEvidence::default()),
        Err(CutoverError::OutOfOrder { .. })
    ));
    cutover
        .advance(
            CutoverState::Snapshot,
            fence,
            StepEvidence {
                checksum: Some("sha256:aa".into()),
                records: Some(3),
                divergences: Some(0),
            },
        )
        .unwrap();
    cutover
        .advance(
            CutoverState::Export,
            fence,
            StepEvidence {
                checksum: Some("sha256:bb".into()),
                records: Some(3),
                divergences: Some(0),
            },
        )
        .unwrap();
    cutover
        .advance(
            CutoverState::Import,
            fence,
            StepEvidence {
                checksum: Some("sha256:bb".into()),
                records: Some(3),
                divergences: Some(0),
            },
        )
        .unwrap();
    cutover
        .advance(
            CutoverState::Verify,
            fence,
            StepEvidence {
                checksum: Some("sha256:bb".into()),
                records: Some(3),
                divergences: Some(0),
            },
        )
        .unwrap();
    // A shadow read with a divergence must not be followed by a cutover.
    assert!(matches!(
        cutover.advance(
            CutoverState::ShadowRead,
            fence,
            StepEvidence {
                checksum: None,
                records: Some(3),
                divergences: Some(2),
            }
        ),
        Err(CutoverError::PreconditionFailed { .. })
    ));
    let mut steps: BTreeMap<&str, usize> = BTreeMap::new();
    for step in cutover.steps() {
        *steps.entry(step.state.as_str()).or_default() += 1;
    }
    assert_eq!(steps.len(), 4);
    assert_eq!(cutover.state(), CutoverState::Verify);
}
