fun String?.sizeOrZero(): Int = if (this == null) 0 else length

data class NullableReceiverReference(val function: () -> Int) {
    override fun toString(): String = "NullableReceiverReference"
}

fun box(): String {
    val absent: String? = null
    val present: String? = "abc"
    val first = NullableReceiverReference(absent::sizeOrZero)
    val second = NullableReceiverReference(absent::sizeOrZero)
    val other = NullableReceiverReference(present::sizeOrZero)
    if (first.function() != 0 || other.function() != 3) return "invoke"
    if (first != second) return "null receiver equality"
    if (first.hashCode() != second.hashCode()) return "null receiver hash"
    if (first == other) return "null/non-null receiver equality"
    return "OK"
}
