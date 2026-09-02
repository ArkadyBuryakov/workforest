package pro.buryakov.workforest.idea

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class BundledTest {
    @Test
    fun mapsTheJvmNamesOfEverySupportedPlatform() {
        assertEquals("bin/linux-x64/workforest", bundledRelativePath("Linux", "amd64"))
        assertEquals("bin/linux-arm64/workforest", bundledRelativePath("Linux", "aarch64"))
        assertEquals("bin/darwin-x64/workforest", bundledRelativePath("Mac OS X", "x86_64"))
        assertEquals("bin/darwin-arm64/workforest", bundledRelativePath("Mac OS X", "aarch64"))
    }

    @Test
    fun hasNothingForOtherPlatforms() {
        assertNull(bundledRelativePath("Windows 11", "amd64"))
        assertNull(bundledRelativePath("Linux", "riscv64"))
        assertNull(bundledRelativePath("", ""))
    }
}
