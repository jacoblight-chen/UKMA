//! UKMA v1.2.3 contract-closure validator (P0-WP5).
//!
//! Enforces the machine-checkable gates the specification requires:
//! - **DOC-01** 65 domain operations + 4 system operations, ids unique (§43)
//! - **DOC-02** every error code / scope / event is a member of a closed set
//! - **DOC-03** a single registry declares each operation once, with the 15
//!   fields §41.1 requires (§31.1, ADR-034)
//! - **DOC-04** job responses expose orthogonal execution/commit/projection
//!   phases (ADR-037, §40.4)
//! - **SCHEMA-01** all 69 request+response schemas exist, are closed, and their
//!   `$ref`s resolve (§41.2)
//! - **SCHEMA-02** every event carries the 11 registry fields and a closed
//!   payload schema (§40.6)
//! - positive/negative fixtures behave as declared (§41.3)
//! - registry ↔ generated Rust bindings ↔ MCP manifest drift == 0 (PROP-10)
//!
//! Exit code 0 only when every check passes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Value};
use serde_yaml::Value as Yaml;

const REGISTRY: &str = "contracts/api/operation-registry.yaml";
const ERROR_REGISTRY: &str = "contracts/api/error-registry.yaml";
const SCOPE_REGISTRY: &str = "contracts/api/scope-registry.yaml";
const EVENT_REGISTRY: &str = "contracts/events/event-registry.yaml";
const SCHEMA_DIR: &str = "contracts/schema/v1";
const FIXTURE_DIR: &str = "contracts/fixtures/v1";
const MCP_MANIFEST: &str = "contracts/mcp/tool-manifest.json";

/// The 15 fields §41.1 requires on every operation.
const REQUIRED_OPERATION_FIELDS: [&str; 15] = [
    "operation_id",
    "api_major",
    "request_schema",
    "response_schema",
    "required_scopes",
    "preconditions",
    "state_machine",
    "consistency_modes",
    "idempotency",
    "concurrency",
    "error_codes",
    "events",
    "storage_ports",
    "tests",
    "declaration_section",
];

/// The 11 fields §40.6 requires on every registered event.
const REQUIRED_EVENT_FIELDS: [&str; 11] = [
    "event_type",
    "event_version",
    "aggregate_kind",
    "payload_schema_id",
    "required_scopes",
    "producer",
    "consumers",
    "ordering_key",
    "dedup_key",
    "privacy_class",
    "retention_policy",
];

/// Expected operation count per domain (§36.1).
fn expected_counts() -> BTreeMap<&'static str, usize> {
    BTreeMap::from([
        ("code", 13),
        ("document", 14),
        ("memory", 25),
        ("traceability", 13),
        ("system", 4),
    ])
}

#[derive(Default)]
struct Report {
    passed: usize,
    failures: Vec<String>,
    sections: Vec<(String, usize, usize)>,
}

impl Report {
    fn check(&mut self, section: &str, condition: bool, message: impl FnOnce() -> String) {
        if condition {
            self.passed += 1;
            self.bump(section, true);
        } else {
            self.bump(section, false);
            self.failures.push(format!("[{section}] {}", message()));
        }
    }

    fn fail(&mut self, section: &str, message: String) {
        self.bump(section, false);
        self.failures.push(format!("[{section}] {message}"));
    }

    fn bump(&mut self, section: &str, passed: bool) {
        if let Some(entry) = self
            .sections
            .iter_mut()
            .find(|(name, _, _)| name == section)
        {
            if passed {
                entry.1 += 1;
            } else {
                entry.2 += 1;
            }
        } else {
            let (ok, bad) = if passed { (1, 0) } else { (0, 1) };
            self.sections.push((section.to_string(), ok, bad));
        }
    }
}

fn load_yaml(path: &str) -> Result<Yaml, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_yaml::from_str(&raw).map_err(|e| format!("{path}: {e}"))
}

