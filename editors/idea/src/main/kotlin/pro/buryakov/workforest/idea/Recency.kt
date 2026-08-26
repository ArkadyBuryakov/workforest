// "Recently used" for the worktree list: when this IDE last opened the
// worktree as a project — recorded here, since RecentProjectsManager is
// internal API — else when the worktree was created.
package pro.buryakov.workforest.idea

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import java.nio.file.Files
import java.nio.file.Path

@Service(Service.Level.APP)
@State(name = "WorkforestRecency", storages = [Storage("workforest.xml")])
class WorkforestRecency : SimplePersistentStateComponent<WorkforestRecency.State>(State()) {
    class State : BaseState() {
        /** project path -> epoch millis of its last opening, as text (the XML store's native shape). */
        var lastOpened by map<String, String>()
    }

    fun touch(path: Path, now: Long = System.currentTimeMillis()) {
        state.lastOpened[path.toString()] = now.toString() // the map delegate tracks the change
    }

    fun lastOpened(path: Path): Long? = state.lastOpened[path.toString()]?.toLongOrNull()

    companion object {
        fun getInstance(): WorkforestRecency = service()
    }
}

/** Every project opening, by whatever route, counts. */
class RecordProjectOpenActivity : ProjectActivity {
    override suspend fun execute(project: Project) {
        project.basePath?.let { WorkforestRecency.getInstance().touch(Path.of(it)) }
    }
}

object Recency {
    /**
     * The main checkout first, then the others most recent first: by
     * [lastOpened] when known, else by [created]; ties keep the CLI's order.
     */
    fun order(worktrees: List<Worktree>, lastOpened: (Path) -> Long?, created: (Path) -> Long): List<Worktree> {
        val (main, managed) = worktrees.partition { it.isMain }
        return main + managed.sortedByDescending { lastOpened(it.path) ?: created(it.path) }
    }

    /** A worktree's `.git` is written once, when git creates it. */
    fun createdAt(path: Path): Long = try {
        Files.getLastModifiedTime(path.resolve(".git")).toMillis()
    } catch (_: java.io.IOException) {
        0L
    }
}
