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
import kotlin.script.experimental.api.filePathPattern
import kotlin.script.experimental.api.providedProperties
import kotlin.script.experimental.host.createCompilationConfigurationFromTemplate
import kotlin.script.experimental.jvm.defaultJvmScriptingHostConfiguration
import kotlin.reflect.KClass
import kotlin.reflect.KType
import kotlin.reflect.KVariance
import kotlin.reflect.full.primaryConstructor

/**
 * The compilation configuration of the script template [template], a class on [classpath], made as
 * kotlinc makes a `-script-templates` definition: the template's `@KotlinScript` names the
 * configuration, which is built from the template class with kotlinc's default JVM host
 * configuration. The template loads in a class loader over [classpath] whose parent holds the
 * scripting libraries, as kotlinc's does, so the configuration it builds is the one this host reads.
 * A script's constructor takes the parameters of its base class's primary constructor, as kotlinc's
 * does, so those are read from the base class's Kotlin metadata.
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
        val baseClass = configuration[ScriptCompilationConfiguration.baseClass]
        val filePathPattern = configuration[ScriptCompilationConfiguration.filePathPattern].orEmpty()
        ScriptDefined(
            fileExtension = configuration[ScriptCompilationConfiguration.fileExtension] ?: "kts",
            baseClass = baseClass?.let(::ScriptType),
            constructorParameters = baseClass?.let { constructorParameters(it, loader, template) }.orEmpty(),
            implicitReceivers = configuration[ScriptCompilationConfiguration.implicitReceivers].orEmpty().map(::ScriptType),
            providedProperties = configuration[ScriptCompilationConfiguration.providedProperties].orEmpty()
                .map { (name, type) -> name to ScriptType(type) },
            defaultImports = configuration[ScriptCompilationConfiguration.defaultImports].orEmpty(),
            isStandalone = configuration[ScriptCompilationConfiguration.isStandalone] ?: true,
            otherKeys = configuration.entries().map { it.key.name }.filter { it !in READ_KEYS }
                .plus(listOfNotNull("filePathPattern".takeIf { filePathPattern.isNotEmpty() }))
                .sorted(),
        )
    }
}

/**
 * The parameters of the primary constructor of [baseClass], the class every script of [template]
 * extends, by name and type.
 */
private fun constructorParameters(
    baseClass: KotlinType,
    loader: ClassLoader,
    template: String,
): List<Pair<String, ScriptType>> {
    val kotlinClass = baseClass.fromClass ?: Class.forName(baseClass.typeName, false, loader).kotlin
    val constructor = kotlinClass.primaryConstructor
        ?: throw Refused("the base class ${baseClass.typeName} of the script template $template has no primary constructor")
    return constructor.parameters.map { parameter ->
        val name = parameter.name
            ?: throw Refused("a constructor parameter of ${baseClass.typeName}, the base class of $template, has no name")
        name to ScriptType(parameter.type, template)
    }
}

/**
 * The configuration keys [scriptDefinition] reads, or that only describe the definition to a host:
 * its display name, file name patterns and host configuration. A file path pattern is still named
 * when it is set, since one that is set chooses which files the template claims.
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

/** A type the configuration names, as the scripting API spells it, with its type arguments. */
internal class ScriptType(val name: String, val nullable: Boolean, val arguments: List<ScriptType> = emptyList()) {
    constructor(type: KotlinType) : this(type.typeName, type.isNullable)

    /** [type], a constructor parameter's type in a base class of [template]. */
    constructor(type: KType, template: String) : this(
        ((type.classifier as? KClass<*>)?.qualifiedName
            ?: throw Refused("the script template $template takes a parameter of type $type, which names no class")),
        type.isMarkedNullable,
        type.arguments.map { argument ->
            val argumentType = argument.type
            if (argument.variance != KVariance.INVARIANT || argumentType == null) {
                throw Refused("the script template $template takes a parameter of type $type, which has a projected type argument")
            }
            ScriptType(argumentType, template)
        },
    )

    fun write(out: DataOutputStream) {
        out.writeString(name)
        out.writeBoolean(nullable)
        out.writeInt(arguments.size)
        for (argument in arguments) argument.write(out)
    }
}

/** A template's definition: what [scriptDefinition] read, and the names of the keys it did not. */
internal class ScriptDefined(
    val fileExtension: String,
    val baseClass: ScriptType?,
    val constructorParameters: List<Pair<String, ScriptType>>,
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
        out.writeInt(constructorParameters.size)
        for ((name, type) in constructorParameters) {
            out.writeString(name)
            type.write(out)
        }
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
