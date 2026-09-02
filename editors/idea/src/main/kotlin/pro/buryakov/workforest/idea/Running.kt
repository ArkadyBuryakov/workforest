// Which scripts are running where, from `list --json`'s per-worktree
// `running` names: pure, unit-tested, and what the tool window turns into
// a badge on a script row.
package pro.buryakov.workforest.idea

import java.nio.file.Path

/** A script's instances: in the worktree this window is in, and in how many others. */
data class RunningState(val here: Boolean, val others: Int) {
    /** The badge text, counted only past one instance; null when it runs nowhere. */
    val badge: String?
        get() = when {
            here -> DOT
            others > 1 -> "$DOT $others"
            others == 1 -> DOT
            else -> null
        }

    /** The same in words, for the row's tooltip. */
    val label: String
        get() = when {
            here && others > 0 -> "running here, $others elsewhere"
            here -> "running here"
            others == 1 -> "running in another worktree"
            others > 1 -> "running in $others worktrees"
            else -> ""
        }

    companion object {
        const val DOT = "●"

        fun of(worktrees: List<Worktree>, script: String, here: Path?): RunningState {
            val running = worktrees.filter { script in it.running }
            return RunningState(
                here = running.any { it.path == here },
                others = running.count { it.path != here },
            )
        }
    }
}
