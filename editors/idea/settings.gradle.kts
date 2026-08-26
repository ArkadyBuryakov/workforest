rootProject.name = "workforest-idea"

plugins {
    // Fetches the JDK 21 toolchain when the JDK running Gradle is another
    // version (an IDE's bundled JBR, say), so no JDK install is needed.
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}
