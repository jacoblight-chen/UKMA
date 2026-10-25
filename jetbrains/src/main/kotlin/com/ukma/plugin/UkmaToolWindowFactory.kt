package com.ukma.plugin

import com.intellij.openapi.project.Project
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBTextArea
import com.intellij.ui.content.ContentFactory
import com.ukma.plugin.client.UkmaClient
import javax.swing.JPanel
import javax.swing.SwingUtilities
import javax.swing.border.EmptyBorder

/**
 * Tool window factory (scaffold).
 *
 * The finished plugin renders the tool list and call results here; the
 * scaffold shows the server reachability and the registered tool count so
 * the client wiring is verifiable.
 */
class UkmaToolWindowFactory : com.intellij.ui.toolWindow.ToolWindowFactory {
    override fun createToolWindowContent(project: Project, toolWindow: com.intellij.ui.toolWindow.ToolWindow) {
        val output = JBTextArea(10, 40).apply { isEditable = false }
        val status = JBLabel("UKMA: connecting…")
        val panel = JPanel().apply {
            layout = javax.swing.BoxLayout(this, javax.swingBoxLayout.Y_AXIS)
            border = EmptyBorder(8, 8, 8, 8)
            add(status)
            add(output)
        }
        // Best-effort, off the EDT.
        Thread {
            val client = UkmaClient()
            val reachable = client.ping()
            val tools = if (reachable) client.toolNames() else emptyList()
            SwingUtilities.invokeLater {
                status.text = if (reachable) "UKMA server reachable" else "UKMA server unreachable"
                output.text = tools.joinToString("\n") { "- $it" }
            }
        }.start()
        ContentFactory.getInstance()
            .createContent(panel, "", false)
            .let { toolWindow.contentManager.addContent(it) }
    }
}
