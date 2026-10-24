package library

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class LibraryProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = LibraryProcessor(environment)
}

/**
 * Logs the header of each classpath classifier a processor reaches: by name through the resolver,
 * and as the declaration of each source classifier's direct supertypes. A name the classpath does
 * not declare logs null.
 */
class LibraryProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    private val names = listOf(
        "kotlin.collections.List",
        "kotlin.collections.MutableMap",
        "kotlin.collections.Map.Entry",
        "kotlin.Comparable",
        "kotlin.Any",
        "kotlin.String",
        "kotlin.Int",
        "kotlin.Enum",
        "kotlin.Deprecated",
        "kotlin.ranges.IntRange",
        "kotlin.coroutines.Continuation",
        "kotlin.DeprecationLevel",
        "kotlin.Unit",
        "kotlin.Pair",
        "kotlin.collections.AbstractList",
        "kotlin.Function1",
        "kotlin.jvm.internal.FunctionImpl",
        "kotlin.internal.RequireKotlin",
        "kotlin.internal.RequireKotlin.Container",
        "kotlin.Result",
        "kotlin.io.Serializable",
        "java.io.Serializable",
        "java.lang.Thread",
        "java.lang.Thread.State",
        "java.util.AbstractList",
        "java.lang.annotation.Retention",
        "java.lang.Runnable",
        "java.util.ArrayList",
        "java.util.Map.Entry",
        "java.lang.Object",
        "java.lang.Iterable",
        "java.lang.AutoCloseable",
        "java.util.Comparator",
        "java.util.Collection",
        "dependency.Tag",
        "dependency.Tag.Container",
        "dependency.Tagged",
        "dependency.OneTag",
        "dependency.Old",
        "dependency.Wrapper",
        "dependency.Empty",
        "dependency.Shape",
        "dependency.Circle",
        "dependency.Holder",
        "dependency.Holder.Inner",
        "dependency.Holder.Bare",
        "dependency.Holder.Nested",
        "library.Missing",
    )

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (name in names) {
            val declaration = resolver.getClassDeclarationByName(resolver.getKSNameFromString(name))
            if (declaration == null) env.logger.warn("$name null") else log("$name ->", declaration)
        }
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) {
                if (declaration !is KSClassDeclaration) continue
                for (supertype in declaration.superTypes) {
                    val target = supertype.resolve().declaration
                    if (target is KSClassDeclaration) log("${declaration.simpleName.asString()} super", target)
                }
            }
        }
        return emptyList()
    }

    private fun type(reference: KSTypeReference): String = type(reference.resolve())

    private fun type(type: KSType): String {
        val out = StringBuilder()
        val declaration = type.declaration
        out.append(declaration.qualifiedName?.asString() ?: declaration.simpleName.asString())
        if (type.arguments.isNotEmpty()) {
            out.append('<')
            var first = true
            for (argument in type.arguments) {
                if (!first) out.append(", ")
                first = false
                when (argument.variance) {
                    Variance.STAR -> out.append('*')
                    Variance.COVARIANT -> out.append("out ").append(type(argument.type!!))
                    Variance.CONTRAVARIANT -> out.append("in ").append(type(argument.type!!))
                    Variance.INVARIANT -> out.append(type(argument.type!!))
                }
            }
            out.append('>')
        }
        if (type.isMarkedNullable) out.append('?')
        return out.toString()
    }

    private fun value(value: Any?): String = when (value) {
        null -> "null"
        // A source map's text follows the stdlib build, so a multi-line string logs only its shape.
        is String -> if ('\n' in value) "multi-line:String" else "\"" + value + "\":String"
        is KSType -> "type " + (value.declaration.qualifiedName?.asString() ?: "?")
        is KSClassDeclaration -> "entry " + value.classKind + " " + value.qualifiedName?.asString()
        is KSAnnotation -> annotation(value)
        is List<*> -> value.joinToString(", ", "[", "]:List") { value(it) }
        else -> value.toString() + ":" + value.javaClass.simpleName
    }

    private fun annotation(annotation: KSAnnotation): String =
        "@" + annotation.shortName.asString() +
            "=" + annotation.annotationType.resolve().declaration.qualifiedName?.asString() +
            " origin=" + annotation.origin +
            annotation.arguments.joinToString(", ", " (", ")") { "${it.name?.asString()}=${value(it.value)}" }

    private fun log(what: String, declaration: KSClassDeclaration) {
        val out = StringBuilder()
        out.append(what).append(' ').append(declaration.classKind).append(' ')
            .append(declaration.qualifiedName?.asString())
            .append(" simple=").append(declaration.simpleName.asString())
            .append(" package=").append(declaration.packageName.asString())
            .append(" parent=").append(declaration.parentDeclaration?.qualifiedName?.asString())
            .append(" origin=").append(declaration.origin)
            .append(" modifiers=").append(declaration.modifiers.map { it.name }.sorted().joinToString(","))
            .append(" typeParameters=[")
        var first = true
        for (parameter in declaration.typeParameters) {
            if (!first) out.append("; ")
            first = false
            out.append(parameter.name.asString())
                .append(" variance=").append(parameter.variance)
                .append(" bounds=").append(parameter.bounds.map { type(it) }.joinToString(" & "))
        }
        out.append("] supertypes=").append(declaration.superTypes.map { type(it) }.joinToString(", "))
            .append(" annotations=").append(declaration.annotations.map { annotation(it) }.joinToString("; "))
        val star = declaration.asStarProjectedType()
        out.append(" star=").append(type(star)).append(" starText=").append(star.toString())
            .append(" starArguments=").append(star.arguments.joinToString(", ", "[", "]") {
                "${it.variance} type=${it.type} origin=${it.origin} parent=${it.parent} text=$it"
            })
        env.logger.warn(out.toString())
    }
}
