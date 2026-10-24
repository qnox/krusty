package headers

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class HeadersProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = HeadersProcessor(environment)
}

/**
 * Logs each classifier's header: its modifiers, type parameters and supertypes, once. It lists a
 * class's declarations only to reach the classifiers nested in `Outer`.
 */
class HeadersProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) {
                if (declaration is KSClassDeclaration) log(declaration)
            }
        }
        val outer = resolver.getClassDeclarationByName(resolver.getKSNameFromString("headers.Outer"))!!
        for (nested in outer.declarations) {
            if (nested is KSClassDeclaration) log(nested)
        }
        assignability(resolver, outer)
        return emptyList()
    }

    private val libraryNames = listOf(
        "kotlin.Any", "kotlin.Nothing", "kotlin.Comparable", "java.io.Serializable", "kotlin.Enum",
        "kotlin.Function0", "kotlin.Function", "kotlin.Annotation", "kotlin.CharSequence", "kotlin.String",
    )

    /**
     * Logs, for each classifier's star-projected type and its nullable form, which of those types
     * it is assignable from.
     */
    private fun assignability(resolver: Resolver, outer: KSClassDeclaration) {
        val classes = mutableListOf<KSClassDeclaration>()
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) {
                if (declaration is KSClassDeclaration) classes.add(declaration)
            }
        }
        for (nested in outer.declarations) {
            if (nested is KSClassDeclaration) classes.add(nested)
        }
        for (name in libraryNames) {
            classes.add(resolver.getClassDeclarationByName(resolver.getKSNameFromString(name))!!)
        }
        val types = classes.flatMap {
            val star = it.asStarProjectedType()
            listOf(star, star.makeNullable())
        }
        for (receiver in types) {
            val assignable = types.filter { receiver.isAssignableFrom(it) }.map { type(it) }
            env.logger.warn("assignable ${type(receiver)} <- ${assignable.joinToString(", ")}")
        }
    }

    private fun type(reference: KSTypeReference): String = type(reference.resolve())

    private fun type(type: KSType): String {
        val out = StringBuilder()
        val declaration = type.declaration
        out.append(declaration.qualifiedName?.asString() ?: declaration.simpleName.asString())
        if (declaration is KSTypeParameter) out.append(" [type parameter of ")
            .append(declaration.parentDeclaration?.qualifiedName?.asString()).append(']')
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

    private fun log(declaration: KSClassDeclaration) {
        val out = StringBuilder()
        out.append(declaration.classKind).append(' ').append(declaration.qualifiedName?.asString())
            .append(" modifiers=").append(declaration.modifiers.map { it.name }.sorted().joinToString(","))
            .append(" typeParameters=[")
        var first = true
        for (parameter in declaration.typeParameters) {
            if (!first) out.append("; ")
            first = false
            out.append(parameter.name.asString())
                .append(" qualified=").append(parameter.qualifiedName?.asString())
                .append(" variance=").append(parameter.variance)
                .append(" reified=").append(parameter.isReified)
                .append(" bounds=").append(parameter.bounds.map { type(it) }.joinToString(" & "))
        }
        out.append("] supertypes=").append(declaration.superTypes.map { type(it) }.joinToString(", "))
        val star = declaration.asStarProjectedType()
        out.append(" star=").append(type(star)).append(" starText=").append(star.toString())
        env.logger.warn(out.toString())
    }
}
