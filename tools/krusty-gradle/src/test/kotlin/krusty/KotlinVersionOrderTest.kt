package krusty

import kotlin.test.Test
import kotlin.test.assertEquals

class KotlinVersionOrderTest {
    private val unordered = listOf("2.5.0", "2.5.0-RC2", "2.4.21", "2.5.0-Beta2", "2.5.0-RC", "2.5.0-Beta1", "2.10.0", "2.4.9")

    @Test
    fun aPreReleasePrecedesTheStableReleaseOfTheSameNumbers() {
        assertEquals(
            listOf("2.4.9", "2.4.21", "2.5.0-Beta1", "2.5.0-Beta2", "2.5.0-RC", "2.5.0-RC2", "2.5.0", "2.10.0"),
            orderedKotlinVersions(unordered),
        )
    }

    @Test
    fun aReleaseWithoutItsOwnRecordReadsItsPredecessor() {
        val versions = orderedKotlinVersions(unordered)
        val recorded = { version: String -> version == "2.4.21" || version == "2.5.0-RC" }
        assertEquals("2.5.0-RC", recordedRelease("2.5.0-RC", versions, recorded))
        assertEquals("2.5.0-RC", recordedRelease("2.5.0", versions, recorded))
        assertEquals("2.4.21", recordedRelease("2.5.0-Beta2", versions, recorded))
    }
}
