//! Contract tests: versioning (revision + ETag optimistic concurrency),
//! capability negotiation (ADR-022), and the registered v1.2.3 error-code
//! mapping (no invented wire codes).

use graph_engine::{
    DeterministicIdGenerator, EdgeType, GraphEngine, GraphError, NodeType, PropertyMap, ScopeKey,
};

fn engine() -> (GraphEngine, ScopeKey) {
    let e = GraphEngine::with_store_and_ids(
        std::sync::Arc::new(graph_engine::InMemoryGraphStore::new()),
        Box::new(DeterministicIdGenerator::default()),
    );
    (e, ScopeKey::new("t", "p"))
}

#[test]
fn update_with_correct_revision_bumps_and_recomputes_etag() {
    let (e, s) = engine();
    let n = e
        .add_node(
            &s,
            NodeType::Function,
            PropertyMap::new().with("v", 1i64),
            None,
        )
        .unwrap();
    assert_eq!(n.revision, 1);
    let old_etag = n.etag.clone();

    let updated = e
        .update_node(&s, &n.id, 1, PropertyMap::new().with("v", 2i64))
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_ne!(
        updated.etag, old_etag,
        "etag changes with revision/properties"
    );

    // second update requires the new revision
    let updated2 = e
        .update_node(&s, &n.id, 2, PropertyMap::new().with("v", 3i64))
        .unwrap();
    assert_eq!(updated2.revision, 3);
}