fn load_json(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn as_str(value: &Yaml) -> Option<&str> {
    value.as_str()
}

fn as_seq(value: &Yaml) -> Vec<String> {
    value
        .as_sequence()
        .map(|seq| {
            seq.iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn get<'a>(value: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    value.get(key)
}

/// Rewrites `$ref` pointers of one definition set into a namespaced pointer.
fn namespace_refs(value: &mut Value, prefix: &str) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref").cloned() {
                if let Some(name) = reference.rsplit("#/$defs/").next() {
                    if reference.contains("#/$defs/") {
                        map.insert(
                            "$ref".to_string(),
                            Value::String(format!("#/$defs/{prefix}.{name}")),
                        );
                    }
                }
            }
            for child in map.values_mut() {
                namespace_refs(child, prefix);
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| namespace_refs(item, prefix)),
        _ => {}
    }
}

/// Copies one `$defs` map under a `<prefix>.<name>` namespace.
fn namespace_defs(defs: &Value, prefix: &str) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    if let Some(map) = defs.as_object() {
        for (name, schema) in map {
            let mut cloned = schema.clone();
            namespace_refs(&mut cloned, prefix);
            out.insert(format!("{prefix}.{name}"), cloned);
        }
    }
    out
}

/// Points `wire.schema.json` / `shared.schema.json` refs at the namespaced defs.
fn qualify_envelope_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref").cloned() {
                let qualified = if reference.starts_with("wire.schema.json#/$defs/") {
                    reference
                        .rsplit("#/$defs/")
                        .next()
                        .map(|name| format!("#/$defs/wire.{name}"))
                } else if reference.starts_with("shared.schema.json#/$defs/") {
                    reference
                        .rsplit("#/$defs/")
                        .next()
                        .map(|name| format!("#/$defs/shared.{name}"))
                } else {
                    None
                };
                if let Some(qualified) = qualified {
                    map.insert("$ref".to_string(), Value::String(qualified));
                }
            }
            for child in map.values_mut() {
                qualify_envelope_refs(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(qualify_envelope_refs),
        _ => {}
    }
}

/// Collects the namespaced definition names referenced anywhere in `value`.
fn referenced_names(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                if let Some(name) = reference.strip_prefix("#/$defs/") {
                    out.insert(name.to_string());
                }
            }
            for child in map.values() {
                referenced_names(child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| referenced_names(item, out)),
        _ => {}
    }
}

/// Keeps only the definitions reachable from the document, so a tool schema
/// carries the types its payload actually uses and nothing else.
fn prune_defs(
    document: &Value,
    defs: &mut serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut reachable = BTreeSet::new();
    referenced_names(document, &mut reachable);
    loop {
        let mut added = 0;
        for name in reachable.clone() {
            if let Some(schema) = defs.get(&name) {
                let mut found = BTreeSet::new();
                referenced_names(schema, &mut found);
                for reference in found {
                    if reachable.insert(reference) {
                        added += 1;
                    }
                }
            }
        }
        if added == 0 {
            break;
        }
    }
    defs.iter()
        .filter(|(name, _)| reachable.contains(*name))
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect()
}

/// Inline the `shared.schema.json` and `wire.schema.json` definitions so a
/// schema can be validated standalone. Both sets are namespaced because they
/// describe different type universes: the envelope types and the payload types.
fn compose(schema: &Value, shared_defs: &Value, wire_defs: &Value) -> Value {
    let mut doc = schema.clone();
    qualify_envelope_refs(&mut doc);
    let mut defs = namespace_defs(wire_defs, "wire");
    defs.extend(namespace_defs(shared_defs, "shared"));
    let pruned = prune_defs(&doc, &mut defs);
    doc.as_object_mut()
        .expect("schema root is an object")
        .insert("$defs".to_string(), Value::Object(pruned));
    doc
}

