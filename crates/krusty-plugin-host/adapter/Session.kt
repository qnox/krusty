package krusty.pluginhost

import org.jetbrains.kotlin.backend.common.extensions.IrGenerationExtension
import org.jetbrains.kotlin.cli.common.arguments.K2JVMCompilerArguments
import org.jetbrains.kotlin.cli.common.messages.CompilerMessageSeverity
import org.jetbrains.kotlin.cli.common.messages.CompilerMessageSourceLocation
import org.jetbrains.kotlin.cli.common.messages.GroupingMessageCollector
import org.jetbrains.kotlin.cli.common.messages.MessageCollector
import org.jetbrains.kotlin.cli.common.messages.MessageRenderer
import org.jetbrains.kotlin.cli.pipeline.ArgumentsPipelineArtifact
import org.jetbrains.kotlin.cli.pipeline.CheckCompilationErrors
import org.jetbrains.kotlin.cli.pipeline.ConfigurationPipelineArtifact
import org.jetbrains.kotlin.cli.pipeline.PipelineStepException
import org.jetbrains.kotlin.cli.pipeline.jvm.JvmConfigurationPipelinePhase
import org.jetbrains.kotlin.cli.pipeline.jvm.JvmFir2IrPipelinePhase
import org.jetbrains.kotlin.cli.pipeline.jvm.JvmFrontendPipelinePhase
import org.jetbrains.kotlin.com.intellij.openapi.Disposable
import org.jetbrains.kotlin.com.intellij.openapi.util.Disposer
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.config.Services
import org.jetbrains.kotlin.fir.pipeline.Fir2IrActualizedResult
import org.jetbrains.kotlin.ir.IrElement
import org.jetbrains.kotlin.ir.declarations.IrClass
import org.jetbrains.kotlin.ir.declarations.IrFunction
import org.jetbrains.kotlin.ir.declarations.IrSimpleFunction
import org.jetbrains.kotlin.ir.declarations.IrParameterKind
import org.jetbrains.kotlin.ir.symbols.IrClassSymbol
import org.jetbrains.kotlin.ir.symbols.IrTypeParameterSymbol
import org.jetbrains.kotlin.ir.types.IrSimpleType
import org.jetbrains.kotlin.ir.types.IrStarProjection
import org.jetbrains.kotlin.ir.types.IrType
import org.jetbrains.kotlin.ir.types.IrTypeArgument
import org.jetbrains.kotlin.ir.types.IrTypeProjection
import org.jetbrains.kotlin.ir.types.isMarkedNullable
import org.jetbrains.kotlin.ir.util.classId
import org.jetbrains.kotlin.ir.util.parentClassOrNull
import org.jetbrains.kotlin.ir.util.render
import org.jetbrains.kotlin.name.CallableId
import org.jetbrains.kotlin.name.ClassId
import org.jetbrains.kotlin.name.FqName
import org.jetbrains.kotlin.name.Name
import org.jetbrains.kotlin.platform.jvm.JvmPlatforms
import org.jetbrains.kotlin.types.Variance
import org.jetbrains.kotlin.util.PerformanceManagerImpl
import java.io.DataInputStream
import java.io.File

/**
 * What a compilation's session is opened over: the build's exact classpath and JDK, and the settings
 * that decide how kotlinc reads them.
 */
internal class SessionRequest(
    val classpath: List<String>,
    val jdkHome: String,
    val moduleName: String,
    val languageVersion: String,
    val apiVersion: String,
    val jvmTarget: String,
)

/**
 * kotlinc's view of a compilation's dependencies: its K2 JVM frontend and fir2ir run over the
 * classpath alone, which gives the [Fir2IrActualizedResult] whose plugin context and builtins IR
 * plugins work with. The module's own sources are not part of it; krusty supplies their IR.
 */
private class Session(private val resources: Disposable, val ir: Fir2IrActualizedResult) {
    /** Whether the module's IR is built: a session builds one module, as a compilation does. */
    var generated = false

    fun close() = Disposer.dispose(resources)
}

/** The open session. A host serves one compilation at a time. */
private var session: Session? = null

/**
 * Opens a session over [request] the way kotlinc's own JVM pipeline starts a compilation, with no
 * source files, replacing the session open before. kotlinc's errors refuse it, in kotlinc's words;
 * its warnings go to stderr, which is the user's.
 */
