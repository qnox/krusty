package krusty

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class KotlinVersionOrderTest {
    @Test
    fun aPreReleasePrecedesTheStableReleaseAndTheManifestSelectsTheBeta() {
        val versions = supportedKotlinVersions
        assertEquals("2.5.0-Beta2", versions.last())
        assertTrue(versions.indexOf("2.4.21") < versions.lastIndex)
        assertEquals(
            "2.5.0-Beta2",
            recordedRelease("2.5.0-Beta2") { it == "2.4.20" || it == "2.5.0-Beta2" },
        )
        assertEquals("2.4.20", recordedRelease("2.4.21") { it == "2.4.20" || it == "2.5.0-Beta2" })
    }
}
