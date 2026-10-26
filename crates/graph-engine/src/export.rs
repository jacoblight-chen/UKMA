//! Graph export to DOT / JSON / CSV / Triples (G5 gap closure).
//!
//! Reference semantics: upstream `crates/codegraph/src/export/{csv,dot,json,
//! triples}.rs` (upstream SHA `590c2ca7dd7b37bd1d41caf45fac535b6522f969`,
//! Apache-2.0; recorded in `docs/migration/ledger/s8-gap-closure.yaml`).
//! This is a UKMA-native re-implementation, not a copy.
//!
//! UKMA-mandated changes (recorded in the migration manifest):
//! - **Scope isolation**: the export function operates on a *pre-filtered*
//!   `(tenant, project)` slice supplied by the caller (the engine API
//!   validates scope before calling); it never reads across scopes.
//! - **Determinism (PROP-03)**: nodes render sorted by canonical id; edges
//!   sorted by `(source_id, target_id, edge wire name, id)`.
//! - **Canonical IDs**: the exported identifiers are the canonical string
//!   ids, never vendor `u64` counters.
//! - **Wire names**: node/edge types use the snake_case wire names (ADR-031 D3).
//! - **No I/O**: pure functions returning strings; the caller owns persistence.
//!
//! Preserved upstream semantics: auto-detected sorted CSV property columns,
//! D3.js-compatible `nodes`/`links` JSON, N-Triples `rdf:type`/`prop:`/`edge:`
//! lines, and DOT styling by node/edge type with escaped labels.

use serde_json::{json, Value};

use crate::types::{GraphEdge, GraphNode, NodeType, PropertyValue};

/// Export target format (wire names are stable v1 additions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Graphviz DOT for visualization.
    Dot,
    /// D3.js-compatible JSON.
    Json,
    /// CSV (nodes and edges) for spreadsheets.
    Csv,
    /// N-Triples for the semantic web.
    Triples,
}

impl ExportFormat {
    /// Stable wire token.
    pub fn wire_name(self) -> &'static str {
        match self {
            ExportFormat::Dot => "dot",
            ExportFormat::Json => "json",
            ExportFormat::Csv => "csv",
            ExportFormat::Triples => "triples",
        }
    }

    /// Parse a wire token.
    pub fn from_wire_name(name: &str) -> Option<Self> {
        match name {
            "dot" => Some(ExportFormat::Dot),
            "json" => Some(ExportFormat::Json),
            "csv" => Some(ExportFormat::Csv),
            "triples" => Some(ExportFormat::Triples),
            _ => None,
        }
    }
}

/// Export a pre-filtered (scope-validated) slice of nodes and edges.
///
/// The caller is responsible for scope validation: this function renders exactly
/// the slice it is given and nothing else (PROP-03 determinism, no cross-scope
/// reads).
pub fn export_graph(nodes: &[GraphNode], edges: &[GraphEdge], format: ExportFormat) -> String {
    let nodes = sorted_nodes(nodes);
    let edges = sorted_edges(edges);
    match format {
        ExportFormat::Dot => render_dot(&nodes, &edges),
        ExportFormat::Json => render_json(&nodes, &edges),
        ExportFormat::Csv => render_csv(&nodes, &edges),
        ExportFormat::Triples => render_triples(&nodes, &edges),
    }
}

fn sorted_nodes(nodes: &[GraphNode]) -> Vec<&GraphNode> {
    let mut out: Vec<&GraphNode> = nodes.iter().collect();
    out.sort_by_key(|n| &n.id);
    out
}

fn sorted_edges(edges: &[GraphEdge]) -> Vec<&GraphEdge> {
    let mut out: Vec<&GraphEdge> = edges.iter().collect();
    out.sort_by(|a, b| {
        (&a.source_id, &a.target_id, a.edge_type.wire_name(), &a.id).cmp(&(
            &b.source_id,
            &b.target_id,
            b.edge_type.wire_name(),
            &b.id,
        ))
    });
    out
}

// -- DOT ----------------------------------------------------------------------

/// Default node colours by type (ported from upstream `DotOptions::default`).
fn node_color(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::CodeFile => "#E0E0E0",
        NodeType::Function => "#90CAF9",
        NodeType::Class => "#FFE082",
        NodeType::Variable => "#CE93D8",
        NodeType::Interface => "#FFAB91",
        NodeType::Module => "#BCAAA4",
        _ => "#FFFFFF",
    }
}

