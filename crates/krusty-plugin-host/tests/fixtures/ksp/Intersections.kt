package intersections

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class IntersectionsProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = IntersectionsProcessor(environment)
}

/** Logs every synthetic member each source classifier lists, constructors left out, once. */
class IntersectionsProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
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
        val name = c.simpleName.asString()
        for (f in c.getAllFunctions()) {
            if (f.origin != Origin.SYNTHETIC || f.simpleName.asString() == "<init>") continue
            val parameters = f.parameters.joinToString(", ") {
                "${it.name?.asString()}: ${it.type.resolve()} ${it.origin} ${annotations(it)}"
            }
            env.logger.warn(
                "$name fun ${f.simpleName.asString()}($parameters): ${f.returnType?.resolve()} " +
                    "parent=${f.parentDeclaration?.qualifiedName?.asString()} " +
                    "annotations=${annotations(f)} overrides=${f.findOverridee()?.qualifiedName?.asString()} " +
                    "file=${f.containingFile?.fileName} line=${(f.location as? FileLocation)?.lineNumber}"
            )
        }
        for (p in c.getAllProperties()) {
            if (p.origin != Origin.SYNTHETIC) continue
            val getter = p.getter
            val setter = p.setter
            env.logger.warn(
                "$name property ${p.simpleName.asString()}: ${p.type.resolve()} mutable=${p.isMutable} " +
                    "parent=${p.parentDeclaration?.qualifiedName?.asString()} " +
                    "annotations=${annotations(p)} overrides=${p.findOverridee()?.qualifiedName?.asString()} " +
                    "file=${p.containingFile?.fileName} line=${(p.location as? FileLocation)?.lineNumber} " +
                    "field=${p.hasBackingField} " +
                    "getter=${getter?.origin} ${getter?.modifiers?.map { it.name }?.sorted()} ${getter?.let { annotations(it) }} " +
                    "setter=${setter?.origin} ${setter?.modifiers?.map { it.name }?.sorted()}"
            )
        }
    }

    private fun annotations(annotated: KSAnnotated): List<String> =
        annotated.annotations.map { it.shortName.asString() }.toList()
}
