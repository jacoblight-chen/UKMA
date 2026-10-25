# Phase 4 — Code Intelligence contract implementation

Source: UKMA v1.2.3, §32.1–32.3. Phase 3 retained unchanged except for Code API request/response schema candidates and shared `$defs`.

- 13/13 Code API requests now have typed input objects, mandatory fields, strict additionalProperties and nested canonical types.
- 13/13 Code API responses now have a typed `data` reference or a page of typed items.
- 26 Code API request fixtures (valid/invalid); all verified.
- 9 response types are based on explicit §32.1 or §32.2 field definitions; `JobHandle`, `JobView`, `ChangeReceipt`, and page envelope details are **proposals** requiring approval.
- AuthZ tests, job/transaction transitions, specific event payloads, database contract tests, Rust compilation and MCP conformance **not executed or closed**.
- The definitive release gate remains BLOCKED, including non-Code domains. No change to status of 0/65 fully closed.
- Domain query constraints enforce callers `direction=in`, callees `direction=out`, dependencies only `depends_on/imports`.

Run `python scripts/verify_phase4.py`; also run previous phase validators.
