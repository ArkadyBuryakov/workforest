package pro.buryakov.workforest.idea

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service
import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.ui.DialogPanel
import com.intellij.ui.dsl.builder.bindItem
import com.intellij.ui.dsl.builder.panel
import com.intellij.ui.dsl.listCellRenderer.textListCellRenderer

/** Where Create and Open put a worktree. */
enum class OpenIn(val label: String) {
    NEW_WINDOW("New window"),
    THIS_WINDOW("This window"),
    ASK("Ask every time"),
}

/** Application-level settings. */
@Service(Service.Level.APP)
@State(name = "WorkforestSettings", storages = [Storage("workforest.xml")])
class WorkforestSettings : SimplePersistentStateComponent<WorkforestSettings.State>(State()) {
    class State : BaseState() {
        var openIn by enum(OpenIn.NEW_WINDOW)
    }

    var openIn: OpenIn
        get() = state.openIn ?: OpenIn.NEW_WINDOW
        set(value) {
            state.openIn = value
        }

    companion object {
        fun getInstance(): WorkforestSettings = service()
    }
}

/** Settings | Tools | Workforest. */
class WorkforestConfigurable : BoundConfigurable("Workforest") {
    override fun createPanel(): DialogPanel {
        val settings = WorkforestSettings.getInstance()
        return panel {
            row("Open worktrees in:") {
                comboBox(OpenIn.entries, textListCellRenderer { it?.label })
                    .bindItem({ settings.openIn }, { settings.openIn = it ?: OpenIn.NEW_WINDOW })
                    .comment("Where Create Worktree and Open Worktree open the worktree.")
            }
        }
    }
}