internal fun openSession(request: SessionRequest) {
    closeSession()
    for (entry in request.classpath) {
        // kotlinc takes the classpath as one string, so such an entry would be read as two.
        if (File.pathSeparator in entry) {
            throw Refused("classpath entry $entry contains the path separator ${File.pathSeparator}")
        }
    }
    val arguments = K2JVMCompilerArguments().apply {
        classpath = request.classpath.joinToString(File.pathSeparator)
        jdkHome = request.jdkHome
        moduleName = request.moduleName
        languageVersion = request.languageVersion
        apiVersion = request.apiVersion
        jvmTarget = request.jvmTarget
        // The classpath is the build's own, standard library included.
        noStdlib = true
        noReflect = true
        allowNoSourceFiles = true
    }
    val messages = Messages()
    val collector = GroupingMessageCollector(messages, false, false)
    val resources = Disposer.newDisposable("krusty plugin host session")
    try {
        val performance = PerformanceManagerImpl(JvmPlatforms.defaultJvmPlatform, "krusty plugin host")
        val input = ArgumentsPipelineArtifact(arguments, Services.EMPTY, resources, collector, performance)
        // Some supported compilers declare the configuration phase's result nullable.
        val configured: ConfigurationPipelineArtifact? = JvmConfigurationPipelinePhase.executePhase(input)
        val configuration = finished(configured, "configuration", configured?.configuration, collector, messages)
        val frontend = finished(
            JvmFrontendPipelinePhase.executePhase(configuration),
            "frontend",
            configuration.configuration,
            collector,
            messages,
        )
        val ir = finished(
            JvmFir2IrPipelinePhase.executePhase(frontend, emptyList()),
            "fir2ir",
            configuration.configuration,
            collector,
            messages,
        )
        session = Session(resources, ir.result)
    } catch (error: Throwable) {
        Disposer.dispose(resources)
        if (error is PipelineStepException) {
            collector.flush()
            throw Refused(messages.errors())
        }
        throw error
    }
}

/**
 * The result of a pipeline phase, refusing the session when kotlinc reported an error so far, as its
 * pipeline stops between phases, or when the phase produced nothing.
 */
private fun <T : Any> finished(
    result: T?,
    phase: String,
    /** Where the phase reported its diagnostics, which kotlinc renders to the message collector. */
    configuration: CompilerConfiguration?,
    collector: GroupingMessageCollector,
    messages: Messages,
): T {
    configuration?.let { CheckCompilationErrors.CheckDiagnosticCollector.reportToMessageCollector(it) }
    collector.flush()
    if (messages.hasErrors()) throw Refused(messages.errors())
    return result ?: throw Refused("kotlinc's $phase phase stopped without reporting why")
}

/**
 * Builds the module [input] frames in the open session, adds its files to the session's module as
 * fir2ir adds a module's sources, and runs [extensions] over it in order, with the session's plugin
 * context, as kotlinc runs IR generation extensions after fir2ir. IR the host cannot build refuses
 * the request before anything is added, and the session can still build its module.
 */
internal fun generate(input: DataInputStream, extensions: List<IrGenerationExtension>) {
    val open = session ?: throw Refused("no session is open")
    if (open.generated) throw Refused("the session's module is already built; a session builds one module")
    val files = ModuleBuilder(open.ir).read(input)
    open.generated = true
    val module = open.ir.irModuleFragment
    module.files += files
    for (extension in extensions) extension.generate(module, open.ir.pluginContext)
}

/** Ends the open session, if one is open. */
internal fun closeSession() {
    val open = session ?: return
    session = null
    open.close()
}

/** What kotlinc reports while opening a session: errors are kept for the reply, warnings printed. */
private class Messages : MessageCollector {
    private val errors = mutableListOf<String>()

    override fun report(severity: CompilerMessageSeverity, message: String, location: CompilerMessageSourceLocation?) {
        if (severity in CompilerMessageSeverity.VERBOSE) return
        val text = MessageRenderer.PLAIN_RELATIVE_PATHS.render(severity, message, location)
        if (severity.isError) errors += text else System.err.println(text)
    }

    override fun hasErrors() = errors.isNotEmpty()

    override fun clear() = errors.clear()

    fun errors() = errors.joinToString("\n")
}

/** A class by its package and its names from the outermost class in. */
internal class ClassName(val packageName: String, val classes: List<String>) {
    val classId: ClassId
        get() = ClassId(FqName(packageName), FqName(classes.joinToString(".")), false)

    override fun toString() = classId.asFqNameString()
}

/** A type in a declaration's signature, as kotlinc's IR spells it. */
internal sealed interface TypeIdentity {
    val nullable: Boolean

    /** The classes of the type's annotations, which mark extension function types and platform types. */
    val annotations: List<ClassName>
}

internal class ClassType(
    val classifier: ClassName,
    val arguments: List<ArgumentIdentity>,
    override val nullable: Boolean,
    override val annotations: List<ClassName>,
) : TypeIdentity

/**
 * The [index]th type parameter of the class [owner], or of the declaration itself when [owner] is
 * null: an inner class's members name their outer class's type parameters by that class.
 */
