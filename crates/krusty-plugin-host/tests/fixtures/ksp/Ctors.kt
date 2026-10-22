package ctors

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class CtorsProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = CtorsProcessor(environment)
}

/** Logs each top-level classifier's primary constructor and its parameters, once. */
class CtorsProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) {
                if (declaration is KSClassDeclaration) log(declaration)
            }
        }
        return emptyList()
    }

    private fun log(declaration: KSClassDeclaration) {
        val out = StringBuilder()
        out.append(declaration.qualifiedName?.asString()).append(" primary=")
        val primary = declaration.primaryConstructor
        if (primary == null) {
            out.append("null")
        } else {
            out.append(primary.simpleName.asString())
                .append(" parent=").append(primary.parentDeclaration?.qualifiedName?.asString())
                .append(" (")
            var first = true
            for (parameter in primary.parameters) {
                if (!first) out.append(", ")
                first = false
                out.append(parameter.name?.asString())
                    .append(" val=").append(parameter.isVal)
                    .append(" var=").append(parameter.isVar)
                    .append(" vararg=").append(parameter.isVararg)
                    .append(" default=").append(parameter.hasDefault)
            }
            out.append(")")
        }
        env.logger.warn(out.toString())
    }
}
