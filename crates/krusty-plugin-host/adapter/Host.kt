@file:OptIn(org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi::class)

package krusty.pluginhost

import org.jetbrains.kotlin.backend.common.extensions.IrGenerationExtension
import org.jetbrains.kotlin.cli.create
import org.jetbrains.kotlin.cli.extensionsStorage
import org.jetbrains.kotlin.cli.plugins.processCompilerPluginsOptions
import org.jetbrains.kotlin.com.intellij.openapi.Disposable
import org.jetbrains.kotlin.com.intellij.openapi.util.Disposer
import org.jetbrains.kotlin.compiler.plugin.CliOptionProcessingException
import org.jetbrains.kotlin.compiler.plugin.CommandLineProcessor
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.config.KotlinCompilerVersion
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.File
import java.io.IOException
import java.io.PrintStream
import java.net.URLClassLoader
import java.util.ServiceLoader
import java.util.zip.ZipFile
import kotlin.system.exitProcess

/**
 * krusty's plugin host: runs kotlinc compiler-plugin jars on `kotlin-compiler-embeddable` for krusty.
 *
 * krusty talks to it over stdin and stdout in frames (a big-endian u32 length, then the payload).
 * Plugins may print, so the host moves `System.out` to stderr before anything else runs, and keeps
 * the real stdout for frames. The protocol is `crates/krusty-plugin-host/src/protocol.rs`; keep the
 * two in step and bump [PROTOCOL] on any change.
 */
const val PROTOCOL = 4

private const val HELLO = 0
private const val LOAD = 1
private const val LOADED = 2
private const val FAILED = 3
private const val SHUTDOWN = 4
private const val UNLOAD = 5
private const val UNLOADED = 6
private const val OPEN_SESSION = 7
private const val SESSION_OPENED = 8
private const val CLOSE_SESSION = 9
private const val SESSION_CLOSED = 10
private const val RESOLVE = 11
private const val RESOLVED = 12
private const val GENERATE = 13
private const val GENERATED = 14

/**
 * Serves requests until krusty asks it to stop or closes stdin. Either way the JVM exits at once:
 * a plugin may have started threads that would otherwise keep it alive.
 */
fun main() {
    try {
        serve()
    } finally {
        // Ending the host ends the session and the loaded plugins' lifetime, as the end of a
        // compilation does.
        try {
            closeSession()
        } catch (error: Throwable) {
            System.err.println("krusty plugin host: closing the session failed: ${describe(error)}")
        }
        try {
            unload()
        } catch (error: Throwable) {
            System.err.println("krusty plugin host: disposing the loaded plugins failed: ${describe(error)}")
        }
    }
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
                val reply = answer { load(entries, options) }
                send(frames) { reply.write(this) }
            }
            UNLOAD -> {
                val reply = answer {
                    unload()
                    Unloaded
                }
                send(frames) { reply.write(this) }
            }
            OPEN_SESSION -> {
                val reply = answer {
                    openSession(request.readSessionRequest())
                    SessionOpened
                }
                send(frames) { reply.write(this) }
            }
            CLOSE_SESSION -> {
                val reply = answer {
                    closeSession()
                    SessionClosed
                }
                send(frames) { reply.write(this) }
            }
            RESOLVE -> {
                val identities = request.readList { readCallable() }
                val reply = answer { Resolved(resolve(identities)) }
                send(frames) { reply.write(this) }
            }
            GENERATE -> {
                val reply = answer {
                    generate(request, current?.irGenerationExtensions ?: emptyList())
                    Generated
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

/** The reply [request] gives, or the failure it ends in, worded as kotlinc words it. */
private fun answer(request: () -> Reply): Reply = try {
    request()
} catch (error: CliOptionProcessingException) {
    Failed(error.message ?: error.toString())
} catch (error: Refused) {
    Failed(error.message)
} catch (error: Throwable) {
    Failed(describe(error))
}

/** A request the host will not carry out, for the reason given. */
internal class Refused(override val message: String) : Exception(message)

internal sealed interface Reply {
    fun write(out: DataOutputStream)
}

private object Unloaded : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(UNLOADED)
    }
}

private object SessionOpened : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(SESSION_OPENED)
    }
}

private object SessionClosed : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(SESSION_CLOSED)
    }
}

private object Generated : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(GENERATED)
    }
}

/** Each requested declaration, rendered as kotlinc renders IR, in request order. */
private class Resolved(val declarations: List<String>) : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(RESOLVED)
        out.writeInt(declarations.size)
        for (declaration in declarations) out.writeString(declaration)
    }
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
 * One loaded plugin classpath: its class loader, and the disposable that owns every resource its
 * registrars registered, as a compilation's project owns them in kotlinc.
 */
private class PluginSet(private val loader: URLClassLoader, private val resources: Disposable) {
    /** The IR generation extensions the registrars registered, in registration order. */
    var irGenerationExtensions: List<IrGenerationExtension> = emptyList()

    /** Disposes the registrars' resources the way kotlinc's `Disposer` does, then closes the jars. */
    fun close() {
        try {
            Disposer.dispose(resources)
        } finally {
            loader.close()
        }
    }
}

