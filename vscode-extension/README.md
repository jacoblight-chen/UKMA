# UKMA VS Code extension (Tier 3)

CodeGraph-style integration: the extension publishes the `ukma` binary to the
editor as an **MCP stdio server** — VS Code spawns

```
ukma --mcp --index-cwd <workspace root>
```

and speaks MCP `2024-11-05` over stdin/stdout. All 69 UKMA tools (code, docs,
memory, trace, system domains) become available to Copilot in the chat panel;
the workspace is auto-indexed on startup (Tier 2), and the `@cwd` revision
alias works in every tool call.

## Build

```powershell
npm install
npm run compile        # tsc -> out/
```

Load the folder in the VS Code Extensions development host
(“Run Extension”), or package a `.vsix` with `@vscode/vsce` and install it.

## Or wire Copilot without the extension

`ukma install` also writes a project-scoped `.vscode/mcp.json` (the same
`McpStdioServerDefinition` shape this extension publishes), so Copilot picks the
server up without installing anything from the marketplace:

```powershell
ukma install --target copilot-vscode --cwd .   # writes .vscode/mcp.json + instructions
ukma uninstall --target copilot-vscode --yes   # reverses it
```

The difference: the extension starts/stops the server with the editor session
(and offers the two commands below); the `.vscode/mcp.json` route is plain MCP
and is owned by VS Code.

## Resolve the binary

The editor launches `ukma` from `PATH` by default. If the binary lives
elsewhere, set:

```jsonc
// settings.json
{
  "ukma.serverPath": "C:\\path\\to\\ukma"
}
```

A typical checkout builds it with:

```powershell
cargo build -p ukma-cli --release   # -> <workspace>\target\release\ukma
```

## Commands

| Command | Effect |
| --- | --- |
| `UKMA: Re-index Workspace` | Asks the editor to restart the server with a fresh index of the current tree. |
| `UKMA: Show Status` | Opens the `UKMA` output channel (server diagnostics go to stderr and land here). |

## Honest boundaries

- The server process is a child of the extension host; it dies with the editor
  session. No state is persisted by this extension.
- The index is a content-addressed **dirty snapshot** of the current tree
  (see the `ukma --index-cwd` semantics); it makes no durability or
  cross-process claim.
- Tool output is a JSON envelope rendered to text for the language model.
