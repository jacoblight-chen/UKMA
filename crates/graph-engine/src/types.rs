//! Graph domain model (ported from upstream `property.rs` / `types.rs`).
//!
//! Preserved upstream semantics: the strongly-typed `PropertyValue` bag and the
//! builder-style `PropertyMap`; the 8 node types, 12 edge types, 3 directions.
//! UKMA changes (recorded in the source migration manifest):
//! - canonical string ids instead of vendor `u64` counters (ADR-031 D1);
//! - snake_case wire names per ADR-031 D3 (new addition, versioned v1);
//! - `revision` + `etag` optimistic-concurrency fields (Phase 6R pattern).

use std::collections::HashMap;
use std::hash::Hasher;

use serde::{Deserialize, Serialize};

use crate::ids::CanonicalId;

/// Strongly-typed property value for node/edge metadata (ported from upstream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyValue {
    /// String value (names, paths, signatures).
    String(String),
    /// Integer value (line numbers, counts).
    Int(i64),
    /// Floating point value (metrics, scores).
    Float(f64),
    /// Boolean flag (is_public, is_async, is_test).
    Bool(bool),
    /// List of strings (imported symbols, tags).
    StringList(Vec<String>),
    /// List of integers (line ranges, counts).
    IntList(Vec<i64>),
    /// Explicit null/absence of value.
    Null,
}

impl From<String> for PropertyValue {
    fn from(value: String) -> Self {
        PropertyValue::String(value)
    }
}

impl From<&str> for PropertyValue {
    fn from(value: &str) -> Self {
        PropertyValue::String(value.to_string())
    }
}

impl From<i64> for PropertyValue {
    fn from(value: i64) -> Self {
        PropertyValue::Int(value)
    }
}

impl From<i32> for PropertyValue {
    fn from(value: i32) -> Self {
        PropertyValue::Int(value as i64)
    }
}

impl From<f64> for PropertyValue {
    fn from(value: f64) -> Self {
        PropertyValue::Float(value)
    }
}

impl From<bool> for PropertyValue {
    fn from(value: bool) -> Self {
        PropertyValue::Bool(value)
    }
}

impl From<Vec<String>> for PropertyValue {
    fn from(value: Vec<String>) -> Self {
        PropertyValue::StringList(value)
    }
}

impl From<Vec<i64>> for PropertyValue {
    fn from(value: Vec<i64>) -> Self {
        PropertyValue::IntList(value)
    }
}

/// Value discriminator used for deterministic ETag computation.
fn value_fingerprint(v: &PropertyValue) -> u64 {
    match v {
        PropertyValue::String(s) => hash_of(s.as_str()),
        PropertyValue::Int(i) => hash_of(&i.to_string()),
        PropertyValue::Float(f) => hash_of(&f.to_bits().to_string()),
        PropertyValue::Bool(b) => hash_of(if *b { "1" } else { "0" }),
        PropertyValue::StringList(l) => {
            let mut h = hash_of("stringlist");
            for s in l {
                h ^= hash_of(s);
            }
            h
        }
        PropertyValue::IntList(l) => {
            let mut h = hash_of("intlist");
            for i in l {
                h ^= hash_of(&i.to_string());
            }
            h
        }
        PropertyValue::Null => hash_of("null"),
    }
}

fn hash_of(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(s.as_bytes());
    h.finish()
}

/// Flexible key-value metadata store (ported from upstream builder pattern).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertyMap {
    data: HashMap<String, PropertyValue>,
}

impl PropertyMap {
    /// New empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: add a property and return self.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<PropertyValue>) -> Self {
        self.data.insert(key.into(), value.into());
        self
    }

    /// Insert a property value.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<PropertyValue>) {
        self.data.insert(key.into(), value.into());
    }

    /// Get a property value by key.
    pub fn get(&self, key: &str) -> Option<&PropertyValue> {
        self.data.get(key)
    }

    /// Remove a property by key.
    pub fn remove(&mut self, key: &str) -> Option<PropertyValue> {
        self.data.remove(key)
    }

    /// Check if a property exists.
    pub fn contains_key(&self, key: &str) -> bool {
        self.data.contains_key(key)
    }

    /// Number of properties.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Iterate over all properties (sorted by key — deterministic).
    pub fn iter_sorted(&self) -> impl Iterator<Item = (&String, &PropertyValue)> {
        let mut entries: Vec<_> = self.data.iter().collect();
        entries.sort_by_key(|(k, _)| k.as_str());
        entries.into_iter()
    }
}

/// Type of a graph node (8 variants — upstream semantics, snake_case wire, ADR-031 D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    /// Source code file.
    CodeFile,
    /// Function, method, or procedure.
    Function,
    /// Class, struct, or type definition.
    Class,
    /// Module, namespace, or package.
    Module,
    /// Variable, constant, or field.
    Variable,
    /// Type alias or primitive type.
    Type,
    /// Interface, trait, or protocol.
    Interface,
    /// Catch-all for custom entity types.
    Generic,
}

