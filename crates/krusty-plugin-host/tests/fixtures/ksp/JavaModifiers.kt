package javamodifiers

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class JavaModifiersProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = JavaModifiersProcessor(environment)
}

/** Logs `Resolver.effectiveJavaModifiers` of every source declaration, once. */
@OptIn(com.google.devtools.ksp.KspExperimental::class)
class JavaModifiersProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) visit(resolver, declaration)
        }
        return emptyList()
    }

    private fun visit(resolver: Resolver, declaration: KSDeclaration) {
        val modifiers = resolver.effectiveJavaModifiers(declaration).map { it.name }.sorted()
        val name = declaration.qualifiedName?.asString() ?: declaration.simpleName.asString()
        val kind = when (declaration) {
            is KSClassDeclaration -> declaration.classKind.name
            is KSFunctionDeclaration -> "FUNCTION"
            is KSPropertyDeclaration -> "PROPERTY"
            else -> declaration.javaClass.simpleName
        }
        env.logger.warn("$kind $name ${modifiers.joinToString(",", "[", "]")}")
        if (declaration is KSClassDeclaration) {
            for (member in declaration.declarations) visit(resolver, member)
        }
    }
}
