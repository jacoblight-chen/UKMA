# UKMA rules for AI coding agents

Pre-configured rule files that teach AI coding agents (Claude, Cursor,
Windsurf, Codex, Cline) to use the UKMA MCP tools before falling back to
grep / multi-file reads. Each file maps natural-language intent to the right
`ukma_*` tool.

## Setup

One line per agent — copy the file into the agent's rules location:

```bash
cp agent-rules/claude/UKMA.md   ~/.claude/UKMA.md
cp agent-rules/cursor/UKMA.md   ./.cursor/UKMA.md
cp agent-rules/windsurf/UKMA.md ~/.windsurf/UKMA.md
cp agent-rules/codex/UKMA.md    ./.codex/UKMA.md
cp agent-rules/cline/UKMA.md    ~/.cline/UKMA.md
```

## Prerequisites

UKMA exposes its tools over MCP. Start the server (auto-indexing the current
working tree as revision `@cwd`):

```bash
ukma --mcp --index-cwd .
```

All tools are read-only except the index/admin and memory write operations.
Use `"commit_sha": "@cwd"` in tool arguments to target the indexed working
tree.

## Intent → tool map

| You want to…                 | Use the tool                                    |
| --------------------------- | --------------------------------------------- |
| Understand what a symbol does | `ukma_code_get_symbol` / `ukma_code_get_ai_context` |
| Find a symbol by name        | `ukma_code_find_symbols`                      |
| See who calls a symbol       | `ukma_code_query_callers`                    |
| See what a symbol calls      | `ukma_code_query_callees`                   |
| Assess change impact         | `ukma_code_analyze_impact`                  |
| Search by regex / text       | `ukma_code_search_by_pattern`               |
| Find a symbol by signature   | `ukma_code_find_by_signature`               |
| Get context for an edit      | `ukma_code_get_edit_context`                |
| Get context for a PR         | `ukma_code_pr_context`                      |
| Natural-language search      | `ukma_code_get_curated_context`             |
| Repository overview          | `ukma_code_analyze_repository`              |
| Summarise a module           | `ukma_code_get_module_summary`              |
| Track requirements / memory  | `ukma_document_*` / `ukma_memory_*`       |

## Guidance

1. Prefer the UKMA MCP tools for code understanding, navigation and impact
   analysis; they return structured, cross-file results in one call.
2. Fall back to grep / multi-file reads only when no UKMA tool covers the
   question.
3. For large repositories, index first (`--index-cwd` / `--workspace`) so the
   tools operate on a complete snapshot.
4. Use `--profile core|graph|memory` to trim the tool surface for a focused
   task, and `--graph-only` for fast structural analysis without embeddings.
