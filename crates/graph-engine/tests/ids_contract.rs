//! Contract tests: canonical ids (ADR-031 D1).

use graph_engine::{
    CanonicalId, DefaultIdGenerator, DeterministicIdGenerator, GraphError, IdGenerator, IdKind,
};

fn parse(s: &str) -> Result<CanonicalId, GraphError> {
    CanonicalId::parse(s)
}

/// A valid 26-character Crockford base32 ULID.
const ULID26: &str = "01ARZ3NDEKTSV4RRFFQ79A5TK0";

#[test]
fn parses_valid_node_and_edge_ids() {
    let node = format!("graph.node.{ULID26}");
    assert_eq!(ULID26.len(), 26, "test constant must be 26 chars");
    let n = parse(&node).expect("valid node id");
    assert_eq!(n.kind(), IdKind::Node);
    assert_eq!(n.as_str(), node);
    assert_eq!(n.ulid_part(), ULID26);

    let edge = "graph.edge.00000000000000000000000000";
    assert_eq!(
        edge.len() - "graph.edge.".len(),
        26,
        "edge id must be 26 chars"
    );
    let e = parse(edge).expect("valid edge id");
    assert_eq!(e.kind(), IdKind::Edge);
}

#[test]
fn rejects_malformed_ids() {
    // wrong prefix
    assert!(parse(&format!("graph.vertex.{ULID26}")).is_err());
    // lowercase is not Crockford base32
    assert!(parse(&format!("graph.node.{}", ULID26.to_lowercase())).is_err());
    // wrong length (25 chars)
    assert!(parse(&format!("graph.node.{}", &ULID26[..25])).is_err());
    // wrong length (27 chars)
    assert!(parse(&format!("graph.node.{ULID26}X")).is_err());
    // invalid Crockford characters: I, L, O, U
    for bad in [
        "graph.node.01ARZ3NDEKTSV4RRFFQI9A5TK0",
        "graph.node.01ARZ3NDEKTSV4RRFFQL9A5TK0",
        "graph.node.01ARZ3NDEKTSV4RRFFQO9A5TK0",
        "graph.node.01ARZ3NDEKTSV4RRFFQU9A5TK0",
    ] {
        assert!(
            bad.len() - "graph.node.".len() == 26,
            "{bad} must be 26 chars"
        );
        assert!(parse(bad).is_err(), "{bad} must be rejected");
    }
}

#[test]
fn malformed_ids_are_invalid_argument() {
    match parse("graph.node.too-short") {
        Err(GraphError::InvalidArgument { field, .. }) => assert_eq!(field, "canonical_id"),
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn serde_roundtrip_validates_on_deserialize() {
    let id = parse(&format!("graph.node.{ULID26}")).unwrap();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, format!("\"graph.node.{ULID26}\""));
    let back: CanonicalId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);

    // invalid string (invalid Crockford char) fails to deserialize
    let bad = serde_json::from_str::<CanonicalId>("\"graph.node.01ARZ3NDEKTSV4RRFFQI9A5TK0\"");
    assert!(bad.is_err());
}

#[test]
fn ulid_encoding_is_26_crockford_chars() {
    let bytes = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ];
    let encoded = graph_engine::Ulid(bytes).encode_base32();
    assert_eq!(encoded.len(), 26);
    let alphabet: Vec<char> = "0123456789ABCDEFGHJKMNPQRSTVWXYZ".chars().collect();
    assert!(
        encoded.chars().all(|c| alphabet.contains(&c)),
        "encoded: {encoded}"
    );
}

#[test]
fn default_generator_is_unique() {
    let mut gen = DefaultIdGenerator::default();
    let a = gen.next(IdKind::Node);
    let b = gen.next(IdKind::Node);
    assert_ne!(a, b);
    assert_eq!(a.kind(), IdKind::Node);
    // kind prefixes differ
    let c = gen.next(IdKind::Edge);
    assert!(c.as_str().starts_with("graph.edge."));
}

#[test]
fn deterministic_generator_is_ordered_and_kind_separated() {
    let mut gen = DeterministicIdGenerator::default();
    let n1 = gen.next(IdKind::Node);
    let e1 = gen.next(IdKind::Edge);
    let n2 = gen.next(IdKind::Node);
    // node sequence and edge sequence are independent
    assert!(n1 < n2);
    assert!(n1.as_str().starts_with("graph.node."));
    assert!(e1.as_str().starts_with("graph.edge."));
    // deterministic: two generators produce identical sequences
    let mut gen2 = DeterministicIdGenerator::default();
    assert_eq!(gen2.next(IdKind::Node), n1);
    assert_eq!(gen2.next(IdKind::Edge), e1);
    assert_eq!(gen2.next(IdKind::Node), n2);
}
