package headers

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class HeadersProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = HeadersProcessor(environment)
}

/**
 * Logs each classifier's header: its modifiers, type parameters and supertypes, once. It lists a
 * class's declarations only to reach the classifiers nested in `Outer` and `Holder`.
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
        for (name in listOf("headers.Outer", "headers.Holder")) {
            val outer = resolver.getClassDeclarationByName(resolver.getKSNameFromString(name))!!
            for (nested in outer.declarations) {
                if (nested is KSClassDeclaration) log(nested)
            }
        }
        // `Middle<*>` as `Middle` writes it in its `Comparable` supertype, against its star projection.
        val middle = resolver.getClassDeclarationByName(resolver.getKSNameFromString("headers.Middle"))!!
        val comparable = middle.superTypes.map { it.resolve() }.first { it.declaration.simpleName.asString() == "Comparable" }
        val written = comparable.arguments.single().type!!.resolve()
        val star = middle.asStarProjectedType()
        env.logger.warn(
            "written ${type(written)} equals star=${written == star} star equals written=${star == written}" +
                " same hash=${written.hashCode() == star.hashCode()}",
        )
        return emptyList()
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
            .append(" starArguments=").append(star.arguments.joinToString(", ", "[", "]") {
                "${it.variance} type=${it.type} origin=${it.origin} parent=${it.parent} text=$it"
            })
        env.logger.warn(out.toString())
    }
}
