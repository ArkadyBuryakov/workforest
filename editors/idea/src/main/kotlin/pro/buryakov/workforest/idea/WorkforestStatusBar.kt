// The worktree this window is in, on the status bar; click to open another.
package pro.buryakov.workforest.idea

import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionPlaces
import com.intellij.openapi.actionSystem.ActionUiKind
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.ex.ActionUtil
import com.intellij.openapi.actionSystem.impl.SimpleDataContext
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.StatusBar
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import com.intellij.util.Consumer
import java.awt.Component
import java.awt.event.MouseEvent

class WorkforestStatusBarFactory : StatusBarWidgetFactory {
    override fun getId(): String = WorkforestStatusBarWidget.ID
    override fun getDisplayName(): String = "Workforest"
    override fun isAvailable(project: Project): Boolean = project.basePath != null
    override fun createWidget(project: Project): StatusBarWidget = WorkforestStatusBarWidget(project)
}

class WorkforestStatusBarWidget(private val project: Project) : StatusBarWidget, StatusBarWidget.TextPresentation {
    private var statusBar: StatusBar? = null

    override fun ID(): String = ID

    override fun getPresentation(): StatusBarWidget.WidgetPresentation = this

    override fun install(statusBar: StatusBar) {
        this.statusBar = statusBar
        val service = WorktreeService.getInstance(project)
        service.addListener({ statusBar.updateWidget(ID) }, this)
        if (service.view === ForestView.EMPTY) service.refresh()
    }

    override fun dispose() {
        statusBar = null
    }

    override fun getText(): String {
        val current = WorktreeService.getInstance(project).current ?: return ""
        return if (current.isMain) "${current.name} (main)" else current.name
    }

    override fun getTooltipText(): String {
        val current = WorktreeService.getInstance(project).current ?: return "Workforest"
        val what = if (current.isMain) "the main checkout" else "worktree ${current.name}"
        return "Workforest: $what\nbranch: ${current.branch ?: "(detached)"}\npath: ${current.path}\n\nClick to open another worktree."
    }

    override fun getAlignment(): Float = Component.CENTER_ALIGNMENT

    override fun getClickConsumer(): Consumer<MouseEvent> = Consumer { event ->
        val action = ActionManager.getInstance().getAction("Workforest.Open") ?: return@Consumer
        val context = SimpleDataContext.builder().add(CommonDataKeys.PROJECT, project).build()
        ActionUtil.performAction(action, AnActionEvent.createEvent(context, null, ActionPlaces.STATUS_BAR_PLACE, ActionUiKind.NONE, event))
    }

    companion object {
        const val ID = "Workforest"
    }
}
