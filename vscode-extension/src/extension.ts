import * as vscode from "vscode";

/**
 * UKMA extension (Tier 3, CodeGraph-style).
 *
 * Publishes the `ukma` binary as an MCP stdio server to the editor:
 * `ukma --mcp --index-cwd <workspace root>`. VS Code spawns the process,
 * speaks MCP 2024-11-05 over its stdin/stdout, and exposes all 69 UKMA
 * tools to Copilot — no hand-rolled JSON-RPC client in this extension.
 */

export function activate(context: vscode.ExtensionContext): void {
    const output = vscode.window.createOutputChannel("UKMA");
    context.subscriptions.push(output);

    const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    context.subscriptions.push(status);
    status.text = "$(check) UKMA";
    status.tooltip = "UKMA code-intelligence MCP server (stdio)";

    const binary = (): string => {
        const setting =
            vscode.workspace.getConfiguration("ukma").get<string>("serverPath") ?? "";
        return setting.length > 0 ? setting : "ukma";
    };

    // Firing the event asks the editor to re-provide the server definitions,
    // which restarts the child with a fresh index of the workspace.
    const changed = new vscode.EventEmitter<void>();
    context.subscriptions.push(changed);

    const provider = vscode.lm.registerMcpServerDefinitionProvider(
        "ukma",
        {
            onDidChangeMcpServerDefinitions: changed.event,
            provideMcpServerDefinitions: (_token: vscode.CancellationToken) => {
                const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
                const args: string[] = ["--mcp"];
                if (root) {
                    args.push("--index-cwd", root);
                }
                return [
                    new vscode.McpStdioServerDefinition(
                        "UKMA",
                        binary(),
                        args,
                        undefined,
                        "1.2.3"
                    ),
                ];
            },
        }
    );
    context.subscriptions.push(provider);

    context.subscriptions.push(
        vscode.commands.registerCommand("ukma.indexWorkspace", () => {
            output.appendLine(
                "Asking the editor to restart the UKMA server with a fresh index..."
            );
            changed.fire();
        })
    );

    context.subscriptions.push(
        vscode.commands.registerCommand("ukma.showStatus", () => {
            output.show();
        })
    );

    output.appendLine(
        "UKMA extension active: the editor will spawn `ukma --mcp --index-cwd <workspace>`."
    );
}

export function deactivate(): void {
    // The spawned server is a child of the extension host process group and
    // exits with it; nothing else to dispose here.
}
