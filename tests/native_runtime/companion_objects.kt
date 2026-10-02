// Kotlin's answers for `companion_objects.c`: the companion object of every built-in type Kotlin
// gives one, its class's names, how many of the thirteen each one is, and whether an unsigned
// type's is its signed type's.
fun box(): String = buildString {
    val all: List<Any> = listOf(Byte, Short, Int, Long, Char, Boolean, Float, Double, String,
        UByte, UShort, UInt, ULong)
    for (c in all) appendLine("${c::class.qualifiedName} ${c::class.simpleName}")
    appendLine(all.map { a -> all.count { it === a } })
    appendLine("${UInt === UInt.Companion} ${(UInt as Any) === (Int as Any)} " +
        "${(ULong as Any) === (Long as Any)}")
}
