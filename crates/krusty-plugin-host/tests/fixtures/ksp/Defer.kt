package defer

import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class DeferProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = DeferProcessor()
}

class DeferProcessor : SymbolProcessor {
    override fun process(resolver: Resolver): List<KSAnnotated> = resolver.getAllFiles().toList()
}
