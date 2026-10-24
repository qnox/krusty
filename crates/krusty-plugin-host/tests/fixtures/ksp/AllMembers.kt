package allmembers

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class AllMembersProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = AllMembersProcessor(environment)
}

/** Logs what `getAllFunctions` and `getAllProperties` list for every classifier of every file, once. */
class AllMembersProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) {
                if (declaration is KSClassDeclaration) dump(declaration)
            }
        }
        return emptyList()
    }

    private fun dump(c: KSClassDeclaration) {
        val name = c.qualifiedName!!.asString()
        for (f in c.getAllFunctions()) {
            val parameters = f.parameters.joinToString(", ") { "${it.name?.asString()}: ${it.type.resolve()}" }
            env.logger.warn(
                "$name fun ${f.simpleName.asString()}($parameters): ${f.returnType?.resolve()} " +
                    "parent=${f.parentDeclaration?.qualifiedName?.asString()} origin=${f.origin} " +
                    "modifiers=${f.modifiers.map { it.name }.sorted()} abstract=${f.isAbstract}"
            )
        }
        for (p in c.getAllProperties()) {
            env.logger.warn(
                "$name property ${p.simpleName.asString()}: ${p.type.resolve()} " +
                    "parent=${p.parentDeclaration?.qualifiedName?.asString()} origin=${p.origin} " +
                    "modifiers=${p.modifiers.map { it.name }.sorted()}"
            )
        }
        for (d in c.declarations) if (d is KSClassDeclaration) dump(d)
    }
}
