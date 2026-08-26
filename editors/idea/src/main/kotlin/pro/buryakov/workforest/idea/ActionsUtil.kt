package pro.buryakov.workforest.idea

import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionUiKind
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.actionSystem.DataContext
import com.intellij.openapi.actionSystem.ex.ActionUtil
import com.intellij.openapi.actionSystem.impl.SimpleDataContext
import com.intellij.openapi.project.Project
import java.awt.event.InputEvent

object ActionsUtil {
    /** Runs a registered action by id, with [worktree] / [script] as its target; false when it does not exist. */
    fun perform(
        actionId: String,
        project: Project,
        parent: DataContext? = null,
        worktree: Worktree? = null,
        script: ScriptInfo? = null,
        place: String = TREE_PLACE,
        event: InputEvent? = null,
    ): Boolean {
        val action = ActionManager.getInstance().getAction(actionId) ?: return false
        val builder = SimpleDataContext.builder().setParent(parent).add(CommonDataKeys.PROJECT, project)
        worktree?.let { builder.add(WORKTREE_KEY, it) }
        script?.let { builder.add(SCRIPT_KEY, it) }
        ActionUtil.performAction(action, AnActionEvent.createEvent(builder.build(), null, place, ActionUiKind.NONE, event))
        return true
    }

    fun exists(actionId: String): Boolean = ActionManager.getInstance().getAction(actionId) != null
}
