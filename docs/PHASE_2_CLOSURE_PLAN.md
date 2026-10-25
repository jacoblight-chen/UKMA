# Phase 2 closure order and gates

1. Author per-operation canonical IDL for all 65 operations: exact nested input/output types, field bounds, oneOf states, special authorization, status transitions and idempotency conditions.
2. Generate closed JSON Schema Draft 2020-12 with operation-specific payloads and response union rules. Replace all `$comment: UNRESOLVED` markers.
3. Define all mutation event payloads with event version and outbox transaction semantics; ensure no raw sensitive content in events.
4. Generate Rust request/response structs and operation enum from canonical IDL. Add Rust JSON Schema drift tests and MCP manifest.
5. Build normative fixtures for positive, invalid, ACL, CAS, idempotency, stale generations, pagination and failure recovery for each operation.
6. Implement the storage port capability matrix and at least two distinct backend adapters; keep CodeGraph source locked to SHA in upstream mirror.
7. Run strict release gate and CodeGraph golden regression suite before calling implementation compliant.

## Important correctness gap discovered during generation
The source uses compact operation-row pseudo-fields such as `source:DocumentSource` and response placeholders. These do not encode full constraints for an automatic closed schema. Types cannot be safely invented. The phase-one schemas expressly mark gaps and cannot be used as normative validation on untrusted production inputs.
