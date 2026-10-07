package pro.buryakov.workforest.idea

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.nio.file.Path

class ProtocolTest {
    private val forestJson = """
        {
          "main": {"name": "api", "branch": "main", "path": "/dev/api", "dirty": false,
                   "locked": null, "prunable": null, "running": {}},
          "worktrees_dir": "/dev/worktrees/api",
          "worktrees": [
            {"name": "feat", "branch": "feature/feat", "path": "/dev/worktrees/api/feat", "dirty": true,
             "locked": null, "prunable": null, "running": {"dev": 2, "test": 1}},
            {"name": "fix", "branch": null, "path": "/dev/worktrees/api/fix", "dirty": false,
             "locked": null, "prunable": null, "running": {"dev": 1}}
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

    private fun row(locked: String? = null, prunable: String? = null, dirty: Boolean = false, main: Boolean = false) =
        Worktree("feat", "feat", Path.of("/dev/worktrees/api/feat"), dirty, main, locked = locked, prunable = prunable)

    @Test
    fun parsesLockedAndStaleWorktrees() {
        val json = """{"main": {"name": "api", "branch": "main", "path": "/dev/api", "dirty": false,
                      "locked": null, "prunable": null, "running": {}},
            "worktrees_dir": "/dev/worktrees/api", "worktrees": [
            {"name": "held", "branch": "held", "path": "/w/held", "dirty": true, "locked": "", "prunable": null, "running": {}},
            {"name": "usb", "branch": "usb", "path": "/w/usb", "dirty": null, "locked": "on the\nusb drive",
             "prunable": "gitdir file points to non-existent location", "running": {}}]}"""
        val (held, usb) = Protocol.parseForest(json).worktrees
        // locked without a reason is still locked
        assertEquals(listOf("", null, true, true, false), listOf(held.locked, held.prunable, held.dirty, held.isLocked, held.isStale))
        // a stale worktree is never asked for its changes
        assertEquals(false, usb.dirty)
        assertEquals("on the\nusb drive", usb.locked)
        assertEquals(true, usb.isStale)
    }

    @Test
    fun stateNoteNamesTheStates() {
        assertEquals("", row().stateNote)
        assertEquals("locked", row(locked = "").stateNote)
        assertEquals("stale", row(prunable = "gone").stateNote)
        assertEquals("stale locked", row(locked = "why", prunable = "gone").stateNote)
    }

    @Test
    fun refusalFollowsTheMenuRules() {
        // (locked, stale) × what the action needs
        val live = row()
        val locked = row(locked = "on the\nusb\tdrive")
        val stale = row(prunable = "gone")
        val both = row(locked = "", prunable = "gone")
        // open, terminal, scripts: only a directory matters; a lock never blocks them
        assertEquals(null, Protocol.refusal(live, needsDirectory = true, unlockedOnly = false))
        assertEquals(null, Protocol.refusal(locked, needsDirectory = true, unlockedOnly = false))
        assertEquals(
            "Worktree 'feat' is stale: its directory is gone. Delete it, or prune the stale worktrees",
            Protocol.refusal(stale, needsDirectory = true, unlockedOnly = false),
        )
        assertEquals(
            "Worktree 'feat' is stale and locked: unlock it, then prune the stale worktrees",
            Protocol.refusal(both, needsDirectory = true, unlockedOnly = false),
        )
        // delete: a stale one goes, a locked one does not
        assertEquals(null, Protocol.refusal(live, needsDirectory = false, unlockedOnly = true))
        assertEquals(null, Protocol.refusal(stale, needsDirectory = false, unlockedOnly = true))
        assertEquals(
            "Worktree 'feat' is locked (on the usb drive): unlock it first",
            Protocol.refusal(locked, needsDirectory = false, unlockedOnly = true),
        )
        assertEquals("Worktree 'feat' is locked: unlock it first", Protocol.refusal(both, needsDirectory = false, unlockedOnly = true))
        // checkout: both
        assertEquals(null, Protocol.refusal(live, needsDirectory = true, unlockedOnly = true))
        for (row in listOf(locked, stale, both)) {
            assertEquals(true, Protocol.refusal(row, needsDirectory = true, unlockedOnly = true) != null)
        }
        // copy path: always
        for (row in listOf(live, locked, stale, both)) {
            assertEquals(null, Protocol.refusal(row, needsDirectory = false, unlockedOnly = false))
        }
    }

    @Test
    fun readsWhatPruneReports() {
        val plan = "would prune 2 stale worktree records: a, b\n1 stale worktree record is locked: z — unlock to prune\n"
        assertEquals(true, Protocol.wouldPrune(plan))
        assertEquals(false, Protocol.wouldPrune("no stale worktree records\n"))
        assertEquals(false, Protocol.wouldPrune("1 stale worktree record is locked: z — unlock to prune\n"))
        assertEquals(
            "would prune 2 stale worktree records: a, b\n1 stale worktree record is locked: z — unlock to prune",
            Protocol.said(plan),
        )
        assertEquals("", Protocol.said("\n"))
        assertEquals("a b c", Protocol.oneLine("  a\n\tb   c\n"))
    }

    @Test
    fun failedListingKeepsTheLastGoodForest() {
        val shown = ForestView(listOf(row(main = true), row(prunable = "gone")), emptyList(), null)
        val error = WorkforestException("boom", 1)
        val kept = WorktreeService.failed(shown, error)
        assertEquals(shown.worktrees, kept.worktrees)
        assertEquals(error, kept.error)
        // nothing listed yet, or no CLI at all: the error is all there is
        assertEquals(emptyList<Worktree>(), WorktreeService.failed(ForestView.EMPTY, error).worktrees)
        assertEquals(emptyList<Worktree>(), WorktreeService.failed(shown, WorkforestNotFoundException()).worktrees)
    }

    @Test
    fun emptyForestHasNoWorktrees() {
        val json = """{"main": {"name": "api", "branch": "main", "path": "/dev/api", "dirty": false,
            "locked": null, "prunable": null, "running": {}},
            "worktrees_dir": "/dev/worktrees/api", "worktrees": []}"""
        assertEquals(emptyList<Worktree>(), Protocol.parseForest(json).worktrees)
    }

    @Test
    fun rejectsUnexpectedOutput() {
        val noRunning = """{"main": {"name": "api", "branch": null, "path": "/dev/api", "dirty": false},
            "worktrees_dir": "/dev", "worktrees": []}""" // an older CLI
        val noLock = """{"main": {"name": "api", "branch": null, "path": "/dev/api", "dirty": false, "running": {}},
            "worktrees_dir": "/dev", "worktrees": []}""" // one before `locked` and `prunable`
        for (bad in listOf("", "not json", "[]", """{"worktrees": []}""", """{"main": {"name": "x"}}""", noRunning, noLock)) {
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
    fun parsesMakeTargets() {
        val config = """{"config": {"make": {"exclusive_scripts": ["dev"]}}, "sources": []}"""
        val scripts = Protocol.parseMakeScripts("check\ndev\n", config)
        assertEquals(
            listOf(
                ScriptInfo("check", ScriptKind.MAKE, "make check", background = false, exclusive = false),
                ScriptInfo("dev", ScriptKind.MAKE, "make dev", background = false, exclusive = true),
            ),
            scripts,
        )
        assertEquals("make:check", scripts[0].runningKey)
        assertEquals("make", scripts[0].flags)
        assertEquals("make, exclusive", scripts[1].flags)
    }

    @Test
    fun makeTargetsMissingOrMalformed() {
        assertEquals(emptyList<ScriptInfo>(), Protocol.parseMakeScripts("", """{"config": {}}"""))
        // no `make` section, or output we cannot read: no target is exclusive
        assertEquals(false, Protocol.parseMakeScripts("check\n", """{"config": {}}""")[0].exclusive)
        assertEquals(false, Protocol.parseMakeScripts("check\n", "nope")[0].exclusive)
    }

    @Test
    fun bookkeepingPaths() {
        assertEquals(true, WorktreeService.isBookkeeping("/r/.git/worktrees/feat"))
        assertEquals(true, WorktreeService.isBookkeeping("/r/.git/worktrees/feat/HEAD"))
        assertEquals(true, WorktreeService.isBookkeeping("/r/.git/worktrees/feat/locked"))
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
