package traverse

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class TraverseProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = TraverseProcessor(environment.logger)
}

// Keeps each sequence the resolver and the symbols return, and reads it twice and through two
// iterators at once.
class TraverseProcessor(private val logger: KSPLogger) : SymbolProcessor {
    override fun process(resolver: Resolver): List<KSAnnotated> {
        val files = resolver.getAllFiles()
        report("files", files) { it.fileName }
        report("new files", resolver.getNewFiles()) { it.fileName }
        for (file in files) {
            report("${file.fileName} declarations", file.declarations, ::name)
            for (declaration in file.declarations) visit(declaration)
        }
        return emptyList()
    }

    private fun visit(declaration: KSDeclaration) {
        if (declaration !is KSClassDeclaration) return
        val name = name(declaration)
        val declarations = declaration.declarations
        report("$name declarations", declarations, ::name)
        val annotations = declaration.annotations
        report("$name annotations", annotations) { it.shortName.asString() }
        for (annotation in annotations) {
            val shown = "$name @${annotation.shortName.asString()}"
            report("$shown reference annotations", annotation.annotationType.annotations) { it.shortName.asString() }
            report("$shown type annotations", annotation.annotationType.resolve().annotations) { it.shortName.asString() }
        }
        for (nested in declarations) visit(nested)
    }

    private fun name(declaration: KSDeclaration): String =
        declaration.qualifiedName?.asString() ?: "${declaration.simpleName.asString()}(unqualified)"

    private fun <T> report(what: String, sequence: Sequence<T>, name: (T) -> String) {
        val first = sequence.map(name).joinToString(",")
        val second = sequence.map(name).joinToString(",")
        val left = sequence.iterator()
        val right = sequence.iterator()
        val interleaved = StringBuilder()
        while (left.hasNext()) {
            interleaved.append(name(left.next())).append('|').append(if (right.hasNext()) name(right.next()) else "-").append(' ')
        }
        logger.warn("$what count=${sequence.count()} first=[$first] second=[$second] interleaved=[${interleaved.toString().trim()}]")
    }
}
