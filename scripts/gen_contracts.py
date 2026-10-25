"""P0-WP4 codegen (deleted after run). Single source: operation registry.

Emits:
  crates/ukma-contracts/src/generated/operations.rs   Rust OperationId + spec table
  crates/ukma-contracts/src/generated/mod.rs
  contracts/mcp/tool-manifest.json                    69 MCP tools
  contracts/mcp/tools/<tool>.input.schema.json        self-contained inputSchema per tool
  sdk/typescript/ukma-operations.ts                   TS operation-id union + spec table
"""

import json
from pathlib import Path

import yaml

REG = yaml.safe_load(Path("contracts/api/operation-registry.yaml").read_text(encoding="utf-8"))
SCHEMA_DIR = Path("contracts/schema/v1")
MCP_DIR = Path("contracts/mcp")
GEN_DIR = Path("crates/ukma-contracts/src/generated")
TS_DIR = Path("sdk/typescript")

SHARED = json.loads((SCHEMA_DIR / "shared.schema.json").read_text(encoding="utf-8"))


def compose(schema: dict) -> dict:
    """Inline shared $defs so the schema is self-contained (no file refs)."""
    text = json.dumps(schema)
    text = text.replace("shared.schema.json#/$defs/", "#/$defs/")
    text = text.replace("../schema/v1/shared.schema.json#/$defs/", "#/$defs/")
    out = json.loads(text)
    defs = out.setdefault("$defs", {})
    for name, value in SHARED["$defs"].items():
        defs.setdefault(name, value)
    return out


def pascal(op_id: str) -> str:
    return "".join(part.capitalize() for part in op_id.replace(".", "_").split("_"))


def rust_str_list(values) -> str:
    if not values:
        return "&[]"
    inner = ", ".join(f'"{v}"' for v in values)
    return f"&[{inner}]"


def rust_enum_value(value: str) -> str:
    return {
        "required": "Required",
        "not_required": "NotRequired",
        "expected_etag": "ExpectedEtag",
        "immutable_snapshot": "ImmutableSnapshot",
        "not_applicable": "NotApplicable",
    }[value]


def rust_state_value(value: str) -> str:
    mapping = {
        "read_only": "ReadOnly",
        "job_submit": "JobSubmit",
        "job_cancel": "JobCancel",
        "revision_cas": "RevisionCas",
        "delete_tombstone": "DeleteTombstone",
        "create": "Create",
    }
    return mapping[value]


