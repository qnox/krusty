// Kotlin's answers for `range_unsigned_until_empty.c`: an empty unsigned `until` is the type's
// declared `EMPTY`, and a non-empty one ends one short of its bound.
fun box(): String = buildString {
    val u = 5u until 0u
    val ul = 5uL until 0uL
    appendLine("${u.isEmpty()} ${u.first} ${u.last} ${ul.isEmpty()} ${ul.first} ${ul.last}")
    appendLine("${(1u until 4000000000u).last} ${(7uL until 8uL).first} " +
        "${(5 until Int.MIN_VALUE).first} ${(5L until Long.MIN_VALUE).first}")
}
