package krusty

import org.gradle.api.GradleException
import java.io.File
import java.io.InputStream
import java.net.URI
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption
import java.security.MessageDigest
import java.util.zip.GZIPInputStream
import java.util.zip.ZipInputStream

/** Where krusty releases live; `-Pkrusty.releases=<url>` points the plugin at a mirror instead. */
internal const val GITHUB_RELEASES = "https://github.com/qnox/krusty/releases/download"

/** The release this plugin jar was published with; its compiler is the one the plugin runs. */
internal val pluginRelease: String?
    get() = KrustyKotlinPlugin::class.java.`package`?.implementationVersion

/**
 * The krusty compiler of [release] for this host, downloaded once from [releases] into
 * [gradleUserHome] and checked against the release's `SHA256SUMS`. Every build on the machine
 * shares that copy; a file lock keeps concurrent builds from downloading it twice.
 */
internal fun releasedCompiler(release: String, releases: String, gradleUserHome: File, offline: Boolean): File {
    val triple = hostTriple()
    val windows = triple.endsWith("windows-msvc")
    val directory = File(gradleUserHome, "caches/krusty/$release/$triple")
    val executable = File(directory, if (windows) "krusty.exe" else "krusty")
    if (executable.isFile) return executable
    if (offline) {
        throw GradleException(
            "the krusty $release compiler for $triple is not in $directory and Gradle is offline; " +
                "run once online or set -Pkrusty.binary=<path>",
        )
    }
    directory.mkdirs()
    FileChannel.open(File(directory, ".lock").toPath(), StandardOpenOption.CREATE, StandardOpenOption.WRITE)
        .use { channel ->
            channel.lock().use {
                if (executable.isFile) return executable
                val archive = "krusty-$release-$triple." + if (windows) "zip" else "tar.gz"
                val expected = expectedSha256(releases, release, archive)
                val bytes = download("$releases/v$release/$archive")
                val actual = MessageDigest.getInstance("SHA-256").digest(bytes)
                    .joinToString("") { "%02x".format(it) }
                if (actual != expected) {
                    throw GradleException("$archive from the krusty $release release has SHA-256 $actual, expected $expected")
                }
                val staged = File(directory, executable.name + ".part")
                staged.outputStream().use { out ->
                    val entry = if (windows) zipEntry(bytes, executable.name) else tarEntry(bytes, executable.name)
                    entry.use { it.copyTo(out) }
                }
                staged.setExecutable(true)
                Files.move(staged.toPath(), executable.toPath(), StandardCopyOption.ATOMIC_MOVE)
            }
        }
    return executable
}

internal fun hostTriple(): String {
    val os = System.getProperty("os.name").lowercase()
    val arch = when (val name = System.getProperty("os.arch").lowercase()) {
        "amd64", "x86_64" -> "x86_64"
        "aarch64", "arm64" -> "aarch64"
        else -> throw GradleException("krusty publishes no compiler for the $name architecture; set -Pkrusty.binary=<path>")
    }
    return when {
        os.startsWith("linux") -> "$arch-unknown-linux-gnu"
        os.startsWith("mac") -> "$arch-apple-darwin"
        os.startsWith("windows") -> "$arch-pc-windows-msvc"
        else -> throw GradleException("krusty publishes no compiler for $os; set -Pkrusty.binary=<path>")
    }
}

private fun expectedSha256(releases: String, release: String, archive: String): String {
    val sums = String(download("$releases/v$release/krusty-$release-SHA256SUMS"))
    return sums.lineSequence()
        .map { it.trim().split(Regex("\\s+\\*?")) }
        .firstOrNull { it.size == 2 && it[1] == archive }
        ?.get(0)
        ?: throw GradleException("the krusty $release release lists no checksum for $archive")
}

private fun download(url: String): ByteArray =
    try {
        URI(url).toURL().openStream().use { it.readBytes() }
    } catch (e: java.io.IOException) {
        throw GradleException("could not download $url; set -Pkrusty.binary=<path> to use a local compiler", e)
    }

private fun zipEntry(bytes: ByteArray, name: String): InputStream {
    val zip = ZipInputStream(bytes.inputStream())
    while (true) {
        val entry = zip.nextEntry ?: throw GradleException("the krusty archive has no $name")
        if (entry.name == name) return zip
    }
}

/** The one regular file [name] of a gzipped ustar archive, as `just package` writes it. */
private fun tarEntry(bytes: ByteArray, name: String): InputStream {
    val tar = GZIPInputStream(bytes.inputStream()).readBytes()
    var offset = 0
    while (offset + 512 <= tar.size && tar[offset].toInt() != 0) {
        val header = tar.copyOfRange(offset, offset + 512)
        val entryName = String(header, 0, 100).trimEnd('\u0000').removePrefix("./")
        val size = String(header, 124, 12).trim('\u0000', ' ').toLong(8).toInt()
        val type = header[156].toInt().toChar()
        val data = offset + 512
        if (entryName == name && (type == '0' || type == '\u0000')) {
            return tar.inputStream(data, size)
        }
        offset = data + (size + 511) / 512 * 512
    }
    throw GradleException("the krusty archive has no $name")
}
