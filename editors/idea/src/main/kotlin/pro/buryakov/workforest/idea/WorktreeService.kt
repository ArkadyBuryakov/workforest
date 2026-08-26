package pro.buryakov.workforest.idea

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationActivationListener
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.components.serviceIfCreated
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.openapi.wm.IdeFrame
import com.intellij.util.Alarm
import com.intellij.util.containers.ContainerUtil
import java.nio.file.Path

/** What the tool window and status bar show: the forest and the scripts, or why not. */
data class ForestView(
    /** Main checkout first, then by recency; empty on [error]. */
    val worktrees: List<Worktree>,
    val scripts: List<ScriptInfo>,
    val error: Throwable?,
) {
    val main: Worktree? get() = worktrees.firstOrNull { it.isMain }

    companion object {
        val EMPTY = ForestView(emptyList(), emptyList(), null)
    }
}

/**
 * The project's view of the forest: the last listing, refreshed on demand,
 * after every action, when the window comes back to the front, and when
 * the repository's worktree bookkeeping or the config files change, with
 * listeners (tool window, status bar) told on the EDT. The CLI works out
 * the main checkout from the project directory itself, so this works from
 * the main checkout and from any worktree alike.
 */
@Service(Service.Level.PROJECT)
class WorktreeService(private val project: Project) : Disposable {
    fun interface Listener {
        fun changed(view: ForestView)
    }

    private val listeners = ContainerUtil.createLockFreeCopyOnWriteList<Listener>()
    private val alarm = Alarm(Alarm.ThreadToUse.SWING_THREAD, this)
    private var watchedMain: Path? = null
    private var lastConfigWarning = ""

    @Volatile
    var view: ForestView = ForestView.EMPTY
        private set

    /** The project directory; the CLI's working directory. */
    val root: Path?
        get() = project.basePath?.let { Path.of(it) }

    /** The forest entry this window is in, once listed. */
    val current: Worktree?
        get() = root?.let { here -> view.worktrees.firstOrNull { it.path == here } }

    init {
        project.messageBus.connect(this).subscribe(
            VirtualFileManager.VFS_CHANGES,
            object : BulkFileListener {
                override fun after(events: List<VFileEvent>) {
                    if (events.any { isBookkeeping(it.path) }) scheduleRefresh()
                }
            },
        )
    }

    fun addListener(listener: Listener, parent: Disposable) {
        listeners.add(listener)
        Disposer.register(parent, Disposable { listeners.remove(listener) })
    }

    /** A refresh soon, coalescing bursts of file events. */
    fun scheduleRefresh(delayMillis: Int = 300) {
        alarm.cancelAllRequests()
        alarm.addRequest({ refresh() }, delayMillis)
    }

    fun refresh() {
        val root = root ?: return
        object : Task.Backgroundable(project, "Listing worktrees", false) {
            private var loaded = ForestView.EMPTY

            override fun run(indicator: ProgressIndicator) {
                loaded = try {
                    val forest = WorkforestCli.forest(root)
                    val recency = WorkforestRecency.getInstance()
                    ForestView(
                        worktrees = Recency.order(listOf(forest.main) + forest.worktrees, recency::lastOpened, Recency::createdAt),
                        scripts = WorkforestCli.scripts(root),
                        error = null,
                    )
                } catch (e: WorkforestException) {
                    ForestView(emptyList(), emptyList(), e)
                }
            }

            override fun onSuccess() {
                view = loaded
                loaded.main?.let { watch(it.path) }
                val error = loaded.error
                if (error is WorkforestException && error.isConfigError && error.message != lastConfigWarning) {
                    lastConfigWarning = error.message.orEmpty()
                    WorkforestNotifications.warning(project, error.message.orEmpty())
                }
                listeners.forEach { it.changed(loaded) }
            }
        }.queue()
    }

    /**
     * Have the VFS watch the main checkout's `.git`, which lies outside this
     * project when it is a worktree: `.git/worktrees/NAME` comes and goes
     * with the worktree, its HEAD moves with the branch, `.git/HEAD` with
     * `wf checkout`.
     */
    private fun watch(main: Path) {
        if (watchedMain == main) return
        watchedMain = main
        val gitDir = main.resolve(".git")
        LocalFileSystem.getInstance().addRootToWatch(gitDir.toString(), true)
        LocalFileSystem.getInstance().refreshAndFindFileByNioFile(gitDir)
    }

    override fun dispose() {}

    companion object {
        fun getInstance(project: Project): WorktreeService = project.service()

        /**
         * Worktree bookkeeping (not `index`, rewritten by every `git status` —
         * our own listing included) and the config files.
         */
        fun isBookkeeping(path: String): Boolean {
            val name = path.substringAfterLast('/')
            if (name == ".workforest.yaml" || name == ".workforest.yml" || name == ".workforest.json") return true
            if (path.endsWith("/.git/HEAD")) return true
            val rest = path.substringAfter("/.git/worktrees/", "")
            if (rest.isEmpty()) return false
            val parts = rest.split('/')
            return parts.size == 1 || (parts.size == 2 && parts[1] == "HEAD")
        }
    }
}

/** The window came back to the front: what a shell did meanwhile shows up. */
class RefreshOnActivation : ApplicationActivationListener {
    override fun applicationActivated(ideFrame: IdeFrame) {
        val project = ideFrame.project ?: return
        if (!project.isDisposed) project.serviceIfCreated<WorktreeService>()?.scheduleRefresh(0)
    }
}
