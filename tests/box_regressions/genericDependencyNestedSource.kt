
// MODULE: lib
fun <T> dependencyKeep(value: T): T = value
// MODULE: main(lib)
fun sourceAny(value: Any): Any = value
fun box(): String { dependencyKeep(sourceAny(42)); return "OK" }
