package pro.buryakov.workforest.idea

import com.intellij.icons.AllIcons
import com.intellij.ide.BrowserUtil
import com.intellij.ide.DataManager
import com.intellij.openapi.Disposable
import com.intellij.openapi.actionSystem.ActionGroup
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.DataSink
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.SimpleToolWindowPanel
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.ColoredTreeCellRenderer
import com.intellij.ui.DoubleClickListener
import com.intellij.ui.JBColor
import com.intellij.ui.PopupHandler
import com.intellij.ui.ScrollPaneFactory
import com.intellij.ui.SimpleTextAttributes
import com.intellij.ui.TreeSpeedSearch
import com.intellij.ui.content.ContentFactory
import com.intellij.ui.render.RenderingUtil
import com.intellij.ui.treeStructure.Tree
import com.intellij.util.ui.tree.TreeUtil
import com.intellij.util.ui.update.Activatable
import com.intellij.util.ui.update.UiNotifyConnector
import java.awt.Component
import java.awt.Graphics
import java.awt.Point
import java.awt.Rectangle
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import java.awt.event.MouseMotionAdapter
import java.nio.file.Path
import javax.swing.Icon
import javax.swing.JTree
import javax.swing.SwingUtilities
import javax.swing.ToolTipManager
import javax.swing.tree.DefaultMutableTreeNode
import javax.swing.tree.DefaultTreeModel
import javax.swing.tree.TreePath
import javax.swing.tree.TreeSelectionModel

class WorkforestToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun shouldBeAvailable(project: Project): Boolean = project.basePath != null

    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = WorktreePanel(project)
        val content = ContentFactory.getInstance().createContent(panel, null, false)
        content.setDisposer(panel)
        toolWindow.contentManager.addContent(content)
        WorktreeService.getInstance(project).refresh()
    }
}

/** The tool window's two collapsible sections, in this order. */
enum class Section(val title: String, val empty: String, val menu: String) {
    SCRIPTS("Scripts", "No scripts in the config (right-click: Initialize Project Config)", "Workforest.ScriptsMenu"),
    WORKTREES("Worktrees", "No worktrees", "Workforest.WorktreesMenu"),
}

/** An inline button on a row: an action, run with the row as its target. */
class InlineButton(val actionId: String, val icon: Icon, val text: String)

/** The running badges' colours: light blue in this worktree, orange in the others. */
private val RUNNING_HERE = JBColor(0x3592C4, 0x548AF7)
private val RUNNING_ELSEWHERE = JBColor.ORANGE

private const val ICON = 16
private const val GAP = 4
private const val LEAD = 8

/** The buttons the selected row shows; absent actions (no Terminal plugin) are skipped. */
private fun inlineButtons(node: Any?): List<InlineButton> {
    fun button(id: String, icon: Icon, text: String) = InlineButton(id, icon, text).takeIf { ActionsUtil.exists(id) }
    return when (node) {
        is Worktree -> listOfNotNull(
            button("Workforest.OpenInNewWindow", AllIcons.Actions.OpenNewTab, "Open in New Window"),
            button("Workforest.OpenTerminal", AllIcons.Nodes.Console, "Open in Terminal"),
            if (node.isMain) null else button("Workforest.Delete", AllIcons.Actions.GC, "Delete Worktree"),
        )
        is ScriptInfo -> listOfNotNull(
            button("Workforest.RunScript", AllIcons.Actions.Execute, "Run Script"),
            button("Workforest.StopScript", AllIcons.Actions.Suspend, "Stop Script"),
        )
        Section.WORKTREES -> listOfNotNull(
            button("Workforest.Create", AllIcons.General.Add, "Create Worktree"),
            button("Workforest.Refresh", AllIcons.Actions.Refresh, "Refresh"),
        )
        Section.SCRIPTS -> listOfNotNull(
            button("Workforest.ShowConfig", AllIcons.FileTypes.Text, "Show Merged Configuration"),
            button("Workforest.Refresh", AllIcons.Actions.Refresh, "Refresh"),
        )
        else -> emptyList()
    }
}

private fun buttonsWidth(count: Int) = LEAD + GAP + count * (ICON + GAP)

/**
 * The forest as a tree: the config's scripts, then the worktrees (main
 * checkout first, then by recency; the one this window is in bold). Rows
 * carry a tooltip, inline buttons while hovered or selected, and a
 * context menu; the toolbar's buttons act on nothing but ask.
 * Double-click opens a worktree or runs a script.
 */
