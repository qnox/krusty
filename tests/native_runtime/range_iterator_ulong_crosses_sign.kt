// Kotlin's walks for `range_iterator_ulong_crosses_sign.c`: `ULong` walks across 2^63.
fun box(): String = buildString {
    appendLine((Long.MAX_VALUE.toULong()..(Long.MAX_VALUE.toULong() + 1uL)).toList())
    appendLine(((Long.MAX_VALUE.toULong() + 1uL) downTo Long.MAX_VALUE.toULong()).toList())
    appendLine((0uL..ULong.MAX_VALUE step Long.MAX_VALUE).toList())
}
