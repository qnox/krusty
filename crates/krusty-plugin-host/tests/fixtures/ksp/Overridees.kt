package overridees

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class OverrideesProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = OverrideesProcessor(environment)
}

/**
 * Logs `findOverridee` of every source function and property, once: the overridee's name, origin,
 * classifier, and parameter types or type.
 */
class OverrideesProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
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
        val name = declaration.qualifiedName?.asString() ?: declaration.simpleName.asString()
        val overridee = when (declaration) {
            is KSFunctionDeclaration -> declaration.findOverridee()
            is KSPropertyDeclaration -> declaration.findOverridee()
            else -> null
        }
        if (declaration !is KSClassDeclaration) {
            val parent = overridee?.parentDeclaration?.qualifiedName?.asString()
            val parameters = (overridee as? KSFunctionDeclaration)?.parameters?.map { it.type.resolve() }
            val type = (overridee as? KSPropertyDeclaration)?.type?.resolve()
            env.logger.warn(
                "$name ${declaration.origin} overrides ${overridee?.qualifiedName?.asString()}" +
                    " origin=${overridee?.origin} parent=$parent params=$parameters type=$type"
            )
        }
        if (declaration is KSClassDeclaration) {
            for (member in declaration.declarations) visit(member)
        }
    }
}