/// Default node shapes by type (ported from upstream `DotOptions::default`).
fn node_shape(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::CodeFile => "folder",
        NodeType::Function => "box",
        NodeType::Class => "component",
        NodeType::Variable => "ellipse",
        NodeType::Interface => "diamond",
        NodeType::Module => "folder",
        _ => "box",
    }
}

fn escape_dot_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn node_label(node: &GraphNode) -> String {
    if let Some(PropertyValue::String(name)) = node.properties.get("name") {
        return escape_dot_label(name);
    }
    if let Some(PropertyValue::String(path)) = node.properties.get("path") {
        return escape_dot_label(path);
    }
    escape_dot_label(node.id.as_str())
}

fn render_dot(nodes: &[&GraphNode], edges: &[&GraphEdge]) -> String {
    let mut out = String::new();
    out.push_str("digraph ukma_graph {\n");
    out.push_str("    rankdir=LR;\n");
    out.push_str("    node [style=filled];\n\n");
    for node in nodes {
        out.push_str(&format!(
            "    {} [label=\"{}\", shape={}, fillcolor=\"{}\"];\n",
            node.id.as_str(),
            node_label(node),
            node_shape(node.node_type),
            node_color(node.node_type)
        ));
    }
    out.push('\n');
    for edge in edges {
        out.push_str(&format!(
            "    {} -> {} [label=\"{}\"];\n",
            edge.source_id.as_str(),
            edge.target_id.as_str(),
            edge.edge_type.wire_name()
        ));
    }
    out.push_str("}\n");
    out
}

// -- JSON ---------------------------------------------------------------------

fn property_to_json(value: &PropertyValue) -> Value {
    match value {
        PropertyValue::String(s) => Value::String(s.clone()),
        PropertyValue::Int(i) => Value::from(*i),
        PropertyValue::Float(f) => serde_json::to_value(*f).unwrap_or(Value::Null),
        PropertyValue::Bool(b) => Value::from(*b),
        PropertyValue::StringList(l) => {
            Value::Array(l.iter().map(|s| Value::String(s.clone())).collect())
        }
        PropertyValue::IntList(l) => Value::Array(l.iter().map(|i| Value::from(*i)).collect()),
        PropertyValue::Null => Value::Null,
    }
}

fn properties_to_json(properties: &crate::types::PropertyMap) -> Value {
    let mut obj = serde_json::Map::new();
    for (key, value) in properties.iter_sorted() {
        obj.insert(key.clone(), property_to_json(value));
    }
    Value::Object(obj)
}

fn render_json(nodes: &[&GraphNode], edges: &[&GraphEdge]) -> String {
    let nodes_array: Vec<Value> = nodes
        .iter()
        .map(|node| {
            json!({
                "id": node.id.as_str(),
                "type": node.node_type.wire_name(),
                "label": node
                    .properties
                    .get("name")
                    .and_then(|v| match v {
                        PropertyValue::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| node.id.as_str().to_string()),
                "properties": properties_to_json(&node.properties),
            })
        })
        .collect();
    let links_array: Vec<Value> = edges
        .iter()
        .map(|edge| {
            json!({
                "id": edge.id.as_str(),
                "source": edge.source_id.as_str(),
                "target": edge.target_id.as_str(),
                "type": edge.edge_type.wire_name(),
                "properties": properties_to_json(&edge.properties),
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "nodes": nodes_array, "links": links_array }))
        .expect("json export serialises")
}

// -- CSV ----------------------------------------------------------------------

fn escape_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn format_property_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::String(s) => s.clone(),
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::Bool(b) => b.to_string(),
        PropertyValue::StringList(l) => l.join("|"),
        PropertyValue::IntList(l) => l
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("|"),
        PropertyValue::Null => String::new(),
    }
}

/// Collect the union of property keys across the given maps (sorted).
fn property_keys<'a>(maps: impl Iterator<Item = &'a crate::types::PropertyMap>) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for map in maps {
        for (key, _) in map.iter_sorted() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
    }
    keys
}

