data class ClosureHolder(val value: () -> Int) {
    override fun toString(): String = "ClosureHolder"
}

fun closure(value: Int): () -> Int = { value }

fun box(): String {
    val function = closure(7)
    val holder = ClosureHolder(function)
    val hash = holder.hashCode()
    if (holder.value() != 7) return "invoke"
    if (holder.hashCode() != hash) return "stable hash"
    if (holder != ClosureHolder(function)) return "shared closure equality"
    if (ClosureHolder(function).hashCode() != hash) return "shared closure hash"
    if (holder == ClosureHolder(closure(7))) return "distinct closure equality"
    return "OK"
}
