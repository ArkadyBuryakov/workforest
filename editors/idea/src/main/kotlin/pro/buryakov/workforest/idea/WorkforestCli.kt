// Runs the workforest CLI. The only place a process is spawned; the
// executable comes from Settings | Tools | Workforest, else PATH, else the
// places `uv tool`, pipx, and Homebrew put it. Never git directly: the CLI
// owns the forest, the plugin only drives it.
package pro.buryakov.workforest.idea

import com.intellij.execution.ExecutionException
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.PathEnvironmentVariableUtil
import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.execution.process.ProcessOutput
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.progress.ProgressManager
import java.nio.file.Files
import java.nio.file.Path

/** A CLI failure; the message is the CLI's own and is shown to the user. */
open class WorkforestException(message: String, val exitCode: Int = -1, val stderr: String = "") : RuntimeException(message) {
    /** cli.py exits 4 for a broken configuration file. */
    val isConfigError: Boolean get() = exitCode == 4
}

class WorkforestNotFoundException : WorkforestException(
    "workforest executable not found: install workforest, or set its path in Settings | Tools | Workforest",
)

object WorkforestCli {
    private val fallbackLocations: List<Path> = listOf(
        Path.of(System.getProperty("user.home"), ".local", "bin", "workforest"), // uv tool, pipx
        Path.of("/opt/homebrew/bin/workforest"),
        Path.of("/usr/local/bin/workforest"),
        Path.of("/usr/bin/workforest"),
    )

    fun executable(): Path {
        val configured = WorkforestSettings.getInstance().executable
        if (!configured.isNullOrBlank()) return Path.of(expandHome(configured))
        PathEnvironmentVariableUtil.findInPath("workforest")?.let { return it.toPath() }
        return fallbackLocations.firstOrNull { Files.isExecutable(it) } ?: throw WorkforestNotFoundException()
    }

    private fun expandHome(path: String): String =
        if (path == "~" || path.startsWith("~/")) System.getProperty("user.home") + path.substring(1) else path

    /**
     * Runs `workforest ARGS` in [cwd] and returns its output; a non-zero
     * exit becomes a [WorkforestException] carrying the CLI's message.
     * Cancellable through the current progress indicator, which kills the
     * process. stdin is not a terminal, so the CLI never prompts: callers
     * pass the explicit flags (`--force`, `--keep-branch`) instead.
     */
    fun run(cwd: Path, vararg args: String): ProcessOutput {
        val command = GeneralCommandLine(executable().toString())
            .withParameters(*args)
            .withWorkingDirectory(cwd)
            .withParentEnvironmentType(GeneralCommandLine.ParentEnvironmentType.CONSOLE)
            .withEnvironment("NO_COLOR", "1")
            .withCharset(Charsets.UTF_8)
        val output = try {
            val handler = CapturingProcessHandler(command)
            val indicator = ProgressManager.getInstance().progressIndicator
            if (indicator != null) handler.runProcessWithProgressIndicator(indicator) else handler.runProcess()
        } catch (e: ExecutionException) {
            throw WorkforestException("cannot run ${command.exePath}: ${e.message}")
        }
        if (output.isCancelled) throw ProcessCanceledException()
        if (output.exitCode != 0) {
            throw WorkforestException(Protocol.errorMessage(output.stderr, output.exitCode), output.exitCode, output.stderr)
        }
        return output
    }

    /** The whole forest: main checkout, worktrees dir, managed worktrees. */
    fun forest(cwd: Path): Forest = Protocol.parseForest(run(cwd, "list", "--json").stdout)

    fun branchCandidates(cwd: Path): List<BranchCandidate> =
        Protocol.parseBranches(run(cwd, "--complete", "branches").stdout)

    /** The `scripts` of the merged config. */
    fun scripts(cwd: Path): List<ScriptInfo> = Protocol.parseScripts(run(cwd, "config", "--json").stdout)

    /** `workforest config`: the merged configuration as YAML with its sources. */
    fun configDump(cwd: Path): String = run(cwd, "config").stdout
}
