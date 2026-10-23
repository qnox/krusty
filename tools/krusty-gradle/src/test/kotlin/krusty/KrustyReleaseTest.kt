package krusty

import org.gradle.api.GradleException
import java.io.File
import java.nio.file.Files
import java.security.MessageDigest
import java.util.concurrent.Callable
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import kotlin.io.path.createTempDirectory
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class KrustyReleaseTest {
    private val release = "2.4.21-build.7"
    private val triple = hostTriple()
    private val executableName = if (triple.endsWith("windows-msvc")) "krusty.exe" else "krusty"
    private val archiveName = "krusty-$release-$triple." + if (triple.endsWith("windows-msvc")) "zip" else "tar.gz"

    /** A release directory laid out like a GitHub release, holding one compiler archive. */
    private fun releases(checksum: (String) -> String = { it }): File {
        val root = createTempDirectory("krusty-releases").toFile()
        val tag = File(root, "v$release").apply { mkdirs() }
        val bin = createTempDirectory("krusty-bin").toFile()
        File(bin, executableName).writeText("#!/bin/sh\necho krusty\n")
        val archive = File(tag, archiveName)
        val command = if (archiveName.endsWith(".zip")) {
            listOf("jar", "cfM", archive.path, "-C", bin.path, executableName)
        } else {
            listOf("tar", "-C", bin.path, "-czf", archive.path, executableName)
        }
        assertEquals(0, ProcessBuilder(command).inheritIO().start().waitFor())
        val sha = MessageDigest.getInstance("SHA-256").digest(archive.readBytes())
            .joinToString("") { "%02x".format(it) }
        File(tag, "krusty-$release-SHA256SUMS").writeText(
            "0000  krusty-lsp-$release-$triple.tar.gz\n${checksum(sha)}  $archiveName\n",
        )
        return root
    }

    @Test
    fun downloadsTheReleaseCompilerOnceAndReusesIt() {
        val releases = releases()
        val home = createTempDirectory("gradle-home").toFile()

        val compiler = releasedCompiler(release, releases.toURI().toString().removeSuffix("/"), home, offline = false)

        assertEquals(File(home, "caches/krusty/$release/$triple/$executableName"), compiler)
        assertEquals("#!/bin/sh\necho krusty\n", compiler.readText())
        assertTrue(Files.isExecutable(compiler.toPath()))
        // A second build finds the cached copy and neither downloads nor needs the network.
        releases.deleteRecursively()
        assertEquals(compiler, releasedCompiler(release, "file:/nonexistent", home, offline = true))
    }

    @Test
    fun rejectsAnArchiveWhoseChecksumDiffers() {
        val releases = releases { "f".repeat(64) }
        val home = createTempDirectory("gradle-home").toFile()

        val error = assertFailsWith<GradleException> {
            releasedCompiler(release, releases.toURI().toString().removeSuffix("/"), home, offline = false)
        }

        val actual = MessageDigest.getInstance("SHA-256")
            .digest(File(releases, "v$release/$archiveName").readBytes())
            .joinToString("") { "%02x".format(it) }
        assertEquals(
            "$archiveName from the krusty $release release has SHA-256 $actual, expected ${"f".repeat(64)}",
            error.message,
        )
        assertEquals(false, File(home, "caches/krusty/$release/$triple/$executableName").exists())
    }

    @Test
    fun offlineWithoutACachedCompilerNamesTheWayOut() {
        val home = createTempDirectory("gradle-home").toFile()

        val error = assertFailsWith<GradleException> {
            releasedCompiler(release, "file:/nonexistent", home, offline = true)
        }

        assertEquals(
            "the krusty $release compiler for $triple is not in ${File(home, "caches/krusty/$release/$triple")} " +
                "and Gradle is offline; run once online or set -Pkrusty.binary=<path>",
            error.message,
        )
    }

    @Test
    fun simultaneousFirstUsesInOneJvmAllGetTheCompiler() {
        val releases = releases().toURI().toString().removeSuffix("/")
        val home = createTempDirectory("gradle-home").toFile()
        val callers = 8
        val start = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(callers)
        try {
            val results = (1..callers).map {
                pool.submit(Callable { start.await(); releasedCompiler(release, releases, home, offline = false) })
            }
            start.countDown()

            val compilers = results.map { it.get() }.toSet()

            assertEquals(setOf(File(home, "caches/krusty/$release/$triple/$executableName")), compilers)
            assertEquals("#!/bin/sh\necho krusty\n", compilers.single().readText())
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun aLaterUseRecoversAfterAFailedDownload() {
        val releases = releases { "f".repeat(64) }
        val url = releases.toURI().toString().removeSuffix("/")
        val home = createTempDirectory("gradle-home").toFile()
        assertFailsWith<GradleException> { releasedCompiler(release, url, home, offline = false) }

        val sums = File(releases, "v$release/krusty-$release-SHA256SUMS")
        val sha = MessageDigest.getInstance("SHA-256")
            .digest(File(releases, "v$release/$archiveName").readBytes())
            .joinToString("") { "%02x".format(it) }
        sums.writeText("$sha  $archiveName\n")
        val compiler = releasedCompiler(release, url, home, offline = false)

        assertEquals("#!/bin/sh\necho krusty\n", compiler.readText())
    }
}
