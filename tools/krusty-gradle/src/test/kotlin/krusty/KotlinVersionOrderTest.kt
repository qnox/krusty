package krusty

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class KotlinVersionOrderTest {
    @Test
    fun aPreReleasePrecedesTheStableReleaseAndTheManifestSelectsTheRc() {
        val versions = supportedKotlinVersions
        assertEquals("2.5.0-RC2", versions.last())
        assertTrue(versions.indexOf("2.4.21") < versions.lastIndex)
        assertEquals(
            "2.5.0-RC2",
            recordedRelease("2.5.0-RC2") { it == "2.4.20" || it == "2.5.0-RC2" },
        )
        assertEquals("2.4.20", recordedRelease("2.4.21") { it == "2.4.20" || it == "2.5.0-RC2" })
    }
}
