// The Workforest actions: Tools | Workforest, the tool window toolbar, the
// tree's context menus and inline buttons, and the status bar. Each one is
// a `workforest` command plus what the IDE adds: opening the worktree as a
// project, and the confirmations the CLI would ask for on a terminal.
//
// A tree selection (WORKTREE_KEY / SCRIPT_KEY) is the target for context
// menus and inline buttons; the toolbar buttons ignore it and always ask
// with a searchable popup.
package pro.buryakov.workforest.idea

import com.intellij.icons.AllIcons
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DataKey
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.ui.popup.JBPopupFactory
import com.intellij.openapi.ui.popup.PopupStep
import com.intellij.openapi.ui.popup.util.BaseListPopupStep
import com.intellij.openapi.vfs.LocalFileSystem
import java.awt.datatransfer.StringSelection
import java.nio.file.Files
import java.nio.file.Path
import javax.swing.Icon

val WORKTREE_KEY: DataKey<Worktree> = DataKey.create("workforest.worktree")
val SCRIPT_KEY: DataKey<ScriptInfo> = DataKey.create("workforest.script")

/** The tool window's header buttons: never act on the selection. */
const val TOOLBAR_PLACE = "WorkforestToolbar"

/** The tree's context menus, inline buttons, and double-click: act on the item. */
const val TREE_PLACE = "WorkforestTree"

fun AnActionEvent.targetWorktree(): Worktree? = if (place == TOOLBAR_PLACE) null else getData(WORKTREE_KEY)
fun AnActionEvent.targetScript(): ScriptInfo? = if (place == TOOLBAR_PLACE) null else getData(SCRIPT_KEY)

/**
 * [needsDirectory]: the action opens, or runs something in, the targeted
 * worktree's directory — a stale worktree has none, so the action is not
 * offered on its row.
 */
abstract class WorkforestAction(protected val needsDirectory: Boolean = false) : AnAction(), DumbAware {
    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val offered = !staleFor(e)
        e.presentation.isVisible = offered
        e.presentation.isEnabled = offered && e.project?.basePath != null
    }

    protected fun staleFor(e: AnActionEvent): Boolean = needsDirectory && e.targetWorktree()?.isStale == true
}

/** Only for a targeted worktree (context menu, inline button); [managedOnly] hides it for the main checkout. */
abstract class WorktreeItemAction(private val managedOnly: Boolean = false, needsDirectory: Boolean = false) :
    WorkforestAction(needsDirectory) {
    override fun update(e: AnActionEvent) {
        val worktree = e.targetWorktree()
        e.presentation.isEnabledAndVisible =
            e.project?.basePath != null && worktree != null && !(managedOnly && worktree.isMain) && !staleFor(e)
    }
}

/**
 * A managed worktree the action removes or locks: a chooser without a
 * target; hidden in the main checkout's menu, and on a locked row — the CLI
 * refuses to delete or check out a locked worktree, and Unlock takes the
 * place.
 */
abstract class ManagedWorktreeAction(needsDirectory: Boolean = false) : WorkforestAction(needsDirectory) {
    override fun update(e: AnActionEvent) {
        val worktree = e.targetWorktree()
        e.presentation.isEnabledAndVisible =
            e.project?.basePath != null && worktree?.isMain != true && worktree?.isLocked != true && !staleFor(e)
    }
}

// --- create / open -------------------------------------------------------

class CreateWorktreeAction : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val root = WorktreeService.getInstance(project).root ?: return
        val candidates = runModal(project, "Listing branches") { WorkforestCli.branchCandidates(root) } ?: return
        val dialog = CreateWorktreeDialog(project, candidates)
        if (!dialog.showAndGet()) return
        val branch = dialog.branch
        val args = listOfNotNull("create", branch, "--no-open", if (dialog.runHooks) null else "--no-hooks")
        runInBackground(
            project,
            "Creating worktree for $branch",
            work = {
                WorkforestCli.run(root, *args.toTypedArray())
                WorkforestCli.forest(root)
            },
        ) { forest ->
            refresh(project)
            // `create` names the directory after the branch's last segment; a
            // branch already checked out elsewhere (the main checkout, say) is
            // reused and warned about, and is then not in the forest.
            val created = forest.worktrees.firstOrNull { it.name == Protocol.worktreeName(branch) }
            if (created == null) {
                WorkforestNotifications.info(project, "Branch '$branch' is already checked out outside the forest")
            } else {
                WorkforestNotifications.info(project, "Created worktree '${created.name}' at ${created.path}")
                Projects.open(created.path, project)
            }
        }
    }
}

