package krusty

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * The JVM checks the plugin makes before compiling, as KGP makes them: the Kotlin JVM target
 * against the Java task's `targetCompatibility`, and the Kotlin task's own Java toolchain. The
 * Gradle integration suite drives each through a real build.
 */
class JvmTargetValidationTest {
    @Test
    fun javaTargetCompatibilityIsNormalizedByKgpTable() {
        val matching = listOf("1.6" to "6", "1.7" to "7", "1.8" to "8", "1.8" to "1.8", "9" to "1.9", "9" to "9", "17" to "17")
        assertEquals(
            List(matching.size) { null },
            matching.map { (kotlin, java) -> jvmTargetMismatch(kotlin, java) },
        )
    }

    @Test
    fun differingTargetsAreReportedAsKgpComparesThem() {
        // KGP normalizes only the Java value, and only through its own table.
        val differing = listOf("1.8" to "9", "17" to "11", "8" to "8", "10" to "1.10")
        assertEquals(
            listOf(
                "Kotlin JVM target 1.8 differs from Java target 9",
                "Kotlin JVM target 17 differs from Java target 11",
                "Kotlin JVM target 8 differs from Java target 8",
                "Kotlin JVM target 10 differs from Java target 1.10",
            ),
            differing.map { (kotlin, java) -> jvmTargetMismatch(kotlin, java) },
        )
    }

    @Test
    fun anOwnKotlinJavaToolchainIsLeftToKotlinc() {
        assertEquals(null, kotlinJavaToolchainRefusal("8", "8"))
        assertEquals(
            "Kotlin task Java toolchain 11 differs from Gradle Java toolchain 8, " +
                "whose JDK is the only one krusty receives",
            kotlinJavaToolchainRefusal("11", "8"),
        )
    }
}
