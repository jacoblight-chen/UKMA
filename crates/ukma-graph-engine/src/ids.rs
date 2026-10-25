//! Canonical graph identifiers (ADR-031 D1).
//!
//! Canonical id = structured string `graph.node.<ULID>` / `graph.edge.<ULID>`.
//! Engine-assigned at creation, persisted with the record, therefore stable
//! across adapter substitution (ADR-023, PROP-02). Vendor `u64` counters are
//! never exposed.

use std::fmt;
use std::hash::Hasher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{GraphError, Result};

/// Crockford base32 alphabet (no I/L/O/U).
const B32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Entity class of a canonical id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdKind {
    /// Graph node.
    Node,
    /// Graph edge.
    Edge,
}

impl IdKind {
    fn prefix(self) -> &'static str {
        match self {
            IdKind::Node => "graph.node",
            IdKind::Edge => "graph.edge",
        }
    }
}

/// 128-bit ULID: 48-bit millisecond timestamp + 80-bit entropy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ulid(pub [u8; 16]);

impl Ulid {
    /// Encode as 26 Crockford base32 characters.
    pub fn encode_base32(&self) -> String {
        let mut out = String::with_capacity(26);
        // 16 bytes = 128 bits; 26 chars x 5 bits = 130 bits (2 spare bits, zero).
        for i in 0..26 {
            let mut value = 0u32;
            for b in 0..5 {
                let pos = i * 5 + b;
                // The last two bit positions (128, 129) exceed the 128-bit
                // ULID and are always zero.
                let bit_value: u32 = if pos < 128 {
                    let byte_idx = 15 - (pos / 8);
                    let bit_idx = 7 - (pos % 8);
                    ((self.0[byte_idx] >> bit_idx) & 1) as u32
                } else {
                    0
                };
                value = (value << 1) | bit_value;
            }
            out.push(B32[value as usize] as char);
        }
        out
    }
}

/// Canonical graph id (validated structured string).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalId {
    raw: String,
    kind: IdKind,
}

impl CanonicalId {
    /// Build from kind + ULID bytes (construction path — no validation needed).
    pub fn from_parts(kind: IdKind, ulid: Ulid) -> Self {
        let raw = format!("{}.{}", kind.prefix(), ulid.encode_base32());
        Self { raw, kind }
    }

    /// Parse and validate an external string (boundary path).
    pub fn parse(raw: &str) -> Result<Self> {
        let (kind, ulid_part) = raw
            .strip_prefix("graph.node.")
            .map(|s| (IdKind::Node, s))
            .or_else(|| raw.strip_prefix("graph.edge.").map(|s| (IdKind::Edge, s)))
            .ok_or_else(|| GraphError::InvalidArgument {
                field: "canonical_id",
                reason: "must start with 'graph.node.' or 'graph.edge.'".into(),
            })?;
        if ulid_part.len() != 26 {
            return Err(GraphError::InvalidArgument {
                field: "canonical_id",
                reason: format!("ULID part must be 26 characters, got {}", ulid_part.len()),
            });
        }
        for ch in ulid_part.bytes() {
            if !B32.contains(&ch) {
                return Err(GraphError::InvalidArgument {
                    field: "canonical_id",
                    reason: format!("invalid Crockford base32 character '{}'", ch as char),
                });
            }
        }
        Ok(Self {
            raw: raw.to_string(),
            kind,
        })
    }

    /// Entity class.
    pub fn kind(&self) -> IdKind {
        self.kind
    }

    /// 26-character ULID part.
    pub fn ulid_part(&self) -> &str {
        self.raw
            .strip_prefix("graph.node.")
            .or_else(|| self.raw.strip_prefix("graph.edge."))
            .expect("invariant: raw always carries the kind prefix")
    }

    /// Raw string form.
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for CanonicalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Serialize for CanonicalId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for CanonicalId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        CanonicalId::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Generates canonical ids (injectable for deterministic tests).
pub trait IdGenerator {
    /// Next canonical id of the given kind.
    fn next(&mut self, kind: IdKind) -> CanonicalId;
}

/// Default generator: SystemTime milliseconds + counter-mixed entropy.
#[derive(Default)]
pub struct DefaultIdGenerator {
    counter: AtomicU64,
    last_ms: AtomicU64,
}

impl IdGenerator for DefaultIdGenerator {
    fn next(&mut self, kind: IdKind) -> CanonicalId {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock monotonic")
            .as_millis() as u64;
        let c = self.counter.fetch_add(1, Ordering::Relaxed);
        self.last_ms.store(ms, Ordering::Relaxed);

        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write_u64(ms);
        h.write_u64(c);
        h.write_u64(std::process::id() as u64);
        let a = h.finish();
        let mut h2 = std::collections::hash_map::DefaultHasher::new();
        h2.write_u64(a);
        h2.write_u64(c.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let b = h2.finish();

        let mut bytes = [0u8; 16];
        // 48-bit millisecond timestamp, big-endian, in the first 6 bytes.
        for (i, byte) in bytes[..6].iter_mut().enumerate() {
            *byte = (ms >> (8 * (5 - i))) as u8;
        }
        // 80-bit entropy in the last 10 bytes.
        for (i, byte) in bytes[6..14].iter_mut().enumerate() {
            *byte = (a >> (8 * i)) as u8;
        }
        bytes[14] = (b >> 8) as u8;
        bytes[15] = b as u8;
        CanonicalId::from_parts(kind, Ulid(bytes))
    }
}

/// Deterministic generator for contract tests (counter in the timestamp field).
#[derive(Default)]
pub struct DeterministicIdGenerator {
    node_seq: u64,
    edge_seq: u64,
}

impl IdGenerator for DeterministicIdGenerator {
    fn next(&mut self, kind: IdKind) -> CanonicalId {
        let seq = match kind {
            IdKind::Node => {
                self.node_seq += 1;
                self.node_seq
            }
            IdKind::Edge => {
                self.edge_seq += 1;
                self.edge_seq
            }
        };
        let mut bytes = [0u8; 16];
        for (i, byte) in bytes[..6].iter_mut().enumerate() {
            *byte = (seq >> (8 * (5 - i))) as u8;
        }
        CanonicalId::from_parts(kind, Ulid(bytes))
    }
}
