// The `workforest` executables the plugin ships, one per platform under
// bin/<os>-<arch>/ of the installed plugin directory (put there at package
// time by packaging/binary/build.sh). Pure path arithmetic, unit-tested;
// WorkforestCli decides whether the bundled copy is the one to run.
package pro.buryakov.workforest.idea

/**
 * Where in the plugin directory the executable for this platform lives, or
 * null on a platform workforest has no build for (Windows, other CPUs) —
 * there the CLI has to be installed.
 */
fun bundledRelativePath(osName: String, arch: String): String? {
    val os = when {
        osName.startsWith("Linux", ignoreCase = true) -> "linux"
        osName.startsWith("Mac", ignoreCase = true) -> "darwin"
        else -> return null
    }
    val cpu = when (arch.lowercase()) {
        "x86_64", "amd64" -> "x64"
        "aarch64", "arm64" -> "arm64"
        else -> return null
    }
    return "bin/$os-$cpu/workforest"
}
