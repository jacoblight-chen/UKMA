# UKMA rules for AI coding agents

Pre-configured rule files that teach AI coding agents (Claude, Cursor, Cline,
Windsurf, Codex, Copilot, Gemini CLI, opencode, Kiro) to use the UKMA MCP tools
before falling back to grep / multi-file reads. Each file maps natural-language
intent to the right `ukma_*` tool. Hermes Agent and Antigravity IDE are wired
without a rules file (see the table below for why).

## Setup

`ukma install` writes both halves — the MCP server entry and the rules file —
for every agent it detects. Nothing needs to be copied by hand:

```bash
ukma install                  # only the agents detected on this machine
ukma install --target cursor  # one agent (repeatable; `all` for every target)
ukma install --dry-run        # show the plan without writing
ukma install --no-rules       # wire the MCP server, skip the rules file
```

The same files can still be installed manually — one line per agent:

```bash
cp agent-rules/claude/UKMA.md   ~/.claude/UKMA.md
cp agent-rules/cursor/UKMA.md   ~/.cursor/UKMA.md
cp agent-rules/windsurf/UKMA.md ~/.windsurf/UKMA.md
cp agent-rules/codex/UKMA.md    ~/.codex/UKMA.md
cp agent-rules/cline/UKMA.md    ~/.cline/UKMA.md
cp agent-rules/common/UKMA.md   ~/.copilot/UKMA.md   # Copilot CLI, Gemini, opencode, Kiro
```

| `--target` | MCP configuration | rules file |
| --- | --- | --- |
| `claude-code` | `~/.claude.json` (`mcpServers`) | `~/.claude/UKMA.md` |
| `cursor` | `~/.cursor/mcp.json` | `~/.cursor/UKMA.md` |
| `windsurf` | `~/.codeium/windsurf/mcp_config.json` | `~/.windsurf/UKMA.md` |
| `codex` | `~/.codex/config.toml` (managed block) | `~/.codex/UKMA.md` |
| `cline` | VS Code globalStorage `cline_mcp_settings.json` | `~/.cline/UKMA.md` |
| `copilot-vscode` | `<project>/.vscode/mcp.json` | `<project>/.github/instructions/ukma.instructions.md` |
| `copilot-cli` | `~/.copilot/mcp-config.json` | `~/.copilot/UKMA.md` |
| `gemini-cli` | `~/.gemini/settings.json` | `~/.gemini/UKMA.md` |
| `opencode` | `~/.config/opencode/opencode.json` | `~/.config/opencode/UKMA.md` |
| `kiro` | `~/.kiro/settings/mcp.json` | `~/.kiro/UKMA.md` |
| `hermes` | `$HERMES_HOME/config.yaml` (or `~/.hermes/config.yaml`), `mcp_servers` block | — (Hermes has no rules file UKMA can write) |
| `antigravity` | `~/.gemini/config/mcp_config.json`, else `~/.gemini/antigravity/mcp_config.json` | — (shares `~/.gemini/GEMINI.md` with the Gemini CLI; left to the user) |
| `jetbrains` | — | — (the plugin is a REST client, not an MCP client) |

`ukma uninstall --yes` reverses all of it (and leaves a rules file you edited
alone). `ukma uninstall --target kiro --yes` narrows the scope. Antigravity's
uninstall sweeps **both** of its config paths, because the IDE moved its config
file mid-life.

## Prerequisites

UKMA exposes its tools over MCP. Start the server (auto-indexing the current
working tree as revision `@cwd`):

```bash
ukma --mcp --index-cwd .
```

Inside a project prepared by `ukma init`, a bare `ukma --mcp` is enough: the
`.ukma/project.json` manifest supplies the roots, excludes and file cap, and
`.ukma/index.db` supplies the snapshot (only changed files are read). Run
`ukma status` to see whether the snapshot matches the working tree.

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