class WorktreePanel(private val project: Project) : SimpleToolWindowPanel(true, true), Disposable {
    private val root = DefaultMutableTreeNode()
    private val model = DefaultTreeModel(root)
    private val here: Path? = project.basePath?.let { Path.of(it) }
    private var worktrees: List<Worktree> = emptyList() // what the running badges are counted from
    private var hoveredRow = -1 // the row under the pointer, tracked here: see hover()
    private val tree: Tree = object : Tree(model) {
        override fun getToolTipText(event: MouseEvent): String? {
            buttonAt(event.point)?.let { return it.second.text }
            val row = rowAt(event.point)
            return if (row < 0) null else tooltipFor(userObject(getPathForRow(row)))
        }

        // The inline buttons: an overlay at the right edge of the visible
        // area for the hovered and the selected row, over whatever the
        // row's text reaches there (a long branch), never part of the row.
        override fun paintComponent(g: Graphics) {
            super.paintComponent(g)
            for (row in 0 until rowCount) if (buttonsVisibleOn(row)) paintButtons(g, row)
        }
    }

    private val scroll = ScrollPaneFactory.createScrollPane(tree)

    init {
        tree.isRootVisible = false
        tree.showsRootHandles = true
        tree.selectionModel.selectionMode = TreeSelectionModel.SINGLE_TREE_SELECTION
        tree.cellRenderer = TextRenderer()
        tree.emptyText.text = "Listing worktrees…"
        // A long row stays inside the tool window: no expanded-item popup over the editor.
        tree.setExpandableItemsEnabled(false)
        ToolTipManager.sharedInstance().registerComponent(tree)
        TreeSpeedSearch.installOn(tree)
        tree.addMouseMotionListener(object : MouseMotionAdapter() {
            override fun mouseMoved(e: MouseEvent) = hover(rowAt(e.point))
        })
        tree.addMouseListener(object : MouseAdapter() {
            override fun mouseExited(e: MouseEvent) = hover(-1)

            override fun mousePressed(e: MouseEvent) {
                if (e.button != MouseEvent.BUTTON1 || e.isPopupTrigger) return
                val (node, button) = buttonAt(e.point) ?: return
                e.consume()
                perform(button.actionId, node, e)
            }
        })
        tree.addMouseListener(object : PopupHandler() {
            override fun invokePopup(comp: Component, x: Int, y: Int) {
                val row = rowAt(Point(x, y))
                if (row < 0) return
                val path = tree.getPathForRow(row)
                tree.selectionPath = path
                val groupId = when (val node = userObject(path)) {
                    is Worktree -> "Workforest.WorktreeMenu"
                    is ScriptInfo -> "Workforest.ScriptMenu"
                    is Section -> node.menu
                    else -> return
                }
                val group = ActionManager.getInstance().getAction(groupId) as? ActionGroup ?: return
                ActionManager.getInstance().createActionPopupMenu(TREE_PLACE, group).component.show(comp, x, y)
            }
        })
        object : DoubleClickListener() {
            override fun onDoubleClick(event: MouseEvent): Boolean {
                if (buttonAt(event.point) != null) return false
                return when (val selected = selectedObject()) {
                    is Worktree -> {
                        Projects.open(selected.path, project)
                        true
                    }
                    is ScriptInfo -> perform("Workforest.RunScript", selected, event)
                    else -> false
                }
            }
        }.installOn(tree)

        val actionManager = ActionManager.getInstance()
        val group = actionManager.getAction("Workforest.ToolWindowToolbar") as ActionGroup
        val toolbar = actionManager.createActionToolbar(TOOLBAR_PLACE, group, true)
        toolbar.targetComponent = this
        setToolbar(toolbar.component)
        setContent(scroll)
        val service = WorktreeService.getInstance(project)
        service.addListener({ show(it) }, this)
        // The listing is polled only while this panel is on screen.
        Disposer.register(
            this,
            UiNotifyConnector.installOn(
                this,
                object : Activatable {
                    override fun showNotify() = service.startPolling()

                    override fun hideNotify() = service.stopPolling()
                },
            ),
        )
    }

    private fun userObject(path: TreePath?): Any? = (path?.lastPathComponent as? DefaultMutableTreeNode)?.userObject

    private fun selectedObject(): Any? = userObject(tree.selectionPath)

    /** Runs an action on a row's item; a missing action (no Terminal plugin) is reported. */
    private fun perform(actionId: String, node: Any?, event: MouseEvent?): Boolean {
        val done = ActionsUtil.perform(
            actionId,
            project,
            parent = DataManager.getInstance().getDataContext(tree),
            worktree = node as? Worktree,
            script = node as? ScriptInfo,
            event = event,
        )
        if (!done) WorkforestNotifications.info(project, "This needs the bundled Terminal plugin")
        return done
    }

