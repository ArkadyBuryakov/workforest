package pro.buryakov.workforest.idea

import org.junit.Assert.assertEquals
import org.junit.Test
import java.nio.file.Path

class RecencyTest {
    private fun wt(name: String, main: Boolean = false) =
        Worktree(name, name, Path.of("/w/$name"), dirty = false, isMain = main)

    @Test
    fun mainFirstThenMostRecentlyOpenedOrCreated() {
        val worktrees = listOf(wt("a"), wt("b"), wt("main", main = true), wt("c"), wt("d"))
        val opened = mapOf(Path.of("/w/a") to 50L, Path.of("/w/c") to 500L)
        val created = mapOf(Path.of("/w/b") to 100L, Path.of("/w/d") to 100L)
        val ordered = Recency.order(worktrees, { opened[it] }, { created[it] ?: 0L })
        assertEquals(listOf("main", "c", "b", "d", "a"), ordered.map { it.name })
    }

    @Test
    fun noMainIsFine() {
        val ordered = Recency.order(listOf(wt("x"), wt("y")), { null }, { if (it.endsWith("y")) 2L else 1L })
        assertEquals(listOf("y", "x"), ordered.map { it.name })
    }
}