/** The plugins loaded now. A host serves one plugin set at a time, as a compilation has one. */
private var current: PluginSet? = null

/**
 * Loads the plugin classpath [entries] as kotlinc's `-Xplugin` does, replacing what was loaded
 * before: one class loader for the classpath, and one configuration, made the way kotlinc makes it,
 * whose extension storage every registrar shares, so a registrar sees what those before it
 * registered. The command-line processors the classpath declares read [options] (each a `-P` value,
 * `plugin:<id>:<key>=<value>`) into the configuration first. Each registrar is reported with what
 * it added to the storage.
 *
 * The resources registrars register belong to the loaded set and are disposed, and the class loader
 * closed, when the set is unloaded or replaced, when the host stops, or at once when loading fails.
 */
private fun load(entries: List<String>, options: List<String>): Reply {
    unload()
    for (entry in entries) checkEntry(File(entry))
    val loader = URLClassLoader(
        entries.map { File(it).toURI().toURL() }.toTypedArray(),
        CompilerPluginRegistrar::class.java.classLoader,
    )
    val resources = Disposer.newDisposable()
    val set = PluginSet(loader, resources)
    var storage: CompilerPluginRegistrar.ExtensionStorage? = null
    try {
        val configuration = CompilerConfiguration.create()
        val shared = configuration.extensionsStorage ?: error("kotlinc made a configuration without an extension storage")
        storage = shared
        refuseLegacyRegistrars(loader)
        val processors = ServiceLoader.load(CommandLineProcessor::class.java, loader).toList()
        processCompilerPluginsOptions(configuration, options, processors)
        val plugins = ServiceLoader.load(CompilerPluginRegistrar::class.java, loader).map { registrar ->
            val before = counts(shared)
            with(registrar) { shared.registerExtensions(configuration) }
            LoadedPlugin(
                registrar = registrar.javaClass.name,
                pluginId = registrar.pluginId,
                supportsK2 = registrar.supportsK2,
                extensions = counts(shared).mapNotNull { (point, count) ->
                    val added = count - (before[point] ?: 0)
                    if (added > 0) point to added else null
                },
            )
        }
        own(resources, shared)
        set.irGenerationExtensions =
            shared.registeredExtensions[IrGenerationExtension].orEmpty().map { it as IrGenerationExtension }
        current = set
        return Loaded(plugins)
    } catch (error: Throwable) {
        storage?.let { own(resources, it) }
        try {
            set.close()
        } catch (cleanup: Throwable) {
            error.addSuppressed(cleanup)
        }
        throw error
    }
}

/**
 * Refuses [entry] unless it is a directory or a jar. A class loader takes any path and finds nothing
 * in one that is neither, so a missing or broken plugin jar would load as no plugin at all.
 */
private fun checkEntry(entry: File) {
    if (entry.isDirectory) return
    if (!entry.isFile) throw Refused("plugin classpath entry $entry does not exist")
    try {
        ZipFile(entry).close()
    } catch (error: IOException) {
        throw Refused("plugin classpath entry $entry is not a jar: ${error.message}")
    }
}

/** The service through which a jar declares a plugin with the registrar kotlinc's K1 loaded. */
private const val LEGACY_REGISTRARS = "META-INF/services/org.jetbrains.kotlin.compiler.plugin.ComponentRegistrar"

/**
 * Refuses a classpath that declares a legacy `ComponentRegistrar`, which registers into a compiler
 * project rather than an extension storage. The host has no such project, so it would otherwise
 * load the plugin as nothing.
 */
private fun refuseLegacyRegistrars(loader: URLClassLoader) {
    val declared = loader.findResources(LEGACY_REGISTRARS).toList().flatMap { service ->
        service.openStream().bufferedReader().useLines { lines ->
            lines.map { it.substringBefore('#').trim() }.filter { it.isNotEmpty() }.toList()
        }
    }
    if (declared.isNotEmpty()) {
        throw Refused(
            "the plugin classpath declares the legacy ComponentRegistrar ${declared.joinToString(", ")}; " +
                "the plugin host loads CompilerPluginRegistrar plugins only"
        )
    }
}

/** Ends the loaded set's lifetime, if one is loaded. */
private fun unload() {
    val set = current ?: return
    current = null
    set.close()
}

/** Puts every resource the registrars registered under [resources], as kotlinc puts them under its project. */
private fun own(resources: Disposable, storage: CompilerPluginRegistrar.ExtensionStorage) {
    for (resource in storage.disposables) {
        Disposer.register(resources, Disposable { resource.dispose() })
    }
}

/** How many extensions [storage] holds per extension point, by name, in registration order. */
private fun counts(storage: CompilerPluginRegistrar.ExtensionStorage): Map<String, Int> =
    storage.registeredExtensions.entries.associate { (point, extensions) -> point.name to extensions.size }

private fun describe(error: Throwable): String {
    val causes = generateSequence(error) { it.cause }.joinToString(": caused by ") { it.toString() }
    val at = error.stackTrace.firstOrNull()?.let { " (at $it)" } ?: ""
    return causes + at
}
