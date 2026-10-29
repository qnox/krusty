package alloverridees

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class AllOverrideesProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = AllOverrideesProcessor(environment)
}

/** Logs `findOverridee` of every function each source classifier lists in `getAllFunctions`, once. */
class AllOverrideesProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false
    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) {
                if (declaration !is KSClassDeclaration) continue
                for (function in declaration.getAllFunctions()) {
                    val overridee = function.findOverridee()
                    val parent = function.parentDeclaration?.qualifiedName?.asString()
                    env.logger.warn(
                        "${declaration.simpleName.asString()}.${function.simpleName.asString()} ${function.origin}" +
                            " parent=$parent overrides ${overridee?.qualifiedName?.asString()} origin=${overridee?.origin}"
                    )
                }
            }
        }
        return emptyList()
    }
}