class OpenWorktreeAction : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        chooseWorktree(e, "Open Worktree", includeMain = true, needsDirectory = true) { Projects.open(it.path, project) }
    }
}

class OpenInNewWindowAction : WorktreeItemAction(needsDirectory = true) {
    override fun actionPerformed(e: AnActionEvent) {
        e.targetWorktree()?.let { Projects.openInNewWindow(it.path) }
    }
}

class OpenInThisWindowAction : WorktreeItemAction(needsDirectory = true) {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        e.targetWorktree()?.let { Projects.openInsteadOf(it.path, project) }
    }
}

class CopyPathAction : WorktreeItemAction() {
    override fun actionPerformed(e: AnActionEvent) {
        e.targetWorktree()?.let { CopyPasteManager.getInstance().setContents(StringSelection(it.path.toString())) }
    }
}

// --- delete / checkout ---------------------------------------------------

class DeleteWorktreeAction : ManagedWorktreeAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        chooseWorktreeOrCurrent(
            e,
            "Delete Worktree",
            needsDirectory = false, // deleting a stale worktree is how its record goes
            confirm = { "Delete worktree '${it.name}'?\nThis window's worktree, on ${branchPhrase(it)}." },
            okText = "Delete",
        ) { worktree ->
            if (!confirmDirty(project, worktree, "Delete Worktree", "Delete")) return@chooseWorktreeOrCurrent
            val branchFlag = when (val branch = worktree.branch) {
                null -> "--keep-branch"
                else -> when (
                    Messages.showYesNoCancelDialog(
                        project,
                        "Also delete branch '$branch'?",
                        "Delete Worktree",
                        "Delete Branch",
                        "Keep Branch",
                        "Cancel",
                        Messages.getQuestionIcon(),
                    )
                ) {
                    Messages.YES -> "--delete-branch"
                    Messages.NO -> "--keep-branch"
                    else -> return@chooseWorktreeOrCurrent
                }
            }
            removeWorktree(
                project,
                worktree,
                title = "Deleting worktree ${worktree.name}",
                args = listOf("delete", worktree.name, "--force", branchFlag),
                message = "Deleted worktree '${worktree.name}'",
            )
        }
    }
}

class CheckoutWorktreeAction : ManagedWorktreeAction(needsDirectory = true) {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        chooseWorktreeOrCurrent(
            e,
            "Checkout into Main Checkout",
            needsDirectory = true,
            confirm = {
                "Check out '${it.branch ?: it.name}' in the main checkout?" +
                    "\nDeletes '${it.name}', this window's worktree."
            },
            okText = "Check Out",
        ) { worktree ->
            if (!confirmDirty(project, worktree, "Checkout Worktree", "Check out")) return@chooseWorktreeOrCurrent
            removeWorktree(
                project,
                worktree,
                title = "Checking out ${worktree.branch ?: worktree.name}",
                args = listOf("checkout", worktree.name, "--force"),
                message = "'${worktree.branch ?: worktree.name}' is now in the main checkout",
                offerMain = true,
            )
        }
    }
}

// --- lock / unlock / prune -----------------------------------------------

/** `wf lock NAME [--reason TEXT]`: Delete, Checkout and Prune then refuse the worktree. */
class LockWorktreeAction : ManagedWorktreeAction(needsDirectory = true) {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val root = WorktreeService.getInstance(project).root ?: return
        chooseWorktree(e, "Lock Worktree", needsDirectory = true, unlockedOnly = true) { worktree ->
            val reason = Messages.showInputDialog(
                project,
                "Delete, Checkout and Prune refuse a locked worktree until it is unlocked.\nWhy (optional):",
                "Lock Worktree '${worktree.name}'",
                null,
            ) ?: return@chooseWorktree
            val args = listOf("lock", worktree.name) + if (reason.isBlank()) emptyList() else listOf("--reason", reason.trim())
            runInBackground(project, "Locking ${worktree.name}", work = { WorkforestCli.run(root, *args.toTypedArray()) }) {
                refresh(project)
            }
        }
    }
}

