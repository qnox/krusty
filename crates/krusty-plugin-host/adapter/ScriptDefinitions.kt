package krusty.pluginhost

import java.io.DataOutputStream
import java.io.File
import java.net.URLClassLoader
import kotlin.script.experimental.api.KotlinType
import kotlin.script.experimental.api.ScriptCompilationConfiguration
import kotlin.script.experimental.api.baseClass
import kotlin.script.experimental.api.defaultImports
import kotlin.script.experimental.api.fileExtension
import kotlin.script.experimental.api.implicitReceivers
import kotlin.script.experimental.api.isStandalone
import kotlin.script.experimental.api.providedProperties
import kotlin.script.experimental.host.createCompilationConfigurationFromTemplate
import kotlin.script.experimental.jvm.defaultJvmScriptingHostConfiguration

/**
 * The compilation configuration of the script template [template], a class on [classpath], made as
 * kotlinc makes a `-script-templates` definition: the template's `@KotlinScript` names the
 * configuration, which is built from the template class with kotlinc's default JVM host
 * configuration. The template loads in a class loader over [classpath] whose parent holds the
 * scripting libraries, as kotlinc's does, so the configuration it builds is the one this host reads.
 */
internal fun scriptDefinition(classpath: List<String>, template: String): Reply {
    for (entry in classpath) checkEntry(File(entry), "script template")
    val loader = URLClassLoader(
        classpath.map { File(it).toURI().toURL() }.toTypedArray(),
        ScriptCompilationConfiguration::class.java.classLoader,
    )
    return loader.use {
        val templateClass = try {
            Class.forName(template, false, loader).kotlin
        } catch (_: ClassNotFoundException) {
            throw Refused("the script template $template is not on the classpath")
        }
        val configuration = createCompilationConfigurationFromTemplate(
            KotlinType(templateClass),
            defaultJvmScriptingHostConfiguration,
            templateClass,
        )
        ScriptDefined(
            fileExtension = configuration[ScriptCompilationConfiguration.fileExtension] ?: "kts",
            baseClass = configuration[ScriptCompilationConfiguration.baseClass]?.let(::ScriptType),
            implicitReceivers = configuration[ScriptCompilationConfiguration.implicitReceivers].orEmpty().map(::ScriptType),
            providedProperties = configuration[ScriptCompilationConfiguration.providedProperties].orEmpty()
                .map { (name, type) -> name to ScriptType(type) },
            defaultImports = configuration[ScriptCompilationConfiguration.defaultImports].orEmpty(),
            isStandalone = configuration[ScriptCompilationConfiguration.isStandalone] ?: true,
            otherKeys = configuration.entries().map { it.key.name }.filter { it !in READ_KEYS }.sorted(),
        )
    }
}

/**
 * The configuration keys [scriptDefinition] reads, or that only describe the definition to a host:
 * its display name, file name patterns and host configuration.
 */
private val READ_KEYS = setOf(
    "baseClass",
    "fileExtension",
    "implicitReceivers",
    "providedProperties",
    "defaultImports",
    "isStandalone",
    "displayName",
    "fileNamePattern",
    "filePathPattern",
    "hostConfiguration",
)

/** A type the configuration names, as the scripting API spells it. */
internal class ScriptType(val name: String, val nullable: Boolean) {
    constructor(type: KotlinType) : this(type.typeName, type.isNullable)

    fun write(out: DataOutputStream) {
        out.writeString(name)
        out.writeBoolean(nullable)
    }
}

/** A template's definition: what [scriptDefinition] read, and the names of the keys it did not. */
internal class ScriptDefined(
    val fileExtension: String,
    val baseClass: ScriptType?,
    val implicitReceivers: List<ScriptType>,
    val providedProperties: List<Pair<String, ScriptType>>,
    val defaultImports: List<String>,
    val isStandalone: Boolean,
    val otherKeys: List<String>,
) : Reply {
    override fun write(out: DataOutputStream) {
        out.writeByte(SCRIPT_DEFINED)
        out.writeString(fileExtension)
        out.writeBoolean(baseClass != null)
        baseClass?.write(out)
        out.writeInt(implicitReceivers.size)
        for (receiver in implicitReceivers) receiver.write(out)
        out.writeInt(providedProperties.size)
        for ((name, type) in providedProperties) {
            out.writeString(name)
            type.write(out)
        }
        out.writeInt(defaultImports.size)
        for (import in defaultImports) out.writeString(import)
        out.writeBoolean(isStandalone)
        out.writeInt(otherKeys.size)
        for (key in otherKeys) out.writeString(key)
    }
}
