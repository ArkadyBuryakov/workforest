package pro.buryakov.workforest.idea

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.ui.DialogPanel
import com.intellij.ui.dsl.builder.AlignX
import com.intellij.ui.dsl.builder.bindItem
import com.intellij.ui.dsl.builder.bindText
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
        var executable by string()
        var openIn by enum(OpenIn.NEW_WINDOW)
    }

    /** Explicit path to the executable; null means "find it". */
    var executable: String?
        get() = state.executable
        set(value) {
            state.executable = value?.trim()?.ifEmpty { null }
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
            row("Executable:") {
                textFieldWithBrowseButton(
                    FileChooserDescriptorFactory.singleFile().withTitle("Workforest Executable"),
                )
                    .align(AlignX.FILL)
                    .bindText({ settings.executable ?: "" }, { settings.executable = it })
                    .comment(
                        "Path to the <code>workforest</code> command. Leave empty to find it on PATH, " +
                            "then in ~/.local/bin, /opt/homebrew/bin, /usr/local/bin, and /usr/bin.",
                    )
            }
            row("Open worktrees in:") {
                comboBox(OpenIn.entries, textListCellRenderer { it?.label })
                    .bindItem({ settings.openIn }, { settings.openIn = it ?: OpenIn.NEW_WINDOW })
                    .comment("Where Create Worktree and Open Worktree open the worktree.")
            }
        }
    }
}