def generate_rust(ops):
    lines = [
        "//! GENERATED FILE — do not edit by hand.",
        "//!",
        "//! Source: `contracts/api/operation-registry.yaml` (§41.1 single registry).",
        "//! Regenerate: `python scripts/gen_contracts.py`; drift is enforced by",
        "//! `cargo run -p ukma-contract-check`.",
        "",
        "/// Idempotency requirement of an operation (§41.1).",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub enum Idempotency {",
        "    /// Idempotency key is mandatory (all mutating operations).",
        "    Required,",
        "    /// Idempotency key is not applicable (read-only operations).",
        "    NotRequired,",
        "}",
        "",
        "/// Concurrency control of an operation (§41.1).",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub enum Concurrency {",
        "    /// Caller must supply `expected_etag`.",
        "    ExpectedEtag,",
        "    /// Read-only snapshot semantics.",
        "    ImmutableSnapshot,",
        "    /// Creates a new aggregate; no prior revision to compare.",
        "    NotApplicable,",
        "}",
        "",
        "/// State machine class of an operation (§41.1 `state_machine`).",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub enum StateMachine {",
        "    /// No state transition; read only.",
        "    ReadOnly,",
        "    /// Submits a cancellable background job.",
        "    JobSubmit,",
        "    /// Cancels a background job (cancellation does not undo a commit).",
        "    JobCancel,",
        "    /// Revision compare-and-swap with `expected_etag`.",
        "    RevisionCas,",
        "    /// Creates a new aggregate.",
        "    Create,",
        "    /// Deletes with a durable tombstone.",
        "    DeleteTombstone,",
        "}",
        "",
        "/// Fully declared contract of one operation (§41.1).",
        "#[derive(Debug, Clone, Copy)]",
        "pub struct OperationSpec {",
        "    /// Canonical operation id, e.g. `code.find_symbols`.",
        "    pub operation_id: &'static str,",
        "    /// Public API major version.",
        "    pub api_major: u32,",
        "    /// Canonical operation URI, e.g. `ukma.code.find_symbols.v1`.",
        "    pub operation_uri: &'static str,",
        "    /// JSON Schema `$id` of the request schema.",
        "    pub request_schema: &'static str,",
        "    /// JSON Schema `$id` of the response schema.",
        "    pub response_schema: &'static str,",
        "    /// Required authorization scopes (deny-by-default).",
        "    pub required_scopes: &'static [&'static str],",
        "    /// Preconditions that must hold before execution.",
        "    pub preconditions: &'static [&'static str],",
        "    /// State-machine class.",
        "    pub state_machine: StateMachine,",
        "    /// Supported consistency modes.",
        "    pub consistency_modes: &'static [&'static str],",
        "    /// Idempotency requirement.",
        "    pub idempotency: Idempotency,",
        "    /// Concurrency control.",
        "    pub concurrency: Concurrency,",
        "    /// Error codes this operation may return (subset of the closed set).",
        "    pub error_codes: &'static [&'static str],",
        "    /// Domain events emitted on success.",
        "    pub events: &'static [&'static str],",
        "    /// Storage ports this operation depends on.",
        "    pub storage_ports: &'static [&'static str],",
        "    /// Required test ids (§36.1 / §41.3).",
        "    pub tests: &'static [&'static str],",
        "    /// MCP tool name (§36.4).",
        "    pub mcp_tool: &'static str,",
        "    /// REST route (§31.2).",
        "    pub server_method: &'static str,",
        "}",
        "",
        "/// Every registered operation id (§41.1: the single registry).",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]",
        "pub enum OperationId {",
    ]
    for op in ops:
        lines.append(f"    /// `{op['operation_id']}`")
        lines.append(f"    {pascal(op['operation_id'])},")
    lines += [
        "}",
        "",
        "/// All operations, in registry order.",
        "pub const OPERATIONS: &[OperationSpec] = &[",
    ]
    for op in ops:
        lines.append("    OperationSpec {")
        lines.append(f'        operation_id: "{op["operation_id"]}",')
        lines.append(f'        api_major: {op["api_major"]},')
        lines.append(f'        operation_uri: "{op["operation_uri"]}",')
        lines.append(f'        request_schema: "{op["request_schema"]}",')
        lines.append(f'        response_schema: "{op["response_schema"]}",')
        lines.append(f'        required_scopes: {rust_str_list(op["required_scopes"])},')
        lines.append(f'        preconditions: {rust_str_list(op["preconditions"])},')
        lines.append(f'        state_machine: StateMachine::{rust_state_value(op["state_machine"])},')
        lines.append(f'        consistency_modes: {rust_str_list(op["consistency_modes"])},')
        lines.append(f'        idempotency: Idempotency::{rust_enum_value(op["idempotency"])},')
        lines.append(f'        concurrency: Concurrency::{rust_enum_value(op["concurrency"])},')
        lines.append(f'        error_codes: {rust_str_list(op["error_codes"])},')
        lines.append(f'        events: {rust_str_list(op["events"])},')
        lines.append(f'        storage_ports: {rust_str_list(op["storage_ports"])},')
        lines.append(f'        tests: {rust_str_list(op["tests"])},')
        lines.append(f'        mcp_tool: "{op["mcp_tool"]}",')
        lines.append(f'        server_method: "{op["server_method"]}",')
        lines.append("    },")
    lines += [
        "];",
        "",
        "impl OperationId {",
        "    /// Registry id string of this operation.",
        "    pub fn as_str(self) -> &'static str {",
        "        self.spec().operation_id",
        "    }",
        "",
        "    /// Declared contract of this operation.",
        "    pub fn spec(self) -> &'static OperationSpec {",
        "        &OPERATIONS[self as usize]",
        "    }",
        "",
        "    /// Look up an operation by its canonical id.",
        "    pub fn from_id(id: &str) -> Option<Self> {",
        "        OPERATIONS",
        "            .iter()",
        "            .position(|spec| spec.operation_id == id)",
        "            .map(|index| OPERATION_ENUM[index])",
        "    }",
        "}",
        "",
        "/// `OperationId` variants in the same order as [`OPERATIONS`].",
        "pub const OPERATION_ENUM: &[OperationId] = &[",
    ]
    for op in ops:
        lines.append(f"    OperationId::{pascal(op['operation_id'])},")
    lines += ["];", ""]
    return "\n".join(lines)


def main() -> None:
    ops = REG["operations"]
    assert len(ops) == 69, len(ops)

    GEN_DIR.mkdir(parents=True, exist_ok=True)
    (GEN_DIR / "operations.rs").write_text(generate_rust(ops), encoding="utf-8")
    (GEN_DIR / "mod.rs").write_text(
        "//! GENERATED — registry-derived contract bindings.\n"
        "//!\n"
        "//! Regenerate with `python scripts/gen_contracts.py`.\n"
        "\n"
        "pub mod operations;\n"
        "\n"
        "pub use operations::{Concurrency, Idempotency, OperationId, OperationSpec, "
        "StateMachine, OPERATIONS, OPERATION_ENUM};\n",
        encoding="utf-8",
    )

    # ---- MCP tools ----
    MCP_DIR.mkdir(parents=True, exist_ok=True)
    tools_dir = MCP_DIR / "tools"
    tools_dir.mkdir(parents=True, exist_ok=True)
    manifest = {
        "spec_version": "1.2.3",
        "api_major": 1,
        "generated_from": "contracts/api/operation-registry.yaml",
        "source": "docs/UKMA_v1.2.3_Full_Spec.md §36.4 (MCP operation mapping)",
        "rule": "Tool names are registry-derived; a tool absent from the registry MUST be rejected.",
        "tools": [],
    }
    for op in ops:
        tool = op["mcp_tool"]
        request_schema = json.loads(
            (SCHEMA_DIR / f"{op['operation_id']}.request.schema.json").read_text(encoding="utf-8")
        )
        composed = compose(request_schema)
        tool_schema_path = tools_dir / f"{tool}.input.schema.json"
        tool_schema_path.write_text(
            json.dumps(composed, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        manifest["tools"].append({
            "name": tool,
            "description": f"{op['operation_id']} (api v{op['api_major']})",
            "operation_id": op["operation_id"],
            "operation_uri": op["operation_uri"],
            "input_schema": f"contracts/mcp/tools/{tool}.input.schema.json",
            "input_schema_id": op["request_schema"],
            "output_schema_id": op["response_schema"],
            "required_scopes": op["required_scopes"],
            "consistency_modes": op["consistency_modes"],
            "idempotency": op["idempotency"],
            "error_codes": op["error_codes"],
        })
    (MCP_DIR / "tool-manifest.json").write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    # ---- TypeScript ----
    TS_DIR.mkdir(parents=True, exist_ok=True)
    ids = " |\n  | ".join(f"'{op['operation_id']}'" for op in ops)
    rows = ",\n".join(
        "  { id: '%s', uri: '%s', mcp: '%s', scopes: [%s], idempotency: '%s' }"
        % (op["operation_id"], op["operation_uri"], op["mcp_tool"],
           ", ".join(f"'{s}'" for s in op["required_scopes"]), op["idempotency"])
        for op in ops
    )
    ts = (
        "// GENERATED FILE — do not edit by hand.\n"
        "// Source: contracts/api/operation-registry.yaml (spec §41.1).\n"
        "// Regenerate: python scripts/gen_contracts.py\n\n"
        "export type OperationId =\n  | " + ids + ";\n\n"
        "export interface OperationSpec {\n"
        "  readonly id: OperationId;\n"
        "  readonly uri: string;\n"
        "  readonly mcp: string;\n"
        "  readonly scopes: readonly string[];\n"
        "  readonly idempotency: 'required' | 'not_required';\n"
        "}\n\n"
        "export const OPERATIONS: readonly OperationSpec[] = [\n" + rows + ",\n];\n"
    )
    (TS_DIR / "ukma-operations.ts").write_text(ts, encoding="utf-8")

    print(f"rust operations: {len(ops)}")
    print(f"mcp tools: {len(manifest['tools'])} (+{len(manifest['tools'])} input schemas)")
    print(f"ts operation ids: {len(ops)}")


if __name__ == "__main__":
    main()
