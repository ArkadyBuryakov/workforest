package pro.buryakov.workforest.idea

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
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
    fun onlyWindowsIsBeyondInstalling() {
        assertTrue(isUnsupportedOs("Windows 11"))
        assertTrue(isUnsupportedOs("windows server 2022"))
        assertFalse(isUnsupportedOs("Linux"))
        assertFalse(isUnsupportedOs("Mac OS X"))
        assertFalse(isUnsupportedOs("FreeBSD"))
        assertFalse(isUnsupportedOs(""))
    }

    @Test
    fun hasNothingForOtherPlatforms() {
        assertNull(bundledRelativePath("Windows 11", "amd64"))
        assertNull(bundledRelativePath("Linux", "riscv64"))
        assertNull(bundledRelativePath("", ""))
    }
}