    /** The row whose band [point] is in, anywhere across the width; -1 outside every row. */
    private fun rowAt(point: Point): Int {
        val row = tree.getClosestRowForLocation(point.x, point.y)
        if (row < 0) return -1
        val bounds = tree.getRowBounds(row) ?: return -1
        return if (point.y >= bounds.y && point.y < bounds.y + bounds.height) row else -1
    }

    /** Where a row's inline buttons are: at the right edge of the visible area, in row order. */
    private fun buttonRects(row: Int): List<Pair<InlineButton, Rectangle>> {
        val buttons = inlineButtons(userObject(tree.getPathForRow(row)))
        if (buttons.isEmpty()) return emptyList()
        val bounds = tree.getRowBounds(row) ?: return emptyList()
        val first = buttonsRight() - buttonsWidth(buttons.size) + LEAD + GAP
        val y = bounds.y + (bounds.height - ICON) / 2
        return buttons.mapIndexed { i, button -> button to Rectangle(first + i * (ICON + GAP), y, ICON, ICON) }
    }

    /**
     * The right edge the buttons hug: the visible area's, pulled left of an
     * overlay scrollbar drawn over it (a scrollbar laid out beside the
     * viewport is already outside the visible area).
     */
    private fun buttonsRight(): Int {
        val visible = tree.visibleRect
        val right = visible.x + visible.width
        val bar = scroll.verticalScrollBar
        if (bar == null || !bar.isShowing) return right
        return minOf(right, SwingUtilities.convertPoint(bar, 0, 0, tree).x)
    }

    private fun paintButtons(g: Graphics, row: Int) {
        val rects = buttonRects(row)
        if (rects.isEmpty()) return
        val bounds = tree.getRowBounds(row) ?: return
        val visible = tree.visibleRect
        val left = rects.first().second.x - LEAD - GAP
        g.color = RenderingUtil.getBackground(tree, row == selectedRow())
        g.fillRect(left, bounds.y, visible.x + visible.width - left, bounds.height)
        for ((button, rect) in rects) button.icon.paintIcon(tree, g, rect.x, rect.y)
    }

    /** The row under the pointer; the buttons follow it, so a move repaints. */
    private fun hover(row: Int) {
        if (row == hoveredRow) return
        hoveredRow = row
        tree.repaint()
    }

    private fun selectedRow(): Int = tree.selectionRows?.firstOrNull() ?: -1

    /**
     * Whether [row] draws its inline buttons: the hovered row and the
     * selected one. The one predicate painting and hit-testing share, so
     * the click area can never outlive the icons.
     */
    private fun buttonsVisibleOn(row: Int): Boolean = row >= 0 && (row == hoveredRow || row == selectedRow())

    /** The inline button under [point], with its row's item; only where the buttons are drawn. */
    private fun buttonAt(point: Point): Pair<Any?, InlineButton>? {
        val row = rowAt(point)
        if (!buttonsVisibleOn(row)) return null
        val hit = buttonRects(row).firstOrNull { (_, rect) -> rect.contains(point) } ?: return null
        return userObject(tree.getPathForRow(row)) to hit.first
    }

    private fun tooltipFor(node: Any?): String? = when (node) {
        is Worktree -> buildString {
            append("<html><b>").append(node.name).append("</b>")
            if (node.isMain) append(" — main checkout")
            if (node.path == here) append(" (this window)")
            append("<br>branch: <code>").append(node.branch ?: "(detached)").append("</code>")
            append("<br>state: ").append(if (node.dirty) "uncommitted changes" else "clean")
            append("<br>path: <code>").append(node.path).append("</code></html>")
        }
        is ScriptInfo -> buildString {
            append("<html><b>").append(node.name).append("</b><br><code>").append(node.detail).append("</code>")
            if (node.flags.isNotEmpty()) append("<br>").append(node.flags)
            runningOf(node).label.takeIf { it.isNotEmpty() }?.let { append("<br>").append(it) }
            append("</html>")
        }
        else -> null
    }

    /** Where [script] is running: how many instances here, and how many elsewhere. */
    private fun runningOf(script: ScriptInfo): RunningState = RunningState.of(worktrees, script.name, here)

    private fun show(view: ForestView) {
        worktrees = view.worktrees
        val collapsed = Section.entries.filter { section -> sectionPath(section)?.let { !tree.isExpanded(it) } == true }
        val selected = selectedObject()
        root.removeAllChildren()
        if (view.error == null) {
            root.add(sectionNode(Section.SCRIPTS, view.scripts))
            root.add(sectionNode(Section.WORKTREES, view.worktrees))
        }
        model.reload()
        showEmptyText(view.error)
        TreeUtil.expandAll(tree)
        collapsed.forEach { section -> sectionPath(section)?.let(tree::collapsePath) }
        selected?.let { reselect(it) }
    }

