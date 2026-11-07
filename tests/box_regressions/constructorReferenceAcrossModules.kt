// MODULE: lib
class SharedConstructor(val value: Int) {
    override fun hashCode(): Int = value
}
fun libraryConstructorReference(): Any = ::SharedConstructor

// MODULE: main(lib)
data class ConstructorReference(val function: Any) {
    override fun toString(): String = "ConstructorReference"
}

fun box(): String {
    val local: (Int) -> SharedConstructor = ::SharedConstructor
    if (local(7).value != 7) return "consumer invoke"
    val fromLibrary = ConstructorReference(libraryConstructorReference())
    val fromConsumer = ConstructorReference(local)
    if (fromLibrary != fromConsumer) return "constructor source/dependency identity"
    if (fromLibrary.hashCode() != fromConsumer.hashCode()) return "constructor source/dependency hash"
    return "OK"
}