impl NodeType {
    /// Stable name used in ETag computation.
    pub fn wire_name(&self) -> &'static str {
        match self {
            NodeType::CodeFile => "code_file",
            NodeType::Function => "function",
            NodeType::Class => "class",
            NodeType::Module => "module",
            NodeType::Variable => "variable",
            NodeType::Type => "type",
            NodeType::Interface => "interface",
            NodeType::Generic => "generic",
        }
    }
}

/// Type of edge (relationship) between nodes (12 variants — upstream semantics, snake_case wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    /// File A imports File B.
    Imports,
    /// File A imports symbols from File B.
    ImportsFrom,
    /// Parent contains child entity (file contains function).
    Contains,
    /// Function A calls Function B.
    Calls,
    /// Function invokes method on object.
    Invokes,
    /// Function creates instance of class.
    Instantiates,
    /// Class A extends/inherits from Class B.
    Extends,
    /// Class implements interface/trait.
    Implements,
    /// Generic usage relationship.
    Uses,
    /// Module defines entity.
    Defines,
    /// Generic reference.
    References,
    /// Runtime dependency (e.g., HTTP client call → route handler).
    RuntimeCalls,
}

impl EdgeType {
    /// Stable name used in ETag computation.
    pub fn wire_name(&self) -> &'static str {
        match self {
            EdgeType::Imports => "imports",
            EdgeType::ImportsFrom => "imports_from",
            EdgeType::Contains => "contains",
            EdgeType::Calls => "calls",
            EdgeType::Invokes => "invokes",
            EdgeType::Instantiates => "instantiates",
            EdgeType::Extends => "extends",
            EdgeType::Implements => "implements",
            EdgeType::Uses => "uses",
            EdgeType::Defines => "defines",
            EdgeType::References => "references",
            EdgeType::RuntimeCalls => "runtime_calls",
        }
    }
}

/// Direction for neighbor queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow outgoing edges (from this node).
    Outgoing,
    /// Follow incoming edges (to this node).
    Incoming,
    /// Follow edges in both directions.
    Both,
}

/// Deterministic ETag from revision + type + property contents.
pub fn compute_etag(revision: u64, type_name: &str, properties: &PropertyMap) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_u64(revision);
    h.write(type_name.as_bytes());
    for (k, v) in properties.iter_sorted() {
        h.write(k.as_bytes());
        h.write_u64(value_fingerprint(v));
    }
    format!("{:016x}", h.finish())
}

/// A node in the graph (canonical id + type + property bag + version fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    /// Canonical identifier (engine-assigned, persisted).
    pub id: CanonicalId,
    /// Type of entity.
    pub node_type: NodeType,
    /// Flexible key-value metadata.
    pub properties: PropertyMap,
    /// Monotonic revision (optimistic concurrency).
    pub revision: u64,
    /// Deterministic ETag for this revision.
    pub etag: String,
}

impl GraphNode {
    /// New node at revision 1 with computed ETag.
    pub fn new(id: CanonicalId, node_type: NodeType, properties: PropertyMap) -> Self {
        let revision = 1u64;
        let etag = compute_etag(revision, node_type.wire_name(), &properties);
        Self {
            id,
            node_type,
            properties,
            revision,
            etag,
        }
    }

    /// Add or update a property.
    pub fn set_property(&mut self, key: impl Into<String>, value: impl Into<PropertyValue>) {
        self.properties.insert(key, value);
    }

    /// Get a property value.
    pub fn get_property(&self, key: &str) -> Option<&PropertyValue> {
        self.properties.get(key)
    }
}

/// A directed edge in the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Canonical identifier (engine-assigned, persisted).
    pub id: CanonicalId,
    /// Source node id.
    pub source_id: CanonicalId,
    /// Target node id.
    pub target_id: CanonicalId,
    /// Type of relationship.
    pub edge_type: EdgeType,
    /// Optional metadata (e.g., line number for calls).
    pub properties: PropertyMap,
    /// Monotonic revision (optimistic concurrency).
    pub revision: u64,
    /// Deterministic ETag for this revision.
    pub etag: String,
}

impl GraphEdge {
    /// New edge at revision 1 with computed ETag.
    pub fn new(
        id: CanonicalId,
        source_id: CanonicalId,
        target_id: CanonicalId,
        edge_type: EdgeType,
        properties: PropertyMap,
    ) -> Self {
        let revision = 1u64;
        let etag = compute_etag(revision, edge_type.wire_name(), &properties);
        Self {
            id,
            source_id,
            target_id,
            edge_type,
            properties,
            revision,
            etag,
        }
    }

    /// Add or update a property.
    pub fn set_property(&mut self, key: impl Into<String>, value: impl Into<PropertyValue>) {
        self.properties.insert(key, value);
    }

    /// Get a property value.
    pub fn get_property(&self, key: &str) -> Option<&PropertyValue> {
        self.properties.get(key)
    }
}
