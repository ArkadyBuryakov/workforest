// JetBrains IDE plugin for workforest: a thin UI over the `workforest` CLI.
// Build: ./gradlew buildPlugin  ->  build/distributions/workforest-idea-*.zip
// Try:   ./gradlew runIde       (a sandboxed IntelliJ IDEA with the plugin)

import org.jetbrains.intellij.platform.gradle.TestFrameworkType
import org.jetbrains.kotlin.gradle.dsl.JvmDefaultMode
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion

plugins {
    id("java")
    id("org.jetbrains.kotlin.jvm") version "2.3.21"
    id("org.jetbrains.intellij.platform") version "2.18.1"
}

group = "pro.buryakov"
version = providers.gradleProperty("pluginVersion").get()

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    intellijPlatform {
        // Built against the oldest supported platform; any IntelliJ-based IDE
        // from 2025.2 on can load it (no until-build).
        intellijIdeaCommunity("2025.2")
        // Optional dependency: Run Script opens a tab in the IDE terminal.
        bundledPlugin("org.jetbrains.plugins.terminal")
        pluginVerifier()
        testFramework(TestFrameworkType.Platform)
    }
    testImplementation("junit:junit:4.13.2")
}

kotlin {
    jvmToolchain(21)
    compilerOptions {
        // Match the Kotlin bundled with the 2025.2 platform so the compiled
        // classes only rely on stdlib API that every supported IDE ships.
        apiVersion = KotlinVersion.KOTLIN_2_2
        languageVersion = KotlinVersion.KOTLIN_2_2
        // Like the platform itself: interface defaults as JVM default
        // methods, no DefaultImpls bridges (which the Plugin Verifier
        // otherwise reports as overrides of internal ToolWindowFactory API).
        jvmDefault = JvmDefaultMode.NO_COMPATIBILITY
    }
}

intellijPlatform {
    buildSearchableOptions = false
    pluginConfiguration {
        name = "Workforest"
        version = project.version.toString()
        ideaVersion {
            sinceBuild = "252"
            untilBuild = provider { null }
        }
    }
    pluginVerification {
        ides {
            recommended()
            // An installed IDE to check against as well, e.g. a newer one:
            //   ./gradlew verifyPlugin -PlocalIde=/opt/idea
            providers.gradleProperty("localIde").orNull?.let { local(it) }
        }
    }
}
