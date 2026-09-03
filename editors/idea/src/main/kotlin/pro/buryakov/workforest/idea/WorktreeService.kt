package pro.buryakov.workforest.idea

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationActivationListener
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
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
import com.intellij.util.concurrency.AppExecutorUtil
import com.intellij.util.containers.ContainerUtil
import java.nio.file.Path
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

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
    private val listing = AppExecutorUtil.createBoundedScheduledExecutorService("Workforest listing", 1)
    private var poll: ScheduledFuture<*>? = null
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
                    ForestView(order(WorkforestCli.forest(root)), WorkforestCli.scripts(root), null)
                } catch (e: WorkforestException) {
                    ForestView(emptyList(), emptyList(), e)
                }
            }

            override fun onSuccess() = publish(loaded)
        }.queue()
    }

    /** The main checkout first, then the worktrees by recency. */
    private fun order(forest: Forest): List<Worktree> {
        val recency = WorkforestRecency.getInstance()
        return Recency.order(listOf(forest.main) + forest.worktrees, recency::lastOpened, Recency::createdAt)
    }

    /** A new listing becomes the view, on the EDT. */
    private fun publish(loaded: ForestView) {
        view = loaded
        loaded.main?.let { watch(it.path) }
        val error = loaded.error
        if (error is WorkforestException && error.isConfigError && error.message != lastConfigWarning) {
            lastConfigWarning = error.message.orEmpty()
            WorkforestNotifications.warning(project, error.message.orEmpty())
        }
        listeners.forEach { it.changed(loaded) }
    }

    /**
     * Run the listing again every second while the tool window is on
     * screen. Nothing tells the IDE that a script started or ended: the
     * records live under the common git dir, which is outside the project
     * for a worktree, and the VFS turns an outside change into an event
     * only while a refresh session runs — one the IDE starts when its
     * window comes back to the front. So the running badges (and the
     * dirty marks, which no file event can carry either) need `list
     * --json` itself. `config --json` stays out of the poll: the scripts
     * change with the config files, and those the VFS does report.
     */
    @Synchronized
    fun startPolling() {
        if (poll == null) {
            poll = listing.scheduleWithFixedDelay(::pollListing, POLL_MILLIS, POLL_MILLIS, TimeUnit.MILLISECONDS)
        }
    }

    @Synchronized
    fun stopPolling() {
        poll?.cancel(false)
        poll = null
    }

    private fun pollListing() {
        val root = root ?: return
        val shown = view
        val loaded = try {
            ForestView(order(WorkforestCli.forest(root)), shown.scripts, null)
        } catch (e: WorkforestException) {
            ForestView(emptyList(), emptyList(), e)
        }
        if (loaded.worktrees == shown.worktrees && loaded.error?.message == shown.error?.message) return
        ApplicationManager.getApplication().invokeLater({ publish(loaded) }, ModalityState.any(), project.disposed)
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

    override fun dispose() {
        stopPolling()
        listing.shutdownNow()
    }

    companion object {
        private const val POLL_MILLIS = 1_000L

        fun getInstance(project: Project): WorktreeService = project.service()

        /**
         * Worktree bookkeeping (not `index`, rewritten by every `git status` —
         * our own listing included), the running-script records, and the
         * config files.
         */
        fun isBookkeeping(path: String): Boolean {
            val name = path.substringAfterLast('/')
            if (name == ".workforest.yaml" || name == ".workforest.yml" || name == ".workforest.json") return true
            if (path.endsWith("/.git/HEAD")) return true
            if (path.contains("/.git/workforest/running/")) return true
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
