package com.ukma.plugin.action

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.project.Project
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.ukma.plugin.client.UkmaClient

/**
 * IDE action: list and call UKMA tools (scaffold).
 *
 * The finished plugin renders the tool list and results in the tool window;
 * the scaffold posts a notification with the tool count so the wiring is
 * verifiable without the full UI.
 */
class UkmaToolsAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.getData(CommonDataKeys.PROJECT) ?: return
        val client = UkmaClient()
        if (!client.ping()) {
            notify(project, "UKMA server unreachable at " + client.toString(), NotificationType.WARNING)
            return
        }
        val tools = client.toolNames()
        notify(project, "UKMA tools: ${tools.size} registered", NotificationType.INFORMATION)
    }

    private fun notify(project: Project, message: String, type: NotificationType) {
        NotificationGroupManager.getInstance()
            .getNotificationGroup("UKMA")
            .createNotification(message, type)
            .notify(project)
    }
}
