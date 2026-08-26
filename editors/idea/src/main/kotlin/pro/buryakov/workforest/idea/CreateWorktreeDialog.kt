package pro.buryakov.workforest.idea

import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.TextFieldWithAutoCompletion
import com.intellij.ui.TextFieldWithAutoCompletionListProvider
import com.intellij.ui.dsl.builder.AlignX
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.panel
import javax.swing.JComponent

/** `wf create BRANCH`: the branch, with the CLI's own completion candidates. */
class CreateWorktreeDialog(project: Project, candidates: List<BranchCandidate>) : DialogWrapper(project) {
    private val branchField = TextFieldWithAutoCompletion(project, BranchCompletionProvider(candidates), true, null)

    var branch: String = ""
        private set
    var runHooks: Boolean = true

    init {
        title = "Create Worktree"
        setOKButtonText("Create")
        init()
    }

    override fun createCenterPanel(): JComponent = panel {
        row("Branch:") {
            cell(branchField)
                .align(AlignX.FILL)
                .focused()
                .validationOnApply { if (it.text.isBlank()) error("Branch name is required") else null }
                .comment("An existing local branch, REMOTE/BRANCH, or a name for a new branch")
        }
        row {
            checkBox("Run symlinks and setup scripts").bindSelected(::runHooks)
                .comment("Where the worktree opens is a setting: Settings | Tools | Workforest")
        }
    }

    override fun getPreferredFocusedComponent(): JComponent = branchField

    override fun doOKAction() {
        branch = branchField.text.trim()
        super.doOKAction()
    }
}

private class BranchCompletionProvider(candidates: Collection<BranchCandidate>) :
    TextFieldWithAutoCompletionListProvider<BranchCandidate>(candidates) {
    override fun getLookupString(item: BranchCandidate): String = item.name

    override fun getTypeText(item: BranchCandidate): String? = item.location.ifEmpty { null }
}