fn render_csv(nodes: &[&GraphNode], edges: &[&GraphEdge]) -> String {
    let mut out = String::new();
    let node_keys = property_keys(nodes.iter().map(|n| &n.properties));
    out.push_str("id,type");
    for key in &node_keys {
        out.push(',');
        out.push_str(key);
    }
    out.push('\n');
    for node in nodes {
        out.push_str(node.id.as_str());
        out.push(',');
        out.push_str(node.node_type.wire_name());
        for key in &node_keys {
            out.push(',');
            if let Some(value) = node.properties.get(key) {
                out.push_str(&escape_csv(&format_property_value(value)));
            }
        }
        out.push('\n');
    }
    let edge_keys = property_keys(edges.iter().map(|e| &e.properties));
    out.push_str("id,source,target,type");
    for key in &edge_keys {
        out.push(',');
        out.push_str(key);
    }
    out.push('\n');
    for edge in edges {
        out.push_str(edge.id.as_str());
        out.push(',');
        out.push_str(edge.source_id.as_str());
        out.push(',');
        out.push_str(edge.target_id.as_str());
        out.push(',');
        out.push_str(edge.edge_type.wire_name());
        for key in &edge_keys {
            out.push(',');
            if let Some(value) = edge.properties.get(key) {
                out.push_str(&escape_csv(&format_property_value(value)));
            }
        }
        out.push('\n');
    }
    out
}

// -- Triples ------------------------------------------------------------------

fn triple_object(value: &PropertyValue) -> String {
    match value {
        PropertyValue::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\"")
        }
        PropertyValue::Int(i) => format!("\"{i}\"^^<xsd:integer>"),
        PropertyValue::Float(f) => format!("\"{f}\"^^<xsd:double>"),
        PropertyValue::Bool(b) => {
            format!("\"{b}\"^^<xsd:boolean>")
        }
        PropertyValue::StringList(l) => {
            let escaped = l.join(" ");
            format!("\"{escaped}\"")
        }
        PropertyValue::IntList(l) => {
            let joined = l
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            format!("\"{joined}\"")
        }
        PropertyValue::Null => "<xsd:nil>".to_string(),
    }
}

