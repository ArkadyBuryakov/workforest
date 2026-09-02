// Opening a worktree as an IDE project, deterministically. ProjectUtil.
// openOrImport honours forceOpenInNewFrame — and runs the configurators
// that set the content root up — for a directory that has .idea/; for one
// without it (a fresh worktree) PlatformProjectOpenProcessor rebuilds the
// options and drops the flag, so the IDE's "Open project in" setting
// decided: a new window for some worktrees, a replaced one for others.
// The IDE creates .idea/ on opening anyway, so it is created first.
//
// OpenProjectTask is built only through platform methods: its constructor,
// the `OpenProjectTask {}` DSL, and `copy` are inlined constructor calls
// that break on the next platform version adding a field.
package pro.buryakov.workforest.idea

import com.intellij.ide.impl.OpenProjectTask
import com.intellij.ide.impl.ProjectUtil
import com.intellij.openapi.diagnostic.logger
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.project.ex.ProjectManagerEx
import com.intellij.openapi.ui.Messages
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path

object Projects {
    private val log = logger<Projects>()

    /** The open project rooted at [path], if any. */
    fun find(path: Path): Project? =
        ProjectManager.getInstance().openProjects.firstOrNull { it.basePath?.let(Path::of) == path }

    /** Always a new window (or the existing one, focused, when it is open already). EDT. */
    fun openInNewWindow(path: Path): Project? {
        find(path)?.let {
            ProjectUtil.focusProjectWindow(it, true)
            return it
        }
        try {
            Files.createDirectories(path.resolve(Project.DIRECTORY_STORE_FOLDER))
        } catch (e: IOException) {
            log.warn("cannot create ${Project.DIRECTORY_STORE_FOLDER} in $path; the IDE's open-in setting applies", e)
        }
        return ProjectUtil.openOrImport(path, OpenProjectTask.build().withForceOpenInNewFrame(true))
    }

    /** Opens [path] as the settings say (or as [mode] says), asking when so configured. EDT. */
    fun open(path: Path, from: Project, mode: OpenIn = WorkforestSettings.getInstance().openIn): Project? = when (mode) {
        OpenIn.NEW_WINDOW -> openInNewWindow(path)
        OpenIn.THIS_WINDOW -> openInsteadOf(path, from)
        OpenIn.ASK -> when (
            Messages.showDialog(
                from,
                "Open ${path.fileName} in…",
                "Open Worktree",
                arrayOf("New Window", "This Window", "Cancel"),
                0,
                Messages.getQuestionIcon(),
            )
        ) {
            0 -> openInNewWindow(path)
            1 -> openInsteadOf(path, from)
            else -> null
        }
    }

    /** Replaces [current]'s window: a new one for [path], then [current] closes. EDT. */
    fun openInsteadOf(path: Path, current: Project): Project? {
        val opened = openInNewWindow(path)
        if (opened != null && opened != current) ProjectManagerEx.getInstanceEx().closeAndDispose(current)
        return opened
    }
}