/** `wf unlock NAME`: on a locked row, where it stands in for Delete; a chooser of the locked ones elsewhere. */
class UnlockWorktreeAction : WorkforestAction() {
    override fun update(e: AnActionEvent) {
        val worktree = e.targetWorktree()
        e.presentation.isEnabledAndVisible =
            e.project?.basePath != null && (worktree == null || (!worktree.isMain && worktree.isLocked))
    }

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val root = WorktreeService.getInstance(project).root ?: return
        chooseWorktree(e, "Unlock Worktree", only = { it.isLocked }, none = "No worktree is locked") { worktree ->
            runInBackground(project, "Unlocking ${worktree.name}", work = { WorkforestCli.run(root, "unlock", worktree.name) }) {
                refresh(project)
            }
        }
    }
}

/**
 * `wf prune`, after showing what `wf prune --dry-run` would remove. Never
 * on a row: the failure it fixes is the one that leaves no usable row.
 */
class PruneWorktreesAction : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val root = WorktreeService.getInstance(project).root ?: return
        // The CLI's own dry run decides: pruning is repository-wide and
        // reaches records this tool window does not show.
        val plan = runModal(project, "Looking for stale worktrees") { WorkforestCli.run(root, "prune", "--dry-run").stderr } ?: return
        if (!Protocol.wouldPrune(plan)) {
            WorkforestNotifications.info(project, Protocol.said(plan).ifEmpty { "No stale worktree records" })
            refresh(project)
            return
        }
        val answer = Messages.showYesNoDialog(
            project,
            Protocol.said(plan) + "\n\nOnly git's records go, never files. A worktree on a drive that is not mounted" +
                " looks exactly like a deleted one: lock those instead.",
            "Prune Stale Worktrees",
            "Prune",
            "Cancel",
            Messages.getWarningIcon(),
        )
        if (answer != Messages.YES) return
        runInBackground(project, "Pruning stale worktrees", work = { WorkforestCli.run(root, "prune").stderr }) { report ->
            WorkforestNotifications.info(project, Protocol.said(report))
            refresh(project)
        }
    }
}

// --- scripts -------------------------------------------------------------

class StopScriptAction : WorkforestAction(needsDirectory = true) {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val cwd = e.scriptCwd() ?: return
        chooseScript(e, "Stop Script in ${cwd.fileName}") { script ->
            val args = if (script.isMake) arrayOf("stop", "--make", script.name) else arrayOf("stop", script.name)
            runInBackground(project, "Stopping ${script.name}", work = { WorkforestCli.run(cwd, *args) }) {
                WorkforestNotifications.info(project, "Stopped ${script.name} in ${cwd.fileName}")
            }
        }
    }
}

// --- configuration -------------------------------------------------------

class ShowConfigAction : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val root = WorktreeService.getInstance(project).root ?: return
        runInBackground(project, "Reading the configuration", work = { WorkforestCli.configDump(root) }) { dump ->
            WorkforestNotifications.showText(
                project,
                "workforest-config.yaml",
                "# workforest config — merged, as seen from $root\n$dump",
            )
        }
    }
}

/** `wf init`: scaffold the project config and open it. */
open class InitConfigAction(private val local: Boolean = false) : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val root = WorktreeService.getInstance(project).root ?: return
        runInBackground(
            project,
            "Scaffolding the configuration",
            work = {
                val main = WorkforestCli.forest(root).main.path
                if (local) {
                    // The CLI takes the first existing IDE folder; this is a JetBrains IDE, so make ours exist.
                    Files.createDirectories(main.resolve(Project.DIRECTORY_STORE_FOLDER))
                    WorkforestCli.run(root, "init", "--local")
                    listOf(".vscode", Project.DIRECTORY_STORE_FOLDER).map { main.resolve(it).resolve(".workforest.yaml") }
                        .first { Files.isRegularFile(it) }
                } else {
                    WorkforestCli.run(root, "init")
                    main.resolve(".workforest.yaml")
                }
            },
        ) { file ->
            refresh(project)
            LocalFileSystem.getInstance().refreshAndFindFileByNioFile(file)?.let {
                FileEditorManager.getInstance(project).openFile(it, true)
            }
        }
    }
}

class InitLocalConfigAction : InitConfigAction(local = true)