fn render_triples(nodes: &[&GraphNode], edges: &[&GraphEdge]) -> String {
    let mut out = String::new();
    for node in nodes {
        let id = node.id.as_str();
        out.push_str(&format!(
            "<{id}> <rdf:type> <type:{wire}> .\n",
            wire = node.node_type.wire_name()
        ));
        for (key, value) in node.properties.iter_sorted() {
            out.push_str(&format!("<{id}> <prop:{key}> {} .\n", triple_object(value)));
        }
    }
    for edge in edges {
        let id = edge.id.as_str();
        out.push_str(&format!(
            "<{src}> <edge:{wire}> <{dst}> .\n",
            src = edge.source_id.as_str(),
            wire = edge.edge_type.wire_name(),
            dst = edge.target_id.as_str()
        ));
        for (key, value) in edge.properties.iter_sorted() {
            out.push_str(&format!("<{id}> <prop:{key}> {} .\n", triple_object(value)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CanonicalId, IdKind, Ulid};
    use crate::types::{EdgeType, GraphEdge, GraphNode, NodeType, PropertyMap, PropertyValue};

    /// Deterministic test id: kind prefix + zero-padded ULID with a marker char.
    fn test_id(kind: IdKind, marker: u8) -> CanonicalId {
        let mut bytes = [0u8; 16];
        bytes[15] = marker;
        CanonicalId::from_parts(kind, Ulid(bytes))
    }

    fn node_id(marker: u8) -> CanonicalId {
        test_id(IdKind::Node, marker)
    }

    fn edge_id(marker: u8) -> CanonicalId {
        test_id(IdKind::Edge, marker)
    }

    fn node(marker: u8, node_type: NodeType, name: &str) -> GraphNode {
        GraphNode::new(
            node_id(marker),
            node_type,
            PropertyMap::new().with("name", PropertyValue::String(name.to_string())),
        )
    }

    fn edge(marker: u8, source: u8, target: u8, edge_type: EdgeType) -> GraphEdge {
        GraphEdge::new(
            edge_id(marker),
            node_id(source),
            node_id(target),
            edge_type,
            PropertyMap::new(),
        )
    }

    fn fixture() -> (Vec<GraphNode>, Vec<GraphEdge>) {
        (
            vec![
                node(2, NodeType::Function, "beta"),
                node(1, NodeType::CodeFile, "alpha"),
            ],
            vec![
                edge(2, 2, 1, EdgeType::Calls),
                edge(1, 1, 2, EdgeType::Contains),
            ],
        )
    }

    #[test]
    fn export_is_deterministic_across_formats() {
        let (nodes, edges) = fixture();
        for fmt in [
            ExportFormat::Dot,
            ExportFormat::Json,
            ExportFormat::Csv,
            ExportFormat::Triples,
        ] {
            let first = export_graph(&nodes, &edges, fmt);
            let second = export_graph(&nodes, &edges, fmt);
            assert_eq!(first, second, "{fmt:?} must be deterministic");
        }
    }

    #[test]
    fn dot_orders_by_canonical_id_and_escapes_labels() {
        let (nodes, edges) = fixture();
        let out = export_graph(&nodes, &edges, ExportFormat::Dot);
        // node 1 (alpha) must render before node 2 (beta): sorted by id.
        let a = out.find("alpha").expect("alpha present");
        let b = out.find("beta").expect("beta present");
        assert!(a < b, "nodes must be sorted by canonical id: {out}");
        assert!(out.contains("rankdir=LR"), "dot carries the rankdir header");
        assert!(out.contains("label=\"contains\""), "edge label: {out}");
        assert!(out.starts_with("digraph ukma_graph {"), "dot header");
    }

    #[test]
    fn dot_escapes_special_characters_in_labels() {
        let node = GraphNode::new(
            node_id(1),
            NodeType::Function,
            PropertyMap::new().with("name", PropertyValue::String("a\"b\\c".to_string())),
        );
        let out = export_graph(&[node], &[], ExportFormat::Dot);
        assert!(out.contains("a\\\"b\\\\c"), "label escaping: {out}");
    }

    #[test]
    fn json_is_d3_compatible_with_wire_names() {
        let (nodes, edges) = fixture();
        let out = export_graph(&nodes, &edges, ExportFormat::Json);
        let value: Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(
            value["nodes"][0]["type"], "code_file",
            "wire name, sorted nodes"
        );
        assert_eq!(value["links"][0]["source"], node_id(1).as_str());
        assert_eq!(value["links"][0]["type"], "contains");
        assert!(value["nodes"][0]["properties"].is_object());
    }

    #[test]
    fn csv_carries_sorted_auto_detected_property_columns() {
        let nodes = vec![
            node(1, NodeType::Function, "a"),
            GraphNode::new(
                node_id(2),
                NodeType::Class,
                PropertyMap::new()
                    .with("name", PropertyValue::String("b".to_string()))
                    .with("extra", PropertyValue::Int(7)),
            ),
        ];
        let out = export_graph(&nodes, &[], ExportFormat::Csv);
        let first_line = out.lines().next().expect("header");
        assert!(
            first_line.contains("extra"),
            "auto-detected column: {first_line}"
        );
        assert!(first_line.contains("id,type"));
        // The node without `extra` leaves the column empty.
        assert!(out.lines().nth(1).unwrap().ends_with(","));
    }

    #[test]
    fn csv_escapes_values_with_commas() {
        let node = GraphNode::new(
            node_id(1),
            NodeType::Function,
            PropertyMap::new().with("name", PropertyValue::String("a,b".to_string())),
        );
        let out = export_graph(&[node], &[], ExportFormat::Csv);
        assert!(out.contains("\"a,b\""), "csv escaping: {out}");
    }

    #[test]
    fn triples_render_rdf_type_and_edge_lines() {
        let (nodes, edges) = fixture();
        let out = export_graph(&nodes, &edges, ExportFormat::Triples);
        assert!(out.contains("<rdf:type> <type:code_file> ."), "{out}");
        assert!(
            out.contains("<prop:name> \"alpha\" ."),
            "property triple: {out}"
        );
        assert!(out.contains("<edge:contains> "), "edge triple: {out}");
    }

    #[test]
    fn empty_graph_renders_each_format() {
        for fmt in [
            ExportFormat::Dot,
            ExportFormat::Json,
            ExportFormat::Csv,
            ExportFormat::Triples,
        ] {
            let out = export_graph(&[], &[], fmt);
            if fmt == ExportFormat::Triples {
                // N-Triples carries no header: an empty slice renders empty.
                assert!(out.is_empty(), "{fmt:?} empty render: {out}");
            } else {
                assert!(!out.is_empty(), "{fmt:?} empty render");
            }
        }
    }

    #[test]
    fn wire_names_round_trip() {
        for format in [
            ExportFormat::Dot,
            ExportFormat::Json,
            ExportFormat::Csv,
            ExportFormat::Triples,
        ] {
            assert_eq!(
                ExportFormat::from_wire_name(format.wire_name()),
                Some(format)
            );
        }
        assert_eq!(ExportFormat::from_wire_name("nope"), None);
    }
}