internal class TypeParameterType(
    val owner: ClassName?,
    val index: Int,
    override val nullable: Boolean,
    override val annotations: List<ClassName>,
) : TypeIdentity

internal sealed interface ArgumentIdentity

internal object Star : ArgumentIdentity

internal class Projection(val variance: Variance, val type: TypeIdentity) : ArgumentIdentity

internal enum class CallableKind { FUNCTION, PROPERTY, CONSTRUCTOR }

/**
 * A declaration on the session's classpath, named by its owner and signature: the parameter types
 * kotlinc declares, a vararg parameter by its array type. It names one declaration or none.
 */
internal class CallableIdentity(
    val kind: CallableKind,
    val packageName: String,
    val classes: List<String>,
    val name: String,
    val typeParameters: Int,
    val contextParameters: List<TypeIdentity>,
    val extensionReceiver: TypeIdentity?,
    val valueParameters: List<TypeIdentity>,
) {
    override fun toString(): String {
        val owner = (listOf(packageName).filter { it.isNotEmpty() } + classes).joinToString(".")
        val context = if (contextParameters.isEmpty()) "" else "context(${contextParameters.joinToString(", ") { render(it) }}) "
        val receiver = extensionReceiver?.let { "${render(it)}." } ?: ""
        val keyword = when (kind) {
            CallableKind.FUNCTION -> "fun"
            CallableKind.PROPERTY -> "val"
            CallableKind.CONSTRUCTOR -> "constructor"
        }
        val path = if (kind == CallableKind.CONSTRUCTOR) owner else listOf(owner, name).filter { it.isNotEmpty() }.joinToString(".")
        val parameters = if (kind == CallableKind.PROPERTY) "" else valueParameters.joinToString(", ", "(", ")") { render(it) }
        val generics = if (typeParameters == 0) "" else "<$typeParameters type parameters> "
        return "$keyword $generics$context$receiver$path$parameters"
    }
}

private fun render(type: TypeIdentity): String {
    val annotations = type.annotations.joinToString("") { "@$it " }
    val base = when (type) {
        is ClassType -> type.classifier.toString() +
            if (type.arguments.isEmpty()) "" else type.arguments.joinToString(", ", "<", ">") { render(it) }
        is TypeParameterType -> "type parameter ${type.index}" + (type.owner?.let { " of $it" } ?: "")
    }
    return annotations + base + if (type.nullable) "?" else ""
}

private fun render(argument: ArgumentIdentity): String = when (argument) {
    Star -> "*"
    is Projection -> when (argument.variance) {
        Variance.INVARIANT -> ""
        Variance.IN_VARIANCE -> "in "
        Variance.OUT_VARIANCE -> "out "
    } + render(argument.type)
}

/**
 * Finds each of [identities] among the session's declarations, through the plugin context IR plugins
 * look declarations up with, and renders it as kotlinc renders IR. An identity that names no
 * declaration, or more than one, refuses the request by name.
 */
internal fun resolve(identities: List<CallableIdentity>): List<String> {
    val ir = (session ?: throw Refused("no session is open")).ir
    return identities.map { identity -> render(resolveIn(ir, identity), identity) }
}

/** The function [identity] names on [ir]'s classpath, refusing it as [resolve] does. */
internal fun resolveFunction(ir: Fir2IrActualizedResult, identity: CallableIdentity): IrSimpleFunction =
    match(ir, identity).second as IrSimpleFunction

private fun resolveIn(ir: Fir2IrActualizedResult, identity: CallableIdentity): IrElement = match(ir, identity).first

/** The declaration [identity] names, with the function whose signature it was matched by. */
private fun match(ir: Fir2IrActualizedResult, identity: CallableIdentity): Pair<IrElement, IrFunction> {
    val context = ir.pluginContext
    val owner = identity.classes.takeIf { it.isNotEmpty() }?.let { ClassName(identity.packageName, it).classId }
    fun callableId() = owner?.let { CallableId(it, Name.identifier(identity.name)) }
        ?: CallableId(FqName(identity.packageName), Name.identifier(identity.name))
    val candidates: List<Pair<IrElement, IrFunction>> = when (identity.kind) {
        CallableKind.FUNCTION -> context.referenceFunctions(callableId()).map { it.owner to it.owner }
        CallableKind.CONSTRUCTOR -> context.referenceConstructors(
            owner ?: throw Refused("$identity: a constructor needs a class"),
        ).map { it.owner to it.owner }
        CallableKind.PROPERTY -> context.referenceProperties(callableId()).mapNotNull { symbol ->
            symbol.owner.getter?.let { symbol.owner to it }
        }
    }
    val matches = candidates.filter { (_, function) -> signatureIs(function, identity) }
    return when (matches.size) {
        1 -> matches.single()
        0 -> throw Refused("$identity names no declaration on the session's classpath")
        else -> throw Refused("$identity names ${matches.size} declarations on the session's classpath")
    }
}

