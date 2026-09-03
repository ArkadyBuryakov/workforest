// Which scripts are running where, from `list --json`'s per-worktree
// `running` counts: pure, unit-tested, and what the tool window turns into
// the badges on a script row.
package pro.buryakov.workforest.idea

import java.nio.file.Path

/**
 * A script's instances: how many run in the worktree this window is in,
 * how many in the other worktrees, and how many of those there are.
 */
data class RunningState(val here: Int, val others: Int, val otherWorktrees: Int) {
    /** The badge for this worktree, counted past one instance; null when none runs here. */
    val hereBadge: String?
        get() = badge(here)

    /** The badge for the other worktrees, same shape; the two are shown side by side. */
    val elsewhereBadge: String?
        get() = badge(others)

    /** The same in words, for the row's tooltip. */
    val label: String
        get() = when {
            here > 0 -> {
                val mine = if (here > 1) "$here running here" else "running here"
                if (others > 0) "$mine, $others elsewhere" else mine
            }
            others == 0 -> ""
            else -> {
                val where = if (otherWorktrees > 1) "$otherWorktrees worktrees" else "another worktree"
                if (others > 1) "$others running in $where" else "running in $where"
            }
        }

    private fun badge(count: Int): String? = when {
        count > 1 -> "$DOT $count"
        count == 1 -> DOT
        else -> null
    }

    companion object {
        const val DOT = "●"

        fun of(worktrees: List<Worktree>, script: String, here: Path?): RunningState {
            val running = worktrees.mapNotNull { worktree -> worktree.running[script]?.let { worktree.path to it } }
            val elsewhere = running.filter { (path, _) -> path != here }
            return RunningState(
                here = running.filter { (path, _) -> path == here }.sumOf { (_, count) -> count },
                others = elsewhere.sumOf { (_, count) -> count },
                otherWorktrees = elsewhere.size,
            )
        }
    }
}
