data class ErasedClosureHolder(val value: Any) {
    override fun toString(): String = "ErasedClosureHolder"
}

fun capturedClosure(value: Int): () -> Int = { value }

fun box(): String {
    val function = capturedClosure(7)
    val first = ErasedClosureHolder(function)
    val same = ErasedClosureHolder(function)
    val hash = first.hashCode()
    if (first != same) return "shared erased closure equality"
    if (hash != same.hashCode()) return "shared erased closure hash"
    if (hash != first.hashCode()) return "stable erased closure hash"
    if (first == ErasedClosureHolder(capturedClosure(7))) return "distinct erased closures"
    return "OK"
}
