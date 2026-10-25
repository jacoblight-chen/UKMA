package com.ukma.plugin

import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.StartupActivity
import com.ukma.plugin.client.UkmaClient

/**
 * UKMA JetBrains plugin — startup activity (registered via `postStartupActivity`).
 *
 * Honest boundary: this is a thin IDE shell. On startup it opens a handle to
 * the UKMA server (REST) so the tool window and actions can query
 * `system.capabilities` and call the `ukma_*` tools. No analysis, indexing,
 * or model logic lives here.
 */
class UkmaPlugin : StartupActivity {
    /** Shared client handle for the project (single, project-scoped). */
    private val client = UkmaClient()

    override fun executeActivity(project: Project) {
        // Connect (best-effort, off the EDT). The tool window surfaces the
        // connection state; here we log it.
        Thread {
            val connected = client.ping()
            println("UKMA plugin: server ${if (connected) "reachable" else "unreachable"}")
        }.start()
    }
}
