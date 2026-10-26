package librarymembers

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class LibraryMembersProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = LibraryMembersProcessor(environment)
}

/**
 * Logs the members of each of a few classpath classifiers: the declarations it lists, then
 * the properties and functions it has with the inherited ones.
 */
class LibraryMembersProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    private val names = listOf(
        "kotlin.Any",
        "kotlin.Annotation",
        "java.lang.annotation.Retention",
        "java.lang.annotation.Target",
        "java.lang.annotation.Repeatable",
        "java.lang.annotation.Documented",
        "java.lang.Deprecated",
        "kotlin.Deprecated",
        "kotlin.Suppress",
        "kotlin.annotation.Target",
        "kotlin.jvm.JvmName",
        "dependency.Tag",
        "dependency.Choices",
        "dependency.Constants",
        "dependency.Constants.Entry",
        "dependency.Aliased",
    )

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (name in names) {
            val declaration = resolver.getClassDeclarationByName(resolver.getKSNameFromString(name))
            if (declaration == null) {
                env.logger.warn("$name null")
                continue
            }
            env.logger.warn("$name ${declaration.classKind} ${declaration.origin}")
            for (member in declaration.declarations) {
                env.logger.warn("  declares ${member(member)}")
                annotations(member)
            }
            for (property in declaration.getAllProperties()) env.logger.warn("  property ${member(property)}")
            for (function in declaration.getAllFunctions()) env.logger.warn("  function ${member(function)}")
        }
        return emptyList()
    }

    /** Logs the annotations of a declared member, its getter, and its parameters. */
    private fun annotations(member: KSDeclaration) {
        for (annotation in member.annotations) env.logger.warn("    annotated ${annotation(annotation)}")
        if (member is KSPropertyDeclaration) {
            member.getter?.annotations?.forEach { env.logger.warn("    getter annotated ${annotation(it)}") }
        }
        if (member is KSFunctionDeclaration) {
            for (parameter in member.parameters) {
                for (annotation in parameter.annotations) {
                    env.logger.warn("    parameter ${parameter.name?.asString()} annotated ${annotation(annotation)}")
                }
            }
        }
    }

    private fun annotation(annotation: KSAnnotation): String =
        "@" + annotation.shortName.asString() +
            "=" + annotation.annotationType.resolve().declaration.qualifiedName?.asString() +
            " target=" + annotation.useSiteTarget +
            " origin=" + annotation.origin +
            annotation.arguments.joinToString(", ", " (", ")") {
                "${it.name?.asString()}=${value(it.value)} ${it.origin}"
            }

    private fun value(value: Any?): String = when (value) {
        null -> "null"
        is String -> "\"" + value + "\""
        is KSType -> "type " + (value.declaration.qualifiedName?.asString() ?: "?")
        is KSClassDeclaration -> "entry " + value.qualifiedName?.asString()
        is KSAnnotation -> annotation(value)
        is List<*> -> value.joinToString(", ", "[", "]") { value(it) }
        else -> value.toString() + ":" + value.javaClass.simpleName
    }

    private fun member(declaration: KSDeclaration): String {
        val owner = declaration.parentDeclaration?.qualifiedName?.asString()
        val origin = declaration.origin
        val modifiers = declaration.modifiers.map { it.name }.sorted().joinToString(",")
        return when (declaration) {
            is KSPropertyDeclaration ->
                "val ${declaration.simpleName.asString()}: ${type(declaration.type)} mutable=${declaration.isMutable} " +
                    // krusty does not answer a Kotlin library property's backing field yet.
                    (if (origin == Origin.KOTLIN_LIB) "" else "backingField=${declaration.hasBackingField} ") +
                    "owner=$owner origin=$origin modifiers=$modifiers getter=${accessor(declaration.getter)} " +
                    "setter=${accessor(declaration.setter)}"
            is KSFunctionDeclaration ->
                "fun ${declaration.simpleName.asString()}(" +
                    declaration.parameters.joinToString(", ") { parameter(it) } +
                    "): ${declaration.returnType?.let { type(it) }} kind=${declaration.functionKind} " +
                    "abstract=${declaration.isAbstract} owner=$owner origin=$origin modifiers=$modifiers"
            is KSClassDeclaration -> "class ${declaration.qualifiedName?.asString()} ${declaration.classKind}"
            else -> "other ${declaration.simpleName.asString()}"
        }
    }

    private fun accessor(accessor: KSPropertyAccessor?): String =
        accessor?.let { "${it.origin}:" + it.modifiers.map { modifier -> modifier.name }.sorted().joinToString(",") }
            ?: "none"

    private fun parameter(parameter: KSValueParameter): String =
        (if (parameter.isVararg) "vararg " else "") + "${parameter.name?.asString()}: ${type(parameter.type)}" +
            (if (parameter.hasDefault) " = default" else "")

    private fun type(reference: KSTypeReference): String = type(reference.resolve())

    private fun type(type: KSType): String {
        val out = StringBuilder()
        val declaration = type.declaration
        out.append(declaration.qualifiedName?.asString() ?: declaration.simpleName.asString())
        if (type.arguments.isNotEmpty()) {
            out.append(type.arguments.joinToString(", ", "<", ">") { argument ->
                when (argument.variance) {
                    Variance.STAR -> "*"
                    Variance.COVARIANT -> "out " + type(argument.type!!)
                    Variance.CONTRAVARIANT -> "in " + type(argument.type!!)
                    Variance.INVARIANT -> type(argument.type!!)
                }
            })
        }
        if (type.isMarkedNullable) out.append('?')
        return out.toString()
    }
}