class OpenSettingsAction : WorkforestAction() {
    override fun update(e: AnActionEvent) {
        e.presentation.isEnabled = true
    }

    override fun actionPerformed(e: AnActionEvent) {
        ShowSettingsUtil.getInstance().showSettingsDialog(e.project, WorkforestConfigurable::class.java)
    }
}

class RefreshWorktreesAction : WorkforestAction() {
    override fun actionPerformed(e: AnActionEvent) {
        e.project?.let(::refresh)
    }
}

// --- shared steps --------------------------------------------------------

fun refresh(project: Project) {
    if (!project.isDisposed) WorktreeService.getInstance(project).refresh()
}

/** A list popup with speed search (type to filter), as the IDE's own choosers. */
fun <T> choosePopup(title: String, items: List<T>, text: (T) -> String, icon: (T) -> Icon?, chosen: (T) -> Unit) =
    JBPopupFactory.getInstance().createListPopup(
        object : BaseListPopupStep<T>(title, items) {
            override fun isSpeedSearchEnabled() = true
            override fun getTextFor(value: T) = text(value)
            override fun getIconFor(value: T) = icon(value)
            override fun onChosen(selectedValue: T, finalChoice: Boolean): PopupStep<*>? = doFinalStep { chosen(selectedValue) }
        },
    )

/** How to name a worktree's branch in a sentence. */
fun branchPhrase(worktree: Worktree): String =
    worktree.branch?.let { "branch '$it'" } ?: "a detached HEAD"

fun worktreeLabel(worktree: Worktree): String {
    val branch = worktree.branch ?: "(detached)"
    val role = if (worktree.isMain) "main checkout · " else ""
    val state = listOf(if (worktree.dirty) "●" else "", worktree.stateNote).filter { it.isNotEmpty() }
        .joinToString(" ").let { if (it.isEmpty()) "" else " $it" }
    return "${worktree.name}  ($role$branch)$state"
}

fun worktreeIcon(worktree: Worktree): Icon = if (worktree.isMain) AllIcons.Nodes.HomeFolder else AllIcons.Vcs.Branch

fun scriptLabel(script: ScriptInfo): String = if (script.flags.isEmpty()) script.name else "${script.name}  (${script.flags})"

fun scriptIcon(script: ScriptInfo): Icon = when (script.kind) {
    ScriptKind.COMMAND -> AllIcons.Nodes.Console
    ScriptKind.BULK -> AllIcons.Actions.GroupBy
    ScriptKind.PIPELINE -> AllIcons.Actions.ListFiles
    ScriptKind.MAKE -> AllIcons.Actions.Compile
}

/**
 * Hands the targeted worktree to [onChosen]; without one, asks with a
 * searchable popup of the managed worktrees (plus the main checkout when
 * [includeMain]). Either way only a worktree the action can take: a stale
 * one is left out when it [needsDirectory], a locked one when it works on
 * the [unlockedOnly], and [only] narrows further — the popup has no menu
 * to hide a row's actions in, so the rows themselves go ([none] says so
 * when that leaves nothing).
 */
fun chooseWorktree(
    e: AnActionEvent,
    title: String,
    includeMain: Boolean = false,
    needsDirectory: Boolean = false,
    unlockedOnly: Boolean = false,
    only: (Worktree) -> Boolean = { true },
    none: String = "No worktree to choose: the others are locked or stale",
    onChosen: (Worktree) -> Unit,
) {
    val project = e.project ?: return
    e.targetWorktree()?.let { target ->
        val refusal = Protocol.refusal(target, needsDirectory, unlockedOnly)
        when {
            refusal != null -> WorkforestNotifications.warning(project, refusal)
            (includeMain || !target.isMain) && only(target) -> onChosen(target)
        }
        return
    }
    val root = WorktreeService.getInstance(project).root ?: return
    val forest = runModal(project, "Listing worktrees") { WorkforestCli.forest(root) } ?: return
    val recency = WorkforestRecency.getInstance()
    val managed = Recency.order(forest.worktrees, recency::lastOpened, Recency::createdAt)
    val worktrees = (if (includeMain) listOf(forest.main) + managed else managed)
        .filter { Protocol.refusal(it, needsDirectory, unlockedOnly) == null && only(it) }
    if (worktrees.isEmpty()) {
        WorkforestNotifications.info(
            project,
            if (managed.isEmpty()) "No worktrees yet: create one with Tools | Workforest | Create Worktree" else none,
        )
        return
    }
    choosePopup(title, worktrees, ::worktreeLabel, ::worktreeIcon, onChosen).showInBestPositionFor(e.dataContext)
}

