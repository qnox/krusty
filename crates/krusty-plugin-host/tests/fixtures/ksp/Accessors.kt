package accessors

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class AccessorsProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = AccessorsProcessor(environment)
}

/** Logs each source property's getter return type, once. */
class AccessorsProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) visit(declaration)
        }
        return emptyList()
    }

    private fun visit(declaration: KSDeclaration) {
        when (declaration) {
            is KSClassDeclaration -> declaration.declarations.forEach(::visit)
            is KSPropertyDeclaration -> log(declaration)
            else -> {}
        }
    }

    private fun log(property: KSPropertyDeclaration) {
        val name = property.qualifiedName?.asString()
        val getter = property.getter
        if (getter == null) {
            env.logger.warn("getter $name null")
            return
        }
        val type = getter.returnType
        if (type == null) {
            env.logger.warn("getter $name returnType=null")
            return
        }
        val parent = when (val p = type.parent) {
            null -> "null"
            getter -> "getter"
            property -> "property"
            else -> p.javaClass.simpleName
        }
        env.logger.warn(
            "getter $name origin=${getter.origin} type=${render(type.resolve())} typeOrigin=${type.origin}" +
                " sameAsProperty=${type === property.type} equalsProperty=${type == property.type}" +
                " parent=$parent"
        )
    }

    private fun render(type: KSType): String {
        val out = StringBuilder()
        out.append(type.declaration.qualifiedName?.asString() ?: type.declaration.simpleName.asString())
        if (type.arguments.isNotEmpty()) {
            out.append(type.arguments.joinToString(", ", "<", ">") { argument ->
                when (argument.variance) {
                    Variance.STAR -> "*"
                    Variance.COVARIANT -> "out " + render(argument.type!!.resolve())
                    Variance.CONTRAVARIANT -> "in " + render(argument.type!!.resolve())
                    Variance.INVARIANT -> render(argument.type!!.resolve())
                }
            })
        }
        if (type.isMarkedNullable) out.append('?')
        return out.toString()
    }
}
