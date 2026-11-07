// MODULE: lib
fun sharedTarget(value: Int): Int = value + 1
fun libraryReference(): (Int) -> Int = ::sharedTarget

// MODULE: main(lib)
data class ImportedReference(val function: (Int) -> Int) {
    override fun toString(): String = "ImportedReference"
}

fun box(): String {
    val fromLibrary = ImportedReference(libraryReference())
    val fromConsumer = ImportedReference(::sharedTarget)
    if (fromLibrary.function(7) != 8) return "library invoke"
    if (fromConsumer.function(7) != 8) return "consumer invoke"
    if (fromLibrary != fromConsumer) return "source/dependency identity"
    if (fromLibrary.hashCode() != fromConsumer.hashCode()) return "source/dependency hash"
    return "OK"
}
