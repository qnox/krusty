package members

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class MembersProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = MembersProcessor(environment)
}

/** Logs every declaration of every file with its modifiers, supertypes and types, once. */
class MembersProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) dump(declaration, "")
        }
        return emptyList()
    }

    private fun type(reference: KSTypeReference?): String {
        if (reference == null) return "null"
        return type(reference.resolve())
    }

    private fun type(type: KSType): String {
        val out = StringBuilder()
        out.append(type.declaration.qualifiedName?.asString() ?: type.declaration.simpleName.asString())
        if (type.arguments.isNotEmpty()) {
            out.append('<')
            var first = true
            for (argument in type.arguments) {
                if (!first) out.append(", ")
                first = false
                when (argument.variance) {
                    Variance.STAR -> out.append('*')
                    Variance.COVARIANT -> out.append("out ").append(type(argument.type))
                    Variance.CONTRAVARIANT -> out.append("in ").append(type(argument.type))
                    Variance.INVARIANT -> out.append(type(argument.type))
                }
            }
            out.append('>')
        }
        if (type.isMarkedNullable) out.append('?')
        return out.toString()
    }

    private fun modifiers(declaration: KSDeclaration): String =
        declaration.modifiers.map { it.name }.sorted().joinToString(",")

    private fun dump(declaration: KSDeclaration, indent: String) {
        val out = StringBuilder(indent)
        when (declaration) {
            is KSClassDeclaration -> {
                out.append(declaration.classKind).append(' ').append(declaration.qualifiedName?.asString())
                    .append(" modifiers=").append(modifiers(declaration))
                    .append(" typeParameters=").append(declaration.typeParameters.joinToString(",") {
                        it.name.asString() + ":" + it.bounds.map { bound -> type(bound) }.joinToString("&")
                    })
                    .append(" supertypes=").append(declaration.superTypes.map { type(it) }.joinToString(","))
            }
            is KSFunctionDeclaration -> {
                out.append("fun ").append(declaration.simpleName.asString())
                    .append(" kind=").append(declaration.functionKind)
                    .append(" abstract=").append(declaration.isAbstract)
                    .append(" modifiers=").append(modifiers(declaration))
                    .append(" typeParameters=").append(declaration.typeParameters.joinToString(",") { it.name.asString() })
                    .append(" receiver=").append(type(declaration.extensionReceiver))
                    .append(" (")
                    .append(declaration.parameters.joinToString(", ") {
                        "${it.name?.asString()}: ${type(it.type)} vararg=${it.isVararg} default=${it.hasDefault}"
                    })
                    .append("): ").append(type(declaration.returnType))
                for (annotation in declaration.annotations) out.append(" @").append(annotation.shortName.asString())
            }
            is KSPropertyDeclaration -> {
                out.append("property ").append(declaration.simpleName.asString())
                    .append(" mutable=").append(declaration.isMutable)
                    .append(" backingField=").append(declaration.hasBackingField)
                    .append(" modifiers=").append(modifiers(declaration))
                    .append(": ").append(type(declaration.type))
                declaration.extensionReceiver?.let { out.append(" receiver=").append(type(it)) }
            }
            else -> out.append(declaration::class.java.interfaces.first().simpleName).append(' ')
                .append(declaration.simpleName.asString())
        }
        env.logger.warn(out.toString())
        if (declaration is KSClassDeclaration) {
            for (nested in declaration.declarations) dump(nested, "$indent  ")
        }
    }
}
