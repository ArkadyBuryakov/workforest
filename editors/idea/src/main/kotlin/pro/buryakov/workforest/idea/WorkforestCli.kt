// Runs the workforest CLI. The only place a process is spawned; the
// executable is the copy the plugin ships for this platform — built with
// the plugin, so the two always match — else PATH, else the places `uv
// tool`, pipx, and Homebrew put it. Never git directly: the CLI owns the
// forest, the plugin only drives it.
package pro.buryakov.workforest.idea

import com.intellij.execution.ExecutionException
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.PathEnvironmentVariableUtil
import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.execution.process.ProcessOutput
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.diagnostic.logger
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.progress.ProgressManager
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.PosixFilePermission

/** A CLI failure; the message is the CLI's own and is shown to the user. */
open class WorkforestException(message: String, val exitCode: Int = -1, val stderr: String = "") : RuntimeException(message) {
    /** cli.py exits 4 for a broken configuration file. */
    val isConfigError: Boolean get() = exitCode == 4
}

/** Where to send someone whose platform this build ships no executable for. */
const val INSTALL_URL = "https://github.com/ArkadyBuryakov/workforest#install"

/**
 * No executable to run. On Windows that is final ([unsupportedOs]); anywhere
 * else installing workforest fixes it, so the callers offer that.
 */
class WorkforestNotFoundException(val unsupportedOs: Boolean = false) : WorkforestException(
    if (unsupportedOs) {
        "Workforest supports Linux and macOS only: there is no Windows build of the workforest CLI this plugin drives"
    } else {
        "workforest executable not found: this build of the plugin carries no copy for your platform, so install workforest"
    },
)

object WorkforestCli {
    private const val PLUGIN_ID = "pro.buryakov.workforest"

    private val log = logger<WorkforestCli>()

    private val fallbackLocations: List<Path> = listOf(
        Path.of(System.getProperty("user.home"), ".local", "bin", "workforest"), // uv tool, pipx
        Path.of("/opt/homebrew/bin/workforest"),
        Path.of("/usr/local/bin/workforest"),
        Path.of("/usr/bin/workforest"),
    )

    fun executable(): Path {
        bundled()?.let { return it }
        PathEnvironmentVariableUtil.findInPath("workforest")?.let { return it.toPath() }
        fallbackLocations.firstOrNull { Files.isExecutable(it) }?.let { return it }
        throw WorkforestNotFoundException(isUnsupportedOs(System.getProperty("os.name").orEmpty()))
    }

    /**
     * The executable shipped inside the plugin: the one to run, since it
     * was built with the plugin. Null when this build carries none for the
     * platform. Unzipping a plugin can drop the executable bit; restore it.
     */
    private fun bundled(): Path? {
        val relative = bundledRelativePath(
            System.getProperty("os.name").orEmpty(),
            System.getProperty("os.arch").orEmpty(),
        ) ?: return null
        val plugin = PluginManagerCore.getPlugin(PluginId.getId(PLUGIN_ID)) ?: return null
        val path = plugin.pluginPath.resolve(relative)
        if (!Files.isRegularFile(path)) return null
        if (!Files.isExecutable(path)) {
            try {
                Files.setPosixFilePermissions(
                    path,
                    Files.getPosixFilePermissions(path) + PosixFilePermission.OWNER_EXECUTE,
                )
            } catch (e: IOException) {
                log.warn("cannot make the bundled workforest executable: $path", e)
                return null
            }
        }
        return path
    }

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