#[test]
fn stale_revision_is_etag_mismatch() {
    let (e, s) = engine();
    let n = e
        .add_node(&s, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    e.update_node(&s, &n.id, 1, PropertyMap::new().with("v", 2i64))
        .unwrap();

    // caller still believes revision 1
    match e.update_node(&s, &n.id, 1, PropertyMap::new().with("v", 99i64)) {
        Err(GraphError::Conflict {
            entity,
            expected_revision,
            actual_revision,
            ..
        }) => {
            assert_eq!(entity, "node");
            assert_eq!(expected_revision, 1);
            assert_eq!(actual_revision, 2);
        }
        ok => panic!("expected Conflict, got {ok:?}"),
    }
    // wire code is the registered one
    let err = e
        .update_node(&s, &n.id, 1, PropertyMap::new().with("v", 99i64))
        .unwrap_err();
    assert_eq!(err.code(), "ETAG_MISMATCH");
    assert_eq!(err.error_detail().code, "ETAG_MISMATCH");
    assert!(!err.retryable(), "conflict is not retryable");

    // the failed update must not have mutated the record
    let current = e.get_node(&s, &n.id).unwrap();
    assert_eq!(current.revision, 2);
}

#[test]
fn update_missing_entity_is_not_found() {
    let (e, s) = engine();
    let id = graph_engine::CanonicalId::parse("graph.node.00000000000000000000000007").unwrap();
    match e.update_node(&s, &id, 1, PropertyMap::new()) {
        Err(GraphError::NotFound { .. }) => {}
        ok => panic!("expected NotFound, got {ok:?}"),
    }
}

#[test]
fn etag_is_deterministic_and_content_sensitive() {
    let (e, s) = engine();
    let n1 = e
        .add_node(
            &s,
            NodeType::Function,
            PropertyMap::new().with("a", 1i64),
            None,
        )
        .unwrap();
    let n2 = e
        .add_node(
            &s,
            NodeType::Function,
            PropertyMap::new().with("a", 1i64),
            None,
        )
        .unwrap();
    assert_eq!(
        n1.etag, n2.etag,
        "same revision + type + properties => same etag"
    );

    let n3 = e
        .add_node(
            &s,
            NodeType::Function,
            PropertyMap::new().with("a", 2i64),
            None,
        )
        .unwrap();
    assert_ne!(n1.etag, n3.etag, "different properties => different etag");

    let n4 = e
        .add_node(
            &s,
            NodeType::Class,
            PropertyMap::new().with("a", 1i64),
            None,
        )
        .unwrap();
    assert_ne!(n1.etag, n4.etag, "different type => different etag");

    // revision sensitivity
    let updated = e
        .update_node(&s, &n1.id, 1, PropertyMap::new().with("a", 1i64))
        .unwrap();
    assert_ne!(n1.etag, updated.etag, "revision bump => new etag");

    // edge etags
    let a = n1;
    let b = n2;
    let e1 = e
        .add_edge(
            &s,
            &a.id,
            &b.id,
            EdgeType::Calls,
            PropertyMap::new().with("l", 1i64),
            None,
        )
        .unwrap();
    let e2 = e
        .add_edge(
            &s,
            &a.id,
            &b.id,
            EdgeType::Calls,
            PropertyMap::new().with("l", 1i64),
            None,
        )
        .unwrap();
    assert_eq!(e1.etag, e2.etag);
}

#[test]
fn error_code_mapping_is_registered_only() {
    // Every GraphError variant maps to a v1.2.3 registered code.
    let cases: Vec<(GraphError, &str, bool)> = vec![
        (
            GraphError::NotFound {
                entity: "node",
                id: "graph.node.0".into(),
            },
            "NOT_FOUND",
            false,
        ),
        (
            GraphError::Conflict {
                entity: "node",
                id: "graph.node.0".into(),
                expected_revision: 1,
                actual_revision: 2,
            },
            "ETAG_MISMATCH",
            false,
        ),
        (
            GraphError::IdempotencyConflict {
                id: "graph.node.0".into(),
            },
            "IDEMPOTENCY_CONFLICT",
            false,
        ),
        (
            GraphError::InvalidArgument {
                field: "tenant_id",
                reason: "empty".into(),
            },
            "INVALID_ARGUMENT",
            false,
        ),
        (
            GraphError::CapabilityUnavailable {
                capability: "durable_flush",
            },
            "CAPABILITY_UNAVAILABLE",
            false,
        ),
        (
            GraphError::CrossScopeDenied {
                tenant_id: "t".into(),
                project_id: "p".into(),
            },
            "CROSS_SCOPE_DENIED",
            false,
        ),
        (
            GraphError::ResourceLimit { limit: 5 },
            "RESOURCE_LIMIT",
            false,
        ),
        (GraphError::Internal("boom".into()), "INTERNAL", true),
    ];
    for (err, code, retryable) in cases {
        assert_eq!(err.code(), code);
        assert_eq!(err.error_detail().code, code);
        assert_eq!(err.retryable(), retryable);
        assert_eq!(err.error_detail().retryable, retryable);
    }
}

#[test]
fn capabilities_are_probed_not_assumed() {
    let (e, _) = engine();
    let caps = e.capabilities();
    assert!(!caps.persistent, "in-memory store is not persistent");
    assert!(caps.atomic_batches);
}

#[test]
fn flush_on_non_persistent_store_is_capability_unavailable() {
    let (e, _) = engine();
    match e.flush() {
        Err(GraphError::CapabilityUnavailable { capability }) => {
            assert_eq!(capability, "durable_flush");
        }
        ok => panic!("expected CapabilityUnavailable, got {ok:?}"),
    }
    let err = e.flush().unwrap_err();
    assert_eq!(err.code(), "CAPABILITY_UNAVAILABLE");
    assert_eq!(err.error_detail().code, "CAPABILITY_UNAVAILABLE");
}

#[test]
fn display_and_error_detail_safe_message() {
    let err = GraphError::Conflict {
        entity: "node",
        id: "graph.node.abc".into(),
        expected_revision: 1,
        actual_revision: 2,
    };
    let detail = err.error_detail();
    assert!(
        detail.safe_message.contains("mismatch"),
        "safe message: {}",
        detail.safe_message
    );
    let s = format!("{err}");
    assert!(!s.is_empty());
}
