package pro.buryakov.workforest.idea

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.nio.file.Path

class ProtocolTest {
    private val forestJson = """
        {
          "main": {"name": "api", "branch": "main", "path": "/dev/api", "dirty": false, "running": {}},
          "worktrees_dir": "/dev/worktrees/api",
          "worktrees": [
            {"name": "feat", "branch": "feature/feat", "path": "/dev/worktrees/api/feat", "dirty": true,
             "running": {"dev": 2, "test": 1}},
            {"name": "fix", "branch": null, "path": "/dev/worktrees/api/fix", "dirty": false, "running": {"dev": 1}}
          ]
        }
    """.trimIndent()

    @Test
    fun parsesForest() {
        val forest = Protocol.parseForest(forestJson)
        assertEquals(Worktree("api", "main", Path.of("/dev/api"), dirty = false, isMain = true), forest.main)
        assertEquals(Path.of("/dev/worktrees/api"), forest.worktreesDir)
        assertEquals(
            listOf(
                Worktree(
                    "feat", "feature/feat", Path.of("/dev/worktrees/api/feat"), dirty = true,
                    running = mapOf("dev" to 2, "test" to 1),
                ),
                Worktree("fix", null, Path.of("/dev/worktrees/api/fix"), dirty = false, running = mapOf("dev" to 1)),
            ),
            forest.worktrees,
        )
    }

    @Test
    fun emptyForestHasNoWorktrees() {
        val json = """{"main": {"name": "api", "branch": "main", "path": "/dev/api", "dirty": false, "running": {}},
            "worktrees_dir": "/dev/worktrees/api", "worktrees": []}"""
        assertEquals(emptyList<Worktree>(), Protocol.parseForest(json).worktrees)
    }

    @Test
    fun rejectsUnexpectedOutput() {
        val noRunning = """{"main": {"name": "api", "branch": null, "path": "/dev/api", "dirty": false},
            "worktrees_dir": "/dev", "worktrees": []}""" // an older CLI
        for (bad in listOf("", "not json", "[]", """{"worktrees": []}""", """{"main": {"name": "x"}}""", noRunning)) {
            val error = assertThrows(WorkforestException::class.java) { Protocol.parseForest(bad) }
            assertEquals(true, error.message!!.startsWith("unexpected `list --json` output"))
        }
    }

    @Test
    fun parsesBranchCandidates() {
        val lines = Protocol.parseBranches("feat\tlocal, origin\norigin/fix\torigin\nbare\n\n")
        assertEquals(
            listOf(
                BranchCandidate("feat", "local, origin"),
                BranchCandidate("origin/fix", "origin"),
                BranchCandidate("bare", ""),
            ),
            lines,
        )
    }

    @Test
    fun parsesScriptsFromConfigJson() {
        val json = """{"config": {"scripts": {
            "test": "npm test",
            "backend": {"command": "docker compose up", "background": true, "exclusive": true, "cleanup": "docker compose down"},
            "dev": {"bulk": ["backend", "frontend"]},
            "fresh": {"pipeline": ["migrate", "dev"], "background": true},
            "migrate": {"command": "npm run db:migrate", "hidden": true}
        }}, "sources": []}"""
        val scripts = Protocol.parseScripts(json)
        assertEquals(listOf("backend", "dev", "fresh", "test"), scripts.map { it.name }) // `migrate` is hidden
        assertEquals(ScriptInfo("test", ScriptKind.COMMAND, "npm test", background = false, exclusive = false), scripts[3])
        assertEquals(ScriptInfo("backend", ScriptKind.COMMAND, "docker compose up", background = true, exclusive = true), scripts[0])
        assertEquals("background, exclusive", scripts[0].flags)
        assertEquals(ScriptInfo("dev", ScriptKind.BULK, "bulk: backend, frontend", background = false, exclusive = false), scripts[1])
        assertEquals(ScriptInfo("fresh", ScriptKind.PIPELINE, "pipeline: migrate → dev", background = true, exclusive = false), scripts[2])
        assertEquals("", scripts[1].flags)
    }

    @Test
    fun scriptsMissingOrMalformed() {
        assertEquals(emptyList<ScriptInfo>(), Protocol.parseScripts("""{"config": {}, "sources": []}"""))
        assertEquals(emptyList<ScriptInfo>(), Protocol.parseScripts("""{"config": {"scripts": null}}"""))
        val error = assertThrows(WorkforestException::class.java) { Protocol.parseScripts("nope") }
        assertEquals(true, error.message!!.startsWith("unexpected `config --json` output"))
    }

    @Test
    fun bookkeepingPaths() {
        assertEquals(true, WorktreeService.isBookkeeping("/r/.git/worktrees/feat"))
        assertEquals(true, WorktreeService.isBookkeeping("/r/.git/worktrees/feat/HEAD"))
        assertEquals(false, WorktreeService.isBookkeeping("/r/.git/worktrees/feat/index"))
        assertEquals(true, WorktreeService.isBookkeeping("/r/.git/HEAD"))
        assertEquals(true, WorktreeService.isBookkeeping("/r/.idea/.workforest.yaml"))
        assertEquals(true, WorktreeService.isBookkeeping("/r/.git/workforest/running/dev/feat"))
        assertEquals(false, WorktreeService.isBookkeeping("/r/src/main.py"))
    }

    @Test
    fun shellQuotesOnlyWhenNeeded() {
        assertEquals("make", Protocol.shellQuote("make"))
        assertEquals("/usr/local/bin/wf", Protocol.shellQuote("/usr/local/bin/wf"))
        assertEquals("'a b'", Protocol.shellQuote("a b"))
        assertEquals("'it'\\''s'", Protocol.shellQuote("it's"))
        assertEquals("''", Protocol.shellQuote(""))
    }

    @Test
    fun errorMessageIsTheLastStderrLine() {
        assertEquals("boom", Protocol.errorMessage("Resolved 12 packages\nError: boom\n\n", 1))
        assertEquals("workforest exited with status 4", Protocol.errorMessage("  \n", 4))
    }

    @Test
    fun worktreeNameIsTheLastBranchSegment() {
        assertEquals("login", Protocol.worktreeName("feature/login"))
        assertEquals("login", Protocol.worktreeName("origin/login"))
        assertEquals("main", Protocol.worktreeName("main"))
    }
}
