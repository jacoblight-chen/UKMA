"""Generate valid/invalid fixtures for the 16 parity operations (CodeGraph gap
closure). Valid `input`/`data` are derived from the wire type schemas so they
pass the composed schema; the invalid fixtures are shape-independent."""

import json
from pathlib import Path

SCHEMA_DIR = Path("contracts/schema/v1")
FIXTURE_DIR = Path("contracts/fixtures/v1")

WIRE = json.loads((SCHEMA_DIR / "wire.schema.json").read_text(encoding="utf-8"))
SHARED = json.loads((SCHEMA_DIR / "shared.schema.json").read_text(encoding="utf-8"))

# op_id -> (input wire def, data wire def)
OPS = {
    "code.get_ai_context": ("AiContextQuery", "AiContextResult"),
    "code.get_curated_context": ("CuratedContextQuery", "CuratedContext"),
    "code.analyze_complexity": ("ComplexityQuery", "Array_of_ComplexityReport"),
    "code.find_circular_deps": ("CircularDepsQuery", "Page_for_CircularDependency"),
    "code.find_hot_paths": ("HotPathsQuery", "Array_of_HotPath"),
    "code.find_dead_imports": ("DeadImportsQuery", "Page_for_DeadImport"),
    "code.get_module_summary": ("ModuleSummaryQuery", "Page_for_ModuleSummary"),
    "code.search_by_pattern": ("PatternSearchQuery", "Page_for_PatternMatch"),
    "code.search_by_error": ("ErrorSearchQuery", "Page_for_ErrorMatch"),
    "code.find_by_signature": ("SignatureQuery", "Page_for_CodeSymbol"),
    "code.find_entry_points": ("EntryPointsQuery", "Page_for_EntryPoint"),
    "code.find_related_tests": ("RelatedTestsQuery", "Page_for_RelatedTest"),
    "code.index_files": ("IndexFiles", "IndexedRevision"),
    "code.pr_context": ("PrContextQuery", "PrContextReport"),
    "document.list_doc_sources": ("ListDocSources", "Page_for_DocSourceSummary"),
    "document.generate_architecture_doc": ("GenerateArchitectureDoc", "ArchitectureDoc"),
}

DEFS = {}
DEFS.update(WIRE.get("$defs", {}))
DEFS.update(SHARED.get("$defs", {}))


def resolve_ref(name: str) -> dict:
    # refs may be "wire.schema.json#/$defs/X" or "#/$defs/X" or bare "X"
    for sep in ("#/$defs/", "/"):
        if sep in name:
            name = name.rsplit(sep, 1)[1]
    return DEFS[name]


def pattern_value(pattern: str):
    p = pattern
    if p.startswith("^") and ":[0-9a-f]" in p:  # Hash-like
        return "abc:01234567"
    if p in ("^[A-Za-z0-9_-]+$", "^[A-Za-z0-9_.-]+$"):
        return "abc123"
    if "date-time" in p:
        return "2024-01-01T00:00:00Z"
    return "x"


def gen(schema: dict, depth: int = 0) -> object:
    if depth > 12:
        return "x"
    # Combine top-level hints with any allOf/anyOf/oneOf branch hints.
    merged = dict(schema)
    for key in ("allOf", "anyOf", "oneOf"):
        for branch in schema.get(key, []):
            if isinstance(branch, dict):
                for k, v in branch.items():
                    merged.setdefault(k, v)
    if "const" in merged:
        return merged["const"]
    if "enum" in merged:
        return merged["enum"][0]
    if "$ref" in merged:
        return gen(resolve_ref(merged["$ref"]), depth + 1)
    typ = merged.get("type", "object")
    if isinstance(typ, list):
        typ = "null" in typ and "null" or typ[0]
    if typ == "string":
        return pattern_value(merged["pattern"]) if "pattern" in merged else "x"
    if typ in ("integer", "number"):
        return 0
    if typ == "boolean":
        return True
    if typ == "array":
        if merged.get("minItems", 0) >= 1:
            items = merged.get("items", {})
            return [gen(items, depth + 1)]
        return []
    if typ == "object":
        out = {}
        for prop in merged.get("required", []):
            out[prop] = gen(merged.get("properties", {}).get(prop, {}), depth + 1)
        return out
    return "x"


def wire_instance(type_name: str) -> object:
    return gen(DEFS[type_name])


CONTEXT = {
    "request_id": "x",
    "trace_id": "x",
    "scope": {"tenant_id": "x", "project_id": "x"},
    "principal_id": "x",
    "scopes": [],
    "consistency": "eventual",
}


def valid_request(op: str, input_obj: object) -> dict:
    return {
        "api_version": "1",
        "operation": "ukma." + op + ".v1",
        "context": dict(CONTEXT),
        "input": input_obj,
    }


def valid_response(op: str, data_obj: object) -> dict:
    return {
        "api_version": "1",
        "request_id": "x",
        "trace_id": "x",
        "status": "ok",
        "data": data_obj,
        "warnings": [],
        "partial_reasons": [],
        "freshness": "current",
    }


def invalid_bad_api_version(req: dict) -> dict:
    out = json.loads(json.dumps(req))
    out["api_version"] = "2"
    return out


def invalid_additional_property(req: dict) -> dict:
    out = json.loads(json.dumps(req))
    out["unexpected_field"] = True
    return out


def invalid_missing_request_id(req: dict) -> dict:
    out = json.loads(json.dumps(req))
    out["context"].pop("request_id", None)
    return out


def invalid_ok_without_data(resp: dict) -> dict:
    out = json.loads(json.dumps(resp))
    out.pop("data", None)
    return out


def write(path: Path, obj: object):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


for op, (input_def, data_def) in OPS.items():
    input_obj = wire_instance(input_def)
    data_obj = wire_instance(data_def)
    base = FIXTURE_DIR / op
    req = valid_request(op, input_obj)
    resp = valid_response(op, data_obj)
    write(base / "valid" / (op + ".request.json"), req)
    write(base / "valid" / (op + ".response.json"), resp)
    inv = base / "invalid"
    write(inv / (op + ".request.bad-api-version.json"), invalid_bad_api_version(req))
    write(inv / (op + ".request.additional-property.json"), invalid_additional_property(req))
    write(inv / (op + ".request.missing-request-id.json"), invalid_missing_request_id(req))
    write(inv / (op + ".response.ok-without-data.json"), invalid_ok_without_data(resp))
    print("fixtures for", op, "| input:", input_def, "data:", data_def)

print("done:", len(OPS), "operations")
