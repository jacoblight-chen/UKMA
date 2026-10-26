# UKMA — code graph rules

You have access to the UKMA code-graph MCP tools. **Prefer them over grep and
multi-file reads** for understanding, navigating and analysing code. Fall back
to grep / reads only when no UKMA tool covers the question.

## Setup

The server is started with `ukma --mcp --index-cwd .` (auto-indexes the working
tree as revision `@cwd`), or with `ukma --mcp` alone inside a project prepared
by `ukma init` (the `.ukma/project.json` manifest supplies the roots and
excludes). Pass `"commit_sha": "@cwd"` in tool arguments to target the indexed
tree.

Inside a project prepared by `ukma init`, `ukma daemon start` keeps one warm
index in a background process and every `ukma --mcp` session delegates its
`tools/call` to it (`ukma daemon status` / `ukma daemon stop`). Sessions still
work without it; delegation only skips rebuilding the index.

If a response starts with `⚠️ Some files referenced below changed after the
index snapshot`, those files were edited after indexing: re-read them directly
(or run `ukma sync`, or `ukma sync --verify` when even the file's timestamp looks
untouched) before trusting the answer.

## Intent → tool map

| You want to…                | Use the tool                                   |
| --------------------------- | --------------------------------------------- |
| Understand what a symbol does | `ukma_code_get_symbol` / `ukma_code_get_ai_context` |
| Find a symbol by name        | `ukma_code_find_symbols`                      |
| See who calls a symbol       | `ukma_code_query_callers`                   |
| See what a symbol calls      | `ukma_code_query_callees`                  |
| Assess change impact         | `ukma_code_analyze_impact`                 |
| Search by regex / text       | `ukma_code_search_by_pattern`              |
| Find a symbol by signature   | `ukma_code_find_by_signature`              |
| Get context for an edit      | `ukma_code_get_edit_context`               |
| Get context for a PR         | `ukma_code_pr_context`                     |
| Natural-language search      | `ukma_code_get_curated_context`            |
| Repository overview          | `ukma_code_analyze_repository`             |
| Summarise a module           | `ukma_code_get_module_summary`             |
| Track requirements / memory  | `ukma_document_*` / `ukma_memory_*`     |

## Guidance

1. UKMA tools return structured, cross-file results in one call — use them first.
2. Before editing, call `ukma_code_get_edit_context` to gather the surrounding
   context, then `ukma_code_analyze_impact` to check the blast radius.
3. For a PR, call `ukma_code_pr_context` for a consolidated summary.
4. Index first for large repos (`ukma --mcp --index-cwd . --workspace <path>`).
5. Trim the surface with `--profile core|graph|memory`; use `--graph-only` for
   fast structural analysis.