private fun render(element: IrElement, identity: CallableIdentity): String = try {
    element.render()
} catch (error: Throwable) {
    throw IllegalStateException("could not render $identity", error)
}

private fun signatureIs(function: IrFunction, identity: CallableIdentity): Boolean {
    if (function.typeParameters.size != identity.typeParameters) return false
    fun ofKind(kind: IrParameterKind) = function.parameters.filter { it.kind == kind }.map { it.type }
    if (identity.kind == CallableKind.PROPERTY && ofKind(IrParameterKind.Regular).isNotEmpty()) return false
    return typesAre(ofKind(IrParameterKind.Context), identity.contextParameters, function) &&
        typesAre(ofKind(IrParameterKind.ExtensionReceiver), listOfNotNull(identity.extensionReceiver), function) &&
        typesAre(ofKind(IrParameterKind.Regular), identity.valueParameters, function)
}

private fun typesAre(types: List<IrType>, identities: List<TypeIdentity>, function: IrFunction): Boolean =
    types.size == identities.size && types.zip(identities).all { (type, identity) -> typeIs(type, identity, function) }

private fun typeIs(type: IrType, identity: TypeIdentity, function: IrFunction): Boolean {
    if (type !is IrSimpleType || type.isMarkedNullable() != identity.nullable) return false
    val annotations = type.annotations.map { it.symbol.owner.parentClassOrNull?.classId }
    if (annotations != identity.annotations.map { it.classId }) return false
    return when (identity) {
        is ClassType -> {
            val classifier = type.classifier as? IrClassSymbol ?: return false
            classifier.owner.classId == identity.classifier.classId &&
                type.arguments.size == identity.arguments.size &&
                type.arguments.zip(identity.arguments).all { (argument, expected) -> argumentIs(argument, expected, function) }
        }
        is TypeParameterType -> {
            val parameter = (type.classifier as? IrTypeParameterSymbol)?.owner ?: return false
            val ownedHere = when (val owner = identity.owner) {
                null -> parameter.parent == function
                else -> (parameter.parent as? IrClass)?.classId == owner.classId
            }
            parameter.index == identity.index && ownedHere
        }
    }
}

private fun argumentIs(argument: IrTypeArgument, identity: ArgumentIdentity, function: IrFunction): Boolean =
    when (identity) {
        Star -> argument is IrStarProjection
        is Projection -> argument is IrTypeProjection &&
            argument.variance == identity.variance &&
            typeIs(argument.type, identity.type, function)
    }

internal fun DataInputStream.readSessionRequest() = SessionRequest(
    classpath = readList { readString() },
    jdkHome = readString(),
    moduleName = readString(),
    languageVersion = readString(),
    apiVersion = readString(),
    jvmTarget = readString(),
)

internal fun DataInputStream.readCallable(): CallableIdentity {
    val kind = when (val tag = readByte().toInt()) {
        0 -> CallableKind.FUNCTION
        1 -> CallableKind.PROPERTY
        2 -> CallableKind.CONSTRUCTOR
        else -> throw Refused("unknown callable kind $tag")
    }
    val packageName = readString()
    val classes = readList { readString() }
    val name = if (kind == CallableKind.CONSTRUCTOR) "<init>" else readString()
    val typeParameters = readInt()
    val contextParameters = readList { readType() }
    val extensionReceiver = if (readBoolean()) readType() else null
    val valueParameters = readList { readType() }
    return CallableIdentity(kind, packageName, classes, name, typeParameters, contextParameters, extensionReceiver, valueParameters)
}

private fun DataInputStream.readClassName() = ClassName(readString(), readList { readString() })

internal fun DataInputStream.readType(): TypeIdentity = when (val tag = readByte().toInt()) {
    0 -> {
        val classifier = readClassName()
        val arguments = readList { readArgument() }
        ClassType(classifier, arguments, readBoolean(), readList { readClassName() })
    }
    1 -> {
        val owner = when (val tag = readByte().toInt()) {
            0 -> null
            1 -> readClassName()
            else -> throw Refused("unknown type parameter owner $tag")
        }
        val index = readInt()
        TypeParameterType(owner, index, readBoolean(), readList { readClassName() })
    }
    else -> throw Refused("unknown type kind $tag")
}

private fun DataInputStream.readArgument(): ArgumentIdentity = when (val tag = readByte().toInt()) {
    0 -> Star
    1 -> Projection(Variance.INVARIANT, readType())
    2 -> Projection(Variance.IN_VARIANCE, readType())
    3 -> Projection(Variance.OUT_VARIANCE, readType())
    else -> throw Refused("unknown type argument kind $tag")
}
