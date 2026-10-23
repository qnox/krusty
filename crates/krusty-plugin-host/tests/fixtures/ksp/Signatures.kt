package signatures

import com.google.devtools.ksp.KspExperimental
import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class SignaturesProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = SignaturesProcessor(environment)
}

/**
 * Logs the JVM signature the resolver maps each source declaration to: every file's declarations
 * and, inside each classifier, its declarations, accessors and constructor parameters.
 */
@OptIn(KspExperimental::class)
class SignaturesProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false
    private lateinit var resolver: Resolver

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        this.resolver = resolver
        for (file in resolver.getAllFiles().sortedBy { it.fileName }) {
            for (declaration in file.declarations) log(declaration, "")
        }
        return emptyList()
    }

    private fun log(declaration: KSDeclaration, indent: String) {
        env.logger.warn("$indent${label(declaration)} -> ${resolver.mapToJvmSignature(declaration)}")
        if (declaration is KSPropertyDeclaration) {
            declaration.getter?.let { env.logger.warn("$indent  getter -> ${resolver.getJvmName(it)}") }
            declaration.setter?.let { env.logger.warn("$indent  setter -> ${resolver.getJvmName(it)}") }
        }
        if (declaration is KSFunctionDeclaration) {
            env.logger.warn("$indent  jvmName -> ${resolver.getJvmName(declaration)}")
        }
        if (declaration is KSClassDeclaration) {
            for (member in declaration.declarations) log(member, "$indent  ")
        }
    }

    private fun label(declaration: KSDeclaration): String {
        val kind = when (declaration) {
            is KSClassDeclaration -> declaration.classKind.name
            is KSFunctionDeclaration -> "FUNCTION"
            is KSPropertyDeclaration -> "PROPERTY"
            is KSTypeAlias -> "TYPEALIAS"
            else -> "OTHER"
        }
        return "$kind ${declaration.simpleName.asString()}"
    }
}
