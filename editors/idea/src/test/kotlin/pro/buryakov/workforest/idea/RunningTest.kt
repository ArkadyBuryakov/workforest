package pro.buryakov.workforest.idea

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.nio.file.Path

class RunningTest {
    private val here = Path.of("/dev/worktrees/api/feat")
    private val worktrees = listOf(
        Worktree("api", "main", Path.of("/dev/api"), dirty = false, isMain = true, running = listOf("dev")),
        Worktree("feat", "feat", here, dirty = false, running = listOf("dev", "test")),
        Worktree("fix", "fix", Path.of("/dev/worktrees/api/fix"), dirty = false, running = listOf("dev")),
    )

    @Test
    fun splitsThisWorktreeFromTheOthers() {
        assertEquals(RunningState(here = true, others = 2), RunningState.of(worktrees, "dev", here))
        assertEquals(RunningState(here = true, others = 0), RunningState.of(worktrees, "test", here))
        assertEquals(RunningState(here = false, others = 3), RunningState.of(worktrees, "dev", null))
        assertEquals(RunningState(here = false, others = 0), RunningState.of(worktrees, "lint", here))
    }

    @Test
    fun badgeCountsOnlyPastOneInstance() {
        assertEquals("●", RunningState(here = true, others = 0).badge)
        assertEquals("●", RunningState(here = true, others = 3).badge)
        assertEquals("●", RunningState(here = false, others = 1).badge)
        assertEquals("● 3", RunningState(here = false, others = 3).badge)
        assertNull(RunningState(here = false, others = 0).badge)
    }

    @Test
    fun labelSaysWhereInWords() {
        assertEquals("running here", RunningState(here = true, others = 0).label)
        assertEquals("running here, 2 elsewhere", RunningState(here = true, others = 2).label)
        assertEquals("running in another worktree", RunningState(here = false, others = 1).label)
        assertEquals("running in 3 worktrees", RunningState(here = false, others = 3).label)
        assertEquals("", RunningState(here = false, others = 0).label)
    }
}