    private fun sectionNode(section: Section, items: List<Any>): DefaultMutableTreeNode {
        val node = DefaultMutableTreeNode(section)
        if (items.isEmpty()) node.add(DefaultMutableTreeNode(section.empty)) else items.forEach { node.add(DefaultMutableTreeNode(it)) }
        return node
    }

    private fun sectionPath(section: Section): TreePath? =
        root.children().asSequence().filterIsInstance<DefaultMutableTreeNode>()
            .firstOrNull { it.userObject == section }?.let { TreePath(it.path) }

    private fun reselect(previous: Any) {
        val key = identity(previous)
        val node = TreeUtil.treeNodeTraverser(root).filter(DefaultMutableTreeNode::class.java)
            .firstOrNull { identity(it.userObject) == key } ?: return
        tree.selectionPath = TreePath(node.path)
    }

    private fun identity(userObject: Any?): Any? = when (userObject) {
        is Worktree -> userObject.path
        is ScriptInfo -> "script:${userObject.name}"
        else -> userObject
    }

    private fun showEmptyText(error: Throwable?) {
        val emptyText = tree.emptyText
        emptyText.clear()
        when (error) {
            null -> emptyText.appendText("No worktrees")
            is WorkforestNotFoundException -> if (error.unsupportedOs) {
                emptyText.appendText("Workforest supports Linux and macOS only")
            } else {
                emptyText.appendText("workforest not found. ")
                emptyText.appendText("Install Workforest", SimpleTextAttributes.LINK_ATTRIBUTES) {
                    BrowserUtil.browse(INSTALL_URL)
                }
            }
            else -> emptyText.appendText(error.message ?: "Cannot list worktrees")
        }
    }

    override fun uiDataSnapshot(sink: DataSink) {
        super.uiDataSnapshot(sink)
        when (val selected = selectedObject()) {
            is Worktree -> sink[WORKTREE_KEY] = selected
            is ScriptInfo -> sink[SCRIPT_KEY] = selected
            else -> {}
        }
    }

    override fun dispose() {}

    private inner class TextRenderer : ColoredTreeCellRenderer() {
        override fun customizeCellRenderer(
            tree: JTree,
            value: Any?,
            selected: Boolean,
            expanded: Boolean,
            leaf: Boolean,
            row: Int,
            hasFocus: Boolean,
        ) {
            when (val userObject = (value as? DefaultMutableTreeNode)?.userObject) {
                is Section -> append(userObject.title, SimpleTextAttributes.REGULAR_BOLD_ATTRIBUTES)
                is Worktree -> renderWorktree(userObject)
                is ScriptInfo -> {
                    icon = scriptIcon(userObject)
                    append(userObject.name)
                    if (userObject.flags.isNotEmpty()) append("  ${userObject.flags}", SimpleTextAttributes.GRAYED_ATTRIBUTES)
                    renderRunning(runningOf(userObject))
                }
                is String -> append(userObject, SimpleTextAttributes.GRAYED_ATTRIBUTES)
                else -> {}
            }
        }

        /** The running badges, side by side: light blue for this worktree,
         * orange for the others, each counted past one instance. */
        private fun renderRunning(state: RunningState) {
            state.hereBadge?.let { append("  $it", SimpleTextAttributes(SimpleTextAttributes.STYLE_PLAIN, RUNNING_HERE)) }
            state.elsewhereBadge?.let {
                append("  $it", SimpleTextAttributes(SimpleTextAttributes.STYLE_PLAIN, RUNNING_ELSEWHERE))
            }
        }

        private fun renderWorktree(worktree: Worktree) {
            icon = worktreeIcon(worktree)
            val current = worktree.path == here
            append(worktree.name, if (current) SimpleTextAttributes.REGULAR_BOLD_ATTRIBUTES else SimpleTextAttributes.REGULAR_ATTRIBUTES)
            val branch = worktree.branch ?: "(detached)"
            append("  ${if (worktree.isMain) "main checkout · $branch" else branch}", SimpleTextAttributes.GRAYED_ATTRIBUTES)
            if (worktree.dirty) append(" ●", SimpleTextAttributes(SimpleTextAttributes.STYLE_PLAIN, JBColor.ORANGE))
            if (current) append("  (this window)", SimpleTextAttributes.GRAYED_ATTRIBUTES)
        }
    }
}
