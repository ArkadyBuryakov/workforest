// The workforest CLI's machine interface, parsed here and nowhere else:
// `list --json`, `--complete` lines, and error text on stderr. Pure
// functions, unit-tested; nothing here touches the IDE.
package pro.buryakov.workforest.idea

import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import java.nio.file.Path

/** A worktree of `list --json`: the main checkout or a managed one. */
data class Worktree(
    val name: String,
    val branch: String?,
    val path: Path,
    val dirty: Boolean,
    val isMain: Boolean = false,
    /** The scripts running there, by name. */
    val running: List<String> = emptyList(),
)

/** `list --json`: the whole forest. */
data class Forest(val main: Worktree, val worktreesDir: Path, val worktrees: List<Worktree>)

/** One line of `workforest --complete branches`: a branch `create` accepts. */
data class BranchCandidate(val name: String, val location: String)

enum class ScriptKind { COMMAND, BULK, PIPELINE }

/** A `scripts` entry of `config --json`. */
data class ScriptInfo(
    val name: String,
    val kind: ScriptKind,
    val detail: String, // the command, or the members of a group
    val background: Boolean,
    val exclusive: Boolean,
) {
    /** The flags worth showing next to the name: "background, exclusive". */
    val flags: String
        get() = listOfNotNull("background".takeIf { background }, "exclusive".takeIf { exclusive }).joinToString(", ")
}

object Protocol {
    /** `{"main": {...}, "worktrees_dir": "...", "worktrees": [{...}]}`. */
    fun parseForest(stdout: String): Forest = try {
        val root = JsonParser.parseString(stdout).asJsonObject
        Forest(
            main = worktree(root.getAsJsonObject("main")).copy(isMain = true),
            worktreesDir = Path.of(root.get("worktrees_dir").asString),
            worktrees = root.getAsJsonArray("worktrees").map { worktree(it.asJsonObject) },
        )
    } catch (e: RuntimeException) { // malformed JSON, or a shape this plugin does not know
        throw WorkforestException("unexpected `list --json` output: ${e.message}")
    }

    private fun worktree(entry: JsonObject) = Worktree(
        name = entry.get("name").asString,
        branch = entry.get("branch")?.takeUnless { it.isJsonNull }?.asString,
        path = Path.of(entry.get("path").asString),
        dirty = entry.get("dirty").asBoolean,
        running = entry.getAsJsonArray("running").map { it.asString },
    )

    /** The `scripts` of `config --json` (`{"config": {"scripts": {...}}, "sources": [...]}`), by name. */
    fun parseScripts(stdout: String): List<ScriptInfo> = try {
        val scripts = JsonParser.parseString(stdout).asJsonObject.getAsJsonObject("config").get("scripts")
        if (scripts == null || scripts.isJsonNull) emptyList()
        else scripts.asJsonObject.entrySet().sortedBy { it.key }.map { (name, entry) -> script(name, entry) }
    } catch (e: RuntimeException) {
        throw WorkforestException("unexpected `config --json` output: ${e.message}")
    }

    private fun script(name: String, entry: JsonElement): ScriptInfo {
        if (entry.isJsonPrimitive) return ScriptInfo(name, ScriptKind.COMMAND, entry.asString, background = false, exclusive = false)
        val o = entry.asJsonObject
        fun flag(key: String) = o.get(key)?.takeIf { it.isJsonPrimitive }?.asBoolean == true
        fun names(key: String) = o.getAsJsonArray(key).map { it.asString }
        val (kind, detail) = when {
            o.has("bulk") -> ScriptKind.BULK to "bulk: ${names("bulk").joinToString(", ")}"
            o.has("pipeline") -> ScriptKind.PIPELINE to "pipeline: ${names("pipeline").joinToString(" → ")}"
            else -> ScriptKind.COMMAND to (o.get("command")?.asString ?: "")
        }
        return ScriptInfo(name, kind, detail, background = flag("background"), exclusive = flag("exclusive"))
    }

    /** `NAME<TAB>LOCATION` per line; a bare name means an unknown location. */
    fun parseBranches(stdout: String): List<BranchCandidate> =
        stdout.lineSequence().filter { it.isNotBlank() }.map { line ->
            val fields = line.split('\t', limit = 2)
            BranchCandidate(fields[0], fields.getOrElse(1) { "" })
        }.toList()

    /**
     * Shell-quotes one word for a command typed into a terminal, POSIX
     * single-quote style: safe for any byte but a NUL.
     */
    fun shellQuote(word: String): String =
        if (word.isNotEmpty() && word.all { it.isLetterOrDigit() || it in "-_./=:@%+" }) word
        else "'" + word.replace("'", "'\\''") + "'"

    /**
     * What to tell the user when the CLI fails: its last stderr line, minus
     * the `Error: ` prefix cli.py adds — the human-facing message. Setup
     * scripts write above it, so earlier lines are noise here.
     */
    fun errorMessage(stderr: String, exitCode: Int): String {
        val last = stderr.lineSequence().map { it.trim() }.lastOrNull { it.isNotEmpty() }
        return last?.removePrefix("Error: ") ?: "workforest exited with status $exitCode"
    }

    /** The directory name `create` gives a branch: the part after the last `/`. */
    fun worktreeName(branch: String): String = branch.substringAfterLast('/')
}