/**
 * The target for Delete and Checkout: the row they were invoked on, else —
 * from the toolbar or the Tools menu — the worktree this window is in,
 * once [confirm] is answered; from the main checkout, where there is no
 * such worktree, the chooser as usual.
 */
fun chooseWorktreeOrCurrent(
    e: AnActionEvent,
    title: String,
    needsDirectory: Boolean,
    confirm: (Worktree) -> String,
    okText: String,
    onChosen: (Worktree) -> Unit,
) {
    val project = e.project ?: return
    val current = if (e.targetWorktree() != null) null else WorktreeService.getInstance(project).current
    if (current == null || current.isMain) {
        chooseWorktree(e, title, needsDirectory = needsDirectory, unlockedOnly = true, onChosen = onChosen)
        return
    }
    Protocol.refusal(current, needsDirectory, unlockedOnly = true)?.let {
        WorkforestNotifications.warning(project, it)
        return
    }
    val answer = Messages.showYesNoDialog(
        project,
        confirm(current),
        title,
        okText,
        "Cancel",
        Messages.getWarningIcon(),
    )
    if (answer == Messages.YES) onChosen(current)
}

/** Where a script runs: the targeted worktree, else this window's directory. */
fun AnActionEvent.scriptCwd(): Path? = targetWorktree()?.path ?: project?.let { WorktreeService.getInstance(it).root }

/** Hands the targeted script to [onChosen]; without one, asks with a searchable popup. */
fun chooseScript(e: AnActionEvent, title: String, onChosen: (ScriptInfo) -> Unit) {
    val project = e.project ?: return
    e.targetScript()?.let {
        onChosen(it)
        return
    }
    val cwd = e.scriptCwd() ?: return
    val scripts = runModal(project, "Reading the configuration") { WorkforestCli.scripts(cwd) } ?: return
    if (scripts.isEmpty()) {
        WorkforestNotifications.info(project, "No scripts configured: add a `scripts` entry to .workforest.yaml", "Initialize Project Config" to {
            ActionsUtil.perform("Workforest.Init", project)
        })
        return
    }
    choosePopup(title, scripts, ::scriptLabel, ::scriptIcon, onChosen).showInBestPositionFor(e.dataContext)
}

/** The CLI's uncommitted-changes guard, as a dialog; true when it is fine to go on. */
private fun confirmDirty(project: Project, worktree: Worktree, title: String, verb: String): Boolean {
    if (!worktree.dirty) return true
    val answer = Messages.showYesNoDialog(
        project,
        "Worktree '${worktree.name}' has uncommitted changes.\nThey are discarded with the worktree.",
        title,
        "$verb anyway",
        "Cancel",
        Messages.getWarningIcon(),
    )
    return answer == Messages.YES
}

/**
 * `delete` and `checkout` both remove a worktree directory. When that
 * directory is open as a project, the IDE analog of the CLI's `cd` back to
 * the main checkout is to open the main checkout (asked for before the
 * directory is gone) in place of the orphaned window; after a checkout
 * that leaves no window on the main checkout, offer to open it.
 */
private fun removeWorktree(
    project: Project,
    worktree: Worktree,
    title: String,
    args: List<String>,
    message: String,
    offerMain: Boolean = false,
) {
    val root = WorktreeService.getInstance(project).root ?: return
    val orphan = Projects.find(worktree.path)
    runInBackground(
        project,
        title,
        work = {
            val main = WorkforestCli.forest(root).main.path
            WorkforestCli.run(root, *args.toTypedArray())
            main
        },
    ) { main ->
        if (orphan != null) {
            WorkforestNotifications.info(project, message)
            Projects.openInsteadOf(main, orphan)
        } else if (offerMain && Projects.find(main) == null) {
            WorkforestNotifications.info(project, message, "Open Main Checkout" to { Projects.open(main, project) })
        } else {
            WorkforestNotifications.info(project, message)
        }
        refresh(project)
    }
}
