#!/usr/bin/env python3
"""One-off generator: per-op request/response schema JSONs for the 15 parity
operations (CodeGraph gap closure). The shapes follow the existing v1.2.3
contract exactly; run once, then delete is acceptable (the files stay)."""

import json
from pathlib import Path

SCHEMA_DIR = Path("contracts/schema/v1")

# op_id -> (request input $def, response data $def)
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

def request_schema(op: str, input_def: str) -> dict:
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": f"https://schemas.ukma.dev/v1/{op}.request.schema.json",
        "title": f"{op} Request (Phase 4 typed candidate)",
        "type": "object",
        "additionalProperties": False,
        "required": ["api_version", "operation", "context", "input"],
        "properties": {
            "api_version": {"const": "1"},
            "operation": {"const": f"ukma.{op}.v1"},
            "context": {"$ref": "shared.schema.json#/$defs/RequestContext"},
            "input": {"$ref": f"wire.schema.json#/$defs/{input_def}"},
        },
    }


def if_status(const: str) -> dict:
    return {"if": {"properties": {"status": {"const": const}}, "required": ["status"]}}


def then_required(items: list) -> dict:
    return {"required": items}


def response_schema(op: str, data_def: str) -> dict:
    partial_props = {"properties": {"partial_reasons": {"minItems": 1}}}
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": f"https://schemas.ukma.dev/v1/{op}.response.schema.json",
        "title": f"{op} Response (Phase 4 typed candidate)",
        "type": "object",
        "additionalProperties": False,
        "required": [
            "api_version",
            "request_id",
            "trace_id",
            "status",
            "warnings",
            "partial_reasons",
            "freshness",
        ],
        "properties": {
            "api_version": {"const": "1"},
            "request_id": {"$ref": "shared.schema.json#/$defs/ID"},
            "trace_id": {"$ref": "shared.schema.json#/$defs/ID"},
            "status": {"enum": ["ok", "partial", "error"]},
            "data": {"$ref": f"wire.schema.json#/$defs/{data_def}"},
            "error": {"$ref": "shared.schema.json#/$defs/ErrorDetail"},
            "warnings": {"type": "array", "items": {"type": "string"}},
            "partial_reasons": {
                "type": "array",
                "items": {"$ref": "shared.schema.json#/$defs/PartialReason"},
            },
            "snapshot_id": {"$ref": "shared.schema.json#/$defs/ID"},
            "index_generation": {"$ref": "shared.schema.json#/$defs/ID"},
            "freshness": {"enum": ["current", "stale", "rebuilding", "unknown"]},
            "observed_revision": {"$ref": "shared.schema.json#/$defs/ID"},
        },
        "allOf": [
            {
                **if_status("error"),
                "then": {
                    **then_required(["error"]),
                    "not": {"required": ["data"]},
                },
            },
            {
                **if_status("ok"),
                "then": {
                    **then_required(["data"]),
                    "not": {"required": ["error"]},
                },
            },
            {
                **if_status("partial"),
                "then": {**then_required(["partial_reasons"]), **partial_props},
            },
            {**if_status("ok"), "then": then_required(["data"])},
            {**if_status("error"), "then": then_required(["error"])},
            {
                **if_status("partial"),
                "then": {**then_required(["data"]), **partial_props},
            },
        ],
    }


def main() -> None:
    for op, (input_def, data_def) in OPS.items():
        for role, schema in (
            ("request", request_schema(op, input_def)),
            ("response", response_schema(op, data_def)),
        ):
            path = SCHEMA_DIR / f"{op}.{role}.schema.json"
            path.write_text(
                json.dumps(schema, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
            )
            print(f"wrote {path}")


if __name__ == "__main__":
    main()
