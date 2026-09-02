// `wf run NAME` in a new tab of the IDE terminal: the script gets a real
// terminal (colors, Ctrl-C, Ctrl-Z) exactly as from a shell, and `wf stop`
// works from anywhere. Loaded only when the bundled terminal plugin is on.
package pro.buryakov.workforest.idea.terminal

import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.Project
import org.jetbrains.plugins.terminal.TerminalToolWindowManager
import pro.buryakov.workforest.idea.Protocol
import pro.buryakov.workforest.idea.WorkforestAction
import pro.buryakov.workforest.idea.WorkforestCli
import pro.buryakov.workforest.idea.WorkforestException
import pro.buryakov.workforest.idea.WorkforestNotifications
import pro.buryakov.workforest.idea.chooseScript
import pro.buryakov.workforest.idea.chooseWorktree
import pro.buryakov.workforest.idea.scriptCwd
import java.nio.file.Path

class RunScriptAction : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val cwd = e.scriptCwd() ?: return
        chooseScript(e, "Run Script in ${cwd.fileName}") { runInTerminal(project, cwd, it.name) }
    }

    private fun runInTerminal(project: Project, cwd: Path, script: String) {
        val executable = try {
            WorkforestCli.executable()
        } catch (e: WorkforestException) {
            WorkforestNotifications.error(project, e)
            return
        }
        val widget = TerminalToolWindowManager.getInstance(project)
            .createShellWidget(cwd.toString(), "wf run $script", true, true)
        widget.sendCommandToExecute("${Protocol.shellQuote(executable.toString())} run ${Protocol.shellQuote(script)}")
    }
}

/** A terminal tab in a worktree's directory. */
class OpenTerminalAction : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        chooseWorktree(e, "Open Terminal In", includeMain = true) {
            TerminalToolWindowManager.getInstance(project).createShellWidget(it.path.toString(), it.name, true, true)
        }
    }
}
