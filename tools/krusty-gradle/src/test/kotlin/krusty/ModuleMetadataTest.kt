package krusty

import java.io.DataInputStream
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals

class ModuleMetadataTest {
    /**
     * Gradle 7.6.3's Kotlin 1.7.10 reads a `.kotlin_module` flags integer only for metadata major
     * version 1. The wrapper's default 2.2.0 metadata makes that integer a protobuf tag of zero.
     */
    @Test
    fun moduleMetadataStaysOnMajorVersionOne() {
        val classes = File(System.getProperty("krusty.main.classes"))
        val modules = File(classes, "META-INF").listFiles { file -> file.name.endsWith(".kotlin_module") }
            ?: error("no module metadata under $classes")
        assertEquals(1, modules.size, modules.map { it.name }.toString())
        val version = DataInputStream(modules.single().inputStream()).use { input ->
            IntArray(input.readInt()) { input.readInt() }.toList()
        }
        assertEquals(listOf(1, 9, 0), version)
    }
}
