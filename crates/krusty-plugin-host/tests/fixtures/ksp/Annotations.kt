package annotations

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class AnnotationsProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = AnnotationsProcessor(environment)
}

/** Logs every annotation of every declaration and parameter with its arguments and defaults, once. */
class AnnotationsProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (file in resolver.getAllFiles()) {
            for (declaration in file.declarations) dump(declaration)
        }
        return emptyList()
    }

    private fun value(value: Any?): String = when (value) {
        null -> "null"
        is String -> "\"" + value + "\":String"
        is KSType -> "type " + (value.declaration.qualifiedName?.asString() ?: "?") +
            (if (value.arguments.isEmpty()) "" else value.arguments.joinToString(",", "<", ">") { argument ->
                argument.variance.name + " " + (argument.type?.resolve()?.declaration?.qualifiedName?.asString() ?: "*")
            })
        is KSClassDeclaration -> "entry " + value.classKind + " " + value.qualifiedName?.asString()
        is KSAnnotation -> annotation(value)
        is List<*> -> value.joinToString(", ", "[", "]:List") { value(it) }
        else -> value.toString() + ":" + value.javaClass.simpleName
    }

    private fun annotation(annotation: KSAnnotation): String =
        "@" + annotation.shortName.asString() +
            "=" + annotation.annotationType.resolve().declaration.qualifiedName?.asString() +
            " target=" + annotation.useSiteTarget +
            " origin=" + annotation.origin +
            annotation.arguments.joinToString(", ", " (", ")") {
                "${it.name?.asString()}=${value(it.value)}${if (it.isSpread) " spread" else ""} ${it.origin}"
            } +
            annotation.defaultArguments.joinToString(", ", " defaults(", ")") {
                "${it.name?.asString()}=${value(it.value)}"
            }

    private fun accessor(accessor: KSPropertyAccessor): String =
        accessor.origin.name + "/" + accessor.modifiers.map { it.name }.sorted().joinToString(",")

    private fun annotations(what: String, annotated: KSAnnotated) {
        for (annotation in annotated.annotations) env.logger.warn(what + " " + annotation(annotation))
    }

    private fun dump(declaration: KSDeclaration) {
        val name = declaration.qualifiedName?.asString() ?: declaration.simpleName.asString()
        annotations(name, declaration)
        when (declaration) {
            is KSClassDeclaration -> {
                for (nested in declaration.declarations) dump(nested)
            }
            is KSFunctionDeclaration -> {
                for (parameter in declaration.parameters) annotations(name + "(" + parameter.name?.asString() + ")", parameter)
            }
            is KSPropertyDeclaration -> {
                val getter = declaration.getter
                val setter = declaration.setter
                env.logger.warn(
                    "$name accessors get=" + getter?.let { accessor(it) } + " set=" + setter?.let { accessor(it) }
                )
                getter?.let { annotations("$name.get", it) }
                setter?.let {
                    annotations("$name.set", it)
                    annotations("$name.set(" + it.parameter.name?.asString() + ")", it.parameter)
                }
            }
            else -> {}
        }
    }
}