/// Collect every `$ref` string in a JSON document.
fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref" {
                    if let Some(text) = child.as_str() {
                        out.push(text.to_string());
                    }
                }
                collect_refs(child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
        _ => {}
    }
}

fn validate(
    schema: &Value,
    shared_defs: &Value,
    wire_defs: &Value,
    instance: &Value,
) -> Result<(), String> {
    let composed = compose(schema, shared_defs, wire_defs);
    let compiled =
        jsonschema::JSONSchema::compile(&composed).map_err(|e| format!("schema: {e}"))?;
    let outcome = compiled.validate(instance);
    match outcome {
        Ok(()) => Ok(()),
        Err(errors) => {
            let mut messages: Vec<String> = Vec::new();
            for error in errors {
                messages.push(error.to_string());
            }
            Err(messages.join("; "))
        }
    }
}
fn section_of(registry: &Yaml) -> Vec<String> {
    registry
        .get("operations")
        .and_then(Yaml::as_sequence)
        .map(|seq| {
            seq.iter()
                .filter_map(|item| get(item, "operation_id").and_then(as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn main() -> ExitCode {
    let mut report = Report::default();

    let registry = match load_yaml(REGISTRY) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("fatal: {error}");
            return ExitCode::FAILURE;
        }
    };
    let error_registry = load_yaml(ERROR_REGISTRY).unwrap_or(Yaml::Null);
    let scope_registry = load_yaml(SCOPE_REGISTRY).unwrap_or(Yaml::Null);
    let event_registry = load_yaml(EVENT_REGISTRY).unwrap_or(Yaml::Null);

    let closed_codes: BTreeSet<String> = error_registry
        .get("error_codes")
        .and_then(Yaml::as_sequence)
        .map(|seq| {
            seq.iter()
                .filter_map(|item| get(item, "code").and_then(as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let known_scopes: BTreeSet<String> = scope_registry
        .get("scopes")
        .and_then(Yaml::as_sequence)
        .map(|seq| {
            seq.iter()
                .filter_map(|item| get(item, "scope").and_then(as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let event_entries: Vec<&Yaml> = event_registry
        .get("events")
        .and_then(Yaml::as_sequence)
        .map(|seq| seq.iter().collect())
        .unwrap_or_default();
    let known_events: BTreeSet<String> = event_entries
        .iter()
        .filter_map(|item| get(item, "event_type").and_then(as_str).map(str::to_string))
        .collect();

    report.check("DOC-02", !closed_codes.is_empty(), || {
        format!("error registry missing or empty ({ERROR_REGISTRY})")
    });
    report.check("DOC-02", !known_scopes.is_empty(), || {
        format!("scope registry missing or empty ({SCOPE_REGISTRY})")
    });
    report.check("SCHEMA-02", !known_events.is_empty(), || {
        format!("event registry missing or empty ({EVENT_REGISTRY})")
    });

    let operations: Vec<&Yaml> = registry
        .get("operations")
        .and_then(Yaml::as_sequence)
        .map(|seq| seq.iter().collect())
        .unwrap_or_default();

    // ---------- DOC-01 / DOC-03: operation catalog ----------
    report.check("DOC-01", operations.len() == 69, || {
        format!(
            "expected 69 operations (65 domain + 4 system), found {}",
            operations.len()
        )
    });

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for operation in &operations {
        let id = get(operation, "operation_id")
            .and_then(as_str)
            .unwrap_or("<missing>")
            .to_string();
        if !seen.insert(id.clone()) {
            report.fail("DOC-03", format!("duplicate operation id: {id}"));
        }
        if let Some(domain) = get(operation, "domain").and_then(as_str) {
            *counts.entry(domain.to_string()).or_default() += 1;
        }
    }
    for (domain, expected) in expected_counts() {
        let actual = counts.get(domain).copied().unwrap_or(0);
        report.check("DOC-01", actual == expected, || {
            format!("{domain}: expected {expected} operations, found {actual}")
        });
    }
    let ids = section_of(&registry);

    // ---------- §41.1: 15 fields, uri/tool derivation, idempotency coherence ----------
    for operation in &operations {
        let id = get(operation, "operation_id")
            .and_then(as_str)
            .unwrap_or("<missing>");
        for field in REQUIRED_OPERATION_FIELDS {
            report.check("DOC-03", get(operation, field).is_some(), || {
                format!("{id}: missing required field `{field}` (§41.1)")
            });
        }
        let uri = get(operation, "operation_uri")
            .and_then(as_str)
            .unwrap_or("");
        let expected_uri = format!("ukma.{id}.v1");
        report.check("DOC-03", uri == expected_uri, || {
            format!("{id}: operation_uri `{uri}` != `{expected_uri}` (§31.2)")
        });
        if let Some(domain) = get(operation, "domain").and_then(as_str) {
            let tool = get(operation, "mcp_tool").and_then(as_str).unwrap_or("");
            let operation_name = id.split_once('.').map(|(_, rest)| rest).unwrap_or(id);
            let expected_tool = format!("ukma_{domain}_{operation_name}_v1");
            report.check("DOC-03", tool == expected_tool, || {
                format!("{id}: mcp_tool `{tool}` != `{expected_tool}` (§36.4)")
            });
        }
        let idempotency = get(operation, "idempotency").and_then(as_str).unwrap_or("");
        let state = get(operation, "state_machine")
            .and_then(as_str)
            .unwrap_or("");
        let read_only = state == "read_only";
        report.check("DOC-03", (idempotency == "required") == !read_only, || {
            format!(
                "{id}: idempotency `{idempotency}` inconsistent with state machine `{state}` (§31.4)"
            )
        });
    }

    // ---------- DOC-02: closed error codes / scopes / events ----------
    for operation in &operations {
        let id = get(operation, "operation_id")
            .and_then(as_str)
            .unwrap_or("<missing>");
        for code in get(operation, "error_codes")
            .map(as_seq)
            .unwrap_or_default()
        {
            report.check("DOC-02", closed_codes.contains(&code), || {
                format!("{id}: error code `{code}` is not in the closed ErrorCode set")
            });
        }
        for scope in get(operation, "required_scopes")
            .map(as_seq)
            .unwrap_or_default()
        {
            report.check("DOC-02", known_scopes.contains(&scope), || {
                format!("{id}: scope `{scope}` is not declared in the scope registry")
            });
        }
        for event in get(operation, "events").map(as_seq).unwrap_or_default() {
            report.check("SCHEMA-02", known_events.contains(&event), || {
                format!("{id}: event `{event}` is not registered (§40.6)")
            });
        }
    }

    // ---------- SCHEMA-02: event registry fields + payload schemas ----------
    for entry in &event_entries {
        let event_type = get(entry, "event_type")
            .and_then(as_str)
            .unwrap_or("<missing>");
        for field in REQUIRED_EVENT_FIELDS {
            report.check("SCHEMA-02", get(entry, field).is_some(), || {
                format!("{event_type}: missing registry field `{field}` (§40.6)")
            });
        }
        let path = get(entry, "payload_schema_path")
            .and_then(as_str)
            .unwrap_or("");
        if path.is_empty() {
            report.fail("SCHEMA-02", format!("{event_type}: no payload_schema_path"));
            continue;
        }
        match load_json(Path::new(path)) {
            Ok(schema) => {
                report.check("SCHEMA-02", schema.get("$schema").is_some(), || {
                    format!("{path}: missing $schema")
                });
                report.check(
                    "SCHEMA-02",
                    schema.get("additionalProperties") == Some(&Value::Bool(false)),
                    || format!("{path}: payload must set additionalProperties:false (§40.6)"),
                );
            }
            Err(error) => report.fail("SCHEMA-02", error),
        }
    }

    // ---------- SCHEMA-01: operation schemas ----------
    let shared_path = Path::new(SCHEMA_DIR).join("shared.schema.json");
    let shared = match load_json(&shared_path) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("fatal: {error}");
            return ExitCode::FAILURE;
        }
    };
    let shared_defs = shared.get("$defs").cloned().unwrap_or_else(|| json!({}));
    report.check(
        "SCHEMA-01",
        shared_defs.as_object().is_some_and(|o| !o.is_empty()),
        || "shared.schema.json has no $defs (all cross-schema $refs would dangle)".to_string(),
    );
    let wire_path = Path::new(SCHEMA_DIR).join("wire.schema.json");
    let wire = match load_json(&wire_path) {
        Ok(value) => value,
        Err(error) => {
            report.fail(
                "SCHEMA-03",
                format!("{error} (run `cargo run -p ukma-wire-schema`)"),
            );
            json!({})
        }
    };
    let wire_defs = wire.get("$defs").cloned().unwrap_or_else(|| json!({}));
    report.check(
        "SCHEMA-03",
        wire_defs.as_object().is_some_and(|defs| !defs.is_empty()),
        || "wire.schema.json has no $defs (payload types would dangle)".to_string(),
    );
    let wire_operations = wire
        .get("x-operations")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    for operation in &operations {
        let id = get(operation, "operation_id")
            .and_then(as_str)
            .unwrap_or("<missing>");
        for (kind, key) in [
            ("request", "request_schema"),
            ("response", "response_schema"),
        ] {
            let path = PathBuf::from(SCHEMA_DIR).join(format!("{id}.{kind}.schema.json"));
            let schema = match load_json(&path) {
                Ok(schema) => schema,
                Err(error) => {
                    report.fail("SCHEMA-01", format!("{id}: {error}"));
                    continue;
                }
            };
            report.check("SCHEMA-01", schema.get("$schema").is_some(), || {
                format!("{id}.{kind}: missing $schema")
            });
            report.check("SCHEMA-01", schema.get("type").is_some(), || {
                format!("{id}.{kind}: missing type")
            });
            report.check("SCHEMA-01", schema.get("required").is_some(), || {
                format!("{id}.{kind}: missing required")
            });
            report.check(
                "SCHEMA-01",
                schema.get("additionalProperties") == Some(&Value::Bool(false)),
                || format!("{id}.{kind}: root must set additionalProperties:false (§41.2)"),
            );
            if let Some(declared) = get(operation, key).and_then(as_str) {
                let actual = schema.get("$id").and_then(Value::as_str).unwrap_or("");
                report.check("SCHEMA-01", actual == declared, || {
                    format!("{id}.{kind}: $id `{actual}` != registry `{declared}`")
                });
            }
            let mut refs = Vec::new();
            collect_refs(&schema, &mut refs);
            let local_defs = schema.get("$defs").and_then(Value::as_object);
            for reference in refs {
                let resolvable = match reference.rsplit("#/$defs/").next() {
                    Some(name) if reference.contains("#/$defs/") => {
                        let in_wire = reference.starts_with("wire.schema.json#/$defs/")
                            || (!reference.starts_with("shared.schema.json#/$defs/")
                                && !reference.starts_with('#')
                                && wire_defs.get(name).is_some());
                        let in_shared = reference.starts_with("shared.schema.json#/$defs/")
                            || (!reference.starts_with("wire.schema.json#/$defs/")
                                && !reference.starts_with('#')
                                && shared_defs.get(name).is_some());
                        in_wire
                            || in_shared
                            || local_defs.is_some_and(|defs| defs.contains_key(name))
                    }
                    _ => false,
                };
                report.check("SCHEMA-01", resolvable, || {
                    format!("{id}.{kind}: unresolvable $ref `{reference}`")
                });
            }
            let pointer = if kind == "request" {
                "/properties/input"
            } else {
                "/properties/data"
            };
            let payload = schema.pointer(pointer);
            let typed = payload.is_some_and(|value| {
                value.get("$ref").is_some()
                    || (value.get("type").and_then(Value::as_str) == Some("object")
                        && value
                            .get("properties")
                            .and_then(Value::as_object)
                            .is_some_and(|props| !props.is_empty()))
            });
            report.check("SCHEMA-03", typed, || {
                format!("{id}.{kind}: payload is not a typed schema (closure, §R1)")
            });
            if !id.starts_with("system.") {
                let entry = wire_operations.get(id);
                report.check("SCHEMA-03", entry.is_some(), || {
                    format!("{id}: no generated wire types (run `cargo run -p ukma-wire-schema`)")
                });
                for role in ["request", "response"] {
                    let name = entry
                        .and_then(|entry| entry.get(role))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    report.check("SCHEMA-03", wire_defs.get(name).is_some(), || {
                        format!("{id}: {role} type `{name}` missing from wire.schema.json")
                    });
                }
            }
            if kind == "request" {
                let declared = get(operation, "operation_uri")
                    .and_then(as_str)
                    .unwrap_or("");
                let actual = schema
                    .pointer("/properties/operation/const")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                report.check("SCHEMA-01", actual == declared, || {
                    format!("{id}.request: operation const `{actual}` != `{declared}`")
                });
            } else {
                let clauses = schema
                    .get("allOf")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let has = |needle: &str| clauses.iter().any(|c| c.to_string().contains(needle));
                report.check("SCHEMA-01", has("\"ok\"") && has("\"data\""), || {
                    format!("{id}.response: missing `status=ok => data` clause (§41.2)")
                });
                report.check("SCHEMA-01", has("\"error\""), || {
                    format!("{id}.response: missing `status=error => error` clause (§41.2)")
                });
                report.check("SCHEMA-01", has("\"partial\""), || {
                    format!(
                        "{id}.response: missing `status=partial => partial_reasons` clause (§41.2)"
                    )
                });
            }
        }
    }

    // ---------- §41.3: fixtures behave as declared ----------
    for operation in &operations {
        let id = get(operation, "operation_id")
            .and_then(as_str)
            .unwrap_or("<missing>");
        let base = PathBuf::from(FIXTURE_DIR).join(id);
        let request_schema =
            load_json(&PathBuf::from(SCHEMA_DIR).join(format!("{id}.request.schema.json"))).ok();
        let response_schema =
            load_json(&PathBuf::from(SCHEMA_DIR).join(format!("{id}.response.schema.json"))).ok();

        let valid_request = base.join("valid").join(format!("{id}.request.json"));
        let valid_response = base.join("valid").join(format!("{id}.response.json"));
        match (request_schema.as_ref(), load_json(&valid_request)) {
            (Some(schema), Ok(instance)) => {
                let outcome = validate(schema, &shared_defs, &wire_defs, &instance);
                report.check("FIXTURES", outcome.is_ok(), || {
                    format!(
                        "{id}: valid request fixture rejected: {}",
                        outcome.unwrap_err()
                    )
                });
            }
            _ => report.fail("FIXTURES", format!("{id}: missing valid request fixture")),
        }
        match (response_schema.as_ref(), load_json(&valid_response)) {
            (Some(schema), Ok(instance)) => {
                let outcome = validate(schema, &shared_defs, &wire_defs, &instance);
                report.check("FIXTURES", outcome.is_ok(), || {
                    format!(
                        "{id}: valid response fixture rejected: {}",
                        outcome.unwrap_err()
                    )
                });
            }
            _ => report.fail("FIXTURES", format!("{id}: missing valid response fixture")),
        }

        let invalid_dir = base.join("invalid");
        let mut invalid_count = 0;
        if let Ok(entries) = std::fs::read_dir(&invalid_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                invalid_count += 1;
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                let schema = if name.contains(".response.") {
                    response_schema.as_ref()
                } else {
                    request_schema.as_ref()
                };
                let Some(schema) = schema else { continue };
                match load_json(&path) {
                    Ok(instance) => {
                        report.check(
                            "FIXTURES",
                            validate(schema, &shared_defs, &wire_defs, &instance).is_err(),
                            || format!("{id}: invalid fixture `{name}` was accepted by the schema"),
                        );
                    }
                    Err(error) => report.fail("FIXTURES", error),
                }
            }
        }
        report.check("FIXTURES", invalid_count >= 2, || {
            format!("{id}: need >=2 negative fixtures, found {invalid_count} (§41.3)")
        });
    }

    // ---------- PROP-10: drift against generated Rust bindings ----------
    let generated = &ukma_contracts::generated::OPERATIONS;
    report.check("DRIFT", generated.len() == operations.len(), || {
        format!(
            "generated Rust has {} operations, registry has {}",
            generated.len(),
            operations.len()
        )
    });
    for (index, operation) in operations.iter().enumerate() {
        let id = get(operation, "operation_id")
            .and_then(as_str)
            .unwrap_or("<missing>");
        let Some(spec) = generated.get(index) else {
            continue;
        };
        report.check("DRIFT", spec.operation_id == id, || {
            format!(
                "index {index}: generated `{}` != registry `{id}`",
                spec.operation_id
            )
        });
        let scopes = get(operation, "required_scopes")
            .map(as_seq)
            .unwrap_or_default();
        report.check(
            "DRIFT",
            spec.required_scopes == scopes.iter().map(String::as_str).collect::<Vec<_>>(),
            || format!("{id}: required_scopes drift"),
        );
        let tests = get(operation, "tests").map(as_seq).unwrap_or_default();
        report.check(
            "DRIFT",
            spec.tests == tests.iter().map(String::as_str).collect::<Vec<_>>(),
            || format!("{id}: tests drift"),
        );
        let events = get(operation, "events").map(as_seq).unwrap_or_default();
        report.check(
            "DRIFT",
            spec.events == events.iter().map(String::as_str).collect::<Vec<_>>(),
            || format!("{id}: events drift"),
        );
        let errors = get(operation, "error_codes")
            .map(as_seq)
            .unwrap_or_default();
        report.check(
            "DRIFT",
            spec.error_codes == errors.iter().map(String::as_str).collect::<Vec<_>>(),
            || format!("{id}: error_codes drift"),
        );
        let idempotency = get(operation, "idempotency").and_then(as_str).unwrap_or("");
        report.check(
            "DRIFT",
            (spec.idempotency == ukma_contracts::generated::Idempotency::Required)
                == (idempotency == "required"),
            || format!("{id}: idempotency drift"),
        );
    }

    // ---------- PROP-10: drift against the MCP manifest ----------
    match load_json(Path::new(MCP_MANIFEST)) {
        Ok(manifest) => {
            let tools = manifest
                .get("tools")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            report.check("DRIFT", tools.len() == operations.len(), || {
                format!(
                    "MCP manifest has {} tools, registry has {} operations",
                    tools.len(),
                    operations.len()
                )
            });
            let manifest_ids: BTreeSet<String> = tools
                .iter()
                .filter_map(|tool| tool.get("operation_id").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            let registry_ids: BTreeSet<String> = ids.iter().cloned().collect();
            report.check("DRIFT", manifest_ids == registry_ids, || {
                let missing: Vec<_> = registry_ids.difference(&manifest_ids).cloned().collect();
                let extra: Vec<_> = manifest_ids.difference(&registry_ids).cloned().collect();
                format!("MCP manifest drift — missing: {missing:?}, extra: {extra:?}")
            });
            for tool in &tools {
                let path = tool
                    .get("input_schema")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let operation_id = tool
                    .get("operation_id")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>");
                report.check(
                    "DRIFT",
                    !path.is_empty() && Path::new(path).exists(),
                    || format!("MCP tool input schema missing: `{path}`"),
                );
                // The MCP tool schema is the composed request schema, so a stale
                // copy cannot advertise an untyped payload.
                let canonical =
                    PathBuf::from(SCHEMA_DIR).join(format!("{operation_id}.request.schema.json"));
                if let (Ok(tool_schema), Ok(request_schema)) =
                    (load_json(Path::new(path)), load_json(&canonical))
                {
                    let expected = compose(&request_schema, &shared_defs, &wire_defs);
                    report.check("SCHEMA-03", tool_schema == expected, || {
                        format!(
                            "{operation_id}: `{path}` drifted from the canonical request schema"
                        )
                    });
                } else {
                    report.fail(
                        "SCHEMA-03",
                        format!(
                            "{operation_id}: cannot compare `{path}` with `{}`",
                            canonical.display()
                        ),
                    );
                }
            }
        }
        Err(error) => report.fail("DRIFT", error),
    }

    // ---------- SCHEMA-03: no unresolved payload placeholders anywhere ----------
    for directory in [SCHEMA_DIR, "contracts/mcp/tools"] {
        let mut scanned = 0;
        let mut unresolved: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                scanned += 1;
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for marker in ["UNRESOLVED TYPE", "NOT CLOSED", "PROVISIONAL"] {
                    if text.contains(marker) {
                        unresolved.push(format!("{}: {marker}", path.display()));
                    }
                }
            }
        }
        report.check("SCHEMA-03", scanned > 0, || {
            format!("no JSON schemas found under `{directory}`")
        });
        report.check("SCHEMA-03", unresolved.is_empty(), || {
            format!("unresolved payload placeholders remain: {unresolved:?}")
        });
    }

    // ---------- DOC-04: orthogonal execution/commit/projection (§40.4) ----------
    // A submit operation returns a JobHandle; the orthogonal phases must be
    // observable through a registered polling operation returning
    // `OperationOutcome` (ADR-037).
    let phase_markers = ["execution", "commit", "projection"];
    let outcome_spec = shared_defs.get("OperationOutcome");
    let outcome_exposes_phases = outcome_spec.is_some_and(|spec| {
        let text = spec.to_string();
        phase_markers.iter().all(|marker| text.contains(marker))
    });
    report.check("DOC-04", outcome_exposes_phases, || {
        "shared OperationOutcome must expose execution/commit/projection (§40.4)".to_string()
    });
    let poll_ops: Vec<&str> = operations
        .iter()
        .filter(|operation| {
            get(operation, "operation_id").and_then(as_str) == Some("system.get_job")
        })
        .filter_map(|operation| get(operation, "operation_id").and_then(as_str))
        .collect();
    report.check("DOC-04", !poll_ops.is_empty(), || {
        "no job polling operation registered (system.get_job) — phases unobservable (§40.4)"
            .to_string()
    });
    let submit_ops = operations
        .iter()
        .filter(|operation| get(operation, "state_machine").and_then(as_str) == Some("job_submit"))
        .count();
    report.check("DOC-04", submit_ops > 0, || {
        "no job_submit operations found — registry inconsistent".to_string()
    });

    // ---------- output ----------
    println!("UKMA v1.2.3 contract-closure check");
    println!("{}", "-".repeat(60));
    for (section, ok, bad) in &report.sections {
        let status = if *bad == 0 { "PASS" } else { "FAIL" };
        println!("{section:<12} {status}  ok={ok:<5} failed={bad}");
    }
    println!("{}", "-".repeat(60));
    println!("checks passed: {}", report.passed);
    println!("checks failed: {}", report.failures.len());
    if !report.failures.is_empty() {
        let mut shown = String::new();
        for failure in report.failures.iter().take(100) {
            let _ = writeln!(shown, "  - {failure}");
        }
        if report.failures.len() > 100 {
            let _ = writeln!(shown, "  ... and {} more", report.failures.len() - 100);
        }
        println!("\nfailures:\n{shown}");
        return ExitCode::FAILURE;
    }
    println!("\nAll contract-closure checks passed.");
    ExitCode::SUCCESS
}
