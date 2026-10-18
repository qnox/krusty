@file:OptIn(
    org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi::class,
    org.jetbrains.kotlin.config.CompilerConfiguration.Internals::class,
)

package krusty.pluginhost

import org.jetbrains.kotlin.cli.plugins.processCompilerPluginsOptions
import org.jetbrains.kotlin.compiler.plugin.CliOptionProcessingException
import org.jetbrains.kotlin.compiler.plugin.CommandLineProcessor
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.config.KotlinCompilerVersion
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.File
import java.io.PrintStream
import java.net.URLClassLoader
import java.util.ServiceLoader
import kotlin.system.exitProcess

/**
 * krusty's plugin host: runs kotlinc compiler-plugin jars on `kotlin-compiler-embeddable` for krusty.
 *
 * krusty talks to it over stdin and stdout in frames (a big-endian u32 length, then the payload).
 * Plugins may print, so the host moves `System.out` to stderr before anything else runs, and keeps
 * the real stdout for frames. The protocol is `crates/krusty-plugin-host/src/protocol.rs`; keep the
 * two in step and bump [PROTOCOL] on any change.
 */
const val PROTOCOL = 1

private const val HELLO = 0
private const val LOAD = 1
private const val LOADED = 2
private const val FAILED = 3
private const val SHUTDOWN = 4

/**
 * Serves requests until krusty asks it to stop or closes stdin. Either way the JVM exits at once:
 * a plugin may have started threads that would otherwise keep it alive.
 */
fun main() {
    serve()
    exitProcess(0)
}

private fun serve() {
    val frames = DataOutputStream(System.out.buffered())
    System.setOut(PrintStream(System.err, true))
    val input = DataInputStream(System.`in`.buffered())
    send(frames) {
        writeByte(HELLO)
        writeInt(PROTOCOL)
        writeString(KotlinCompilerVersion.VERSION)
    }
    while (true) {
        val request = try {
            Frame(input.readFrame())
        } catch (_: EOFException) {
            return
        }
        when (val tag = request.readByte().toInt()) {
            LOAD -> {
                val entries = request.readList { readString() }
                val options = request.readList { readString() }
                val reply = try {
                    load(entries, options)
                } catch (error: CliOptionProcessingException) {
                    Failed(error.message ?: error.toString())
                } catch (error: Throwable) {
                    Failed(describe(error))
                }
                send(frames) { reply.write(this) }
            }
            SHUTDOWN -> return
            else -> {
                send(frames) { Failed("unknown request $tag").write(this) }
                return
            }
        }
    }
}

private sealed interface Reply {
    fun write(out: DataOutputStream)
}

private class Failed(val message: String) : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(FAILED)
        out.writeString(message)
    }
}

private class Loaded(val plugins: List<LoadedPlugin>) : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(LOADED)
        out.writeInt(plugins.size)
        for (plugin in plugins) {
            out.writeString(plugin.registrar)
            out.writeString(plugin.pluginId)
            out.writeBoolean(plugin.supportsK2)
            out.writeInt(plugin.extensions.size)
            for ((point, count) in plugin.extensions) {
                out.writeString(point)
                out.writeInt(count)
            }
        }
    }
}

private class LoadedPlugin(
    val registrar: String,
    val pluginId: String,
    val supportsK2: Boolean,
    /** Each extension point the plugin registered extensions for, by name, in registration order. */
    val extensions: List<Pair<String, Int>>,
)

/**
 * Loads the plugin classpath [entries] in their own class loader, as kotlinc does: the command-line
 * processors they declare read [options] (each a `-P` value, `plugin:<id>:<key>=<value>`) into one
 * configuration, then each declared registrar registers its extensions against it.
 */
private fun load(entries: List<String>, options: List<String>): Reply {
    val loader = URLClassLoader(
        entries.map { File(it).toURI().toURL() }.toTypedArray(),
        CompilerPluginRegistrar::class.java.classLoader,
    )
    val configuration = CompilerConfiguration()
    val processors = ServiceLoader.load(CommandLineProcessor::class.java, loader).toList()
    processCompilerPluginsOptions(configuration, options, processors)
    val plugins = ServiceLoader.load(CompilerPluginRegistrar::class.java, loader).map { registrar ->
        val storage = CompilerPluginRegistrar.ExtensionStorage()
        with(registrar) { storage.registerExtensions(configuration) }
        LoadedPlugin(
            registrar = registrar.javaClass.name,
            pluginId = registrar.pluginId,
            supportsK2 = registrar.supportsK2,
            extensions = storage.registeredExtensions.map { (point, extensions) -> point.name to extensions.size },
        )
    }
    return Loaded(plugins)
}

private fun describe(error: Throwable): String {
    val causes = generateSequence(error) { it.cause }.joinToString(": caused by ") { it.toString() }
    val at = error.stackTrace.firstOrNull()?.let { " (at $it)" } ?: ""
    return causes + at
}

private class Frame(bytes: ByteArray) : DataInputStream(bytes.inputStream())

private fun DataInputStream.readFrame(): ByteArray {
    val bytes = ByteArray(readInt())
    readFully(bytes)
    return bytes
}

private fun DataInputStream.readString(): String {
    val bytes = ByteArray(readInt())
    readFully(bytes)
    return String(bytes, Charsets.UTF_8)
}

private fun <T> DataInputStream.readList(element: DataInputStream.() -> T): List<T> =
    List(readInt()) { element() }

private fun DataOutputStream.writeString(value: String) {
    val bytes = value.toByteArray(Charsets.UTF_8)
    writeInt(bytes.size)
    write(bytes)
}

private fun send(frames: DataOutputStream, body: DataOutputStream.() -> Unit) {
    val payload = java.io.ByteArrayOutputStream()
    DataOutputStream(payload).apply(body).flush()
    frames.writeInt(payload.size())
    payload.writeTo(frames)
    frames.flush()
}
