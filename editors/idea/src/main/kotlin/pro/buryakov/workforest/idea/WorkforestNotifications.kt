package pro.buryakov.workforest.idea

import com.intellij.ide.BrowserUtil
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileTypes.FileType
import com.intellij.openapi.fileTypes.FileTypeManager
import com.intellij.openapi.fileTypes.PlainTextFileType
import com.intellij.openapi.fileTypes.UnknownFileType
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.ThrowableComputable
import com.intellij.testFramework.LightVirtualFile

object WorkforestNotifications {
    private fun group() = NotificationGroupManager.getInstance().getNotificationGroup("Workforest")

    fun info(project: Project?, content: String, action: Pair<String, () -> Unit>? = null) {
        val notification = group().createNotification(content, NotificationType.INFORMATION).setTitle("Workforest")
        action?.let { (text, run) -> notification.addAction(NotificationAction.createSimpleExpiring(text) { run() }) }
        notification.notify(project)
    }

    fun warning(project: Project?, content: String) {
        group().createNotification(content, NotificationType.WARNING).setTitle("Workforest").notify(project)
    }

    fun error(project: Project?, error: Throwable) {
        val notification = group()
            .createNotification(error.message ?: error.toString(), NotificationType.ERROR)
            .setTitle("Workforest")
        if (error is WorkforestNotFoundException) {
            notification.addAction(
                NotificationAction.createSimpleExpiring("Install Workforest") { BrowserUtil.browse(INSTALL_URL) },
            )
        } else if (error is WorkforestException && error.stderr.isNotBlank() && project != null) {
            // The whole stderr — a failing setup script says more than its last line.
            notification.addAction(
                NotificationAction.createSimpleExpiring("Show Output") {
                    showText(project, "workforest-output.txt", error.stderr)
                },
            )
        }
        notification.notify(project)
    }

    /**
     * Opens [text] read-only in an editor tab, typed by the name's extension
     * when the IDE knows it (YAML needs its plugin) — else as plain text,
     * which is also the fallback when no editor accepts the typed file (the
     * text editor refuses types it takes for binary).
     */
    fun showText(project: Project, name: String, text: String) {
        val byName = FileTypeManager.getInstance().getFileTypeByFileName(name)
        val typed = if (byName is UnknownFileType || byName.isBinary) PlainTextFileType.INSTANCE else byName
        val manager = FileEditorManager.getInstance(project)
        if (manager.openFile(lightFile(name, typed, text), true).isNotEmpty() || typed === PlainTextFileType.INSTANCE) return
        manager.openFile(lightFile(name, PlainTextFileType.INSTANCE, text), true)
    }

    private fun lightFile(name: String, type: FileType, text: String): LightVirtualFile =
        LightVirtualFile(name, type, text).apply { isWritable = false }
}

/**
 * Runs [work] off the EDT under a cancellable background progress, then
 * [done] on the EDT with its result. A CLI failure becomes an error
 * notification and [done] is not called.
 */
fun <T : Any> runInBackground(project: Project, title: String, work: (ProgressIndicator) -> T, done: (T) -> Unit) {
    object : Task.Backgroundable(project, title, true) {
        private lateinit var result: T

        override fun run(indicator: ProgressIndicator) {
            result = work(indicator)
        }

        override fun onSuccess() = done(result)

        override fun onThrowable(error: Throwable) {
            if (error is WorkforestException) WorkforestNotifications.error(project, error) else super.onThrowable(error)
        }
    }.queue()
}

/**
 * Runs [work] under a modal progress and returns its result; null when the
 * user cancelled or the CLI failed (which is reported as a notification).
 */
fun <T : Any> runModal(project: Project, title: String, work: () -> T): T? = try {
    ProgressManager.getInstance().runProcessWithProgressSynchronously(
        ThrowableComputable<T, RuntimeException> { work() },
        title,
        true,
        project,
    )
} catch (e: WorkforestException) {
    WorkforestNotifications.error(project, e)
    null
} catch (_: ProcessCanceledException) {
    null
}
