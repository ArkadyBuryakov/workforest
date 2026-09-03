package pro.buryakov.workforest.idea

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.nio.file.Path

class RunningTest {
    private val here = Path.of("/dev/worktrees/api/feat")
    private val worktrees = listOf(
        Worktree("api", "main", Path.of("/dev/api"), dirty = false, isMain = true, running = mapOf("dev" to 1)),
        Worktree("feat", "feat", here, dirty = false, running = mapOf("dev" to 2, "test" to 1)),
        Worktree("fix", "fix", Path.of("/dev/worktrees/api/fix"), dirty = false, running = mapOf("dev" to 1)),
    )

    @Test
    fun countsTheInstancesHereAndInTheOtherWorktrees() {
        assertEquals(RunningState(here = 2, others = 2, otherWorktrees = 2), RunningState.of(worktrees, "dev", here))
        assertEquals(RunningState(here = 1, others = 0, otherWorktrees = 0), RunningState.of(worktrees, "test", here))
        assertEquals(RunningState(here = 0, others = 4, otherWorktrees = 3), RunningState.of(worktrees, "dev", null))
        assertEquals(RunningState(here = 0, others = 0, otherWorktrees = 0), RunningState.of(worktrees, "lint", here))
    }

    @Test
    fun badgesShowBothPlacesAndCountPastOneInstance() {
        val both = RunningState(here = 1, others = 3, otherWorktrees = 2)
        assertEquals("●", both.hereBadge)
        assertEquals("● 3", both.elsewhereBadge)
        assertEquals("● 2", RunningState(here = 2, others = 0, otherWorktrees = 0).hereBadge)
        assertNull(RunningState(here = 2, others = 0, otherWorktrees = 0).elsewhereBadge)
        assertNull(RunningState(here = 0, others = 1, otherWorktrees = 1).hereBadge)
        assertEquals("●", RunningState(here = 0, others = 1, otherWorktrees = 1).elsewhereBadge)
    }

    @Test
    fun labelSaysHowManyAndWhereInWords() {
        assertEquals("running here", RunningState(here = 1, others = 0, otherWorktrees = 0).label)
        assertEquals("3 running here", RunningState(here = 3, others = 0, otherWorktrees = 0).label)
        assertEquals("running here, 2 elsewhere", RunningState(here = 1, others = 2, otherWorktrees = 2).label)
        assertEquals("running in another worktree", RunningState(here = 0, others = 1, otherWorktrees = 1).label)
        assertEquals("2 running in another worktree", RunningState(here = 0, others = 2, otherWorktrees = 1).label)
        assertEquals("3 running in 3 worktrees", RunningState(here = 0, others = 3, otherWorktrees = 3).label)
        assertEquals("", RunningState(here = 0, others = 0, otherWorktrees = 0).label)
    }
}
