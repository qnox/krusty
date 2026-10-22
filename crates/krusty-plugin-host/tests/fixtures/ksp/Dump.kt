package dump

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class DumpProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = DumpProcessor(environment)
}

class DumpProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var round = 0
    private val out = StringBuilder()

    override fun process(resolver: Resolver): List<KSAnnotated> {
        round++
        out.append("round ").append(round).append('\n')
        val newFiles = resolver.getNewFiles().map { it.filePath }.toSet()
        for (file in resolver.getAllFiles()) {
            out.append("file ").append(file.fileName).append(" package=").append(file.packageName.asString())
                .append(" new=").append(file.filePath in newFiles).append('\n')
            for (declaration in file.declarations) dump(declaration, "  ", out)
        }
        if (round == 1) {
            env.codeGenerator.createNewFile(Dependencies.ALL_FILES, "gen", "Generated").use {
                it.write("package gen\n\n@sample.Marker\nclass Generated {\n    class Nested\n}\n".toByteArray())
            }
        }
        return emptyList()
    }

    private fun dump(declaration: KSDeclaration, indent: String, out: StringBuilder) {
        if (declaration !is KSClassDeclaration) {
            out.append(indent).append(declaration::class.java.interfaces.first().simpleName).append(' ')
                .append(declaration.simpleName.asString()).append('\n')
            return
        }
        out.append(indent).append(declaration.classKind).append(' ').append(declaration.qualifiedName?.asString())
            .append(" simple=").append(declaration.simpleName.asString())
            .append(" package=").append(declaration.packageName.asString())
            .append(" parent=").append(declaration.parentDeclaration?.qualifiedName?.asString())
            .append(" file=").append(declaration.containingFile?.fileName)
            .append(" origin=").append(declaration.origin)
            .append(" location=").append(declaration.location)
            .append(" fileParent=").append(declaration.parent is KSFile)
        for (annotation in declaration.annotations) {
            out.append(" @").append(annotation.shortName.asString()).append('=')
                .append(annotation.annotationType.resolve().declaration.qualifiedName?.asString())
        }
        out.append('\n')
        if (round == 1 && declaration.simpleName.asString() == "Root") env.logger.warn("seen here", declaration)
        for (nested in declaration.declarations) dump(nested, "$indent  ", out)
    }

    override fun finish() {
        env.codeGenerator.createNewFile(Dependencies.ALL_FILES, "", "dump", "txt").use {
            it.write(out.toString().toByteArray())
        }
        env.logger.warn("finished after $round rounds")
    }
}
